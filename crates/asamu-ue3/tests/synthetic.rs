//! Parsing tests against synthetic packages written byte by byte in `common`.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

mod common;

use asamu_ue3::{
    CompressionMethod, IndexKind, Package, PackageIndex, Severity, Storage, Summary, Ue3Error,
};
use common::*;

#[test]
fn uncompressed_package_parses_completely() {
    let synth = Synth::sample();
    let (bytes, layout) = synth.build();
    let p = Package::from_bytes(bytes.clone()).unwrap();

    assert!(p.issues.is_empty(), "{:?}", p.issues);
    assert!(matches!(p.storage, Storage::Uncompressed));
    assert_eq!(p.stream(), &bytes[..]);
    assert_eq!(p.file_size, bytes.len() as u64);

    let s = &p.summary;
    assert_eq!(s.file_version, 868);
    assert_eq!(s.licensee_version, 0);
    assert_eq!(s.folder_name, "None");
    assert_eq!(s.package_flags, synth.package_flags);
    assert!(s.contains_map());
    assert!(s.is_cooked());
    assert!(!s.contains_script());
    assert_eq!(s.name_count as usize, NAMES.len());
    assert_eq!(s.name_offset as usize, layout.name_offset);
    assert_eq!(s.import_offset as usize, layout.import_offset);
    assert_eq!(s.export_offset as usize, layout.export_offset);
    assert_eq!(s.depends_offset as usize, layout.depends_offset);
    assert_eq!(s.total_header_size as usize, layout.header_end);
    assert_eq!(s.serialized_size, layout.summary_len);
    assert_eq!(s.engine_version, 12097);
    assert_eq!(s.cooker_version, 136);
    assert_eq!(s.compression(), CompressionMethod::None);
    assert!(!s.is_compressed());
    assert_eq!(s.package_source, 0x1234_5678);
    assert_eq!(s.guid.to_string(), "DEADBEEF000000010000000200000003");
    assert_eq!(s.generations.len(), 1);
    assert_eq!(s.generations[0].export_count, 5);
    assert_eq!(s.additional_packages_to_cook, vec!["sublevel_a"]);
    assert_eq!(s.texture_allocations.len(), 1);
    assert_eq!(s.texture_allocations[0].size_x, 256);
    assert_eq!(s.texture_allocations[0].export_indices, vec![4, 5]);

    // Tables.
    assert_eq!(p.names.len(), NAMES.len());
    for (i, n) in NAMES.iter().enumerate() {
        assert_eq!(p.names[i].name, *n);
        assert_eq!(p.names[i].flags, 0x0007_0010_0000_0000 + i as u64);
    }
    assert_eq!(p.imports.len(), 5);
    assert_eq!(p.exports.len(), 5);
    assert_eq!(p.extents.names, (layout.name_offset, layout.import_offset));
    assert_eq!(
        p.extents.imports,
        (layout.import_offset, layout.export_offset)
    );
    assert_eq!(
        p.extents.exports,
        (layout.export_offset, layout.depends_offset)
    );
    assert_eq!(
        p.extents.depends,
        Some((layout.depends_offset, layout.header_end))
    );
    assert_eq!(p.depends.as_ref().map(Vec::len), Some(5));

    // Import resolution.
    assert_eq!(p.import_path(0).unwrap(), "Core");
    assert_eq!(p.import_path(2).unwrap(), "Engine.PlayerStart");
    assert_eq!(p.fname(p.imports[2].class_name), "Class");
    assert_eq!(p.class_name(PackageIndex(-3)).unwrap(), "Class");
    assert_eq!(
        p.root_name(PackageIndex(-3)).unwrap().as_deref(),
        Some("Engine")
    );

    // Export resolution.
    let paths: Vec<String> = (0..5).map(|i| p.export_path(i).unwrap()).collect();
    assert_eq!(
        paths,
        vec![
            "TestPkg",
            "TestPkg.TestActor",
            "TheWorld",
            "TheWorld.Obj_2",
            "TheWorld.Obj"
        ]
    );
    let classes: Vec<String> = (0..5).map(|i| p.export_class_name(i).unwrap()).collect();
    assert_eq!(
        classes,
        vec!["Package", "Class", "World", "PlayerStart", "TestActor"]
    );
    let class_pkgs: Vec<Option<String>> =
        (0..5).map(|i| p.export_class_package(i).unwrap()).collect();
    assert_eq!(
        class_pkgs,
        vec![
            Some("Core".to_owned()),
            Some("Core".to_owned()),
            Some("Engine".to_owned()),
            Some("Engine".to_owned()),
            Some("TestPkg".to_owned())
        ]
    );
    assert_eq!(
        p.object_path(p.exports[1].super_index).unwrap(),
        "Engine.PlayerStart"
    );
    assert_eq!(p.object_path(PackageIndex::NULL).unwrap(), "None");
    assert_eq!(p.top_level_exports().collect::<Vec<_>>(), vec![0, 2]);
    assert_eq!(p.find_exports("Obj"), vec![4]);
    assert_eq!(p.find_exports("Obj_2"), vec![3]);
    assert!(p.is_inside(PackageIndex(4), PackageIndex(3)).unwrap());
    assert!(!p.is_inside(PackageIndex(3), PackageIndex(3)).unwrap());
    assert_eq!(p.exports[0].generation_net_object_count, vec![2]);
    assert_eq!(p.exports[0].package_flags, 0x0020_0000);
    assert_eq!(
        p.exports[0].package_guid.to_string(),
        "11111111222222223333333344444444"
    );

    // Payloads.
    for (i, e) in synth.exports.iter().enumerate() {
        assert_eq!(p.export_data(i).unwrap(), &e.payload[..]);
        assert_eq!(
            p.exports[i].serial_offset as usize,
            layout.payload_offsets[i]
        );
    }

    // Census.
    let census = p.class_census().unwrap();
    assert_eq!(census.get("PlayerStart"), Some(&1));
    assert_eq!(census.get("Class"), Some(&1));
    assert_eq!(census.values().sum::<usize>(), 5);

    // Package-index helpers.
    assert_eq!(p.export_ref(0).unwrap(), PackageIndex(1));
    assert_eq!(p.import_ref(0).unwrap(), PackageIndex(-1));
    assert!(p.export_ref(5).is_err());
    assert!(p.import_ref(5).is_err());
    assert_eq!(PackageIndex(-3).kind(), IndexKind::Import(2));
}

