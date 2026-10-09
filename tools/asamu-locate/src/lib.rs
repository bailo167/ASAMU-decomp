//! Locate a legitimate Steam installation of *A Story About My Uncle* (Steam App ID 278360).
//!
//! Discovery order:
//!
//! 1. `ASAMU_ORIGINAL_DIR` (or [`LocateOptions::original_dir`]) overrides Steam discovery
//!    entirely. It may name the folder that contains `A Story About My Uncle.app`, the `.app`
//!    bundle itself, or a Windows/Linux-style root that contains `ASAMU/` plus `Engine/` or
//!    `Binaries/`.
//! 2. Otherwise the Steam root is `ASAMU_STEAM_ROOT` if set, else the per-OS default roots
//!    ([`steam_root_candidates`]). For each root that exists, `steamapps/libraryfolders.vdf`
//!    (modern or legacy format) lists the libraries; the library holding
//!    `steamapps/appmanifest_278360.acf` wins, and the manifest's `installdir` names the
//!    folder under `steamapps/common/`.
//!
//! No username or home path is hard-coded: everything is derived from `HOME` /
//! `USERPROFILE` / `XDG_DATA_HOME` / `ProgramFiles(x86)` / `ProgramFiles`.
//!
//! Library code never reads the process environment except in [`LocateOptions::from_env`],
//! so tests can describe a fake machine with [`LocateOptions`] directly.

pub mod steam;
pub mod vdf;

use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

pub use steam::{AppManifest, Depot, LibraryFolder};

/// Steam App ID of *A Story About My Uncle*.
pub const APP_ID: u32 = 278360;
/// The Mac depot observed on the analysis machine (see docs/reverse-engineering/INVENTORY.md).
pub const MAC_DEPOT_ID: u32 = 278362;
/// Default `installdir` under `steamapps/common`.
pub const DEFAULT_INSTALL_DIR: &str = "A Story About My Uncle";
/// Name of the macOS application bundle inside the install folder.
pub const MAC_APP_BUNDLE: &str = "A Story About My Uncle.app";
/// Environment variable that overrides discovery with an explicit install root.
pub const ENV_ORIGINAL_DIR: &str = "ASAMU_ORIGINAL_DIR";
/// Environment variable that overrides the Steam root used for discovery.
pub const ENV_STEAM_ROOT: &str = "ASAMU_STEAM_ROOT";

/// Upper bound for reading Steam metadata files (they are a few KiB).
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// Install layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// `A Story About My Uncle.app/Contents/{MacOS/ASAMU, Resources/ASAMU, Resources/Engine}`.
    /// CONFIRMED on the analysis machine (depot 278362, build 1822049).
    MacApp,
    /// `Binaries/Win32|Win64/*.exe`, `ASAMU/CookedPC*`. Expected, UNVERIFIED here.
    Windows,
    /// `Binaries/Linux*`. UNVERIFIED (no Linux depot has been observed).
    Linux,
    /// `ASAMU/` plus `Engine/` with no recognised platform binaries (e.g. a copied data tree).
    Loose,
}

impl Layout {
    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            Layout::MacApp => "mac-app",
            Layout::Windows => "windows",
            Layout::Linux => "linux",
            Layout::Loose => "loose",
        }
    }
}

/// Operating system whose Steam default roots are searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostOs {
    /// macOS.
    MacOs,
    /// Linux (native, Flatpak and Snap Steam).
    Linux,
    /// Windows.
    Windows,
    /// Anything else: only explicit overrides work.
    Other,
}

impl HostOs {
    /// The OS this binary was compiled for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else if cfg!(target_os = "linux") {
            HostOs::Linux
        } else if cfg!(target_os = "windows") {
            HostOs::Windows
        } else {
            HostOs::Other
        }
    }
}

impl Default for HostOs {
    fn default() -> Self {
        HostOs::current()
    }
}

/// Everything discovery depends on, so tests can describe a fake machine.
#[derive(Debug, Clone, Default)]
pub struct LocateOptions {
    /// Explicit install root (`ASAMU_ORIGINAL_DIR`). Overrides Steam discovery entirely.
    pub original_dir: Option<PathBuf>,
    /// Explicit Steam root (`ASAMU_STEAM_ROOT`). Replaces the default root list.
    pub steam_root: Option<PathBuf>,
    /// Home directory (`HOME`, or `USERPROFILE` on Windows).
    pub home: Option<PathBuf>,
    /// `XDG_DATA_HOME` (Linux).
    pub xdg_data_home: Option<PathBuf>,
    /// `ProgramFiles(x86)` (Windows).
    pub program_files_x86: Option<PathBuf>,
    /// `ProgramFiles` (Windows).
    pub program_files: Option<PathBuf>,
    /// Which OS's default roots to search.
    pub host: HostOs,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl LocateOptions {
    /// Read the options from the process environment.
    pub fn from_env() -> Self {
        let host = HostOs::current();
        let home = if host == HostOs::Windows {
            env_path("USERPROFILE").or_else(|| env_path("HOME"))
        } else {
            env_path("HOME")
        };
        LocateOptions {
            original_dir: env_path(ENV_ORIGINAL_DIR),
            steam_root: env_path(ENV_STEAM_ROOT),
            home,
            xdg_data_home: env_path("XDG_DATA_HOME"),
            program_files_x86: env_path("ProgramFiles(x86)"),
            program_files: env_path("ProgramFiles"),
            host,
        }
    }
}

/// How the install was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Discovery {
    /// From `ASAMU_ORIGINAL_DIR` / `--original-dir`.
    OriginalDir {
        /// The path exactly as given.
        given: PathBuf,
    },
    /// Through Steam library metadata.
    Steam {
        /// Steam root whose `libraryfolders.vdf` was read.
        steam_root: PathBuf,
        /// Library folder that contains the game.
        library: PathBuf,
        /// The app manifest used, if one existed.
        manifest_path: Option<PathBuf>,
    },
}

/// A located install and its well-known directories.
#[derive(Debug, Clone, Serialize)]
pub struct Install {
    /// Install root: the folder that contains `A Story About My Uncle.app` (Mac), or the
    /// folder that contains `ASAMU/` and `Engine/`/`Binaries/` (Windows/Linux).
    pub root: PathBuf,
    /// Detected layout.
    pub layout: Layout,
    /// Steam App ID (always [`APP_ID`]).
    pub app_id: u32,
    /// `buildid` from the app manifest, when one was read.
    pub build_id: Option<u64>,
    /// `InstalledDepots` from the app manifest (empty when no manifest was read).
    pub depots: Vec<Depot>,
    /// How the install was found.
    pub discovery: Discovery,
    /// The `.app` bundle (Mac layout only).
    pub app_bundle: Option<PathBuf>,
    /// Folder that contains `ASAMU/` and `Engine/` (`.app/Contents/Resources` on Mac).
    pub content_dir: PathBuf,
    /// Game content folder (`ASAMU/`).
    pub game_dir: PathBuf,
    /// Engine folder (`Engine/`).
    pub engine_dir: PathBuf,
    /// Cooked package folder (`ASAMU/CookedMac`, `ASAMU/CookedPC`, ...).
    pub cooked_dir: PathBuf,
    /// Cooked maps folder (`<cooked>/Maps`).
    pub maps_dir: PathBuf,
    /// Game config folder (`ASAMU/Config`).
    pub config_dir: PathBuf,
    /// Game localization folder (`ASAMU/Localization`).
    pub localization_dir: PathBuf,
    /// Main executable, when present.
    pub executable: Option<PathBuf>,
    /// The parsed app manifest (account fields omitted), when one was read.
    pub manifest: Option<AppManifest>,
    /// Non-fatal oddities noticed during discovery.
    pub warnings: Vec<String>,
}

