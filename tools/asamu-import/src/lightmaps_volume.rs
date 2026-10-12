//! The level's precomputed light volume: the lighting samples the original
//! bakes for objects that have no light map of their own (movable meshes,
//! characters, the first-person hand). See
//! `docs/reverse-engineering/LIGHTING.md`.
//!
//! A `Level` export stores, in its native tail, an `FPrecomputedLightVolume`
//! (`LEVEL_FORMAT.md`, field 19): bounds, one `f32`, and an array of
//! 33-byte samples. `asamu_ue3::level` checks the block's size and keeps a
//! summary ([`LightVolumeInfo`]); the samples themselves are decoded here:
//!
//! ```text
//! sample (33 bytes, little-endian)
//!   FVector Position           world space, UU
//!   f32     Radius             how far the sample reaches
//!   u8 x 2  IndirectDirection  theta, phi (quantized spherical coordinates)
//!   u8 x 2  EnvironmentDirection
//!   FColor  IndirectRadiance   bytes B, G, R, exponent
//!   FColor  EnvironmentRadiance
//!   FColor  AmbientRadiance
//!   u8      bShadowedFromDominantLights
//! ```
//!
//! The field order and the colour encoding are UE3's (TENTATIVE: not read
//! from the executable); what the shipped data confirms is in the statistics
//! this module computes ([`VolumeStats`]): every position lies inside the
//! volume's stored bounds, every radius is positive and finite and the flag
//! byte is 0 or 1.
//!
//! The block is found inside the export by its own header: the bytes
//! `u32 1`, the bounds, the `f32` and the sample count, exactly as the level
//! reader decoded them, must occur exactly once in the payload and be
//! followed by `count x 33` bytes. Anything else is refused.
//!
//! Output (`<out>/lightmaps/<map>.lightvolume.bin`, user-local): a 16-byte
//! header ([`MAGIC`], record size, sample count) and one 36-byte record per
//! sample ([`RECORD_SIZE`]): position and radius as stored, the three
//! radiances as linear RGB in `R9G9B9E5`, the four direction bytes and the
//! flag as stored.

use anyhow::{Result, bail};
use asamu_ue3::level::{LightVolumeInfo, VOLUME_LIGHTING_SAMPLE_SIZE};
use serde::Serialize;

use super::rgb9e5;

/// First eight bytes of a `.lightvolume.bin` file.
pub const MAGIC: &[u8; 8] = b"ASAMULV1";
/// Bytes per record of a `.lightvolume.bin` file.
pub const RECORD_SIZE: usize = 36;
/// Bytes of the file header.
pub const HEADER_SIZE: usize = 16;
/// Most samples written for one level (the largest shipped level has about
/// 1.1 million; a damaged count must not ask for gigabytes).
pub const MAX_SAMPLES: usize = 8_000_000;

/// One decoded sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeSample {
    /// World position (UU).
    pub position: [f32; 3],
    /// Reach (UU).
    pub radius: f32,
    /// Quantized direction of the indirect light (theta, phi).
    pub indirect_direction: [u8; 2],
    /// Quantized direction of the environment light (theta, phi).
    pub environment_direction: [u8; 2],
    /// Indirect radiance (linear RGB, UE3 light units).
    pub indirect: [f32; 3],
    /// Environment radiance.
    pub environment: [f32; 3],
    /// Ambient radiance.
    pub ambient: [f32; 3],
    /// `bShadowedFromDominantLights` as stored.
    pub shadowed: u8,
}

/// Linear RGB of a colour stored as UE3 writes an `FColor` holding an RGBE
/// value: bytes blue, green, red, exponent; `channel x 2^(exponent - 128) /
/// 255`, and black when the exponent byte is 0.
#[must_use]
pub fn rgbe(bytes: [u8; 4]) -> [f32; 3] {
    let [b, g, r, e] = bytes;
    if e == 0 {
        return [0.0; 3];
    }
    let scale = 2f32.powi(i32::from(e) - 128) / 255.0;
    [
        f32::from(r) * scale,
        f32::from(g) * scale,
        f32::from(b) * scale,
    ]
}