#[test]
fn summary_reserializes_byte_identically() {
    let (bytes, layout) = Synth::sample().build();
    let s = Summary::parse(&bytes).unwrap();
    let again = s.to_uncompressed_bytes().unwrap();
    assert_eq!(again, &bytes[..layout.summary_len]);
}

#[test]
fn lzo_literal_encoder_matches_decompressor() {
    for n in [
        0usize, 1, 2, 3, 4, 17, 18, 19, 238, 239, 240, 255, 273, 274, 600, 5000,
    ] {
        let data: Vec<u8> = (0..n).map(|i| (i * 31 + 7) as u8).collect();
        let enc = lzo_literal(&data);
        let dec = asamu_ue3::lzo::decompress(&enc, n)
            .unwrap_or_else(|e| panic!("literal stream of {n} bytes rejected: {e}"));
        assert_eq!(dec, data, "n = {n}");
    }
}

#[test]
fn compressed_package_rebuilds_the_uncompressed_stream() {
    let synth = Synth::sample();
    for (chunk, block) in [
        (200usize, 64usize),
        (100_000, 131_072),
        (37, 5),
        (1000, 239),
    ] {
        let (file, stream) = synth.build_compressed(chunk, block);
        let p = Package::from_bytes(file.clone()).unwrap();
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(p.stream(), &stream[..], "chunk {chunk} block {block}");
        assert!(p.is_compressed());
        assert_eq!(p.file_size, file.len() as u64);
        let Storage::Compressed { method, chunks } = &p.storage else {
            panic!("expected compressed storage");
        };
        assert_eq!(*method, CompressionMethod::Lzo);
        let body_len = stream.len() - p.summary.compressed_chunks[0].uncompressed_offset as usize;
        assert_eq!(chunks.len(), body_len.div_ceil(chunk));
        for c in chunks {
            assert_eq!(c.block_size as usize, block);
            assert_eq!(
                c.blocks.len(),
                (c.uncompressed_size as usize).div_ceil(block)
            );
            assert_eq!(c.header_size as usize, 16 + 8 * c.blocks.len());
        }
        // Same tables as the uncompressed build.
        assert_eq!(p.exports.len(), 5);
        assert_eq!(p.export_path(3).unwrap(), "TheWorld.Obj_2");
        for (i, e) in synth.exports.iter().enumerate() {
            assert_eq!(p.export_data(i).unwrap(), &e.payload[..]);
        }
        // Summary on disk keeps its chunk table; the stream's summary does not.
        assert_eq!(p.summary.compression_flags, 2);
        let inner = Summary::parse(p.stream()).unwrap();
        assert_eq!(inner.compression_flags, 0);
        assert!(inner.compressed_chunks.is_empty());
        assert_eq!(inner.name_count, p.summary.name_count);
        // Single-threaded decompression gives the same result.
        let opts = asamu_ue3::ReadOptions {
            threads: 1,
            ..Default::default()
        };
        let p1 = Package::from_bytes_with(file, &opts).unwrap();
        assert_eq!(p1.stream(), p.stream());
    }
}

