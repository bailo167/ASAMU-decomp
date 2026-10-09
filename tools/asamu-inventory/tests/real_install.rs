//! Real-data check: re-inventories the original install and compares it with the committed
//! sanitized inventory. Skips (passes with a note) when the game is not installed, as on CI,
//! or when the installed build is not the one the committed inventory describes.

use std::path::PathBuf;

use asamu_inventory::{ScanOptions, scan, to_json};

const COMMITTED: &str = "mac-depot278362-build1822049.json";
const BUILD: u64 = 1822049;

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
    // Compare the data sections; the generator line may carry a newer tool version.
    for key in [
        "totals",
        "categories",
        "extensions",
        "types",
        "tocs",
        "files",
    ] {
        assert_eq!(
            fresh[key], committed[key],
            "section {key:?} differs from {COMMITTED}; regenerate it with asamu-inventory"
        );
    }
    assert_eq!(committed["steam"]["build_id"], BUILD);
}
