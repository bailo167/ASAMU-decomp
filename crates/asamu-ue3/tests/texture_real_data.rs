//! Texture decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts
//! are asserted; nothing is copied or written.
//!
//! This is the acceptance test for `docs/reverse-engineering/TEXTURES.md`:
//! every texture export of every package decodes exactly, every mip's bulk
//! record resolves inside its package or `.tfc`, every mip's size matches
//! the format math, and every stored mip loads (and decompresses) to exactly
//! its `ElementCount` bytes.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_ue3::PackageSet;
use asamu_ue3::bulkdata::TextureFileCaches;
use asamu_ue3::texture::{PixelFormat, TextureCoverage, TextureDecoder};

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

fn counts<K: Ord + Clone>(m: &BTreeMap<K, usize>) -> Vec<(K, usize)> {
    m.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

#[test]
fn every_texture_decodes_resolves_and_loads() {
    let dir = require_data!();
    let dirs = [dir.clone(), dir.join("Maps")];
    let set = PackageSet::new(&dirs);
    let caches = TextureFileCaches::discover(&dirs);
    let decoder = TextureDecoder::new(&set);
    let mut cov = TextureCoverage::default();
    for file in packages(&dir) {
        let lp = set.open_file(&file).unwrap();
        cov.add_package(&decoder, &lp, &caches, true);
    }
    cov.finish(&caches);

    for p in &cov.packages {
        assert_eq!(p.failure_count, 0, "{}: {:?}", p.package, p.failures);
    }
    assert_eq!(cov.failure_count, 0);

    // Classes: (exports, class default objects, exact).
    let classes: Vec<(String, usize, usize, usize)> = cov
        .classes
        .iter()
        .map(|(k, c)| (k.clone(), c.total, c.default_objects, c.exact))
        .collect();
    let expect = [
        ("LightMapTexture2D", 5442, 0, 5442),
        ("ShadowMapTexture2D", 220, 1, 219),
        // Engine.Texture itself and its other subclasses ScriptedTexture,
        // TerrainWeightMapTexture, Texture2DComposite, Texture2DDynamic and
        // TextureRenderTarget: default objects only.
        ("Texture (other)", 6, 6, 0),
        ("Texture2D", 3216, 1, 3215),
        ("TextureCube", 23, 1, 22),
        ("TextureFlipBook", 8, 1, 7),
        ("TextureMovie", 1, 1, 0),
        ("TextureRenderTarget2D", 4, 1, 3),
        ("TextureRenderTargetCube", 1, 1, 0),
    ];
    let expect: Vec<(String, usize, usize, usize)> = expect
        .iter()
        .map(|(k, a, b, c)| ((*k).to_owned(), *a, *b, *c))
        .collect();
    assert_eq!(classes, expect);
    assert!(cov.classes.values().all(|c| c.failed == 0));

    // Formats: every stored mip has the format's size and loads exactly.
    let formats: Vec<&str> = cov.formats.keys().map(String::as_str).collect();
    assert_eq!(
        formats,
        ["PF_A8R8G8B8", "PF_DXT1", "PF_DXT5", "PF_G8", "PF_V8U8"]
    );
    let mut textures = 0;
    for (name, f) in &cov.formats {
        assert!(PixelFormat::from_enum_name(name).is_some());
        assert_eq!(f.inline_mips + f.tfc_mips + f.unused_mips, f.mips, "{name}");
        assert_eq!(f.size_ok, f.mips - f.unused_mips, "{name}");
        assert_eq!(f.loaded_ok, f.size_ok, "{name}");
        assert_eq!(
            f.lzo_mips,
            f.tfc_mips
                + if name == "PF_DXT1" || name == "PF_DXT5" {
                    2
                } else {
                    0
                }
        );
        textures += f.textures;
    }
    assert_eq!(textures, 8883);
    let per_format: Vec<(&str, usize, usize)> = cov
        .formats
        .iter()
        .map(|(k, f)| (k.as_str(), f.textures, f.mips))
        .collect();
    assert_eq!(
        per_format,
        [
            ("PF_A8R8G8B8", 156, 1522),
            ("PF_DXT1", 7639, 64492),
            ("PF_DXT5", 839, 5791),
            ("PF_G8", 236, 1849),
            ("PF_V8U8", 13, 129),
        ]
    );

    // Bulk records.
    assert_eq!(
        counts(&cov.mip_flags),
        [
            ("0x00".to_owned(), 58831),
            ("0x10".to_owned(), 4),
            ("0x11".to_owned(), 14706),
            ("0x21".to_owned(), 242),
        ]
    );
    assert_eq!(cov.inline_lzo_mips, 4);
    // Every LZO payload (14,706 in .tfc files + 4 inline) uses 128 KiB blocks.
    assert_eq!(counts(&cov.lzo_block_sizes), [(131_072, 14_710)]);
    assert_eq!(cov.lzo_blocks, 23_967);
    assert_eq!(counts(&cov.source_art_flags), [("0x00".to_owned(), 8908)]);
    assert_eq!(cov.source_art_non_empty, 0);
    assert_eq!(counts(&cov.cached_flash_flags), [("0x21".to_owned(), 8883)]);
    assert_eq!(cov.cached_platform_mips_non_empty, 0);
    assert_eq!(cov.cached_flash_resolution_non_zero, 621);
    assert_eq!(counts(&cov.lightmap_flags), [(0, 54), (1, 5388)]);
    assert_eq!(cov.inline_offset_ok, 58835);
    assert_eq!(cov.inline_offset_bad, 0);
    // The empty SourceArt record of every texture (8,883 with mips, 22 cubes,
    // 3 render targets) also points at its own stream position.
    assert_eq!(cov.source_art_offset_ok, 8908);
    assert_eq!(cov.source_art_offset_bad, 0);

    // Mip chains.
    assert_eq!(cov.mip_dims_natural, 55649);
    assert_eq!(cov.mip_dims_block_clamped, 18134);
    assert_eq!(cov.mip_dims_other, 0);
    assert_eq!(cov.mip0_matches_size, 8883);
    assert_eq!(cov.first_resource_mem_mip_ok, 8883);
    assert_eq!(cov.single_mip_textures, 366);
    // Single-mip textures: MipTailBaseIdx is absent, the last index of the
    // full chain their size would have, or (5 UI textures) one more.
    assert_eq!(
        counts(&cov.single_mip_tail),
        [
            ("absent".to_owned(), 280),
            ("full chain".to_owned(), 81),
            ("full chain + 1".to_owned(), 5),
        ]
    );
    assert_eq!(cov.mip_tail_is_last, 8517);
    assert_eq!(cov.mip_tail_other, 0);
    assert_eq!(cov.full_chains, 8517);
    assert_eq!(cov.partial_chains, 0);
    assert_eq!(cov.unused_mips_leading, 8883);
    assert_eq!(cov.tfc_mips_leading, 8883);
    assert_eq!(cov.streaming_split_ok, 8883);
    assert_eq!(cov.streaming_split_other, 0);
    // Leading mips are stripped exactly when a multi-mip texture is larger
    // than its LOD group's MaxLODSize in [SystemSettings] of
    // DefaultSystemSettings.ini (1024 for World, WorldNormalMap,
    // WorldSpecular and MobileFlattened; 2048 for the others listed).
    let stripped: Vec<(String, usize)> = counts(&cov.stripped_mips_by_group);
    let expect = [
        ("TEXTUREGROUP_Character 2048 unused=0", 19),
        ("TEXTUREGROUP_Cinematic 2048 unused=0", 41),
        ("TEXTUREGROUP_MobileFlattened 2048 unused=1", 4),
        ("TEXTUREGROUP_Skybox 2048 unused=0", 12),
        ("TEXTUREGROUP_Weapon 2048 unused=0", 8),
        ("TEXTUREGROUP_WeaponNormalMap 2048 unused=0", 3),
        ("TEXTUREGROUP_WeaponSpecular 2048 unused=0", 1),
        ("TEXTUREGROUP_World 2048 unused=1", 155),
        ("TEXTUREGROUP_World 4096 unused=2", 2),
        ("TEXTUREGROUP_WorldNormalMap 2048 unused=1", 78),
        ("TEXTUREGROUP_WorldSpecular 2048 unused=1", 1),
    ];
    let expect: Vec<(String, usize)> = expect.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
    assert_eq!(stripped, expect);

    // Texture file caches.
    assert_eq!(cov.textures_with_tfc_mips, 5933);
    assert_eq!(cov.tfc_without_name, 0);
    assert_eq!(cov.zero_guid, 0);
    assert_eq!(cov.range_guid_conflicts, 0);
    let tfc: Vec<(&str, usize, usize, usize)> = cov
        .file_caches
        .iter()
        .map(|(k, f)| (k.as_str(), f.records, f.distinct_ranges, f.gap_payloads))
        .collect();
    assert_eq!(
        tfc,
        [
            ("chartextures", 320, 320, 6),
            ("lighting", 5983, 5983, 0),
            ("textures", 8403, 4573, 53),
        ]
    );
    for (name, f) in &cov.file_caches {
        assert_eq!(f.out_of_range, 0, "{name}");
        assert_eq!(f.overlapping_ranges, 0, "{name}");
        assert_eq!(f.gap_bytes_tiled, f.uncovered_bytes, "{name}");
        assert_eq!(f.covered_bytes + f.uncovered_bytes, f.file_len, "{name}");
    }

    // A8R8G8B8 alpha lane: the stored fourth byte is the only lane that is
    // ever constant across a whole mip more often than the colour lanes.
    assert_eq!(cov.argb_textures, 145);
    assert!(cov.argb_lane_constant[3] > cov.argb_lane_constant[0]);
}

/// The storage order of `PF_A8R8G8B8` texels, from a colour-grading lookup
/// table: a 256 x 16 strip of sixteen 16 x 16 slices where red grows along x
/// inside a slice, green along y, and blue from slice to slice. Byte lane 2
/// follows red, lane 1 green and lane 0 blue: texels are stored B, G, R, A.
#[test]
fn a8r8g8b8_is_stored_bgra() {
    let dir = require_data!();
    let dirs = [dir.clone(), dir.join("Maps")];
    let set = PackageSet::new(&dirs);
    let caches = TextureFileCaches::discover(&dirs);
    let decoder = TextureDecoder::new(&set);
    let lp = set
        .open_file(&dir.join("Maps").join("AG-BeautifulCity.asamu"))
        .unwrap();
    let index = lp
        .export_by_qualified("MapTemplates.lut.LUT_Night")
        .unwrap();
    let tex = decoder.decode(&lp, index).unwrap();
    assert_eq!(tex.format, Some(PixelFormat::A8R8G8B8));
    assert_eq!((tex.props.size_x, tex.props.size_y), (Some(256), Some(16)));
    let payload = lp.package.export_data(index).unwrap();
    let texels = tex.load_mip(payload, Some(&caches), 0).unwrap();
    assert_eq!(texels.len(), 256 * 16 * 4);

    let corr = |lane: usize, axis: &dyn Fn(usize) -> f64| {
        let n = 256 * 16;
        let xs: Vec<f64> = (0..n).map(|i| f64::from(texels[i * 4 + lane])).collect();
        let ys: Vec<f64> = (0..n).map(axis).collect();
        let mx = xs.iter().sum::<f64>() / n as f64;
        let my = ys.iter().sum::<f64>() / n as f64;
        let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        let sx = xs.iter().map(|x| (x - mx).powi(2)).sum::<f64>().sqrt();
        let sy = ys.iter().map(|y| (y - my).powi(2)).sum::<f64>().sqrt();
        sxy / (sx * sy)
    };
    let red_axis = |i: usize| ((i % 256) % 16) as f64;
    let green_axis = |i: usize| (i / 256) as f64;
    let blue_axis = |i: usize| ((i % 256) / 16) as f64;
    assert!(corr(2, &red_axis) > 0.9);
    assert!(corr(1, &green_axis) > 0.9);
    assert!(corr(0, &blue_axis) > 0.9);
    assert!(corr(0, &red_axis).abs() < 0.2);
    assert!(corr(2, &blue_axis).abs() < 0.2);
}
