//! Runtime localized text tables, converted on the user's machine from their
//! own install by `asamu-import localization` (`<converted>/localization/`).
//! Nothing here ships game text: the tables are user-local data, and every
//! test uses synthetic strings.
//!
//! - [`Localization`]: every language's table, the language menu order
//!   (`ASAMUSettingsManager.LanguageCodes`, CONFIRMED (cdo)), Steam's default
//!   language and the credits text. Every lookup falls back to `INT` when
//!   the chosen language lacks the entry (TENTATIVE match of the engine's
//!   `Localize` fallback; for subtitles it reproduces the original exactly
//!   where a language file lacks a wave: the cooked wave keeps its `INT`
//!   lines, LOCALIZATION.md §4).
//! - [`SubtitleTranslation`]: maps a subtitle line in any language to the
//!   chosen language (the HUD shows the audio's lines through it).
//! - [`display_text`] / [`expand_key_placeholders`]: the presentation of a
//!   raw localized string (line-break escapes, the tutorial texts' key
//!   placeholders).
//!
//! Converted files are untrusted input: sizes and counts are bounded, a
//! malformed table is an error value, never a panic.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::audio::{SubtitleLine, SubtitleTrack};
use crate::error::{AssetError, AssetResult};
use crate::files::{parse_json, read_bounded, safe_relative_path};

/// Folder of the converted tables inside a converted directory.
pub const LOCALIZATION_DIR: &str = "localization";
/// The language every lookup falls back to.
pub const FALLBACK_LANGUAGE: &str = "INT";
/// Separator entries of the language menu (`LanguageCodes`).
pub const MENU_SEPARATOR: &str = "---";

const MANIFEST_FORMAT: &str = "asamu-localization";
const TABLE_FORMAT: &str = "asamu-localization-table";
const CREDITS_FORMAT: &str = "asamu-localization-credits";
const FORMAT_VERSION: u32 = 1;
/// Largest manifest accepted.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Largest language table accepted (a shipped language converts to ~100 KB).
const MAX_TABLE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest credits document accepted.
const MAX_CREDITS_BYTES: u64 = 4 * 1024 * 1024;
/// Most languages loaded.
const MAX_LANGUAGES: usize = 64;
/// Longest string kept (characters; longer ones are cut).
const MAX_CHARS: usize = 4096;
/// Longest key accepted (a string key, wave path or achievement name; bytes).
const MAX_KEY_BYTES: usize = 512;
/// Most strings in one table (a shipped language converts to 343).
const MAX_STRINGS: usize = 16_384;
/// Most subtitled waves in one table (shipped: 158).
const MAX_WAVES: usize = 8_192;
/// Most subtitle lines of one wave (shipped: 15).
const MAX_WAVE_LINES: usize = 256;
/// Most subtitle lines in one table (shipped: 523). Together with
/// [`MAX_WAVE_LINES`] this bounds the work of
/// [`Localization::subtitle_translation`].
const MAX_SUBTITLE_LINES: usize = 32_768;
/// Most achievements in one table (shipped: 15).
const MAX_ACHIEVEMENTS: usize = 1_024;
/// Most credits rows (shipped: 77) and cells in a row (shipped: 3).
const MAX_CREDITS_ROWS: usize = 4_096;
const MAX_CREDITS_CELLS: usize = 16;

/// A language extension (`INT`, `DEU`, ...) normalized to upper case, or
/// `None` when it is not two to four ASCII letters.
#[must_use]
pub fn normalize_language(code: &str) -> Option<String> {
    let c = code.trim();
    ((2..=4).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_alphabetic()))
        .then(|| c.to_ascii_uppercase())
}

/// An achievement's localized texts (from Steam's stats schema).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct AchievementText {
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Hidden until earned.
    #[serde(default)]
    pub hidden: bool,
}

/// One cell of a credits row.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CreditsCell {
    /// Horizontal position in the movie (twips).
    #[serde(default)]
    pub x: i32,
    /// Font height (twips), when set.
    #[serde(default)]
    pub size: Option<u16>,
    /// Paragraph alignment (`left`, `right`, `center`), when set.
    #[serde(default)]
    pub align: Option<String>,
    /// Text.
    #[serde(default)]
    pub text: String,
}

/// A credits row (cells left to right).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CreditsRow {
    /// Vertical position in the movie (twips).
    #[serde(default)]
    pub y: i32,
    /// Cells.
    #[serde(default)]
    pub cells: Vec<CreditsCell>,
}

impl CreditsRow {
    /// The row as one line: cells joined by `sep`.
    #[must_use]
    pub fn line(&self, sep: &str) -> String {
        self.cells
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(sep)
    }
}

/// The credits movie's text (language independent: the movie's own text
/// fields, LOCALIZATION.md §5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credits {
    /// Rows top to bottom.
    pub rows: Vec<CreditsRow>,
}

/// One language's table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LanguageTable {
    /// Language extension.
    pub language: String,
    strings: BTreeMap<String, String>,
    /// Lower-case key → key.
    lower: HashMap<String, String>,
    subtitles: BTreeMap<String, Vec<SubtitleLine>>,
    /// Lower-case wave path → wave path.
    wave_lower: HashMap<String, String>,
    achievements: BTreeMap<String, AchievementText>,
}

#[derive(Deserialize)]
struct TableDoc {
    format: String,
    version: u32,
    language: String,
    #[serde(default)]
    strings: BTreeMap<String, String>,
    #[serde(default)]
    subtitles: BTreeMap<String, SubtitlesDoc>,
    #[serde(default)]
    achievements: BTreeMap<String, AchievementText>,
}

#[derive(Deserialize)]
struct SubtitlesDoc {
    #[serde(default)]
    lines: Vec<LineDoc>,
}

#[derive(Deserialize)]
struct LineDoc {
    #[serde(default)]
    text: String,
    #[serde(default)]
    time: f32,
}

fn bounded(s: &str) -> String {
    s.chars().take(MAX_CHARS).collect()
}

