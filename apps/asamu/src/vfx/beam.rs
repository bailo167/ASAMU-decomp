//! The grapple beam (`asamu.GrappleGun`'s `MyBeam` particle system): colours
//! per mode, the beam's end point, its noise and the camera-facing ribbon
//! geometry. Pure functions; `crate::vfx` draws them.
//!
//! Behaviour from local reading of `GrappleGun` / `ASAMUSettingsManager`
//! script and the class defaults (VFX_DECALS.md §2); no script text here.

use asamu_core::glam::Vec3 as UeVec3;
use bevy::math::Vec3;

/// `GrappleGun.DefaultBeamColor` (CDO, linear RGBA). CONFIRMED (cdo).
pub const DEFAULT_BEAM_COLOR: [f32; 4] = [0.04, 0.19, 0.79, 1.0];
/// `GrappleGun.midasBeamColor` (CDO). CONFIRMED (cdo).
pub const MIDAS_BEAM_COLOR: [f32; 4] = [1.0, 0.95, 0.0, 1.0];
/// Decal emissive multiplier for the default and goat beams
/// (`UpdateBeamVisuals`; also the default of `SetDecalAndLightColor`).
/// CONFIRMED (src).
pub const EMISSIVE_DEFAULT: f32 = 300.0;
/// Decal emissive multiplier for the Midas and custom-colour beams
/// (`UpdateBeamVisuals`). CONFIRMED (src).
pub const EMISSIVE_TINTED: f32 = 50.0;
/// Saturation and value of the custom beam colour (`ApplyBeamColor` builds
/// an HSV colour from the `BeamColor` setting as hue). CONFIRMED (src).
pub const CUSTOM_SATURATION: f32 = 0.85;
/// See [`CUSTOM_SATURATION`].
pub const CUSTOM_VALUE: f32 = 0.85;
/// Collectibles needed to unlock the beam colour (`CheckIfUnlocked`).
/// CONFIRMED (src).
pub const UNLOCK_BEAM_COLOR: u32 = 10;
/// Collectibles needed for goat mode. CONFIRMED (src).
pub const UNLOCK_GOAT: u32 = 15;
/// Collectibles needed for Midas mode. CONFIRMED (src).
pub const UNLOCK_MIDAS: u32 = 20;

/// Width of the two enabled beam emitters of
/// `AdventureSuitEffects.ParticleSystems.GrappleBeam` (`ParticleModuleSize`
/// `StartSize`, constant), UU, in the system's emitter order: emitter 0 is
/// the wide beam. CONFIRMED (content). A third beam emitter and the two
/// sprite emitters are disabled (`bEnabled` false on their LOD level).
pub const BEAM_WIDTHS_UU: [f32; 2] = [4.0, 2.0];
/// Noise points along the beam (`ParticleModuleBeamNoise.Frequency`).
/// CONFIRMED (content).
pub const NOISE_POINTS: usize = 20;
/// Noise displacement range, UU (`NoiseRange` ±1 on the wide emitter).
/// CONFIRMED (content); how the engine scales it is TENTATIVE.
pub const NOISE_RANGE_UU: f32 = 1.0;
/// The noise points are re-randomised after this long (`NoiseLockTime`), s.
/// CONFIRMED (content).
pub const NOISE_LOCK_TIME: f32 = 0.2;

/// The gun's colour modes (`bCustomBeamColor`, `bMidasMode`, `bGoatMode`).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct BeamModes {
    /// Custom beam colour (`customBeamModeColor`) when enabled.
    pub custom: Option<[f32; 4]>,
    /// Midas mode.
    pub midas: bool,
    /// Goat mode (the beam becomes the goat's tongue).
    pub goat: bool,
}

