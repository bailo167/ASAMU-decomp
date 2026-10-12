//! The grapple beam (`asamu.GrappleGun`'s `MyBeam` particle system): colours
//! per mode, the beam's end point, its strands, their noise and the
//! camera-facing ribbon geometry. Pure functions; `crate::vfx` draws them and
//! [`super::emitter`] says where the beam starts.
//!
//! Behaviour from local reading of `GrappleGun` / `ASAMUSettingsManager`
//! script, the class defaults and the particle system's modules
//! (VFX_DECALS.md §2); no script text here.

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

/// One enabled beam emitter of
/// `AdventureSuitEffects.ParticleSystems.GrappleBeam`. The system has five
/// emitters; the third beam emitter and the two sprite emitters are disabled
/// (`bEnabled` false on their LOD level). CONFIRMED (content).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamEmitter {
    /// The beam's size, UU (`ParticleModuleSize.StartSize`, constant;
    /// CONFIRMED (content)): what the engine puts to **each side** of the
    /// beam's centre line ([`Self::width_uu`]). The type data's taper is off
    /// (`TaperMethod` none, factor and scale 1): the beam is as wide at the
    /// anchor as at the hand.
    pub size_uu: f32,
    /// Source tangent strength: the gun sets emitter 0 to 1000 at start
    /// (`SetBeamSourceStrength(0, 1000, 0)`, CONFIRMED (src)); emitter 1
    /// keeps the asset's `ParticleModuleBeamSource.SourceStrength`, 1250.
    pub source_strength: f32,
    /// Noise displacement range across the beam, UU: `NoiseRange` (uniform
    /// between these two) times `NoiseRangeScale` (1 and 1.3). How the
    /// engine applies the three components is TENTATIVE: two of them are
    /// used here, across the beam.
    pub noise_min_uu: f32,
    /// See [`Self::noise_min_uu`].
    pub noise_max_uu: f32,
}

/// The two enabled emitters, in the system's emitter order: the wide beam
/// first.
pub const EMITTERS: [BeamEmitter; 2] = [
    BeamEmitter {
        size_uu: 4.0,
        source_strength: 1000.0,
        noise_min_uu: -1.0,
        noise_max_uu: 1.0,
    },
    BeamEmitter {
        size_uu: 2.0,
        source_strength: 1250.0,
        noise_min_uu: -1.3,
        noise_max_uu: 6.5,
    },
];
impl BeamEmitter {
    /// The width of the beam's ribbon, UU: twice the size.
    ///
    /// The engine's beam vertex builder writes, for every point of the
    /// beam, one vertex at the point plus and one at the point minus the
    /// unit vector across the beam times the particle's size (times the
    /// taper, 1 here, and the component's scale): the size is the ribbon's
    /// half width. CONFIRMED (native): `FDynamicBeam2EmitterData::
    /// FillData_Noise` @ 0x100A509E0, which `DoBufferFill` @ 0x100A505E0
    /// runs for an emitter with noise, and `FillVertexData_NoNoise`
    /// @ 0x100A546C0 (read locally, nothing quoted). TENTATIVE: that the
    /// component's scale is 1 (the field of view it is given changes its
    /// transform, VFX_DECALS.md §2; whether that reaches the scale the
    /// builder multiplies by was not read).
    #[must_use]
    pub fn width_uu(&self) -> f32 {
        2.0 * self.size_uu
    }
}

