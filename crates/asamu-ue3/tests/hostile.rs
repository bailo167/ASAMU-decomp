//! Targeted hostile-input tests: negative and huge summary fields, offsets that
//! point outside the stream, self-referential outer chains, the outer-depth
//! limit, thumbnail tables, chunk-table layout attacks and resource caps.
//!
//! Every fixture is synthetic and written byte by byte in `common`.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

mod common;

use asamu_ue3::{MAX_OUTER_DEPTH, Package, PackageIndex, ReadOptions, Severity, Summary, Ue3Error};
use common::*;

fn parse(bytes: &[u8]) -> Result<Package, Ue3Error> {
    let opts = ReadOptions {
        threads: 1,
        ..Default::default()
    };
    let r = Package::from_bytes_with(bytes.to_vec(), &opts);
    if let Ok(p) = &r {
        exercise(p);
    }
    r
}

fn has_warning(p: &Package, needle: &str) -> bool {
    p.issues
        .iter()
        .any(|i| i.severity == Severity::Warning && i.message.contains(needle))
}

// ------------------------------------------------------------------ thumbnails

#[test]
fn thumbnail_tables_parse_in_both_storage_modes() {
    let synth = Synth::with_thumbnails();
    let (plain, layout) = synth.build();
    let (comp, stream) = synth.build_compressed(150, 64);
    for (label, bytes) in [("uncompressed", plain.clone()), ("compressed", comp)] {
        let p = parse(&bytes).unwrap();
        assert!(p.issues.is_empty(), "{label}: {:?}", p.issues);
        let s = &p.summary;
        assert_eq!(s.thumbnail_table_offset as usize, layout.thumb_table_offset);
        assert_eq!(s.import_export_guids_offset as usize, layout.ieguids_offset);
        let t = p.thumbnails.as_ref().unwrap();
        assert_eq!(t.len(), 2, "{label}");
        assert_eq!(t[0].class_name, "Texture2D");
        assert_eq!(t[0].object_path, "TestPkg.TestActor");
        assert_eq!((t[0].width, t[0].height), (64, 32));
        assert_eq!(t[0].data_size as usize, synth.thumbnails[0].data.len());
        assert_eq!(t[0].offset as usize, layout.thumb_offsets[0]);
        assert_eq!(t[1].class_name, "World");
        assert_eq!((t[1].width, t[1].height, t[1].data_size), (0, 0, 0));
        assert_eq!(
            p.extents.thumbnail_data,
            Some((layout.ieguids_offset, layout.thumb_table_offset))
        );
        assert_eq!(
            p.extents.thumbnail_table,
            Some((layout.thumb_table_offset, layout.header_end))
        );
        for (i, e) in synth.exports.iter().enumerate() {
            assert_eq!(p.export_data(i).unwrap(), &e.payload[..], "{label}");
        }
    }
    assert_eq!(parse(&plain).unwrap().stream(), &plain[..]);
    assert_eq!(
        parse(&synth.build_compressed(150, 64).0).unwrap().stream(),
        &stream[..]
    );
}

#[test]
fn hostile_thumbnail_tables_are_soft_failures() {
    let synth = Synth::with_thumbnails();
    let (plain, layout) = synth.build();
    let table = layout.thumb_table_offset;
    // Entry 0's FileOffset: count(4) + FString "Texture2D"(4+10) + FString
    // "TestPkg.TestActor"(4+18).
    let entry0_offset = table + 4 + 14 + 22;
    assert_eq!(
        get_i32(&plain, entry0_offset) as usize,
        layout.thumb_offsets[0]
    );

    for v in [plain.len() as i32, plain.len() as i32 + 1, i32::MAX, -1] {
        let mut b = plain.clone();
        put_i32(&mut b, entry0_offset, v);
        let p = parse(&b).unwrap();
        assert!(p.thumbnails.is_none(), "record offset {v}");
        assert!(has_warning(&p, "thumbnail table not parsed"), "{v}");
    }
    // Huge entry count and huge image size.
    for (off, v) in [
        (table, i32::MAX),
        (table, -1),
        (layout.thumb_offsets[0] + 8, i32::MAX),
        (layout.thumb_offsets[0] + 8, -7),
    ] {
        let mut b = plain.clone();
        put_i32(&mut b, off, v);
        let p = parse(&b).unwrap();
        assert!(p.thumbnails.is_none(), "{off:#x} = {v}");
        assert!(has_warning(&p, "thumbnail table not parsed"));
    }
    // Table offset beyond the stream: warning, never an error.
    for v in [plain.len() as i32 + 1, i32::MAX] {
        let mut b = plain.clone();
        put_i32(&mut b, OFF_THUMBNAIL_TABLE_OFFSET, v);
        let p = parse(&b).unwrap();
        assert!(p.thumbnails.is_none());
        assert!(has_warning(&p, "ThumbnailTableOffset"));
    }
}

