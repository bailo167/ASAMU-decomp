//! Sanitized metadata inventory of an original *A Story About My Uncle* install.
//!
//! For every file under the install root: relative path (forward slashes), size, lowercase
//! extension, streaming SHA-256, sniffed type ([`sniff`]) and category ([`category`]).
//! Output is deterministic (sorted by path, no timestamps, no absolute paths), so a
//! committed inventory can be diffed against a fresh run.
//!
//! The install is only ever read. Hashing streams each file in 1 MiB blocks.

pub mod category;
pub mod sniff;
pub mod toc;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::category::{Category, categorize, is_toc_name};
use crate::sniff::{FileType, SNIFF_BYTES, sniff};
use crate::toc::TocReport;

/// JSON schema identifier written into every inventory.
pub const SCHEMA: &str = "asamu-inventory/1";

/// Read block size for hashing.
const HASH_BLOCK: usize = 1024 * 1024;

/// Largest TOC file that will be read for the cross-check.
const MAX_TOC_BYTES: u64 = 16 * 1024 * 1024;

/// One inventoried file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    /// Path relative to the install root, `/`-separated.
    pub path: String,
    /// Size in bytes (bytes actually hashed).
    pub size: u64,
    /// Lowercase extension, absent when the name has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
    /// Lowercase hex SHA-256 (absent for symlinks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Sniffed type.
    #[serde(rename = "type")]
    pub kind: FileType,
    /// Category from the documented rules.
    pub category: Category,
    /// UE3 package file version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ue3_version: Option<u16>,
    /// UE3 package licensee version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ue3_licensee: Option<u16>,
    /// Short sniffing detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One installed Steam depot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepotInfo {
    /// Depot ID.
    pub depot_id: u32,
    /// Manifest GID as a decimal string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_id: Option<String>,
}

/// Steam metadata for the inventoried install (from the app manifest; no account data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SteamInfo {
    /// App ID.
    pub app_id: u32,
    /// Build ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<u64>,
    /// Installed depots.
    pub depots: Vec<DepotInfo>,
}

/// A complete inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    /// Name of the install root folder (never an absolute path).
    pub root_name: String,
    /// Detected layout label, when known.
    pub layout: Option<String>,
    /// Steam metadata, when an app manifest was read.
    pub steam: Option<SteamInfo>,
    /// Files sorted by path.
    pub files: Vec<FileEntry>,
    /// Cooker TOC cross-checks, sorted by TOC path.
    pub tocs: Vec<TocReport>,
}

/// Scan options.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Abort when more than this many files are found (protects against pointing the tool
    /// at a huge unrelated folder).
    pub max_files: usize,
    /// Cross-check cooker TOC files.
    pub toc: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            max_files: 50_000,
            toc: true,
        }
    }
}

/// Lowercase extension of a file name (`None` for `PkgInfo`, `.DS_Store`, ...).
pub fn extension_of(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
}

/// `/`-separated path of `path` relative to `root`.
pub fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is not under the root", path.display()))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            other => bail!("unexpected path component {other:?} in {}", rel.display()),
        }
    }
    Ok(parts.join("/"))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Stream a file: returns (bytes read, SHA-256 hex, first [`SNIFF_BYTES`] bytes).
pub fn hash_file(path: &Path) -> Result<(u64, String, Vec<u8>)> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_BLOCK];
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    let mut total: u64 = 0;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let chunk = buf.get(..n).unwrap_or_default();
        hasher.update(chunk);
        if head.len() < SNIFF_BYTES {
            let take = (SNIFF_BYTES - head.len()).min(chunk.len());
            head.extend_from_slice(chunk.get(..take).unwrap_or_default());
        }
        total = total
            .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
            .context("file size overflow")?;
    }
    Ok((total, hex(hasher.finalize().as_slice()), head))
}

