//! Texture native data, bulk data records and texel decoding against
//! synthetic fixtures written byte by byte here (no original game data).
//!
//! The fixture package `TexPkg` holds:
//! - `T_Diffuse` (`Texture2D`, 8x8 DXT1): mip 0 LZO-compressed in a `.tfc`,
//!   mip 1 inline, mip 2 inline LZO with its stored size clamped to 4x4;
//! - `LM_0` (`LightMapTexture2D`, 4x4 DXT1, one inline mip, `LightmapFlags` 1);
//! - `Cube` (`TextureCube`) whose six faces `Face_0` ... `Face_5` are 4x4
//!   DXT1 `Texture2D` objects;
//! - `T_Stripped` (`Texture2D`, 8x8 G8) whose mip 0 is unused (stripped).

#![allow(clippy::unwrap_used)]

mod common;

use asamu_ue3::bulkdata::{
    self, BulkCompression, BulkError, BulkStorage, TextureFileCaches, read_bulk_record,
};
use asamu_ue3::reader::Reader;
use asamu_ue3::texture::{
    self, NativeLayout, PIXEL_FORMATS, PixelFormat, TextureClass, TextureCoverage, TextureDecoder,
    TextureError, TextureNative, decode_bc1_block, decode_bc4_block, decode_bc5_block,
    decode_dxt3_block, decode_dxt5_block, decode_to_rgba8, mip_dims,
};
use asamu_ue3::{Package, PackageSet};
use common::{Export, Import, Synth, TAG, W, lzo_literal};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const NAMES: &[&str] = &[
    "None",                 // 0
    "Core",                 // 1
    "Package",              // 2
    "Class",                // 3
    "Engine",               // 4
    "Texture2D",            // 5
    "TexPkg",               // 6
    "T_Diffuse",            // 7
    "SizeX",                // 8
    "IntProperty",          // 9
    "SizeY",                // 10
    "Format",               // 11
    "ByteProperty",         // 12
    "EPixelFormat",         // 13
    "PF_DXT1",              // 14
    "TextureFileCacheName", // 15
    "NameProperty",         // 16
    "Textures",             // 17
    "LightMapTexture2D",    // 18
    "LM_0",                 // 19
    "TextureCube",          // 20
    "Cube",                 // 21
    "FacePosX",             // 22
    "FaceNegX",             // 23
    "FacePosY",             // 24
    "FaceNegY",             // 25
    "FacePosZ",             // 26
    "FaceNegZ",             // 27
    "ObjectProperty",       // 28
    "Face",                 // 29
    "T_Stripped",           // 30
    "PF_G8",                // 31
];

/// Export order (1-based package indices are `index + 1`).
const EX_PKG: usize = 0;
const EX_DIFFUSE: usize = 1;
const EX_LIGHTMAP: usize = 2;
const EX_CUBE: usize = 3;
const EX_FACE0: usize = 4;
const EX_STRIPPED: usize = 10;

const IMP_TEXTURE2D: i32 = -3;
const IMP_LIGHTMAP: i32 = -4;
const IMP_CUBE: i32 = -5;
const IMP_PACKAGE_CLASS: i32 = -6;

/// Where the referenced compressed mip starts in the fixture `.tfc` (after
/// one unreferenced payload).
fn tfc_layout() -> (Vec<u8>, i32, usize) {
    let unreferenced = compressed(&[0x5Au8; 16], 131_072);
    let referenced = compressed(&diffuse_mip0(), 131_072);
    let offset = unreferenced.len();
    let mut file = unreferenced;
    file.extend_from_slice(&referenced);
    (file, offset as i32, referenced.len())
}

/// UE3 compressed payload: header, block table, literal-only LZO blocks.
fn compressed(data: &[u8], block_size: usize) -> Vec<u8> {
    let blocks: Vec<&[u8]> = data.chunks(block_size).collect();
    let encoded: Vec<Vec<u8>> = blocks.iter().map(|b| lzo_literal(b)).collect();
    let csum: usize = encoded.iter().map(Vec::len).sum();
    let mut w = W::default();
    w.u32(TAG);
    w.u32(block_size as u32);
    w.u32(csum as u32);
    w.u32(data.len() as u32);
    for (b, e) in blocks.iter().zip(&encoded) {
        w.u32(e.len() as u32);
        w.u32(b.len() as u32);
    }
    for e in &encoded {
        w.bytes(e);
    }
    w.0
}

/// A DXT1 block of one solid 565 colour.
fn solid_dxt1(c: u16) -> [u8; 8] {
    let [lo, hi] = c.to_le_bytes();
    [lo, hi, lo, hi, 0, 0, 0, 0]
}

fn diffuse_mip0() -> Vec<u8> {
    // 8x8 DXT1: four blocks.
    [0xF800u16, 0x07E0, 0x001F, 0xFFFF]
        .iter()
        .flat_map(|&c| solid_dxt1(c))
        .collect()
}

fn diffuse_mip1() -> Vec<u8> {
    solid_dxt1(0x8410).to_vec()
}

fn diffuse_mip2() -> Vec<u8> {
    solid_dxt1(0x0000).to_vec()
}

fn fname(w: &mut W, index: i32) {
    w.i32(index);
    w.i32(0);
}

fn tag_int(w: &mut W, name: i32, v: i32) {
    fname(w, name);
    fname(w, 9);
    w.i32(4);
    w.i32(0);
    w.i32(v);
}

fn tag_enum(w: &mut W, name: i32, enum_name: i32, value: i32) {
    fname(w, name);
    fname(w, 12);
    w.i32(8);
    w.i32(0);
    fname(w, enum_name);
    fname(w, value);
}

fn tag_name(w: &mut W, name: i32, value: i32) {
    fname(w, name);
    fname(w, 16);
    w.i32(8);
    w.i32(0);
    fname(w, value);
}

fn tag_object(w: &mut W, name: i32, index: i32) {
    fname(w, name);
    fname(w, 28);
    w.i32(4);
    w.i32(0);
    w.i32(index);
}

fn bulk(w: &mut W, flags: u32, count: i32, size: i32, offset: i32) {
    w.u32(flags);
    w.i32(count);
    w.i32(size);
    w.i32(offset);
}

/// Inline bulk record with its data; `base` is the payload's absolute
/// stream offset.
fn bulk_inline(w: &mut W, base: usize, flags: u32, count: usize, stored: &[u8]) {
    let data_at = base + w.len() + 16;
    bulk(w, flags, count as i32, stored.len() as i32, data_at as i32);
    w.bytes(stored);
}

