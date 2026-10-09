//! Package, export, object and name-table flag evidence against the user's
//! own installed game (see PACKAGE_ANALYSIS.md, "Package flags" and "Object,
//! export and name flags").
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and skips
//! when the data is absent. Every package is opened once; the tests assert
//! counts and correlations only. Nothing is copied or written.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use asamu_ue3::flags::{self, export, name, object, package_source::package_source_crc};
use asamu_ue3::script::{ScriptBody, decode_script_object};
use asamu_ue3::{NoSchema, Package, package_flags};

const APP: &str = "A Story About My Uncle.app/Contents/Resources";

fn install_root() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    root.join(APP)
        .join("ASAMU/CookedMac")
        .is_dir()
        .then_some(root)
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

/// Per-package summary facts.
#[derive(Debug)]
struct PkgFacts {
    file: String,
    base: String,
    flags: u32,
    package_source: u32,
    net_object_count: i32,
}

/// Everything the tests assert on, gathered in one pass over the install.
#[derive(Debug, Default)]
struct Census {
    packages: Vec<PkgFacts>,
    exports: usize,
    forced: usize,
    forced_outside_forced_package: usize,
    unforced_inside_forced_package: usize,
    forced_top_packages: usize,
    forced_top_packages_without_flags: usize,
    other_exports_with_package_flags: usize,
    forced_top_sso_with_net_counts: usize,
    forced_top_no_sso_without_net_counts: usize,
    outside_keep_mask: usize,
    without_load_for_edit: usize,
    edit_only: usize,
    not_for_client: usize,
    not_for_client_mismatch: usize,
    not_for_client_loaded: usize,
    cdo_bit: usize,
    cdo_bit_name_mismatch: usize,
    protected_non_property: usize,
    protected: usize,
    per_object_localized: usize,
    sound_waves: usize,
    sound_waves_without_pol: usize,
    native_and_class_native: usize,
    neither_native: usize,
    native_mismatch: usize,
    native_on_non_class: usize,
    names: usize,
    names_without_saved_bits: usize,
    names_suppressed: usize,
    suppressed_not_in_list: usize,
    in_list_not_suppressed: usize,
    imports_resolved: usize,
    imports_resolved_not_public: usize,
}

fn effective_suppress_list(root: &Path) -> BTreeSet<String> {
    let read = |rel: &str| {
        std::fs::read(root.join(APP).join(rel))
            .map(|b| b.iter().map(|&c| char::from(c)).collect::<String>())
            .unwrap_or_default()
    };
    let mut set = BTreeSet::new();
    for line in read("Engine/Config/BaseEngine.ini").lines() {
        if let Some(v) = line.trim().strip_prefix("Suppress=") {
            set.insert(v.trim().to_owned());
        }
    }
    for line in read("ASAMU/Config/DefaultEngine.ini").lines() {
        if let Some(v) = line.trim().strip_prefix("-Suppress=") {
            set.remove(v.trim());
        }
    }
    set
}

fn outermost(pkg: &Package, i: usize) -> usize {
    let mut cur = i;
    for _ in 0..asamu_ue3::MAX_OUTER_DEPTH {
        match pkg.exports[cur].outer_index.export_index() {
            Some(o) => cur = o,
            None => break,
        }
    }
    cur
}