// ------------------------------------------------------------------ summary fields

#[test]
fn negative_summary_fields_are_rejected() {
    let (plain, _) = Synth::sample().build();
    let (comp, _) = Synth::sample().build_compressed(200, 64);
    let fields = [
        ("TotalHeaderSize", OFF_TOTAL_HEADER_SIZE),
        ("NameCount", OFF_NAME_COUNT),
        ("NameOffset", OFF_NAME_OFFSET),
        ("ExportCount", OFF_EXPORT_COUNT),
        ("ExportOffset", OFF_EXPORT_OFFSET),
        ("ImportCount", OFF_IMPORT_COUNT),
        ("ImportOffset", OFF_IMPORT_OFFSET),
        ("DependsOffset", OFF_DEPENDS_OFFSET),
        ("ImportExportGuidsOffset", OFF_IEGUIDS_OFFSET),
        ("ImportGuidsCount", OFF_IMPORT_GUIDS_COUNT),
        ("ExportGuidsCount", OFF_EXPORT_GUIDS_COUNT),
        ("ThumbnailTableOffset", OFF_THUMBNAIL_TABLE_OFFSET),
    ];
    for (name, off) in fields {
        for v in [-1, i32::MIN, -0x7FFF_0000] {
            let mut b = plain.clone();
            put_i32(&mut b, off, v);
            match parse(&b) {
                Err(Ue3Error::InvalidValue { what, value, .. }) => {
                    assert_eq!(what, name);
                    assert_eq!(value, i64::from(v));
                }
                other => panic!("{name} = {v}: {other:?}"),
            }
        }
    }
    // Negative chunk-entry fields.
    for k in 0..4 {
        let mut b = comp.clone();
        put_i32(&mut b, OFF_FIRST_CHUNK + 4 * k, -1);
        assert!(
            matches!(parse(&b), Err(Ue3Error::InvalidValue { .. })),
            "chunk field {k}"
        );
    }
    // Negative array counts.
    for off in [OFF_GENERATION_COUNT, OFF_CHUNK_COUNT] {
        let mut b = comp.clone();
        put_i32(&mut b, off, -1);
        assert!(matches!(parse(&b), Err(Ue3Error::InvalidValue { .. })));
    }
}

#[test]
fn table_offsets_outside_the_stream() {
    let (plain, layout) = Synth::sample().build();
    let len = plain.len() as i32;
    for off in [OFF_NAME_OFFSET, OFF_IMPORT_OFFSET, OFF_EXPORT_OFFSET] {
        for v in [len, len + 1, len + 4096, i32::MAX] {
            let mut b = plain.clone();
            put_i32(&mut b, off, v);
            match parse(&b) {
                Err(
                    Ue3Error::OutOfBounds { .. }
                    | Ue3Error::CountTooLarge { .. }
                    | Ue3Error::UnexpectedEof { .. },
                ) => {}
                other => panic!("offset field {off:#x} = {v}: {other:?}"),
            }
        }
        // Pointing at the last few bytes: the table cannot fit.
        let mut b = plain.clone();
        put_i32(&mut b, off, len - 3);
        assert!(parse(&b).is_err());
    }
    // Soft offsets: depends map, GUID offset and header size.
    for v in [len + 1, i32::MAX] {
        let mut b = plain.clone();
        put_i32(&mut b, OFF_DEPENDS_OFFSET, v);
        let p = parse(&b).unwrap();
        assert!(p.depends.is_none());
        assert!(has_warning(&p, "depends map not parsed"));
        assert!(has_warning(&p, "DependsOffset"));

        let mut b = plain.clone();
        put_i32(&mut b, OFF_IEGUIDS_OFFSET, v);
        let p = parse(&b).unwrap();
        assert!(has_warning(&p, "ImportExportGuidsOffset"));

        let mut b = plain.clone();
        put_i32(&mut b, OFF_TOTAL_HEADER_SIZE, v);
        let p = parse(&b).unwrap();
        assert!(p.has_errors(), "TotalHeaderSize {v} beyond the stream");
    }
    // Depends map pointing into the export table: parses as garbage at best,
    // never panics, and is flagged.
    let mut b = plain.clone();
    put_i32(&mut b, OFF_DEPENDS_OFFSET, layout.export_offset as i32);
    let p = parse(&b).unwrap();
    assert!(!p.issues.is_empty());
    // Non-zero GUID counts are reported, not parsed.
    let mut b = plain.clone();
    put_i32(&mut b, OFF_IMPORT_GUIDS_COUNT, 3);
    let p = parse(&b).unwrap();
    assert!(has_warning(&p, "GUID records present"));
}