fn texture2d_tail(w: &mut W, base: usize, mips: impl FnOnce(&mut W)) {
    // SourceArt: empty inline record.
    let at = base + w.len() + 16;
    bulk(w, 0, 0, 0, at as i32);
    mips(w);
    for g in [1u32, 2, 3, 4] {
        w.u32(g); // TextureFileCacheGuid
    }
    w.i32(0); // CachedPVRTCMips
    w.i32(0); // CachedFlashMipsMaxResolution
    w.i32(0); // CachedATITCMips
    bulk(w, 0x21, 0, -1, -1); // CachedFlashMips
    w.i32(0); // CachedETCMips
}

fn diffuse_payload(base: usize, tfc_offset: i32, tfc_len: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(2); // NetIndex
    tag_int(&mut w, 8, 8);
    tag_int(&mut w, 10, 8);
    tag_enum(&mut w, 11, 13, 14);
    tag_name(&mut w, 15, 17);
    fname(&mut w, 0);
    texture2d_tail(&mut w, base, |w| {
        w.i32(3);
        bulk(w, 0x11, 32, tfc_len as i32, tfc_offset);
        w.i32(8);
        w.i32(8);
        bulk_inline(w, base, 0, 8, &diffuse_mip1());
        w.i32(4);
        w.i32(4);
        bulk_inline(w, base, 0x10, 8, &compressed(&diffuse_mip2(), 131_072));
        w.i32(4); // 2x2 stored as 4x4
        w.i32(4);
    });
    w.0
}

fn small_dxt1_payload(base: usize, colour: u16, lightmap: bool) -> Vec<u8> {
    let mut w = W::default();
    w.i32(3);
    tag_int(&mut w, 8, 4);
    tag_int(&mut w, 10, 4);
    tag_enum(&mut w, 11, 13, 14);
    fname(&mut w, 0);
    texture2d_tail(&mut w, base, |w| {
        w.i32(1);
        bulk_inline(w, base, 0, 8, &solid_dxt1(colour));
        w.i32(4);
        w.i32(4);
    });
    if lightmap {
        w.u32(1);
    }
    w.0
}

fn stripped_payload(base: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(4);
    tag_int(&mut w, 8, 2);
    tag_int(&mut w, 10, 2);
    tag_enum(&mut w, 11, 13, 31);
    fname(&mut w, 0);
    texture2d_tail(&mut w, base, |w| {
        w.i32(2);
        bulk(w, 0x21, 0, -1, -1);
        w.i32(2);
        w.i32(2);
        bulk_inline(w, base, 0, 1, &[0x80]);
        w.i32(1);
        w.i32(1);
    });
    w.0
}

fn cube_payload(base: usize) -> Vec<u8> {
    let mut w = W::default();
    w.i32(5);
    for (k, name) in (22..=27).enumerate() {
        tag_object(&mut w, name, (EX_FACE0 + k + 1) as i32);
    }
    fname(&mut w, 0);
    let at = base + w.len() + 16;
    bulk(&mut w, 0, 0, 0, at as i32);
    w.0
}

fn synth_with(payloads: &[Vec<u8>]) -> Synth {
    let mut s = Synth::sample();
    s.names = NAMES
        .iter()
        .enumerate()
        .map(|(i, n)| (n.to_string(), 0x0007_0010_0000_0000u64 + i as u64))
        .collect();
    let imp = |cp, cn, outer, name| Import {
        class_package: cp,
        class_name: cn,
        outer,
        name,
        number: 0,
    };
    s.imports = vec![
        imp(1, 2, 0, 1),   // -1 Core
        imp(1, 2, 0, 4),   // -2 Engine
        imp(1, 3, -2, 5),  // -3 Engine.Texture2D
        imp(1, 3, -2, 18), // -4 Engine.LightMapTexture2D
        imp(1, 3, -2, 20), // -5 Engine.TextureCube
        imp(1, 3, -1, 2),  // -6 Core.Package
    ];
    let exp = |class: i32, outer: i32, name: i32, number: i32, payload: &Vec<u8>| Export {
        class,
        super_: 0,
        outer,
        name,
        number,
        archetype: 0,
        object_flags: 0x0007_0004_0000_0000,
        payload: payload.clone(),
        export_flags: 1,
        net_counts: Vec::new(),
        guid: [0; 4],
        package_flags: 0,
    };
    let mut exports = vec![
        exp(IMP_PACKAGE_CLASS, 0, 6, 0, &payloads[EX_PKG]),
        exp(IMP_TEXTURE2D, 1, 7, 0, &payloads[EX_DIFFUSE]),
        exp(IMP_LIGHTMAP, 1, 19, 0, &payloads[EX_LIGHTMAP]),
        exp(IMP_CUBE, 1, 21, 0, &payloads[EX_CUBE]),
    ];
    for k in 0..6 {
        exports.push(exp(
            IMP_TEXTURE2D,
            (EX_CUBE + 1) as i32,
            29,
            k as i32 + 1,
            &payloads[EX_FACE0 + k],
        ));
    }
    exports.push(exp(IMP_TEXTURE2D, 1, 30, 0, &payloads[EX_STRIPPED]));
    s.exports = exports;
    s.additional_packages = Vec::new();
    s.texture_allocations = Vec::new();
    s.depends = true;
    s.package_flags = 0x0000_0008; // cooked
    s
}

fn payloads(bases: &[usize], tfc_offset: i32, tfc_len: usize) -> Vec<Vec<u8>> {
    let face_colours = [0xF800u16, 0x07E0, 0x001F, 0xFFE0, 0x07FF, 0xF81F];
    let mut v = vec![
        vec![0u8; 12],
        diffuse_payload(bases[EX_DIFFUSE], tfc_offset, tfc_len),
        small_dxt1_payload(bases[EX_LIGHTMAP], 0x1234, true),
        cube_payload(bases[EX_CUBE]),
    ];
    for (k, c) in face_colours.iter().enumerate() {
        v.push(small_dxt1_payload(bases[EX_FACE0 + k], *c, false));
    }
    v.push(stripped_payload(bases[EX_STRIPPED]));
    v
}

struct Fixture {
    package: Vec<u8>,
    tfc: Vec<u8>,
    tfc_offset: i32,
}

fn fixture() -> Fixture {
    let (tfc, tfc_offset, tfc_len) = tfc_layout();
    // Payload lengths do not depend on the offsets, so build once to learn
    // the layout, then again with the real absolute offsets.
    let probe = synth_with(&payloads(&[0; 11], tfc_offset, tfc_len));
    let (_, layout) = probe.build();
    let real = synth_with(&payloads(&layout.payload_offsets, tfc_offset, tfc_len));
    let (package, layout2) = real.build();
    assert_eq!(layout.payload_offsets, layout2.payload_offsets);
    Fixture {
        package,
        tfc,
        tfc_offset,
    }
}

fn set_for(f: &Fixture) -> PackageSet {
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("TexPkg", Package::from_bytes(f.package.clone()).unwrap());
    set
}

fn caches_in(dir: &std::path::Path, tfc: &[u8]) -> TextureFileCaches {
    std::fs::write(dir.join("Textures.tfc"), tfc).unwrap();
    TextureFileCaches::discover(&[dir])
}