/// What `UpdateBeamVisuals` sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamVisuals {
    /// The beam material's `GrappleBeamColor`.
    pub beam_color: [f32; 4],
    /// The hit decal's `DecalColor` and the hand plates' `LightColor`.
    pub decal_color: [f32; 4],
    /// The hit decal's `DecalEmissiveMultiplier`.
    pub emissive_multiplier: f32,
    /// The goat tongue particle systems replace the beam.
    pub tongue: bool,
    /// The HUD crosshair is tinted with this colour.
    pub crosshair_tint: Option<[f32; 3]>,
}

/// `GrappleGun.UpdateBeamVisuals` (CONFIRMED (src)): the beam colour is the
/// default for goat mode, else Midas, else custom, else default; the decal
/// and plate colour then switches to Midas or custom when those are on (so
/// goat mode keeps the custom/Midas decal colour); the crosshair is tinted
/// with the decal colour whenever any mode is on (the unlock check that
/// guards it always passes then: every mode needs at least the beam-colour
/// unlock).
#[must_use]
pub fn beam_visuals(m: BeamModes) -> BeamVisuals {
    let (beam_color, mut emissive) = if m.goat {
        (DEFAULT_BEAM_COLOR, EMISSIVE_DEFAULT)
    } else if m.midas {
        (MIDAS_BEAM_COLOR, EMISSIVE_TINTED)
    } else if let Some(c) = m.custom {
        (c, EMISSIVE_TINTED)
    } else {
        (DEFAULT_BEAM_COLOR, EMISSIVE_DEFAULT)
    };
    let mut decal_color = beam_color;
    if m.midas {
        decal_color = MIDAS_BEAM_COLOR;
        emissive = EMISSIVE_TINTED;
    } else if let Some(c) = m.custom {
        decal_color = c;
        emissive = EMISSIVE_TINTED;
    }
    let any = m.custom.is_some() || m.midas || m.goat;
    BeamVisuals {
        beam_color,
        decal_color,
        emissive_multiplier: emissive,
        tongue: m.goat,
        crosshair_tint: any.then_some([decal_color[0], decal_color[1], decal_color[2]]),
    }
}

/// `ASAMUSettingsManager.HSVToRGB` (hue in degrees, CONFIRMED (src)): the
/// textbook sector rule; a hue outside `[0, 360]` leaves the sector colour
/// at zero, as in the original.
#[must_use]
pub fn hsv_to_rgb(hue_degrees: f32, s: f32, v: f32) -> [f32; 3] {
    let chroma = s * v;
    let h = hue_degrees / 60.0;
    let x = chroma * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = if h < 1.0 {
        (chroma, x, 0.0)
    } else if h < 2.0 {
        (x, chroma, 0.0)
    } else if h < 3.0 {
        (0.0, chroma, x)
    } else if h < 4.0 {
        (0.0, x, chroma)
    } else if h < 5.0 {
        (x, 0.0, chroma)
    } else if h <= 6.0 {
        (chroma, 0.0, x)
    } else {
        (0.0, 0.0, 0.0)
    };
    let m = v - chroma;
    [r + m, g + m, b + m]
}

/// The player's extras settings (`ASAMUSettingsManager` config values) and
/// progress, which decide the gun's modes.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ExtrasSettings {
    /// Collectibles found (`GetTotalAmountCollected`).
    pub collected: u32,
    /// `BeamColorActive` (default false, `DefaultSettings.ini`).
    pub beam_color_active: bool,
    /// `BeamColor` (hue, degrees; default 0).
    pub beam_color_hue: f32,
    /// `GoatModeActive`.
    pub goat_active: bool,
    /// `MidasModeActive`.
    pub midas_active: bool,
}