#[test]
fn huge_counts_inside_tables() {
    let synth = Synth::sample();
    let (plain, layout) = synth.build();
    // Export 0's GenerationNetObjectCount count field: export_offset + 44.
    let gen_count = layout.export_offset + 44;
    assert_eq!(get_i32(&plain, gen_count), 1);
    for v in [i32::MAX, 0x1000_0000, -1] {
        let mut b = plain.clone();
        put_i32(&mut b, gen_count, v);
        assert!(
            matches!(
                parse(&b),
                Err(Ue3Error::CountTooLarge { .. } | Ue3Error::InvalidValue { .. })
            ),
            "{v}"
        );
    }
    // First name's FString length.
    for v in [i32::MAX, i32::MIN, -0x4000_0000, 0x4000_0000] {
        let mut b = plain.clone();
        put_i32(&mut b, layout.name_offset, v);
        assert!(parse(&b).is_err(), "name length {v}");
    }
    // A depends-map entry with a huge count is a soft failure.
    let mut b = plain.clone();
    put_i32(&mut b, layout.depends_offset, i32::MAX);
    let p = parse(&b).unwrap();
    assert!(p.depends.is_none());
    assert!(has_warning(&p, "depends map not parsed"));
}

#[test]
fn reference_fields_with_extreme_values() {
    // Every package-index field of every export and import set to extreme
    // values must give a clean BadPackageIndex (or parse, for valid values).
    let base = Synth::sample();
    for v in [i32::MIN, i32::MIN + 1, -6, 6, i32::MAX] {
        for field in 0..4 {
            for k in 0..base.exports.len() {
                let mut s = base.clone();
                let e = &mut s.exports[k];
                match field {
                    0 => e.class = v,
                    1 => e.super_ = v,
                    2 => e.outer = v,
                    _ => e.archetype = v,
                }
                assert!(
                    matches!(
                        parse(&s.build().0),
                        Err(Ue3Error::BadPackageIndex { index, .. }) if index == v
                    ),
                    "export {k} field {field} = {v}"
                );
            }
        }
        for k in 0..base.imports.len() {
            let mut s = base.clone();
            s.imports[k].outer = v;
            assert!(matches!(
                parse(&s.build().0),
                Err(Ue3Error::BadPackageIndex { .. })
            ));
        }
    }
    // Name indices in every FName slot.
    for v in [-1, i32::MIN, NAMES.len() as i32, i32::MAX] {
        for slot in 0..3 {
            let mut s = base.clone();
            match slot {
                0 => s.imports[0].class_package = v,
                1 => s.imports[0].class_name = v,
                _ => s.imports[0].name = v,
            }
            assert!(matches!(
                parse(&s.build().0),
                Err(Ue3Error::BadNameIndex { index, .. }) if index == v
            ));
        }
    }
    // Extreme FName numbers are fine (display only).
    let mut s = base.clone();
    s.exports[4].number = i32::MAX;
    s.exports[3].number = i32::MIN;
    let p = parse(&s.build().0).unwrap();
    assert_eq!(
        p.export_path(4).unwrap(),
        format!("TheWorld.Obj_{}", i32::MAX - 1)
    );
    assert_eq!(p.export_path(3).unwrap(), "TheWorld.Obj");
}

// ------------------------------------------------------------------ outer chains