// ---------------------------------------------------------------------------
// End-to-end decoding
// ---------------------------------------------------------------------------

#[test]
fn texture2d_decodes_and_loads_every_mip() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    assert!(lp.package.issues.is_empty(), "{:?}", lp.package.issues);
    let dir = tempfile::tempdir().unwrap();
    let caches = caches_in(dir.path(), &f.tfc);
    let dec = TextureDecoder::new(&set);

    let tex = dec.decode(&lp, EX_DIFFUSE).unwrap();
    assert_eq!(tex.path, "TexPkg.T_Diffuse");
    assert_eq!(tex.class, TextureClass::Texture2D);
    assert!(!tex.is_default_object);
    assert_eq!(tex.props.size_x, Some(8));
    assert_eq!(tex.props.size_y, Some(8));
    assert_eq!(tex.props.format.as_deref(), Some("PF_DXT1"));
    assert_eq!(tex.format, Some(PixelFormat::Dxt1));
    assert_eq!(tex.props.file_cache_name.as_deref(), Some("Textures"));
    let t = tex.texture2d().unwrap();
    assert_eq!(t.mips.len(), 3);
    assert_eq!(t.lightmap_flags, None);
    assert_eq!(
        t.file_cache_guid.to_string(),
        "00000001000000020000000300000004"
    );
    assert_eq!(t.cached_flash_mips.storage(), BulkStorage::Unused);
    assert_eq!(t.source_art.storage(), BulkStorage::Inline);
    let m = &t.mips;
    assert_eq!(m[0].data.storage(), BulkStorage::SeparateFile);
    assert_eq!(m[0].data.compression(), BulkCompression::Lzo);
    assert_eq!(m[0].data.offset_in_file, f.tfc_offset);
    assert_eq!(m[1].data.storage(), BulkStorage::Inline);
    assert_eq!(m[1].data.compression(), BulkCompression::None);
    assert_eq!(m[2].data.compression(), BulkCompression::Lzo);
    assert_eq!((m[2].size_x, m[2].size_y), (4, 4));
    for mip in &m[1..] {
        assert!(mip.data.inline_offset_matches(tex.serial_offset));
    }
    let payload = lp.package.export_data(EX_DIFFUSE).unwrap();
    assert_eq!(
        tex.load_mip(payload, Some(&caches), 0).unwrap(),
        diffuse_mip0()
    );
    assert_eq!(
        tex.load_mip(payload, Some(&caches), 1).unwrap(),
        diffuse_mip1()
    );
    assert_eq!(
        tex.load_mip(payload, Some(&caches), 2).unwrap(),
        diffuse_mip2()
    );
    assert!(tex.load_mip(payload, Some(&caches), 3).is_err());
    assert_eq!(tex.first_stored_mip(), Some(0));

    // Texels of mip 0: red, green, blue and white 4x4 quadrants.
    let rgba = decode_to_rgba8(PixelFormat::Dxt1, 8, 8, &diffuse_mip0()).unwrap();
    let px = |x: usize, y: usize| &rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
    assert_eq!(px(0, 0), &[255, 0, 0, 255]);
    assert_eq!(px(7, 0), &[0, 255, 0, 255]);
    assert_eq!(px(0, 7), &[0, 0, 255, 255]);
    assert_eq!(px(7, 7), &[255, 255, 255, 255]);
}

#[test]
fn lightmap_cube_and_stripped_textures() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    let dec = TextureDecoder::new(&set);

    let lm = dec.decode(&lp, EX_LIGHTMAP).unwrap();
    assert_eq!(lm.class, TextureClass::LightMapTexture2D);
    assert_eq!(lm.texture2d().unwrap().lightmap_flags, Some(1));
    assert_eq!(lm.props.file_cache_name, None);

    let cube = dec.decode(&lp, EX_CUBE).unwrap();
    assert_eq!(cube.class, TextureClass::TextureCube);
    assert!(matches!(cube.native, TextureNative::SourceArtOnly { .. }));
    assert!(cube.mips().is_empty());
    let faces: Vec<String> = cube
        .props
        .faces
        .iter()
        .map(|f| f.clone().unwrap())
        .collect();
    assert_eq!(faces[0], "TexPkg.Cube.Face_0");
    assert_eq!(faces[5], "TexPkg.Cube.Face_5");
    for (k, path) in faces.iter().enumerate() {
        let i = lp.export_by_qualified(path).unwrap();
        assert_eq!(i, EX_FACE0 + k);
        let face = dec.decode(&lp, i).unwrap();
        assert_eq!(face.mips().len(), 1);
    }

    let s = dec.decode(&lp, EX_STRIPPED).unwrap();
    assert_eq!(s.format, Some(PixelFormat::G8));
    assert_eq!(s.mips()[0].data.storage(), BulkStorage::Unused);
    assert_eq!(s.first_stored_mip(), Some(1));
    let payload = lp.package.export_data(EX_STRIPPED).unwrap();
    assert!(matches!(
        s.load_mip(payload, None, 0),
        Err(TextureError::Bulk(BulkError::NotAvailable(_)))
    ));
    assert_eq!(s.load_mip(payload, None, 1).unwrap(), vec![0x80]);

    // The package export is not a texture.
    assert!(matches!(
        dec.decode(&lp, EX_PKG),
        Err(TextureError::NotATexture { .. })
    ));
}