impl ExtrasSettings {
    /// The gun's modes after `InitSettings` and the gun's own start: each
    /// mode is on only when its setting is on and it is unlocked
    /// (`CheckIfUnlocked`). CONFIRMED (src).
    #[must_use]
    pub fn modes(&self) -> BeamModes {
        let custom = (self.beam_color_active && self.collected >= UNLOCK_BEAM_COLOR).then(|| {
            let [r, g, b] = hsv_to_rgb(self.beam_color_hue, CUSTOM_SATURATION, CUSTOM_VALUE);
            [r, g, b, 1.0]
        });
        BeamModes {
            custom,
            midas: self.midas_active && self.collected >= UNLOCK_MIDAS,
            goat: self.goat_active && self.collected >= UNLOCK_GOAT,
        }
    }
}

/// Deterministic xorshift for render-only randomness (beam noise, decal
/// rotation): never feeds the simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderRng(u64);

impl RenderRng {
    /// A generator from `seed` (0 is replaced).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next value in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }

    /// Next value in `[-1, 1)`.
    pub fn next_signed(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

/// The beam noise: per noise point a 2-D offset across the beam, locked for
/// [`NOISE_LOCK_TIME`] and then re-randomised.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamNoise {
    offsets: Vec<[f32; 2]>,
    timer: f32,
    rng: RenderRng,
}

impl BeamNoise {
    /// A noise state seeded with `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut n = Self {
            offsets: vec![[0.0; 2]; NOISE_POINTS],
            timer: 0.0,
            rng: RenderRng::new(seed),
        };
        n.randomise();
        n
    }

    fn randomise(&mut self) {
        for o in &mut self.offsets {
            *o = [self.rng.next_signed(), self.rng.next_signed()];
        }
    }

    /// Advances the lock timer by `dt` seconds.
    pub fn update(&mut self, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.timer += dt;
        if self.timer >= NOISE_LOCK_TIME {
            self.timer %= NOISE_LOCK_TIME;
            self.randomise();
        }
    }

    /// The current offsets (unit range).
    #[must_use]
    pub fn offsets(&self) -> &[[f32; 2]] {
        &self.offsets
    }
}

/// The beam's end point: `vGrappleLocation`, which the gun replaces every
/// tick with the anchor helper's location while it follows an `InterpActor`
/// (G-AT-8). The simulation keeps both; `follow` wins when present.
#[must_use]
pub fn beam_end(grapple_location: UeVec3, follow_helper: Option<UeVec3>) -> UeVec3 {
    follow_helper.unwrap_or(grapple_location)
}

/// Points the beam is drawn through (`ParticleModuleTypeDataBeam2.
/// InterpolationPoints`). CONFIRMED (content).
pub const INTERPOLATION_POINTS: usize = 40;
/// Source tangent strength per beam emitter, in the order of
/// [`BEAM_WIDTHS_UU`]: the gun sets emitter 0 (the wide beam) to 1000 at
/// start (`SetBeamSourceStrength(0, 1000, 0)`, CONFIRMED (src)); the narrow
/// beam keeps the asset's `ParticleModuleBeamSource.SourceStrength`, 1250
/// (CONFIRMED (content)).
pub const SOURCE_TANGENT_STRENGTHS: [f32; 2] = [1000.0, 1250.0];
/// Target tangent strength (`ParticleModuleBeamTarget.TargetStrength`,
/// constant). CONFIRMED (content).
pub const TARGET_TANGENT_STRENGTH: f32 = 20.0;

