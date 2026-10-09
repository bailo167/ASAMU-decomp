//! UE3 cooker table-of-contents (`PCTOC.txt`) parsing and cross-checking against the
//! inventory.
//!
//! Observed line format (ASAMU Mac depot, CONFIRMED by reading the file):
//! `<size> <uncompressed size> <path> <crc>` where the path is Windows-style and relative to
//! the engine's `Binaries` folder (it starts with `..\`), and may contain spaces.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

/// One TOC line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// File size recorded by the cooker.
    pub size: u64,
    /// Uncompressed size (0 when the file is not compressed).
    pub uncompressed_size: u64,
    /// Path exactly as written (Windows separators).
    pub path: String,
    /// CRC column as written.
    pub crc: String,
}

/// Parsed TOC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedToc {
    /// Well-formed entries in file order.
    pub entries: Vec<TocEntry>,
    /// Non-empty lines that did not parse.
    pub malformed_lines: usize,
}

fn split_first_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace)?;
    Some((text.get(..end)?, text.get(end..)?))
}

/// Parse TOC text. Never panics; unparseable lines are counted.
pub fn parse_toc(text: &str) -> ParsedToc {
    let mut out = ParsedToc::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = (|| {
            let (size, rest) = split_first_word(line)?;
            let (uncompressed, rest) = split_first_word(rest)?;
            let rest = rest.trim();
            let split = rest.rfind(char::is_whitespace)?;
            let path = rest.get(..split)?.trim_end();
            let crc = rest.get(split..)?.trim();
            if path.is_empty() || crc.is_empty() {
                return None;
            }
            Some(TocEntry {
                size: size.parse().ok()?,
                uncompressed_size: uncompressed.parse().ok()?,
                path: path.to_string(),
                crc: crc.to_string(),
            })
        })();
        match parsed {
            Some(entry) => out.entries.push(entry),
            None => out.malformed_lines = out.malformed_lines.saturating_add(1),
        }
    }
    out
}

/// Decode TOC bytes: UTF-16LE when a BOM is present, else lossy UTF-8.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Resolve a TOC path against the working folder `cwd` (inventory-relative,
/// `/`-separated). Returns `None` when `..` climbs above the inventory root.
pub fn resolve(cwd: &str, toc_path: &str) -> Option<String> {
    let mut parts: Vec<&str> = cwd.split('/').filter(|c| !c.is_empty()).collect();
    for component in toc_path.split(['\\', '/']) {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// Platform-neutral lookup key: lowercase, with `Cooked*` folders mapped to `cooked*` and
/// `PC`/`Mac`/`Linux` folders mapped to `<platform>`.
pub fn platform_key(rel_path: &str) -> String {
    let lower = rel_path.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let last = parts.len().saturating_sub(1);
    for (i, part) in parts.iter_mut().enumerate() {
        if i == last {
            break;
        }
        if part.starts_with("cooked") {
            *part = "cooked*";
        } else if matches!(*part, "pc" | "mac" | "linux") {
            *part = "<platform>";
        }
    }
    parts.join("/")
}

/// A file whose size differs between TOC and disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SizeMismatch {
    /// Inventory-relative path on disk.
    pub path: String,
    /// Size recorded in the TOC.
    pub toc_size: u64,
    /// Size on disk.
    pub disk_size: u64,
}

/// File count and bytes for one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirCount {
    /// Folder relative to the TOC base.
    pub dir: String,
    /// Number of files.
    pub files: usize,
    /// Total bytes (TOC sizes for missing files, disk sizes for unlisted files).
    pub bytes: u64,
}

/// Cross-check of one TOC against the inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TocReport {
    /// Inventory-relative path of the TOC file.
    pub path: String,
    /// Inventory-relative folder the TOC's `..\` paths resolve against.
    pub base: String,
    /// Well-formed entries.
    pub entries: usize,
    /// Unparseable non-empty lines.
    pub malformed_lines: usize,
    /// Entries found on disk at exactly the TOC path (case-insensitive).
    pub found_exact: usize,
    /// Entries found only after mapping `CookedPC`→`Cooked*` / `PC`→platform folders.
    pub found_remapped: usize,
    /// Found entries whose size equals the TOC size.
    pub size_matches: usize,
    /// Found entries whose size differs.
    pub size_mismatches: Vec<SizeMismatch>,
    /// Entries not found on disk.
    pub missing: usize,
    /// Missing entries grouped by folder.
    pub missing_by_dir: Vec<DirCount>,
    /// Entries with a non-zero uncompressed size.
    pub nonzero_uncompressed: usize,
    /// Entries whose CRC column is not `0`.
    pub nonzero_crc: usize,
    /// Inventory files under `base` that no entry refers to.
    pub unlisted_files: usize,
    /// Unlisted files grouped by folder.
    pub unlisted_by_dir: Vec<DirCount>,
}

