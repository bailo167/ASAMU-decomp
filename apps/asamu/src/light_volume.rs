//! Indirect light for the objects that have no light map: movable meshes,
//! characters and the first-person hand (APPROXIMATION, see below and
//! `docs/reverse-engineering/LIGHTING.md`).
//!
//! What the original does: every such object owns a light environment. It
//! gathers the direct light of the lights that reach the object (with a
//! visibility test per light) and the indirect light its level baked into a
//! precomputed light volume, and lights the object with one synthesized
//! directional light plus a low-order spherical ambient term.
//!
//! What this module does: once the level's light volume is loaded
//! (`lightmaps.rs` reads it next to the light maps), the ambient light of
//! everything without a light map follows the volume **at the viewer's
//! position**: its interpolated radiance reduced to one irradiance
//! ([`asamu_assets::lighting::IncidentLight::irradiance`]), on the same
//! scale as the light maps, plus the level's sky lights. That replaces the
//! constant stand-in (`LightMapping::base_ambient`), a number of ours, with
//! the level's own data.
//!
//! Approximations, all render-only:
//!
//! - one value for the whole view (the viewer's), not one per object: right
//!   for the first-person hand, close for what stands near the player, and
//!   wrong for a distant movable object in other light;
//! - no direction: the volume's dominant direction is not used;
//! - direct light is unchanged: the level's lights still light these
//!   objects one by one and without the original's visibility test;
//! - PLACEHOLDER: the change of the value is eased over
//!   [`BLEND_SECONDS`] (the original blends a light environment over time
//!   too; its durations are not recovered).
//!
//! Without a volume (a level that has none, or data converted before
//! `asamu-import lightmaps` exported it) the constant stand-in stays.
//! `ASAMU_LIGHT_VOLUME=0` turns the module off.

use asamu_assets::lighting::{IncidentLight, LightVolume};
use asamu_core::coords::bevy_pos_to_ue;
use asamu_core::glam as sim_glam;
use bevy::prelude::*;

use crate::{PlayerCamera, SCALE};

/// Seconds over which the ambient light follows a new value (PLACEHOLDER,
/// ours: render-only smoothing).
pub const BLEND_SECONDS: f32 = 0.4;

/// The viewer must move this far (UU) before the volume is asked again
/// (ours: the samples are 200 UU apart).
const QUERY_STEP_UU: f32 = 16.0;

/// The light volume of the level being shown and the ambient light derived
/// from it.
#[derive(Resource, Default)]
pub struct VolumeAmbient {
    /// The level's volume (`None`: the constant stand-in is used).
    volume: Option<LightVolume>,
    /// Render ambient per unit of baked irradiance (the light maps' scale).
    per_irradiance: f32,
    /// Ambient of the level's sky lights (render units).
    sky: f32,
    /// Where the volume was last asked (UE3, UU).
    asked_at: Option<sim_glam::Vec3>,
    /// The value being approached and the one shown (linear RGB, render
    /// units).
    target: [f32; 3],
    shown: Option<[f32; 3]>,
    /// One line for the log and the developer read-out.
    pub line: String,
}

impl VolumeAmbient {
    /// Uses `volume` from now on (`None`: back to the stand-in).
    pub fn set(&mut self, volume: Option<LightVolume>, per_irradiance: f32, sky: f32) {
        *self = Self {
            volume: volume.filter(|v| !v.is_empty()),
            per_irradiance,
            sky,
            ..Self::default()
        };
        self.line = match &self.volume {
            Some(v) => format!(
                "light volume: {} samples light the objects without a light map",
                v.len()
            ),
            None => "light volume: none (constant ambient stand-in)".to_owned(),
        };
    }

    /// Forgets the volume (another level is loading).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// True while a volume drives the ambient light.
    #[cfg(test)]
    #[must_use]
    pub fn active(&self) -> bool {
        self.volume.is_some()
    }
}

/// Whether the module is on (`ASAMU_LIGHT_VOLUME=0` turns it off).
#[must_use]
pub fn enabled() -> bool {
    std::env::var("ASAMU_LIGHT_VOLUME").map_or(true, |v| v != "0")
}

