//! UE3 light components → physically based render lights (APPROXIMATION).
//!
//! UE3 lights have no physical units: a `PointLightComponent` has a unitless
//! `Brightness`, an sRGB `LightColor`, a `Radius` beyond which it contributes
//! nothing and a `FalloffExponent` shaping the attenuation inside it. Most of
//! the original's look comes from **baked light maps** ([`crate::lightmaps`]);
//! the mapping here serves the lights that are not baked and the objects
//! that have no light map, and can only approximate the original there.
//!
//! The mapping below is ours, chosen so a level reads plausibly under Bevy's
//! default camera exposure; it is not recovered from the original and must
//! never feed gameplay:
//!
//! | UE3 | Render light | Mapping |
//! |---|---|---|
//! | point (`PointLightComponent`, `DominantPointLightComponent`) | point light | luminous power = `Brightness × K_point × Radius_m²`, range = `Radius_m` |
//! | spot (`SpotLightComponent`, `DominantSpotLightComponent`) | spot light | as point; inner/outer half-angles from `InnerConeAngle`/`OuterConeAngle` (degrees, clamped to 0..89.9°) |
//! | directional (`DirectionalLightComponent`, `DominantDirectionalLightComponent`) | directional light | illuminance = `Brightness × K_directional` |
//! | sky (`SkyLightComponent`) | ambient term | ambient brightness += `Brightness × K_sky` |
//!
//! `Radius_m` is the radius converted with the presentation scale. With
//! `K_point = 2500 lm/m²` a light of brightness 1 gives about 800 lux at half
//! its radius, comparable to Bevy's default 1,000,000 lm / 20 m point light.
//! The constants are [`LightMapping`] fields so the app can expose them.
//!
//! # Light for objects without a light map
//!
//! The original lights movable meshes, characters and the first-person hand
//! through a light environment per object: the direct light of the lights
//! that reach it, plus the indirect light its level baked into a
//! **precomputed light volume** (lighting samples spread through the level).
//! [`LightVolume`] reads those samples (`lightmaps/<map>.lightvolume.bin`,
//! written by `asamu-import lightmaps`) and interpolates them at a point
//! with the engine's weighting; [`IncidentLight::irradiance`] reduces the
//! result to one colour (see `docs/reverse-engineering/LIGHTING.md`).

use std::path::Path;

use asamu_core::WorldScale;
use asamu_core::glam::Vec3;

use crate::error::{AssetError, AssetResult};
use crate::files::read_bounded;
use crate::scene::{SceneLight, UeLightKind};

/// Scale factors of the UE3 → render light mapping (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightMapping {
    /// Lumens per unit `Brightness` per square metre of `Radius` (point and
    /// spot lights).
    pub point_lumens_per_brightness_m2: f32,
    /// Lux per unit `Brightness` (directional lights).
    pub directional_lux_per_brightness: f32,
    /// Ambient brightness per unit sky light `Brightness`.
    pub sky_ambient_per_brightness: f32,
    /// Ambient brightness of surfaces without a light map where the level's
    /// precomputed light volume is not converted (a number of ours that
    /// stands in for the baked indirect light; with a volume the app uses
    /// [`LightVolume`] instead).
    pub base_ambient: f32,
}

impl Default for LightMapping {
    fn default() -> Self {
        Self {
            point_lumens_per_brightness_m2: 2500.0,
            directional_lux_per_brightness: 10_000.0,
            sky_ambient_per_brightness: 300.0,
            base_ambient: 150.0,
        }
    }
}

/// What kind of render light a UE3 light becomes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderLightKind {
    /// Point light: luminous power in lumens, range in render units.
    Point {
        /// Luminous power (lm).
        lumens: f32,
        /// Range (render units).
        range: f32,
    },
    /// Spot light.
    Spot {
        /// Luminous power (lm).
        lumens: f32,
        /// Range (render units).
        range: f32,
        /// Inner half-angle (radians).
        inner: f32,
        /// Outer half-angle (radians).
        outer: f32,
    },
    /// Directional light: illuminance in lux.
    Directional {
        /// Illuminance (lx).
        lux: f32,
    },
    /// Ambient contribution.
    Ambient {
        /// Ambient brightness to add.
        brightness: f32,
    },
}

/// A render light in **UE3 coordinates** (the app converts location and
/// direction with `asamu_core::coords`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderLight {
    /// Kind and intensity.
    pub kind: RenderLightKind,
    /// Location (UE3, UU).
    pub location_ue: Vec3,
    /// Direction the light points along (UE3, unit).
    pub direction_ue: Vec3,
    /// sRGB colour bytes (`LightColor` R, G, B).
    pub color_srgb: [u8; 3],
    /// `CastShadows`.
    pub cast_shadows: bool,
}

