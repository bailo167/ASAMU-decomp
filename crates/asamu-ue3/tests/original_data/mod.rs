//! Where the real-data tests find the user's own installed game (shared by
//! every `*_real_data.rs` file; nothing here reads more than folder names
//! and Steam's library metadata).
//!
//! The install is found by `asamu-locate`, as the tools find it: the root
//! named by `ASAMU_ORIGINAL_DIR`, else the Steam library that holds the game
//! (the host's default Steam folders, or `ASAMU_STEAM_ROOT`). The tests
//! therefore run on a Windows or Linux machine with the game installed
//! without any variable set, and a machine without the game (CI) skips them.
//! The retail game has two layouts:
//!
//! - macOS: `<root>/A Story About My Uncle.app/Contents/Resources/{ASAMU/CookedMac, Engine}`
//! - Windows: `<root>/{ASAMU/CookedPC, Engine}`
//!
//! The Mac depot reuses the PC cook. CONFIRMED on 2026-10-10 for Steam build
//! 1822049: `asamu-inventory` run on a Windows install (depot 278361) gives,
//! for all 48 files of `ASAMU/CookedPC` (the 42 packages among them), the
//! size and SHA-256 that the committed Mac inventory
//! (docs/reverse-engineering/data/inventory) lists under `CookedMac`. The
//! counts these tests assert therefore hold for either build, and no test is
//! tied to one.

#![allow(dead_code)] // each test file uses a part of this module

use std::path::{Path, PathBuf};

/// The folders of an original install that the tests read.
pub struct OriginalData {
    /// Holds `ASAMU/` and `Engine/`.
    pub content_dir: PathBuf,
    /// Holds the cooked packages and `Maps/`.
    pub cooked_dir: PathBuf,
}

impl From<asamu_locate::Install> for OriginalData {
    fn from(install: asamu_locate::Install) -> Self {
        Self {
            content_dir: install.content_dir,
            cooked_dir: install.cooked_dir,
        }
    }
}

/// The install at `root`, if it has one of the known layouts.
pub fn original_data_in(root: &Path) -> Option<OriginalData> {
    asamu_locate::from_original_dir(root).ok().map(Into::into)
}

/// The user's install, or `None` when the game data is absent (CI).
pub fn original_data() -> Option<OriginalData> {
    asamu_locate::locate().ok().map(Into::into)
}

/// The cooked folder of the user's install (`CookedMac` or `CookedPC`).
pub fn cooked_dir() -> Option<PathBuf> {
    original_data().map(|d| d.cooked_dir)
}