/// Discovery failure. Messages say what was tried and how to fix it.
#[derive(Debug, thiserror::Error)]
pub enum LocateError {
    /// `ASAMU_ORIGINAL_DIR` does not point at a usable install.
    #[error(
        "{ENV_ORIGINAL_DIR}={} is not a usable A Story About My Uncle install: {reason}. \
         Point it at the folder containing \"{MAC_APP_BUNDLE}\", the .app bundle itself, \
         or a root containing ASAMU/ and Engine/ (or Binaries/)",
        path.display()
    )]
    BadOriginalDir {
        /// The path given.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// No Steam root exists.
    #[error(
        "no Steam installation found; tried: {}. Set {ENV_STEAM_ROOT} to your Steam folder \
         or {ENV_ORIGINAL_DIR} to the game folder",
        list_paths(tried)
    )]
    NoSteam {
        /// Steam roots that were checked.
        tried: Vec<PathBuf>,
    },
    /// Steam exists but no library contains App ID 278360.
    #[error(
        "Steam found at {} but App ID {APP_ID} (A Story About My Uncle) is not installed in \
         any library; checked libraries: {}{}. Install the game through Steam or set \
         {ENV_ORIGINAL_DIR}",
        list_paths(steam_roots),
        list_paths(libraries),
        format_notes(notes)
    )]
    NotInstalled {
        /// Steam roots that exist.
        steam_roots: Vec<PathBuf>,
        /// Library folders that were checked.
        libraries: Vec<PathBuf>,
        /// Details about near misses (bad manifests, missing folders).
        notes: Vec<String>,
    },
}

fn list_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "(none)".to_string();
    }
    paths
        .iter()
        .map(|p| format!("\"{}\"", p.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_notes(notes: &[String]) -> String {
    let mut out = String::new();
    for note in notes {
        let _ = write!(out, " [{note}]");
    }
    out
}

/// Locate the install using the process environment.
pub fn locate() -> Result<Install, LocateError> {
    locate_with(&LocateOptions::from_env())
}

/// Locate the install using explicit options.
pub fn locate_with(opts: &LocateOptions) -> Result<Install, LocateError> {
    if let Some(dir) = &opts.original_dir {
        return from_original_dir(dir);
    }
    locate_via_steam(opts)
}

/// Default Steam roots for `opts.host`, or just `opts.steam_root` when it is set.
pub fn steam_root_candidates(opts: &LocateOptions) -> Vec<PathBuf> {
    if let Some(root) = &opts.steam_root {
        return vec![root.clone()];
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let home = opts.home.as_deref();
    match opts.host {
        HostOs::MacOs => {
            if let Some(h) = home {
                out.push(h.join("Library").join("Application Support").join("Steam"));
            }
        }
        HostOs::Linux => {
            if let Some(h) = home {
                out.push(h.join(".steam").join("steam"));
                out.push(h.join(".steam").join("root"));
            }
            if let Some(xdg) = &opts.xdg_data_home {
                out.push(xdg.join("Steam"));
            }
            if let Some(h) = home {
                out.push(h.join(".local").join("share").join("Steam"));
                out.push(
                    h.join(".var")
                        .join("app")
                        .join("com.valvesoftware.Steam")
                        .join(".local")
                        .join("share")
                        .join("Steam"),
                );
                out.push(
                    h.join("snap")
                        .join("steam")
                        .join("common")
                        .join(".local")
                        .join("share")
                        .join("Steam"),
                );
            }
        }
        HostOs::Windows => {
            if let Some(pf) = &opts.program_files_x86 {
                out.push(pf.join("Steam"));
            }
            if let Some(pf) = &opts.program_files {
                out.push(pf.join("Steam"));
            }
            out.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
            if let Some(h) = home {
                out.push(h.join("Steam"));
                out.push(h.join("scoop").join("apps").join("steam").join("current"));
            }
        }
        HostOs::Other => {}
    }
    dedupe_paths(out)
}

/// Remove duplicates (including different spellings of the same directory, such as
/// `~/.steam/steam` symlinked to `~/.local/share/Steam`) while keeping order.
fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for p in paths {
        let key = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if !seen.contains(&key) {
            seen.push(key);
            out.push(p);
        }
    }
    out
}

/// Find a child entry by name, exactly first and then ASCII-case-insensitively (Windows data
/// copied to a case-sensitive filesystem, `SteamApps` vs `steamapps`, ...).
///
/// The returned path uses the on-disk spelling, also on case-insensitive filesystems.
fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        // Unlistable directory: fall back to a direct lookup.
        let exact = dir.join(name);
        return fs::symlink_metadata(&exact).is_ok().then_some(exact);
    };
    let mut matches: Vec<(bool, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file_name = e.file_name();
            let file_name = file_name.to_string_lossy();
            file_name
                .eq_ignore_ascii_case(name)
                .then(|| (file_name != name, e.path()))
        })
        .collect();
    // Exact spelling first, then case-insensitive matches in sorted order.
    matches.sort();
    matches.into_iter().next().map(|(_, path)| path)
}

fn child_dir_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    child_ci(dir, name).filter(|p| p.is_dir())
}

fn child_file_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    child_ci(dir, name).filter(|p| p.is_file())
}

fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(MAX_METADATA_BYTES.saturating_add(1))
        .read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > MAX_METADATA_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("larger than {MAX_METADATA_BYTES} bytes"),
        ));
    }
    Ok(buf)
}

/// `installdir` must be one plain path component: never absolute, never `..`, never a
/// separator. The manifest is input like any other and must not steer reads elsewhere.
fn is_safe_component(name: &str) -> bool {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Paths derived from an install root by layout detection.
#[derive(Debug, Clone)]
struct DetectedPaths {
    root: PathBuf,
    layout: Layout,
    app_bundle: Option<PathBuf>,
    content_dir: PathBuf,
    game_dir: PathBuf,
    engine_dir: PathBuf,
    cooked_dir: PathBuf,
    maps_dir: PathBuf,
    config_dir: PathBuf,
    localization_dir: PathBuf,
    executable: Option<PathBuf>,
    warnings: Vec<String>,
}

fn has_app_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("app"))
}

fn is_mac_bundle(path: &Path) -> bool {
    has_app_extension(path)
        && child_dir_ci(path, "Contents")
            .and_then(|c| child_dir_ci(&c, "Resources"))
            .is_some_and(|r| child_dir_ci(&r, "ASAMU").is_some())
}