#[test]
fn coverage_over_the_fixture() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let caches = caches_in(dir.path(), &f.tfc);
    let dec = TextureDecoder::new(&set);
    let mut cov = TextureCoverage::default();
    cov.add_package(&dec, &lp, &caches, true);
    cov.finish(&caches);
    assert_eq!(cov.failure_count, 0, "{:?}", cov.packages[0].failures);
    assert_eq!(cov.classes["Texture2D"].total, 8);
    assert_eq!(cov.classes["Texture2D"].exact, 8);
    assert_eq!(cov.classes["LightMapTexture2D"].exact, 1);
    assert_eq!(cov.classes["TextureCube"].exact, 1);
    let dxt1 = &cov.formats["PF_DXT1"];
    assert_eq!(dxt1.textures, 8);
    assert_eq!(dxt1.mips, 10);
    assert_eq!(dxt1.tfc_mips, 1);
    assert_eq!(dxt1.inline_mips, 9);
    assert_eq!(dxt1.lzo_mips, 2);
    assert_eq!(dxt1.size_ok, 10);
    assert_eq!(dxt1.loaded_ok, 10);
    let g8 = &cov.formats["PF_G8"];
    assert_eq!((g8.mips, g8.unused_mips, g8.loaded_ok), (2, 1, 1));
    assert_eq!(cov.inline_offset_ok, 10);
    assert_eq!(cov.inline_offset_bad, 0);
    assert_eq!(cov.mip_dims_block_clamped, 1);
    assert_eq!(cov.mip_dims_other, 0);
    assert_eq!(cov.inline_lzo_mips, 1);
    assert_eq!(cov.lzo_block_sizes.get(&131_072), Some(&2));
    assert_eq!(cov.lzo_blocks, 2);
    assert_eq!(cov.lightmap_flags.get(&1), Some(&1));
    assert_eq!(cov.mip_flags.get("0x21"), Some(&1));
    assert_eq!(cov.unused_mips_leading, 9);
    // T_Diffuse keeps an 8x8 mip in the .tfc: below the 128 split.
    assert_eq!(cov.streaming_split_other, 1);
    // T_Diffuse is the only texture with a .tfc mip; its FirstResourceMemMip
    // is absent (0) although mip 1 is the first inline one.
    assert_eq!(cov.first_resource_mem_mip_other, 2);
    let tfc = &cov.file_caches["textures"];
    assert_eq!(tfc.records, 1);
    assert_eq!(tfc.distinct_ranges, 1);
    assert_eq!(tfc.out_of_range, 0);
    assert_eq!(tfc.gaps, 1);
    assert_eq!(tfc.gap_payloads, 1);
    assert_eq!(tfc.gap_bytes_tiled, tfc.uncovered_bytes);
    assert_eq!(tfc.uncovered_bytes, f.tfc_offset as u64);
    // Every SourceArt record (nine Texture2D-layout textures and the cube)
    // points at its own stream position.
    assert_eq!(cov.source_art_offset_ok, 10);
    assert_eq!(cov.source_art_offset_bad, 0);
    // LM_0 and the six faces have one mip and no MipTailBaseIdx tag.
    assert_eq!(
        cov.single_mip_tail.iter().collect::<Vec<_>>(),
        [(&"absent".to_owned(), &7)]
    );
}

/// The package bytes of the fixture with every byte flipped (and set to
/// 0xFF) in turn, from the export table on: whatever still parses is decoded,
/// covered and loaded without a panic, and every mip that loads has exactly
/// its `ElementCount` bytes.
#[test]
fn whole_package_mutations_never_panic() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let caches = caches_in(dir.path(), &f.tfc);
    let start = Package::from_bytes(f.package.clone())
        .unwrap()
        .summary
        .export_offset as usize;
    let (mut parsed, mut decoded, mut loaded) = (0usize, 0usize, 0usize);
    for pos in start..f.package.len() {
        for mutation in 0..2 {
            let mut bytes = f.package.clone();
            match mutation {
                0 => bytes[pos] ^= 0x80,
                _ => bytes[pos] = 0xFF,
            }
            let Ok(pkg) = Package::from_bytes(bytes) else {
                continue;
            };
            parsed += 1;
            let set = PackageSet::new::<&str>(&[]);
            let lp = set.insert_package("TexPkg", pkg);
            let dec = TextureDecoder::new(&set);
            let mut cov = TextureCoverage::default();
            cov.add_package(&dec, &lp, &caches, true);
            cov.finish(&caches);
            for i in 0..lp.package.exports.len() {
                let Ok(tex) = dec.decode(&lp, i) else {
                    continue;
                };
                decoded += 1;
                let Ok(payload) = lp.package.export_data(i) else {
                    continue;
                };
                for level in 0..tex.mips().len() {
                    if let Ok(texels) = tex.load_mip(payload, Some(&caches), level) {
                        let count = tex.mips()[level].data.element_count;
                        assert_eq!(texels.len() as i64, i64::from(count));
                        loaded += 1;
                    }
                }
            }
        }
    }
    // Most single-byte mutations still leave a parseable package with
    // decodable textures, so the decoders really see the corrupted bytes.
    eprintln!(
        "{} positions: {parsed} packages parsed, {decoded} textures decoded, {loaded} mips loaded",
        f.package.len() - start
    );
    assert!(parsed > f.package.len() - start);
    assert!(decoded > parsed);
    assert!(loaded > parsed);
}

/// A class default object must end at its tags; trailing bytes are an error,
/// not native data.
#[test]
fn class_default_objects_have_no_native_data() {
    for (junk, ok) in [(&[][..], true), (&[1u8, 2, 3, 4][..], false)] {
        let mut s = synth_with(&payloads(&[0; 11], 0, 0));
        let mut payload = W::default();
        payload.i32(0); // NetIndex
        fname(&mut payload, 0); // None
        payload.bytes(junk);
        let mut cdo = s.exports[EX_DIFFUSE].clone();
        cdo.object_flags |= 0x200; // RF_ClassDefaultObject
        cdo.number = 9;
        cdo.payload = payload.0;
        s.exports.push(cdo);
        let index = s.exports.len() - 1;
        let (bytes, _) = s.build();
        let set = PackageSet::new::<&str>(&[]);
        let lp = set.insert_package("TexPkg", Package::from_bytes(bytes).unwrap());
        let dec = TextureDecoder::new(&set);
        let got = dec.decode(&lp, index);
        if ok {
            let tex = got.unwrap();
            assert!(tex.is_default_object);
            assert!(matches!(tex.native, TextureNative::None));
        } else {
            assert!(matches!(got, Err(TextureError::Malformed(_))), "{got:?}");
        }
    }
}

/// Unreferenced `.tfc` bytes that are not well-formed payloads (garbage, a
/// header that lies about its sizes, a block that does not decompress) end
/// the gap walk without a panic and are not counted.
#[test]
fn hostile_texture_file_cache_gaps() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    let dec = TextureDecoder::new(&set);
    let referenced = &f.tfc[f.tfc_offset as usize..];
    let gap_len = f.tfc_offset as usize;
    let mut lying = W::default();
    lying.u32(TAG);
    lying.u32(bulkdata::MAX_COMPRESSED_BLOCK_SIZE);
    lying.u32(16);
    lying.u32(bulkdata::MAX_BULK_SIZE as u32);
    for _ in 0..16 {
        lying.u32(1);
        lying.u32(bulkdata::MAX_COMPRESSED_BLOCK_SIZE);
    }
    lying.bytes(&[0x11; 16]);
    let mut bad_block = compressed(&[0x5Au8; 16], 131_072);
    let n = bad_block.len();
    bad_block[n - 1] = 0x7F;
    for gap in [vec![0xA5u8; gap_len], lying.0, bad_block] {
        let mut file = gap.clone();
        file.extend_from_slice(referenced);
        // The referenced mip moves with the gap: rebuild the fixture so its
        // record points behind the new gap.
        let (tfc_len, offset) = (referenced.len(), gap.len() as i32);
        let probe = synth_with(&payloads(&[0; 11], offset, tfc_len));
        let (_, layout) = probe.build();
        let real = synth_with(&payloads(&layout.payload_offsets, offset, tfc_len));
        let (package, _) = real.build();
        let set = PackageSet::new::<&str>(&[]);
        let lp2 = set.insert_package("TexPkg", Package::from_bytes(package).unwrap());
        let dec2 = TextureDecoder::new(&set);
        let dir = tempfile::tempdir().unwrap();
        let caches = caches_in(dir.path(), &file);
        let mut cov = TextureCoverage::default();
        cov.add_package(&dec2, &lp2, &caches, true);
        cov.finish(&caches);
        assert_eq!(cov.failure_count, 0, "{:?}", cov.packages[0].failures);
        let tfc = &cov.file_caches["textures"];
        assert_eq!(tfc.uncovered_bytes, gap.len() as u64);
        assert_eq!((tfc.gap_payloads, tfc.gap_bytes_tiled), (0, 0));
    }
    // The original fixture still decodes with the original cache.
    assert!(dec.decode(&lp, EX_DIFFUSE).is_ok());
}