/// Beams each emitter keeps alive (`ParticleModuleTypeDataBeam2.
/// MaxBeamCount` 2, with the spawn rate and the bursts at zero: an always-on
/// beam emitter fills up to its count). CONFIRMED (content) for the count,
/// STRONG that both are alive at once (the converted emitters' peak particle
/// count is 4: two beams of two particles' worth).
pub const STRANDS_PER_EMITTER: usize = 2;
/// Noise points along the beam (`ParticleModuleBeamNoise.Frequency`; with
/// `FrequencyDistance` 20 UU the engine uses fewer on a beam shorter than
/// 400 UU, not modelled). CONFIRMED (content).
pub const NOISE_POINTS: usize = 20;
/// The noise points take new places after this long (`NoiseLockTime`), s.
/// CONFIRMED (content).
pub const NOISE_LOCK_TIME: f32 = 0.2;
/// The beam material's emissive factor: `AdventureSuitEffects.MainBeam_Mat`
/// multiplies the sum of its three textures by `GrappleBeamColor` and by
/// this constant. CONFIRMED (content).
pub const EMISSIVE_SCALE: f32 = 20.0;
/// Texture of the material's thin line, sampled through the window
/// [`THIN_UV_SCALE`] / [`THIN_UV_OFFSET`]. CONFIRMED (content).
pub const TEXTURE_THIN: &str = "AdventureSuitEffects.BeamParticle.ThinBeam";
/// Texture of the material's wide core, sampled over the whole ribbon.
/// CONFIRMED (content).
pub const TEXTURE_MAIN: &str = "AdventureSuitEffects.BeamParticle.MainBeam";
/// Texture of the material's travelling pulse, a round soft spot stretched
/// over the beam's length and panned along it. CONFIRMED (content).
pub const TEXTURE_PULSE: &str = "AdventureSuitEffects.BeamSphere";
/// UV scale of the thin line's sample (`VTiling` 0.5). CONFIRMED (content).
pub const THIN_UV_SCALE: [f32; 2] = [1.0, 0.5];
/// UV offset of the thin line's sample. CONFIRMED (content).
pub const THIN_UV_OFFSET: [f32; 2] = [0.0, 0.25];
/// Panner speed of the pulse along the beam, texture lengths a second
/// (`SpeedX` −2: the picture moves from the hand to the anchor twice a
/// second, the beam's texture being laid once along it, `TextureTile` 1).
/// CONFIRMED (content).
pub const PULSE_PAN_SPEED: f32 = -2.0;

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
}

/// The noise of one beam strand: per noise point a 2-D offset across the
/// beam, UU, drawn uniformly from the emitter's range. The points move to
/// new places every [`NOISE_LOCK_TIME`]; in between they glide (the module's
/// `bSmooth`; that the engine's smoothing is a glide over the lock time is
/// TENTATIVE).
#[derive(Clone, Debug, PartialEq)]
pub struct BeamNoise {
    from: Vec<[f32; 2]>,
    to: Vec<[f32; 2]>,
    min: f32,
    max: f32,
    timer: f32,
    rng: RenderRng,
}

impl BeamNoise {
    /// A noise state seeded with `seed` for offsets in `[min, max]` UU (a
    /// reversed or non-finite range gives no noise).
    #[must_use]
    pub fn new(seed: u64, min: f32, max: f32) -> Self {
        let (min, max) = if min.is_finite() && max.is_finite() && min <= max {
            (min, max)
        } else {
            (0.0, 0.0)
        };
        let mut n = Self {
            from: vec![[0.0; 2]; NOISE_POINTS],
            to: vec![[0.0; 2]; NOISE_POINTS],
            min,
            max,
            timer: 0.0,
            rng: RenderRng::new(seed),
        };
        n.randomise();
        n.from.clone_from(&n.to);
        n
    }

    /// The noise of `emitter`'s strand number `strand`.
    #[must_use]
    pub fn for_strand(emitter: &BeamEmitter, strand: u64) -> Self {
        Self::new(
            0x5EED ^ (strand + 1).wrapping_mul(0x9E37_79B9),
            emitter.noise_min_uu,
            emitter.noise_max_uu,
        )
    }

    fn randomise(&mut self) {
        let span = self.max - self.min;
        for o in &mut self.to {
            *o = [
                self.min + span * self.rng.next_f32(),
                self.min + span * self.rng.next_f32(),
            ];
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
            self.from.clone_from(&self.to);
            self.randomise();
        }
    }

