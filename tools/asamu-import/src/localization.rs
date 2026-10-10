//! `asamu-import localization`: the localized text of the user's own install
//! → user-local JSON tables, one per language.
//!
//! Sources (all read-only; evidence and confidence in
//! `docs/reverse-engineering/LOCALIZATION.md`):
//!
//! - `ASAMU/Localization/<LANG>/<Package>.<lang>` — UE3 localization files
//!   (UTF-16 with a byte-order mark, or plain ASCII). Class sections
//!   (`[ASAMUHUD]`, `[GFxASAMUMenu]`, ...) hold the localized class
//!   properties (menu labels, tutorial texts, ...); object sections
//!   (`[Group.Name SoundNodeWave]`) hold the waves' localized `Subtitles`.
//! - the class default object of `asamu.ASAMUSettingsManager`
//!   (`LanguageCodes`, the language menu's order) in `Startup.upk`;
//! - the credits movie `ASAMUFrontEndFlash.asamu_credits` (a GFx/SWF movie):
//!   only the initial text of its text fields and where they are placed, read
//!   from the tag structure (no ActionScript is decoded);
//! - optional Steam metadata on the same machine: the app manifest's
//!   `UserConfig.language` (the default language) and Steam's cached stats
//!   schema (`appcache/stats/UserGameStatsSchema_278360.bin`: achievement
//!   names and descriptions per language — they are not in the install).
//!
//! Output under `<out>/localization/` (game text: user-local only, never
//! the repository, never redistributed):
//!
//! ```text
//! manifest.json   languages, language menu order, Steam default language
//! <LANG>.json     strings ("Section.Key" -> text), subtitles (wave path -> lines),
//!                 achievements (API name -> name, description)
//! credits.json    the credits movie's text fields in reading order
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_locate::Install;
use asamu_ue3::PackageSet;
use asamu_ue3::property::Value as UeValue;
use serde::Serialize;

use crate::safety;

/// Format version of every JSON file written.
const FORMAT_VERSION: u32 = 1;

const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
                      Copyrighted game data: keep it local, never redistribute.";

/// Largest localization file read (the biggest shipped one is ~32 KB).
const MAX_LOC_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Largest Steam file read (app manifest, stats schema).
const MAX_STEAM_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Deepest binary KeyValues nesting accepted.
const MAX_KV_DEPTH: usize = 32;
/// Deepest SWF sprite nesting accepted (sprites do not nest by the spec).
const MAX_SWF_DEPTH: usize = 8;
/// Steam App ID of the game.
const APP_ID: u32 = 278_360;
/// Cooked package holding the credits movie, and the movie's object name.
const CREDITS_PACKAGE: &str = "ASAMUFrontEndFlash.upk";
const CREDITS_MOVIE: &str = "asamu_credits";
/// Two text fields of the credits whose vertical positions differ by at most
/// this many twips (1/20 px) share a row (ours: the movie places a name and
/// its role 100 twips apart, rows are 1,400 apart).
const CREDITS_ROW_TWIPS: i32 = 400;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only these languages (comma separated, e.g. INT,DEU); default: every
    /// language folder of the install.
    #[arg(long, value_delimiter = ',')]
    lang: Vec<String>,
    /// Steam stats schema to read achievement names from (default: found
    /// next to the Steam installation that holds the game).
    #[arg(long)]
    steam_stats: Option<PathBuf>,
    /// Do not read Steam metadata (default language, achievement names).
    #[arg(long)]
    no_steam: bool,
    /// Skip the credits movie.
    #[arg(long)]
    no_credits: bool,
    /// Print the counts without writing anything.
    #[arg(long)]
    dry_run: bool,
}

// ---------------------------------------------------------------------------
// Text decoding
// ---------------------------------------------------------------------------

/// How a localization file was encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TextEncoding {
    /// UTF-16 little endian with a byte-order mark.
    Utf16Le,
    /// UTF-16 big endian with a byte-order mark.
    Utf16Be,
    /// UTF-8 with a byte-order mark.
    Utf8Bom,
    /// UTF-8 (or ASCII) without a byte-order mark.
    Utf8,
    /// Not valid UTF-8: read as Latin-1 (UE3 would use the system code page).
    Latin1,
}

/// Decodes a localization file (byte-order mark decides; files without one
/// are UTF-8 when valid, else Latin-1). Unpaired surrogates become U+FFFD; a
/// trailing odd byte of UTF-16 is ignored.
pub(crate) fn decode_text(bytes: &[u8]) -> (String, TextEncoding) {
    let utf16 = |data: &[u8], le: bool| -> String {
        let (pairs, _odd) = data.as_chunks::<2>();
        let units = pairs.iter().map(|&pair| {
            if le {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (utf16(rest, true), TextEncoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (utf16(rest, false), TextEncoding::Utf16Be);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (
            String::from_utf8_lossy(rest).into_owned(),
            TextEncoding::Utf8Bom,
        );
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), TextEncoding::Utf8),
        Err(_) => (
            bytes.iter().map(|&b| char::from(b)).collect(),
            TextEncoding::Latin1,
        ),
    }
}

// ---------------------------------------------------------------------------
// UE3 ini files
// ---------------------------------------------------------------------------

/// One section; entries in file order (duplicates kept; a lookup takes the
/// last one — STRONG: the shipped package data resolves a duplicated key
/// that way, LOCALIZATION.md §2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IniSection {
    /// The text between the brackets.
    pub name: String,
    /// `(key, value)` with the value as the config reader stores it.
    pub entries: Vec<(String, String)>,
}

impl IniSection {
    /// The last value of `key` (ASCII case-insensitive, like UE3 names).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// A parsed localization file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IniFile {
    /// Sections in first-appearance order; a repeated header continues its
    /// section.
    pub sections: Vec<IniSection>,
    /// Non-empty, non-comment lines that are neither a header nor a
    /// `key=value` inside a section (they are ignored).
    pub ignored_lines: usize,
    /// Backslash escapes in quoted values other than `\\`, `\"` and `\n`
    /// (kept as written; none in the shipped files).
    pub unknown_escapes: usize,
}

/// The blanks the game's config reader trims: space and tab.
fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Parses UE3 ini text the way the game's own reader does
/// (`FConfigFile::ProcessInputFileContents`, read natively in this
/// executable; LOCALIZATION.md §2):
///
/// - lines end at a line feed or a carriage return; trailing blanks (space,
///   tab) are dropped;
/// - a header is a line whose first character is `[` and whose last is `]`
///   (no leading blanks; anything else is not a header, so its following
///   keys stay in the previous section); sections of the same name (ASCII
///   case-insensitive) are one section;
/// - lines before the first header, lines whose first character is `;` and
///   lines without `=` are ignored;
/// - other lines split at the first `=`, blanks around key and value
///   trimmed; the value goes through [`config_value`].
///
/// Not implemented (no shipped file needs it): a line ending in two
/// backslashes continues on the next line.
pub(crate) fn parse_ini(text: &str) -> IniFile {
    let mut file = IniFile::default();
    // Lower-case section name → index (a repeated header continues its section).
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut current: Option<usize> = None;
    for raw in text.split(['\n', '\r']) {
        let line = raw.trim_end_matches(is_blank);
        if line.is_empty() {
            continue;
        }
        if line.len() >= 2 && line.starts_with('[') && line.ends_with(']') {
            let name = &line[1..line.len() - 1];
            let next = file.sections.len();
            let idx = *index.entry(name.to_ascii_lowercase()).or_insert(next);
            if idx == next {
                file.sections.push(IniSection {
                    name: name.to_owned(),
                    entries: Vec::new(),
                });
            }
            current = Some(idx);
            continue;
        }
        if line.starts_with(';') {
            continue;
        }
        match (current, line.split_once('=')) {
            (Some(i), Some((k, v))) => {
                let key = k.trim_matches(is_blank);
                if key.is_empty() {
                    file.ignored_lines += 1;
                    continue;
                }
                let value = config_value(v.trim_matches(is_blank), &mut file.unknown_escapes);
                if let Some(section) = file.sections.get_mut(i) {
                    section.entries.push((key.to_owned(), value));
                }
            }
            _ => file.ignored_lines += 1,
        }
    }
    file
}

/// A value as the game's config reader stores it (CONFIRMED, native:
/// LOCALIZATION.md §2). A value that does not start with `"` is kept as
/// written. Otherwise the leading quote goes, one trailing quote goes when
/// there is one (a value whose closing quote is missing, or sits on the next
/// line, still loses its opening quote), quotes inside stay, and the escapes
/// `\\`, `\"` and `\n` become a backslash, a quote and a line break.
///
/// Deviation (ours): the original reads any other escape as two hex digits;
/// no shipped file has one, so such an escape is kept as written and counted
/// in `unknown_escapes`.
fn config_value(v: &str, unknown_escapes: &mut usize) -> String {
    let Some(rest) = v.strip_prefix('"') else {
        return v.to_owned();
    };
    let inner = rest.strip_suffix('"').unwrap_or(rest);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            _ => {
                *unknown_escapes += 1;
                out.push('\\');
                continue;
            }
        }
        chars.next();
    }
    out
}

