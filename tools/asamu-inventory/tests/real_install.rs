//! Real-data check: re-inventories the original install and checks that every original file of
//! the committed sanitized inventory is still present and identical; files the game itself
//! writes when it runs (generated ini files, logs, saves) are tolerated. Skips (passes with a note) when the game is not installed, as on CI,
//! or when the installed build is not the one the committed inventory describes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use asamu_inventory::{ScanOptions, scan, to_json};

const COMMITTED: &str = "mac-depot278362-build1822049.json";
const BUILD: u64 = 1822049;

/// Files the game itself writes when it runs: generated configuration, logs, saves and
/// cloud state inside its game-content directory (`ASAMU/` next to `Engine/`).
fn is_runtime_output(path: &str) -> bool {
    let Some(rest) = path
        .split_once("/ASAMU/")
        .map(|(_, rest)| rest)
        .or_else(|| path.strip_prefix("ASAMU/"))
    else {
        return false;
    };
    let lower = rest.to_ascii_lowercase();
    lower.starts_with("saves/")
        || lower.starts_with("logs/")
        || lower.starts_with("cloud/")
        || (lower.starts_with("config/")
            && lower.ends_with(".ini")
            && !lower["config/".len()..].contains('/')
            && {
                let name = &lower["config/".len()..];
                name.starts_with("asamu")
                    || name.starts_with("mac-asamu")
                    || name.starts_with("pc-asamu")
            })
}

#[test]
fn runtime_output_rule() {
    for ok in [
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Saves/SaveGame.bin",
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Config/Mac-ASAMUEngine.ini",
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Config/ASAMUSettings.ini",
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Logs/benchmark.log",
        "ASAMU/Cloud/CloudStorage.ini",
    ] {
        assert!(is_runtime_output(ok), "{ok}");
    }
    for bad in [
        "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Extra.upk",
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Config/DefaultGame.ini",
        "A Story About My Uncle.app/Contents/Resources/ASAMU/Config/Mac/MacEngine.ini",
        "A Story About My Uncle.app/Contents/MacOS/ASAMU2",
        "A Story About My Uncle.app/Contents/Resources/Engine/Config/ASAMUEngine.ini",
    ] {
        assert!(!is_runtime_output(bad), "{bad}");
    }
}

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reverse-engineering/data/inventory")
        .join(COMMITTED)
}

#[test]
fn real_install_matches_committed_inventory() {
    let install = match asamu_locate::locate() {
        Ok(install) => install,
        Err(e) => {
            eprintln!("SKIP real_install_matches_committed_inventory: {e}");
            return;
        }
    };
    if install.build_id != Some(BUILD) {
        eprintln!(
            "SKIP real_install_matches_committed_inventory: installed build {:?} is not {BUILD}",
            install.build_id
        );
        return;
    }
    let committed_text = match std::fs::read_to_string(committed_path()) {
        Ok(text) => text,
        Err(e) => {
            eprintln!(
                "SKIP real_install_matches_committed_inventory: no committed inventory ({e})"
            );
            return;
        }
    };
    let inventory = scan(&install.root, &ScanOptions::default()).unwrap();
    let fresh: serde_json::Value = serde_json::from_str(&to_json(&inventory).unwrap()).unwrap();
    let committed: serde_json::Value = serde_json::from_str(&committed_text).unwrap();
    // Every original file of the committed inventory must still be there, byte-identical
    // (path, size, hash, type and category all compare equal).
    let by_path = |v: &serde_json::Value| -> BTreeMap<String, serde_json::Value> {
        v["files"]
            .as_array()
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| Some((f["path"].as_str()?.to_owned(), f.clone())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let fresh_files = by_path(&fresh);
    let committed_files = by_path(&committed);
    assert!(!committed_files.is_empty(), "{COMMITTED} lists no files");
    for (path, entry) in &committed_files {
        assert_eq!(
            fresh_files.get(path),
            Some(entry),
            "original file {path:?} is missing or differs from {COMMITTED}"
        );
    }
    // Running the game makes it write its own output next to its data (generated ini
    // files, logs, saves, cloud state). Those are not original files: they are allowed,
    // and anything else that is new is reported as a failure.
    let extras: Vec<&String> = fresh_files
        .keys()
        .filter(|p| !committed_files.contains_key(*p))
        .collect();
    for path in &extras {
        assert!(
            is_runtime_output(path),
            "unexpected new file {path:?} in the install (not in {COMMITTED} and not game runtime output)"
        );
    }
    if !extras.is_empty() {
        eprintln!(
            "NOTE real_install_matches_committed_inventory: {} runtime-generated file(s) ignored",
            extras.len()
        );
    }
    // The TOC files themselves are original files (checked above). Their derived summary
    // must agree too, apart from the "on disk but not in this TOC" counters, which count
    // the runtime output as well.
    let toc_core = |v: &serde_json::Value| -> Vec<serde_json::Value> {
        v["tocs"]
            .as_array()
            .map(|tocs| {
                tocs.iter()
                    .map(|t| {
                        let mut t = t.clone();
                        if let Some(o) = t.as_object_mut() {
                            o.remove("unlisted_files");
                            o.remove("unlisted_by_dir");
                        }
                        t
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        toc_core(&fresh),
        toc_core(&committed),
        "section \"tocs\" differs from {COMMITTED}; regenerate it with asamu-inventory"
    );
    assert_eq!(committed["steam"]["build_id"], BUILD);
}