#[test]
fn self_referential_outer_chains() {
    // Import that is its own outer.
    let mut s = Synth::sample();
    s.imports[0].outer = -1;
    let p = parse(&s.build().0).unwrap();
    assert!(matches!(
        p.import_path(0),
        Err(Ue3Error::OuterChainTooDeep { start: -1, .. })
    ));
    // Engine.PlayerStart's root walks through the looping import? No: its
    // outer is Engine (-2), which is fine.
    assert_eq!(p.import_path(2).unwrap(), "Engine.PlayerStart");
    // Core.Package's outer is the looping import.
    assert!(p.import_path(4).is_err());
    // Export 0's class is Core.Package (import 4) whose chain loops.
    assert!(p.export_class_package(0).is_err());

    // Import <-> export cycle.
    let mut s = Synth::sample();
    s.imports[1].outer = 3; // Engine -> TheWorld (export 2)
    s.exports[2].outer = -2; // TheWorld -> Engine
    let p = parse(&s.build().0).unwrap();
    assert!(p.import_path(1).is_err());
    assert!(p.export_path(2).is_err());
    assert!(p.export_path(4).is_err()); // TheWorld.Obj walks into the cycle
    assert!(p.is_inside(PackageIndex(5), PackageIndex(3)).is_err());
    assert!(p.root_name(PackageIndex(-3)).is_err());

    // A class that is its own class, super and archetype: structurally valid
    // references; names still resolve, nothing loops.
    let mut s = Synth::sample();
    s.exports[1].class = 2;
    s.exports[1].super_ = 2;
    s.exports[1].archetype = 2;
    let p = parse(&s.build().0).unwrap();
    assert_eq!(p.export_class_name(1).unwrap(), "TestActor");
    assert_eq!(
        p.export_class_package(1).unwrap().as_deref(),
        Some("TestPkg")
    );
    assert_eq!(p.object_path(PackageIndex(2)).unwrap(), "TestPkg.TestActor");

    // Every export in one big cycle.
    let mut s = Synth::sample();
    let n = s.exports.len() as i32;
    for (k, e) in s.exports.iter_mut().enumerate() {
        e.outer = (k as i32 + 1) % n + 1;
    }
    let p = parse(&s.build().0).unwrap();
    for k in 0..p.exports.len() {
        assert!(p.export_path(k).is_err());
    }
    assert_eq!(p.top_level_exports().count(), 0);
}

/// A linear chain of exports `Obj -> Obj_0 -> Obj_1 -> ...`, each the outer of
/// the next.
fn chain_package(len: usize) -> Synth {
    let mut s = Synth::sample();
    let template = s.exports[4].clone();
    s.exports = (0..len)
        .map(|k| {
            let mut e = template.clone();
            e.class = -3; // Engine.PlayerStart
            e.outer = k as i32; // export k-1 (0 = null for the first)
            e.name = 11; // "Obj"
            e.number = k as i32;
            e.payload = vec![k as u8; 3];
            e
        })
        .collect();
    s.texture_allocations.clear();
    s
}

#[test]
fn outer_depth_limit_boundary() {
    let s = chain_package(MAX_OUTER_DEPTH + 1);
    let p = parse(&s.build().0).unwrap();
    assert!(p.issues.is_empty(), "{:?}", p.issues);
    // Export k has a chain of k + 1 objects.
    let deepest_ok = MAX_OUTER_DEPTH - 1;
    let path = p.export_path(deepest_ok).unwrap();
    assert_eq!(path.split('.').count(), MAX_OUTER_DEPTH);
    assert!(path.starts_with("Obj.Obj_0.Obj_1."));
    assert_eq!(
        p.outer_chain(PackageIndex::from_export(deepest_ok).unwrap())
            .unwrap()
            .len(),
        MAX_OUTER_DEPTH
    );
    assert!(matches!(
        p.export_path(MAX_OUTER_DEPTH),
        Err(Ue3Error::OuterChainTooDeep { limit, .. }) if limit == MAX_OUTER_DEPTH
    ));
}

// ------------------------------------------------------------------ truncation