/// Reading from a cache checks the record against the file before reading
/// or allocating anything.
#[test]
fn file_cache_reads_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let caches = caches_in(dir.path(), &[0u8; 64]);
    let rec = |flags: u32, size: i32, offset: i32| bulkdata::BulkDataRecord {
        flags,
        element_count: 0,
        size_on_disk: size,
        offset_in_file: offset,
        header_offset: 0,
    };
    let read = |r: &bulkdata::BulkDataRecord| caches.read_stored("textures", r);
    assert_eq!(read(&rec(0x01, 64, 0)).unwrap().len(), 64);
    assert_eq!(read(&rec(0x01, 0, 64)).unwrap().len(), 0);
    assert!(matches!(
        read(&rec(0x01, 1, 64)),
        Err(BulkError::OutOfRange { .. })
    ));
    assert!(matches!(
        read(&rec(0x01, 16, i32::MAX)),
        Err(BulkError::OutOfRange { .. })
    ));
    assert!(matches!(
        read(&rec(0x01, i32::MAX, 0)),
        Err(BulkError::TooLarge { .. })
    ));
    assert!(matches!(
        read(&rec(0x01, 4, -4)),
        Err(BulkError::Malformed { .. })
    ));
    // Inline and unused records are not read from a cache.
    assert!(matches!(
        read(&rec(0x00, 4, 0)),
        Err(BulkError::NotAvailable(_))
    ));
    assert!(matches!(
        read(&rec(0x21, -1, -1)),
        Err(BulkError::NotAvailable(_))
    ));
    assert!(matches!(
        caches.read_stored("Missing", &rec(0x01, 4, 0)),
        Err(BulkError::FileCache { .. })
    ));
    // Cache names are case-insensitive.
    assert_eq!(
        caches
            .read_stored("TEXTURES", &rec(0x01, 8, 8))
            .unwrap()
            .len(),
        8
    );
}

/// Headers that claim far more data than they carry fail quickly: the block
/// table must fit the bytes, and a lying block cannot decompress.
#[test]
fn compressed_headers_that_lie_about_sizes() {
    // 256 MiB in 1-byte blocks: a block table of 2 GiB that is not there.
    let mut w = W::default();
    w.u32(TAG);
    w.u32(1);
    w.u32(0);
    w.u32(bulkdata::MAX_BULK_SIZE as u32);
    w.bytes(&[0u8; 64]);
    assert!(matches!(
        bulkdata::parse_compressed(&w.0),
        Err(BulkError::Ue3(_))
    ));
    // 256 MiB in sixteen 16 MiB blocks of one compressed byte each.
    let mut w = W::default();
    w.u32(TAG);
    w.u32(bulkdata::MAX_COMPRESSED_BLOCK_SIZE);
    w.u32(16);
    w.u32(bulkdata::MAX_BULK_SIZE as u32);
    for _ in 0..16 {
        w.u32(1);
        w.u32(bulkdata::MAX_COMPRESSED_BLOCK_SIZE);
    }
    w.bytes(&[0x11; 16]);
    let layout = bulkdata::parse_compressed(&w.0).unwrap();
    assert_eq!(layout.total_len(), w.0.len());
    assert!(matches!(
        bulkdata::decompress_lzo(&w.0, bulkdata::MAX_BULK_SIZE),
        Err(BulkError::Lzo { block: 0, .. })
    ));
    // An empty payload is valid and yields nothing.
    let mut w = W::default();
    w.u32(TAG);
    w.u32(131_072);
    w.u32(0);
    w.u32(0);
    assert_eq!(bulkdata::decompress_lzo(&w.0, 0).unwrap(), Vec::<u8>::new());
    assert!(bulkdata::decompress_lzo(&w.0, 1).is_err());
}

#[test]
fn texture_file_cache_failures_are_errors() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    let dec = TextureDecoder::new(&set);
    let tex = dec.decode(&lp, EX_DIFFUSE).unwrap();
    let payload = lp.package.export_data(EX_DIFFUSE).unwrap();

    // No caches at all, and no file of that name.
    assert!(tex.load_mip(payload, None, 0).is_err());
    let empty = tempfile::tempdir().unwrap();
    let none = TextureFileCaches::discover(&[empty.path()]);
    assert!(matches!(
        tex.load_mip(payload, Some(&none), 0),
        Err(TextureError::Bulk(BulkError::FileCache { .. }))
    ));

    // Truncated file: the record lies outside it.
    let dir = tempfile::tempdir().unwrap();
    let short = caches_in(dir.path(), &f.tfc[..f.tfc.len() - 1]);
    assert!(matches!(
        tex.load_mip(payload, Some(&short), 0),
        Err(TextureError::Bulk(BulkError::OutOfRange { .. }))
    ));

    // Corrupted tag and corrupted LZO stream.
    let mut bad_tag = f.tfc.clone();
    bad_tag[f.tfc_offset as usize] ^= 0xFF;
    let dir = tempfile::tempdir().unwrap();
    let c = caches_in(dir.path(), &bad_tag);
    assert!(matches!(
        tex.load_mip(payload, Some(&c), 0),
        Err(TextureError::Bulk(BulkError::BadCompressed(_)))
    ));
    let mut bad_lzo = f.tfc.clone();
    let n = bad_lzo.len();
    bad_lzo[n - 1] = 0x7F; // end marker
    let dir = tempfile::tempdir().unwrap();
    let c = caches_in(dir.path(), &bad_lzo);
    assert!(matches!(
        tex.load_mip(payload, Some(&c), 0),
        Err(TextureError::Bulk(BulkError::Lzo { .. }))
    ));
}

// ---------------------------------------------------------------------------
// Hostile native data
// ---------------------------------------------------------------------------