/// Refuses a table section with more than `limit` entries or with a key
/// longer than [`MAX_KEY_BYTES`].
fn check_entries<'a>(
    path: &Path,
    what: &str,
    count: usize,
    limit: usize,
    mut keys: impl Iterator<Item = &'a String>,
) -> AssetResult<()> {
    if count > limit {
        return Err(AssetError::Format {
            path: path.to_path_buf(),
            expected: format!("at most {limit} {what}"),
            found: count.to_string(),
        });
    }
    if let Some(long) = keys.find(|k| k.len() > MAX_KEY_BYTES) {
        return Err(AssetError::Format {
            path: path.to_path_buf(),
            expected: format!("{what} keys of at most {MAX_KEY_BYTES} bytes"),
            found: format!("a key of {} bytes", long.len()),
        });
    }
    Ok(())
}

impl LanguageTable {
    /// An empty table for `language`.
    #[must_use]
    pub fn new(language: &str) -> Self {
        Self {
            language: language.to_owned(),
            ..Self::default()
        }
    }

    /// Adds (or replaces) a string.
    pub fn insert_string(&mut self, key: &str, text: &str) {
        let lower = key.to_ascii_lowercase();
        if let Some(old) = self.lower.get(&lower).cloned() {
            self.strings.remove(&old);
        }
        self.lower.insert(lower, key.to_owned());
        self.strings.insert(key.to_owned(), bounded(text));
    }

    /// Adds (or replaces) a wave's subtitle lines.
    pub fn insert_subtitles(&mut self, wave: &str, lines: Vec<SubtitleLine>) {
        let lower = wave.to_ascii_lowercase();
        if let Some(old) = self.wave_lower.get(&lower).cloned() {
            self.subtitles.remove(&old);
        }
        self.wave_lower.insert(lower, wave.to_owned());
        self.subtitles.insert(wave.to_owned(), lines);
    }

    /// Adds (or replaces) an achievement's texts.
    pub fn insert_achievement(&mut self, api_name: &str, text: AchievementText) {
        self.achievements.insert(api_name.to_owned(), text);
    }

    /// The string `key` (`Section.Key`; exact, else ASCII case-insensitive
    /// as UE3 names are).
    #[must_use]
    pub fn string(&self, key: &str) -> Option<&str> {
        if let Some(s) = self.strings.get(key) {
            return Some(s);
        }
        let k = self.lower.get(&key.to_ascii_lowercase())?;
        self.strings.get(k).map(String::as_str)
    }

    /// Number of strings.
    #[must_use]
    pub fn string_count(&self) -> usize {
        self.strings.len()
    }

    /// The string keys (sorted).
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.strings.keys().map(String::as_str)
    }

    /// The subtitle lines of a wave (`Package.Group.Name`; exact, else
    /// ASCII case-insensitive as UE3 object paths are).
    #[must_use]
    pub fn subtitles(&self, wave: &str) -> Option<&[SubtitleLine]> {
        if let Some(lines) = self.subtitles.get(wave) {
            return Some(lines);
        }
        let key = self.wave_lower.get(&wave.to_ascii_lowercase())?;
        self.subtitles.get(key).map(Vec::as_slice)
    }

    /// Every wave with subtitles.
    pub fn subtitled_waves(&self) -> impl Iterator<Item = (&str, &[SubtitleLine])> {
        self.subtitles
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// An achievement's texts by API name (`FLOOR_IS_LAVA`, ...).
    #[must_use]
    pub fn achievement(&self, api_name: &str) -> Option<&AchievementText> {
        self.achievements.get(api_name)
    }

    /// Parses a `<LANG>.json` table.
    ///
    /// # Errors
    /// Malformed JSON, a wrong format or version, an invalid language code,
    /// or more entries than a table may hold (strings, waves, lines of one
    /// wave, lines in all, achievements) or an over-long key.
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let doc: TableDoc = parse_json(path, data)?;
        check(path, &doc.format, doc.version, TABLE_FORMAT)?;
        check_entries(
            path,
            "strings",
            doc.strings.len(),
            MAX_STRINGS,
            doc.strings.keys(),
        )?;
        check_entries(
            path,
            "subtitled waves",
            doc.subtitles.len(),
            MAX_WAVES,
            doc.subtitles.keys(),
        )?;
        check_entries(
            path,
            "achievements",
            doc.achievements.len(),
            MAX_ACHIEVEMENTS,
            doc.achievements.keys(),
        )?;
        let longest = doc.subtitles.values().map(|s| s.lines.len()).max();
        check_entries(
            path,
            "subtitle lines in a wave",
            longest.unwrap_or(0),
            MAX_WAVE_LINES,
            std::iter::empty(),
        )?;
        let lines = doc
            .subtitles
            .values()
            .map(|s| s.lines.len())
            .fold(0usize, usize::saturating_add);
        check_entries(
            path,
            "subtitle lines",
            lines,
            MAX_SUBTITLE_LINES,
            std::iter::empty(),
        )?;
        let language = normalize_language(&doc.language).ok_or_else(|| AssetError::Format {
            path: path.to_path_buf(),
            expected: "a language extension such as INT".to_owned(),
            found: doc.language.clone(),
        })?;
        let mut t = Self::new(&language);
        for (k, v) in &doc.strings {
            t.insert_string(k, v);
        }
        for (wave, s) in doc.subtitles {
            let lines = s
                .lines
                .into_iter()
                .map(|l| SubtitleLine {
                    text: bounded(&l.text),
                    time: if l.time.is_finite() { l.time } else { 0.0 },
                })
                .collect();
            t.insert_subtitles(&wave, lines);
        }
        for (k, a) in doc.achievements {
            t.insert_achievement(
                &k,
                AchievementText {
                    name: bounded(&a.name),
                    description: bounded(&a.description),
                    hidden: a.hidden,
                },
            );
        }
        Ok(t)
    }
}

fn check(path: &Path, format: &str, version: u32, expected: &str) -> AssetResult<()> {
    if format != expected || version != FORMAT_VERSION {
        return Err(AssetError::Format {
            path: path.to_path_buf(),
            expected: format!("{expected} version {FORMAT_VERSION}"),
            found: format!("{format} version {version}"),
        });
    }
    Ok(())
}

