//! Where the real-data tests find the user's own installed game (shared by
//! every `*_real_data.rs` file; nothing here reads more than folder names).
//!
//! `ASAMU_ORIGINAL_DIR` names the install root. Without it the default macOS
//! Steam location is tried. The retail game has two layouts:
//!
//! - macOS: `<root>/A Story About My Uncle.app/Contents/Resources/{ASAMU/CookedMac, Engine}`
//! - Windows: `<root>/{ASAMU/CookedPC, Engine}`
//!
//! The Mac depot reuses the PC cook (docs/reverse-engineering/INVENTORY.md:
//! the 42 packages have the same sizes and SHA-256 in both installs), so the
//! counts these tests assert hold for either build and no test is tied to one.

#![allow(dead_code)] // each test file uses a part of this module

use std::path::{Path, PathBuf};

/// Folder of the Mac app bundle that holds `ASAMU/` and `Engine/`.
const MAC_CONTENT: [&str; 3] = ["A Story About My Uncle.app", "Contents", "Resources"];
/// Steam's default install folder below `HOME` on macOS.
const MAC_STEAM_INSTALL: [&str; 6] = [
    "Library",
    "Application Support",
    "Steam",
    "steamapps",
    "common",
    "A Story About My Uncle",
];

/// The folders of an original install that the tests read.
pub struct OriginalData {
    /// Holds `ASAMU/` and `Engine/`.
    pub content_dir: PathBuf,
    /// Holds the cooked packages and `Maps/`.
    pub cooked_dir: PathBuf,
}

fn join_all(base: &Path, parts: &[&str]) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in parts {
        path.push(part);
    }
    path
}

/// The layout of the install at `root`, if it is one of the two known ones.
pub fn original_data_in(root: &Path) -> Option<OriginalData> {
    let mac = join_all(root, &MAC_CONTENT);
    let candidates = [
        (mac.clone(), join_all(&mac, &["ASAMU", "CookedMac"])),
        (root.to_path_buf(), join_all(root, &["ASAMU", "CookedPC"])),
    ];
    candidates
        .into_iter()
        .find(|(_, cooked)| cooked.is_dir())
        .map(|(content_dir, cooked_dir)| OriginalData {
            content_dir,
            cooked_dir,
        })
}

/// The user's install, or `None` when the game data is absent (CI).
pub fn original_data() -> Option<OriginalData> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => join_all(
            &PathBuf::from(std::env::var_os("HOME")?),
            &MAC_STEAM_INSTALL,
        ),
    };
    original_data_in(&root)
}

/// The cooked folder of the user's install (`CookedMac` or `CookedPC`).
pub fn cooked_dir() -> Option<PathBuf> {
    original_data().map(|d| d.cooked_dir)
}