/// Parses a UE3 struct text value `(Name=Value,Name="Quoted",...)`: names and
/// values are trimmed, a quoted value may contain `,` `)` and the escapes
/// `\"` `\\`, an unquoted value runs to the next `,` or `)`; text after the
/// closing parenthesis is ignored. A stray quote before the opening
/// parenthesis (`"(Text=,Time=1.0)`, four shipped TUR lines) is skipped: the
/// game's config reader already removes the opening quote of such a value
/// ([`config_value`] does the same; this keeps the function usable on raw
/// text). `None` without parentheses.
pub(crate) fn parse_struct_fields(value: &str) -> Option<Vec<(String, String)>> {
    let value = value.trim_start();
    let value = value
        .strip_prefix('"')
        .filter(|v| v.starts_with('('))
        .unwrap_or(value);
    let mut chars = value.chars().peekable();
    if chars.next() != Some('(') {
        return None;
    }
    let mut fields = Vec::new();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        match chars.peek() {
            None => return None,
            Some(')') => return Some(fields),
            Some(_) => {}
        }
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ',' || c == ')' {
                break;
            }
            name.push(c);
            chars.next();
        }
        if chars.peek() != Some(&'=') {
            // A bare name without a value: skip it.
            continue;
        }
        chars.next();
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut val = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    None => return None,
                    Some('"') => break,
                    Some('\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                        if let Some(n) = chars.next() {
                            val.push(n);
                        }
                    }
                    Some(c) => val.push(c),
                }
            }
            // Anything up to the separator after a closing quote is ignored.
            while chars.peek().is_some_and(|c| *c != ',' && *c != ')') {
                chars.next();
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' || c == ')' {
                    break;
                }
                val.push(c);
                chars.next();
            }
            val = val.trim().to_owned();
        }
        fields.push((name.trim().to_owned(), val));
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// One subtitle line.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct LineOut {
    /// Text (game text: user-local output only).
    pub text: String,
    /// Start time in seconds from the start of the wave.
    pub time: f32,
}

/// A wave's localized subtitles.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct SubtitlesOut {
    /// Lines in index order.
    pub lines: Vec<LineOut>,
    /// `bManualWordWrap` when the file sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_word_wrap: Option<bool>,
    /// `bSingleLine` when the file sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_line: Option<bool>,
    /// `bMature` when the file sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mature: Option<bool>,
}

/// An achievement's localized name and description.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AchievementOut {
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Hidden until earned.
    pub hidden: bool,
}

/// What one file contributed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FileReport {
    /// File name.
    pub file: String,
    /// Encoding found.
    pub encoding: TextEncoding,
    /// Sections.
    pub sections: usize,
    /// Entries.
    pub entries: usize,
    /// Lines ignored by the ini rules.
    pub ignored_lines: usize,
}

/// Counters of one language's export.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct TableIssues {
    /// `Section.Key` pairs seen again (the last value is kept).
    pub duplicate_keys: usize,
    /// `Subtitles[i]` values that are not a struct text.
    pub malformed_lines: usize,
    /// `Subtitles[i]` keys after a missing index (UE3 stops at the first
    /// missing index; TENTATIVE).
    pub lines_after_gap: usize,
    /// Object sections of another class than `SoundNodeWave` (not exported).
    pub other_object_sections: usize,
    /// Lines ignored by the ini rules.
    pub ignored_lines: usize,
    /// Escapes in quoted values other than `\\`, `\"` and `\n` (kept as
    /// written; the original reads them as two hex digits).
    pub unknown_escapes: usize,
}

/// One language's table (`<LANG>.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct LanguageTable {
    pub format: &'static str,
    pub version: u32,
    pub notice: &'static str,
    /// Language extension (`INT`, `DEU`, ...).
    pub language: String,
    /// The files read.
    pub files: Vec<FileReport>,
    /// `Section.Key` → text, from the class sections.
    pub strings: BTreeMap<String, String>,
    /// Wave path (`Package.Group.Name`) → subtitles.
    pub subtitles: BTreeMap<String, SubtitlesOut>,
    /// Achievement API name → text (Steam schema; empty without it).
    pub achievements: BTreeMap<String, AchievementOut>,
    /// Problems counted while reading.
    pub issues: TableIssues,
    /// Lower-case `Section.Key` → the key as first written (UE3 names
    /// compare without case).
    #[serde(skip)]
    lower: BTreeMap<String, String>,
}

impl LanguageTable {
    fn new(language: &str) -> Self {
        Self {
            format: "asamu-localization-table",
            version: FORMAT_VERSION,
            notice: NOTICE,
            language: language.to_owned(),
            ..Self::default()
        }
    }

    /// Adds one parsed file of package `package` (the file stem).
    pub fn add_file(&mut self, package: &str, file_name: &str, enc: TextEncoding, ini: &IniFile) {
        self.files.push(FileReport {
            file: file_name.to_owned(),
            encoding: enc,
            sections: ini.sections.len(),
            entries: ini.sections.iter().map(|s| s.entries.len()).sum(),
            ignored_lines: ini.ignored_lines,
        });
        self.issues.ignored_lines += ini.ignored_lines;
        self.issues.unknown_escapes += ini.unknown_escapes;
        for section in &ini.sections {
            match section.name.rsplit_once(' ') {
                Some((object, class)) => {
                    let object = object.trim();
                    if class.eq_ignore_ascii_case("SoundNodeWave") && !object.is_empty() {
                        let (subs, issues) = wave_subtitles(section);
                        self.issues.malformed_lines += issues.0;
                        self.issues.lines_after_gap += issues.1;
                        self.subtitles
                            .entry(format!("{package}.{object}"))
                            .or_insert(subs);
                    } else {
                        self.issues.other_object_sections += 1;
                    }
                }
                None => {
                    for (k, v) in &section.entries {
                        let key = format!("{}.{}", section.name, k);
                        match self.lower.get(&key.to_ascii_lowercase()) {
                            Some(first) => {
                                // The last value wins under the first spelling.
                                self.issues.duplicate_keys += 1;
                                self.strings.insert(first.clone(), v.clone());
                            }
                            None => {
                                self.lower.insert(key.to_ascii_lowercase(), key.clone());
                                self.strings.insert(key, v.clone());
                            }
                        }
                    }
                }
            }
        }
    }
}

fn parse_flag(v: Option<&str>) -> Option<bool> {
    let v = v?.trim();
    if v.eq_ignore_ascii_case("true") || v == "1" {
        Some(true)
    } else if v.eq_ignore_ascii_case("false") || v == "0" {
        Some(false)
    } else {
        None
    }
}

/// The `Subtitles[0..]` lines of a wave section (consecutive indices from 0
/// up to the first missing one) and (malformed values, keys after a gap).
fn wave_subtitles(section: &IniSection) -> (SubtitlesOut, (usize, usize)) {
    let mut indexed: BTreeMap<usize, &str> = BTreeMap::new();
    for (k, v) in &section.entries {
        let Some(rest) = k
            .get(..10)
            .filter(|p| p.eq_ignore_ascii_case("Subtitles["))
            .and_then(|_| k.get(10..))
        else {
            continue;
        };
        let Some(index) = rest.strip_suffix(']').and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        // A repeated index: the last value wins.
        indexed.insert(index, v.as_str());
    }
    let mut out = SubtitlesOut {
        manual_word_wrap: parse_flag(section.get("bManualWordWrap")),
        single_line: parse_flag(section.get("bSingleLine")),
        mature: parse_flag(section.get("bMature")),
        ..SubtitlesOut::default()
    };
    let mut malformed = 0;
    let mut next = 0usize;
    for (&i, v) in &indexed {
        if i != next {
            break;
        }
        next += 1;
        match parse_struct_fields(v) {
            Some(fields) => {
                let field = |n: &str| {
                    fields
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(n))
                        .map(|(_, v)| v.as_str())
                };
                let time = match field("Time").map(|t| t.parse::<f32>()) {
                    None => 0.0,
                    Some(Ok(t)) if t.is_finite() => t,
                    Some(_) => {
                        malformed += 1;
                        0.0
                    }
                };
                out.lines.push(LineOut {
                    text: field("Text").unwrap_or("").to_owned(),
                    time,
                });
            }
            None => {
                malformed += 1;
                out.lines.push(LineOut {
                    text: String::new(),
                    time: 0.0,
                });
            }
        }
    }
    let after_gap = indexed.len() - next.min(indexed.len());
    (out, (malformed, after_gap))
}

/// The language folders of `localization_dir` (three ASCII letters), sorted.
fn language_dirs(localization_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let rd = std::fs::read_dir(localization_dir)
        .with_context(|| format!("reading {}", localization_dir.display()))?;
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.len() == 3 && name.chars().all(|c| c.is_ascii_alphabetic()))
                .then(|| (name.to_ascii_uppercase(), e.path()))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Reads one language folder into a table: every `<Package>.<lang>` file.
pub(crate) fn read_language(code: &str, dir: &Path) -> Result<LanguageTable> {
    let mut table = LanguageTable::new(code);
    let rd = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(code))
        })
        .collect();
    files.sort();
    for path in files {
        let bytes = read_capped(&path, MAX_LOC_FILE_BYTES)?;
        let (text, enc) = decode_text(&bytes);
        let ini = parse_ini(&text);
        let package = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        table.add_file(&package, &name, enc, &ini);
    }
    Ok(table)
}

fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut data = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut data)
        .with_context(|| format!("reading {}", path.display()))?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > limit {
        bail!("{} is larger than {limit} bytes", path.display());
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Steam metadata
// ---------------------------------------------------------------------------

/// Steam's API language name → the game's language extension (the
/// extensions are `ASAMUSettingsManager.LanguageCodes`, CONFIRMED (cdo)).
/// Steam has no Slovak; `latam` (Latin American Spanish) maps to `ESN`, the
/// game's only Spanish (ours).
pub(crate) fn steam_language_code(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "english" => "INT",
        "german" => "DEU",
        "french" => "FRA",
        "italian" => "ITA",
        "polish" => "POL",
        "spanish" | "latam" => "ESN",
        "portuguese" => "POR",
        "brazilian" => "BRA",
        "turkish" => "TUR",
        "finnish" => "FIN",
        "czech" => "CZE",
        "dutch" => "NLD",
        "hungarian" => "HUN",
        _ => return None,
    })
}