#[derive(Deserialize)]
struct ManifestDoc {
    format: String,
    version: u32,
    #[serde(default)]
    languages: Vec<ManifestLanguage>,
    #[serde(default)]
    language_menu: Vec<String>,
    #[serde(default)]
    steam_language: Option<String>,
    #[serde(default)]
    default_language: Option<String>,
    #[serde(default)]
    credits: Option<String>,
}

#[derive(Deserialize)]
struct ManifestLanguage {
    code: String,
    file: String,
}

#[derive(Deserialize)]
struct CreditsDoc {
    format: String,
    version: u32,
    #[serde(default)]
    rows: Vec<CreditsRow>,
}

/// Every language's localized text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Localization {
    tables: BTreeMap<String, LanguageTable>,
    language_menu: Vec<String>,
    steam_language: Option<String>,
    default_language: Option<String>,
    credits: Option<Credits>,
    warnings: Vec<String>,
}

impl Localization {
    /// The tables of `converted_root/localization/`; `Ok(None)` when the
    /// directory has no manifest (not converted). A language whose table
    /// cannot be read is skipped with a warning.
    ///
    /// # Errors
    /// An unreadable or malformed manifest.
    pub fn load(converted_root: &Path) -> AssetResult<Option<Self>> {
        let dir = converted_root.join(LOCALIZATION_DIR);
        let manifest_path = dir.join("manifest.json");
        if !manifest_path.is_file() {
            return Ok(None);
        }
        let data = read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
        let doc: ManifestDoc = parse_json(&manifest_path, &data)?;
        check(&manifest_path, &doc.format, doc.version, MANIFEST_FORMAT)?;
        let mut out = Self {
            // An entry that is no language extension becomes a separator, so
            // the positions (the indexes of the language-name lists) stay.
            language_menu: doc
                .language_menu
                .iter()
                .take(MAX_LANGUAGES)
                .map(|c| normalize_language(c).unwrap_or_else(|| MENU_SEPARATOR.to_owned()))
                .collect(),
            steam_language: doc.steam_language.map(|s| bounded(&s)),
            default_language: doc.default_language.as_deref().and_then(normalize_language),
            ..Self::default()
        };
        for l in doc.languages.iter().take(MAX_LANGUAGES) {
            let loaded = safe_relative_path(&l.file).and_then(|file| {
                let path = dir.join(&file);
                let data = read_bounded(&path, MAX_TABLE_BYTES)?;
                LanguageTable::from_json(&path, &data)
            });
            match loaded {
                Ok(t) if normalize_language(&l.code).as_deref() == Some(t.language.as_str()) => {
                    out.tables.insert(t.language.clone(), t);
                }
                Ok(t) => out.warnings.push(format!(
                    "{}: table says {} but the manifest lists {}",
                    l.file, t.language, l.code
                )),
                Err(e) => out.warnings.push(format!("{}: {e}", l.file)),
            }
        }
        if let Some(file) = &doc.credits {
            let loaded = safe_relative_path(file).and_then(|file| {
                let path = dir.join(&file);
                let data = read_bounded(&path, MAX_CREDITS_BYTES)?;
                let c: CreditsDoc = parse_json(&path, &data)?;
                check(&path, &c.format, c.version, CREDITS_FORMAT)?;
                check_entries(
                    &path,
                    "credits rows",
                    c.rows.len(),
                    MAX_CREDITS_ROWS,
                    std::iter::empty(),
                )?;
                check_entries(
                    &path,
                    "cells in a credits row",
                    c.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0),
                    MAX_CREDITS_CELLS,
                    std::iter::empty(),
                )?;
                Ok(c)
            });
            match loaded {
                Ok(c) => {
                    let rows = c
                        .rows
                        .into_iter()
                        .map(|mut r| {
                            for cell in &mut r.cells {
                                cell.text = bounded(&cell.text);
                                cell.align = cell.align.take().map(|a| bounded(&a));
                            }
                            r
                        })
                        .collect();
                    out.credits = Some(Credits { rows });
                }
                Err(e) => out.warnings.push(format!("credits: {e}")),
            }
        }
        Ok(Some(out))
    }

    /// Localization from tables built in memory (tests, tools).
    #[must_use]
    pub fn from_tables(tables: Vec<LanguageTable>) -> Self {
        Self {
            tables: tables
                .into_iter()
                .map(|t| (t.language.clone(), t))
                .collect(),
            ..Self::default()
        }
    }

    /// Sets the language menu order (`LanguageCodes`, `---` separators).
    #[must_use]
    pub fn with_language_menu(mut self, menu: &[&str]) -> Self {
        self.language_menu = menu
            .iter()
            .take(MAX_LANGUAGES)
            .map(|c| normalize_language(c).unwrap_or_else(|| MENU_SEPARATOR.to_owned()))
            .collect();
        self
    }

    /// Sets the default language (Steam's).
    #[must_use]
    pub fn with_default_language(mut self, code: &str) -> Self {
        self.default_language = normalize_language(code);
        self
    }

    /// Sets the credits.
    #[must_use]
    pub fn with_credits(mut self, credits: Credits) -> Self {
        self.credits = Some(credits);
        self
    }

    /// Problems met while loading.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The languages with a table, in the language menu's order (languages
    /// the menu does not list follow, sorted).
    #[must_use]
    pub fn languages(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for c in &self.language_menu {
            if let Some((k, _)) = self.tables.get_key_value(c)
                && !out.contains(&k.as_str())
            {
                out.push(k);
            }
        }
        for k in self.tables.keys() {
            if !out.contains(&k.as_str()) {
                out.push(k);
            }
        }
        out
    }

    /// A table exists for `code` (any case).
    #[must_use]
    pub fn has_language(&self, code: &str) -> bool {
        normalize_language(code).is_some_and(|c| self.tables.contains_key(&c))
    }

    /// Steam's language name for the game (`UserConfig.language`), if read.
    #[must_use]
    pub fn steam_language(&self) -> Option<&str> {
        self.steam_language.as_deref()
    }

    /// The language a fresh start uses: the converted default (Steam's, when
    /// the install has it), else `INT`, else the first table.
    #[must_use]
    pub fn default_language(&self) -> &str {
        if let Some(d) = self
            .default_language
            .as_deref()
            .filter(|d| self.tables.contains_key(*d))
        {
            return d;
        }
        if self.tables.contains_key(FALLBACK_LANGUAGE) {
            return FALLBACK_LANGUAGE;
        }
        self.tables
            .keys()
            .next()
            .map_or(FALLBACK_LANGUAGE, String::as_str)
    }

    /// `wanted` when a table exists for it, else [`Self::default_language`].
    #[must_use]
    pub fn resolve_language(&self, wanted: Option<&str>) -> String {
        wanted
            .and_then(normalize_language)
            .filter(|c| self.tables.contains_key(c))
            .unwrap_or_else(|| self.default_language().to_owned())
    }

    /// The table of `code`.
    #[must_use]
    pub fn table(&self, code: &str) -> Option<&LanguageTable> {
        self.tables.get(code)
    }

    /// The tables to consult for `lang`: its own, then `INT`.
    fn chain(&self, lang: &str) -> impl Iterator<Item = &LanguageTable> {
        let own = self.tables.get(lang);
        let fallback = (lang != FALLBACK_LANGUAGE)
            .then(|| self.tables.get(FALLBACK_LANGUAGE))
            .flatten();
        own.into_iter().chain(fallback)
    }

    /// The string `key` (`Section.Key`) in `lang`, else in `INT`.
    #[must_use]
    pub fn string(&self, lang: &str, key: &str) -> Option<&str> {
        self.chain(lang).find_map(|t| t.string(key))
    }

    /// A wave's subtitle lines in `lang`, else in `INT`.
    #[must_use]
    pub fn subtitles(&self, lang: &str, wave: &str) -> Option<&[SubtitleLine]> {
        self.chain(lang).find_map(|t| t.subtitles(wave))
    }

    /// An achievement's texts in `lang`, else in `INT`.
    #[must_use]
    pub fn achievement(&self, lang: &str, api_name: &str) -> Option<&AchievementText> {
        self.chain(lang).find_map(|t| t.achievement(api_name))
    }

    /// The display name of language `of` as the menu of language `lang`
    /// shows it: `ASAMUSettingsManager.SupportedLanguages[i]` for the menu
    /// index `i` of `of` (the original lists these localized names), else
    /// the English `SupportedLanguagesINT[i]`.
    #[must_use]
    pub fn language_name(&self, lang: &str, of: &str) -> Option<String> {
        let of = normalize_language(of)?;
        let i = self.language_menu.iter().position(|c| *c == of)?;
        self.string(
            lang,
            &format!("ASAMUSettingsManager.SupportedLanguages[{i}]"),
        )
        .or_else(|| {
            self.string(
                FALLBACK_LANGUAGE,
                &format!("ASAMUSettingsManager.SupportedLanguagesINT[{i}]"),
            )
        })
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty() && s != MENU_SEPARATOR)
    }

    /// The credits, when converted.
    #[must_use]
    pub fn credits(&self) -> Option<&Credits> {
        self.credits.as_ref()
    }

    /// Replaces the lines of every track that `lang`'s table has (keeping
    /// duration and flags) — for the audio module, so subtitles keep their
    /// own timing in the chosen language. Returns the number of tracks
    /// changed. Waves the table lacks keep their lines, as the original's
    /// cooked waves keep their `INT` lines.
    pub fn localize_tracks(
        &self,
        lang: &str,
        tracks: &mut BTreeMap<String, SubtitleTrack>,
    ) -> usize {
        let Some(table) = self.tables.get(lang) else {
            return 0;
        };
        let mut n = 0;
        for (wave, track) in tracks.iter_mut() {
            if let Some(lines) = table.subtitles(wave)
                && track.lines.as_slice() != lines
            {
                track.lines = lines.to_vec();
                n += 1;
            }
        }
        n
    }

    /// A text-to-text map that turns a subtitle line of any language (the
    /// audio export's language, usually `INT`) into `lang`'s line for the
    /// same wave. Lists of equal length pair line by line; otherwise each
    /// source line maps to the target lines that start within its time
    /// window (joined), or the target line showing at its start (ours: an
    /// approximation for the few waves whose translations split lines
    /// differently).
    ///
    /// The first mapping of a text wins: `INT` lines first, then the chosen
    /// language's own lines (a line already in that language is shown as it
    /// is, whatever another language pairs the same words with), then the
    /// other languages. One text can only map to one line: where a language
    /// translates two occurrences of the same `INT` line differently, the
    /// first translation shows for both.
    #[must_use]
    pub fn subtitle_translation(&self, lang: &str) -> SubtitleTranslation {
        let mut map: HashMap<String, String> = HashMap::new();
        let mut sources: Vec<&LanguageTable> = Vec::new();
        for code in [FALLBACK_LANGUAGE, lang] {
            if let Some(table) = self.tables.get(code)
                && !sources.iter().any(|s| s.language == table.language)
            {
                sources.push(table);
            }
        }
        sources.extend(
            self.tables
                .values()
                .filter(|t| t.language != FALLBACK_LANGUAGE && t.language != lang),
        );
        for src in sources {
            for (wave, src_lines) in src.subtitled_waves() {
                let Some(dst) = self.subtitles(lang, wave) else {
                    continue;
                };
                for (from, to) in align_lines(src_lines, dst) {
                    if !from.is_empty() {
                        map.entry(from.to_owned()).or_insert(to);
                    }
                }
            }
        }
        // Lines that stay as they are need no entry.
        map.retain(|from, to| from != to);
        SubtitleTranslation { map }
    }
}