fn f32_at(b: &[u8], at: usize) -> Option<f32> {
    let bytes: [u8; 4] = b.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(f32::from_le_bytes(bytes))
}

fn bytes4(b: &[u8], at: usize) -> Option<[u8; 4]> {
    b.get(at..at.checked_add(4)?)?.try_into().ok()
}

/// Decodes one 33-byte sample (`None` when `b` is shorter).
#[must_use]
pub fn decode_sample(b: &[u8]) -> Option<VolumeSample> {
    if b.len() < VOLUME_LIGHTING_SAMPLE_SIZE {
        return None;
    }
    Some(VolumeSample {
        position: [f32_at(b, 0)?, f32_at(b, 4)?, f32_at(b, 8)?],
        radius: f32_at(b, 12)?,
        indirect_direction: [*b.get(16)?, *b.get(17)?],
        environment_direction: [*b.get(18)?, *b.get(19)?],
        indirect: rgbe(bytes4(b, 20)?),
        environment: rgbe(bytes4(b, 24)?),
        ambient: rgbe(bytes4(b, 28)?),
        shadowed: *b.get(32)?,
    })
}

/// The header the level reader decoded, as bytes: `u32 1`, bounds minimum,
/// bounds maximum, the valid byte, the `f32`, the sample count.
fn block_header(info: &LightVolumeInfo) -> Option<Vec<u8>> {
    let count = u32::try_from(info.samples).ok()?;
    let mut h = Vec::with_capacity(37);
    h.extend_from_slice(&1u32.to_le_bytes());
    for v in info.bounds_min.iter().chain(&info.bounds_max) {
        h.extend_from_slice(&v.to_le_bytes());
    }
    h.push(info.bounds_valid);
    h.extend_from_slice(&info.spacing.to_le_bytes());
    h.extend_from_slice(&count.to_le_bytes());
    Some(h)
}

/// The sample bytes of the light volume inside a `Level` export's payload.
///
/// The block header rebuilt from `info` must occur exactly once and be
/// followed by all the samples; otherwise this is an error (the payload is
/// not the one `info` was read from, or it is damaged).
pub fn locate_samples<'a>(payload: &'a [u8], info: &LightVolumeInfo) -> Result<&'a [u8]> {
    if info.samples > MAX_SAMPLES {
        bail!(
            "the light volume names {} samples (limit {MAX_SAMPLES})",
            info.samples
        );
    }
    let Some(header) = block_header(info) else {
        bail!("the light volume's sample count does not fit its header");
    };
    let Some(bytes) = info.samples.checked_mul(VOLUME_LIGHTING_SAMPLE_SIZE) else {
        bail!("the light volume's size overflows");
    };
    let mut found: Option<usize> = None;
    let mut at = 0usize;
    while let Some(rest) = payload.get(at..) {
        let Some(p) = rest
            .windows(header.len())
            .position(|w| w == header.as_slice())
        else {
            break;
        };
        if found.is_some() {
            bail!("the light volume's header occurs more than once in the level export");
        }
        found = Some(at + p);
        at += p + 1;
    }
    let Some(start) = found else {
        bail!("the light volume's header was not found in the level export");
    };
    let from = start + header.len();
    match from
        .checked_add(bytes)
        .and_then(|end| payload.get(from..end))
    {
        Some(s) => Ok(s),
        None => bail!("the level export ends inside the light volume's samples"),
    }
}

/// One record of the output file.
#[must_use]
pub fn encode_record(s: &VolumeSample) -> [u8; RECORD_SIZE] {
    let mut r = [0u8; RECORD_SIZE];
    let mut put = |at: usize, bytes: [u8; 4]| {
        if let Some(slot) = r.get_mut(at..at + 4) {
            slot.copy_from_slice(&bytes);
        }
    };
    put(0, s.position[0].to_le_bytes());
    put(4, s.position[1].to_le_bytes());
    put(8, s.position[2].to_le_bytes());
    put(12, s.radius.to_le_bytes());
    put(16, rgb9e5(s.indirect).to_le_bytes());
    put(20, rgb9e5(s.environment).to_le_bytes());
    put(24, rgb9e5(s.ambient).to_le_bytes());
    put(
        28,
        [
            s.indirect_direction[0],
            s.indirect_direction[1],
            s.environment_direction[0],
            s.environment_direction[1],
        ],
    );
    put(32, [s.shadowed, 0, 0, 0]);
    r
}

