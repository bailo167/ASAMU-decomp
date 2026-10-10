//! Menu text. The defaults are ours (English, written for this project);
//! nothing here is the original's text. Two user-local layers override them
//! by key (e.g. `chapter.Sanctuary` for a chapter's display title):
//!
//! 1. the localized layer, filled by [`super::locale`] from the tables
//!    `asamu-import localization` converted from the user's own install, in
//!    the chosen language (falling back to `INT`);
//! 2. on top, an optional `<converted>/ui/strings.json` (hand-made
//!    overrides).
//!
//! Both are user-local data and never enter the repository.
//!
//! Format: `{"format": "asamu-decomp/ui-strings", "format_version": 1,
//! "strings": {"<key>": "<text>", ...}}`; unknown keys are ignored, entries
//! are bounded in count and length, and a malformed file is ignored with a
//! warning.

use std::collections::BTreeMap;
use std::path::Path;

use asamu_game::save::ChapterId;
use asamu_game::save::json::{self, Value};
use bevy::prelude::*;

/// Largest strings file read (bytes).
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Most entries kept.
const MAX_ENTRIES: usize = 2048;
/// Longest entry kept (characters).
const MAX_CHARS: usize = 256;

/// Text overrides by key.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UiStrings {
    /// `ui/strings.json` (wins over everything).
    overrides: BTreeMap<String, String>,
    /// The chosen language's texts ([`super::locale`]).
    localized: BTreeMap<String, String>,
}