#[test]
fn truncated_and_corrupted_native_data_never_panics() {
    let f = fixture();
    let set = set_for(&f);
    let lp = set.package("TexPkg").unwrap();
    let dec = TextureDecoder::new(&set);
    let dir = tempfile::tempdir().unwrap();
    let caches = caches_in(dir.path(), &f.tfc);
    for (index, layout) in [
        (EX_DIFFUSE, NativeLayout::Texture2D),
        (EX_LIGHTMAP, NativeLayout::LightMapTexture2D),
        (EX_CUBE, NativeLayout::SourceArtOnly),
        (EX_STRIPPED, NativeLayout::Texture2D),
    ] {
        let tex = dec.decode(&lp, index).unwrap();
        let payload = lp.package.export_data(index).unwrap().to_vec();
        let start = tex.properties_end;
        let (_, end) = texture::read_native(&payload, start, layout).unwrap();
        assert_eq!(end, payload.len());
        // Every truncation fails cleanly.
        for cut in start..payload.len() {
            assert!(
                texture::read_native(&payload[..cut], start, layout).is_err(),
                "export {index} cut at {cut}"
            );
        }
        // Bit flips and extreme values: no panic, and loading whatever
        // decodes stays bounded.
        for pos in start..payload.len() {
            for mutation in 0..3 {
                let mut p = payload.clone();
                match mutation {
                    0 => p[pos] ^= 0x80,
                    1 => {
                        let end = (pos + 4).min(p.len());
                        p[pos..end].fill(0xFF);
                    }
                    _ => {
                        let end = (pos + 4).min(p.len());
                        p[pos..end].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes()[..end - pos]);
                    }
                }
                if let Ok((TextureNative::Texture2D(t), _)) =
                    texture::read_native(&p, start, layout)
                {
                    for mip in &t.mips {
                        let _ = bulkdata::load(&mip.data, &p, Some(&caches), Some("Textures"), 1);
                    }
                }
            }
        }
    }
    // Undecoded layouts are refused.
    assert!(texture::read_native(&[0u8; 64], 0, NativeLayout::Undecoded).is_err());
}

#[test]
fn absurd_mip_counts_are_rejected() {
    // SourceArt, then a mip count larger than the bound, with enough bytes
    // behind it that only the bound can reject it.
    let mut w = W::default();
    bulk(&mut w, 0, 0, 0, 0);
    w.i32(33);
    w.bytes(&vec![0u8; 33 * 24 + 64]);
    assert!(matches!(
        texture::read_native(&w.0, 0, NativeLayout::Texture2D),
        Err(TextureError::Malformed(_))
    ));
    // A negative count and a count larger than the data.
    for count in [-1i32, 1_000_000] {
        let mut w = W::default();
        bulk(&mut w, 0, 0, 0, 0);
        w.i32(count);
        w.bytes(&[0u8; 64]);
        assert!(texture::read_native(&w.0, 0, NativeLayout::Texture2D).is_err());
    }
}

// ---------------------------------------------------------------------------
// Bulk data records and compressed payloads
// ---------------------------------------------------------------------------

#[test]
fn bulk_records_parse() {
    let mut w = W::default();
    bulk_inline(&mut w, 1000, 0, 3, &[7, 8, 9]);
    bulk(&mut w, 0x11, 65_536, 23_342, 572_826);
    bulk(&mut w, 0x21, 0, -1, -1);
    let mut r = Reader::new(&w.0);
    let a = read_bulk_record(&mut r).unwrap();
    assert_eq!(r.position(), 19);
    assert_eq!(a.storage(), BulkStorage::Inline);
    assert_eq!(a.offset_in_file, 1016);
    assert!(a.inline_offset_matches(1000));
    assert_eq!(bulkdata::inline_bytes(&a, &w.0).unwrap(), &[7, 8, 9]);
    let b = read_bulk_record(&mut r).unwrap();
    assert_eq!(b.storage(), BulkStorage::SeparateFile);
    assert_eq!(b.compression(), BulkCompression::Lzo);
    assert_eq!(b.uncompressed_len(1), Some(65_536));
    assert_eq!(b.stored_len(), 23_342);
    let c = read_bulk_record(&mut r).unwrap();
    assert_eq!(c.storage(), BulkStorage::Unused);
    assert!(c.is_empty());
    assert_eq!(c.stored_len(), 0);
    assert_eq!(r.remaining(), 0);
}

#[test]
fn compressed_payloads_decode_exactly() {
    // Three blocks of a 16-byte block size.
    let data: Vec<u8> = (0..40u8).collect();
    let p = compressed(&data, 16);
    let layout = bulkdata::parse_compressed(&p).unwrap();
    assert_eq!(layout.block_size, 16);
    assert_eq!(layout.blocks.len(), 3);
    assert_eq!(layout.blocks[2].1, 8);
    assert_eq!(layout.header_len, 16 + 24);
    assert_eq!(layout.total_len(), p.len());
    assert_eq!(bulkdata::decompress_lzo(&p, 40).unwrap(), data);
    // Wrong expected size, trailing bytes, truncation.
    assert!(bulkdata::decompress_lzo(&p, 39).is_err());
    let mut longer = p.clone();
    longer.push(0);
    assert!(bulkdata::decompress_lzo(&longer, 40).is_err());
    for cut in 0..p.len() {
        assert!(
            bulkdata::decompress_lzo(&p[..cut], 40).is_err(),
            "cut {cut}"
        );
    }
}