/// Inventory everything under `root`.
pub fn scan(root: &Path, opts: &ScanOptions) -> Result<Inventory> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .min_depth(1)
    {
        let entry = entry.with_context(|| format!("walking {}", root.display()))?;
        let file_type = entry.file_type();
        if file_type.is_dir() {
            continue;
        }
        if files.len() >= opts.max_files {
            bail!(
                "more than {} files under {}; is this really the game folder? (raise --max-files to continue)",
                opts.max_files,
                root.display()
            );
        }
        let rel = relative_path(root, entry.path())?;
        let ext = extension_of(&entry.file_name().to_string_lossy());
        if file_type.is_symlink() {
            let target = fs::read_link(entry.path())
                .map(|t| symlink_target_label(&t))
                .unwrap_or_else(|_| "(unreadable)".to_string());
            files.push(FileEntry {
                category: categorize(&rel, ext.as_deref(), FileType::Symlink),
                path: rel,
                size: 0,
                ext,
                sha256: None,
                kind: FileType::Symlink,
                ue3_version: None,
                ue3_licensee: None,
                detail: Some(format!("-> {target}")),
            });
            continue;
        }
        let (size, sha256, head) = hash_file(entry.path())?;
        let sniffed = sniff(&head, size, ext.as_deref());
        files.push(FileEntry {
            category: categorize(&rel, ext.as_deref(), sniffed.kind),
            path: rel,
            size,
            ext,
            sha256: Some(sha256),
            kind: sniffed.kind,
            ue3_version: sniffed.ue3_version,
            ue3_licensee: sniffed.ue3_licensee,
            detail: sniffed.detail,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut tocs = Vec::new();
    if opts.toc {
        let pairs: Vec<(&str, u64)> = files.iter().map(|f| (f.path.as_str(), f.size)).collect();
        for file in &files {
            let name = file.path.rsplit('/').next().unwrap_or_default();
            if file.kind == FileType::Symlink || !is_toc_name(name) || file.size > MAX_TOC_BYTES {
                continue;
            }
            let full = root.join(&file.path);
            let bytes = fs::read(&full).with_context(|| format!("read {}", full.display()))?;
            let parsed = toc::parse_toc(&toc::decode_text(&bytes));
            tocs.push(toc::cross_check(&file.path, &parsed, &pairs));
        }
    }

    Ok(Inventory {
        root_name: root_name(root),
        layout: None,
        steam: None,
        files,
        tocs,
    })
}

/// Name of the root folder only. `.` or `x/..` have no file name, so the canonical path is
/// asked for its last component; the absolute path itself is never returned.
fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            fs::canonicalize(root)
                .ok()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// Symlink target as recorded in the (committed) inventory. Relative targets are kept with
/// `/` separators; absolute targets would leak local paths (user names), so only their
/// final component is kept.
pub fn symlink_target_label(target: &Path) -> String {
    let is_absolute = target.is_absolute() || target.has_root();
    let parts: Vec<String> = target
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir => Some(".".to_string()),
            Component::RootDir | Component::Prefix(_) => None,
        })
        .collect();
    if is_absolute {
        let last = parts.last().map_or("", String::as_str);
        format!("(absolute)/{last}")
    } else {
        parts.join("/")
    }
}

/// File count and bytes for one group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupTotal {
    /// Group key (category, extension or type).
    pub key: String,
    /// Number of files.
    pub files: usize,
    /// Total bytes.
    pub bytes: u64,
}

/// Group files by `key`, sorted by bytes (descending) then key.
pub fn totals_by(files: &[FileEntry], key: impl Fn(&FileEntry) -> String) -> Vec<GroupTotal> {
    let mut map: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for f in files {
        let slot = map.entry(key(f)).or_insert((0, 0));
        slot.0 = slot.0.saturating_add(1);
        slot.1 = slot.1.saturating_add(f.size);
    }
    let mut out: Vec<GroupTotal> = map
        .into_iter()
        .map(|(key, (files, bytes))| GroupTotal { key, files, bytes })
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
    out
}

fn ext_key(f: &FileEntry) -> String {
    f.ext.clone().unwrap_or_else(|| "(none)".to_string())
}

impl Inventory {
    /// Total bytes.
    pub fn total_bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size))
    }
}

#[derive(Serialize)]
struct Totals {
    files: usize,
    bytes: u64,
}