/// Points along the beam from `start` to `end` (UE space).
///
/// The beam emitters interpolate between source and target with tangents
/// (the engine's cubic Hermite interpolation over
/// [`INTERPOLATION_POINTS`]): the beam leaves the hand along
/// `source_dir` (the emitter's direction; here the view direction,
/// TENTATIVE: the hand socket's own axis is not exposed) with
/// `source_strength` ([`SOURCE_TANGENT_STRENGTHS`]) and arrives along the
/// chord with [`TARGET_TANGENT_STRENGTH`] (direction TENTATIVE). The noise offsets
/// (linearly interpolated along the beam) displace the points across the
/// chord by `amplitude` UU; the displacement fades to zero at both ends.
#[must_use]
pub fn beam_path(
    start: UeVec3,
    end: UeVec3,
    source_dir: UeVec3,
    source_strength: f32,
    noise: &[[f32; 2]],
    amplitude: f32,
) -> Vec<UeVec3> {
    let axis = end - start;
    let len = axis.length();
    if !len.is_finite() || len < 1e-3 {
        return vec![start, end];
    }
    let dir = axis / len;
    let helper = if dir.z.abs() < 0.9 {
        UeVec3::Z
    } else {
        UeVec3::X
    };
    let side = dir.cross(helper).normalize_or_zero();
    let up = side.cross(dir).normalize_or_zero();
    let strength = if source_strength.is_finite() {
        source_strength
    } else {
        0.0
    };
    let t0 = source_dir.normalize_or_zero() * strength;
    let t1 = dir * TARGET_TANGENT_STRENGTH;
    let n = INTERPOLATION_POINTS;
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let a = i as f32 / n as f32;
        let (a2, a3) = (a * a, a * a * a);
        // Cubic Hermite (the engine's `CubicInterp`).
        let p = start * (2.0 * a3 - 3.0 * a2 + 1.0)
            + t0 * (a3 - 2.0 * a2 + a)
            + t1 * (a3 - a2)
            + end * (-2.0 * a3 + 3.0 * a2);
        let offset = if noise.is_empty() || i == 0 || i == n {
            [0.0, 0.0]
        } else {
            let x = a * (noise.len() - 1) as f32;
            let k = (x.floor() as usize).min(noise.len() - 1);
            let k1 = (k + 1).min(noise.len() - 1);
            let f = x - k as f32;
            [
                noise[k][0] + (noise[k1][0] - noise[k][0]) * f,
                noise[k][1] + (noise[k1][1] - noise[k][1]) * f,
            ]
        };
        let pin = (a * std::f32::consts::PI).sin();
        out.push(p + (side * offset[0] + up * offset[1]) * amplitude * pin);
    }
    out
}

/// A camera-facing strip through `points` (render space) of `width`
/// render units, as positions, texture coordinates (`u` along the beam from
/// 0 to 1, `v` across from 0 to 1) and triangle indices.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Ribbon {
    /// Positions.
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangles.
    pub indices: Vec<u32>,
}

