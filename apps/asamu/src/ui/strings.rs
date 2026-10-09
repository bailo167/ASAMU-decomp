//! Menu text. The defaults are ours (English, written for this project);
//! nothing here is the original's text. A user's converted directory may
//! carry `ui/strings.json` — produced on the user's machine from their own
//! install's localization files — whose entries override the defaults by
//! key (e.g. `chapter.Sanctuary` for a chapter's display title). That file
//! is user-local data and never enters the repository.
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
    overrides: BTreeMap<String, String>,
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
        Ok(Self { overrides })
    }

    /// The text for `key`, or `default`.
    #[must_use]
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.overrides.get(key).map_or(default, String::as_str)
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
        assert!(UiStrings::parse("{}").is_err());
        assert!(UiStrings::parse("[").is_err());
        assert!(
            UiStrings::parse(r#"{"format": "asamu-decomp/ui-strings", "format_version": 2}"#)
                .is_err()
        );
    }
}