/// Pick the cooked folder: preferred names first, then any `Cooked*` folder (sorted).
fn find_cooked_dir(game_dir: &Path, preferred: &[&str]) -> Option<PathBuf> {
    for name in preferred {
        if let Some(dir) = child_dir_ci(game_dir, name) {
            return Some(dir);
        }
    }
    let mut others: Vec<PathBuf> = fs::read_dir(game_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("cooked")
        })
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    others.sort();
    others.into_iter().next()
}

fn first_existing_file(base: &Path, candidates: &[&[&str]]) -> Option<PathBuf> {
    'outer: for parts in candidates {
        let mut path = base.to_path_buf();
        for part in *parts {
            match child_ci(&path, part) {
                Some(p) => path = p,
                None => continue 'outer,
            }
        }
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Expected Windows executables, most specific first. `ASAMU-Win32-Shipping.exe` is named in
/// the shipped `PCTOC.txt` (STRONG); the others are conventional UE3/UDK names (TENTATIVE).
const WINDOWS_EXECUTABLES: &[&[&str]] = &[
    &["Binaries", "Win32", "ASAMU-Win32-Shipping.exe"],
    &["Binaries", "Win32", "ASAMU.exe"],
    &["Binaries", "Win64", "ASAMU-Win64-Shipping.exe"],
    &["Binaries", "Win64", "ASAMU.exe"],
    &["Binaries", "Win32", "UDK.exe"],
    &["Binaries", "Win64", "UDK.exe"],
];

/// Linux executables (UNVERIFIED: no Linux depot has been observed).
const LINUX_EXECUTABLES: &[&[&str]] = &[
    &["Binaries", "Linux", "ASAMU"],
    &["Binaries", "Linux", "ASAMU-Linux-Shipping"],
    &["Binaries", "Linux64", "ASAMU"],
];

fn detect_mac(root: PathBuf, bundle: PathBuf) -> Result<DetectedPaths, String> {
    let contents = child_dir_ci(&bundle, "Contents")
        .ok_or_else(|| format!("{} has no Contents/ folder", bundle.display()))?;
    let content_dir = child_dir_ci(&contents, "Resources")
        .ok_or_else(|| format!("{} has no Resources/ folder", contents.display()))?;
    let game_dir = child_dir_ci(&content_dir, "ASAMU")
        .ok_or_else(|| format!("{} has no ASAMU/ folder", content_dir.display()))?;
    let mut warnings = Vec::new();
    let engine_dir = child_dir_ci(&content_dir, "Engine").unwrap_or_else(|| {
        warnings.push(format!("{} has no Engine/ folder", content_dir.display()));
        content_dir.join("Engine")
    });
    let cooked_dir = find_cooked_dir(&game_dir, &["CookedMac"])
        .ok_or_else(|| format!("{} has no Cooked*/ folder", game_dir.display()))?;
    let executable = child_dir_ci(&contents, "MacOS").and_then(|m| child_file_ci(&m, "ASAMU"));
    if executable.is_none() {
        warnings.push("Contents/MacOS/ASAMU not found".to_string());
    }
    Ok(finish_paths(
        root,
        Layout::MacApp,
        Some(bundle),
        content_dir,
        game_dir,
        engine_dir,
        cooked_dir,
        executable,
        warnings,
    ))
}

fn detect_loose(root: PathBuf) -> Result<DetectedPaths, String> {
    let game_dir = child_dir_ci(&root, "ASAMU")
        .ok_or_else(|| format!("{} has no ASAMU/ folder", root.display()))?;
    let engine = child_dir_ci(&root, "Engine");
    let binaries = child_dir_ci(&root, "Binaries");
    if engine.is_none() && binaries.is_none() {
        return Err(format!(
            "{} has ASAMU/ but neither Engine/ nor Binaries/",
            root.display()
        ));
    }
    let has_bin = |name: &str| {
        binaries
            .as_deref()
            .and_then(|b| child_dir_ci(b, name))
            .is_some()
    };
    let layout = if has_bin("Win32") || has_bin("Win64") {
        Layout::Windows
    } else if has_bin("Linux") || has_bin("Linux64") {
        Layout::Linux
    } else {
        Layout::Loose
    };
    let preferred: &[&str] = match layout {
        Layout::Windows => &["CookedPC", "CookedPCConsole"],
        Layout::Linux => &["CookedLinux", "CookedPC", "CookedPCConsole"],
        Layout::MacApp | Layout::Loose => &["CookedPC", "CookedPCConsole", "CookedMac"],
    };
    let cooked_dir = find_cooked_dir(&game_dir, preferred)
        .ok_or_else(|| format!("{} has no Cooked*/ folder", game_dir.display()))?;
    // A loose tree whose cooked folder is CookedPC* is most likely Windows data.
    let layout = if layout == Layout::Loose
        && cooked_dir.file_name().is_some_and(|n| {
            n.to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("cookedpc")
        }) {
        Layout::Windows
    } else {
        layout
    };
    let mut warnings = Vec::new();
    let engine_dir = engine.unwrap_or_else(|| {
        warnings.push(format!("{} has no Engine/ folder", root.display()));
        root.join("Engine")
    });
    let executable = match layout {
        Layout::Windows => first_existing_file(&root, WINDOWS_EXECUTABLES),
        Layout::Linux => first_existing_file(&root, LINUX_EXECUTABLES),
        Layout::MacApp | Layout::Loose => None,
    };
    if matches!(layout, Layout::Windows | Layout::Linux) {
        warnings.push(format!(
            "{} layout detection is UNVERIFIED (only the Mac depot has been examined)",
            layout.label()
        ));
    }
    Ok(finish_paths(
        root.clone(),
        layout,
        None,
        root,
        game_dir,
        engine_dir,
        cooked_dir,
        executable,
        warnings,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish_paths(
    root: PathBuf,
    layout: Layout,
    app_bundle: Option<PathBuf>,
    content_dir: PathBuf,
    game_dir: PathBuf,
    engine_dir: PathBuf,
    cooked_dir: PathBuf,
    executable: Option<PathBuf>,
    mut warnings: Vec<String>,
) -> DetectedPaths {
    let maps_dir = child_dir_ci(&cooked_dir, "Maps").unwrap_or_else(|| {
        warnings.push(format!("{} has no Maps/ folder", cooked_dir.display()));
        cooked_dir.join("Maps")
    });
    let config_dir = child_dir_ci(&game_dir, "Config").unwrap_or_else(|| {
        warnings.push(format!("{} has no Config/ folder", game_dir.display()));
        game_dir.join("Config")
    });
    let localization_dir = child_dir_ci(&game_dir, "Localization").unwrap_or_else(|| {
        warnings.push(format!(
            "{} has no Localization/ folder",
            game_dir.display()
        ));
        game_dir.join("Localization")
    });
    DetectedPaths {
        root,
        layout,
        app_bundle,
        content_dir,
        game_dir,
        engine_dir,
        cooked_dir,
        maps_dir,
        config_dir,
        localization_dir,
        executable,
        warnings,
    }
}

/// The install root for a path naming the `.app` bundle itself: its parent folder. A bare
/// relative bundle name (`A Story About My Uncle.app`) has an empty parent, which means the
/// current directory, never the bundle itself.
fn bundle_root(bundle: &Path) -> PathBuf {
    match bundle.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) => PathBuf::from("."),
        // A filesystem root cannot be a bundle; keep the path rather than invent one.
        None => bundle.to_path_buf(),
    }
}

/// Detect the layout of an install root (see [`Layout`]). Returns a reason on failure.
fn detect(path: &Path) -> Result<DetectedPaths, String> {
    if !path.is_dir() {
        return Err(if path.exists() {
            "not a directory".to_string()
        } else {
            "does not exist".to_string()
        });
    }
    // The .app bundle itself: the install root is its parent folder.
    if is_mac_bundle(path) {
        return detect_mac(bundle_root(path), path.to_path_buf());
    }
    // The folder that contains the bundle (the Steam install dir on macOS).
    if let Some(bundle) = child_dir_ci(path, MAC_APP_BUNDLE).filter(|b| is_mac_bundle(b)) {
        return detect_mac(path.to_path_buf(), bundle);
    }
    // A renamed bundle: any *.app child that contains Contents/Resources/ASAMU.
    if let Ok(entries) = fs::read_dir(path) {
        let mut bundles: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir() && is_mac_bundle(p))
            .collect();
        bundles.sort();
        if let Some(bundle) = bundles.into_iter().next() {
            return detect_mac(path.to_path_buf(), bundle);
        }
    }
    if child_dir_ci(path, "ASAMU").is_some() {
        return detect_loose(path.to_path_buf());
    }
    Err(format!(
        "found neither \"{MAC_APP_BUNDLE}\" nor an ASAMU/ folder"
    ))
}

fn install_from(
    paths: DetectedPaths,
    discovery: Discovery,
    manifest: Option<AppManifest>,
    mut extra_warnings: Vec<String>,
) -> Install {
    let mut warnings = paths.warnings;
    warnings.append(&mut extra_warnings);
    if let Some(m) = &manifest {
        if let Some(id) = m.app_id.filter(|id| *id != APP_ID) {
            warnings.push(format!("app manifest names appid {id}, expected {APP_ID}"));
        }
        if m.fully_installed() == Some(false) {
            warnings.push(format!(
                "Steam StateFlags {} does not have the fully-installed bit (4); the install may be incomplete",
                m.state_flags.unwrap_or_default()
            ));
        }
    }
    Install {
        root: paths.root,
        layout: paths.layout,
        app_id: APP_ID,
        build_id: manifest.as_ref().and_then(|m| m.build_id),
        depots: manifest
            .as_ref()
            .map(|m| m.depots.clone())
            .unwrap_or_default(),
        discovery,
        app_bundle: paths.app_bundle,
        content_dir: paths.content_dir,
        game_dir: paths.game_dir,
        engine_dir: paths.engine_dir,
        cooked_dir: paths.cooked_dir,
        maps_dir: paths.maps_dir,
        config_dir: paths.config_dir,
        localization_dir: paths.localization_dir,
        executable: paths.executable,
        manifest,
        warnings,
    }
}

/// Build an [`Install`] from an explicit root (the `ASAMU_ORIGINAL_DIR` path).
///
/// Steam discovery is skipped. If the root happens to sit at
/// `<library>/steamapps/common/<installdir>` next to a matching `appmanifest_278360.acf`,
/// that manifest is read for the build/depot fields; otherwise they are empty.
pub fn from_original_dir(given: &Path) -> Result<Install, LocateError> {
    let paths = detect(given).map_err(|reason| LocateError::BadOriginalDir {
        path: given.to_path_buf(),
        reason,
    })?;
    let mut warnings = Vec::new();
    let manifest = sibling_manifest(&paths.root, &mut warnings);
    Ok(install_from(
        paths,
        Discovery::OriginalDir {
            given: given.to_path_buf(),
        },
        manifest,
        warnings,
    ))
}

fn sibling_manifest(root: &Path, warnings: &mut Vec<String>) -> Option<AppManifest> {
    let dir_name = root.file_name()?.to_string_lossy().into_owned();
    let common = root.parent()?;
    if !common
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("common"))
    {
        return None;
    }
    let steamapps = common.parent()?;
    let path = child_file_ci(steamapps, &format!("appmanifest_{APP_ID}.acf"))?;
    let manifest = match read_manifest(&path) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(e);
            return None;
        }
    };
    let matches = manifest
        .install_dir
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case(&dir_name));
    matches.then_some(manifest)
}