fn parent_dir(path: &str) -> &str {
    path.rfind('/').and_then(|i| path.get(..i)).unwrap_or("")
}

fn relative_to<'a>(base: &str, path: &'a str) -> &'a str {
    if base.is_empty() {
        return path;
    }
    path.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(path)
}

fn group(items: impl Iterator<Item = (String, u64)>) -> Vec<DirCount> {
    let mut map: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for (dir, bytes) in items {
        let slot = map.entry(dir).or_insert((0, 0));
        slot.0 = slot.0.saturating_add(1);
        slot.1 = slot.1.saturating_add(bytes);
    }
    map.into_iter()
        .map(|(dir, (files, bytes))| DirCount { dir, files, bytes })
        .collect()
}

/// Cross-check `toc` (located at inventory path `toc_rel_path`) against inventory files given
/// as `(path, size)` pairs.
pub fn cross_check(toc_rel_path: &str, toc: &ParsedToc, files: &[(&str, u64)]) -> TocReport {
    // UE3 resolves TOC paths from <root>/Binaries; the TOC lives in <root>/<Game>/, so the
    // base is the TOC folder's parent.
    let base = parent_dir(parent_dir(toc_rel_path)).to_string();
    let cwd = if base.is_empty() {
        "Binaries".to_string()
    } else {
        format!("{base}/Binaries")
    };

    let mut exact: HashMap<String, usize> = HashMap::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, (path, _)) in files.iter().enumerate() {
        exact.entry(path.to_ascii_lowercase()).or_insert(i);
        by_key.entry(platform_key(path)).or_insert(i);
    }

    let mut report = TocReport {
        path: toc_rel_path.to_string(),
        base: base.clone(),
        entries: toc.entries.len(),
        malformed_lines: toc.malformed_lines,
        found_exact: 0,
        found_remapped: 0,
        size_matches: 0,
        size_mismatches: Vec::new(),
        missing: 0,
        missing_by_dir: Vec::new(),
        nonzero_uncompressed: 0,
        nonzero_crc: 0,
        unlisted_files: 0,
        unlisted_by_dir: Vec::new(),
    };
    let mut referenced: HashSet<usize> = HashSet::new();
    let mut missing: Vec<(String, u64)> = Vec::new();

    for entry in &toc.entries {
        if entry.uncompressed_size != 0 {
            report.nonzero_uncompressed = report.nonzero_uncompressed.saturating_add(1);
        }
        if entry.crc != "0" {
            report.nonzero_crc = report.nonzero_crc.saturating_add(1);
        }
        let resolved = resolve(&cwd, &entry.path);
        let hit = resolved.as_ref().and_then(|r| {
            exact
                .get(&r.to_ascii_lowercase())
                .map(|&i| (i, false))
                .or_else(|| by_key.get(&platform_key(r)).map(|&i| (i, true)))
        });
        match hit {
            Some((index, remapped)) => {
                if remapped {
                    report.found_remapped = report.found_remapped.saturating_add(1);
                } else {
                    report.found_exact = report.found_exact.saturating_add(1);
                }
                referenced.insert(index);
                if let Some(&(path, disk_size)) = files.get(index) {
                    if disk_size == entry.size {
                        report.size_matches = report.size_matches.saturating_add(1);
                    } else {
                        report.size_mismatches.push(SizeMismatch {
                            path: path.to_string(),
                            toc_size: entry.size,
                            disk_size,
                        });
                    }
                }
            }
            None => {
                report.missing = report.missing.saturating_add(1);
                let dir = match &resolved {
                    Some(r) => parent_dir(relative_to(&base, r)).to_string(),
                    None => "(outside inventory root)".to_string(),
                };
                missing.push((dir, entry.size));
            }
        }
    }
    report.size_mismatches.sort_by(|a, b| a.path.cmp(&b.path));
    report.missing_by_dir = group(missing.into_iter());

    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };
    let unlisted = files.iter().enumerate().filter(|(i, (path, _))| {
        !referenced.contains(i) && path.starts_with(&prefix) && *path != toc_rel_path
    });
    let unlisted: Vec<(String, u64)> = unlisted
        .map(|(_, (path, size))| (parent_dir(relative_to(&base, path)).to_string(), *size))
        .collect();
    report.unlisted_files = unlisted.len();
    report.unlisted_by_dir = group(unlisted.into_iter());
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_with_spaces_and_crlf() {
        let text = "25920 0 ..\\Binaries\\Win32\\libogg.dll 0\r\n\
                    12 0 ..\\Engine\\EditorResources\\FaceFX\\Configs\\Generic Coarticulation\\placeholder.txt 0\r\n\
                    \r\n\
                    garbage line\r\n\
                    x 0 ..\\a 0\r\n";
        let toc = parse_toc(text);
        assert_eq!(toc.entries.len(), 2);
        assert_eq!(toc.malformed_lines, 2);
        assert_eq!(toc.entries[0].size, 25920);
        assert_eq!(
            toc.entries[1].path,
            "..\\Engine\\EditorResources\\FaceFX\\Configs\\Generic Coarticulation\\placeholder.txt"
        );
        assert_eq!(toc.entries[1].crc, "0");
    }

    #[test]
    fn decode_utf16_and_utf8() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("1 0 a 0".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_text(&utf16), "1 0 a 0");
        assert_eq!(decode_text(b"\xEF\xBB\xBF1 0 a 0"), "1 0 a 0");
        // Odd trailing byte in UTF-16 is ignored, not a panic.
        assert_eq!(decode_text(&[0xFF, 0xFE, b'a', 0, b'b']), "a");
    }

    #[test]
    fn resolve_and_keys() {
        assert_eq!(
            resolve(
                "App.app/Contents/Resources/Binaries",
                "..\\ASAMU\\CookedPC\\Core.u"
            )
            .as_deref(),
            Some("App.app/Contents/Resources/ASAMU/CookedPC/Core.u")
        );
        assert_eq!(resolve("", "..\\x"), None);
        assert_eq!(
            platform_key("R/ASAMU/CookedMac/Maps/A.asamu"),
            platform_key("R/asamu/cookedpc/maps/a.asamu")
        );
        assert_eq!(
            platform_key("R/Engine/Splash/Mac/Splash.bmp"),
            "r/engine/splash/<platform>/splash.bmp"
        );
        // The file name itself is never remapped.
        assert_eq!(platform_key("a/PC"), "a/pc");
    }

    #[test]
    fn cross_check_counts() {
        let base = "G.app/Contents/Resources";
        let files: Vec<(String, u64)> = vec![
            (format!("{base}/ASAMU/CookedMac/Core.u"), 100),
            (format!("{base}/ASAMU/CookedMac/Engine.u"), 999),
            (format!("{base}/ASAMU/Config/DefaultEngine.ini"), 50),
            (format!("{base}/ASAMU/PCTOC.txt"), 10),
            (format!("{base}/Engine/Config/Mac/MacEngine.ini"), 7),
            ("G.app/Contents/MacOS/ASAMU".to_string(), 1000),
        ];
        let refs: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let toc = parse_toc(
            "100 0 ..\\ASAMU\\CookedPC\\Core.u 0\n\
             200 0 ..\\ASAMU\\CookedPC\\Engine.u 0\n\
             50 0 ..\\ASAMU\\Config\\DefaultEngine.ini 0\n\
             300 0 ..\\Binaries\\Win32\\ASAMU-Win32-Shipping.exe 0\n\
             400 0 ..\\Binaries\\Win32\\steam_api.dll 5\n",
        );
        let report = cross_check(&format!("{base}/ASAMU/PCTOC.txt"), &toc, &refs);
        assert_eq!(report.base, base);
        assert_eq!(report.entries, 5);
        assert_eq!(report.found_exact, 1);
        assert_eq!(report.found_remapped, 2);
        assert_eq!(report.size_matches, 2);
        assert_eq!(report.size_mismatches.len(), 1);
        assert_eq!(report.size_mismatches[0].toc_size, 200);
        assert_eq!(report.missing, 2);
        assert_eq!(
            report.missing_by_dir,
            vec![DirCount {
                dir: "Binaries/Win32".to_string(),
                files: 2,
                bytes: 700
            }]
        );
        assert_eq!(report.nonzero_crc, 1);
        // MacEngine.ini is unlisted; the TOC itself and files outside the base are not counted.
        assert_eq!(report.unlisted_files, 1);
        assert_eq!(report.unlisted_by_dir[0].dir, "Engine/Config/Mac");
    }
}