/// Maps one scene light. Returns `None` for lights with no effect (disabled,
/// zero or negative brightness, unknown class, non-finite values).
#[must_use]
pub fn map_light(
    light: &SceneLight,
    mapping: &LightMapping,
    scale: WorldScale,
) -> Option<RenderLight> {
    if !light.enabled || !(light.brightness.is_finite() && light.brightness > 0.0) {
        return None;
    }
    if !(light.location_ue.is_finite() && light.direction_ue.is_finite()) {
        return None;
    }
    let radius_render = |r: Option<f32>| {
        r.filter(|r| r.is_finite() && *r > 0.0)
            .map(|r| r * scale.bevy_units_per_uu)
    };
    let kind = match light.kind {
        UeLightKind::Point => {
            let range = radius_render(light.radius)?;
            RenderLightKind::Point {
                lumens: light.brightness * mapping.point_lumens_per_brightness_m2 * range * range,
                range,
            }
        }
        UeLightKind::Spot => {
            let range = radius_render(light.radius)?;
            let max = 89.9_f32.to_radians();
            // Missing angles take the `Engine.SpotLightComponent` class
            // defaults (CONFIRMED: `asamu-inspect defaults --inherited
            // Engine.u SpotLightComponent` gives OuterConeAngle 44.0 and no
            // InnerConeAngle, i.e. 0). The importer writes merged values, so
            // this only applies to hand-written or damaged scenes.
            let outer = light
                .outer_cone_angle
                .filter(|a| a.is_finite())
                .unwrap_or(44.0)
                .to_radians()
                .clamp(0.0, max);
            let inner = light
                .inner_cone_angle
                .filter(|a| a.is_finite())
                .unwrap_or(0.0)
                .to_radians()
                .clamp(0.0, outer);
            RenderLightKind::Spot {
                lumens: light.brightness * mapping.point_lumens_per_brightness_m2 * range * range,
                range,
                inner,
                outer,
            }
        }
        UeLightKind::Directional => RenderLightKind::Directional {
            lux: light.brightness * mapping.directional_lux_per_brightness,
        },
        UeLightKind::Sky => RenderLightKind::Ambient {
            brightness: light.brightness * mapping.sky_ambient_per_brightness,
        },
        UeLightKind::Other => return None,
    };
    // Large but finite inputs (or mapping constants) can overflow `f32`; an
    // infinite intensity would poison the renderer's lighting, so such a
    // light is dropped like any other light without a usable effect.
    let finite = match kind {
        RenderLightKind::Point { lumens, range } => lumens.is_finite() && range.is_finite(),
        RenderLightKind::Spot {
            lumens,
            range,
            inner,
            outer,
        } => lumens.is_finite() && range.is_finite() && inner.is_finite() && outer.is_finite(),
        RenderLightKind::Directional { lux } => lux.is_finite(),
        RenderLightKind::Ambient { brightness } => brightness.is_finite(),
    };
    if !finite {
        return None;
    }
    Some(RenderLight {
        kind,
        location_ue: light.location_ue,
        direction_ue: light.direction_ue,
        color_srgb: light.color_srgb,
        cast_shadows: light.cast_shadows,
    })
}

// ------------------------------------------------- precomputed light volume

/// First eight bytes of a `.lightvolume.bin` file.
pub const LIGHT_VOLUME_MAGIC: &[u8; 8] = b"ASAMULV1";
/// Bytes of the file header: the magic, the record size, the record count.
pub const LIGHT_VOLUME_HEADER: usize = 16;
/// Bytes per record.
pub const LIGHT_VOLUME_RECORD: usize = 36;
/// Most samples accepted over all files of a level (the largest shipped
/// level has about 1.1 million).
pub const MAX_LIGHT_VOLUME_SAMPLES: usize = 8_000_000;
/// Largest `.lightvolume.bin` file read.
pub const MAX_LIGHT_VOLUME_BYTES: u64 =
    (LIGHT_VOLUME_HEADER + MAX_LIGHT_VOLUME_SAMPLES * LIGHT_VOLUME_RECORD) as u64;

/// Edge of the lookup grid's cells, UU (ours). Samples that reach no further
/// than this are found through the grid; the few that reach further (the
/// coarse samples far from surfaces) are kept in a list of their own.
const VOLUME_CELL: f32 = 512.0;
/// Cell coordinates are kept within this many cells of the origin.
const VOLUME_CELL_LIMIT: i64 = (1 << 20) - 1;

