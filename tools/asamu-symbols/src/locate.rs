//! Discovery of the original Mac executable.
//!
//! Order: an explicit `--binary` path (handled by the caller), then
//! `ASAMU_ORIGINAL_DIR` (the folder that contains `A Story About My Uncle.app`),
//! then `ASAMU_STEAM_ROOT`, then the default macOS Steam library under `$HOME`.
//! Nothing here hard-codes a user name; paths are built from the environment.

use std::path::{Path, PathBuf};

/// Name of the Steam install folder (`steamapps/common/<this>`).
pub const INSTALL_DIR_NAME: &str = "A Story About My Uncle";
/// Name of the Mac application bundle inside the install folder.
pub const APP_BUNDLE_NAME: &str = "A Story About My Uncle.app";
/// Executable path relative to the install root.
pub const EXECUTABLE_COMPONENTS: [&str; 4] = [APP_BUNDLE_NAME, "Contents", "MacOS", "ASAMU"];

/// How a binary path was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `ASAMU_ORIGINAL_DIR`.
    OriginalDirEnv,
    /// `ASAMU_STEAM_ROOT`.
    SteamRootEnv,
    /// `$HOME/Library/Application Support/Steam`.
    DefaultMacSteam,
}

impl Source {
    /// Short label for diagnostics (never includes the path itself).
    pub fn label(&self) -> &'static str {
        match self {
            Source::OriginalDirEnv => "ASAMU_ORIGINAL_DIR",
            Source::SteamRootEnv => "ASAMU_STEAM_ROOT",
            Source::DefaultMacSteam => "default macOS Steam library",
        }
    }
}

/// The executable path inside an install root (the folder containing the `.app`).
pub fn executable_in_root(root: &Path) -> PathBuf {
    let mut path = root.to_path_buf();
    for component in EXECUTABLE_COMPONENTS {
        path.push(component);
    }
    path
}

/// The install root inside a Steam root.
pub fn install_root_in_steam(steam_root: &Path) -> PathBuf {
    steam_root
        .join("steamapps")
        .join("common")
        .join(INSTALL_DIR_NAME)
}

/// Candidate executable paths in priority order, built from the given
/// environment values (injected so that tests do not touch the real environment).
pub fn candidates(
    original_dir: Option<&str>,
    steam_root: Option<&str>,
    home: Option<&str>,
) -> Vec<(PathBuf, Source)> {
    let mut out = Vec::new();
    if let Some(dir) = original_dir.filter(|d| !d.is_empty()) {
        out.push((executable_in_root(Path::new(dir)), Source::OriginalDirEnv));
    }
    if let Some(root) = steam_root.filter(|d| !d.is_empty()) {
        out.push((
            executable_in_root(&install_root_in_steam(Path::new(root))),
            Source::SteamRootEnv,
        ));
    }
    if let Some(home) = home.filter(|d| !d.is_empty()) {
        let steam = Path::new(home)
            .join("Library")
            .join("Application Support")
            .join("Steam");
        out.push((
            executable_in_root(&install_root_in_steam(&steam)),
            Source::DefaultMacSteam,
        ));
    }
    out
}

/// Find the original executable using the process environment.
/// Returns `None` when no candidate exists (e.g. CI without game data).
pub fn find_original_binary() -> Option<(PathBuf, Source)> {
    let original = std::env::var("ASAMU_ORIGINAL_DIR").ok();
    let steam = std::env::var("ASAMU_STEAM_ROOT").ok();
    let home = std::env::var("HOME").ok();
    candidates(original.as_deref(), steam.as_deref(), home.as_deref())
        .into_iter()
        .find(|(path, _)| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_and_layout() {
        let c = candidates(Some("/r"), Some("/s"), Some("/h"));
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].1, Source::OriginalDirEnv);
        assert!(
            c[0].0
                .ends_with("A Story About My Uncle.app/Contents/MacOS/ASAMU")
        );
        assert!(
            c[1].0
                .starts_with("/s/steamapps/common/A Story About My Uncle")
        );
        assert!(
            c[2].0
                .starts_with("/h/Library/Application Support/Steam/steamapps/common")
        );
    }

    #[test]
    fn empty_values_are_ignored() {
        assert!(candidates(Some(""), None, Some("")).is_empty());
    }
}