/// The app manifest of the install: the one discovery read, else
/// `<root>/../../appmanifest_278360.acf` (a Steam library layout).
fn app_manifest_path(install: &Install) -> Option<PathBuf> {
    if let asamu_locate::Discovery::Steam {
        manifest_path: Some(p),
        ..
    } = &install.discovery
    {
        return Some(p.clone());
    }
    let steamapps = install.root.parent()?.parent()?;
    let p = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
    p.is_file().then_some(p)
}

/// `AppState.UserConfig.language` of an app manifest.
pub(crate) fn manifest_language(bytes: &[u8]) -> Option<String> {
    let doc = asamu_locate::vdf::parse_bytes(bytes).ok()?;
    let (_, root) = doc.root()?;
    let lang = root.get_path(&["UserConfig", "language"])?.as_str()?;
    let lang = lang.trim();
    (!lang.is_empty() && lang.len() <= 32 && lang.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| lang.to_owned())
}

/// The Steam roots worth searching for the stats schema.
fn steam_roots(install: &Install) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let asamu_locate::Discovery::Steam { steam_root, .. } = &install.discovery {
        roots.push(steam_root.clone());
    }
    roots.extend(asamu_locate::steam_root_candidates(
        &asamu_locate::LocateOptions::from_env(),
    ));
    roots.dedup();
    roots
}

/// A binary KeyValues value (Valve's `.bin` stats schema format).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kv {
    /// A nested node.
    Node(Vec<(String, Kv)>),
    /// A string.
    Str(String),
    /// A 32-bit integer (also pointer and colour values).
    Int(i32),
    /// A 32-bit float.
    Float(f32),
    /// A 64-bit integer.
    Int64(i64),
    /// An unsigned 64-bit integer.
    UInt64(u64),
}

impl Kv {
    fn child(&self, key: &str) -> Option<&Kv> {
        match self {
            Kv::Node(c) => c
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn children(&self) -> &[(String, Kv)] {
        match self {
            Kv::Node(c) => c,
            _ => &[],
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Kv::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Kv::Int(i) => Some(*i != 0),
            Kv::Int64(i) => Some(*i != 0),
            Kv::UInt64(i) => Some(*i != 0),
            Kv::Float(f) => Some(*f != 0.0),
            Kv::Str(s) => parse_flag(Some(s)),
            Kv::Node(_) => None,
        }
    }
}

struct KvReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl KvReader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| format!("truncated at {}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let end = self
            .pos
            .checked_add(N)
            .ok_or_else(|| "offset overflow".to_owned())?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| format!("truncated at {}", self.pos))?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        self.pos = end;
        Ok(out)
    }

    fn cstr(&mut self) -> Result<String, String> {
        let rest = self
            .data
            .get(self.pos..)
            .ok_or_else(|| format!("truncated at {}", self.pos))?;
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| format!("unterminated string at {}", self.pos))?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    fn wstr(&mut self) -> Result<String, String> {
        let mut units = Vec::new();
        loop {
            let u = u16::from_le_bytes(self.take::<2>()?);
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Ok(char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect())
    }

    /// Children until an end marker (or the end of the data at depth 0).
    fn node(&mut self, depth: usize) -> Result<Vec<(String, Kv)>, String> {
        if depth > MAX_KV_DEPTH {
            return Err("nesting too deep".to_owned());
        }
        let mut out = Vec::new();
        loop {
            if depth == 0 && self.pos >= self.data.len() {
                return Ok(out);
            }
            let ty = self.byte()?;
            if ty == 8 || ty == 11 {
                return Ok(out);
            }
            let key = self.cstr()?;
            let value = match ty {
                0 => Kv::Node(self.node(depth + 1)?),
                1 => Kv::Str(self.cstr()?),
                2 | 4 | 6 => Kv::Int(i32::from_le_bytes(self.take::<4>()?)),
                3 => Kv::Float(f32::from_le_bytes(self.take::<4>()?)),
                5 => Kv::Str(self.wstr()?),
                7 => Kv::UInt64(u64::from_le_bytes(self.take::<8>()?)),
                10 => Kv::Int64(i64::from_le_bytes(self.take::<8>()?)),
                other => return Err(format!("unknown value type {other} at {}", self.pos)),
            };
            out.push((key, value));
        }
    }
}

/// Parses binary KeyValues (the root's children).
///
/// # Errors
/// Truncated data, unknown value types, nesting deeper than 32.
pub(crate) fn parse_binary_kv(data: &[u8]) -> Result<Vec<(String, Kv)>, String> {
    KvReader { data, pos: 0 }.node(0)
}

/// Achievement texts per language extension from a stats schema:
/// `<appid> / stats / <n> / bits / <bit> / { name, display { name { <lang> },
/// desc { <lang> }, hidden } }`. Steam languages the game does not have are
/// dropped.
pub(crate) fn schema_achievements(
    root: &[(String, Kv)],
) -> BTreeMap<String, BTreeMap<String, AchievementOut>> {
    let mut out: BTreeMap<String, BTreeMap<String, AchievementOut>> = BTreeMap::new();
    let app = APP_ID.to_string();
    let Some(app_node) = root.iter().find(|(k, _)| *k == app).map(|(_, v)| v) else {
        return out;
    };
    let Some(stats) = app_node.child("stats") else {
        return out;
    };
    for (_, stat) in stats.children() {
        let Some(bits) = stat.child("bits") else {
            continue;
        };
        for (_, bit) in bits.children() {
            let Some(api) = bit.child("name").and_then(Kv::as_str) else {
                continue;
            };
            if api.is_empty()
                || api.len() > 64
                || !api.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            let Some(display) = bit.child("display") else {
                continue;
            };
            let hidden = display
                .child("hidden")
                .and_then(Kv::as_bool)
                .unwrap_or(false);
            let names = display.child("name");
            let descs = display.child("desc");
            let mut langs: BTreeSet<&str> = BTreeSet::new();
            for node in [names, descs].into_iter().flatten() {
                for (k, _) in node.children() {
                    if let Some(code) = steam_language_code(k) {
                        langs.insert(code);
                    }
                }
            }
            for code in langs {
                let pick = |node: Option<&Kv>| -> String {
                    node.map(Kv::children)
                        .unwrap_or_default()
                        .iter()
                        .find(|(k, _)| steam_language_code(k) == Some(code))
                        .and_then(|(_, v)| v.as_str())
                        .unwrap_or("")
                        .to_owned()
                };
                out.entry(code.to_owned()).or_default().insert(
                    api.to_owned(),
                    AchievementOut {
                        name: pick(names),
                        description: pick(descs),
                        hidden,
                    },
                );
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// SWF / GFx text fields
// ---------------------------------------------------------------------------

/// A text field placed in a movie timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwfText {
    /// Timeline: 0 = the main timeline, else the sprite's character id.
    pub sprite: u16,
    /// Display depth.
    pub depth: u16,
    /// Translation in twips (1/20 px).
    pub x: i32,
    /// Translation in twips.
    pub y: i32,
    /// Font height in twips, when set.
    pub size: Option<u16>,
    /// Paragraph alignment of HTML text (`left`, `right`, `center`).
    pub align: Option<String>,
    /// The initial text (HTML reduced to plain text).
    pub text: String,
}

struct EditText {
    size: Option<u16>,
    align: Option<String>,
    text: String,
}

struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.pos).ok_or("truncated")?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes([
            self.u8()?,
            self.u8()?,
            self.u8()?,
            self.u8()?,
        ]))
    }

    fn skip(&mut self, n: usize) -> Result<(), String> {
        let end = self.pos.checked_add(n).ok_or("offset overflow")?;
        if end > self.data.len() {
            return Err("truncated".to_owned());
        }
        self.pos = end;
        Ok(())
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("offset overflow")?;
        let s = self.data.get(self.pos..end).ok_or("truncated")?;
        self.pos = end;
        Ok(s)
    }

    fn cstr(&mut self) -> Result<String, String> {
        let rest = self.data.get(self.pos..).ok_or("truncated")?;
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated string")?;
        let s = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += len + 1;
        Ok(s)
    }

    /// A RECT: skipped (bit-packed, byte aligned at its end).
    fn skip_rect(&mut self) -> Result<(), String> {
        let first = *self.data.get(self.pos).ok_or("truncated")?;
        let nbits = usize::from(first >> 3);
        let bits = 5 + 4 * nbits;
        self.skip(bits.div_ceil(8))
    }

    /// A MATRIX: returns the translation (twips).
    fn matrix(&mut self) -> Result<(i32, i32), String> {
        let mut br = BitReader {
            data: self.data,
            bit: self.pos.checked_mul(8).ok_or("offset overflow")?,
        };
        if br.bits(1)? == 1 {
            let n = br.bits(5)?;
            br.signed(n)?;
            br.signed(n)?;
        }
        if br.bits(1)? == 1 {
            let n = br.bits(5)?;
            br.signed(n)?;
            br.signed(n)?;
        }
        let n = br.bits(5)?;
        let x = br.signed(n)?;
        let y = br.signed(n)?;
        self.pos = br.bit.div_ceil(8);
        Ok((x, y))
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl BitReader<'_> {
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut v: u32 = 0;
        for _ in 0..n {
            let byte = *self.data.get(self.bit / 8).ok_or("truncated")?;
            let b = (byte >> (7 - (self.bit % 8))) & 1;
            v = (v << 1) | u32::from(b);
            self.bit += 1;
        }
        Ok(v)
    }