fn read_manifest(path: &Path) -> Result<AppManifest, String> {
    let bytes = read_capped(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    steam::parse_app_manifest(&String::from_utf8_lossy(&bytes))
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))
}

/// Read a Steam root's library list. The root itself is always a library (the legacy format
/// does not list it). Libraries that claim to hold the game come first.
fn libraries_for_root(root: &Path, notes: &mut Vec<String>) -> Vec<LibraryFolder> {
    let mut libs = vec![LibraryFolder {
        path: root.to_path_buf(),
        apps: None,
    }];
    let candidates = [
        child_dir_ci(root, "steamapps").and_then(|s| child_file_ci(&s, "libraryfolders.vdf")),
        child_dir_ci(root, "config").and_then(|c| child_file_ci(&c, "libraryfolders.vdf")),
    ];
    for vdf_path in candidates.into_iter().flatten() {
        let parsed = read_capped(&vdf_path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                steam::parse_library_folders(&String::from_utf8_lossy(&bytes))
                    .map_err(|e| e.to_string())
            });
        match parsed {
            Ok(found) => {
                let canonical_root = fs::canonicalize(root).ok();
                for lib in found {
                    let same_as_root = lib.path == root
                        || (canonical_root.is_some()
                            && fs::canonicalize(&lib.path).ok() == canonical_root);
                    if same_as_root {
                        // Keep the app list from the file for the root entry.
                        if let Some(first) = libs.first_mut() {
                            first.apps = lib.apps;
                        }
                    } else if !libs.iter().any(|l| l.path == lib.path) {
                        libs.push(lib);
                    }
                }
                break;
            }
            Err(e) => notes.push(format!("cannot use {}: {e}", vdf_path.display())),
        }
    }
    // Libraries that list the app first, then unknown (legacy), then the rest; stable.
    libs.sort_by_key(|l| match l.lists_app(APP_ID) {
        Some(true) => 0u8,
        None => 1,
        Some(false) => 2,
    });
    libs
}

fn locate_via_steam(opts: &LocateOptions) -> Result<Install, LocateError> {
    let roots = steam_root_candidates(opts);
    let mut existing_roots = Vec::new();
    let mut checked_libraries: Vec<PathBuf> = Vec::new();
    let mut notes = Vec::new();

    for root in &roots {
        if child_dir_ci(root, "steamapps").is_none() {
            continue;
        }
        existing_roots.push(root.clone());
        for lib in libraries_for_root(root, &mut notes) {
            if checked_libraries.contains(&lib.path) {
                continue;
            }
            checked_libraries.push(lib.path.clone());
            if let Some(install) = try_library(root, &lib, &mut notes) {
                return Ok(install);
            }
        }
    }

    if existing_roots.is_empty() {
        return Err(LocateError::NoSteam { tried: roots });
    }
    Err(LocateError::NotInstalled {
        steam_roots: existing_roots,
        libraries: checked_libraries,
        notes,
    })
}