fn build(root: &Path) -> Census {
    let cooked = root.join(APP).join("ASAMU/CookedMac");
    let suppress = effective_suppress_list(root);
    let mut c = Census::default();
    // Lower-cased object path -> (class name, object flags) for every export
    // that can be the target of a cross-package import: everything in the
    // `.u` script packages (qualified with the package name) and everything
    // inside a forced top-level `Package` export (its path already starts
    // with that package's name).
    let mut targets: HashMap<String, Vec<(String, u64)>> = HashMap::new();
    let mut imports: Vec<(String, String)> = Vec::new();
    for path in packages(&cooked) {
        let pkg = Package::open(&path).unwrap();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let base = path.file_stem().unwrap().to_string_lossy().into_owned();
        let is_script = file.ends_with(".u");
        c.packages.push(PkgFacts {
            file: file.clone(),
            base: base.clone(),
            flags: pkg.summary.package_flags,
            package_source: pkg.summary.package_source,
            net_object_count: pkg.summary.latest_generation().unwrap().net_object_count,
        });

        for n in &pkg.names {
            c.names += 1;
            if n.flags & name::SAVED != name::SAVED {
                c.names_without_saved_bits += 1;
            }
            let sup = n.flags & name::SUPPRESS != 0;
            let listed = suppress.contains(&n.name);
            c.names_suppressed += usize::from(sup);
            c.suppressed_not_in_list += usize::from(sup && !listed);
            c.in_list_not_suppressed += usize::from(listed && !sup);
        }

        for (i, e) in pkg.exports.iter().enumerate() {
            c.exports += 1;
            let class = pkg.export_class_name(i).unwrap();
            let top = outermost(&pkg, i);
            let top_e = &pkg.exports[top];
            let top_is_forced_package = pkg.export_class_name(top).unwrap() == "Package"
                && top_e.export_flags & export::FORCED_EXPORT != 0;
            let forced = e.export_flags & export::FORCED_EXPORT != 0;
            assert_eq!(e.export_flags & !export::FORCED_EXPORT, 0, "{file} #{i}");
            c.forced += usize::from(forced);
            c.forced_outside_forced_package += usize::from(forced && !top_is_forced_package);
            c.unforced_inside_forced_package += usize::from(!forced && top_is_forced_package);

            if class == "Package" && top == i && forced {
                c.forced_top_packages += 1;
                c.forced_top_packages_without_flags += usize::from(e.package_flags == 0);
                let sso = e.package_flags & package_flags::SERVER_SIDE_ONLY != 0;
                let has_net = !e.generation_net_object_count.is_empty();
                c.forced_top_sso_with_net_counts += usize::from(sso && has_net);
                c.forced_top_no_sso_without_net_counts += usize::from(!sso && !has_net);
            } else if e.package_flags != 0 {
                c.other_exports_with_package_flags += 1;
            }

            let of = e.object_flags;
            c.outside_keep_mask += usize::from(of & !object::LOAD_KEEP_MASK != 0);
            c.without_load_for_edit += usize::from(of & object::LOAD_FOR_EDIT == 0);
            c.edit_only += usize::from(!object::loaded_in_game(of));
            let nfc = of & object::NOT_FOR_CLIENT != 0;
            let nfs = of & object::NOT_FOR_SERVER != 0;
            c.not_for_client += usize::from(nfc);
            c.not_for_client_mismatch += usize::from(nfc != nfs);
            c.not_for_client_loaded += usize::from(nfc && object::loaded_in_game(of));
            let obj_name = pkg.fname(e.object_name);
            let cdo = of & object::CLASS_DEFAULT_OBJECT != 0;
            c.cdo_bit += usize::from(cdo);
            c.cdo_bit_name_mismatch += usize::from(cdo != obj_name.starts_with("Default__"));
            if of & object::PROTECTED != 0 {
                c.protected += 1;
                c.protected_non_property += usize::from(!class.ends_with("Property"));
            }
            c.per_object_localized += usize::from(of & object::PER_OBJECT_LOCALIZED != 0);
            if class == "SoundNodeWave" {
                c.sound_waves += 1;
                c.sound_waves_without_pol += usize::from(of & object::PER_OBJECT_LOCALIZED == 0);
            }
            let rf_native = of & object::NATIVE != 0;
            if class == "Class" {
                let obj = decode_script_object(&pkg, Some(&base), i, &NoSchema).unwrap();
                let ScriptBody::Class { class: data, .. } = obj.body else {
                    panic!("{file} #{i} is not a class body");
                };
                let class_native = data.class_flags & flags::class::NATIVE != 0;
                match (rf_native, class_native) {
                    (true, true) => c.native_and_class_native += 1,
                    (false, false) => c.neither_native += 1,
                    _ => c.native_mismatch += 1,
                }
            } else if rf_native {
                c.native_on_non_class += 1;
            }

            let export_path = pkg.export_path(i).unwrap();
            let key = if is_script {
                Some(format!("{base}.{export_path}").to_ascii_lowercase())
            } else if top_is_forced_package {
                Some(export_path.to_ascii_lowercase())
            } else {
                None
            };
            if let Some(k) = key {
                targets.entry(k).or_default().push((class, of));
            }
        }

        for j in 0..pkg.imports.len() {
            let p = pkg.import_path(j).unwrap().to_ascii_lowercase();
            let class = pkg.fname(pkg.imports[j].class_name);
            imports.push((p, class));
        }
    }
    for (p, class) in &imports {
        let Some(t) = targets.get(p) else { continue };
        // Only count imports whose class agrees with a target of that path
        // (a class import `Core.MetaData` must not match an object of the
        // same path but another class).
        let same_class: Vec<_> = t.iter().filter(|(tc, _)| tc == class).collect();
        if same_class.is_empty() {
            continue;
        }
        c.imports_resolved += 1;
        if same_class.iter().any(|(_, of)| of & object::PUBLIC == 0) {
            c.imports_resolved_not_public += 1;
        }
    }
    c
}

fn census() -> Option<&'static Census> {
    static CELL: OnceLock<Option<Census>> = OnceLock::new();
    CELL.get_or_init(|| install_root().map(|r| build(&r)))
        .as_ref()
}