/// Builds the [`Ribbon`] of `points` seen from `eye`.
#[must_use]
pub fn ribbon(points: &[Vec3], width: f32, eye: Vec3) -> Ribbon {
    let mut r = Ribbon::default();
    if points.len() < 2 || !width.is_finite() || width <= 0.0 {
        return r;
    }
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut along = 0.0f32;
    for (i, p) in points.iter().enumerate() {
        let prev = points[i.saturating_sub(1)];
        let next = points[(i + 1).min(points.len() - 1)];
        let tangent = (next - prev).normalize_or_zero();
        let to_eye = (eye - *p).normalize_or_zero();
        let mut side = tangent.cross(to_eye).normalize_or_zero();
        if side == Vec3::ZERO {
            side = tangent.any_orthonormal_vector();
        }
        if i > 0 {
            along += points[i - 1].distance(*p);
        }
        let u = if total > 0.0 { along / total } else { 0.0 };
        let h = side * (width * 0.5);
        r.positions.push((*p - h).to_array());
        r.positions.push((*p + h).to_array());
        r.uvs.push([u, 0.0]);
        r.uvs.push([u, 1.0]);
    }
    for i in 0..(points.len() - 1) as u32 {
        let a = i * 2;
        r.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn default_modes_use_the_default_colour_and_multiplier() {
        let v = beam_visuals(BeamModes::default());
        assert_eq!(v.beam_color, DEFAULT_BEAM_COLOR);
        assert_eq!(v.decal_color, DEFAULT_BEAM_COLOR);
        assert_eq!(v.emissive_multiplier, EMISSIVE_DEFAULT);
        assert!(!v.tongue);
        assert_eq!(v.crosshair_tint, None);
    }

    #[test]
    fn mode_precedence_follows_update_beam_visuals() {
        let red = [1.0, 0.0, 0.0, 1.0];
        // Custom only.
        let v = beam_visuals(BeamModes {
            custom: Some(red),
            ..BeamModes::default()
        });
        assert_eq!((v.beam_color, v.decal_color), (red, red));
        assert_eq!(v.emissive_multiplier, EMISSIVE_TINTED);
        assert_eq!(v.crosshair_tint, Some([1.0, 0.0, 0.0]));
        // Midas beats custom.
        let v = beam_visuals(BeamModes {
            custom: Some(red),
            midas: true,
            goat: false,
        });
        assert_eq!(v.beam_color, MIDAS_BEAM_COLOR);
        assert_eq!(v.decal_color, MIDAS_BEAM_COLOR);
        // Goat: default beam colour (the tongue), but the decal keeps the
        // Midas colour and multiplier.
        let v = beam_visuals(BeamModes {
            custom: Some(red),
            midas: true,
            goat: true,
        });
        assert_eq!(v.beam_color, DEFAULT_BEAM_COLOR);
        assert_eq!(v.decal_color, MIDAS_BEAM_COLOR);
        assert_eq!(v.emissive_multiplier, EMISSIVE_TINTED);
        assert!(v.tongue);
        // Goat alone: default everything, crosshair tinted default blue.
        let v = beam_visuals(BeamModes {
            goat: true,
            ..BeamModes::default()
        });
        assert_eq!(v.decal_color, DEFAULT_BEAM_COLOR);
        assert_eq!(v.emissive_multiplier, EMISSIVE_DEFAULT);
        assert_eq!(v.crosshair_tint, Some([0.04, 0.19, 0.79]));
    }

    #[test]
    fn hsv_sectors() {
        assert!(close(hsv_to_rgb(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]));
        assert!(close(hsv_to_rgb(120.0, 1.0, 1.0), [0.0, 1.0, 0.0]));
        assert!(close(hsv_to_rgb(240.0, 1.0, 1.0), [0.0, 0.0, 1.0]));
        assert!(close(hsv_to_rgb(60.0, 1.0, 1.0), [1.0, 1.0, 0.0]));
        // The settings' S = V = 0.85 at hue 0: chroma 0.7225, minimum 0.1275.
        assert!(close(
            hsv_to_rgb(0.0, CUSTOM_SATURATION, CUSTOM_VALUE),
            [0.85, 0.1275, 0.1275]
        ));
        // Out of range: the original leaves the sector colour at zero.
        assert!(close(hsv_to_rgb(400.0, 1.0, 1.0), [0.0, 0.0, 0.0]));
    }

    #[test]
    fn extras_need_their_unlocks() {
        let mut s = ExtrasSettings {
            collected: 9,
            beam_color_active: true,
            beam_color_hue: 120.0,
            goat_active: true,
            midas_active: true,
        };
        assert_eq!(s.modes(), BeamModes::default());
        s.collected = 10;
        let m = s.modes();
        assert!(m.custom.is_some() && !m.goat && !m.midas);
        s.collected = 15;
        assert!(s.modes().goat && !s.modes().midas);
        s.collected = 20;
        assert!(s.modes().midas);
    }

    #[test]
    fn beam_end_prefers_the_following_helper() {
        let a = UeVec3::new(1.0, 2.0, 3.0);
        let h = UeVec3::new(4.0, 5.0, 6.0);
        assert_eq!(beam_end(a, None), a);
        assert_eq!(beam_end(a, Some(h)), h);
    }

    #[test]
    fn beam_path_is_pinned_at_both_ends() {
        let start = UeVec3::new(0.0, 0.0, 0.0);
        let end = UeVec3::new(2000.0, 0.0, 0.0);
        let noise = BeamNoise::new(7);
        // Source tangent along the chord: a straight beam plus noise.
        let p = beam_path(
            start,
            end,
            UeVec3::X,
            SOURCE_TANGENT_STRENGTHS[0],
            noise.offsets(),
            NOISE_RANGE_UU,
        );
        assert_eq!(p.len(), INTERPOLATION_POINTS + 1);
        assert_eq!(p[0], start);
        assert!((*p.last().unwrap() - end).length() < 1e-3);
        for (i, q) in p.iter().enumerate().skip(1) {
            assert!(q.x > p[i - 1].x, "monotonic along the chord");
            let off = (q.y * q.y + q.z * q.z).sqrt();
            assert!(off <= NOISE_RANGE_UU * 2f32.sqrt() + 1e-3, "{off}");
        }
        // Without noise and with the tangent off the chord the beam bows
        // towards the source direction and still ends at the anchor.
        let bowed = beam_path(start, end, UeVec3::Y, SOURCE_TANGENT_STRENGTHS[0], &[], 0.0);
        assert!(bowed[INTERPOLATION_POINTS / 4].y > 50.0);
        assert!((*bowed.last().unwrap() - end).length() < 1e-3);
        assert!(bowed.iter().all(|q| q.is_finite()));
        // The narrow beam keeps the asset's stronger source tangent: it bows
        // further (the Hermite tangent term scales with the strength).
        let narrow = beam_path(start, end, UeVec3::Y, SOURCE_TANGENT_STRENGTHS[1], &[], 0.0);
        let k = INTERPOLATION_POINTS / 4;
        assert!((narrow[k].y / bowed[k].y - 1.25).abs() < 1e-3);
        // A non-finite strength gives a straight, finite beam.
        let straight = beam_path(start, end, UeVec3::Y, f32::NAN, &[], 0.0);
        assert!(straight.iter().all(|q| q.is_finite() && q.y.abs() < 1e-3));
        // Degenerate beam.
        assert_eq!(
            beam_path(
                start,
                start,
                UeVec3::X,
                SOURCE_TANGENT_STRENGTHS[0],
                noise.offsets(),
                1.0
            )
            .len(),
            2
        );
    }

    #[test]
    fn noise_relocks_after_the_lock_time() {
        let mut n = BeamNoise::new(3);
        let first = n.offsets().to_vec();
        n.update(NOISE_LOCK_TIME * 0.5);
        assert_eq!(n.offsets(), first.as_slice());
        n.update(NOISE_LOCK_TIME * 0.6);
        assert_ne!(n.offsets(), first.as_slice());
        n.update(f32::NAN);
        assert!(
            n.offsets()
                .iter()
                .all(|o| o[0].abs() <= 1.0 && o[1].abs() <= 1.0)
        );
    }

    #[test]
    fn ribbon_faces_the_eye_and_spans_the_width() {
        let pts = [Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0)];
        let eye = Vec3::new(0.0, 5.0, 0.0);
        let r = ribbon(&pts, 2.0, eye);
        assert_eq!(r.positions.len(), 4);
        assert_eq!(r.indices.len(), 6);
        // Side vector perpendicular to the beam and to the eye direction: X.
        let a = Vec3::from_array(r.positions[0]);
        let b = Vec3::from_array(r.positions[1]);
        assert!((a.distance(b) - 2.0).abs() < 1e-5);
        assert!((b - a).normalize().dot(Vec3::X).abs() > 0.999);
        assert_eq!(r.uvs[2], [1.0, 0.0]);
        assert!(ribbon(&pts[..1], 2.0, eye).positions.is_empty());
        assert!(ribbon(&pts, 0.0, eye).positions.is_empty());
        // Eye on the beam axis: still a valid strip.
        let r = ribbon(&pts, 1.0, Vec3::new(0.0, 0.0, 5.0));
        assert!(r.positions.iter().all(|p| p.iter().all(|c| c.is_finite())));
    }
}