fn push_json_array<T: Serialize>(
    out: &mut String,
    key: &str,
    items: &[T],
    last: bool,
) -> Result<()> {
    let _ = write!(out, "  \"{key}\": [");
    if items.is_empty() {
        out.push(']');
    } else {
        out.push('\n');
        for (i, item) in items.iter().enumerate() {
            out.push_str("    ");
            out.push_str(&serde_json::to_string(item)?);
            if i + 1 < items.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]");
    }
    out.push_str(if last { "\n" } else { ",\n" });
    Ok(())
}

fn push_json_field<T: Serialize>(out: &mut String, key: &str, value: &T) -> Result<()> {
    let _ = writeln!(out, "  \"{key}\": {},", serde_json::to_string(value)?);
    Ok(())
}

/// Deterministic JSON: one object per line inside arrays, so diffs stay readable.
pub fn to_json(inv: &Inventory) -> Result<String> {
    let mut out = String::from("{\n");
    push_json_field(&mut out, "schema", &SCHEMA)?;
    push_json_field(
        &mut out,
        "generator",
        &format!("asamu-inventory {}", env!("CARGO_PKG_VERSION")),
    )?;
    push_json_field(&mut out, "root_name", &inv.root_name)?;
    push_json_field(&mut out, "layout", &inv.layout)?;
    push_json_field(&mut out, "steam", &inv.steam)?;
    push_json_field(
        &mut out,
        "totals",
        &Totals {
            files: inv.files.len(),
            bytes: inv.total_bytes(),
        },
    )?;
    push_json_array(
        &mut out,
        "categories",
        &totals_by(&inv.files, |f| f.category.as_str().to_string()),
        false,
    )?;
    push_json_array(
        &mut out,
        "extensions",
        &totals_by(&inv.files, ext_key),
        false,
    )?;
    push_json_array(
        &mut out,
        "types",
        &totals_by(&inv.files, |f| f.kind.as_str().to_string()),
        false,
    )?;
    push_json_array(&mut out, "tocs", &inv.tocs, false)?;
    push_json_array(&mut out, "files", &inv.files, true)?;
    out.push_str("}\n");
    Ok(out)
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Longest common `/`-terminated directory prefix of `paths`.
fn common_dir_prefix<'a>(paths: impl Iterator<Item = &'a str>) -> String {
    let mut prefix: Option<Vec<&str>> = None;
    for path in paths {
        let dirs: Vec<&str> = match path.rsplit_once('/') {
            Some((dir, _)) => dir.split('/').collect(),
            None => Vec::new(),
        };
        prefix = Some(match prefix {
            None => dirs,
            Some(p) => p
                .iter()
                .zip(dirs.iter())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| *a)
                .collect(),
        });
    }
    match prefix {
        Some(p) if !p.is_empty() => format!("{}/", p.join("/")),
        _ => String::new(),
    }
}

fn group_table(out: &mut String, title: &str, key_name: &str, rows: &[GroupTotal]) {
    let _ = writeln!(out, "## {title}\n");
    let _ = writeln!(out, "| {key_name} | Files | Bytes |");
    let _ = writeln!(out, "|---|---:|---:|");
    for row in rows {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} |",
            row.key,
            row.files,
            thousands(row.bytes)
        );
    }
    out.push('\n');
}