macro_rules! require_census {
    () => {
        match census() {
            Some(c) => c,
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

#[test]
fn package_source_is_the_crc_of_the_base_file_name() {
    let c = require_census!();
    assert_eq!(c.packages.len(), 42);
    for p in &c.packages {
        assert_eq!(
            package_source_crc(&p.base),
            Some(p.package_source),
            "{}",
            p.file
        );
    }
}

#[test]
fn package_flag_census() {
    let c = require_census!();
    let mut values: BTreeMap<u32, usize> = BTreeMap::new();
    let mut union = 0u32;
    let count = |bit: u32| c.packages.iter().filter(|p| p.flags & bit != 0).count();
    for p in &c.packages {
        *values.entry(p.flags).or_default() += 1;
        union |= p.flags;
    }
    // Exactly nine bits occur.
    assert_eq!(union, 0x22AA_000D);
    assert_eq!(union.count_ones(), 9);
    assert_eq!(count(package_flags::ALLOW_DOWNLOAD), 32);
    assert_eq!(count(package_flags::SERVER_SIDE_ONLY), 20);
    assert_eq!(count(package_flags::COOKED), 39);
    assert_eq!(count(package_flags::CONTAINS_MAP), 12);
    assert_eq!(count(package_flags::DISALLOW_LAZY_LOADING), 37);
    assert_eq!(count(package_flags::CONTAINS_SCRIPT), 12);
    assert_eq!(count(package_flags::REQUIRE_IMPORTS_ALREADY_LOADED), 35);
    assert_eq!(count(package_flags::STORE_COMPRESSED), 38);
    assert_eq!(count(package_flags::NO_EXPORT_ALLOWED), 27);
    // The GUID cache package is the one the executable explicitly strips of
    // AllowDownload.
    let guid_cache = c.packages.iter().find(|p| p.base == "GuidCache").unwrap();
    assert_eq!(guid_cache.flags & package_flags::ALLOW_DOWNLOAD, 0);
}

#[test]
fn server_side_only_tracks_missing_net_objects() {
    let c = require_census!();
    let mut zero_net = 0;
    for p in &c.packages {
        let sso = p.flags & package_flags::SERVER_SIDE_ONLY != 0;
        if p.base.starts_with("RefShaderCache-") {
            // The shader-cache save path sets the bit explicitly; these are
            // also the only packages without PKG_Cooked.
            assert!(sso, "{}", p.file);
            assert_eq!(p.flags & package_flags::COOKED, 0, "{}", p.file);
            continue;
        }
        assert_eq!(sso, p.net_object_count == 0, "{}", p.file);
        zero_net += usize::from(p.net_object_count == 0);
    }
    assert_eq!(zero_net, 17);
    // Forced top-level package exports carry the same relation.
    assert_eq!(c.forced_top_sso_with_net_counts, 0);
    assert_eq!(c.forced_top_no_sso_without_net_counts, 0);
}

#[test]
fn forced_export_bit_marks_objects_of_forced_packages() {
    let c = require_census!();
    assert_eq!(c.exports, 202_685);
    assert_eq!(c.forced, 62_169);
    assert_eq!(c.forced_outside_forced_package, 0);
    assert_eq!(c.unforced_inside_forced_package, 0);
    // Export-level PackageFlags: non-zero exactly on forced top-level Package
    // exports (the original package's flags travel with its objects).
    assert_eq!(c.forced_top_packages, 711);
    assert_eq!(c.forced_top_packages_without_flags, 0);
    assert_eq!(c.other_exports_with_package_flags, 0);
}

#[test]
fn object_flags_stay_inside_the_loader_keep_mask() {
    let c = require_census!();
    assert_eq!(c.outside_keep_mask, 0);
    assert_eq!(c.without_load_for_edit, 0);
    assert_eq!(c.edit_only, 16_730);
    assert_eq!(c.not_for_client, 3_315);
    assert_eq!(c.not_for_client_mismatch, 0);
    assert_eq!(c.not_for_client_loaded, 0);
    assert_eq!(c.cdo_bit, 2_521);
    assert_eq!(c.cdo_bit_name_mismatch, 0);
    assert_eq!(c.protected, 211);
    assert_eq!(c.protected_non_property, 0);
    assert_eq!(c.per_object_localized, 877);
    assert_eq!(c.sound_waves, 852);
    assert_eq!(c.sound_waves_without_pol, 0);
}

#[test]
fn native_object_bit_matches_native_class_flag() {
    let c = require_census!();
    assert_eq!(c.native_and_class_native, 1_652);
    assert_eq!(c.neither_native, 869);
    assert_eq!(c.native_mismatch, 0);
    assert_eq!(c.native_on_non_class, 0);
}

#[test]
fn every_resolvable_cross_package_import_targets_a_public_export() {
    let c = require_census!();
    assert_eq!(c.imports_resolved, 10_259);
    assert_eq!(c.imports_resolved_not_public, 0);
}

#[test]
fn name_flags_are_the_save_mark_plus_log_suppression() {
    let c = require_census!();
    assert_eq!(c.names, 77_358);
    assert_eq!(c.names_without_saved_bits, 0);
    assert_eq!(c.names_suppressed, 51);
    assert_eq!(c.suppressed_not_in_list, 0);
    assert_eq!(c.in_list_not_suppressed, 0);
}