/// The ambient light (linear RGB, render units) for baked light `light`
/// (`None`: no sample reaches the viewer, so the level baked no indirect
/// light there) plus the sky lights' white ambient `sky`.
#[must_use]
pub fn ambient_target(light: Option<&IncidentLight>, per_irradiance: f32, sky: f32) -> [f32; 3] {
    let sky = if sky.is_finite() { sky.max(0.0) } else { 0.0 };
    let k = if per_irradiance.is_finite() {
        per_irradiance.max(0.0)
    } else {
        0.0
    };
    let e = light.map_or([0.0; 3], IncidentLight::irradiance);
    let mut out = [sky; 3];
    for (o, v) in out.iter_mut().zip(e) {
        let add = v * k;
        if add.is_finite() {
            *o += add.max(0.0);
        }
    }
    out
}

/// `shown` moved towards `target` by `dt` seconds of an exponential ease
/// with time constant `seconds` (a non-positive constant jumps).
#[must_use]
pub fn approach(shown: [f32; 3], target: [f32; 3], dt: f32, seconds: f32) -> [f32; 3] {
    let f = if seconds > 0.0 && dt.is_finite() && dt >= 0.0 {
        1.0 - (-dt / seconds).exp()
    } else {
        1.0
    };
    let mut out = shown;
    for (o, t) in out.iter_mut().zip(target) {
        *o += (t - *o) * f;
    }
    out
}

/// Colour (linear RGB, largest channel 1) and brightness of an ambient
/// value; white at brightness 0 for black or unusable input.
#[must_use]
pub fn colour_and_brightness(rgb: [f32; 3]) -> ([f32; 3], f32) {
    let clean = rgb.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
    let max = clean[0].max(clean[1]).max(clean[2]);
    if max > 0.0 {
        (clean.map(|v| v / max), max)
    } else {
        ([1.0; 3], 0.0)
    }
}

/// Drives the ambient light from the volume.
pub struct LightVolumePlugin;

impl Plugin for LightVolumePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VolumeAmbient>()
            .add_systems(Update, follow_volume);
    }
}