/// Why a `.lightvolume.bin` file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightVolumeError {
    /// Shorter than a header, or not starting with [`LIGHT_VOLUME_MAGIC`].
    NotAVolume,
    /// A record size other than [`LIGHT_VOLUME_RECORD`].
    RecordSize(u32),
    /// The header's count does not match the bytes that follow.
    Length {
        /// Records the header names.
        records: u32,
        /// Bytes after the header.
        bytes: usize,
    },
    /// More than [`MAX_LIGHT_VOLUME_SAMPLES`] samples.
    TooMany,
}

impl std::fmt::Display for LightVolumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAVolume => write!(f, "not a light volume file"),
            Self::RecordSize(n) => write!(
                f,
                "records of {n} bytes (this reader knows {LIGHT_VOLUME_RECORD})"
            ),
            Self::Length { records, bytes } => write!(
                f,
                "{records} records announced, {bytes} bytes of records present"
            ),
            Self::TooMany => write!(f, "more than {MAX_LIGHT_VOLUME_SAMPLES} samples"),
        }
    }
}

/// Linear RGB of an `R9G9B9E5` value.
#[must_use]
pub fn rgb9e5_to_linear(v: u32) -> [f32; 3] {
    // The exponent field is five bits: the cast cannot lose anything.
    let scale = 2f32.powi((v >> 27) as i32 - 24);
    [
        (v & 511) as f32 * scale,
        ((v >> 9) & 511) as f32 * scale,
        ((v >> 18) & 511) as f32 * scale,
    ]
}

/// The baked light arriving at a point, interpolated from the samples that
/// reach it (linear RGB, UE3 light units).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct IncidentLight {
    /// Radiance of the indirect light along its dominant direction.
    pub indirect: [f32; 3],
    /// Radiance of the light the sample keeps without a direction.
    pub ambient: [f32; 3],
    /// Radiance of the environment (sky) light.
    pub environment: [f32; 3],
    /// Samples that contributed.
    pub samples: usize,
}

impl IncidentLight {
    /// One irradiance for the whole object, whatever way a surface faces:
    /// a quarter of the summed radiances (linear RGB; 1.0 is what a light of
    /// `Brightness` 1 delivers at normal incidence, as in the light maps).
    ///
    /// APPROXIMATION. The original turns a sample into a low-order spherical
    /// function (a lobe along the stored direction plus a constant) and
    /// lights each surface by its own normal; a quarter is what the constant
    /// part of both terms gives a surface under the cosine law (a lobe of
    /// radiance `L` gives `L x max(0, cos)`, whose mean over all directions
    /// is `L / 4`). Checked against the light maps of floors next to
    /// samples: with this factor the two agree at the mode
    /// (`LIGHTING.md`); TENTATIVE.
    #[must_use]
    pub fn irradiance(&self) -> [f32; 3] {
        let mut e = [0.0f32; 3];
        for (i, v) in e.iter_mut().enumerate() {
            let sum = self.indirect.get(i).copied().unwrap_or(0.0)
                + self.ambient.get(i).copied().unwrap_or(0.0)
                + self.environment.get(i).copied().unwrap_or(0.0);
            *v = if sum.is_finite() {
                sum.max(0.0) * 0.25
            } else {
                0.0
            };
        }
        e
    }
}

/// One stored sample.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeSample {
    position: [f32; 3],
    radius: f32,
    /// Indirect, environment, ambient radiance (`R9G9B9E5`).
    light: [u32; 3],
}

/// The precomputed light volume of a level and its merged sub-levels.
#[derive(Debug, Clone, Default)]
pub struct LightVolume {
    /// Samples found through the grid, sorted by cell.
    near: Vec<VolumeSample>,
    /// (cell key, first sample of the cell in `near`), sorted by key.
    cells: Vec<(u64, u32)>,
    /// Samples that reach further than a cell.
    far: Vec<VolumeSample>,
    /// Records skipped: a position or radius that is no usable number.
    pub rejected: usize,
}

fn cell_coord(v: f32) -> i64 {
    // `as` saturates and maps NaN to 0; positions were checked to be finite.
    ((v / VOLUME_CELL).floor() as i64).clamp(-VOLUME_CELL_LIMIT, VOLUME_CELL_LIMIT)
}

