//! Real-data validation against the user's own installed game.
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or falls back to the default macOS Steam
//! location. When the data is absent (e.g. CI) every test prints a note and
//! passes. Nothing is copied or written; only counts and structural facts are
//! asserted.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_ue3::{IndexKind, Package, PackageIndex, Storage, package_flags};

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

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if p.is_file() && matches!(ext.as_str(), "u" | "upk" | "asamu") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
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

fn name_of(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn every_package_parses_and_resolves() {
    let dir = require_data!();
    let files = packages(&dir);
    assert_eq!(files.len(), 42, "expected 42 packages in CookedMac + Maps");
    let mut totals = (0usize, 0usize, 0usize);
    let mut compressed = 0;
    let mut thumbnailed = 0;
    for f in &files {
        let file = name_of(f);
        let p = Package::open(f).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(p.issues.is_empty(), "{file}: {:?}", p.issues);
        let s = &p.summary;
        assert_eq!(s.file_version, 868, "{file}");
        assert_eq!(s.licensee_version, 0, "{file}");
        assert_eq!(s.folder_name, "None", "{file}");
        assert_eq!(s.engine_version, 12097, "{file}");
        let want_cooker = if file == "RefShaderCache-PC-D3D-SM3.upk" {
            0
        } else {
            136
        };
        assert_eq!(s.cooker_version, want_cooker, "{file}");
        assert_eq!(
            (s.import_guids_count, s.export_guids_count),
            (0, 0),
            "{file}"
        );
        let g = s.latest_generation().unwrap();
        assert_eq!(g.export_count as u32, s.export_count, "{file}");
        assert_eq!(g.name_count as u32, s.name_count, "{file}");

        // Header layout: names, imports, exports, depends map, then (only when
        // a thumbnail table exists) thumbnail records and the thumbnail table,
        // all adjacent and ending at TotalHeaderSize.
        assert_eq!(p.extents.names.0 as u32, s.name_offset, "{file}");
        assert_eq!(p.extents.names.1 as u32, s.import_offset, "{file}");
        assert_eq!(p.extents.imports.1 as u32, s.export_offset, "{file}");
        assert_eq!(p.extents.exports.1 as u32, s.depends_offset, "{file}");
        assert_eq!(
            p.extents.depends.map(|d| d.1 as u32),
            Some(s.import_export_guids_offset),
            "{file}"
        );
        if s.thumbnail_table_offset == 0 {
            assert!(p.thumbnails.is_none(), "{file}");
            assert_eq!(s.import_export_guids_offset, s.total_header_size, "{file}");
        } else {
            thumbnailed += 1;
            let thumbs = p.thumbnails.as_ref().unwrap();
            assert!(!thumbs.is_empty(), "{file}");
            assert_eq!(
                p.extents.thumbnail_data,
                Some((
                    s.import_export_guids_offset as usize,
                    s.thumbnail_table_offset as usize
                )),
                "{file}"
            );
            assert_eq!(
                p.extents.thumbnail_table.map(|t| t.1),
                Some(s.total_header_size as usize),
                "{file}"
            );
            let mut recs: Vec<(usize, usize)> =
                thumbs.iter().map(|t| (t.offset as usize, t.end)).collect();
            recs.sort_unstable();
            for w in recs.windows(2) {
                assert_eq!(w[0].1, w[1].0, "{file}: thumbnail records not contiguous");
            }
        }
        assert!(
            p.depends.as_ref().unwrap().iter().all(Vec::is_empty),
            "{file}: depends map expected empty in cooked packages"
        );

        // Flag correlations used in PACKAGE_ANALYSIS.md.
        let ext = f
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert_eq!(s.contains_map(), ext == "asamu", "{file} ContainsMap");
        assert_eq!(s.contains_script(), ext == "u", "{file} ContainsScript");
        assert_eq!(
            s.package_flags & package_flags::STORE_COMPRESSED != 0,
            s.compression_flags != 0,
            "{file} StoreCompressed"
        );
        // PKG_Cooked (0x8) is set everywhere except the three RefShaderCache packages.
        assert_eq!(
            s.is_cooked(),
            !file.starts_with("RefShaderCache-"),
            "{file} Cooked"
        );

        match &p.storage {
            Storage::Compressed { chunks, .. } => {
                compressed += 1;
                assert_eq!(s.compression_flags, 2, "{file}");
                let c0 = &s.compressed_chunks[0];
                assert_eq!(c0.uncompressed_offset, s.name_offset, "{file}");
                assert_eq!(c0.compressed_offset as usize, s.serialized_size, "{file}");
                assert_eq!(
                    s.serialized_size - 16 * s.compressed_chunks.len(),
                    s.name_offset as usize,
                    "{file}"
                );
                for c in chunks {
                    assert_eq!(c.block_size, 0x20000, "{file}");
                    let last = c.blocks.len().saturating_sub(1);
                    for (i, b) in c.blocks.iter().enumerate() {
                        if i < last {
                            assert_eq!(b.uncompressed_size, c.block_size, "{file}");
                        }
                    }
                }
                let end = chunks.last().unwrap();
                assert_eq!(
                    u64::from(end.entry.compressed_offset) + u64::from(end.entry.compressed_size),
                    p.file_size,
                    "{file}: last chunk ends at EOF"
                );
            }
            Storage::Uncompressed => {
                assert_eq!(s.compression_flags, 0, "{file}");
                assert!(s.compressed_chunks.is_empty(), "{file}");
                assert_eq!(p.stream().len() as u64, p.file_size, "{file}");
            }
        }

        // Every package index resolves; every path and class name is computable;
        // every payload is in range.
        for (i, e) in p.exports.iter().enumerate() {
            for idx in [
                e.class_index,
                e.super_index,
                e.outer_index,
                e.archetype_index,
            ] {
                p.object_path(idx)
                    .unwrap_or_else(|err| panic!("{file} export {i}: {err}"));
            }
            p.export_path(i).unwrap();
            p.export_class_name(i).unwrap();
            p.export_class_package(i).unwrap();
            let data = p.export_data(i).unwrap();
            assert_eq!(data.len(), e.serial_size as usize, "{file} export {i}");
        }
        for i in 0..p.imports.len() {
            p.import_path(i).unwrap();
        }
        totals.0 += p.names.len();
        totals.1 += p.imports.len();
        totals.2 += p.exports.len();
    }
    assert_eq!(compressed, 38);
    assert_eq!(thumbnailed, 4);
    eprintln!(
        "parsed {} packages: {} names, {} imports, {} exports",
        files.len(),
        totals.0,
        totals.1,
        totals.2
    );
}

#[test]
fn known_package_counts() {
    let dir = require_data!();
    let expect: &[(&str, usize, usize, usize, usize)] = &[
        // file, names, imports, exports, chunks
        ("Core.u", 827, 20, 1542, 1),
        ("UTGameContent.u", 2013, 1069, 1004, 0),
        ("Startup.upk", 17658, 3998, 37183, 70),
        ("Engine.u", 20153, 182, 33443, 10),
        ("Maps/AG-StarHaven.asamu", 3372, 547, 23892, 84),
    ];
    for &(file, n, i, e, c) in expect {
        let p = Package::open(dir.join(file)).unwrap();
        assert_eq!(
            (p.names.len(), p.imports.len(), p.exports.len()),
            (n, i, e),
            "{file}"
        );
        assert_eq!(p.summary.compressed_chunks.len(), c, "{file}");
    }
    let core = Package::open(dir.join("Core.u")).unwrap();
    let c0 = core.summary.compressed_chunks[0];
    assert_eq!(
        (
            c0.uncompressed_offset,
            c0.uncompressed_size,
            c0.compressed_offset,
            c0.compressed_size
        ),
        (129, 309_301, 145, 90_240)
    );
    let startup = Package::open(dir.join("Startup.upk")).unwrap();
    assert_eq!(startup.summary.texture_allocations.len(), 66);
    for (map, sub) in [
        ("Maps/AG-BeautifulCity.asamu", "freds_place"),
        ("Maps/AG-IceCave.asamu", "thecore"),
    ] {
        let (s, _) = asamu_ue3::Summary::read_from_path(&dir.join(map)).unwrap();
        assert_eq!(s.additional_packages_to_cook, vec![sub.to_owned()], "{map}");
    }
}

/// Where the game's script classes live: Startup.upk carries forced exports of
/// the `asamu` and `UTGame` script packages.
#[test]
fn startup_contains_cooked_script_packages() {
    let dir = require_data!();
    let p = Package::open(dir.join("Startup.upk")).unwrap();
    let top: BTreeMap<String, usize> = p.top_level_exports().fold(BTreeMap::new(), |mut m, i| {
        *m.entry(p.export_class_name(i).unwrap()).or_default() += 1;
        m
    });
    assert_eq!(top.get("Package"), Some(&130));
    assert_eq!(top.get("ObjectReferencer"), Some(&1));
    assert_eq!(top.values().sum::<usize>(), 131);

    for (pkg, under, classes) in [("asamu", 4371usize, 172usize), ("UTGame", 17371, 411)] {
        let roots: Vec<usize> = p
            .top_level_exports()
            .filter(|&i| p.export_path(i).unwrap() == pkg)
            .collect();
        assert_eq!(roots.len(), 1, "{pkg}");
        let root = roots[0];
        assert_eq!(p.export_class_name(root).unwrap(), "Package");
        let root_ref = PackageIndex::from_export(root).unwrap();
        let mut n_under = 0;
        let mut n_class = 0;
        for i in 0..p.exports.len() {
            let r = PackageIndex::from_export(i).unwrap();
            if p.is_inside(r, root_ref).unwrap() {
                n_under += 1;
                if p.export_class_name(i).unwrap() == "Class" {
                    n_class += 1;
                }
            }
        }
        assert_eq!(n_under, under, "{pkg}");
        assert_eq!(n_class, classes, "{pkg}");
        // The forced-export Package entry records the same object count.
        assert_eq!(
            p.exports[root].generation_net_object_count,
            vec![under as i32],
            "{pkg}"
        );
        assert_eq!(
            p.exports[root].package_flags,
            package_flags::CONTAINS_SCRIPT | package_flags::NO_EXPORT_ALLOWED,
            "{pkg}"
        );
    }
    assert!(!p.find_exports("ASAMUPawn").is_empty());
    assert!(!p.find_exports("GrappleGun").is_empty());
}

#[test]
fn maps_import_gameplay_classes_rather_than_define_them() {
    let dir = require_data!();
    for f in packages(&dir) {
        if f.extension().is_none_or(|e| e != "asamu") {
            continue;
        }
        let file = name_of(&f);
        let p = Package::open(&f).unwrap();
        let census = p.class_census().unwrap();
        assert_eq!(census.get("Class"), None, "{file} defines classes");
        assert_eq!(census.get("World"), Some(&1), "{file}");
        assert_eq!(p.find_exports("PersistentLevel").len(), 1, "{file}");
        // Class references from maps resolve to imports.
        for e in &p.exports {
            assert!(
                matches!(e.class_index.kind(), IndexKind::Import(_)),
                "{file}: export class is not an import"
            );
        }
    }
}

/// Metadata claims in PACKAGE_ANALYSIS.md that were first established by an
/// independent cross-check (separate parser + reference LZO decoder): texture
/// allocations, thumbnail records, outer-chain depth and `ScriptText` buffers.
/// Only counts and structure are asserted; no payload content is inspected
/// beyond an 8-byte image signature.
#[test]
fn metadata_claims_in_package_analysis() {
    let dir = require_data!();
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut with_tex_alloc = 0;
    let (mut thumbs, mut thumbs_png, mut thumbs_empty) = (0, 0, 0);
    let mut max_chain = 0;
    let mut u_text_buffers = 0;
    for f in packages(&dir) {
        let file = name_of(&f);
        let p = Package::open(&f).unwrap();
        if !p.summary.texture_allocations.is_empty() {
            with_tex_alloc += 1;
        }
        for t in p.thumbnails.iter().flatten() {
            thumbs += 1;
            if t.data_size == 0 {
                assert_eq!((t.width, t.height), (0, 0), "{file}");
                thumbs_empty += 1;
            } else {
                let start = t.offset as usize + 12;
                assert_eq!(p.stream().get(start..start + 8), Some(PNG), "{file}");
                thumbs_png += 1;
            }
        }
        for i in 0..p.exports.len() {
            let r = PackageIndex::from_export(i).unwrap();
            max_chain = max_chain.max(p.outer_chain(r).unwrap().len());
        }
        for i in 0..p.imports.len() {
            let r = PackageIndex::from_import(i).unwrap();
            max_chain = max_chain.max(p.outer_chain(r).unwrap().len());
        }
        let census = p.class_census().unwrap();
        let text_buffers = census.get("TextBuffer").copied().unwrap_or(0);
        if file.ends_with(".u") {
            u_text_buffers += text_buffers;
        }
        if file == "Engine.u" {
            assert_eq!(text_buffers, 1343);
        }
        if file == "Startup.upk" {
            assert_eq!(p.summary.texture_allocations.len(), 66);
            // One ScriptText buffer per class, parented to that class.
            let classes = census.get("Class").copied().unwrap_or(0);
            assert_eq!((text_buffers, classes), (583, 583));
            let mut parents = std::collections::BTreeSet::new(); // of outer index values
            for i in 0..p.exports.len() {
                if p.export_class_name(i).unwrap() != "TextBuffer" {
                    continue;
                }
                let e = &p.exports[i];
                assert_eq!(p.fname(e.object_name), "ScriptText");
                assert_eq!(p.class_name(e.outer_index).unwrap(), "Class");
                assert!(parents.insert(e.outer_index.0), "two buffers share a class");
            }
            assert_eq!(parents.len(), 583);
        }
    }
    assert_eq!(with_tex_alloc, 21, "packages with texture allocations");
    assert_eq!((thumbs, thumbs_png, thumbs_empty), (21, 13, 8));
    assert_eq!(max_chain, 9, "deepest outer chain");
    assert_eq!(
        u_text_buffers, 1938,
        "TextBuffer exports in the 12 .u packages"
    );
}