fn follow_volume(
    time: Res<Time<Real>>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut state: ResMut<VolumeAmbient>,
    mut ambient: ResMut<GlobalAmbientLight>,
) {
    let state = &mut *state;
    let Some(volume) = &state.volume else {
        return;
    };
    let Some(camera) = camera.iter().next() else {
        return;
    };
    let t = camera.translation();
    let viewer = bevy_pos_to_ue(sim_glam::Vec3::new(t.x, t.y, t.z), SCALE);
    if !viewer.is_finite() {
        return;
    }
    let moved = state
        .asked_at
        .is_none_or(|p| p.distance(viewer) >= QUERY_STEP_UU);
    if moved {
        let light = volume.interpolate(viewer);
        state.target = ambient_target(light.as_ref(), state.per_irradiance, state.sky);
        if state.asked_at.is_none() {
            // Once per level: what the first value is, in the light maps'
            // units, for comparisons with the stand-in.
            let e = light.as_ref().map_or([0.0; 3], IncidentLight::irradiance);
            info!(
                "light volume: at the viewer ({:.0}, {:.0}, {:.0}) {} sample(s) give irradiance \
                 ({:.4}, {:.4}, {:.4}); sky ambient {:.1}, ambient light ({:.1}, {:.1}, {:.1})",
                viewer.x,
                viewer.y,
                viewer.z,
                light.as_ref().map_or(0, |l| l.samples),
                e[0],
                e[1],
                e[2],
                state.sky,
                state.target[0],
                state.target[1],
                state.target[2]
            );
        }
        state.asked_at = Some(viewer);
    }
    // The first value is shown at once: a level does not fade in from the
    // stand-in.
    let shown = match state.shown {
        Some(s) => approach(s, state.target, time.delta_secs(), BLEND_SECONDS),
        None => state.target,
    };
    state.shown = Some(shown);
    let ([r, g, b], brightness) = colour_and_brightness(shown);
    let colour = Color::linear_rgb(r, g, b);
    if ambient.brightness != brightness || ambient.color != colour {
        ambient.brightness = brightness;
        ambient.color = colour;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_is_baked_irradiance_on_the_light_map_scale_plus_sky() {
        let light = IncidentLight {
            indirect: [0.4, 0.0, 0.0],
            ambient: [0.4, 0.4, 0.0],
            environment: [0.0; 3],
            samples: 2,
        };
        // A quarter of the summed radiance, times the scale, plus the sky.
        let t = ambient_target(Some(&light), 1000.0, 30.0);
        assert!((t[0] - 230.0).abs() < 1e-3 && (t[1] - 130.0).abs() < 1e-3);
        assert!((t[2] - 30.0).abs() < 1e-6);
        // Where no sample reaches there is no baked indirect light.
        assert_eq!(ambient_target(None, 1000.0, 30.0), [30.0; 3]);
        assert_eq!(ambient_target(None, 1000.0, 0.0), [0.0; 3]);
        // Unusable numbers never reach the renderer.
        for bad in [f32::NAN, f32::INFINITY, -5.0] {
            let t = ambient_target(Some(&light), bad, bad);
            assert!(t.iter().all(|v| v.is_finite() && *v >= 0.0), "{bad}: {t:?}");
        }
        let huge = IncidentLight {
            indirect: [f32::MAX; 3],
            ..light
        };
        let t = ambient_target(Some(&huge), f32::MAX, 1.0);
        assert!(t.iter().all(|v| v.is_finite()), "{t:?}");
    }

    #[test]
    fn the_shown_value_eases_towards_the_target() {
        let from = [0.0, 10.0, 5.0];
        let to = [10.0, 0.0, 5.0];
        assert_eq!(approach(from, to, 0.0, BLEND_SECONDS), from);
        let half = approach(
            from,
            to,
            BLEND_SECONDS * std::f32::consts::LN_2,
            BLEND_SECONDS,
        );
        assert!((half[0] - 5.0).abs() < 1e-4 && (half[1] - 5.0).abs() < 1e-4);
        assert!((half[2] - 5.0).abs() < 1e-6);
        let late = approach(from, to, 100.0, BLEND_SECONDS);
        assert!((late[0] - 10.0).abs() < 1e-4 && late[1].abs() < 1e-4);
        // No time constant, or a time step that is no number: jump.
        assert_eq!(approach(from, to, 0.016, 0.0), to);
        assert_eq!(approach(from, to, f32::NAN, BLEND_SECONDS), to);
        // Many small steps equal one long step.
        let mut s = from;
        for _ in 0..60 {
            s = approach(s, to, 1.0 / 60.0, BLEND_SECONDS);
        }
        let once = approach(from, to, 1.0, BLEND_SECONDS);
        assert!((s[0] - once[0]).abs() < 1e-3, "{s:?} vs {once:?}");
    }

    #[test]
    fn colour_is_normalised_and_black_is_white_at_zero() {
        let (c, b) = colour_and_brightness([2.0, 1.0, 0.5]);
        assert_eq!((c, b), ([1.0, 0.5, 0.25], 2.0));
        assert_eq!(colour_and_brightness([0.0; 3]), ([1.0; 3], 0.0));
        assert_eq!(
            colour_and_brightness([f32::NAN, -1.0, f32::NEG_INFINITY]),
            ([1.0; 3], 0.0)
        );
        let (c, b) = colour_and_brightness([f32::INFINITY, 4.0, 0.0]);
        assert_eq!((c, b), ([0.0, 1.0, 0.0], 4.0));
    }

    #[test]
    fn a_volume_is_used_only_when_it_has_samples() {
        let mut state = VolumeAmbient::default();
        assert!(!state.active());
        state.set(Some(LightVolume::default()), 10.0, 0.0);
        assert!(!state.active(), "an empty volume keeps the stand-in");
        assert!(state.line.contains("stand-in"));
        // One sample, written by the test.
        let mut file = Vec::new();
        file.extend_from_slice(asamu_assets::lighting::LIGHT_VOLUME_MAGIC);
        file.extend_from_slice(&36u32.to_le_bytes());
        file.extend_from_slice(&1u32.to_le_bytes());
        for v in [0.0f32, 0.0, 0.0, 300.0] {
            file.extend_from_slice(&v.to_le_bytes());
        }
        file.extend_from_slice(&[0u8; 20]);
        let mut volume = LightVolume::default();
        assert_eq!(volume.add_bytes(&file, sim_glam::Vec3::ZERO), Ok(1));
        volume.finish();
        state.set(Some(volume), 10.0, 2.0);
        assert!(state.active());
        assert!(state.line.contains("1 samples"));
        state.clear();
        assert!(!state.active());
    }
}