#[test]
fn every_truncation_is_detected() {
    // Payloads tile to the end of the stream, so any truncation must be either
    // a hard error or at least one finding; never a silent clean parse.
    let fixtures = [
        ("plain", Synth::sample().build().0),
        ("thumbs", Synth::with_thumbnails().build().0),
        (
            "thumbs-lzo",
            Synth::with_thumbnails().build_compressed(90, 40).0,
        ),
    ];
    for (name, full) in fixtures {
        assert!(parse(&full).unwrap().issues.is_empty(), "{name}");
        let summary_len = Summary::parse(&full).unwrap().serialized_size;
        for cut in 0..full.len() {
            match parse(&full[..cut]) {
                Err(_) => {}
                Ok(p) => {
                    assert!(
                        cut >= summary_len,
                        "{name}: summary truncated at {cut} parsed"
                    );
                    assert!(
                        p.has_errors(),
                        "{name}: truncation at {cut} gave no error finding: {:?}",
                        p.issues
                    );
                }
            }
        }
    }
}

// ------------------------------------------------------------------ chunk table layout

#[test]
fn chunk_table_layout_attacks() {
    let (file, _) = Synth::sample().build_compressed(200, 64);
    let s = Summary::parse(&file).unwrap();
    assert!(s.compressed_chunks.len() >= 3);
    let entry = |i: usize| OFF_FIRST_CHUNK + 16 * i;

    // Chunk 1 reuses chunk 0's compressed bytes (would amplify output).
    let mut b = file.clone();
    put_i32(&mut b, entry(1) + 8, get_i32(&file, entry(0) + 8));
    assert!(matches!(
        parse(&b),
        Err(Ue3Error::BadChunk { chunk: 1, .. })
    ));
    // Chunk 0 compressed data inside the summary.
    for v in [0, 4, s.serialized_size as i32 - 1] {
        let mut b = file.clone();
        put_i32(&mut b, entry(0) + 8, v);
        assert!(matches!(
            parse(&b),
            Err(Ue3Error::BadChunk { chunk: 0, .. })
        ));
    }
    // First chunk output overlapping the rebuilt summary, or far beyond it.
    let hdr = get_i32(&file, entry(0));
    for v in [0, hdr - 1, hdr + 5000] {
        let mut b = file.clone();
        put_i32(&mut b, entry(0), v);
        assert!(
            matches!(parse(&b), Err(Ue3Error::BadChunk { chunk: 0, .. })),
            "first uncompressed offset {v}"
        );
    }
    // Chunk out of order in stream space.
    let mut b = file.clone();
    put_i32(&mut b, entry(2), get_i32(&file, entry(1)));
    assert!(matches!(
        parse(&b),
        Err(Ue3Error::BadChunk { chunk: 2, .. })
    ));

    // Trailing bytes after the last chunk: accepted with a warning.
    let mut b = file.clone();
    b.extend_from_slice(&[0xAB; 7]);
    let p = parse(&b).unwrap();
    assert!(has_warning(&p, "7 trailing file bytes"));

    // Unused bytes between the summary and the first chunk: every compressed
    // offset shifts; accepted with a warning and the same stream.
    let pad = 5usize;
    let mut b = file[..s.serialized_size].to_vec();
    b.extend(std::iter::repeat_n(0xCD, pad));
    b.extend_from_slice(&file[s.serialized_size..]);
    for i in 0..s.compressed_chunks.len() {
        let v = get_i32(&b, entry(i) + 8);
        put_i32(&mut b, entry(i) + 8, v + pad as i32);
    }
    let p = parse(&b).unwrap();
    assert!(has_warning(&p, "unused file bytes"));
    assert_eq!(p.stream(), parse(&file).unwrap().stream());
}