    fn signed(&mut self, n: u32) -> Result<i32, String> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.bits(n)?;
        let shift = 32 - n;
        // Sign-extend the n-bit value.
        Ok(((v << shift) as i32) >> shift)
    }
}

/// The text fields of an uncompressed SWF or GFx movie (`FWS` / `GFX`): the
/// initial text of every `DefineEditText` and where `PlaceObject2/3` puts it
/// first, in every timeline. Compressed movies (`CWS` / `CFX`) are refused
/// (no inflater in this workspace; the shipped movies are uncompressed).
///
/// # Errors
/// A wrong signature, a compressed movie, or truncated tag data.
pub(crate) fn swf_text_fields(data: &[u8]) -> Result<Vec<SwfText>, String> {
    let sig = data.get(..3).ok_or("too short for a movie header")?;
    match sig {
        b"FWS" | b"GFX" => {}
        b"CWS" | b"CFX" => return Err("compressed movie (not supported)".to_owned()),
        _ => return Err("not a SWF/GFx movie".to_owned()),
    }
    let mut r = ByteReader { data, pos: 4 };
    let declared = usize::try_from(r.u32()?).unwrap_or(usize::MAX);
    let end = declared.min(data.len());
    r.skip_rect()?;
    r.skip(4)?;
    let mut edits: BTreeMap<u16, EditText> = BTreeMap::new();
    let mut placed: Vec<(u16, u16, u16, i32, i32)> = Vec::new();
    let mut seen: BTreeSet<(u16, u16)> = BTreeSet::new();
    walk_tags(data, r.pos, end, 0, 0, &mut edits, &mut placed)?;
    let mut out = Vec::new();
    for (sprite, depth, id, x, y) in placed {
        let Some(e) = edits.get(&id) else {
            continue;
        };
        // The first placement at a depth wins (later ones animate it).
        if !seen.insert((sprite, depth)) {
            continue;
        }
        out.push(SwfText {
            sprite,
            depth,
            x,
            y,
            size: e.size,
            align: e.align.clone(),
            text: e.text.clone(),
        });
    }
    Ok(out)
}