    /// The current offsets, UU.
    #[must_use]
    pub fn offsets(&self) -> Vec<[f32; 2]> {
        let a = (self.timer / NOISE_LOCK_TIME).clamp(0.0, 1.0);
        self.from
            .iter()
            .zip(&self.to)
            .map(|(f, t)| [f[0] + (t[0] - f[0]) * a, f[1] + (t[1] - f[1]) * a])
            .collect()
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
/// Target tangent strength (`ParticleModuleBeamTarget.TargetStrength`,
/// constant). CONFIRMED (content).
pub const TARGET_TANGENT_STRENGTH: f32 = 20.0;

/// Points along the beam from `start` to `end`, in any space whose unit is
/// `unit` UU long (the strengths and the noise are in UU).
///
/// The beam emitters interpolate between source and target with tangents
/// (the engine's cubic Hermite interpolation over
/// [`INTERPOLATION_POINTS`]): the beam leaves the hand along `source_dir`,
/// the emitter's own X axis (`SourceTangentMethod` "emitter", tangent
/// (1, 0, 0): the socket's axis, CONFIRMED (content); that the engine
/// normalises it before the strength is applied is TENTATIVE), with
/// `source_strength` ([`BeamEmitter::source_strength`]), and arrives along
/// the chord with [`TARGET_TANGENT_STRENGTH`] (the asset's target tangent
/// is zero and locked: direction TENTATIVE). The noise offsets (linearly
/// interpolated along the beam) displace the points across the chord; both
/// ends are pinned, and the displacement is whole from the first noise point
/// to the last.
#[must_use]
pub fn beam_path(
    start: Vec3,
    end: Vec3,
    source_dir: Vec3,
    source_strength: f32,
    noise: &[[f32; 2]],
    unit: f32,
) -> Vec<Vec3> {
    let axis = end - start;
    let len = axis.length();
    if !len.is_finite() || len < 1e-6 || !unit.is_finite() {
        return vec![start, end];
    }
    let dir = axis / len;
    let helper = if dir.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let side = dir.cross(helper).normalize_or_zero();
    let up = side.cross(dir).normalize_or_zero();
    let strength = if source_strength.is_finite() {
        source_strength * unit
    } else {
        0.0
    };
    let t0 = source_dir.normalize_or_zero() * strength;
    let t1 = dir * (TARGET_TANGENT_STRENGTH * unit);
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
        // Whole between the first and the last noise point, zero at the
        // ends: the noise points lie between source and target.
        let steps = (noise.len() + 1) as f32;
        let pin = (a * steps).min((1.0 - a) * steps).clamp(0.0, 1.0);
        out.push(p + (side * offset[0] + up * offset[1]) * (unit * pin));
    }
    out
}

/// A strip through the beam's points that faces the eye, in the camera's
/// own space (the eye at the origin): positions, texture coordinates (`u`
/// along the beam from 0 to 1, as the beam's `TextureTile` 1 lays the
/// texture once along it; `v` across from 0 to 1), a brightness factor per
/// vertex, and triangle indices.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Ribbon {
    /// Positions.
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Brightness factor of each vertex, `(0, 1]` ([`ribbon`]).
    pub brightness: Vec<f32>,
    /// Triangles.
    pub indices: Vec<u32>,
}