#[test]
fn block_table_attacks() {
    let (file, _) = Synth::sample().build_compressed(200, 64);
    let s = Summary::parse(&file).unwrap();
    let c0 = s.compressed_chunks[0].compressed_offset as usize;
    let block0 = c0 + 16;
    let c = get_i32(&file, block0);
    let u = get_i32(&file, block0 + 4);

    // Block bigger than the block size.
    let mut b = file.clone();
    put_i32(&mut b, block0 + 4, 65);
    assert!(matches!(
        parse(&b),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));
    // Impossible expansion: 1 compressed byte claiming > 512 output bytes needs
    // a block size above that, so raise the header block size too.
    let mut b = file.clone();
    put_i32(&mut b, c0 + 4, 4096);
    put_i32(&mut b, block0, 1);
    put_i32(&mut b, block0 + 4, 1000);
    assert!(matches!(
        parse(&b),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));
    // Block sizes no longer summing to the header totals.
    for (off, v) in [(block0, c + 1), (block0, c - 1), (block0 + 4, u - 1)] {
        let mut b = file.clone();
        put_i32(&mut b, off, v);
        assert!(
            matches!(parse(&b), Err(Ue3Error::BadChunk { chunk: 0, .. })),
            "{off:#x} = {v}"
        );
    }
    // Negative and huge block entries.
    for v in [-1, i32::MIN, i32::MAX] {
        for off in [block0, block0 + 4] {
            let mut b = file.clone();
            put_i32(&mut b, off, v);
            assert!(matches!(
                parse(&b),
                Err(Ue3Error::BadChunk { chunk: 0, .. })
            ));
        }
    }
    // Header CompressedSize disagreeing with the blocks.
    let mut b = file.clone();
    let hc = get_i32(&file, c0 + 8);
    put_i32(&mut b, c0 + 8, hc + 1);
    assert!(matches!(
        parse(&b),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));
}

// ------------------------------------------------------------------ resource caps

#[test]
fn stream_cap_applies_to_uncompressed_packages() {
    let (plain, _) = Synth::sample().build();
    let len = plain.len() as u64;
    let opts = |max| ReadOptions {
        max_stream_size: max,
        threads: 1,
    };
    assert!(matches!(
        Package::from_bytes_with(plain.clone(), &opts(len - 1)),
        Err(Ue3Error::TooLarge { .. })
    ));
    assert!(Package::from_bytes_with(plain.clone(), &opts(len)).is_ok());

    // `open_with` refuses an over-limit file before reading it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.upk");
    std::fs::write(&path, &plain).unwrap();
    assert!(matches!(
        Package::open_with(&path, &opts(len - 1)),
        Err(Ue3Error::TooLarge {
            what: "package file",
            ..
        })
    ));
    assert!(Package::open_with(&path, &opts(len)).is_ok());
}

#[test]
fn summary_prefix_limit_is_enforced() {
    let mut s = Synth::sample();
    s.texture_allocations[0].export_indices = (0..5_000).collect();
    let (bytes, layout) = s.build();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.upk");
    std::fs::write(&path, &bytes).unwrap();
    let need = layout.summary_len as u64;
    assert!(matches!(
        Summary::read_from_path_limited(&path, need - 1),
        Err(Ue3Error::TooLarge {
            what: "package summary",
            ..
        })
    ));
    let (summary, size) = Summary::read_from_path_limited(&path, need).unwrap();
    assert_eq!(summary.serialized_size, layout.summary_len);
    assert_eq!(size, bytes.len() as u64);
    // A file that simply ends inside its summary is a truncation error (here
    // the last TArray count no longer fits), not TooLarge.
    let short = dir.path().join("short.upk");
    std::fs::write(&short, &bytes[..layout.summary_len - 1]).unwrap();
    assert!(matches!(
        Summary::read_from_path_limited(&short, need * 2),
        Err(Ue3Error::UnexpectedEof { .. } | Ue3Error::CountTooLarge { .. })
    ));
    // A hostile count no longer makes the prefix reader take the whole file.
    let mut hostile = bytes.clone();
    put_i32(&mut hostile, OFF_GENERATION_COUNT, 0x0FFF_FFFF);
    let hp = dir.path().join("hostile.upk");
    std::fs::write(&hp, &hostile).unwrap();
    assert!(matches!(
        Summary::read_from_path_limited(&hp, 1024),
        Err(Ue3Error::TooLarge { .. })
    ));
    assert!(matches!(
        Summary::read_from_path(&hp),
        Err(Ue3Error::CountTooLarge { .. })
    ));
}

#[test]
fn explicit_thread_counts_are_capped() {
    let synth = Synth::sample();
    let (file, stream) = synth.build_compressed(13, 7);
    let chunks = Summary::parse(&file).unwrap().compressed_chunks.len();
    assert!(
        chunks > 64,
        "need more chunks than the thread cap, got {chunks}"
    );
    for threads in [0, 1, 2, 3, 64, 65, usize::MAX] {
        let opts = ReadOptions {
            threads,
            ..Default::default()
        };
        let p = Package::from_bytes_with(file.clone(), &opts).unwrap();
        assert_eq!(p.stream(), &stream[..], "threads {threads}");
    }
}