fn walk_tags(
    data: &[u8],
    start: usize,
    end: usize,
    sprite: u16,
    depth: usize,
    edits: &mut BTreeMap<u16, EditText>,
    placed: &mut Vec<(u16, u16, u16, i32, i32)>,
) -> Result<(), String> {
    if depth > MAX_SWF_DEPTH {
        return Err("sprites nested too deep".to_owned());
    }
    let window = data.get(..end).ok_or("truncated")?;
    let mut r = ByteReader {
        data: window,
        pos: start,
    };
    while r.pos < end {
        let code_len = r.u16()?;
        let code = code_len >> 6;
        let mut len = usize::from(code_len & 0x3F);
        if len == 0x3F {
            len = usize::try_from(r.u32()?).map_err(|_| "tag length overflow")?;
        }
        let body_start = r.pos;
        let body = r.bytes(len)?;
        match code {
            0 => return Ok(()),
            37 => {
                if let Ok((id, e)) = edit_text(body) {
                    edits.insert(id, e);
                }
            }
            26 | 70 => {
                if let Ok(Some(p)) = place_object(body, code == 70) {
                    placed.push((sprite, p.0, p.1, p.2, p.3));
                }
            }
            39 => {
                let mut b = ByteReader { data: body, pos: 0 };
                let id = b.u16()?;
                let body_end = body_start.checked_add(len).ok_or("offset overflow")?;
                walk_tags(
                    window,
                    body_start.saturating_add(4),
                    body_end,
                    id,
                    depth + 1,
                    edits,
                    placed,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// `DefineEditText` → (character id, text).
fn edit_text(body: &[u8]) -> Result<(u16, EditText), String> {
    let mut r = ByteReader { data: body, pos: 0 };
    let id = r.u16()?;
    r.skip_rect()?;
    let f1 = r.u8()?;
    let f2 = r.u8()?;
    let has_text = f1 & 0x80 != 0;
    let has_color = f1 & 0x04 != 0;
    let has_max = f1 & 0x02 != 0;
    let has_font = f1 & 0x01 != 0;
    let has_font_class = f2 & 0x80 != 0;
    let has_layout = f2 & 0x20 != 0;
    let html = f2 & 0x02 != 0;
    if has_font {
        r.skip(2)?;
    }
    if has_font_class {
        r.cstr()?;
    }
    let size = if has_font || has_font_class {
        Some(r.u16()?)
    } else {
        None
    };
    if has_color {
        r.skip(4)?;
    }
    if has_max {
        r.skip(2)?;
    }
    if has_layout {
        r.skip(9)?;
    }
    let _variable = r.cstr()?;
    let raw = if has_text { r.cstr()? } else { String::new() };
    let (text, align) = if html {
        (html_to_text(&raw), html_align(&raw))
    } else {
        (raw.replace('\r', "\n").trim().to_owned(), None)
    };
    Ok((id, EditText { size, align, text }))
}

/// `PlaceObject2/3` that places a character → (depth, id, x, y).
fn place_object(body: &[u8], v3: bool) -> Result<Option<(u16, u16, i32, i32)>, String> {
    let mut r = ByteReader { data: body, pos: 0 };
    let f = r.u8()?;
    let f2 = if v3 { r.u8()? } else { 0 };
    let depth = r.u16()?;
    let has_char = f & 0x02 != 0;
    let has_matrix = f & 0x04 != 0;
    if v3 && (f2 & 0x08 != 0 || (f2 & 0x10 != 0 && has_char)) {
        r.cstr()?;
    }
    if !has_char {
        return Ok(None);
    }
    let id = r.u16()?;
    let (x, y) = if has_matrix { r.matrix()? } else { (0, 0) };
    Ok(Some((depth, id, x, y)))
}

/// Flash HTML text → plain text: `<br>` and paragraph ends become line
/// breaks, other tags are dropped, the five named entities and numeric
/// references are decoded.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..lt]));
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = after[1..gt].trim().to_ascii_lowercase();
        if tag == "br" || tag == "br/" || tag == "/p" {
            out.push('\n');
        }
        rest = &after[gt + 1..];
    }
    out.push_str(&decode_entities(rest));
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    lines.join("\n").trim().to_owned()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let decoded = after.find(';').filter(|&e| e <= 10).and_then(|e| {
            let name = &after[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The first `align` attribute of a `<p>` tag.
fn html_align(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let p = lower.find("<p ")?;
    let tag_end = lower[p..].find('>')? + p;
    let tag = &lower[p..tag_end];
    let a = tag.find("align=")? + 6;
    let rest = tag.get(a..)?;
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    let v = &inner[..end];
    matches!(v, "left" | "right" | "center" | "justify").then(|| v.to_owned())
}

/// One cell of a credits row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CreditsCell {
    /// Horizontal position (twips, sprite space).
    pub x: i32,
    /// Font height (twips), when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u16>,
    /// Paragraph alignment, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// Text (game text: user-local output only).
    pub text: String,
}

/// A credits row: cells whose vertical positions are close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CreditsRow {
    /// Vertical position of the row's first cell (twips).
    pub y: i32,
    /// Cells left to right.
    pub cells: Vec<CreditsCell>,
}

/// The credits document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CreditsOut {
    pub format: &'static str,
    pub version: u32,
    pub notice: &'static str,
    /// Source movie.
    pub movie: String,
    /// Text fields in the scrolling timeline.
    pub fields: usize,
    /// Text fields elsewhere in the movie (not exported).
    pub other_fields: usize,
    /// Rows top to bottom.
    pub rows: Vec<CreditsRow>,
}

/// The credits rows: the timeline with the most text fields (the scrolling
/// list), fields ordered by position and grouped into rows.
pub(crate) fn credits_rows(fields: &[SwfText]) -> (Vec<CreditsRow>, usize, usize) {
    let mut per: BTreeMap<u16, usize> = BTreeMap::new();
    for f in fields {
        *per.entry(f.sprite).or_default() += 1;
    }
    let Some((&main, &count)) = per
        .iter()
        .max_by_key(|(id, n)| (**n, std::cmp::Reverse(**id)))
    else {
        return (Vec::new(), 0, 0);
    };
    let mut list: Vec<&SwfText> = fields
        .iter()
        .filter(|f| f.sprite == main && !f.text.is_empty())
        .collect();
    list.sort_by_key(|f| (f.y, f.x, f.depth));
    let mut rows: Vec<CreditsRow> = Vec::new();
    for f in list {
        let cell = CreditsCell {
            x: f.x,
            size: f.size,
            align: f.align.clone(),
            text: f.text.clone(),
        };
        match rows.last_mut() {
            Some(row) if f.y.saturating_sub(row.y) <= CREDITS_ROW_TWIPS => row.cells.push(cell),
            _ => rows.push(CreditsRow {
                y: f.y,
                cells: vec![cell],
            }),
        }
    }
    for row in &mut rows {
        row.cells.sort_by_key(|c| c.x);
    }
    (rows, count, fields.len() - count)
}

/// The credits movie's text from the cooked package.
fn read_credits(cooked: &Path) -> Result<CreditsOut> {
    let file = cooked.join(CREDITS_PACKAGE);
    let (set, lp) =
        PackageSet::for_file(&file).with_context(|| format!("opening {}", file.display()))?;
    let index = lp
        .find(CREDITS_MOVIE)
        .with_context(|| format!("no {CREDITS_MOVIE} in {CREDITS_PACKAGE}"))?;
    let obj = set
        .decode(&lp, index)
        .with_context(|| format!("decoding {CREDITS_MOVIE}"))?;
    let raw = obj
        .properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("RawData"))
        .context("the movie has no RawData")?;
    let bytes: Vec<u8> = match &raw.value {
        UeValue::Array(items) => items
            .iter()
            .map(|v| match v {
                UeValue::Byte(b) => Ok(*b),
                _ => Err(anyhow::anyhow!("RawData is not a byte array")),
            })
            .collect::<Result<_>>()?,
        _ => bail!("RawData is not a byte array"),
    };
    let fields = swf_text_fields(&bytes).map_err(|e| anyhow::anyhow!("credits movie: {e}"))?;
    let (rows, count, other) = credits_rows(&fields);
    Ok(CreditsOut {
        format: "asamu-localization-credits",
        version: FORMAT_VERSION,
        notice: NOTICE,
        movie: obj.path,
        fields: count,
        other_fields: other,
        rows,
    })
}

/// `asamu.ASAMUSettingsManager.LanguageCodes` (the language menu's order;
/// `---` entries are separators) from the class default object.
fn read_language_menu(cooked: &Path) -> Result<Vec<String>> {
    let file = cooked.join("Startup.upk");
    let (set, _) =
        PackageSet::for_file(&file).with_context(|| format!("opening {}", file.display()))?;
    let cdo = set
        .class_defaults("asamu.ASAMUSettingsManager")
        .context("decoding the ASAMUSettingsManager class defaults")?;
    let prop = cdo
        .properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("LanguageCodes"))
        .context("no LanguageCodes in the class defaults")?;
    match &prop.value {
        UeValue::Array(items) => Ok(items
            .iter()
            .filter_map(|v| match v {
                UeValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()),
        _ => bail!("LanguageCodes is not an array"),
    }
}

// ---------------------------------------------------------------------------
// Manifest and run
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct LanguageEntry {
    code: String,
    file: String,
    strings: usize,
    subtitled_waves: usize,
    subtitle_lines: usize,
    achievements: usize,
}

#[derive(Debug, Serialize)]
struct Manifest {
    format: &'static str,
    version: u32,
    notice: &'static str,
    languages: Vec<LanguageEntry>,
    /// `ASAMUSettingsManager.LanguageCodes` (class default), when read.
    language_menu: Vec<String>,
    /// Steam's `UserConfig.language` for the game, when read.
    steam_language: Option<String>,
    /// The language a fresh runtime starts in (Steam's, when converted;
    /// else `INT`; else the first converted language).
    default_language: String,
    /// `credits.json` when written.
    credits: Option<String>,
    /// Where achievement names came from.
    achievements_source: Option<String>,
    warnings: Vec<String>,
}

/// Validated language filter entries (three ASCII letters, upper case).
fn language_filter(args: &[String]) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for a in args {
        let l = a.trim().to_ascii_uppercase();
        if l.len() != 3 || !l.chars().all(|c| c.is_ascii_alphabetic()) {
            bail!("--lang takes language extensions such as INT, DEU or FRA (got {a:?})");
        }
        out.insert(l);
    }
    Ok(out)
}

/// The language a fresh runtime starts in: Steam's language for the game
/// when it was converted, else `INT`, else the first converted language
/// (a `--lang` filter may leave `INT` out).
fn default_language(steam_language: Option<&str>, available: &BTreeSet<&str>) -> String {
    steam_language
        .and_then(steam_language_code)
        .filter(|c| available.contains(c))
        .or_else(|| available.contains("INT").then_some("INT"))
        .or_else(|| available.iter().next().copied())
        .unwrap_or("INT")
        .to_owned()
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    let filter = language_filter(&args.lang)?;
    let mut langs = language_dirs(&install.localization_dir)?;
    if !filter.is_empty() {
        langs.retain(|(code, _)| filter.contains(code));
        if langs.is_empty() {
            bail!("none of the requested languages is in the install");
        }
    }
    let mut warnings = Vec::new();

    let mut tables: Vec<LanguageTable> = Vec::new();
    for (code, dir) in &langs {
        tables.push(read_language(code, dir)?);
    }

    // Steam metadata.
    let mut steam_language = None;
    let mut achievements_source = None;
    if !args.no_steam {
        match app_manifest_path(&install) {
            Some(p) => match read_capped(&p, MAX_STEAM_FILE_BYTES) {
                Ok(b) => steam_language = manifest_language(&b),
                Err(e) => warnings.push(format!("app manifest unreadable: {e}")),
            },
            None => warnings.push("no Steam app manifest found".to_owned()),
        }
        let schema = args.steam_stats.clone().or_else(|| {
            steam_roots(&install)
                .into_iter()
                .map(|r| {
                    r.join("appcache")
                        .join("stats")
                        .join(format!("UserGameStatsSchema_{APP_ID}.bin"))
                })
                .find(|p| p.is_file())
        });
        match schema {
            Some(p) => match read_capped(&p, MAX_STEAM_FILE_BYTES)
                .and_then(|b| parse_binary_kv(&b).map_err(|e| anyhow::anyhow!(e)))
            {
                Ok(root) => {
                    let per_lang = schema_achievements(&root);
                    for t in &mut tables {
                        if let Some(a) = per_lang.get(&t.language) {
                            t.achievements = a.clone();
                        }
                    }
                    achievements_source = Some("Steam stats schema".to_owned());
                }
                Err(e) => warnings.push(format!("Steam stats schema unreadable: {e}")),
            },
            None => warnings
                .push("no Steam stats schema found: achievement names are not exported".to_owned()),
        }
    }
    let available: BTreeSet<&str> = tables.iter().map(|t| t.language.as_str()).collect();
    let default_language = default_language(steam_language.as_deref(), &available);

    let language_menu = match read_language_menu(&install.cooked_dir) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!("language menu order not read: {e:#}"));
            Vec::new()
        }
    };
    let credits = if args.no_credits {
        None
    } else {
        match read_credits(&install.cooked_dir) {
            Ok(c) => Some(c),
            Err(e) => {
                warnings.push(format!("credits not read: {e:#}"));
                None
            }
        }
    };

    let manifest = Manifest {
        format: "asamu-localization",
        version: FORMAT_VERSION,
        notice: NOTICE,
        languages: tables
            .iter()
            .map(|t| LanguageEntry {
                code: t.language.clone(),
                file: format!("{}.json", t.language),
                strings: t.strings.len(),
                subtitled_waves: t.subtitles.len(),
                subtitle_lines: t.subtitles.values().map(|s| s.lines.len()).sum(),
                achievements: t.achievements.len(),
            })
            .collect(),
        language_menu,
        steam_language,
        default_language,
        credits: credits.as_ref().map(|_| "credits.json".to_owned()),
        achievements_source,
        warnings,
    };

    // Summary (counts only: never game text).
    for e in &manifest.languages {
        let t = tables.iter().find(|t| t.language == e.code);
        let issues = t.map(|t| &t.issues);
        println!(
            "{}: {} strings, {} subtitled waves ({} lines), {} achievements{}",
            e.code,
            e.strings,
            e.subtitled_waves,
            e.subtitle_lines,
            e.achievements,
            issues
                .map(|i| format!(
                    " [duplicates {}, malformed lines {}, after gaps {}, ignored lines {}, \
                     unknown escapes {}]",
                    i.duplicate_keys,
                    i.malformed_lines,
                    i.lines_after_gap,
                    i.ignored_lines,
                    i.unknown_escapes
                ))
                .unwrap_or_default()
        );
    }
    if let Some(c) = &credits {
        println!(
            "credits: {} text fields in {} rows ({} other fields)",
            c.fields,
            c.rows.len(),
            c.other_fields
        );
    }
    println!(
        "language menu: {} entries; Steam language: {}; default: {}",
        manifest.language_menu.len(),
        manifest.steam_language.as_deref().unwrap_or("-"),
        manifest.default_language
    );
    for w in &manifest.warnings {
        eprintln!("warning: {w}");
    }
    if args.dry_run {
        println!("dry run: nothing written");
        return Ok(());
    }

    let input = install.localization_dir.clone();
    let root = prepare_out_dir(&ctx.out, &input, &install.root)?;
    let write = |name: &str, json: String| -> Result<()> {
        let target = safety::check_output_path(&root.join(name), &input, true)?;
        safety::write_output(&target, json.as_bytes(), true)
    };
    for t in &tables {
        write(
            &format!("{}.json", t.language),
            serde_json::to_string_pretty(t)?,
        )?;
    }
    if let Some(c) = &credits {
        write("credits.json", serde_json::to_string_pretty(c)?)?;
    }
    write("manifest.json", serde_json::to_string_pretty(&manifest)?)?;
    println!("output {}", root.display());
    println!("note: converted data is copyrighted game data; keep it local, never redistribute");
    Ok(())
}

/// Refuse `dir` (canonical) when it lies inside the game install.
fn refuse_install(dir: &Path, install_root: &Path) -> Result<()> {
    let Ok(root) = install_root.canonicalize() else {
        return Ok(());
    };
    if dir.starts_with(&root) {
        bail!(
            "refusing to write inside the game install {} (choose an output directory outside it)",
            root.display()
        );
    }
    Ok(())
}