/// Builds the [`Ribbon`] of `points` (camera space) for a beam `width` wide.
///
/// `min_width(depth)` is the narrowest the strip may be drawn `depth` in
/// front of the eye (ours: a strip thinner than a pixel or two breaks up
/// into dots; `crate::vfx` passes a width in pixels). Where the beam is
/// narrower than that it is drawn that wide and dimmer by the same factor,
/// so that it carries the same light: far away the beam thins out to a fine
/// line instead of ending. Points at or behind the eye are left out.
#[must_use]
pub fn ribbon(points: &[Vec3], width: f32, min_width: impl Fn(f32) -> f32) -> Ribbon {
    let mut r = Ribbon::default();
    if points.len() < 2 || !width.is_finite() || width <= 0.0 {
        return r;
    }
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut along = 0.0f32;
    let mut started = false;
    for (i, p) in points.iter().enumerate() {
        if i > 0 {
            along += points[i - 1].distance(*p);
        }
        let depth = -p.z;
        if !p.is_finite() || depth <= 1e-4 {
            // The strip does not bridge a gap: it ends at the first one.
            if started {
                break;
            }
            continue;
        }
        started = true;
        let prev = points[i.saturating_sub(1)];
        let next = points[(i + 1).min(points.len() - 1)];
        let tangent = (next - prev).normalize_or_zero();
        let to_eye = (-*p).normalize_or_zero();
        let mut side = tangent.cross(to_eye).normalize_or_zero();
        if side == Vec3::ZERO {
            side = tangent.any_orthonormal_vector();
        }
        let floor = min_width(depth);
        let drawn = if floor.is_finite() && floor > width {
            floor
        } else {
            width
        };
        let u = if total > 0.0 { along / total } else { 0.0 };
        let h = side * (drawn * 0.5);
        r.positions.push((*p - h).to_array());
        r.positions.push((*p + h).to_array());
        r.uvs.push([u, 0.0]);
        r.uvs.push([u, 1.0]);
        r.brightness.push(width / drawn);
        r.brightness.push(width / drawn);
    }
    let rows = (r.positions.len() / 2) as u32;
    if rows < 2 {
        return Ribbon::default();
    }
    for i in 0..rows - 1 {
        let a = i * 2;
        r.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ribbons are the emitters' size to each side of the centre line:
    /// 8 and 4 UU wide, the wide beam first.
    #[test]
    fn a_beam_is_its_size_to_each_side() {
        assert_eq!(EMITTERS.map(|e| e.size_uu), [4.0, 2.0]);
        assert_eq!(EMITTERS.map(|e| e.width_uu()), [8.0, 4.0]);
        // A ribbon of that width seen face on: its two edges are the size
        // away from the centre line on either side.
        let pts = [Vec3::new(0.0, 0.0, -100.0), Vec3::new(50.0, 0.0, -100.0)];
        let r = ribbon(&pts, EMITTERS[0].width_uu(), |_| 0.0);
        let (a, b) = (
            Vec3::from_array(r.positions[0]),
            Vec3::from_array(r.positions[1]),
        );
        assert!((a.distance(b) - 8.0).abs() < 1e-4, "{a} {b}");
        assert!((a.distance(pts[0]) - EMITTERS[0].size_uu).abs() < 1e-4);
        assert!((b.distance(pts[0]) - EMITTERS[0].size_uu).abs() < 1e-4);
    }

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
        let start = Vec3::ZERO;
        let end = Vec3::new(2000.0, 0.0, 0.0);
        let wide = EMITTERS[0];
        let noise = BeamNoise::for_strand(&wide, 7).offsets();
        // Source tangent along the chord: a straight beam plus noise.
        let p = beam_path(start, end, Vec3::X, wide.source_strength, &noise, 1.0);
        assert_eq!(p.len(), INTERPOLATION_POINTS + 1);
        assert_eq!(p[0], start);
        assert!((*p.last().unwrap() - end).length() < 1e-3);
        let reach = wide.noise_max_uu.abs().max(wide.noise_min_uu.abs());
        for (i, q) in p.iter().enumerate().skip(1) {
            assert!(q.x > p[i - 1].x, "monotonic along the chord");
            let off = (q.y * q.y + q.z * q.z).sqrt();
            assert!(off <= reach * 2f32.sqrt() + 1e-3, "{off}");
        }
        // Without noise and with the tangent off the chord the beam bows
        // towards the source direction and still ends at the anchor.
        let bowed = beam_path(start, end, Vec3::Y, wide.source_strength, &[], 1.0);
        assert!(bowed[INTERPOLATION_POINTS / 4].y > 50.0);
        assert!((*bowed.last().unwrap() - end).length() < 1e-3);
        assert!(bowed.iter().all(|q| q.is_finite()));
        // The narrow beam keeps the asset's stronger source tangent: it bows
        // further (the Hermite tangent term scales with the strength).
        let narrow = beam_path(start, end, Vec3::Y, EMITTERS[1].source_strength, &[], 1.0);
        let k = INTERPOLATION_POINTS / 4;
        assert!((narrow[k].y / bowed[k].y - 1.25).abs() < 1e-3);
        // A non-finite strength gives a straight, finite beam.
        let straight = beam_path(start, end, Vec3::Y, f32::NAN, &[], 1.0);
        assert!(straight.iter().all(|q| q.is_finite() && q.y.abs() < 1e-3));
        // Degenerate beam.
        assert_eq!(
            beam_path(start, start, Vec3::X, wide.source_strength, &noise, 1.0).len(),
            2
        );
    }

    /// The same beam in metres is the beam in UU times the unit: strengths
    /// and noise scale with the space the beam is computed in.
    #[test]
    fn beam_path_scales_with_its_unit() {
        let narrow = EMITTERS[1];
        let noise = BeamNoise::for_strand(&narrow, 3).offsets();
        let end = Vec3::new(300.0, 900.0, -1200.0);
        let dir = Vec3::new(0.2, 0.1, -1.0);
        let uu = beam_path(Vec3::ZERO, end, dir, narrow.source_strength, &noise, 1.0);
        let unit = 0.01;
        let m = beam_path(
            Vec3::ZERO,
            end * unit,
            dir,
            narrow.source_strength,
            &noise,
            unit,
        );
        for (a, b) in uu.iter().zip(&m) {
            assert!((*a * unit).distance(*b) < 1e-3, "{a} {b}");
        }
        // The noise is whole from the first noise point on: the strand is
        // displaced already in its first tenth.
        let quiet = beam_path(Vec3::ZERO, end, dir, narrow.source_strength, &[], 1.0);
        let moved = uu
            .iter()
            .zip(&quiet)
            .take(INTERPOLATION_POINTS / 10 + 1)
            .map(|(a, b)| a.distance(*b))
            .fold(0.0f32, f32::max);
        assert!(moved > 0.0, "{moved}");
    }

    #[test]
    fn noise_glides_to_new_places_every_lock_time() {
        let narrow = EMITTERS[1];
        let mut n = BeamNoise::for_strand(&narrow, 3);
        let first = n.offsets();
        assert_eq!(first.len(), NOISE_POINTS);
        n.update(NOISE_LOCK_TIME * 0.5);
        assert_eq!(n.offsets(), first, "locked until the first lock time");
        n.update(NOISE_LOCK_TIME * 0.6);
        let early = n.offsets();
        assert_ne!(early, first);
        // A tenth into the glide the points have moved a tenth of the way.
        n.update(NOISE_LOCK_TIME * 0.8);
        let late = n.offsets();
        for ((f, e), l) in first.iter().zip(&early).zip(&late) {
            let (de, dl) = (e[0] - f[0], l[0] - f[0]);
            assert!((dl - de * 9.0).abs() < 1e-3 * (1.0 + dl.abs()), "{de} {dl}");
        }
        n.update(f32::NAN);
        n.update(-1.0);
        for _ in 0..50 {
            n.update(NOISE_LOCK_TIME * 0.7);
            assert!(n.offsets().iter().all(|o| {
                o.iter()
                    .all(|c| (narrow.noise_min_uu..=narrow.noise_max_uu).contains(c))
            }));
        }
        // Strands differ; a bad range is no noise.
        assert_ne!(
            BeamNoise::for_strand(&narrow, 0).offsets(),
            BeamNoise::for_strand(&narrow, 1).offsets()
        );
        for (min, max) in [(1.0, -1.0), (f32::NAN, 1.0), (0.0, f32::INFINITY)] {
            assert!(
                BeamNoise::new(1, min, max)
                    .offsets()
                    .iter()
                    .all(|o| *o == [0.0, 0.0])
            );
        }
    }

    #[test]
    fn ribbon_faces_the_eye_and_spans_the_width() {
        // A beam going away from the eye, below it.
        let pts = [Vec3::new(0.0, -1.0, -1.0), Vec3::new(0.0, -1.0, -11.0)];
        let r = ribbon(&pts, 2.0, |_| 0.0);
        assert_eq!(r.positions.len(), 4);
        assert_eq!(r.indices.len(), 6);
        // Side vector perpendicular to the beam and to the eye direction: X.
        let a = Vec3::from_array(r.positions[0]);
        let b = Vec3::from_array(r.positions[1]);
        assert!((a.distance(b) - 2.0).abs() < 1e-5);
        assert!((b - a).normalize().dot(Vec3::X).abs() > 0.999);
        assert_eq!(r.uvs[2], [1.0, 0.0]);
        assert!(r.brightness.iter().all(|k| *k == 1.0));
        assert!(ribbon(&pts[..1], 2.0, |_| 0.0).positions.is_empty());
        assert!(ribbon(&pts, 0.0, |_| 0.0).positions.is_empty());
        // The beam along the view axis: still a valid strip.
        let axis = [Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 0.0, -9.0)];
        let r = ribbon(&axis, 1.0, |_| 0.0);
        assert!(r.positions.iter().all(|p| p.iter().all(|c| c.is_finite())));
    }

    /// Far away the strip is not allowed to get thinner than the floor: it
    /// is drawn that wide and dimmer by the same factor.
    #[test]
    fn ribbon_keeps_a_minimum_width_and_the_light() {
        let pts = [
            Vec3::new(0.2, -0.2, -0.5),
            Vec3::new(0.1, 0.0, -10.0),
            Vec3::new(0.0, 0.5, -40.0),
        ];
        // The floor grows with depth (a pixel's size): 0.002 per metre.
        let r = ribbon(&pts, 0.04, |depth| depth * 0.002);
        let width = |row: usize| {
            Vec3::from_array(r.positions[row * 2])
                .distance(Vec3::from_array(r.positions[row * 2 + 1]))
        };
        assert!((width(0) - 0.04).abs() < 1e-6 && r.brightness[0] == 1.0);
        assert!((width(1) - 0.04).abs() < 1e-6 && r.brightness[2] == 1.0);
        assert!((width(2) - 0.08).abs() < 1e-5);
        assert!((r.brightness[4] - 0.5).abs() < 1e-5 && r.brightness[5] == r.brightness[4]);
        // width × brightness is the beam's own width everywhere.
        for row in 0..3 {
            assert!((width(row) * r.brightness[row * 2] - 0.04).abs() < 1e-5);
        }
        // A non-finite floor is ignored.
        let r = ribbon(&pts, 0.04, |_| f32::NAN);
        assert!(r.brightness.iter().all(|k| *k == 1.0));
    }

    /// Points at or behind the eye are not drawn: the strip starts at the
    /// first point in front of it and ends at a later one that is not.
    #[test]
    fn ribbon_leaves_out_what_is_behind_the_eye() {
        let pts = [
            Vec3::new(0.0, -0.1, 0.2),
            Vec3::new(0.0, -0.1, -0.5),
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, -9.0),
        ];
        let r = ribbon(&pts, 0.1, |_| 0.0);
        assert_eq!(r.positions.len(), 6);
        assert_eq!(r.indices.len(), 12);
        assert!(r.positions.iter().all(|p| p[2] < 0.0));
        // Only one point in front of the eye: nothing to draw.
        let r = ribbon(&pts[..2], 0.1, |_| 0.0);
        assert!(r.positions.is_empty() && r.indices.is_empty());
        let nan = [
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::NAN,
            Vec3::new(0.0, 0.0, -3.0),
        ];
        assert!(ribbon(&nan, 0.1, |_| 0.0).indices.is_empty());
    }
}