fn cell_key(x: i64, y: i64, z: i64) -> u64 {
    let f = |v: i64| {
        u64::try_from(v.clamp(-VOLUME_CELL_LIMIT, VOLUME_CELL_LIMIT) + VOLUME_CELL_LIMIT)
            .unwrap_or(0)
    };
    (f(x) << 42) | (f(y) << 21) | f(z)
}

fn key_of(p: [f32; 3]) -> u64 {
    cell_key(cell_coord(p[0]), cell_coord(p[1]), cell_coord(p[2]))
}

fn le_f32(b: &[u8], at: usize) -> Option<f32> {
    let bytes: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(f32::from_le_bytes(bytes))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

impl LightVolume {
    /// Number of usable samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.near.len() + self.far.len()
    }

    /// True when the volume has no usable sample.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds the samples of one `.lightvolume.bin` file, moved by `offset`
    /// (UE3 units: a streaming sub-level's offset, zero for the persistent
    /// level). Call [`Self::finish`] after the last file. Returns the number
    /// of samples added.
    ///
    /// # Errors
    /// A file that is not a light volume, has another record size, whose
    /// length does not match its header, or that would exceed
    /// [`MAX_LIGHT_VOLUME_SAMPLES`]. Nothing is added then.
    pub fn add_bytes(&mut self, data: &[u8], offset: Vec3) -> Result<usize, LightVolumeError> {
        let magic = data.get(..8).ok_or(LightVolumeError::NotAVolume)?;
        if magic != LIGHT_VOLUME_MAGIC {
            return Err(LightVolumeError::NotAVolume);
        }
        let record = le_u32(data, 8).ok_or(LightVolumeError::NotAVolume)?;
        let records = le_u32(data, 12).ok_or(LightVolumeError::NotAVolume)?;
        if usize::try_from(record) != Ok(LIGHT_VOLUME_RECORD) {
            return Err(LightVolumeError::RecordSize(record));
        }
        let body = data.get(LIGHT_VOLUME_HEADER..).unwrap_or(&[]);
        let count = usize::try_from(records).unwrap_or(usize::MAX);
        if count.checked_mul(LIGHT_VOLUME_RECORD) != Some(body.len()) {
            return Err(LightVolumeError::Length {
                records,
                bytes: body.len(),
            });
        }
        if self.len().saturating_add(count) > MAX_LIGHT_VOLUME_SAMPLES {
            return Err(LightVolumeError::TooMany);
        }
        let offset = if offset.is_finite() {
            offset
        } else {
            Vec3::ZERO
        };
        let mut added = 0usize;
        let (chunks, _) = body.as_chunks::<LIGHT_VOLUME_RECORD>();
        for r in chunks {
            let (Some(x), Some(y), Some(z), Some(radius)) =
                (le_f32(r, 0), le_f32(r, 4), le_f32(r, 8), le_f32(r, 12))
            else {
                continue;
            };
            let (Some(indirect), Some(environment), Some(ambient)) =
                (le_u32(r, 16), le_u32(r, 20), le_u32(r, 24))
            else {
                continue;
            };
            let position = [x + offset.x, y + offset.y, z + offset.z];
            let usable = position.iter().all(|v| v.is_finite())
                && radius.is_finite()
                && radius > 0.0
                && (radius * radius).is_finite();
            if !usable {
                self.rejected += 1;
                continue;
            }
            let sample = VolumeSample {
                position,
                radius,
                light: [indirect, environment, ambient],
            };
            if radius <= VOLUME_CELL {
                self.near.push(sample);
            } else {
                self.far.push(sample);
            }
            added += 1;
        }
        self.cells.clear();
        Ok(added)
    }

    /// Builds the lookup grid. Call once after the last [`Self::add_bytes`].
    pub fn finish(&mut self) {
        self.near.sort_by_key(|s| key_of(s.position));
        self.cells.clear();
        let mut last = None;
        for (i, s) in self.near.iter().enumerate() {
            let k = key_of(s.position);
            if last != Some(k) {
                self.cells.push((k, u32::try_from(i).unwrap_or(u32::MAX)));
                last = Some(k);
            }
        }
    }

    /// Reads one `.lightvolume.bin` file into the volume (see
    /// [`Self::add_bytes`]).
    ///
    /// # Errors
    /// The file cannot be read, is too large, or is refused by
    /// [`Self::add_bytes`].
    pub fn add_file(&mut self, path: &Path, offset: Vec3) -> AssetResult<usize> {
        let data = read_bounded(path, MAX_LIGHT_VOLUME_BYTES)?;
        self.add_bytes(&data, offset)
            .map_err(|e| AssetError::Format {
                path: path.to_path_buf(),
                expected: "a light volume file".to_owned(),
                found: e.to_string(),
            })
    }

    /// The samples of grid cell `key`.
    fn cell(&self, key: u64) -> &[VolumeSample] {
        let Ok(i) = self.cells.binary_search_by_key(&key, |c| c.0) else {
            return &[];
        };
        let start = self.cells.get(i).map_or(0, |c| c.1 as usize);
        let end = self
            .cells
            .get(i + 1)
            .map_or(self.near.len(), |c| c.1 as usize);
        self.near.get(start..end).unwrap_or(&[])
    }

    /// The baked light arriving at `p` (UE3 world space, UU): every sample
    /// whose radius reaches `p`, weighted by `(1 - d² / r²) / r²` (`d` the
    /// distance to the sample, `r` its radius), so a sample fades out at its
    /// radius and small, detailed samples outweigh the coarse ones. `None`
    /// where no sample reaches (the original has no baked light for a
    /// movable object there) and for a point that is no usable number.
    ///
    /// The weighting is the engine's own (STRONG from the engine family's
    /// source convention; not read from the shipped executable).
    #[must_use]
    pub fn interpolate(&self, p: Vec3) -> Option<IncidentLight> {
        if !p.is_finite() {
            return None;
        }
        let mut weight = 0.0f64;
        let mut sums = [[0.0f64; 3]; 3];
        let mut samples = 0usize;
        let mut visit = |s: &VolumeSample| {
            let d = [
                f64::from(s.position[0]) - f64::from(p.x),
                f64::from(s.position[1]) - f64::from(p.y),
                f64::from(s.position[2]) - f64::from(p.z),
            ];
            let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let r2 = f64::from(s.radius) * f64::from(s.radius);
            if d2 >= r2 {
                return;
            }
            let w = (1.0 - d2 / r2) / r2;
            if !(w.is_finite() && w > 0.0) {
                return;
            }
            weight += w;
            samples += 1;
            for (sum, packed) in sums.iter_mut().zip(s.light) {
                let c = rgb9e5_to_linear(packed);
                for (a, b) in sum.iter_mut().zip(c) {
                    *a += w * f64::from(b);
                }
            }
        };
        let (cx, cy, cz) = (cell_coord(p.x), cell_coord(p.y), cell_coord(p.z));
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for s in self.cell(cell_key(cx + dx, cy + dy, cz + dz)) {
                        visit(s);
                    }
                }
            }
        }
        for s in &self.far {
            visit(s);
        }
        if samples == 0 || !(weight.is_finite() && weight > 0.0) {
            return None;
        }
        let mean = |sum: [f64; 3]| {
            let mut out = [0.0f32; 3];
            for (o, v) in out.iter_mut().zip(sum) {
                // Narrowing an average of `f32` values back to `f32`.
                *o = (v / weight) as f32;
            }
            out
        };
        Some(IncidentLight {
            indirect: mean(sums[0]),
            environment: mean(sums[1]),
            ambient: mean(sums[2]),
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(kind: UeLightKind) -> SceneLight {
        SceneLight {
            level: 0,
            actor_slot: 3,
            actor_name: "L".to_owned(),
            light_class: "PointLightComponent".to_owned(),
            kind,
            location_ue: Vec3::new(100.0, 0.0, 50.0),
            direction_ue: Vec3::X,
            brightness: 2.0,
            color_srgb: [255, 128, 0],
            radius: Some(500.0),
            falloff_exponent: Some(2.0),
            inner_cone_angle: Some(20.0),
            outer_cone_angle: Some(120.0),
            enabled: true,
            cast_shadows: true,
        }
    }

    #[test]
    fn point_light_scales_with_brightness_and_radius() {
        let m = LightMapping::default();
        let s = WorldScale::PRESENTATION_METRES;
        let r = map_light(&light(UeLightKind::Point), &m, s).unwrap();
        let RenderLightKind::Point { lumens, range } = r.kind else {
            panic!("{r:?}");
        };
        assert!((range - 10.0).abs() < 1e-5, "500 uu = 10 m");
        assert!((lumens - 2.0 * 2500.0 * 100.0).abs() < 1e-2);
        assert_eq!(r.color_srgb, [255, 128, 0]);
    }

    #[test]
    fn spot_angles_are_clamped() {
        let r = map_light(
            &light(UeLightKind::Spot),
            &LightMapping::default(),
            WorldScale::IDENTITY,
        )
        .unwrap();
        let RenderLightKind::Spot { inner, outer, .. } = r.kind else {
            panic!("{r:?}");
        };
        assert!((outer - 89.9_f32.to_radians()).abs() < 1e-6);
        assert!((inner - 20.0_f32.to_radians()).abs() < 1e-6);
        let mut l = light(UeLightKind::Spot);
        l.inner_cone_angle = Some(80.0);
        l.outer_cone_angle = Some(30.0);
        let r = map_light(&l, &LightMapping::default(), WorldScale::IDENTITY).unwrap();
        let RenderLightKind::Spot { inner, outer, .. } = r.kind else {
            panic!("{r:?}");
        };
        assert!(inner <= outer);
    }

    #[test]
    fn lights_without_effect_are_dropped() {
        let m = LightMapping::default();
        let s = WorldScale::IDENTITY;
        let mut l = light(UeLightKind::Point);
        l.enabled = false;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.brightness = 0.0;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.brightness = f32::NAN;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.radius = None;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.location_ue = Vec3::splat(f32::INFINITY);
        assert!(map_light(&l, &m, s).is_none());
        assert!(map_light(&light(UeLightKind::Other), &m, s).is_none());
    }

    #[test]
    fn overflowing_intensities_are_dropped() {
        let s = WorldScale::IDENTITY;
        for kind in [
            UeLightKind::Point,
            UeLightKind::Spot,
            UeLightKind::Directional,
            UeLightKind::Sky,
        ] {
            let mut l = light(kind);
            l.brightness = f32::MAX;
            l.radius = Some(f32::MAX);
            assert!(
                map_light(&l, &LightMapping::default(), s).is_none(),
                "{kind:?}"
            );
            // A finite light with an overflowing mapping constant too.
            let huge = LightMapping {
                point_lumens_per_brightness_m2: f32::MAX,
                directional_lux_per_brightness: f32::MAX,
                sky_ambient_per_brightness: f32::MAX,
                base_ambient: 0.0,
            };
            assert!(map_light(&light(kind), &huge, s).is_none(), "{kind:?}");
            // Non-finite cone angles fall back to the defaults.
            let mut l = light(kind);
            l.inner_cone_angle = Some(f32::NAN);
            l.outer_cone_angle = Some(f32::INFINITY);
            assert!(
                map_light(&l, &LightMapping::default(), s).is_some(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn directional_and_sky() {
        let m = LightMapping::default();
        let d = map_light(&light(UeLightKind::Directional), &m, WorldScale::IDENTITY).unwrap();
        assert_eq!(d.kind, RenderLightKind::Directional { lux: 20_000.0 });
        let a = map_light(&light(UeLightKind::Sky), &m, WorldScale::IDENTITY).unwrap();
        assert_eq!(a.kind, RenderLightKind::Ambient { brightness: 600.0 });
    }

    /// A `.lightvolume.bin` file written by the test: (position, radius,
    /// indirect, environment, ambient) per record.
    fn volume_file(records: &[([f32; 3], f32, u32, u32, u32)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(LIGHT_VOLUME_MAGIC);
        b.extend_from_slice(&(LIGHT_VOLUME_RECORD as u32).to_le_bytes());
        b.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for (p, r, i, e, a) in records {
            for v in p {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&r.to_le_bytes());
            for v in [i, e, a] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&[0u8; 8]);
        }
        b
    }

    /// `R9G9B9E5` of a grey level that the format holds exactly.
    fn grey(v: f32) -> u32 {
        // mantissa 256 at exponent e is 2^(e - 16).
        let e = (v.log2().round() as i32 + 16).clamp(0, 31) as u32;
        256 | (256 << 9) | (256 << 18) | (e << 27)
    }

    #[test]
    fn rgb9e5_decodes_known_values() {
        assert_eq!(rgb9e5_to_linear(0), [0.0; 3]);
        assert_eq!(rgb9e5_to_linear(grey(1.0)), [1.0; 3]);
        assert_eq!(rgb9e5_to_linear(grey(0.25)), [0.25; 3]);
        // Red 511 at the largest exponent: the format's maximum.
        let max = rgb9e5_to_linear(511 | (31 << 27));
        assert!((max[0] - 65_408.0).abs() < 1e-3 && max[1] == 0.0);
        // Every bit pattern decodes to finite, non-negative values.
        for v in [u32::MAX, 0x8000_0000, 0x7fff_ffff, 1, 1 << 27] {
            assert!(
                rgb9e5_to_linear(v)
                    .iter()
                    .all(|c| c.is_finite() && *c >= 0.0)
            );
        }
    }

    #[test]
    fn a_volume_interpolates_with_the_radius_weighting() {
        let mut v = LightVolume::default();
        let file = volume_file(&[
            ([0.0, 0.0, 0.0], 200.0, grey(1.0), 0, grey(0.5)),
            ([100.0, 0.0, 0.0], 200.0, 0, 0, 0),
            // Reaches further than a cell: kept in the far list.
            ([0.0, 0.0, 5000.0], 6000.0, grey(0.25), grey(0.125), 0),
        ]);
        assert_eq!(v.add_bytes(&file, Vec3::ZERO), Ok(3));
        v.finish();
        assert_eq!(v.len(), 3);
        assert!(!v.is_empty());
        // At the first sample's centre: weights 1/r² for it, (1 - 1/4)/r²
        // for the second, (1 - 25/36)/36e6 for the far one.
        let got = v.interpolate(Vec3::ZERO).unwrap();
        assert_eq!(got.samples, 3);
        let (w0, w1) = (1.0 / 40_000.0, 0.75 / 40_000.0);
        let w2 = (1.0 - 25.0 / 36.0) / 36.0e6;
        let want = (w0 * 1.0 + w2 * 0.25) / (w0 + w1 + w2);
        assert!((f64::from(got.indirect[0]) - want).abs() < 1e-6, "{got:?}");
        let want_ambient = (w0 * 0.5) / (w0 + w1 + w2);
        assert!((f64::from(got.ambient[1]) - want_ambient).abs() < 1e-6);
        let want_env = (w2 * 0.125) / (w0 + w1 + w2);
        assert!((f64::from(got.environment[2]) - want_env).abs() < 1e-9);
        // On a sample's radius its weight is zero: only the others count.
        let edge = v.interpolate(Vec3::new(-200.0, 0.0, 0.0)).unwrap();
        assert_eq!(edge.samples, 1, "only the far sample reaches");
        assert!((edge.indirect[0] - 0.25).abs() < 1e-6);
        // Far from everything: nothing.
        assert!(v.interpolate(Vec3::new(0.0, 50_000.0, 0.0)).is_none());
        assert!(v.interpolate(Vec3::splat(f32::NAN)).is_none());
        assert!(v.interpolate(Vec3::splat(f32::INFINITY)).is_none());
    }

    #[test]
    fn samples_are_found_across_cell_borders_and_with_an_offset() {
        // A sample just left of a cell border is found from the right of it,
        // and from the diagonal neighbour.
        let mut v = LightVolume::default();
        let file = volume_file(&[([-1.0, -1.0, -1.0], 300.0, grey(1.0), 0, 0)]);
        assert_eq!(v.add_bytes(&file, Vec3::ZERO), Ok(1));
        v.finish();
        for p in [
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(100.0, 100.0, 100.0),
            Vec3::new(-250.0, 0.0, 0.0),
        ] {
            assert_eq!(v.interpolate(p).map(|l| l.indirect), Some([1.0; 3]), "{p}");
        }
        assert!(v.interpolate(Vec3::new(300.0, 0.0, 0.0)).is_none());
        // A sub-level's samples move with its streaming offset.
        let mut moved = LightVolume::default();
        assert_eq!(
            moved.add_bytes(&file, Vec3::new(10_000.0, 0.0, -2000.0)),
            Ok(1)
        );
        // A second file is merged in.
        assert_eq!(moved.add_bytes(&file, Vec3::ZERO), Ok(1));
        moved.finish();
        assert!(
            moved
                .interpolate(Vec3::new(10_000.0, 0.0, -2000.0))
                .is_some()
        );
        assert!(moved.interpolate(Vec3::ZERO).is_some());
        assert!(moved.interpolate(Vec3::new(5000.0, 0.0, -1000.0)).is_none());
        // A non-finite offset is not applied.
        let mut odd = LightVolume::default();
        assert_eq!(odd.add_bytes(&file, Vec3::splat(f32::NAN)), Ok(1));
        odd.finish();
        assert!(odd.interpolate(Vec3::ZERO).is_some());
    }

    #[test]
    fn damaged_volume_files_are_refused_or_cleaned() {
        let good = volume_file(&[
            ([0.0; 3], 200.0, grey(1.0), 0, 0),
            ([50.0, 0.0, 0.0], 200.0, grey(1.0), 0, 0),
        ]);
        // Every truncation is an error and adds nothing.
        for n in 0..good.len() {
            let mut v = LightVolume::default();
            assert!(v.add_bytes(&good[..n], Vec3::ZERO).is_err(), "{n}");
            assert!(v.is_empty());
        }
        // One byte too many.
        let mut long = good.clone();
        long.push(0);
        assert!(matches!(
            LightVolume::default().add_bytes(&long, Vec3::ZERO),
            Err(LightVolumeError::Length { records: 2, .. })
        ));
        // Another magic, another record size, an absurd count.
        let mut magic = good.clone();
        magic[0] ^= 0xff;
        assert_eq!(
            LightVolume::default().add_bytes(&magic, Vec3::ZERO),
            Err(LightVolumeError::NotAVolume)
        );
        let mut size = good.clone();
        size[8] = 40;
        assert_eq!(
            LightVolume::default().add_bytes(&size, Vec3::ZERO),
            Err(LightVolumeError::RecordSize(40))
        );
        let mut count = good.clone();
        count[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            LightVolume::default()
                .add_bytes(&count, Vec3::ZERO)
                .is_err()
        );
        // Records with a position or radius that is no usable number are
        // skipped and counted; the rest still answers.
        let odd = volume_file(&[
            ([f32::NAN, 0.0, 0.0], 200.0, grey(1.0), 0, 0),
            ([0.0; 3], -5.0, grey(1.0), 0, 0),
            ([0.0; 3], 0.0, grey(1.0), 0, 0),
            ([0.0; 3], f32::INFINITY, grey(1.0), 0, 0),
            ([0.0; 3], f32::MAX, grey(1.0), 0, 0),
            ([f32::MAX, 0.0, 0.0], 200.0, grey(1.0), 0, 0),
            ([0.0; 3], 200.0, grey(0.5), 0, 0),
        ]);
        let mut v = LightVolume::default();
        assert_eq!(v.add_bytes(&odd, Vec3::ZERO), Ok(2));
        assert_eq!(v.rejected, 5);
        v.finish();
        assert_eq!(
            v.interpolate(Vec3::ZERO).map(|l| l.indirect),
            Some([0.5; 3])
        );
        // Noise in every byte never panics and never yields a non-finite
        // light.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..500 {
            let mut noisy = good.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at =
                    LIGHT_VOLUME_HEADER + (seed as usize) % (noisy.len() - LIGHT_VOLUME_HEADER);
                noisy[at] = (seed >> 32) as u8;
            }
            let mut v = LightVolume::default();
            if v.add_bytes(&noisy, Vec3::ZERO).is_ok() {
                v.finish();
                for p in [Vec3::ZERO, Vec3::splat(100.0), Vec3::splat(-1e9)] {
                    if let Some(l) = v.interpolate(p) {
                        assert!(l.irradiance().iter().all(|c| c.is_finite() && *c >= 0.0));
                    }
                }
            }
        }
    }

    #[test]
    fn irradiance_is_a_quarter_of_the_summed_radiance() {
        let l = IncidentLight {
            indirect: [0.4, 0.0, 1.0],
            ambient: [0.4, 0.2, 0.0],
            environment: [0.0, 0.2, 1.0],
            samples: 1,
        };
        let e = l.irradiance();
        assert!(
            (e[0] - 0.2).abs() < 1e-6 && (e[1] - 0.1).abs() < 1e-6 && (e[2] - 0.5).abs() < 1e-6
        );
        let bad = IncidentLight {
            indirect: [f32::NAN, -1.0, f32::INFINITY],
            ..IncidentLight::default()
        };
        assert_eq!(bad.irradiance(), [0.0; 3]);
        assert_eq!(IncidentLight::default().irradiance(), [0.0; 3]);
    }

    #[test]
    fn a_volume_file_on_disk_is_read_and_a_wrong_one_named() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let good = dir.join("a.lightvolume.bin");
        std::fs::write(&good, volume_file(&[([0.0; 3], 200.0, grey(1.0), 0, 0)])).unwrap();
        let mut v = LightVolume::default();
        assert_eq!(v.add_file(&good, Vec3::ZERO).unwrap(), 1);
        let bad = dir.join("b.lightvolume.bin");
        std::fs::write(&bad, b"not a volume at all").unwrap();
        let err = v.add_file(&bad, Vec3::ZERO).unwrap_err();
        assert!(err.to_string().contains("not a light volume file"), "{err}");
        assert!(v.add_file(&dir.join("missing.bin"), Vec3::ZERO).is_err());
        assert_eq!(v.len(), 1);
    }
}