/// Validates `<out>/localization` with the safety rules before creating
/// anything, then creates it.
fn prepare_out_dir(out: &Path, input: &Path, install_root: &Path) -> Result<PathBuf> {
    let root = out.join("localization");
    let mut existing = root.clone();
    while !existing.exists() {
        match existing.parent() {
            Some(p) if !p.as_os_str().is_empty() => existing = p.to_path_buf(),
            _ => {
                existing = PathBuf::from(".");
                break;
            }
        }
    }
    safety::check_output_path(
        &existing.join(".asamu-import-localization-probe"),
        input,
        false,
    )
    .with_context(|| format!("refusing output directory {}", out.display()))?;
    let existing = existing.canonicalize()?;
    refuse_install(&existing, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    refuse_install(&root, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    safety::check_output_path(&root.join("manifest.json"), input, true)
        .with_context(|| format!("refusing output directory {}", root.display()))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn text_decoding_follows_the_byte_order_mark() {
        assert_eq!(
            decode_text(&utf16le("[A]\nk=\"\u{e9}\"")),
            ("[A]\nk=\"\u{e9}\"".to_owned(), TextEncoding::Utf16Le)
        );
        let mut be = vec![0xFE, 0xFF];
        for u in "x\u{20ac}".encode_utf16() {
            be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_text(&be).0, "x\u{20ac}");
        assert_eq!(decode_text(&be).1, TextEncoding::Utf16Be);
        assert_eq!(
            decode_text(b"\xEF\xBB\xBFabc"),
            ("abc".to_owned(), TextEncoding::Utf8Bom)
        );
        assert_eq!(
            decode_text(b"plain"),
            ("plain".to_owned(), TextEncoding::Utf8)
        );
        assert_eq!(
            decode_text(b"caf\xE9"),
            ("caf\u{e9}".to_owned(), TextEncoding::Latin1)
        );
        // Odd trailing byte ignored; a lone surrogate becomes U+FFFD.
        assert_eq!(decode_text(&[0xFF, 0xFE, 0x41, 0x00, 0x42]).0, "A");
        assert_eq!(
            decode_text(&[0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00]).0,
            "\u{fffd}A"
        );
        assert_eq!(decode_text(&[]).0, "");
    }

    #[test]
    fn ini_rules_headers_quotes_duplicates_and_merges() {
        let text = "stray=1\r\n; comment\r\n[Menu]\r\nAlpha = \"one \\\"x\\\" \\\\ \\n\"\r\n\
                    Bare=  unquoted value  \r\nAlpha=\"second\"\r\nnot a pair\r\n\
                    [Wave_1 SoundNodeWave]<\r\nSubtitles[0]=(Text=\"a\",Time=0.0)\r\n\
                    [menu]\r\nBeta=\"two\"\r\n=nokey\r\n";
        let ini = parse_ini(text);
        assert_eq!(
            ini.sections.len(),
            1,
            "same name merges; broken header is no header"
        );
        let s = &ini.sections[0];
        assert_eq!(s.name, "Menu");
        assert_eq!(s.get("alpha"), Some("second"), "last wins");
        assert_eq!(s.entries[0].1, "one \"x\" \\ \n", "escapes");
        assert_eq!(s.get("Bare"), Some("unquoted value"));
        assert_eq!(s.get("Beta"), Some("two"));
        // The keys after the broken header stay in the previous section.
        assert_eq!(
            s.get("Subtitles[0]"),
            Some("(Text=\"a\",Time=0.0)"),
            "kept in the open section"
        );
        // stray (before a section), "not a pair", the broken header, "=nokey".
        assert_eq!(ini.ignored_lines, 4);
        assert_eq!(ini.unknown_escapes, 0);
        assert_eq!(parse_ini("").sections.len(), 0);
    }

    /// The quote and escape rules of the game's own config reader
    /// (LOCALIZATION.md §2), on synthetic values.
    #[test]
    fn quoted_values_follow_the_native_reader() {
        let value = |v: &str| {
            let mut unknown = 0;
            (config_value(v, &mut unknown), unknown)
        };
        assert_eq!(value("\"abc\""), ("abc".to_owned(), 0));
        // A missing closing quote: the opening quote still goes, and the
        // escapes are still read (a value whose closing quote sits on the
        // next line).
        assert_eq!(
            value("\"abc \\\"d\\\" [e]"),
            ("abc \"d\" [e]".to_owned(), 0)
        );
        assert_eq!(value("\""), (String::new(), 0), "a lone quote");
        assert_eq!(value("\"\""), (String::new(), 0));
        // Only one trailing quote goes; quotes inside stay; text after an
        // inner quote is kept.
        assert_eq!(value("\"a\"b\""), ("a\"b".to_owned(), 0));
        assert_eq!(value("\"a\" tail"), ("a\" tail".to_owned(), 0));
        assert_eq!(value("\"say \\\"hi\\\"\""), ("say \"hi\"".to_owned(), 0));
        // `\n` is a line break; an escaped backslash before `n` is not.
        assert_eq!(value("\"x\\ny\""), ("x\ny".to_owned(), 0));
        assert_eq!(value("\"x\\\\ny\""), ("x\\ny".to_owned(), 0));
        // Other escapes are kept as written and counted.
        assert_eq!(value("\"a\\qb\""), ("a\\qb".to_owned(), 1));
        assert_eq!(value("\"tail\\"), ("tail\\".to_owned(), 1));
        // Unquoted values are not touched at all.
        assert_eq!(value("a\\nb \"q\""), ("a\\nb \"q\"".to_owned(), 0));
        assert_eq!(value(""), (String::new(), 0));

        // In a file: the value split over two lines keeps its first line
        // (without the opening quote); the rest is an ignored line.
        let ini = parse_ini("[S]\nSplit=\"first\nrest\"\nNext=\"n\\qx\"\n");
        assert_eq!(ini.sections[0].get("Split"), Some("first"));
        assert_eq!(ini.sections[0].get("Next"), Some("n\\qx"));
        assert_eq!((ini.ignored_lines, ini.unknown_escapes), (1, 1));
        // The stray quote before a struct value goes at this layer already.
        let ini = parse_ini("[W SoundNodeWave]\nSubtitles[0]=\"(Text=,Time=2.5)\n");
        assert_eq!(
            ini.sections[0].get("Subtitles[0]"),
            Some("(Text=,Time=2.5)")
        );
    }

    #[test]
    fn lines_split_and_trim_like_the_native_reader() {
        // A carriage return alone ends a line; blank lines are skipped.
        let ini = parse_ini("[A]\rk=1\r\r\nj = 2 \t\n");
        assert_eq!(ini.sections[0].get("k"), Some("1"));
        assert_eq!(ini.sections[0].get("j"), Some("2"));
        // A header needs `[` as the very first character; with leading blanks
        // it is no header (and, without `=`, an ignored line).
        let ini = parse_ini("[A]\nk=1\n  [B]\nk=2\n");
        assert_eq!(ini.sections.len(), 1);
        assert_eq!(ini.sections[0].get("k"), Some("2"));
        assert_eq!(ini.ignored_lines, 1);
        // Trailing blanks after a header are dropped first.
        assert_eq!(parse_ini("[A] \t\nk=1\n").sections[0].name, "A");
        // A comment is a line whose first character is `;`.
        let ini = parse_ini("[A]\n;k=1\n ;j=2\n");
        assert_eq!(ini.sections[0].get("k"), None);
        assert_eq!(ini.sections[0].get(";j"), Some("2"), "not a comment");
        // Only space and tab are blanks: other white space stays in a value.
        let ini = parse_ini("[A]\nk=\u{a0}v\u{a0}\n");
        assert_eq!(ini.sections[0].get("k"), Some("\u{a0}v\u{a0}"));
        // Lines before the first header are ignored, comments silently.
        let ini = parse_ini("; note\nk=1\n[A]\n");
        assert_eq!(ini.ignored_lines, 1);
        assert!(ini.sections[0].entries.is_empty());
    }

    /// Hostile text never panics, and many sections and repeated keys stay
    /// cheap (lookups go through indexes).
    #[test]
    fn hostile_ini_text_is_handled() {
        let alphabet: Vec<char> = "[]=\"\\(),; \t\r\nnTtexim0.5\u{e9}\u{1f600}S"
            .chars()
            .collect();
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        for _ in 0..4000 {
            let len = next() % 48;
            let text: String = (0..len)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect();
            let ini = parse_ini(&text);
            let _ = parse_struct_fields(&text);
            let mut unknown = 0;
            let _ = config_value(&text, &mut unknown);
            let mut table = LanguageTable::new("TST");
            table.add_file("Pkg", "Pkg.tst", TextEncoding::Utf8, &ini);
            for section in &ini.sections {
                let _ = wave_subtitles(section);
            }
        }
        // 20,000 sections, each repeated once, and 20,000 spellings of one key.
        let mut big = String::new();
        for i in 0..20_000 {
            big.push_str(&format!("[S{i}]\nk=1\n[s{i}]\nK=2\n"));
        }
        big.push_str("[Dup]\n");
        for i in 0..20_000 {
            big.push_str(if i % 2 == 0 { "key=a\n" } else { "KEY=b\n" });
        }
        let ini = parse_ini(&big);
        assert_eq!(ini.sections.len(), 20_001);
        let mut table = LanguageTable::new("TST");
        table.add_file("Pkg", "Pkg.tst", TextEncoding::Utf8, &ini);
        assert_eq!(table.strings.len(), 20_001);
        assert_eq!(table.strings["S7.k"], "2", "last value, first spelling");
        assert_eq!(table.strings["Dup.key"], "b");
        assert_eq!(table.issues.duplicate_keys, 20_000 + 19_999);
        // Huge and odd subtitle indexes are ignored or counted, never allocated.
        let ini = parse_ini(
            "[W SoundNodeWave]\nSubtitles[4294967295]=(Text=\"x\",Time=1)\n\
             Subtitles[99999999999999999999]=(Text=\"y\",Time=1)\nSubtitles[-1]=(Text=\"z\")\n\
             Subtitles[]=(Text=\"e\")\nSubtitles[0=(Text=\"f\")\n",
        );
        let (subs, (malformed, after_gap)) = wave_subtitles(&ini.sections[0]);
        assert!(subs.lines.is_empty());
        assert_eq!((malformed, after_gap), (0, 1));
    }

    #[test]
    fn struct_values_parse_leniently() {
        let f = parse_struct_fields("(Text=\"He said \\\"hi\\\", (ok)\",Time=1.500000)\"").unwrap();
        assert_eq!(
            f,
            vec![
                ("Text".to_owned(), "He said \"hi\", (ok)".to_owned()),
                ("Time".to_owned(), "1.500000".to_owned())
            ]
        );
        let f = parse_struct_fields("(Text = \"x\" , Time = 5.4)").unwrap();
        assert_eq!(f[0], ("Text".to_owned(), "x".to_owned()));
        assert_eq!(f[1], ("Time".to_owned(), "5.4".to_owned()));
        let f = parse_struct_fields("(Text=,Time=12.000000)").unwrap();
        assert_eq!(f[0], ("Text".to_owned(), String::new()));
        assert_eq!(
            parse_struct_fields("(Text= \"spaced\",Time=0)").unwrap()[0].1,
            "spaced"
        );
        assert_eq!(parse_struct_fields("()"), Some(vec![]));
        assert_eq!(parse_struct_fields("Text=x"), None);
        assert_eq!(parse_struct_fields("(Text=\"open"), None);
        assert_eq!(parse_struct_fields("(Text=x"), None);
        assert_eq!(parse_struct_fields("(Flag,Text=y)").unwrap().len(), 1);
        let f = parse_struct_fields("\"(Text=,Time=22.000000)").unwrap();
        assert_eq!(f[1], ("Time".to_owned(), "22.000000".to_owned()));
        assert_eq!(parse_struct_fields("\"Text=x"), None);
    }

    #[test]
    fn wave_sections_become_subtitles_and_class_sections_strings() {
        let ini = parse_ini(
            "[Grp.Wave_A SoundNodeWave]\nSubtitles[1]=(Text=\"two\",Time=2.5)\n\
             Subtitles[0]=(Text=\"one\",Time=0.000000)\nSubtitles[3]=(Text=\"gap\",Time=9)\n\
             bManualWordWrap=True\n\
             [Wave_B SoundNodeWave]\nSubtitles[0]=garbage\nSubtitles[1]=(Text=\"t\",Time=nan)\n\
             [Thing Texture2D]\nX=1\n[ASAMUHUD]\nTutorial_A=\"Press #SPACE#\"\nTutorial_A=\"dup\"\n",
        );
        let mut t = LanguageTable::new("TST");
        t.add_file("Pkg", "Pkg.tst", TextEncoding::Utf8, &ini);
        let a = &t.subtitles["Pkg.Grp.Wave_A"];
        assert_eq!(
            a.lines,
            vec![
                LineOut {
                    text: "one".into(),
                    time: 0.0
                },
                LineOut {
                    text: "two".into(),
                    time: 2.5
                }
            ]
        );
        assert_eq!(a.manual_word_wrap, Some(true));
        assert_eq!(a.single_line, None);
        let b = &t.subtitles["Pkg.Wave_B"];
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.lines[0].text, "");
        assert_eq!(t.issues.malformed_lines, 2, "garbage struct + NaN time");
        assert_eq!(t.issues.lines_after_gap, 1);
        assert_eq!(t.issues.other_object_sections, 1);
        assert_eq!(t.strings["ASAMUHUD.Tutorial_A"], "dup", "last wins");
        assert_eq!(t.issues.duplicate_keys, 1);
        assert_eq!(t.files[0].sections, 4);
    }

    #[test]
    fn language_folders_are_read_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        let int = dir.path().join("INT");
        std::fs::create_dir_all(&int).unwrap();
        std::fs::create_dir_all(dir.path().join("Notes")).unwrap();
        std::fs::write(
            int.join("ASAMU.int"),
            utf16le("[GFxASAMUMenu]\nYesLabel=\"Yes\"\n"),
        )
        .unwrap();
        std::fs::write(
            int.join("Shared_Narrator.int"),
            "[g.W SoundNodeWave]\nSubtitles[0]=(Text=\"Hi.\",Time=0.000000)\n",
        )
        .unwrap();
        std::fs::write(int.join("readme.txt"), "[X]\nY=1\n").unwrap();
        let langs = language_dirs(dir.path()).unwrap();
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].0, "INT");
        let t = read_language("INT", &langs[0].1).unwrap();
        assert_eq!(t.files.len(), 2);
        assert_eq!(t.files[0].encoding, TextEncoding::Utf16Le);
        assert_eq!(t.strings["GFxASAMUMenu.YesLabel"], "Yes");
        assert_eq!(t.subtitles["Shared_Narrator.g.W"].lines[0].text, "Hi.");
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["format"], "asamu-localization-table");
        assert_eq!(json["version"], 1);
        assert!(language_filter(&["deu".into()]).unwrap().contains("DEU"));
        assert!(language_filter(&["DE".into()]).is_err());
        assert!(language_filter(&["D3U".into()]).is_err());
    }

    #[test]
    fn steam_language_comes_from_the_app_manifest() {
        let acf = b"\"AppState\"\n{\n\t\"appid\"\t\t\"278360\"\n\t\"UserConfig\"\n\t{\n\t\t\"language\"\t\t\"german\"\n\t}\n}\n";
        assert_eq!(manifest_language(acf).as_deref(), Some("german"));
        assert_eq!(steam_language_code("german"), Some("DEU"));
        assert_eq!(steam_language_code("English"), Some("INT"));
        assert_eq!(steam_language_code("latam"), Some("ESN"));
        assert_eq!(steam_language_code("japanese"), None);
        assert_eq!(manifest_language(b"\"AppState\"\n{\n}\n"), None);
        assert_eq!(manifest_language(b"{{{"), None);
        let odd = b"\"AppState\"{\"UserConfig\"{\"language\"\"../x\"}}";
        assert_eq!(manifest_language(odd), None, "only a plain word");
        // The default: Steam's language when converted, else INT, else the
        // first converted language.
        let all: BTreeSet<&str> = ["DEU", "FRA", "INT"].into_iter().collect();
        assert_eq!(default_language(Some("german"), &all), "DEU");
        assert_eq!(default_language(Some("japanese"), &all), "INT");
        assert_eq!(
            default_language(Some("polish"), &all),
            "INT",
            "not converted"
        );
        assert_eq!(default_language(None, &all), "INT");
        let some: BTreeSet<&str> = ["FRA", "DEU"].into_iter().collect();
        assert_eq!(default_language(Some("english"), &some), "DEU");
        assert_eq!(default_language(None, &BTreeSet::new()), "INT");
    }

    /// Writes binary KeyValues: `(type, key, payload)`.
    fn kv_node(out: &mut Vec<u8>, key: &str, children: impl FnOnce(&mut Vec<u8>)) {
        out.push(0);
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        children(out);
        out.push(8);
    }

    fn kv_str(out: &mut Vec<u8>, key: &str, v: &str) {
        out.push(1);
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }

    fn kv_int(out: &mut Vec<u8>, key: &str, v: i32) {
        out.push(2);
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn synthetic_schema() -> Vec<u8> {
        let mut b = Vec::new();
        kv_node(&mut b, "278360", |b| {
            kv_node(b, "stats", |b| {
                kv_node(b, "1", |b| {
                    kv_node(b, "bits", |b| {
                        kv_node(b, "1", |b| {
                            kv_str(b, "name", "TEST_ONE");
                            kv_node(b, "display", |b| {
                                kv_node(b, "name", |b| {
                                    kv_str(b, "english", "First");
                                    kv_str(b, "token", "NEW_ACHIEVEMENT_1_0_NAME");
                                    kv_str(b, "german", "Erste");
                                    kv_str(b, "klingon", "x");
                                });
                                kv_node(b, "desc", |b| {
                                    kv_str(b, "english", "Do one thing");
                                });
                                kv_int(b, "hidden", 1);
                            });
                        });
                        kv_node(b, "2", |b| {
                            kv_str(b, "name", "bad name!");
                        });
                    });
                    kv_str(b, "type", "ACHIEVEMENTS");
                });
            });
            kv_int(b, "version", 3);
        });
        b
    }

    #[test]
    fn achievement_texts_come_from_the_stats_schema() {
        let root = parse_binary_kv(&synthetic_schema()).unwrap();
        let per = schema_achievements(&root);
        assert_eq!(per.len(), 2, "INT and DEU; unknown Steam languages dropped");
        let int = &per["INT"]["TEST_ONE"];
        assert_eq!(int.name, "First");
        assert_eq!(int.description, "Do one thing");
        assert!(int.hidden);
        let deu = &per["DEU"]["TEST_ONE"];
        assert_eq!(deu.name, "Erste");
        assert_eq!(deu.description, "", "no German description");
        assert!(!per["INT"].contains_key("bad name!"));
        // Every truncation fails cleanly; never a panic.
        let full = synthetic_schema();
        for n in 0..full.len() {
            let _ = parse_binary_kv(&full[..n]);
        }
        assert!(parse_binary_kv(&[9, b'k', 0]).is_err(), "unknown type");
        let mut deep = Vec::new();
        for _ in 0..40 {
            deep.extend_from_slice(&[0, b'k', 0]);
        }
        assert!(parse_binary_kv(&deep).is_err(), "too deep");
        assert!(schema_achievements(&[]).is_empty());
    }

    // --- SWF ---------------------------------------------------------------

    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Bits {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                acc: 0,
                n: 0,
            }
        }
        fn put(&mut self, v: i64, bits: u32) {
            for i in (0..bits).rev() {
                self.acc = (self.acc << 1) | (((v >> i) & 1) as u64);
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                let pad = 8 - self.n;
                self.put(0, pad);
            }
            self.out
        }
    }

    fn rect() -> Vec<u8> {
        let mut b = Bits::new();
        b.put(15, 5);
        for v in [0, 11000, 0, 8000] {
            b.put(v, 15);
        }
        b.finish()
    }

    fn tag(code: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if body.len() < 0x3F {
            out.extend_from_slice(&((code << 6) | body.len() as u16).to_le_bytes());
        } else {
            out.extend_from_slice(&((code << 6) | 0x3F).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    fn edit_text_tag(id: u16, size: u16, html: bool, text: &str) -> Vec<u8> {
        let mut b = id.to_le_bytes().to_vec();
        b.extend(rect());
        b.push(0x80 | 0x01); // HasText, HasFont
        b.push(if html { 0x02 } else { 0 });
        b.extend_from_slice(&7u16.to_le_bytes()); // font id
        b.extend_from_slice(&size.to_le_bytes());
        b.push(0); // variable name ""
        b.extend_from_slice(text.as_bytes());
        b.push(0);
        tag(37, &b)
    }

    fn place_tag(depth: u16, id: u16, x: i64, y: i64) -> Vec<u8> {
        let mut b = vec![0x02 | 0x04];
        b.extend_from_slice(&depth.to_le_bytes());
        b.extend_from_slice(&id.to_le_bytes());
        let mut m = Bits::new();
        m.put(1, 1); // has scale
        m.put(17, 5);
        m.put(65536, 17);
        m.put(65536, 17);
        m.put(0, 1); // no rotate
        m.put(20, 5);
        m.put(x, 20);
        m.put(y, 20);
        b.extend(m.finish());
        tag(26, &b)
    }

    fn synthetic_movie() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(rect());
        body.extend_from_slice(&[0, 24, 1, 0]);
        body.extend(edit_text_tag(
            3,
            840,
            true,
            "<p align=\"right\"><font size=\"42\">Role &amp; Title</font></p>",
        ));
        body.extend(edit_text_tag(
            4,
            640,
            true,
            "<p align=\"left\"><font>Ann Example</font></p>",
        ));
        body.extend(edit_text_tag(5, 1920, false, "Heading"));
        body.extend(edit_text_tag(6, 360, false, "Press"));
        // Sprite 9: the scrolling list.
        let mut sprite = 9u16.to_le_bytes().to_vec();
        sprite.extend_from_slice(&1u16.to_le_bytes());
        sprite.extend(place_tag(1, 5, -12750, 15040));
        sprite.extend(place_tag(2, 3, -12750, 17640));
        sprite.extend(place_tag(3, 4, 50, 17740));
        // A later move of depth 2 (no character): ignored.
        sprite.extend(tag(26, &[0x01 | 0x04, 2, 0, 0]));
        sprite.extend(tag(1, &[]));
        sprite.extend(tag(0, &[]));
        body.extend(tag(39, &sprite));
        body.extend(place_tag(1, 6, 0, 0));
        body.extend(tag(0, &[]));
        let mut movie = b"GFX\x0f".to_vec();
        movie.extend_from_slice(&((body.len() + 8) as u32).to_le_bytes());
        movie.extend(body);
        movie
    }

    #[test]
    fn swf_text_fields_and_credit_rows() {
        let movie = synthetic_movie();
        let fields = swf_text_fields(&movie).unwrap();
        assert_eq!(fields.len(), 4);
        let role = fields
            .iter()
            .find(|f| f.depth == 2 && f.sprite == 9)
            .unwrap();
        assert_eq!(role.text, "Role & Title");
        assert_eq!(role.align.as_deref(), Some("right"));
        assert_eq!((role.x, role.y), (-12750, 17640));
        assert_eq!(role.size, Some(840));
        let (rows, count, other) = credits_rows(&fields);
        assert_eq!((count, other), (3, 1));
        assert_eq!(rows.len(), 2, "heading row, then role + name");
        assert_eq!(rows[0].cells[0].text, "Heading");
        assert_eq!(rows[1].cells.len(), 2);
        assert_eq!(rows[1].cells[0].text, "Role & Title");
        assert_eq!(rows[1].cells[1].text, "Ann Example");
        // Every truncation and corruption fails cleanly.
        for n in 0..movie.len() {
            let _ = swf_text_fields(&movie[..n]);
        }
        let mut bad = movie.clone();
        for i in (8..bad.len()).step_by(7) {
            bad[i] ^= 0xA5;
            let _ = swf_text_fields(&bad);
        }
        assert!(swf_text_fields(b"CWS\x0a\0\0\0\0").is_err());
        assert!(swf_text_fields(b"PNG").is_err());
        assert!(credits_rows(&[]).0.is_empty());
    }

    #[test]
    fn html_reduces_to_plain_text() {
        assert_eq!(
            html_to_text(
                "<p align=\"center\"><font color=\"#fff\">A &lt;b&gt; &#65;&#x42; &bogus; &</font></p><p>Next</p>"
            ),
            "A <b> AB &bogus; &\nNext"
        );
        assert_eq!(html_to_text("one<br>two<BR/>three"), "one\ntwo\nthree");
        assert_eq!(html_to_text("unclosed <tag"), "unclosed");
        assert_eq!(
            html_align("<P ALIGN='Center'>x</P>").as_deref(),
            Some("center")
        );
        assert_eq!(html_align("<p align=wide>x</p>"), None);
        assert_eq!(html_align("plain"), None);
    }

    #[test]
    fn output_directory_refuses_the_install() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("game");
        std::fs::create_dir_all(&install).unwrap();
        let input = install.join("input");
        assert!(prepare_out_dir(&install.join("out"), &input, &install).is_err());
        assert!(!install.join("out").exists(), "nothing created");
        let ok = prepare_out_dir(&dir.path().join("conv"), &input, &install).unwrap();
        assert!(ok.ends_with("localization"));
        // A link to the install is the install.
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&install, &link).unwrap();
            assert!(prepare_out_dir(&link.join("out"), &input, &install).is_err());
            assert!(
                !install.join("out").exists(),
                "nothing created through the link"
            );
        }
    }

    /// Converted text never lands in the repository (outside the git-ignored
    /// research folders), and a refused directory is not created.
    #[test]
    fn output_directory_refuses_the_repository() {
        let Some(repo) = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
        else {
            return;
        };
        if !repo.join("docs").is_dir() {
            return;
        }
        let elsewhere = tempfile::tempdir().unwrap();
        let input = elsewhere.path().join("input");
        let install = elsewhere.path().join("game");
        for refused in [
            repo.join("docs").join("asamu-localization-test-out"),
            repo.join("asamu-localization-test-out"),
            repo.join("research").join("asamu-localization-test-out"),
        ] {
            assert!(
                prepare_out_dir(&refused, &input, &install).is_err(),
                "{}",
                refused.display()
            );
            assert!(!refused.exists(), "nothing created");
        }
    }

    /// Real data (skipped without the install): every language folder parses,
    /// every language has the HUD's tutorial texts and the subtitles, and the
    /// credits movie yields its text fields. Counts only; no text is printed.
    #[test]
    fn real_install_localization_parses() {
        let install = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
            Some(d) => asamu_locate::from_original_dir(Path::new(&d)),
            None => asamu_locate::locate(),
        };
        let Ok(install) = install else {
            eprintln!("skipping: original install not found");
            return;
        };
        let langs = language_dirs(&install.localization_dir).unwrap();
        assert!(langs.iter().any(|(c, _)| c == "INT"));
        for (code, dir) in &langs {
            let t = read_language(code, dir).unwrap();
            let tutorials = t
                .strings
                .keys()
                .filter(|k| k.starts_with("ASAMUHUD.Tutorial_"))
                .count();
            assert_eq!(tutorials, 26, "{code}");
            assert!(t.subtitles.len() >= 150, "{code}: {}", t.subtitles.len());
            assert_eq!(t.issues.other_object_sections, 0, "{code}");
            eprintln!(
                "{code}: {} strings, {} waves, {} lines, issues {:?}",
                t.strings.len(),
                t.subtitles.len(),
                t.subtitles.values().map(|s| s.lines.len()).sum::<usize>(),
                t.issues
            );
        }
        let menu = read_language_menu(&install.cooked_dir).unwrap();
        assert!(menu.iter().any(|c| c == "INT"));
        let credits = read_credits(&install.cooked_dir).unwrap();
        assert!(credits.fields >= 100, "{}", credits.fields);
        assert!(!credits.rows.is_empty());
    }
}