/// Markdown summary: totals, per category / extension / type, UE3 packages, executables and
/// TOC cross-checks.
pub fn to_markdown(inv: &Inventory) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Inventory summary: {}\n", inv.root_name);
    let _ = writeln!(
        out,
        "Generated by `asamu-inventory {}` (schema `{SCHEMA}`).\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(layout) = &inv.layout {
        let _ = writeln!(out, "- Layout: `{layout}`");
    }
    if let Some(steam) = &inv.steam {
        let _ = writeln!(out, "- Steam App ID: {}", steam.app_id);
        if let Some(build) = steam.build_id {
            let _ = writeln!(out, "- Build ID: {build}");
        }
        for depot in &steam.depots {
            let _ = writeln!(
                out,
                "- Depot: {} (manifest {})",
                depot.depot_id,
                depot.manifest_id.as_deref().unwrap_or("unknown")
            );
        }
    }
    let _ = writeln!(
        out,
        "- Files: {}",
        thousands(u64::try_from(inv.files.len()).unwrap_or(u64::MAX))
    );
    let _ = writeln!(out, "- Bytes: {}\n", thousands(inv.total_bytes()));

    group_table(
        &mut out,
        "Per category",
        "Category",
        &totals_by(&inv.files, |f| f.category.as_str().to_string()),
    );
    group_table(
        &mut out,
        "Per extension",
        "Extension",
        &totals_by(&inv.files, ext_key),
    );
    group_table(
        &mut out,
        "Per detected type",
        "Type",
        &totals_by(&inv.files, |f| f.kind.as_str().to_string()),
    );

    let packages: Vec<&FileEntry> = inv
        .files
        .iter()
        .filter(|f| matches!(f.kind, FileType::Ue3Package | FileType::Ue3PackageBigEndian))
        .collect();
    let prefix = common_dir_prefix(packages.iter().map(|f| f.path.as_str()));
    let _ = writeln!(out, "## UE3 packages ({})\n", packages.len());
    if !prefix.is_empty() {
        let _ = writeln!(out, "Paths relative to `{prefix}`.\n");
    }
    let _ = writeln!(
        out,
        "| Package | Category | Bytes | Version | Licensee | SHA-256 |"
    );
    let _ = writeln!(out, "|---|---|---:|---:|---:|---|");
    for f in &packages {
        let opt = |v: Option<u16>| v.map_or_else(|| "?".to_string(), |v| v.to_string());
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | `{}` |",
            f.path.strip_prefix(&prefix).unwrap_or(&f.path),
            f.category.as_str(),
            thousands(f.size),
            opt(f.ue3_version),
            opt(f.ue3_licensee),
            f.sha256.as_deref().unwrap_or("")
        );
    }
    out.push('\n');

    let detailed: Vec<&FileEntry> = inv
        .files
        .iter()
        .filter(|f| {
            f.category == Category::Executable
                || matches!(
                    f.kind,
                    FileType::Ue3CompressedChunks | FileType::Ue3GlobalShaderCache
                )
        })
        .collect();
    let _ = writeln!(
        out,
        "## Executables, libraries, texture caches and global shader caches\n"
    );
    let _ = writeln!(out, "| Path | Bytes | Type | Detail | SHA-256 |");
    let _ = writeln!(out, "|---|---:|---|---|---|");
    for f in &detailed {
        let _ = writeln!(
            out,
            "| `{}` | {} | `{}` | {} | `{}` |",
            f.path,
            thousands(f.size),
            f.kind.as_str(),
            f.detail.as_deref().unwrap_or(""),
            f.sha256.as_deref().unwrap_or("")
        );
    }
    out.push('\n');

    if !inv.tocs.is_empty() {
        toc_section(&mut out, &inv.tocs);
    }
    out
}

fn toc_file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A TOC without a `_<LANG>` suffix (`PCTOC.txt`) is "primary"; language TOCs are shown as
/// differences from the primary TOC in the same folder.
fn is_primary_toc(path: &str) -> bool {
    !toc_file_name(path).contains('_')
}

fn dir_table(out: &mut String, heading: &str, value_name: &str, rows: &[toc::DirCount]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "{heading}\n");
    let _ = writeln!(out, "| Folder | Files | {value_name} |");
    let _ = writeln!(out, "|---|---:|---:|");
    for d in rows {
        let dir = if d.dir.is_empty() { "." } else { &d.dir };
        let _ = writeln!(out, "| `{dir}` | {} | {} |", d.files, thousands(d.bytes));
    }
    out.push('\n');
}