/// Pairs each non-empty source line with its target text (see
/// [`Localization::subtitle_translation`]).
fn align_lines<'a>(src: &'a [SubtitleLine], dst: &[SubtitleLine]) -> Vec<(&'a str, String)> {
    if src.len() == dst.len() {
        return src
            .iter()
            .zip(dst)
            .map(|(s, d)| (s.text.as_str(), d.text.clone()))
            .collect();
    }
    let mut out = Vec::new();
    for (i, s) in src.iter().enumerate() {
        let start = s.time;
        let end = src.get(i + 1).map_or(f32::INFINITY, |n| n.time);
        let inside: Vec<&str> = dst
            .iter()
            .filter(|d| d.time >= start && d.time < end && !d.text.is_empty())
            .map(|d| d.text.as_str())
            .collect();
        let text = if inside.is_empty() {
            dst.iter()
                .rev()
                .find(|d| d.time <= start)
                .or_else(|| dst.first())
                .map(|d| d.text.clone())
                .unwrap_or_default()
        } else {
            inside.join(" ")
        };
        out.push((s.text.as_str(), text));
    }
    out
}

/// Subtitle text in another language → the chosen language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubtitleTranslation {
    map: HashMap<String, String>,
}

impl SubtitleTranslation {
    /// The line in the chosen language (the input when unknown).
    #[must_use]
    pub fn translate<'a>(&'a self, text: &'a str) -> &'a str {
        self.map.get(text).map_or(text, String::as_str)
    }