impl UiStrings {
    /// Loads `<converted>/ui/strings.json` if present.
    #[must_use]
    pub fn load(converted_root: &Path) -> Self {
        let path = converted_root.join("ui").join("strings.json");
        let Ok(meta) = std::fs::metadata(&path) else {
            return Self::default();
        };
        if meta.len() > MAX_FILE_BYTES {
            warn!("{} is too large; ignored", path.display());
            return Self::default();
        }
        match read_bounded_text(&path).and_then(|t| Self::parse(&t)) {
            Ok(s) => {
                info!(
                    "menu text: {} entries from {}",
                    s.overrides.len(),
                    path.display()
                );
                s
            }
            Err(e) => {
                warn!("{} ignored: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Parses the strings document.
    ///
    /// # Errors
    /// Malformed JSON or a wrong tag/version.
    pub fn parse(text: &str) -> Result<Self, String> {
        let v = json::parse(text).map_err(|e| e.to_string())?;
        if v.get("format").and_then(Value::as_str) != Some("asamu-decomp/ui-strings") {
            return Err("not an asamu-decomp/ui-strings document".to_owned());
        }
        if v.get("format_version").and_then(Value::as_u32) != Some(1) {
            return Err("unsupported format_version".to_owned());
        }
        let mut overrides = BTreeMap::new();
        if let Some(map) = v.get("strings").and_then(Value::as_object) {
            for (k, text) in map.iter().take(MAX_ENTRIES) {
                if let Some(t) = text.as_str() {
                    let t: String = t
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(MAX_CHARS)
                        .collect();
                    if !t.trim().is_empty() {
                        overrides.insert(k.clone(), t);
                    }
                }
            }
        }
        Ok(Self {
            overrides,
            localized: BTreeMap::new(),
        })
    }

    /// The text for `key`: the user override, else the localized text, else
    /// `default`.
    #[must_use]
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.overrides
            .get(key)
            .or_else(|| self.localized.get(key))
            .map_or(default, String::as_str)
    }

    /// The text for `key` when a layer has it.
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<&str> {
        self.overrides
            .get(key)
            .or_else(|| self.localized.get(key))
            .map(String::as_str)
    }

    /// The first key starting with `prefix` whose text is exactly `text`
    /// (overrides first), for diagnostics.
    #[must_use]
    pub fn key_with_text(&self, prefix: &str, text: &str) -> Option<&str> {
        self.overrides
            .iter()
            .chain(&self.localized)
            .find(|(k, v)| k.starts_with(prefix) && v.as_str() == text)
            .map(|(k, _)| k.as_str())
    }

    /// Replaces the localized layer (the overrides stay on top).
    pub fn set_localized(&mut self, localized: BTreeMap<String, String>) {
        self.localized = localized;
    }

    /// Keeps the localized layer of `previous` (used when the overrides
    /// file is reloaded).
    #[must_use]
    pub fn with_localized_from(mut self, previous: &Self) -> Self {
        self.localized.clone_from(&previous.localized);
        self
    }

    /// An achievement's display name (`achievement.<NAME>`; default: the
    /// enumerator name as words, ours).
    #[must_use]
    pub fn achievement_title(&self, achievement: asamu_game::save::Achievement) -> String {
        match self.lookup(&format!("achievement.{}", achievement.name())) {
            Some(t) => t.to_owned(),
            None => achievement_words(achievement),
        }
    }

    /// A chapter's display title (`chapter.<EnumName>`; default: the
    /// `ASAMULevels` enumerator name).
    #[must_use]
    pub fn chapter_title(&self, chapter: ChapterId) -> String {
        self.get(
            &format!("chapter.{}", chapter.enum_name()),
            chapter.enum_name(),
        )
        .to_owned()
    }
}

/// `FLOOR_IS_LAVA` → `Floor is lava` (ours).
#[must_use]
pub(crate) fn achievement_words(a: asamu_game::save::Achievement) -> String {
    let lower = a.name().replace('_', " ").to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The file as UTF-8 text, refusing more than [`MAX_FILE_BYTES`] even when
/// it grew after the size check.
fn read_bounded_text(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("too large".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_file_is_read_from_the_converted_dir_and_bounded() {
        let dir = std::env::temp_dir().join(format!("asamu-ui-strings-{}", std::process::id()));
        let ui = dir.join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        assert_eq!(UiStrings::load(&dir), UiStrings::default(), "no file");
        std::fs::write(
            ui.join("strings.json"),
            r#"{"format": "asamu-decomp/ui-strings", "format_version": 1,
                "strings": {"menu.quit": "Leave"}}"#,
        )
        .unwrap();
        assert_eq!(UiStrings::load(&dir).get("menu.quit", "Quit"), "Leave");
        // Oversized (sparse) and non-UTF-8 files are ignored.
        std::fs::File::create(ui.join("strings.json"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert_eq!(UiStrings::load(&dir), UiStrings::default());
        std::fs::write(ui.join("strings.json"), [0xFF, 0xFE]).unwrap();
        assert_eq!(UiStrings::load(&dir), UiStrings::default());
        assert!(read_bounded_text(&ui.join("strings.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_apply_by_key_and_bad_files_are_rejected() {
        let s = UiStrings::parse(
            r#"{"format": "asamu-decomp/ui-strings", "format_version": 1,
                "strings": {"chapter.Sanctuary": "Title\u0007 X", "menu.quit": "  ", "x": 3}}"#,
        )
        .unwrap();
        assert_eq!(s.chapter_title(ChapterId::Sanctuary), "Title X");
        assert_eq!(s.chapter_title(ChapterId::Village), "Village");
        assert_eq!(s.get("menu.quit", "Quit"), "Quit", "blank entries ignored");
        // The localized layer sits under the overrides.
        let mut s = s;
        s.set_localized(
            [
                ("chapter.Village".to_owned(), "Loc village".to_owned()),
                ("chapter.Sanctuary".to_owned(), "Loc sanctuary".to_owned()),
                (
                    "achievement.FLOOR_IS_LAVA".to_owned(),
                    "Loc lava".to_owned(),
                ),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(s.chapter_title(ChapterId::Village), "Loc village");
        assert_eq!(
            s.chapter_title(ChapterId::Sanctuary),
            "Title X",
            "override wins"
        );
        assert_eq!(
            s.achievement_title(asamu_game::save::Achievement::FLOOR_IS_LAVA),
            "Loc lava"
        );
        assert_eq!(
            s.achievement_title(asamu_game::save::Achievement::ALL_COLLECTIBLES_FOUND),
            "All collectibles found"
        );
        assert_eq!(
            s.key_with_text("chapter.", "Loc village"),
            Some("chapter.Village")
        );
        assert_eq!(
            s.key_with_text("chapter.", "Title X"),
            Some("chapter.Sanctuary"),
            "an override"
        );
        assert_eq!(s.key_with_text("menu.", "Loc village"), None);
        assert_eq!(s.key_with_text("chapter.", "unknown"), None);
        let reloaded = UiStrings::default().with_localized_from(&s);
        assert_eq!(reloaded.chapter_title(ChapterId::Village), "Loc village");
        assert_eq!(reloaded.lookup("menu.quit"), None);
        assert!(UiStrings::parse("{}").is_err());
        assert!(UiStrings::parse("[").is_err());
        assert!(
            UiStrings::parse(r#"{"format": "asamu-decomp/ui-strings", "format_version": 2}"#)
                .is_err()
        );
    }
}