fn toc_section(out: &mut String, tocs: &[TocReport]) {
    let _ = writeln!(out, "## Cooker TOC cross-check\n");
    let _ = writeln!(
        out,
        "TOC paths (`..\\X`) are resolved from `<TOC folder>/../Binaries`; `remapped` means found only after \
         mapping `CookedPC`→`Cooked*` and `PC`→platform folders. CRC columns are counted, not checked.\n"
    );
    let _ = writeln!(
        out,
        "| TOC | Entries | Found exact | Found remapped | Size match | Size differ | Missing | Unlisted on disk | Non-zero CRC | Malformed |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for t in tocs {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            toc_file_name(&t.path),
            t.entries,
            t.found_exact,
            t.found_remapped,
            t.size_matches,
            t.size_mismatches.len(),
            t.missing,
            t.unlisted_files,
            t.nonzero_crc,
            t.malformed_lines
        );
    }
    out.push('\n');

    for t in tocs.iter().filter(|t| is_primary_toc(&t.path)) {
        let _ = writeln!(out, "### `{}`\n", t.path);
        let _ = writeln!(out, "Base folder: `{}`.\n", t.base);
        dir_table(
            out,
            "Listed in the TOC but missing on disk:",
            "TOC bytes",
            &t.missing_by_dir,
        );
        if !t.size_mismatches.is_empty() {
            let _ = writeln!(out, "Size differs from the TOC:\n");
            let _ = writeln!(
                out,
                "| Path (relative to base) | TOC bytes | Disk bytes | Difference |"
            );
            let _ = writeln!(out, "|---|---:|---:|---:|");
            for m in &t.size_mismatches {
                let rel = m
                    .path
                    .strip_prefix(&t.base)
                    .and_then(|r| r.strip_prefix('/'))
                    .unwrap_or(&m.path);
                let diff = i128::from(m.disk_size) - i128::from(m.toc_size);
                let _ = writeln!(
                    out,
                    "| `{rel}` | {} | {} | {diff:+} |",
                    thousands(m.toc_size),
                    thousands(m.disk_size)
                );
            }
            out.push('\n');
        }
        dir_table(
            out,
            "On disk under the base folder but not listed in the TOC:",
            "Disk bytes",
            &t.unlisted_by_dir,
        );
    }

    let mut notes = Vec::new();
    for t in tocs.iter().filter(|t| !is_primary_toc(&t.path)) {
        let folder = t.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let primary = tocs.iter().find(|p| {
            is_primary_toc(&p.path) && p.path.rsplit_once('/').map_or("", |(dir, _)| dir) == folder
        });
        let Some(primary) = primary else {
            continue;
        };
        let extra_missing: Vec<String> = t
            .missing_by_dir
            .iter()
            .filter(|d| !primary.missing_by_dir.iter().any(|p| p.dir == d.dir))
            .map(|d| {
                format!(
                    "`{}` ({} files, {} bytes)",
                    d.dir,
                    d.files,
                    thousands(d.bytes)
                )
            })
            .collect();
        let extra_mismatch: Vec<String> = t
            .size_mismatches
            .iter()
            .filter(|m| !primary.size_mismatches.iter().any(|p| p.path == m.path))
            .map(|m| format!("`{}`", toc_file_name(&m.path)))
            .collect();
        if extra_missing.is_empty() && extra_mismatch.is_empty() {
            continue;
        }
        let mut parts = Vec::new();
        if !extra_missing.is_empty() {
            parts.push(format!("also missing {}", extra_missing.join(", ")));
        }
        if !extra_mismatch.is_empty() {
            parts.push(format!(
                "also size-mismatched {}",
                extra_mismatch.join(", ")
            ));
        }
        notes.push(format!(
            "- `{}` vs `{}`: {}",
            toc_file_name(&t.path),
            toc_file_name(&primary.path),
            parts.join("; ")
        ));
    }
    if !notes.is_empty() {
        let _ = writeln!(out, "### Language TOCs: differences from the primary TOC\n");
        for note in notes {
            let _ = writeln!(out, "{note}");
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn ue3(version: u16, licensee: u16, len: usize) -> Vec<u8> {
        let mut v = sniff::UE3_TAG_LE.to_vec();
        v.extend_from_slice(&version.to_le_bytes());
        v.extend_from_slice(&licensee.to_le_bytes());
        v.resize(len, 0xAB);
        v
    }

    /// A synthetic Mac-style tree written byte by byte (no original data).
    fn synthetic_tree() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path().join("Game");
        let res = "G.app/Contents/Resources";
        let mut macho = vec![0xCF, 0xFA, 0xED, 0xFE];
        macho.extend_from_slice(&0x0100_0007u32.to_le_bytes());
        macho.extend_from_slice(&3u32.to_le_bytes());
        macho.extend_from_slice(&2u32.to_le_bytes());
        write(&r, "G.app/Contents/MacOS/ASAMU", &macho);
        write(&r, "G.app/Contents/PkgInfo", b"APPL????\n");
        write(
            &r,
            &format!("{res}/ASAMU/CookedMac/Core.u"),
            &ue3(868, 0, 64),
        );
        write(
            &r,
            &format!("{res}/ASAMU/CookedMac/Maps/Entry.asamu"),
            &ue3(868, 0, 40),
        );
        let mut tfc = sniff::UE3_TAG_LE.to_vec();
        tfc.extend_from_slice(&0x0002_0000u32.to_le_bytes());
        tfc.extend_from_slice(&[10, 0, 0, 0, 20, 0, 0, 0]);
        write(&r, &format!("{res}/ASAMU/CookedMac/Textures.tfc"), &tfc);
        write(
            &r,
            &format!("{res}/ASAMU/CookedMac/GlobalShaderCache-PC-OpenGL.bin"),
            b"BMSGd\x03\0\0",
        );
        write(
            &r,
            &format!("{res}/ASAMU/Config/DefaultEngine.ini"),
            b"[URL]\r\nMapExt=asamu\r\n",
        );
        write(
            &r,
            &format!("{res}/ASAMU/Localization/INT/ASAMU.int"),
            &[0xFF, 0xFE, b'[', 0],
        );
        write(
            &r,
            &format!("{res}/ASAMU/PCTOC.txt"),
            b"64 0 ..\\ASAMU\\CookedPC\\Core.u 0\r\n99 0 ..\\Binaries\\Win32\\ASAMU-Win32-Shipping.exe 0\r\n",
        );
        write(
            &r,
            &format!("{res}/Engine/EditorResources/wxRes/a.bmp"),
            b"BM\x10\0\0\0\0\0\0\0\x0a\0\0\0\0\0",
        );
        write(
            &r,
            &format!("{res}/Engine/Shaders/Binaries/X.bin"),
            &[1, 0, 0, 0, 0xC7, 0x85],
        );
        write(&r, "G.app/Contents/empty.dat", b"");
        tmp
    }

    #[test]
    fn scan_synthetic_tree() {
        let tmp = synthetic_tree();
        let root = tmp.path().join("Game");
        let inv = scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(inv.root_name, "Game");
        assert_eq!(inv.files.len(), 12);
        let paths: Vec<&str> = inv.files.iter().map(|f| f.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "files must be sorted by path");
        assert!(
            paths
                .iter()
                .all(|p| !p.contains('\\') && !p.starts_with('/'))
        );

        let get = |suffix: &str| inv.files.iter().find(|f| f.path.ends_with(suffix)).unwrap();
        let core = get("Core.u");
        assert_eq!(core.kind, FileType::Ue3Package);
        assert_eq!((core.ue3_version, core.ue3_licensee), (Some(868), Some(0)));
        assert_eq!(core.category, Category::Ue3Package);
        assert_eq!(get("Entry.asamu").category, Category::Map);
        assert_eq!(get("Textures.tfc").kind, FileType::Ue3CompressedChunks);
        assert_eq!(get("Textures.tfc").category, Category::TextureRelated);
        assert_eq!(get("OpenGL.bin").category, Category::Shader);
        assert_eq!(get("MacOS/ASAMU").category, Category::Executable);
        assert_eq!(get("MacOS/ASAMU").ext, None);
        assert_eq!(get("PkgInfo").category, Category::Metadata);
        assert_eq!(get("ASAMU.int").kind, FileType::Utf16LeText);
        assert_eq!(get("a.bmp").category, Category::EditorResource);
        assert_eq!(get("X.bin").detail.as_deref(), Some("magic=01000000"));
        let empty = get("empty.dat");
        assert_eq!(empty.kind, FileType::Empty);
        // SHA-256 of the empty string.
        assert_eq!(
            empty.sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );

        assert_eq!(inv.tocs.len(), 1);
        let toc = &inv.tocs[0];
        assert_eq!(toc.entries, 2);
        assert_eq!(toc.found_remapped, 1);
        assert_eq!(toc.size_matches, 1);
        assert_eq!(toc.missing, 1);
        assert_eq!(toc.missing_by_dir[0].dir, "Binaries/Win32");
    }

    #[test]
    fn hashing_streams_across_blocks() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("big.bin");
        // Larger than one hash block, with a known digest computed independently of the
        // block size: hash the same bytes in one call.
        let data: Vec<u8> = (0..(HASH_BLOCK * 2 + 123))
            .map(|i| (i % 251) as u8)
            .collect();
        fs::write(&path, &data).unwrap();
        let (size, digest, head) = hash_file(&path).unwrap();
        assert_eq!(size, data.len() as u64);
        assert_eq!(head.len(), SNIFF_BYTES);
        assert_eq!(&head[..], &data[..SNIFF_BYTES]);
        assert_eq!(digest, hex(Sha256::digest(&data).as_slice()));
        // Known vector: SHA-256("abc").
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            hash_file(&path).unwrap().1,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn json_is_deterministic_and_sanitized() {
        let tmp = synthetic_tree();
        let root = tmp.path().join("Game");
        let a = to_json(&scan(&root, &ScanOptions::default()).unwrap()).unwrap();
        let b = to_json(&scan(&root, &ScanOptions::default()).unwrap()).unwrap();
        assert_eq!(a, b);
        let root_text = tmp.path().to_string_lossy().into_owned();
        assert!(!a.contains(&root_text), "absolute path leaked into JSON");
        let value: serde_json::Value = serde_json::from_str(&a).unwrap();
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["totals"]["files"], 12);
        assert_eq!(value["files"].as_array().unwrap().len(), 12);
        let core = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"].as_str().unwrap().ends_with("Core.u"))
            .unwrap();
        assert_eq!(core["type"], "ue3-package");
        assert_eq!(core["ue3_version"], 868);
        assert_eq!(core["ext"], "u");
    }

    #[test]
    fn markdown_summary_mentions_tables() {
        let tmp = synthetic_tree();
        let mut inv = scan(&tmp.path().join("Game"), &ScanOptions::default()).unwrap();
        inv.layout = Some("mac-app".to_string());
        inv.steam = Some(SteamInfo {
            app_id: 278360,
            build_id: Some(1),
            depots: vec![DepotInfo {
                depot_id: 2,
                manifest_id: Some("3".to_string()),
            }],
        });
        let md = to_markdown(&inv);
        assert!(md.contains("## Per category"));
        assert!(md.contains("| `ue3-package` | 1 | 64 |"), "{md}");
        assert!(md.contains("## UE3 packages (2)"));
        assert!(md.contains("Paths relative to `G.app/Contents/Resources/ASAMU/CookedMac/`"));
        assert!(
            md.contains("| `Maps/Entry.asamu` | map | 40 | 868 | 0 |"),
            "{md}"
        );
        assert!(md.contains("- Depot: 2 (manifest 3)"));
        assert!(md.contains("Binaries/Win32"));
        assert!(!md.contains(&tmp.path().to_string_lossy().into_owned()));
    }

    #[test]
    fn language_tocs_are_reported_as_differences() {
        let report = |path: &str, missing: &[(&str, usize)]| TocReport {
            path: path.to_string(),
            base: "R".to_string(),
            entries: 3,
            malformed_lines: 0,
            found_exact: 1,
            found_remapped: 0,
            size_matches: 1,
            size_mismatches: Vec::new(),
            missing: missing.len(),
            missing_by_dir: missing
                .iter()
                .map(|(dir, files)| toc::DirCount {
                    dir: dir.to_string(),
                    files: *files,
                    bytes: 10,
                })
                .collect(),
            nonzero_uncompressed: 0,
            nonzero_crc: 0,
            unlisted_files: 0,
            unlisted_by_dir: Vec::new(),
        };
        let tocs = vec![
            report("R/G/PCTOC.txt", &[("Binaries/Win32", 2)]),
            report(
                "R/G/PCTOC_CZE.txt",
                &[("Binaries/Win32", 2), ("Engine/Localization/CZE", 1)],
            ),
            report("R/G/PCTOC_DEU.txt", &[("Binaries/Win32", 2)]),
        ];
        let mut md = String::new();
        toc_section(&mut md, &tocs);
        assert!(md.contains("### `R/G/PCTOC.txt`"), "{md}");
        assert!(!md.contains("### `R/G/PCTOC_DEU.txt`"), "{md}");
        assert!(
            md.contains("- `PCTOC_CZE.txt` vs `PCTOC.txt`: also missing `Engine/Localization/CZE` (1 files, 10 bytes)"),
            "{md}"
        );
        assert!(!md.contains("- `PCTOC_DEU.txt`"), "{md}");
    }

    #[test]
    fn max_files_guard() {
        let tmp = synthetic_tree();
        let err = scan(
            &tmp.path().join("Game"),
            &ScanOptions {
                max_files: 3,
                toc: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--max-files"), "{err}");
        assert!(scan(&tmp.path().join("missing"), &ScanOptions::default()).is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_246_349_269), "1,246,349,269");
        assert_eq!(extension_of("Core.U").as_deref(), Some("u"));
        assert_eq!(extension_of("PkgInfo"), None);
        assert_eq!(extension_of(".DS_Store"), None);
        assert_eq!(
            common_dir_prefix(["a/b/c.u", "a/b/d/e.u"].into_iter()),
            "a/b/"
        );
        assert_eq!(common_dir_prefix(["x.u", "a/y.u"].into_iter()), "");
        assert_eq!(common_dir_prefix(std::iter::empty()), "");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_recorded_not_followed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("r");
        write(&root, "a.txt", b"hello\n");
        std::os::unix::fs::symlink("a.txt", root.join("link.txt")).unwrap();
        let inv = scan(&root, &ScanOptions::default()).unwrap();
        let link = inv.files.iter().find(|f| f.path == "link.txt").unwrap();
        assert_eq!(link.kind, FileType::Symlink);
        assert_eq!(link.sha256, None);
        assert_eq!(link.detail.as_deref(), Some("-> a.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn absolute_symlink_targets_do_not_leak_local_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("r");
        write(&root, "a.txt", b"hello\n");
        let absolute = root.join("a.txt");
        std::os::unix::fs::symlink(&absolute, root.join("abs-link")).unwrap();
        std::os::unix::fs::symlink("sub/../a.txt", root.join("rel-link")).unwrap();
        let inv = scan(&root, &ScanOptions::default()).unwrap();
        let get = |p: &str| inv.files.iter().find(|f| f.path == p).unwrap();
        assert_eq!(
            get("abs-link").detail.as_deref(),
            Some("-> (absolute)/a.txt")
        );
        assert_eq!(get("rel-link").detail.as_deref(), Some("-> sub/../a.txt"));
        let json = to_json(&inv).unwrap();
        assert!(
            !json.contains(&*tmp.path().to_string_lossy()),
            "absolute path leaked: {json}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_is_followed_but_inner_links_are_not() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        write(&real, "d/x.bin", &[1, 2, 3]);
        std::os::unix::fs::symlink(real.join("d"), real.join("dir-link")).unwrap();
        let link = tmp.path().join("Game Link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let inv = scan(&link, &ScanOptions::default()).unwrap();
        assert_eq!(inv.root_name, "Game Link");
        let paths: Vec<&str> = inv.files.iter().map(|f| f.path.as_str()).collect();
        // The directory symlink is one entry; its contents are not walked twice.
        assert_eq!(paths, vec!["d/x.bin", "dir-link"]);
        assert_eq!(inv.files[1].kind, FileType::Symlink);
    }

    #[test]
    fn root_name_never_returns_an_absolute_path() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("Game"), "sub/f.txt", b"x");
        let dotted = tmp.path().join("Game").join("sub").join("..");
        let inv = scan(&dotted, &ScanOptions::default()).unwrap();
        assert_eq!(inv.root_name, "Game");
        assert_eq!(inv.files.len(), 1);
        assert_eq!(inv.files[0].path, "sub/f.txt");
        assert_eq!(symlink_target_label(Path::new("a/b")), "a/b");
        assert_eq!(symlink_target_label(Path::new("/")), "(absolute)/");
    }
}