/// What the samples of one level look like (also the checks of the layout).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VolumeStats {
    /// Samples decoded.
    pub samples: usize,
    /// Samples whose position is outside the stored bounds (grown by 1 UU).
    pub outside_bounds: usize,
    /// Samples whose radius is not a positive finite number.
    pub bad_radius: usize,
    /// Samples whose flag byte is neither 0 nor 1.
    pub bad_flag: usize,
    /// Samples flagged as shadowed from the dominant lights.
    pub shadowed: usize,
    /// Smallest, median and largest radius (UU; of the usable radii).
    pub radius: [f32; 3],
    /// Samples with a non-black indirect / environment / ambient radiance.
    pub lit: [usize; 3],
    /// Largest channel of the indirect / environment / ambient radiance.
    pub max_radiance: [f32; 3],
    /// Median of the largest channel of `indirect + ambient` over all samples.
    pub median_indirect_plus_ambient: f32,
}

/// `light_volume` of `<map>.lightmaps.json`.
#[derive(Debug, Clone, Serialize)]
pub struct LightVolumeEntry {
    /// The sample file, relative to `lightmaps/`.
    pub file: String,
    /// Records in the file.
    pub samples: usize,
    /// Bytes per record ([`RECORD_SIZE`]).
    pub record_size: usize,
    /// Stored bounds of the volume (UE3 world space, UU).
    pub bounds_min: [f32; 3],
    /// Stored bounds of the volume.
    pub bounds_max: [f32; 3],
    /// Statistics of the decoded samples.
    pub stats: VolumeStats,
}