fn try_library(steam_root: &Path, lib: &LibraryFolder, notes: &mut Vec<String>) -> Option<Install> {
    let Some(steamapps) = child_dir_ci(&lib.path, "steamapps") else {
        if lib.lists_app(APP_ID) == Some(true) {
            notes.push(format!(
                "library {} lists App ID {APP_ID} but has no steamapps/ folder",
                lib.path.display()
            ));
        }
        return None;
    };
    let common = child_dir_ci(&steamapps, "common");
    let manifest_path = child_file_ci(&steamapps, &format!("appmanifest_{APP_ID}.acf"));
    let mut warnings = Vec::new();

    let (manifest, install_dir) = match &manifest_path {
        Some(path) => match read_manifest(path) {
            Ok(m) => {
                let dir = match m.install_dir.as_deref() {
                    Some(d) if is_safe_component(d) => d.to_string(),
                    Some(d) => {
                        notes.push(format!(
                            "{} has an unsafe installdir {d:?}; ignored",
                            path.display()
                        ));
                        return None;
                    }
                    None => {
                        warnings.push(format!(
                            "{} has no installdir; assuming \"{DEFAULT_INSTALL_DIR}\"",
                            path.display()
                        ));
                        DEFAULT_INSTALL_DIR.to_string()
                    }
                };
                (Some(m), dir)
            }
            Err(e) => {
                notes.push(e);
                warnings.push(format!(
                    "app manifest unreadable; assuming installdir \"{DEFAULT_INSTALL_DIR}\""
                ));
                (None, DEFAULT_INSTALL_DIR.to_string())
            }
        },
        None => {
            if lib.lists_app(APP_ID) != Some(true) {
                return None;
            }
            warnings.push(format!(
                "library {} lists App ID {APP_ID} but appmanifest_{APP_ID}.acf is missing; \
                 assuming installdir \"{DEFAULT_INSTALL_DIR}\" (no build/depot information)",
                lib.path.display()
            ));
            (None, DEFAULT_INSTALL_DIR.to_string())
        }
    };

    let Some(game_root) = common
        .as_deref()
        .and_then(|c| child_dir_ci(c, &install_dir))
    else {
        notes.push(format!(
            "library {} should contain steamapps/common/{install_dir} but it is missing",
            lib.path.display()
        ));
        return None;
    };
    match detect(&game_root) {
        Ok(paths) => Some(install_from(
            paths,
            Discovery::Steam {
                steam_root: steam_root.to_path_buf(),
                library: lib.path.clone(),
                manifest_path: manifest_path.clone(),
            },
            manifest,
            warnings,
        )),
        Err(reason) => {
            notes.push(format!("{}: {reason}", game_root.display()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const MANIFEST: &str = "\"AppState\"\r\n{\r\n\t\"appid\"\t\t\"278360\"\r\n\t\"StateFlags\"\t\t\"4\"\r\n\t\"installdir\"\t\t\"A Story About My Uncle\"\r\n\t\"buildid\"\t\t\"1822049\"\r\n\t\"InstalledDepots\"\r\n\t{\r\n\t\t\"278362\"\r\n\t\t{\r\n\t\t\t\"manifest\"\t\t\"7137994883443283717\"\r\n\t\t\t\"size\"\t\t\"10\"\r\n\t\t}\r\n\t}\r\n}\r\n";

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    /// A fake Mac game folder: `<dir>/A Story About My Uncle.app/Contents/...`.
    fn fake_mac_game(dir: &Path) {
        let contents = dir.join(MAC_APP_BUNDLE).join("Contents");
        touch(&contents.join("MacOS").join("ASAMU"));
        let res = contents.join("Resources");
        touch(&res.join("ASAMU/CookedMac/Maps/ASAMUEntry.asamu"));
        touch(&res.join("ASAMU/Config/DefaultEngine.ini"));
        touch(&res.join("ASAMU/Localization/INT/ASAMU.int"));
        touch(&res.join("Engine/Config/BaseEngine.ini"));
    }

    fn opts_steam(root: &Path) -> LocateOptions {
        LocateOptions {
            steam_root: Some(root.to_path_buf()),
            host: HostOs::Other,
            ..LocateOptions::default()
        }
    }

    fn write_modern_vdf(steam: &Path, libs: &[(&Path, &[u32])]) {
        let mut text = String::from("\"libraryfolders\"\n{\n");
        for (i, (path, apps)) in libs.iter().enumerate() {
            let escaped = path.to_string_lossy().replace('\\', "\\\\");
            let _ = write!(
                text,
                "\t\"{i}\"\n\t{{\n\t\t\"path\"\t\t\"{escaped}\"\n\t\t\"apps\"\n\t\t{{\n"
            );
            for app in *apps {
                let _ = writeln!(text, "\t\t\t\"{app}\"\t\t\"0\"");
            }
            text.push_str("\t\t}\n\t}\n");
        }
        text.push_str("}\n");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(steam.join("steamapps/libraryfolders.vdf"), text).unwrap();
    }

    #[test]
    fn modern_vdf_secondary_library() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let lib = tmp.path().join("Library Two");
        write_modern_vdf(&steam, &[(&steam, &[228980]), (&lib, &[APP_ID])]);
        fs::create_dir_all(lib.join("steamapps")).unwrap();
        fs::write(
            lib.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            MANIFEST,
        )
        .unwrap();
        fake_mac_game(&lib.join("steamapps/common/A Story About My Uncle"));

        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.layout, Layout::MacApp);
        assert_eq!(install.build_id, Some(1822049));
        assert_eq!(install.depots.len(), 1);
        assert_eq!(install.depots[0].depot_id, 278362);
        assert_eq!(install.depots[0].manifest_id, Some(7137994883443283717));
        assert!(
            install
                .root
                .ends_with("steamapps/common/A Story About My Uncle")
        );
        assert!(install.cooked_dir.ends_with("Resources/ASAMU/CookedMac"));
        assert!(install.maps_dir.ends_with("CookedMac/Maps"));
        assert!(install.config_dir.ends_with("ASAMU/Config"));
        assert!(install.localization_dir.ends_with("ASAMU/Localization"));
        assert!(install.engine_dir.ends_with("Resources/Engine"));
        assert!(
            install
                .executable
                .as_ref()
                .unwrap()
                .ends_with("Contents/MacOS/ASAMU")
        );
        assert!(install.warnings.is_empty(), "{:?}", install.warnings);
        match &install.discovery {
            Discovery::Steam { library, .. } => assert_eq!(library, &lib),
            other => panic!("unexpected discovery {other:?}"),
        }
        let json = serde_json::to_string(&install).unwrap();
        assert!(json.contains("\"layout\":\"mac-app\""), "{json}");
    }

    #[test]
    fn legacy_vdf_lists_bare_paths() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let lib = tmp.path().join("Legacy Lib");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        let escaped = lib.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"LibraryFolders\"\r\n{{\r\n\t\"TimeNextStatsReport\"\t\t\"1\"\r\n\t\"ContentStatsID\"\t\t\"-2\"\r\n\t\"1\"\t\t\"{escaped}\"\r\n}}\r\n"
            ),
        )
        .unwrap();
        fs::create_dir_all(lib.join("steamapps")).unwrap();
        fs::write(
            lib.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            MANIFEST,
        )
        .unwrap();
        fake_mac_game(&lib.join("steamapps/common/A Story About My Uncle"));
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.build_id, Some(1822049));
        match &install.discovery {
            Discovery::Steam { library, .. } => assert_eq!(library, &lib),
            other => panic!("unexpected discovery {other:?}"),
        }
    }

    #[test]
    fn root_library_found_without_vdf() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(
            steam.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            MANIFEST,
        )
        .unwrap();
        fake_mac_game(&steam.join("steamapps/common/A Story About My Uncle"));
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.app_id, APP_ID);
        assert_eq!(install.build_id, Some(1822049));
    }

    #[test]
    fn windows_backslash_paths_in_vdf_are_unescaped() {
        // Library paths for another OS are parsed correctly and then skipped (they do not
        // exist here); discovery falls through to the root library.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t\t\"apps\" { \"278360\" \"1\" }\n\t}\n}\n",
        )
        .unwrap();
        let mut notes = Vec::new();
        let libs = libraries_for_root(&steam, &mut notes);
        assert!(
            libs.iter()
                .any(|l| l.path == Path::new(r"C:\Program Files (x86)\Steam"))
        );
        // The listed library comes first because it claims the app.
        assert_eq!(libs[0].path, PathBuf::from(r"C:\Program Files (x86)\Steam"));

        let err = locate_with(&opts_steam(&steam)).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, LocateError::NotInstalled { .. }), "{msg}");
        assert!(msg.contains(r"C:\Program Files (x86)\Steam"), "{msg}");
        assert!(msg.contains("no steamapps/ folder"), "{msg}");
    }

    #[test]
    fn missing_manifest_falls_back_when_library_lists_app() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        write_modern_vdf(&steam, &[(&steam, &[APP_ID])]);
        fake_mac_game(&steam.join("steamapps/common/A Story About My Uncle"));
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.build_id, None);
        assert!(install.depots.is_empty());
        assert!(install.manifest.is_none());
        assert!(
            install.warnings.iter().any(|w| w.contains("is missing")),
            "{:?}",
            install.warnings
        );
    }

    #[test]
    fn missing_manifest_and_unlisted_app_is_not_installed() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        write_modern_vdf(&steam, &[(&steam, &[228980])]);
        fake_mac_game(&steam.join("steamapps/common/A Story About My Uncle"));
        let err = locate_with(&opts_steam(&steam)).unwrap_err();
        assert!(matches!(err, LocateError::NotInstalled { .. }), "{err}");
    }

    #[test]
    fn manifest_pointing_at_missing_folder_is_reported() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps/common")).unwrap();
        fs::write(
            steam.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            MANIFEST,
        )
        .unwrap();
        let err = locate_with(&opts_steam(&steam)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("steamapps/common/A Story About My Uncle"),
            "{msg}"
        );
    }

    #[test]
    fn unsafe_installdir_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps/common")).unwrap();
        fake_mac_game(&tmp.path().join("escape"));
        fs::write(
            steam.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            "\"AppState\" { \"installdir\" \"../../escape\" }",
        )
        .unwrap();
        let err = locate_with(&opts_steam(&steam)).unwrap_err();
        assert!(err.to_string().contains("unsafe installdir"), "{err}");
        assert!(!is_safe_component(".."));
        assert!(!is_safe_component("/abs"));
        assert!(!is_safe_component(r"a\b"));
        assert!(!is_safe_component(""));
        assert!(is_safe_component("A Story About My Uncle"));
    }

    #[test]
    fn corrupt_vdf_and_manifest_do_not_panic() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            b"\"libraryfolders\" { \"0\" {",
        )
        .unwrap();
        fs::write(
            steam.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            b"\"AppState\" {\xff",
        )
        .unwrap();
        fake_mac_game(&steam.join("steamapps/common/A Story About My Uncle"));
        // The broken manifest falls back to the default install dir with a warning.
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert!(install.manifest.is_none());
        assert!(!install.warnings.is_empty());
    }

    #[test]
    fn no_steam_lists_tried_roots() {
        let tmp = TempDir::new().unwrap();
        let opts = LocateOptions {
            home: Some(tmp.path().to_path_buf()),
            host: HostOs::Linux,
            ..LocateOptions::default()
        };
        let err = locate_with(&opts).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, LocateError::NoSteam { .. }));
        assert!(
            msg.contains(".local/share/Steam") || msg.contains(r".local\share\Steam"),
            "{msg}"
        );
        assert!(msg.contains(ENV_STEAM_ROOT), "{msg}");
    }

    #[test]
    fn candidates_per_os() {
        let home = PathBuf::from("/h");
        let mac = steam_root_candidates(&LocateOptions {
            home: Some(home.clone()),
            host: HostOs::MacOs,
            ..LocateOptions::default()
        });
        assert_eq!(mac, vec![home.join("Library/Application Support/Steam")]);
        let linux = steam_root_candidates(&LocateOptions {
            home: Some(home.clone()),
            host: HostOs::Linux,
            ..LocateOptions::default()
        });
        assert!(linux.contains(&home.join(".steam/steam")));
        assert!(linux.contains(&home.join(".steam/root")));
        assert!(linux.contains(&home.join(".local/share/Steam")));
        assert!(linux.contains(&home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam")));
        let windows = steam_root_candidates(&LocateOptions {
            home: Some(home.clone()),
            program_files_x86: Some(PathBuf::from("X86")),
            program_files: Some(PathBuf::from("PF")),
            host: HostOs::Windows,
            ..LocateOptions::default()
        });
        assert_eq!(windows[0], Path::new("X86").join("Steam"));
        assert_eq!(windows[1], Path::new("PF").join("Steam"));
        assert!(windows.contains(&home.join("Steam")));
        let overridden = steam_root_candidates(&LocateOptions {
            steam_root: Some(PathBuf::from("/custom")),
            home: Some(home),
            host: HostOs::Linux,
            ..LocateOptions::default()
        });
        assert_eq!(overridden, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn original_dir_override_forms() {
        let tmp = TempDir::new().unwrap();
        let game = tmp.path().join("Game Copy");
        fake_mac_game(&game);
        // Folder containing the .app.
        let a = from_original_dir(&game).unwrap();
        assert_eq!(a.layout, Layout::MacApp);
        assert_eq!(a.root, game);
        assert_eq!(a.build_id, None);
        // The .app itself: root becomes the containing folder.
        let b = locate_with(&LocateOptions {
            original_dir: Some(game.join(MAC_APP_BUNDLE)),
            steam_root: Some(tmp.path().join("ignored")),
            host: HostOs::Other,
            ..LocateOptions::default()
        })
        .unwrap();
        assert_eq!(b.root, game);
        assert_eq!(
            b.app_bundle.as_deref(),
            Some(game.join(MAC_APP_BUNDLE).as_path())
        );
        assert!(matches!(b.discovery, Discovery::OriginalDir { .. }));
    }

    #[test]
    fn original_dir_inside_steam_library_reads_manifest() {
        let tmp = TempDir::new().unwrap();
        let steamapps = tmp.path().join("lib/steamapps");
        let game = steamapps.join("common/A Story About My Uncle");
        fake_mac_game(&game);
        fs::write(
            steamapps.join(format!("appmanifest_{APP_ID}.acf")),
            MANIFEST,
        )
        .unwrap();
        let install = from_original_dir(&game).unwrap();
        assert_eq!(install.build_id, Some(1822049));
        assert_eq!(install.depots[0].depot_id, MAC_DEPOT_ID);
    }

    #[test]
    fn windows_style_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("win");
        touch(&root.join("Binaries/Win32/ASAMU-Win32-Shipping.exe"));
        touch(&root.join("ASAMU/CookedPC/Maps/ASAMUEntry.asamu"));
        touch(&root.join("ASAMU/Config/DefaultEngine.ini"));
        touch(&root.join("ASAMU/Localization/INT/ASAMU.int"));
        touch(&root.join("Engine/Config/BaseEngine.ini"));
        let install = from_original_dir(&root).unwrap();
        assert_eq!(install.layout, Layout::Windows);
        assert!(install.cooked_dir.ends_with("ASAMU/CookedPC"));
        assert!(
            install
                .executable
                .as_ref()
                .unwrap()
                .ends_with("Binaries/Win32/ASAMU-Win32-Shipping.exe")
        );
        assert!(install.warnings.iter().any(|w| w.contains("UNVERIFIED")));
    }

    #[test]
    fn windows_root_with_cooked_pc_console_and_case_differences() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("win");
        touch(&root.join("binaries/win64/asamu.exe"));
        touch(&root.join("asamu/cookedpcconsole/maps/x.asamu"));
        touch(&root.join("engine/x.txt"));
        let install = from_original_dir(&root).unwrap();
        assert_eq!(install.layout, Layout::Windows);
        assert!(install.cooked_dir.ends_with("cookedpcconsole"));
        assert!(install.executable.is_some());
    }

    #[test]
    fn linux_and_loose_roots() {
        let tmp = TempDir::new().unwrap();
        let linux = tmp.path().join("linux");
        touch(&linux.join("Binaries/Linux/ASAMU"));
        touch(&linux.join("ASAMU/CookedPC/x.u"));
        let install = from_original_dir(&linux).unwrap();
        assert_eq!(install.layout, Layout::Linux);

        let loose = tmp.path().join("loose");
        touch(&loose.join("Engine/x.ini"));
        touch(&loose.join("ASAMU/CookedMac/x.u"));
        let install = from_original_dir(&loose).unwrap();
        assert_eq!(install.layout, Layout::Loose);
        assert!(install.executable.is_none());
    }

    #[test]
    fn bad_original_dirs_explain_themselves() {
        let tmp = TempDir::new().unwrap();
        let missing = from_original_dir(&tmp.path().join("nope")).unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");
        let empty = from_original_dir(tmp.path()).unwrap_err();
        assert!(empty.to_string().contains(MAC_APP_BUNDLE), "{empty}");
        let only_game = tmp.path().join("only");
        touch(&only_game.join("ASAMU/CookedPC/x.u"));
        let err = from_original_dir(&only_game).unwrap_err();
        assert!(
            err.to_string().contains("neither Engine/ nor Binaries/"),
            "{err}"
        );
        let no_cooked = tmp.path().join("nocooked");
        touch(&no_cooked.join("ASAMU/Config/x.ini"));
        touch(&no_cooked.join("Engine/x.ini"));
        let err = from_original_dir(&no_cooked).unwrap_err();
        assert!(err.to_string().contains("Cooked"), "{err}");
        let file = tmp.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(
            from_original_dir(&file)
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
    }

    fn write_manifest(library: &Path, text: &str) {
        let steamapps = library.join("steamapps");
        fs::create_dir_all(&steamapps).unwrap();
        fs::write(steamapps.join(format!("appmanifest_{APP_ID}.acf")), text).unwrap();
    }

    fn steam_library(install: &Install) -> &Path {
        match &install.discovery {
            Discovery::Steam { library, .. } => library,
            other => panic!("unexpected discovery {other:?}"),
        }
    }

    #[test]
    fn stale_manifest_in_root_library_falls_through_to_next_library() {
        // Legacy vdf (no app lists), so the root library is tried first. Its manifest is stale:
        // the folder it names is gone. Discovery must carry on to the library that has the game.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let lib = tmp.path().join("Other Library");
        fs::create_dir_all(steam.join("steamapps/common")).unwrap();
        write_manifest(&steam, MANIFEST);
        let escaped = lib.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!("\"LibraryFolders\"\n{{\n\t\"1\"\t\t\"{escaped}\"\n}}\n"),
        )
        .unwrap();
        write_manifest(&lib, MANIFEST);
        fake_mac_game(&lib.join("steamapps/common/A Story About My Uncle"));

        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(steam_library(&install), lib.as_path());
        assert_eq!(install.build_id, Some(1822049));
    }

    #[test]
    fn library_that_lists_the_app_is_preferred_over_the_root() {
        // Both libraries hold a copy; the modern vdf says the second one has App 278360.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let lib = tmp.path().join("Games");
        write_modern_vdf(&steam, &[(&steam, &[228980]), (&lib, &[APP_ID])]);
        for library in [&steam, &lib] {
            write_manifest(library, MANIFEST);
            fake_mac_game(&library.join("steamapps/common/A Story About My Uncle"));
        }
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(steam_library(&install), lib.as_path());
    }

    #[test]
    fn case_differences_in_steamapps_installdir_and_bundle() {
        // Old Windows Steam wrote `SteamApps`; data copied between filesystems can change case.
        // On a case-sensitive filesystem this exercises the case-insensitive child lookup; on a
        // case-insensitive one it checks that on-disk spellings are reported.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let steamapps = steam.join("SteamApps");
        fs::create_dir_all(&steamapps).unwrap();
        fs::write(
            steamapps.join(format!("AppManifest_{APP_ID}.ACF")),
            MANIFEST.replace("\"installdir\"", "\"InstallDir\""),
        )
        .unwrap();
        let game = steamapps.join("Common").join("a story about my uncle");
        let res = game.join("a story about my uncle.app/contents/resources");
        touch(&game.join("a story about my uncle.app/contents/macos/ASAMU"));
        touch(&res.join("asamu/cookedmac/maps/x.asamu"));
        touch(&res.join("engine/x.ini"));

        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.layout, Layout::MacApp);
        assert_eq!(install.build_id, Some(1822049));
        assert!(install.root.ends_with("a story about my uncle"));
        assert!(
            install.cooked_dir.ends_with("asamu/cookedmac"),
            "{install:?}"
        );
        assert!(install.maps_dir.ends_with("cookedmac/maps"), "{install:?}");
        assert!(install.executable.is_some());
    }

    #[test]
    fn config_libraryfolders_is_used_when_steamapps_copy_is_corrupt() {
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let lib = tmp.path().join("Lib");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            "\"libraryfolders\" { \"1\" { \"path\" ",
        )
        .unwrap();
        fs::create_dir_all(steam.join("config")).unwrap();
        let escaped = lib.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            steam.join("config/libraryfolders.vdf"),
            format!("\"libraryfolders\" {{ \"1\" {{ \"path\" \"{escaped}\" }} }}"),
        )
        .unwrap();
        write_manifest(&lib, MANIFEST);
        fake_mac_game(&lib.join("steamapps/common/A Story About My Uncle"));

        let mut notes = Vec::new();
        let libs = libraries_for_root(&steam, &mut notes);
        assert!(libs.iter().any(|l| l.path == lib), "{libs:?}");
        assert!(
            notes.iter().any(|n| n.contains("cannot use")),
            "corrupt steamapps copy must be noted: {notes:?}"
        );
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(steam_library(&install), lib.as_path());
    }

    #[test]
    fn manifest_is_used_even_when_the_app_list_omits_the_app() {
        // App lists in libraryfolders.vdf lag behind installs; the manifest is authoritative.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        write_modern_vdf(&steam, &[(&steam, &[228980])]);
        write_manifest(&steam, MANIFEST);
        fake_mac_game(&steam.join("steamapps/common/A Story About My Uncle"));
        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(install.build_id, Some(1822049));
    }

    #[test]
    fn installdir_variants_are_validated() {
        for bad in [".", "..", "a/b", r"a\b", "/", "a\0b", ""] {
            assert!(!is_safe_component(bad), "{bad:?} must be rejected");
        }
        for good in ["A Story About My Uncle", "ASAMU", "...", "Game (Beta)"] {
            assert!(is_safe_component(good), "{good:?} must be accepted");
        }
        // A Windows drive-relative name is never a plain component there.
        if cfg!(windows) {
            assert!(!is_safe_component("C:"));
        }
    }

    #[test]
    fn bundle_root_is_the_parent_or_the_current_directory() {
        assert_eq!(
            bundle_root(Path::new("x").join(MAC_APP_BUNDLE).as_path()),
            PathBuf::from("x")
        );
        assert_eq!(bundle_root(Path::new(MAC_APP_BUNDLE)), PathBuf::from("."));
    }

    #[test]
    fn nul_and_foreign_library_paths_do_not_panic() {
        // No UNC paths here: on Windows CI a `\\server\share` lookup can block on SMB timeouts.
        // UNC parsing is covered by the pure parser tests in `steam.rs`.
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            "\"libraryfolders\" { \"1\" { \"path\" \"a\u{0}b\" \"apps\" { \"278360\" \"1\" } } \
             \"2\" \"Z:\\\\Missing\\\\Steam\" \"3\" { \"path\" \"relative/lib\" } }",
        )
        .unwrap();
        let err = locate_with(&opts_steam(&steam)).unwrap_err();
        assert!(matches!(err, LocateError::NotInstalled { .. }), "{err}");
        let mut notes = Vec::new();
        let libs = libraries_for_root(&steam, &mut notes);
        assert!(
            libs.iter()
                .any(|l| l.path == Path::new(r"Z:\Missing\Steam")),
            "{libs:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_library_and_game_folder() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let steam = tmp.path().join("Steam");
        let real_lib = tmp.path().join("real-lib");
        let lib_link = tmp.path().join("lib-link");
        let real_game = tmp.path().join("moved-game");
        fake_mac_game(&real_game);
        fs::create_dir_all(real_lib.join("steamapps/common")).unwrap();
        write_manifest(&real_lib, MANIFEST);
        symlink(
            &real_game,
            real_lib.join("steamapps/common/A Story About My Uncle"),
        )
        .unwrap();
        symlink(&real_lib, &lib_link).unwrap();
        write_modern_vdf(&steam, &[(&steam, &[]), (&lib_link, &[APP_ID])]);

        let install = locate_with(&opts_steam(&steam)).unwrap();
        assert_eq!(steam_library(&install), lib_link.as_path());
        assert_eq!(install.layout, Layout::MacApp);
        // Paths keep the spelling discovery used (through the symlinks), not the target.
        assert!(install.root.starts_with(&lib_link), "{:?}", install.root);
        assert!(install.cooked_dir.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_steam_roots_are_deduplicated_and_root_entry_matched() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let real = home.join(".local/share/Steam");
        fs::create_dir_all(real.join("steamapps")).unwrap();
        fs::create_dir_all(home.join(".steam")).unwrap();
        symlink(&real, home.join(".steam/steam")).unwrap();
        symlink(&real, home.join(".steam/root")).unwrap();
        let candidates = steam_root_candidates(&LocateOptions {
            home: Some(home.clone()),
            host: HostOs::Linux,
            ..LocateOptions::default()
        });
        assert_eq!(
            candidates
                .iter()
                .filter(|c| fs::canonicalize(c).ok() == fs::canonicalize(&real).ok())
                .count(),
            1,
            "{candidates:?}"
        );
        assert_eq!(candidates[0], home.join(".steam/steam"));

        // libraryfolders.vdf names the canonical path; discovery reached it via the symlink.
        write_modern_vdf(&real, &[(&real, &[APP_ID])]);
        let mut notes = Vec::new();
        let libs = libraries_for_root(&home.join(".steam/steam"), &mut notes);
        assert_eq!(libs.len(), 1, "{libs:?}");
        assert_eq!(libs[0].lists_app(APP_ID), Some(true));
    }

    /// Real-data check: runs only when the game is installed (CI has no game data).
    #[test]
    fn real_install_if_present() {
        let opts = LocateOptions::from_env();
        let install = match locate_with(&opts) {
            Ok(install) => install,
            Err(e) => {
                eprintln!("SKIP real_install_if_present: original install not found ({e})");
                return;
            }
        };
        assert_eq!(install.app_id, APP_ID);
        assert!(install.cooked_dir.is_dir());
        assert!(install.game_dir.is_dir());
        if install.layout == Layout::MacApp {
            assert!(install.executable.as_ref().is_some_and(|p| p.is_file()));
        }
        // Build/depot fields exist only when a Steam manifest was read. The expected values
        // describe the Mac depot inventoried in docs/reverse-engineering/INVENTORY.md; a
        // Windows/Linux install has other depots, and a newer build needs re-inventorying
        // (reported, like the inventory's real-data test, instead of failing).
        const INVENTORIED_BUILD: u64 = 1822049;
        if install.manifest.is_none() {
            eprintln!("NOTE real_install_if_present: no app manifest; build/depot not checked");
        } else if install.layout != Layout::MacApp {
            eprintln!(
                "NOTE real_install_if_present: {} layout, build {:?}, depots {:?} (only the Mac \
                 depot {MAC_DEPOT_ID} has been inventoried)",
                install.layout.label(),
                install.build_id,
                install.depots
            );
        } else if install.build_id != Some(INVENTORIED_BUILD) {
            eprintln!(
                "NOTE real_install_if_present: build {:?} is not the inventoried build \
                 {INVENTORIED_BUILD}; re-run asamu-inventory",
                install.build_id
            );
        } else {
            assert!(
                install.depots.iter().any(|d| d.depot_id == MAC_DEPOT_ID),
                "expected depot {MAC_DEPOT_ID}: {:?}",
                install.depots
            );
        }
    }
}