    /// Number of known lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// No mapping (same language, or nothing converted).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A localized string for display: a `\n` escape still written out becomes
/// a line break. The original's config reader turns the escape into a line
/// break when it reads a quoted value (CONFIRMED, native; LOCALIZATION.md
/// §2), and `asamu-import localization` does the same, so converted strings
/// already hold line breaks; this keeps tables written by an older converter
/// and hand-made texts working.
#[must_use]
pub fn display_text(raw: &str) -> String {
    raw.replace("\\n", "\n")
}

/// Key names of the original's default bindings for the tutorial texts'
/// placeholders (`ASAMUHUDMovie.ReplacePartWithKey`, keyboard mode): the first
/// non-gamepad binding of each command in the shipped `DefaultInput.ini`
/// (CONFIRMED (config + src)). `#MOUSE#` becomes the localized
/// `ASAMUHUDMovie.MouseLabel` instead.
pub const DEFAULT_KEY_NAMES: [(&str, &str); 5] = [
    ("#MOVE#", "W"),
    ("#SPACE#", "SpaceBar"),
    ("#SHIFT#", "LeftShift"),
    ("#RMB#", "RightMouseButton"),
    ("#LMB#", "LeftMouseButton"),
];

/// Replaces the tutorial placeholders: `#MOVE#` `#SPACE#` `#SHIFT#` `#RMB#`
/// `#LMB#` with `[<key>]` (the original's bracketed key name) and `#MOUSE#`
/// with `mouse_label`. Case-sensitive, like the original's `Repl(.., false)`.
#[must_use]
pub fn expand_key_placeholders(text: &str, keys: &[(&str, &str)], mouse_label: &str) -> String {
    let mut out = text.replace("#MOUSE#", mouse_label);
    for (placeholder, key) in keys {
        out = out.replace(placeholder, &format!("[{key}]"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, time: f32) -> SubtitleLine {
        SubtitleLine {
            text: text.to_owned(),
            time,
        }
    }

    /// Two synthetic languages: "INT" and "TST".
    fn synthetic() -> Localization {
        let mut int = LanguageTable::new("INT");
        int.insert_string("Menu.Yes", "yes-int");
        int.insert_string("Menu.OnlyInt", "only-int");
        int.insert_string("ASAMUSettingsManager.SupportedLanguagesINT[1]", "Int-name");
        int.insert_string("ASAMUSettingsManager.SupportedLanguagesINT[2]", "Tst-name");
        int.insert_subtitles("P.G.Same", vec![line("a1", 0.0), line("a2", 2.0)]);
        int.insert_subtitles(
            "P.G.Split",
            vec![line("s1", 0.0), line("s2", 3.0), line("", 6.0)],
        );
        int.insert_subtitles("P.G.IntOnly", vec![line("i1", 0.0)]);
        int.insert_achievement(
            "TEST_ONE",
            AchievementText {
                name: "First".into(),
                description: "d".into(),
                hidden: false,
            },
        );
        let mut tst = LanguageTable::new("TST");
        tst.insert_string("menu.yes", "yes-tst");
        tst.insert_string("ASAMUSettingsManager.SupportedLanguages[2]", "Tst-own");
        tst.insert_subtitles("P.G.Same", vec![line("b1", 0.0), line("b2", 2.0)]);
        tst.insert_subtitles(
            "P.G.Split",
            vec![
                line("t1", 0.0),
                line("t2a", 3.0),
                line("t2b", 4.5),
                line("", 6.0),
            ],
        );
        Localization::from_tables(vec![tst, int])
            .with_language_menu(&["---", "INT", "TST", "XYZ"])
            .with_default_language("tst")
    }

    #[test]
    fn lookups_fall_back_to_int_and_ignore_key_case() {
        let l = synthetic();
        assert_eq!(l.string("TST", "Menu.Yes"), Some("yes-tst"));
        assert_eq!(l.string("TST", "MENU.YES"), Some("yes-tst"));
        assert_eq!(l.string("TST", "Menu.OnlyInt"), Some("only-int"));
        assert_eq!(l.string("INT", "Menu.Yes"), Some("yes-int"));
        assert_eq!(
            l.string("XYZ", "Menu.Yes"),
            Some("yes-int"),
            "unknown language"
        );
        assert_eq!(l.string("TST", "Menu.Missing"), None);
        assert_eq!(l.subtitles("TST", "P.G.IntOnly").map(<[_]>::len), Some(1));
        assert_eq!(
            l.achievement("TST", "TEST_ONE").map(|a| a.name.as_str()),
            Some("First")
        );
        assert_eq!(l.languages(), vec!["INT", "TST"]);
        assert!(l.has_language("tst"));
        assert!(!l.has_language("XYZ"));
        assert_eq!(l.default_language(), "TST");
        assert_eq!(l.resolve_language(Some("int")), "INT");
        assert_eq!(l.resolve_language(Some("XYZ")), "TST");
        assert_eq!(l.resolve_language(Some("not a code")), "TST");
        assert_eq!(l.resolve_language(None), "TST");
        assert_eq!(l.language_name("TST", "TST").as_deref(), Some("Tst-own"));
        assert_eq!(l.language_name("INT", "INT").as_deref(), Some("Int-name"));
        assert_eq!(l.language_name("TST", "INT").as_deref(), Some("Int-name"));
        assert_eq!(l.language_name("TST", "NOPE"), None);
        let empty = Localization::default();
        assert_eq!(empty.default_language(), "INT");
        assert!(empty.languages().is_empty());
    }

    #[test]
    fn subtitles_translate_line_by_line_or_by_time_window() {
        let l = synthetic();
        let tr = l.subtitle_translation("TST");
        assert_eq!(tr.translate("a1"), "b1");
        assert_eq!(tr.translate("a2"), "b2");
        assert_eq!(tr.translate("s1"), "t1");
        assert_eq!(tr.translate("s2"), "t2a t2b", "split line joined");
        assert_eq!(tr.translate("i1"), "i1", "no TST lines: unchanged");
        assert_eq!(tr.translate("unknown"), "unknown");
        // Back to INT from TST lines (audio converted in another language).
        let back = l.subtitle_translation("INT");
        assert_eq!(back.translate("b2"), "a2");
        assert_eq!(back.translate("t2a"), "s2");
        assert!(l.subtitle_translation("XYZ").translate("a1") == "a1");
        assert!(!tr.is_empty());
    }

    #[test]
    fn tracks_take_the_languages_lines_with_their_timing() {
        let l = synthetic();
        let mut tracks = BTreeMap::new();
        for w in ["P.G.Split", "P.G.IntOnly"] {
            tracks.insert(
                w.to_owned(),
                SubtitleTrack {
                    lines: l.subtitles("INT", w).unwrap().to_vec(),
                    duration: Some(7.0),
                    ..SubtitleTrack::default()
                },
            );
        }
        assert_eq!(l.localize_tracks("TST", &mut tracks), 1);
        let split = &tracks["P.G.Split"];
        assert_eq!(split.lines.len(), 4);
        assert_eq!(split.lines[2], line("t2b", 4.5));
        assert_eq!(split.duration, Some(7.0));
        assert_eq!(tracks["P.G.IntOnly"].lines[0].text, "i1");
        assert_eq!(l.localize_tracks("XYZ", &mut tracks), 0);
    }

    #[test]
    fn placeholders_and_line_breaks() {
        assert_eq!(display_text("a\\nb\\n"), "a\nb\n");
        assert_eq!(
            expand_key_placeholders(
                "#MOVE# / #MOUSE# / #SPACE##LMB# #rmb#",
                &DEFAULT_KEY_NAMES,
                "the rodent"
            ),
            "[W] / the rodent / [SpaceBar][LeftMouseButton] #rmb#"
        );
        assert_eq!(normalize_language(" deu "), Some("DEU".to_owned()));
        assert_eq!(normalize_language("D"), None);
        assert_eq!(normalize_language("../x"), None);
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn converted_tables_load_with_bounds_and_warnings() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            Localization::load(root.path()).unwrap(),
            None,
            "not converted"
        );
        let dir = root.path().join(LOCALIZATION_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "manifest.json",
            r#"{"format": "asamu-localization", "version": 1,
                "languages": [{"code": "INT", "file": "INT.json"},
                              {"code": "TST", "file": "TST.json"},
                              {"code": "BAD", "file": "../escape.json"},
                              {"code": "MIS", "file": "MIS.json"}],
                "language_menu": ["---", "INT", "TST", "../"],
                "steam_language": "english", "default_language": "TST",
                "credits": "credits.json"}"#,
        );
        write(
            &dir,
            "INT.json",
            r#"{"format": "asamu-localization-table", "version": 1, "language": "INT",
                "strings": {"Menu.Yes": "Yes"},
                "subtitles": {"P.W": {"lines": [{"text": "Hi", "time": 0.0}]}},
                "achievements": {"TEST_ONE": {"name": "First", "description": "d", "hidden": true}}}"#,
        );
        write(
            &dir,
            "TST.json",
            r#"{"format": "asamu-localization-table", "version": 1, "language": "OTH"}"#,
        );
        write(
            &dir,
            "credits.json",
            r#"{"format": "asamu-localization-credits", "version": 1,
                "rows": [{"y": 10, "cells": [{"x": -5, "text": "Role", "align": "right"},
                                             {"x": 5, "text": "Name"}]}]}"#,
        );
        let l = Localization::load(root.path()).unwrap().unwrap();
        assert_eq!(l.languages(), vec!["INT"]);
        assert_eq!(l.warnings().len(), 3, "{:?}", l.warnings());
        assert_eq!(l.steam_language(), Some("english"));
        assert_eq!(l.default_language(), "INT", "TST did not load");
        assert_eq!(l.string("INT", "Menu.Yes"), Some("Yes"));
        assert!(l.achievement("INT", "TEST_ONE").unwrap().hidden);
        assert_eq!(
            l.credits().unwrap().rows[0].line(" \u{2014} "),
            "Role \u{2014} Name"
        );
        // A wrong manifest format is an error; a malformed table only a warning.
        write(
            &dir,
            "manifest.json",
            r#"{"format": "other", "version": 1}"#,
        );
        assert!(Localization::load(root.path()).is_err());
        write(
            &dir,
            "manifest.json",
            r#"{"format": "asamu-localization", "version": 1,
                "languages": [{"code": "INT", "file": "INT.json"}]}"#,
        );
        write(&dir, "INT.json", "{oops");
        let l = Localization::load(root.path()).unwrap().unwrap();
        assert!(l.languages().is_empty());
        assert_eq!(l.warnings().len(), 1);
        write(&dir, "manifest.json", "[");
        assert!(Localization::load(root.path()).is_err());
    }

    #[test]
    fn table_json_is_bounded_and_validated() {
        let long = "x".repeat(MAX_CHARS + 10);
        let doc = format!(
            r#"{{"format": "asamu-localization-table", "version": 1, "language": "tst",
                "strings": {{"K.A": "{long}"}},
                "subtitles": {{"W": {{"lines": [{{"text": "t"}}]}}}}}}"#
        );
        let t = LanguageTable::from_json(Path::new("t.json"), doc.as_bytes()).unwrap();
        assert_eq!(t.language, "TST");
        assert_eq!(t.string("K.A").unwrap().chars().count(), MAX_CHARS);
        assert_eq!(t.subtitles("W").unwrap()[0].time, 0.0);
        assert_eq!(t.string_count(), 1);
        assert_eq!(t.keys().collect::<Vec<_>>(), vec!["K.A"]);
        assert!(
            LanguageTable::from_json(
                Path::new("t.json"),
                br#"{"format": "asamu-localization-table", "version": 2, "language": "INT"}"#
            )
            .is_err()
        );
        assert!(
            LanguageTable::from_json(
                Path::new("t.json"),
                br#"{"format": "asamu-localization-table", "version": 1, "language": "1"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_tables_and_credits_are_refused() {
        let table = |body: &str| {
            let doc = format!(
                r#"{{"format": "asamu-localization-table", "version": 1, "language": "TST", {body}}}"#
            );
            LanguageTable::from_json(Path::new("t.json"), doc.as_bytes())
        };
        let entries = |n: usize, value: &str| {
            (0..n)
                .map(|i| format!(r#""K{i}": {value}"#))
                .collect::<Vec<_>>()
                .join(",")
        };
        let lines = |n: usize| vec![r#"{"text": "t", "time": 1.0}"#; n].join(",");
        // At the limits a table loads; one more entry is an error value.
        let strings = |n| table(&format!(r#""strings": {{{}}}"#, entries(n, "\"v\"")));
        assert_eq!(strings(MAX_STRINGS).unwrap().string_count(), MAX_STRINGS);
        assert!(strings(MAX_STRINGS + 1).is_err());
        let wave = |n| {
            table(&format!(
                r#""subtitles": {{"W": {{"lines": [{}]}}}}"#,
                lines(n)
            ))
        };
        assert_eq!(
            wave(MAX_WAVE_LINES).unwrap().subtitles("W").map(<[_]>::len),
            Some(MAX_WAVE_LINES)
        );
        assert!(wave(MAX_WAVE_LINES + 1).is_err());
        let per_wave = format!(r#"{{"lines": [{}]}}"#, lines(MAX_WAVE_LINES));
        let waves = |n| table(&format!(r#""subtitles": {{{}}}"#, entries(n, &per_wave)));
        let most = MAX_SUBTITLE_LINES / MAX_WAVE_LINES;
        assert!(waves(most).is_ok());
        assert!(waves(most + 1).is_err(), "too many lines in all");
        let empty_wave = r#"{"lines": []}"#;
        assert!(
            table(&format!(
                r#""subtitles": {{{}}}"#,
                entries(MAX_WAVES + 1, empty_wave)
            ))
            .is_err()
        );
        let achievement = r#"{"name": "n"}"#;
        assert!(
            table(&format!(
                r#""achievements": {{{}}}"#,
                entries(MAX_ACHIEVEMENTS + 1, achievement)
            ))
            .is_err()
        );
        let long_key = "k".repeat(MAX_KEY_BYTES + 1);
        assert!(table(&format!(r#""strings": {{"{long_key}": "v"}}"#)).is_err());
        assert!(
            table(&format!(
                r#""subtitles": {{"{long_key}": {{"lines": []}}}}"#
            ))
            .is_err()
        );

        // Oversized credits are a warning; the rest still loads.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(LOCALIZATION_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "manifest.json",
            r#"{"format": "asamu-localization", "version": 1, "credits": "credits.json",
                "languages": [{"code": "INT", "file": "INT.json"}]}"#,
        );
        write(
            &dir,
            "INT.json",
            r#"{"format": "asamu-localization-table", "version": 1, "language": "INT"}"#,
        );
        let credits = |rows: usize, cells: usize| {
            let cell = r#"{"x": 1, "text": "c"}"#;
            let row = format!(r#"{{"y": 1, "cells": [{}]}}"#, vec![cell; cells].join(","));
            format!(
                r#"{{"format": "asamu-localization-credits", "version": 1, "rows": [{}]}}"#,
                vec![row; rows].join(",")
            )
        };
        for (rows, cells, ok) in [
            (MAX_CREDITS_ROWS, MAX_CREDITS_CELLS, true),
            (MAX_CREDITS_ROWS + 1, 1, false),
            (1, MAX_CREDITS_CELLS + 1, false),
        ] {
            write(&dir, "credits.json", &credits(rows, cells));
            let l = Localization::load(root.path()).unwrap().unwrap();
            assert_eq!(l.credits().is_some(), ok, "{rows} rows of {cells} cells");
            assert_eq!(l.warnings().is_empty(), ok);
            assert_eq!(l.languages(), vec!["INT"]);
        }
    }

    #[test]
    fn menu_positions_wave_case_and_missing_fallback_hold() {
        // A repeated menu entry lists its language once; an entry that is no
        // language keeps its position, so the name lists' indexes stay right.
        let mut int = LanguageTable::new("INT");
        int.insert_string("ASAMUSettingsManager.SupportedLanguages[3]", "Third");
        int.insert_string("ASAMUSettingsManager.SupportedLanguages[0]", "---");
        int.insert_subtitles("Pkg.Grp.Wave", vec![line("a", 0.0)]);
        let mut tst = LanguageTable::new("TST");
        tst.insert_subtitles("pkg.GRP.wave", vec![line("b", 0.0)]);
        let l = Localization::from_tables(vec![int, tst.clone()]).with_language_menu(&[
            "---",
            "INT",
            "not a code",
            "TST",
            "INT",
            "TST",
        ]);
        assert_eq!(l.languages(), vec!["INT", "TST"]);
        assert_eq!(l.language_name("INT", "tst").as_deref(), Some("Third"));
        assert_eq!(
            l.language_name("INT", "---"),
            None,
            "a separator is no language"
        );
        assert_eq!(l.language_name("INT", "INT"), None, "no name at its index");
        // Wave paths compare without case, between tables and in lookups.
        assert_eq!(
            l.subtitles("TST", "PKG.GRP.WAVE")
                .map(|s| s[0].text.as_str()),
            Some("b")
        );
        assert_eq!(l.subtitle_translation("TST").translate("a"), "b");
        let mut tracks = BTreeMap::new();
        tracks.insert(
            "Pkg.Grp.Wave".to_owned(),
            SubtitleTrack {
                lines: vec![line("a", 0.0)],
                ..SubtitleTrack::default()
            },
        );
        assert_eq!(l.localize_tracks("TST", &mut tracks), 1);
        // The same wave inserted under another spelling replaces the first.
        tst.insert_subtitles("PKG.grp.WAVE", vec![line("c", 0.0)]);
        assert_eq!(tst.subtitled_waves().count(), 1);
        assert_eq!(
            tst.subtitles("Pkg.Grp.Wave").map(|s| s[0].text.as_str()),
            Some("c")
        );

        // Without an INT table nothing falls back, and the first table is
        // the default.
        let mut only = LanguageTable::new("TST");
        only.insert_string("Menu.Yes", "yes-tst");
        let l = Localization::from_tables(vec![only]).with_default_language("INT");
        assert_eq!(l.default_language(), "TST");
        assert_eq!(l.resolve_language(Some("INT")), "TST");
        assert_eq!(l.string("TST", "Menu.Yes"), Some("yes-tst"));
        assert_eq!(l.string("INT", "Menu.Yes"), None);
        assert_eq!(l.string("XYZ", "Menu.Yes"), None);
        assert!(l.subtitle_translation("INT").is_empty());
        assert!(l.subtitle_translation("TST").is_empty());
    }

    /// A line that is already in the chosen language (or an `INT` line the
    /// language keeps as it is) is never replaced because another language
    /// happens to use the same words elsewhere.
    #[test]
    fn lines_of_the_chosen_language_are_not_remapped() {
        let mut int = LanguageTable::new("INT");
        int.insert_subtitles("P.A", vec![line("same", 0.0)]);
        int.insert_subtitles("P.B", vec![line("b-int", 0.0)]);
        let mut tst = LanguageTable::new("TST");
        tst.insert_subtitles("P.A", vec![line("same", 0.0)]);
        tst.insert_subtitles("P.B", vec![line("b-tst", 0.0)]);
        // A third language whose line for wave B reads like wave A's.
        let mut oth = LanguageTable::new("OTH");
        oth.insert_subtitles("P.A", vec![line("a-oth", 0.0)]);
        oth.insert_subtitles("P.B", vec![line("same", 0.0)]);
        let l = Localization::from_tables(vec![oth, tst, int]);
        let to_tst = l.subtitle_translation("TST");
        assert_eq!(to_tst.translate("same"), "same");
        assert_eq!(to_tst.translate("b-int"), "b-tst");
        assert_eq!(to_tst.translate("a-oth"), "same");
        let to_int = l.subtitle_translation("INT");
        assert_eq!(to_int.translate("same"), "same");
        assert_eq!(to_int.translate("b-tst"), "b-int");
        assert_eq!(to_int.len(), 2, "b-tst and a-oth; no identity entries");
        // The chosen language's own lines win over a third language's, too.
        let to_oth = l.subtitle_translation("OTH");
        assert_eq!(to_oth.translate("a-oth"), "a-oth");
        assert_eq!(to_oth.translate("b-int"), "same");
        assert_eq!(to_oth.translate("same"), "a-oth", "the INT line of wave A");
    }

    /// The translation of the largest tables a load accepts stays cheap, and
    /// odd times (unsorted, not finite) never panic.
    #[test]
    fn subtitle_translation_is_bounded_and_total() {
        let table = |code: &str, lines_per_wave: usize| {
            let mut t = LanguageTable::new(code);
            for w in 0..MAX_SUBTITLE_LINES / MAX_WAVE_LINES {
                let lines = (0..lines_per_wave)
                    .map(|i| line(&format!("{code}-{w}-{i}"), (i % 7) as f32))
                    .collect();
                t.insert_subtitles(&format!("P.W{w}"), lines);
            }
            t
        };
        // Different line counts force the time-window pairing on every wave.
        let l = Localization::from_tables(vec![
            table("INT", MAX_WAVE_LINES),
            table("TST", MAX_WAVE_LINES - 1),
        ]);
        let tr = l.subtitle_translation("TST");
        assert_eq!(tr.len(), MAX_SUBTITLE_LINES);
        assert!(tr.translate("INT-0-0").starts_with("TST-0-"));

        let mut int = LanguageTable::new("INT");
        int.insert_subtitles(
            "P.W",
            vec![
                line("a", f32::NAN),
                line("b", f32::INFINITY),
                line("c", -1.0),
            ],
        );
        let mut tst = LanguageTable::new("TST");
        tst.insert_subtitles("P.W", vec![line("x", f32::NAN), line("y", 5.0)]);
        tst.insert_subtitles("P.Empty", Vec::new());
        int.insert_subtitles("P.Empty", vec![line("lonely", 0.0)]);
        let l = Localization::from_tables(vec![int, tst]);
        let tr = l.subtitle_translation("TST");
        assert_eq!(
            tr.translate("lonely"),
            "",
            "the language shows nothing there"
        );
        assert!(!tr.translate("a").is_empty());
        let _ = l.subtitle_translation("INT");
    }

    /// Real converted data (skipped unless `ASAMU_CONVERTED_DIR` holds an
    /// `asamu-import localization` output): every language loads, the
    /// tutorial strings resolve, and subtitles translate. Counts only.
    #[test]
    fn converted_localization_loads() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let Some(l) = Localization::load(Path::new(&dir)).unwrap() else {
            eprintln!("skipping: no converted localization");
            return;
        };
        assert!(l.warnings().is_empty(), "{:?}", l.warnings());
        for lang in l.languages() {
            assert!(
                l.string(lang, "ASAMUHUD.Tutorial_UseWASD").is_some(),
                "{lang}"
            );
            assert!(l.language_name(lang, lang).is_some(), "{lang}");
            let tr = l.subtitle_translation(lang);
            eprintln!("{lang}: {} translated subtitle lines", tr.len());
        }
    }
}