fn max3(v: [f32; 3]) -> f32 {
    v[0].max(v[1]).max(v[2])
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// Decodes every sample of `samples` (the bytes [`locate_samples`] found)
/// into the output file's bytes and the statistics. With `write` off only
/// the statistics are computed.
#[must_use]
pub fn convert(samples: &[u8], info: &LightVolumeInfo, write: bool) -> (Vec<u8>, VolumeStats) {
    let count = samples.len() / VOLUME_LIGHTING_SAMPLE_SIZE;
    let mut out = Vec::new();
    if write {
        out.reserve(HEADER_SIZE + count * RECORD_SIZE);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes());
    }
    let mut stats = VolumeStats::default();
    let mut radii = Vec::with_capacity(count);
    let mut levels = Vec::with_capacity(count);
    let inside = |p: [f32; 3]| {
        p.iter()
            .zip(info.bounds_min.iter().zip(&info.bounds_max))
            .all(|(v, (lo, hi))| *v >= *lo - 1.0 && *v <= *hi + 1.0)
    };
    let (chunks, _) = samples.as_chunks::<VOLUME_LIGHTING_SAMPLE_SIZE>();
    for chunk in chunks {
        let Some(s) = decode_sample(chunk) else {
            continue;
        };
        stats.samples += 1;
        stats.outside_bounds += usize::from(!inside(s.position));
        if s.radius.is_finite() && s.radius > 0.0 {
            radii.push(s.radius);
        } else {
            stats.bad_radius += 1;
        }
        stats.bad_flag += usize::from(s.shadowed > 1);
        stats.shadowed += usize::from(s.shadowed == 1);
        for (i, c) in [s.indirect, s.environment, s.ambient]
            .into_iter()
            .enumerate()
        {
            let m = max3(c);
            if let (Some(lit), Some(max)) = (stats.lit.get_mut(i), stats.max_radiance.get_mut(i)) {
                *lit += usize::from(m > 0.0);
                *max = max.max(m);
            }
        }
        levels.push(max3([
            s.indirect[0] + s.ambient[0],
            s.indirect[1] + s.ambient[1],
            s.indirect[2] + s.ambient[2],
        ]));
        if write {
            out.extend_from_slice(&encode_record(&s));
        }
    }
    if !radii.is_empty() {
        radii.sort_by(f32::total_cmp);
        let at = |i: usize| radii.get(i).copied().unwrap_or(0.0);
        stats.radius = [at(0), at(radii.len() / 2), at(radii.len() - 1)];
    }
    stats.median_indirect_plus_ambient = median(&mut levels);
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(samples: usize) -> LightVolumeInfo {
        LightVolumeInfo {
            bounds_min: [-100.0, -200.0, -300.0],
            bounds_max: [100.0, 200.0, 300.0],
            bounds_valid: 1,
            spacing: 0.0,
            samples,
        }
    }

    /// A sample as the engine stores it, written by us.
    fn raw(
        position: [f32; 3],
        radius: f32,
        indirect: [u8; 4],
        ambient: [u8; 4],
        flag: u8,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        for v in position {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&radius.to_le_bytes());
        b.extend_from_slice(&[10, 20, 30, 40]);
        b.extend_from_slice(&indirect);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&ambient);
        b.push(flag);
        assert_eq!(b.len(), VOLUME_LIGHTING_SAMPLE_SIZE);
        b
    }

    /// A level payload: some bytes, the block, some bytes.
    fn payload(info: &LightVolumeInfo, samples: &[u8]) -> Vec<u8> {
        let mut p = vec![7u8; 23];
        p.extend_from_slice(&block_header(info).unwrap());
        p.extend_from_slice(samples);
        p.extend_from_slice(&[9u8; 31]);
        p
    }

    #[test]
    fn rgbe_follows_the_exponent_byte() {
        assert_eq!(rgbe([1, 2, 3, 0]), [0.0; 3]);
        // Exponent 128: the bytes over 255.
        let c = rgbe([51, 102, 255, 128]);
        assert!(
            (c[0] - 1.0).abs() < 1e-6 && (c[1] - 0.4).abs() < 1e-6 && (c[2] - 0.2).abs() < 1e-6
        );
        // One step of the exponent doubles the value.
        let d = rgbe([51, 102, 255, 129]);
        assert!((d[0] - 2.0).abs() < 1e-6);
        let e = rgbe([0, 0, 128, 121]);
        assert!((e[0] - 128.0 / 255.0 / 128.0).abs() < 1e-9);
    }

    #[test]
    fn a_sample_decodes_field_by_field() {
        let b = raw(
            [1.5, -2.5, 3.0],
            640.0,
            [0, 0, 255, 128],
            [255, 0, 0, 127],
            1,
        );
        let s = decode_sample(&b).unwrap();
        assert_eq!(s.position, [1.5, -2.5, 3.0]);
        assert_eq!(s.radius, 640.0);
        assert_eq!(s.indirect_direction, [10, 20]);
        assert_eq!(s.environment_direction, [30, 40]);
        assert_eq!(s.indirect, [1.0, 0.0, 0.0]);
        assert_eq!(s.environment, [0.0; 3]);
        assert_eq!(s.ambient, [0.0, 0.0, 0.5]);
        assert_eq!(s.shadowed, 1);
        // Every shorter input is refused.
        for n in 0..b.len() {
            assert!(decode_sample(&b[..n]).is_none(), "{n}");
        }
    }

    #[test]
    fn the_block_is_found_by_its_header() {
        let mut samples = raw([0.0; 3], 100.0, [0, 0, 255, 128], [0; 4], 0);
        samples.extend(raw(
            [50.0, 0.0, 0.0],
            200.0,
            [0; 4],
            [255, 255, 255, 128],
            1,
        ));
        let i = info(2);
        let p = payload(&i, &samples);
        assert_eq!(locate_samples(&p, &i).unwrap(), samples.as_slice());
        // A different count, bounds or flag is another header: not found.
        assert!(locate_samples(&p, &info(3)).is_err());
        let mut other = i;
        other.bounds_min[0] = -101.0;
        assert!(locate_samples(&p, &other).is_err());
        // The header twice: refused.
        let mut twice = p.clone();
        twice.extend_from_slice(&p);
        assert!(locate_samples(&twice, &i).is_err());
        // Cut anywhere inside the block: refused, never a panic.
        for n in 0..(23 + 37 + samples.len()) {
            assert!(locate_samples(&p[..n], &i).is_err(), "{n}");
        }
        assert!(locate_samples(&p[..23 + 37 + samples.len()], &i).is_ok());
        // An absurd count is refused before anything is searched.
        assert!(locate_samples(&p, &info(MAX_SAMPLES + 1)).is_err());
        assert!(locate_samples(&[], &info(0)).is_err());
    }

    #[test]
    fn an_empty_volume_is_found_and_empty() {
        let i = info(0);
        let p = payload(&i, &[]);
        assert!(locate_samples(&p, &i).unwrap().is_empty());
        let (bin, stats) = convert(&[], &i, true);
        assert_eq!(bin.len(), HEADER_SIZE);
        assert_eq!(stats, VolumeStats::default());
    }

    #[test]
    fn conversion_writes_one_record_per_sample_and_counts() {
        let mut samples = raw([0.0; 3], 100.0, [0, 0, 255, 128], [0; 4], 0);
        samples.extend(raw(
            [50.0, 0.0, 0.0],
            200.0,
            [0; 4],
            [255, 255, 255, 128],
            1,
        ));
        // Outside the bounds, a negative radius, a flag that is no boolean.
        samples.extend(raw([500.0, 0.0, 0.0], -1.0, [0; 4], [0; 4], 7));
        let i = info(3);
        let (bin, stats) = convert(&samples, &i, true);
        assert_eq!(bin.len(), HEADER_SIZE + 3 * RECORD_SIZE);
        assert_eq!(&bin[..8], MAGIC);
        assert_eq!(bin[8..12], (RECORD_SIZE as u32).to_le_bytes());
        assert_eq!(bin[12..16], 3u32.to_le_bytes());
        // First record: position, radius, red 1.0 as R9G9B9E5.
        assert_eq!(bin[16..20], 0f32.to_le_bytes());
        assert_eq!(bin[28..32], 100f32.to_le_bytes());
        assert_eq!(bin[32..36], rgb9e5([1.0, 0.0, 0.0]).to_le_bytes());
        assert_eq!(bin[44..48], [10, 20, 30, 40]);
        assert_eq!(bin[48..52], [0, 0, 0, 0]);
        // Second record's flag.
        assert_eq!(bin[16 + RECORD_SIZE + 32], 1);
        assert_eq!(stats.samples, 3);
        assert_eq!(stats.outside_bounds, 1);
        assert_eq!(stats.bad_radius, 1);
        assert_eq!(stats.bad_flag, 1);
        assert_eq!(stats.shadowed, 1);
        assert_eq!(stats.radius, [100.0, 200.0, 200.0]);
        assert_eq!(stats.lit, [1, 0, 1]);
        assert_eq!(stats.max_radiance, [1.0, 0.0, 1.0]);
        // Statistics alone write nothing.
        let (none, again) = convert(&samples, &i, false);
        assert!(none.is_empty());
        assert_eq!(again, stats);
        // Trailing bytes that are no whole sample are left out.
        let (bin, stats) = convert(&samples[..40], &i, true);
        assert_eq!(bin.len(), HEADER_SIZE + RECORD_SIZE);
        assert_eq!(stats.samples, 1);
    }

    /// Every shipped map (skips without the install): the block is found by
    /// its header, the sample counts are the level reader's, and what the
    /// layout implies holds: usable radii, a flag that is 0 or 1, the
    /// smallest radius 200 x sqrt(2) wherever there are samples, and
    /// positions that reach exactly down to the stored bounds' minimum and
    /// leave the stored bounds only upwards (the stored box is a cube; two
    /// maps have samples above it).
    #[test]
    fn the_shipped_volumes_decode_and_pass_the_layout_checks() {
        use asamu_ue3::level::{self, SceneOptions};
        use asamu_ue3::model::PackageSet;

        let original = std::env::var_os("ASAMU_ORIGINAL_DIR").map(std::path::PathBuf::from);
        let install = match &original {
            Some(d) => asamu_locate::from_original_dir(d),
            None => asamu_locate::locate(),
        };
        let Ok(install) = install else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let cooked = install.cooked_dir;
        let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
        let opts = SceneOptions {
            volume_geometry: false,
            ..SceneOptions::default()
        };
        // (map, samples, samples outside the stored bounds, largest radius).
        type Expected = Option<(usize, usize, f32)>;
        let expected: [(&str, Expected); 12] = [
            ("AG-BeautifulCity", Some((62_767, 40_477, 3464.1016))),
            ("AG-Darkcave", Some((201_664, 0, 4327.0054))),
            ("AG-Epilogue", Some((165, 0, 3464.1016))),
            ("AG-IceCave", Some((301_867, 0, 10_342.171))),
            ("AG-ParadiseCave", Some((1_102_996, 0, 53_949.24))),
            ("AG-StarHaven", Some((105_313, 22, 8730.261))),
            ("AG-Workshop", Some((166, 0, 3464.1016))),
            ("ASAMUEntry", None),
            ("ASAMUFrontEndMap", Some((167, 0, 3464.1016))),
            ("ASAMULegal", None),
            ("Freds_place", Some((0, 0, 0.0))),
            ("TheCore", Some((1203, 0, 282.8427))),
        ];
        let mut total = 0usize;
        for (map, want) in expected {
            let file = cooked.join("Maps").join(format!("{map}.asamu"));
            let lp = set.open_file(&file).unwrap();
            let export = level::level_exports(&lp.package)[0];
            let scene = level::extract_scene(&set, &lp, export, &opts).unwrap();
            let Some((samples, outside, max_radius)) = want else {
                assert!(scene.tail.light_volume.is_none(), "{map}");
                continue;
            };
            let info = scene.tail.light_volume.unwrap();
            assert_eq!(info.samples, samples, "{map}");
            let payload = lp.package.export_data(export).unwrap();
            let bytes = locate_samples(payload, &info).unwrap();
            assert_eq!(bytes.len(), samples * VOLUME_LIGHTING_SAMPLE_SIZE, "{map}");
            let (bin, stats) = convert(bytes, &info, true);
            assert_eq!(bin.len(), HEADER_SIZE + samples * RECORD_SIZE, "{map}");
            assert_eq!(stats.samples, samples, "{map}");
            assert_eq!((stats.bad_radius, stats.bad_flag), (0, 0), "{map}");
            assert_eq!(stats.outside_bounds, outside, "{map}");
            total += samples;
            if samples == 0 {
                continue;
            }
            assert!(
                (stats.radius[0] - 200.0 * 2f32.sqrt()).abs() < 1e-3,
                "{map}"
            );
            assert!(
                (stats.radius[2] - max_radius).abs() < 1e-2,
                "{map}: {:?}",
                stats.radius
            );
            let mut lowest = [f32::INFINITY; 3];
            let (chunks, _) = bytes.as_chunks::<VOLUME_LIGHTING_SAMPLE_SIZE>();
            for chunk in chunks {
                let s = decode_sample(chunk).unwrap();
                for (axis, low) in lowest.iter_mut().enumerate() {
                    *low = low.min(s.position[axis]);
                }
                // Never below the box, never beside it: only above.
                for axis in 0..3 {
                    assert!(s.position[axis] >= info.bounds_min[axis] - 1.0, "{map}");
                }
                for axis in 0..2 {
                    assert!(s.position[axis] <= info.bounds_max[axis] + 1.0, "{map}");
                }
            }
            if map == "AG-BeautifulCity" {
                for axis in 0..3 {
                    assert!(
                        (lowest[axis] - info.bounds_min[axis]).abs() < 1.0,
                        "{lowest:?}"
                    );
                }
            }
        }
        assert_eq!(total, 1_776_308);
    }
}