#[test]
fn unsupported_compression_methods_are_rejected() {
    let (mut file, _) = Synth::sample().build_compressed(200, 64);
    for flags in [1i32, 4, 0x12, 0x22, 3] {
        put_i32(&mut file, OFF_COMPRESSION_FLAGS, flags);
        match Package::from_bytes(file.clone()) {
            Err(Ue3Error::UnsupportedCompression { flags: f, .. }) => assert_eq!(f as i32, flags),
            other => panic!("flags {flags}: {other:?}"),
        }
    }
    // Chunk table present but CompressionFlags 0.
    put_i32(&mut file, OFF_COMPRESSION_FLAGS, 0);
    assert!(matches!(
        Package::from_bytes(file),
        Err(Ue3Error::BadChunk { .. })
    ));
}

#[test]
fn corrupt_chunk_headers_are_rejected() {
    let (file, _) = Synth::sample().build_compressed(200, 64);
    let s = Summary::parse(&file).unwrap();
    let c0 = s.compressed_chunks[0].compressed_offset as usize;

    let mut bad_tag = file.clone();
    bad_tag[c0] ^= 0xFF;
    assert!(matches!(
        Package::from_bytes(bad_tag),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));

    let mut bad_block = file.clone();
    put_i32(&mut bad_block, c0 + 4, 0);
    assert!(matches!(
        Package::from_bytes(bad_block),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));

    let mut huge_block = file.clone();
    put_i32(&mut huge_block, c0 + 4, i32::MAX);
    assert!(Package::from_bytes(huge_block).is_err());

    let mut wrong_usize = file.clone();
    let u = get_i32(&wrong_usize, c0 + 12);
    put_i32(&mut wrong_usize, c0 + 12, u + 1);
    assert!(matches!(
        Package::from_bytes(wrong_usize),
        Err(Ue3Error::BadChunk { chunk: 0, .. })
    ));

    // Summary chunk entry pointing past the end of the file.
    let mut past_end = file.clone();
    put_i32(&mut past_end, OFF_FIRST_CHUNK + 8, file.len() as i32);
    assert!(matches!(
        Package::from_bytes(past_end),
        Err(Ue3Error::BadChunk { .. })
    ));

    // Non-contiguous uncompressed offsets.
    if s.compressed_chunks.len() > 1 {
        let mut gap = file.clone();
        let off = OFF_FIRST_CHUNK + 16;
        let v = get_i32(&gap, off);
        put_i32(&mut gap, off, v + 1);
        assert!(matches!(
            Package::from_bytes(gap),
            Err(Ue3Error::BadChunk { chunk: 1, .. })
        ));
    }

    // Corrupt LZO block data surfaces as an LZO or chunk error, never a panic.
    let nblocks = (get_i32(&file, c0 + 12) as usize).div_ceil(get_i32(&file, c0 + 4) as usize);
    let data_start = c0 + 16 + 8 * nblocks;
    let mut bad_lzo = file.clone();
    bad_lzo[data_start] = 0x10; // a match instruction at stream start
    assert!(Package::from_bytes(bad_lzo).is_err());
}

#[test]
fn reference_errors_are_detected() {
    // Name index out of range.
    let mut s = Synth::sample();
    s.imports[1].name = 999;
    assert!(matches!(
        Package::from_bytes(s.build().0),
        Err(Ue3Error::BadNameIndex { index: 999, .. })
    ));
    let mut s = Synth::sample();
    s.exports[2].name = -1;
    assert!(matches!(
        Package::from_bytes(s.build().0),
        Err(Ue3Error::BadNameIndex { index: -1, .. })
    ));
    // Package index out of range.
    let mut s = Synth::sample();
    s.exports[2].outer = 6;
    assert!(matches!(
        Package::from_bytes(s.build().0),
        Err(Ue3Error::BadPackageIndex { index: 6, .. })
    ));
    let mut s = Synth::sample();
    s.imports[0].outer = -6;
    assert!(matches!(
        Package::from_bytes(s.build().0),
        Err(Ue3Error::BadPackageIndex { index: -6, .. })
    ));
    let mut s = Synth::sample();
    s.exports[0].class = i32::MIN;
    assert!(matches!(
        Package::from_bytes(s.build().0),
        Err(Ue3Error::BadPackageIndex { .. })
    ));
}