#[test]
fn malformed_compressed_headers_are_rejected() {
    let good = compressed(&[1u8; 20], 16);
    // (what, field offset, value written there)
    let cases: [(&str, usize, u32); 9] = [
        ("tag", 0, 0x1234_5678),
        ("zero block size", 4, 0),
        ("huge block size", 4, u32::MAX),
        ("compressed total", 8, 1),
        ("uncompressed total", 12, 21),
        ("huge uncompressed total", 12, u32::MAX),
        ("short first block", 20, 15),
        ("oversized last block", 28, 17),
        ("empty compressed block", 16, 0),
    ];
    for (what, at, value) in cases {
        let mut p = good.clone();
        p[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(bulkdata::parse_compressed(&p).is_err(), "{what}");
        assert!(bulkdata::decompress_lzo(&p, 20).is_err(), "{what}");
    }
    assert!(bulkdata::decompress_lzo(&good, bulkdata::MAX_BULK_SIZE + 1).is_err());
}

#[test]
fn decode_payload_checks_sizes() {
    let mut w = W::default();
    bulk_inline(&mut w, 0, 0, 4, &[1, 2, 3]);
    let rec = read_bulk_record(&mut Reader::new(&w.0)).unwrap();
    // Uncompressed stored size must equal ElementCount.
    assert!(bulkdata::load(&rec, &w.0, None, None, 1).is_err());
    // Element sizes multiply.
    let mut w = W::default();
    bulk_inline(&mut w, 0, 0, 2, &[1, 2, 3, 4]);
    let rec = read_bulk_record(&mut Reader::new(&w.0)).unwrap();
    assert_eq!(
        bulkdata::load(&rec, &w.0, None, None, 2).unwrap(),
        vec![1, 2, 3, 4]
    );
    // zlib and LZX are reported as unsupported.
    for flags in [0x02u32, 0x80] {
        let mut w = W::default();
        bulk_inline(&mut w, 0, flags, 1, &[0]);
        let rec = read_bulk_record(&mut Reader::new(&w.0)).unwrap();
        assert!(matches!(
            bulkdata::load(&rec, &w.0, None, None, 1),
            Err(BulkError::UnsupportedCompression { .. })
        ));
    }
}

// ---------------------------------------------------------------------------
// Pixel formats and texel decoding
// ---------------------------------------------------------------------------

#[test]
fn pixel_format_names_and_math() {
    assert_eq!(PIXEL_FORMATS.len(), 30);
    for (f, name) in PIXEL_FORMATS {
        assert_eq!(PixelFormat::from_enum_name(name), Some(*f));
        assert_eq!(f.enum_name(), *name);
    }
    assert_eq!(PIXEL_FORMATS[5].1, "PF_DXT1");
    assert_eq!(PIXEL_FORMATS[24].1, "PF_BC5");
    assert_eq!(
        PixelFormat::from_enum_name("pf_dxt5"),
        Some(PixelFormat::Dxt5)
    );
    assert_eq!(PixelFormat::from_enum_name("PF_Nope"), None);

    use PixelFormat::*;
    assert_eq!(Dxt1.mip_bytes(1, 1), Some(8));
    assert_eq!(Dxt1.mip_bytes(4, 4), Some(8));
    assert_eq!(Dxt1.mip_bytes(5, 3), Some(16));
    assert_eq!(Dxt1.mip_bytes(696, 780), Some(174 * 195 * 8));
    assert_eq!(Dxt3.mip_bytes(8, 8), Some(64));
    assert_eq!(Dxt5.mip_bytes(2, 2), Some(16));
    assert_eq!(Bc5.mip_bytes(256, 256), Some(65_536));
    assert_eq!(A8R8G8B8.mip_bytes(3, 2), Some(24));
    assert_eq!(G8.mip_bytes(7, 1), Some(7));
    assert_eq!(V8U8.mip_bytes(2, 2), Some(8));
    assert_eq!(Uyvy.mip_bytes(3, 1), Some(8));
    assert_eq!(Unknown.mip_bytes(4, 4), None);
    assert_eq!(D24.layout(), None);
    assert!(Dxt1.is_block_compressed());
    assert!(!G8.is_block_compressed());
    assert_eq!(
        Dxt1.mip_bytes(u32::MAX, u32::MAX),
        Some(9_223_372_036_854_775_808)
    );

    assert_eq!(mip_dims(256, 128, 0), (256, 128));
    assert_eq!(mip_dims(256, 128, 8), (1, 1));
    assert_eq!(mip_dims(256, 128, 7), (2, 1));
    assert_eq!(mip_dims(4, 4, 40), (1, 1));
}

#[test]
fn class_layouts() {
    assert_eq!(
        TextureClass::from_class_name("Engine.LightMapTexture2D"),
        Some(TextureClass::LightMapTexture2D)
    );
    assert_eq!(
        TextureClass::LightMapTexture2D.layout(),
        NativeLayout::LightMapTexture2D
    );
    assert_eq!(
        TextureClass::ShadowMapTexture2D.layout(),
        NativeLayout::Texture2D
    );
    assert_eq!(
        TextureClass::TextureFlipBook.layout(),
        NativeLayout::Texture2D
    );
    assert_eq!(
        TextureClass::TextureCube.layout(),
        NativeLayout::SourceArtOnly
    );
    assert_eq!(TextureClass::TextureMovie.layout(), NativeLayout::Undecoded);
    let chain = vec!["Engine.Texture2D".to_owned(), "Engine.Texture".to_owned()];
    assert_eq!(
        TextureClass::classify("Engine.TerrainWeightMapTexture", &chain),
        Some(TextureClass::Other)
    );
    assert_eq!(TextureClass::Other.layout(), NativeLayout::Undecoded);
    // The base class itself is a texture class (its default object counts).
    assert_eq!(
        TextureClass::classify("Engine.Texture", &[]),
        Some(TextureClass::Other)
    );
    assert_eq!(
        TextureClass::classify("Engine.TextureRenderTarget", &chain[1..]),
        Some(TextureClass::Other)
    );
    assert_eq!(TextureClass::classify("Engine.StaticMesh", &[]), None);
    // A same-named class outside Engine is not taken for the engine class.
    assert_eq!(TextureClass::from_class_name("MyGame.Texture2D"), None);
    assert_eq!(
        TextureClass::from_class_name("texture2d"),
        Some(TextureClass::Texture2D)
    );
    assert_eq!(
        TextureClass::classify("MyGame.Texture2D", &chain),
        Some(TextureClass::Other)
    );
}

/// Pack sixteen 2-bit indices (texel 0 in the low bits).
fn idx2(codes: [u32; 16]) -> [u8; 4] {
    let mut v = 0u32;
    for (i, c) in codes.iter().enumerate() {
        v |= c << (2 * i);
    }
    v.to_le_bytes()
}

/// Pack sixteen 3-bit indices into six bytes.
fn idx3(codes: [u64; 16]) -> [u8; 6] {
    let mut v = 0u64;
    for (i, c) in codes.iter().enumerate() {
        v |= c << (3 * i);
    }
    let b = v.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4], b[5]]
}