#[test]
fn outer_cycles_are_bounded() {
    let mut s = Synth::sample();
    s.exports[2].outer = 4; // TheWorld -> Obj_2 -> TheWorld
    let p = Package::from_bytes(s.build().0).unwrap();
    assert!(matches!(
        p.export_path(2),
        Err(Ue3Error::OuterChainTooDeep { .. })
    ));
    let mut s = Synth::sample();
    s.exports[0].outer = 1; // self-outer
    let p = Package::from_bytes(s.build().0).unwrap();
    assert!(p.export_path(0).is_err());
    assert!(p.root_name(PackageIndex(1)).is_err());
    exercise(&p);
}

#[test]
fn bad_serial_ranges_are_reported_not_fatal() {
    let synth = Synth::sample();
    let (mut bytes, layout) = synth.build();
    // Export 4's SerialSize lives at export_offset + 4 * 68 + 36 (no net counts
    // on exports 1..4; export 0 has one net count => +4).
    let e4 = layout.export_offset + 68 + 4 + 3 * 68;
    put_i32(&mut bytes, e4 + 36, 1_000_000);
    let p = Package::from_bytes(bytes.clone()).unwrap();
    assert!(p.has_errors());
    assert!(p.issues.iter().any(|i| i.severity == Severity::Error));
    assert!(matches!(
        p.export_data(4),
        Err(Ue3Error::OutOfBounds { .. })
    ));
    assert!(p.export_data(3).is_ok());
    put_i32(&mut bytes, e4 + 36, -5);
    let p = Package::from_bytes(bytes).unwrap();
    assert!(p.has_errors());
    assert!(p.export_data(4).is_err());
}

#[test]
fn generation_mismatch_is_a_warning() {
    let (mut bytes, _) = Synth::sample().build();
    put_i32(&mut bytes, OFF_GENERATION_COUNT + 4, 99);
    let p = Package::from_bytes(bytes).unwrap();
    assert!(!p.has_errors());
    assert!(
        p.issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.message.contains("generation"))
    );
}

#[test]
fn summary_prefix_reader_grows_as_needed() {
    let mut s = Synth::sample();
    // ~120 KiB of texture-allocation indices forces several prefix reads.
    s.texture_allocations[0].export_indices = (0..30_000).collect();
    let (bytes, layout) = s.build();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.upk");
    std::fs::write(&path, &bytes).unwrap();
    let (summary, size) = Summary::read_from_path(&path).unwrap();
    assert_eq!(size, bytes.len() as u64);
    assert_eq!(summary.serialized_size, layout.summary_len);
    assert_eq!(summary.texture_allocations[0].export_indices.len(), 30_000);

    let p = Package::open(&path).unwrap();
    assert!(p.issues.is_empty(), "{:?}", p.issues);

    // Truncated file: summary cannot be completed.
    let short = dir.path().join("short.upk");
    std::fs::write(&short, &bytes[..layout.summary_len - 10]).unwrap();
    assert!(Summary::read_from_path(&short).is_err());
    assert!(matches!(
        Package::open(dir.path().join("missing.upk")),
        Err(Ue3Error::Io { .. })
    ));
}

#[test]
fn utf16_folder_name_and_unicode_strings() {
    // Hand-patch: replace the 9-byte Latin-1 "None" FString with a UTF-16 one.
    let (bytes, _) = Synth::sample().build();
    let mut patched = bytes[..12].to_vec();
    patched.extend_from_slice(&(-5i32).to_le_bytes());
    for c in "None\0".encode_utf16() {
        patched.extend_from_slice(&c.to_le_bytes());
    }
    patched.extend_from_slice(&bytes[21..]);
    let s = Summary::parse(&patched).unwrap();
    assert_eq!(s.folder_name, "None");
    assert_eq!(
        s.serialized_size,
        Summary::parse(&bytes).unwrap().serialized_size + 5
    );
}