#[test]
fn dxt1_known_answers() {
    // Four-colour mode: red (0xF800) > blue (0x001F).
    let mut block = vec![0x00, 0xF8, 0x1F, 0x00];
    block.extend_from_slice(&idx2([0, 1, 2, 3, 3, 2, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1]));
    let t = decode_bc1_block(&block, false);
    assert_eq!(t[0], [255, 0, 0, 255]);
    assert_eq!(t[1], [0, 0, 255, 255]);
    assert_eq!(t[2], [170, 0, 85, 255]);
    assert_eq!(t[3], [85, 0, 170, 255]);
    assert_eq!(t[4], [85, 0, 170, 255]);
    assert_eq!(t[15], [0, 0, 255, 255]);

    // Three-colour mode with transparent black: blue (0x001F) <= red.
    let mut block = vec![0x1F, 0x00, 0x00, 0xF8];
    block.extend_from_slice(&idx2([0, 1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    let t = decode_bc1_block(&block, false);
    assert_eq!(t[0], [0, 0, 255, 255]);
    assert_eq!(t[1], [255, 0, 0, 255]);
    assert_eq!(t[2], [127, 0, 127, 255]);
    assert_eq!(t[3], [0, 0, 0, 0]);
    // The same block in a DXT3/DXT5 colour slot is always four-colour.
    let t = decode_bc1_block(&block, true);
    assert_eq!(t[2], [85, 0, 170, 255]);
    assert_eq!(t[3], [170, 0, 85, 255]);

    // 565 expansion replicates the high bits.
    let t = decode_bc1_block(&[0x10, 0x84, 0x10, 0x84, 0, 0, 0, 0], false);
    assert_eq!(t[0], [132, 130, 132, 255]);
    let t = decode_bc1_block(&[0xE0, 0x07, 0xE0, 0x07, 0, 0, 0, 0], false);
    assert_eq!(t[0], [0, 255, 0, 255]);
}

#[test]
fn bc4_dxt5_dxt3_bc5_known_answers() {
    // Eight-value mode.
    let mut block = vec![255, 0];
    block.extend_from_slice(&idx3([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 7]));
    let a = decode_bc4_block(&block);
    assert_eq!(&a[..8], &[255, 0, 218, 182, 145, 109, 72, 36]);
    assert_eq!(a[15], 36);
    // Six-value mode with explicit 0 and 255.
    let mut block = vec![0, 255];
    block.extend_from_slice(&idx3([0, 1, 2, 3, 4, 5, 6, 7, 0, 0, 0, 0, 0, 0, 0, 0]));
    let a = decode_bc4_block(&block);
    assert_eq!(&a[..8], &[0, 255, 51, 102, 153, 204, 0, 255]);

    // DXT5 = BC4 alpha + four-colour BC1.
    let mut block = vec![255, 0];
    block.extend_from_slice(&idx3([1; 16]));
    block.extend_from_slice(&[0x00, 0xF8, 0x1F, 0x00]);
    block.extend_from_slice(&idx2([0; 16]));
    let t = decode_dxt5_block(&block);
    assert_eq!(t[0], [255, 0, 0, 0]);
    assert_eq!(t[15], [255, 0, 0, 0]);

    // DXT3: explicit 4-bit alpha, low nibble first.
    let mut block = vec![0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE];
    block.extend_from_slice(&[0xE0, 0x07, 0x00, 0x00]);
    block.extend_from_slice(&idx2([0; 16]));
    let t = decode_dxt3_block(&block);
    for (i, texel) in t.iter().enumerate() {
        assert_eq!(texel[3], (i as u8) * 17, "texel {i}");
        assert_eq!(&texel[..3], &[0, 255, 0]);
    }

    // BC5: red then green BC4 blocks; preview Z from the normal.
    let mut block = vec![255, 0];
    block.extend_from_slice(&idx3([0; 16]));
    block.extend_from_slice(&[128, 0]);
    block.extend_from_slice(&idx3([0; 16]));
    let t = decode_bc5_block(&block);
    assert_eq!(&t[0][..2], &[255, 128]);
    assert_eq!(t[0][3], 255);
    // X = +1, Y ~ 0: Z ~ 0, stored as ~128.
    assert!((126..=130).contains(&t[0][2]), "{:?}", t[0]);
}

#[test]
fn rgba_conversion_of_plain_formats() {
    // A8R8G8B8 is stored B, G, R, A.
    let rgba = decode_to_rgba8(PixelFormat::A8R8G8B8, 1, 1, &[1, 2, 3, 4]).unwrap();
    assert_eq!(rgba, vec![3, 2, 1, 4]);
    let rgba = decode_to_rgba8(PixelFormat::G8, 2, 1, &[9, 200]).unwrap();
    assert_eq!(rgba, vec![9, 9, 9, 255, 200, 200, 200, 255]);
    // V8U8: signed (0 = flat), biased into R and G.
    let rgba = decode_to_rgba8(PixelFormat::V8U8, 1, 1, &[0, 0]).unwrap();
    assert_eq!(&rgba[..2], &[128, 128]);
    assert!(rgba[2] >= 254);
    let rgba = decode_to_rgba8(PixelFormat::G16, 1, 1, &[0x34, 0x12]).unwrap();
    assert_eq!(rgba, vec![0x12, 0x12, 0x12, 255]);
    // Partial DXT blocks are cropped.
    let rgba = decode_to_rgba8(PixelFormat::Dxt1, 2, 1, &solid_dxt1(0xF800)).unwrap();
    assert_eq!(rgba, vec![255, 0, 0, 255, 255, 0, 0, 255]);
    // Errors: too few bytes, unsupported format, bad sizes.
    assert!(decode_to_rgba8(PixelFormat::Dxt5, 4, 4, &[0u8; 15]).is_err());
    assert!(decode_to_rgba8(PixelFormat::A32B32G32R32F, 1, 1, &[0u8; 16]).is_err());
    assert!(decode_to_rgba8(PixelFormat::Unknown, 1, 1, &[0u8; 16]).is_err());
    assert!(decode_to_rgba8(PixelFormat::G8, 0, 1, &[]).is_err());
    assert!(decode_to_rgba8(PixelFormat::G8, texture::MAX_DIMENSION + 1, 1, &[]).is_err());
}

/// Every format at every small size: exactly the format's byte count
/// decodes to `w * h` RGBA texels (or is refused as unsupported), one byte
/// less is refused, and nothing panics.
#[test]
fn rgba_conversion_sizes_for_every_format() {
    for (format, name) in PIXEL_FORMATS {
        for w in 1..=9u32 {
            for h in 1..=9u32 {
                let Some(need) = format.mip_bytes(w, h) else {
                    assert!(
                        decode_to_rgba8(*format, w, h, &[0u8; 64]).is_err(),
                        "{name}"
                    );
                    continue;
                };
                let data: Vec<u8> = (0..need).map(|i| (i * 37 % 251) as u8).collect();
                match decode_to_rgba8(*format, w, h, &data) {
                    Ok(rgba) => {
                        assert_eq!(rgba.len() as u32, w * h * 4, "{name} {w}x{h}");
                        let short = &data[..data.len() - 1];
                        assert!(decode_to_rgba8(*format, w, h, short).is_err(), "{name}");
                    }
                    Err(TextureError::UnsupportedFormat(_)) => {}
                    Err(e) => panic!("{name} {w}x{h}: {e}"),
                }
            }
        }
    }
    // Sizes past the bound are refused before anything is allocated.
    let max = texture::MAX_DIMENSION;
    assert!(decode_to_rgba8(PixelFormat::Dxt1, max + 1, 4, &[0u8; 8]).is_err());
    assert!(decode_to_rgba8(PixelFormat::Dxt1, u32::MAX, u32::MAX, &[]).is_err());
}

#[test]
fn block_decoders_tolerate_short_input() {
    // Short slices decode as zero bytes instead of panicking.
    let _ = decode_bc1_block(&[], false);
    let _ = decode_bc4_block(&[1]);
    let _ = decode_dxt3_block(&[1, 2, 3]);
    let _ = decode_dxt5_block(&[]);
    let _ = decode_bc5_block(&[9; 9]);
}
