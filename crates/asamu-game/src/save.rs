//! Save system: the original's save **semantics** in **our** format
//! (`docs/reverse-engineering/SAVE.md`, design in `docs/UI_AND_SAVES.md`).
//!
//! The original keeps four encrypted UE3 object files (SAVE.md §2–4). We keep
//! the same four lifetimes as four versioned JSON documents so that "New
//! Game", chapter select, Continue and time trial behave the same, plus a
//! settings document:
//!
//! | Document | Original file | Holds |
//! |---|---|---|
//! | [`Progression`] (`progression.json`) | `PlayerProgression.bin` | chapters entered, collectibles, optional story items, achievements, finished flag |
//! | [`General`] (`general.json`) | `GeneralSave.bin` | chapter pointer (Continue target), Kismet saved strings |
//! | [`Snapshot`] (`snapshot.json`) | `SaveGame.bin` | map, per-chapter latest checkpoint, grapple capacity / boots / grapple latch, savable actor and Kismet state |
//! | [`TimeTrialTimes`] (`time_trial.json`) | `TTS.bin` | best time-trial times |
//! | [`Settings`] (`settings.json`) | user ini files (SAVE.md §7) | FOV, mouse, volumes, window, subtitles |
//!
//! Every document is a JSON object with a `format` tag and a
//! `format_version` ([`FORMAT_VERSION`]), written atomically (temporary file
//! in the same directory, flushed, renamed over the old file) into a
//! user-local directory ([`SaveStore::default_root`]; never the repository,
//! never the original game's folders). An unreadable document (malformed,
//! wrong tag, a newer `format_version`) is **quarantined** by renaming it
//! (`<name>.corrupt-<n>.json`), never silently overwritten; the loader then
//! starts that document from its defaults ([`LoadOutcome::Unreadable`]).
//! Older versions run through an explicit upgrade chain (none exists yet:
//! version 1 is the first format; see [`upgrade_document`]).
//!
//! [`SaveSession`] owns the lifecycle hooks of SAVE.md §5 as plain functions
//! over the model ([`SaveSession::new_game`], [`SaveSession::start_chapter`],
//! [`SaveSession::begin_level`], [`SaveSession::on_checkpoint_saved`],
//! [`SaveSession::on_collectible`], …); [`Game::capture_snapshot`] and
//! [`Game::apply_snapshot`] move the snapshot's state out of and into a
//! running [`Game`]. Everything here is deterministic and render-free.
//!
//! Deliberate differences from the original (data-losing quirks fixed, see
//! SAVE.md §8 and `docs/UI_AND_SAVES.md`): an unreadable document resets only
//! itself (Q2 wiped achievements, story items and the finished flag when
//! either file failed, and again when the snapshot was missing at a story
//! map's start); Continue needs a chapter pointer (Q6); achievements are a
//! set (Q3). Importing the original's own save files is out of scope
//! ([`import_original_saves`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use asamu_player::{grapple_gun, rocket_boots};
use thiserror::Error;

use crate::{Game, apply_level_abilities};

pub use json::Value;

/// Current `format_version` of every document we write.
pub const FORMAT_VERSION: u32 = 1;

/// Largest document we read (bytes); larger files are treated as corrupt.
pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

/// The front-end (main menu) map: `[URL] Map=ASAMUFrontEndMap` in the
/// shipped `DefaultEngine.ini` and `LevelFileNames[NoLevel]` (SAVE.md §1.1,
/// §6.1). CONFIRMED (config, cdo).
pub const FRONT_END_MAP: &str = "ASAMUFrontEndMap";

/// Collectibles per collectible chapter (`ASAMUProgressionManager`
/// `COLLECTIBLES_PER_LEVEL`). CONFIRMED (cdo, map census).
pub const COLLECTIBLES_PER_CHAPTER: usize = 5;

/// Total collectibles (5 chapters × 5). CONFIRMED (cdo, map census).
pub const TOTAL_COLLECTIBLES: usize = 25;

/// Distinct optional story items (`ASAMUProgressionManager`
/// `TOTAL_INTERACTABLES_COUNT`). CONFIRMED (cdo).
pub const TOTAL_STORY_ITEMS: usize = 11;

/// Collectible totals that unlock an extra (beam colour, goat mode, Midas
/// mode, parkour mode) and raise a HUD notice when reached exactly
/// (SAVE.md §6.2). CONFIRMED (src).
pub const EXTRA_THRESHOLDS: [usize; 4] = [10, 15, 20, 25];

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// A small, strict JSON reader and deterministic pretty writer (RFC 8259),
/// used for our save and settings documents.
///
/// Hostile-input discipline: nesting is limited to [`json::MAX_DEPTH`],
/// numbers must be finite, strings must be valid (escapes, surrogate pairs,
/// no raw control characters) and nothing may follow the value. Objects are
/// [`BTreeMap`]s, so output key order is sorted and stable; a repeated key
/// keeps its last value.
pub mod json {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    use thiserror::Error;

    /// Deepest nesting of arrays/objects accepted by [`parse`].
    pub const MAX_DEPTH: usize = 64;

    /// A JSON value.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        /// `null`.
        Null,
        /// `true` / `false`.
        Bool(bool),
        /// A finite number.
        Number(f64),
        /// A string.
        String(String),
        /// An array.
        Array(Vec<Value>),
        /// An object (sorted keys).
        Object(BTreeMap<String, Value>),
    }

    /// Why a text is not accepted as JSON.
    #[derive(Clone, Debug, PartialEq, Eq, Error)]
    pub enum JsonError {
        /// The text ended inside a value.
        #[error("unexpected end of the document")]
        Eof,
        /// A character that cannot start or continue a value here.
        #[error("unexpected character at byte {0}")]
        Unexpected(usize),
        /// A malformed or non-finite number.
        #[error("invalid number at byte {0}")]
        Number(usize),
        /// A malformed string (escape, surrogate, control character).
        #[error("invalid string at byte {0}")]
        String(usize),
        /// Nested deeper than [`MAX_DEPTH`].
        #[error("nested deeper than {MAX_DEPTH} levels")]
        TooDeep,
        /// Characters after the value.
        #[error("trailing characters at byte {0}")]
        Trailing(usize),
        /// A number to write is NaN or infinite.
        #[error("cannot write a non-finite number")]
        NonFinite,
    }

    impl Value {
        /// The member `key` of an object.
        #[must_use]
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Object(m) => m.get(key),
                _ => None,
            }
        }

        /// The number, if this is one.
        #[must_use]
        pub fn as_f64(&self) -> Option<f64> {
            match self {
                Value::Number(n) => Some(*n),
                _ => None,
            }
        }

        /// The number as an `f32`, if it is one and fits.
        #[must_use]
        pub fn as_f32(&self) -> Option<f32> {
            let n = self.as_f64()?;
            let f = n as f32;
            f.is_finite().then_some(f)
        }

        /// The number as an `i64`, if it is an integer within range.
        #[must_use]
        pub fn as_i64(&self) -> Option<i64> {
            let n = self.as_f64()?;
            // 2^63 is exactly representable; anything below it in magnitude
            // with no fraction converts exactly.
            const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
            (n.fract() == 0.0 && n.abs() < TWO_POW_63).then_some(n as i64)
        }

        /// The number as an `i32`, if it is an integer within range.
        #[must_use]
        pub fn as_i32(&self) -> Option<i32> {
            self.as_i64().and_then(|n| i32::try_from(n).ok())
        }

        /// The number as a `u32`, if it is an integer within range.
        #[must_use]
        pub fn as_u32(&self) -> Option<u32> {
            self.as_i64().and_then(|n| u32::try_from(n).ok())
        }

        /// The boolean, if this is one.
        #[must_use]
        pub fn as_bool(&self) -> Option<bool> {
            match self {
                Value::Bool(b) => Some(*b),
                _ => None,
            }
        }

        /// The string, if this is one.
        #[must_use]
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::String(s) => Some(s),
                _ => None,
            }
        }

        /// The elements, if this is an array.
        #[must_use]
        pub fn as_array(&self) -> Option<&[Value]> {
            match self {
                Value::Array(a) => Some(a),
                _ => None,
            }
        }

        /// The members, if this is an object.
        #[must_use]
        pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
            match self {
                Value::Object(m) => Some(m),
                _ => None,
            }
        }

        /// An `f32` as a number that reads back as exactly the same `f32`,
        /// preferring its shortest decimal form (`0.8`, not
        /// `0.800000011920929`).
        #[must_use]
        pub fn from_f32(x: f32) -> Value {
            let short = format!("{x}").parse::<f64>().ok();
            match short {
                Some(s) if s as f32 == x => Value::Number(s),
                _ => Value::Number(f64::from(x)),
            }
        }
    }

    impl From<bool> for Value {
        fn from(b: bool) -> Self {
            Value::Bool(b)
        }
    }

    impl From<i32> for Value {
        fn from(n: i32) -> Self {
            Value::Number(f64::from(n))
        }
    }

    impl From<u32> for Value {
        fn from(n: u32) -> Self {
            Value::Number(f64::from(n))
        }
    }

    impl From<&str> for Value {
        fn from(s: &str) -> Self {
            Value::String(s.to_owned())
        }
    }

    impl From<String> for Value {
        fn from(s: String) -> Self {
            Value::String(s)
        }
    }

    /// Parses a complete JSON text.
    ///
    /// # Errors
    /// Any deviation from the grammar, see [`JsonError`].
    pub fn parse(text: &str) -> Result<Value, JsonError> {
        let mut p = Parser {
            bytes: text.as_bytes(),
            text,
            pos: 0,
        };
        p.skip_ws();
        let v = p.value(0)?;
        p.skip_ws();
        if p.pos != p.bytes.len() {
            return Err(JsonError::Trailing(p.pos));
        }
        Ok(v)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        text: &'a str,
        pos: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn skip_ws(&mut self) {
            while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
                self.pos += 1;
            }
        }

        fn expect_literal(&mut self, word: &str, v: Value) -> Result<Value, JsonError> {
            let end = self.pos.checked_add(word.len()).ok_or(JsonError::Eof)?;
            match self.bytes.get(self.pos..end) {
                Some(s) if s == word.as_bytes() => {
                    self.pos = end;
                    Ok(v)
                }
                Some(_) => Err(JsonError::Unexpected(self.pos)),
                None => Err(JsonError::Eof),
            }
        }

        fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
            match self.peek() {
                None => Err(JsonError::Eof),
                Some(b'n') => self.expect_literal("null", Value::Null),
                Some(b't') => self.expect_literal("true", Value::Bool(true)),
                Some(b'f') => self.expect_literal("false", Value::Bool(false)),
                Some(b'"') => self.string().map(Value::String),
                Some(b'-' | b'0'..=b'9') => self.number(),
                Some(b'[') => {
                    if depth >= MAX_DEPTH {
                        return Err(JsonError::TooDeep);
                    }
                    self.array(depth + 1)
                }
                Some(b'{') => {
                    if depth >= MAX_DEPTH {
                        return Err(JsonError::TooDeep);
                    }
                    self.object(depth + 1)
                }
                Some(_) => Err(JsonError::Unexpected(self.pos)),
            }
        }

        fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
            self.pos += 1; // '['
            let mut items = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            }
            loop {
                self.skip_ws();
                items.push(self.value(depth)?);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b']') => {
                        self.pos += 1;
                        return Ok(Value::Array(items));
                    }
                    Some(_) => return Err(JsonError::Unexpected(self.pos)),
                    None => return Err(JsonError::Eof),
                }
            }
        }

        fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
            self.pos += 1; // '{'
            let mut members = BTreeMap::new();
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Value::Object(members));
            }
            loop {
                self.skip_ws();
                match self.peek() {
                    Some(b'"') => {}
                    Some(_) => return Err(JsonError::Unexpected(self.pos)),
                    None => return Err(JsonError::Eof),
                }
                let key = self.string()?;
                self.skip_ws();
                match self.peek() {
                    Some(b':') => self.pos += 1,
                    Some(_) => return Err(JsonError::Unexpected(self.pos)),
                    None => return Err(JsonError::Eof),
                }
                self.skip_ws();
                let v = self.value(depth)?;
                members.insert(key, v);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        return Ok(Value::Object(members));
                    }
                    Some(_) => return Err(JsonError::Unexpected(self.pos)),
                    None => return Err(JsonError::Eof),
                }
            }
        }

        fn digits(&mut self) -> usize {
            let start = self.pos;
            while let Some(b'0'..=b'9') = self.peek() {
                self.pos += 1;
            }
            self.pos - start
        }

        fn number(&mut self) -> Result<Value, JsonError> {
            let start = self.pos;
            if self.peek() == Some(b'-') {
                self.pos += 1;
            }
            match self.peek() {
                Some(b'0') => self.pos += 1,
                Some(b'1'..=b'9') => {
                    self.digits();
                }
                _ => return Err(JsonError::Number(start)),
            }
            if self.peek() == Some(b'.') {
                self.pos += 1;
                if self.digits() == 0 {
                    return Err(JsonError::Number(start));
                }
            }
            if let Some(b'e' | b'E') = self.peek() {
                self.pos += 1;
                if let Some(b'+' | b'-') = self.peek() {
                    self.pos += 1;
                }
                if self.digits() == 0 {
                    return Err(JsonError::Number(start));
                }
            }
            let slice = self
                .text
                .get(start..self.pos)
                .ok_or(JsonError::Number(start))?;
            match slice.parse::<f64>() {
                Ok(n) if n.is_finite() => Ok(Value::Number(n)),
                _ => Err(JsonError::Number(start)),
            }
        }

        fn hex4(&mut self) -> Result<u32, JsonError> {
            let start = self.pos;
            let end = start.checked_add(4).ok_or(JsonError::Eof)?;
            let digits = self.text.get(start..end).ok_or(JsonError::Eof)?;
            if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(JsonError::String(start));
            }
            self.pos = end;
            u32::from_str_radix(digits, 16).map_err(|_| JsonError::String(start))
        }

        fn string(&mut self) -> Result<String, JsonError> {
            self.pos += 1; // opening quote
            let mut out = String::new();
            loop {
                let run_start = self.pos;
                while let Some(b) = self.peek() {
                    if b == b'"' || b == b'\\' || b < 0x20 {
                        break;
                    }
                    self.pos += 1;
                }
                // Byte positions stop only at ASCII bytes, so the run is a
                // valid `str` slice.
                out.push_str(
                    self.text
                        .get(run_start..self.pos)
                        .ok_or(JsonError::String(run_start))?,
                );
                match self.peek() {
                    None => return Err(JsonError::Eof),
                    Some(b'"') => {
                        self.pos += 1;
                        return Ok(out);
                    }
                    Some(b'\\') => {
                        let at = self.pos;
                        self.pos += 1;
                        let esc = self.peek().ok_or(JsonError::Eof)?;
                        self.pos += 1;
                        match esc {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let hi = self.hex4()?;
                                let code = if (0xD800..0xDC00).contains(&hi) {
                                    // A high surrogate must be followed by
                                    // an escaped low surrogate.
                                    let end = self.pos.checked_add(2).ok_or(JsonError::Eof)?;
                                    if self.bytes.get(self.pos..end) != Some(b"\\u".as_slice()) {
                                        return Err(JsonError::String(at));
                                    }
                                    self.pos = end;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return Err(JsonError::String(at));
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else if (0xDC00..0xE000).contains(&hi) {
                                    return Err(JsonError::String(at));
                                } else {
                                    hi
                                };
                                out.push(char::from_u32(code).ok_or(JsonError::String(at))?);
                            }
                            _ => return Err(JsonError::String(at)),
                        }
                    }
                    Some(_) => return Err(JsonError::String(self.pos)),
                }
            }
        }
    }

    /// Writes `v` as indented JSON (two spaces, sorted keys, trailing
    /// newline).
    ///
    /// # Errors
    /// [`JsonError::NonFinite`] for a NaN or infinite number.
    pub fn to_pretty_string(v: &Value) -> Result<String, JsonError> {
        let mut out = String::new();
        write_value(&mut out, v, 0)?;
        out.push('\n');
        Ok(out)
    }

    fn indent(out: &mut String, level: usize) {
        for _ in 0..level {
            out.push_str("  ");
        }
    }

    fn write_number(out: &mut String, n: f64) -> Result<(), JsonError> {
        if !n.is_finite() {
            return Err(JsonError::NonFinite);
        }
        // Integers up to 2^53 print without a fraction; everything else
        // uses Rust's shortest round-trip form (never an exponent).
        if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
            let _ = write!(out, "{}", n as i64);
        } else {
            let _ = write!(out, "{n}");
        }
        Ok(())
    }

    fn write_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if u32::from(c) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn write_value(out: &mut String, v: &Value, level: usize) -> Result<(), JsonError> {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => write_number(out, *n)?,
            Value::String(s) => write_string(out, s),
            Value::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return Ok(());
                }
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    indent(out, level + 1);
                    write_value(out, item, level + 1)?;
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                indent(out, level);
                out.push(']');
            }
            Value::Object(members) => {
                if members.is_empty() {
                    out.push_str("{}");
                    return Ok(());
                }
                out.push_str("{\n");
                for (i, (k, item)) in members.iter().enumerate() {
                    indent(out, level + 1);
                    write_string(out, k);
                    out.push_str(": ");
                    write_value(out, item, level + 1)?;
                    if i + 1 < members.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                indent(out, level);
                out.push('}');
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// A story chapter: the `ASAMULevels` enumerators except `NoLevel`
/// (SAVE.md §6.1). CONFIRMED (src, cdo, map).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChapterId {
    /// `Workshop` (1), `AG-Workshop`.
    Workshop,
    /// `Sanctuary` (2), `AG-ParadiseCave`.
    Sanctuary,
    /// `Village` (3), `AG-BeautifulCity`.
    Village,
    /// `DarkCave` (4), `AG-Darkcave`.
    DarkCave,
    /// `StarHaven` (5), `AG-StarHaven`.
    StarHaven,
    /// `IceCave` (6), `AG-IceCave` (with the streamed `TheCore`).
    IceCave,
    /// `Epilogue` (7), `AG-Epilogue`.
    Epilogue,
}

impl ChapterId {
    /// Story order (the Kismet `open` chain, SAVE.md finding 6).
    pub const ALL: [ChapterId; 7] = [
        ChapterId::Workshop,
        ChapterId::Sanctuary,
        ChapterId::Village,
        ChapterId::DarkCave,
        ChapterId::StarHaven,
        ChapterId::IceCave,
        ChapterId::Epilogue,
    ];

    /// The chapters with collectibles and a time trial
    /// (`levelWithCollectiblesNames`), in time-trial slot order.
    pub const WITH_COLLECTIBLES: [ChapterId; 5] = [
        ChapterId::Sanctuary,
        ChapterId::Village,
        ChapterId::DarkCave,
        ChapterId::StarHaven,
        ChapterId::IceCave,
    ];

    /// The `ASAMULevels` enumerator name (also the map's `WorldInfo.Title`).
    #[must_use]
    pub fn enum_name(self) -> &'static str {
        match self {
            ChapterId::Workshop => "Workshop",
            ChapterId::Sanctuary => "Sanctuary",
            ChapterId::Village => "Village",
            ChapterId::DarkCave => "DarkCave",
            ChapterId::StarHaven => "StarHaven",
            ChapterId::IceCave => "IceCave",
            ChapterId::Epilogue => "Epilogue",
        }
    }

    /// The `ASAMULevels` value (1–7).
    #[must_use]
    pub fn level_index(self) -> i32 {
        match self {
            ChapterId::Workshop => 1,
            ChapterId::Sanctuary => 2,
            ChapterId::Village => 3,
            ChapterId::DarkCave => 4,
            ChapterId::StarHaven => 5,
            ChapterId::IceCave => 6,
            ChapterId::Epilogue => 7,
        }
    }

    /// `ASAMUGameInfo.LevelFileNames[level_index]`: the map package.
    #[must_use]
    pub fn map_name(self) -> &'static str {
        match self {
            ChapterId::Workshop => "AG-Workshop",
            ChapterId::Sanctuary => "AG-ParadiseCave",
            ChapterId::Village => "AG-BeautifulCity",
            ChapterId::DarkCave => "AG-Darkcave",
            ChapterId::StarHaven => "AG-StarHaven",
            ChapterId::IceCave => "AG-IceCave",
            ChapterId::Epilogue => "AG-Epilogue",
        }
    }

    /// The chapter of an `ASAMULevels` value.
    #[must_use]
    pub fn from_level_index(index: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.level_index() == index)
    }

    /// The chapter whose enumerator name is `name` (case-insensitive, as the
    /// original compares `WorldInfo.Title`).
    #[must_use]
    pub fn from_enum_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.enum_name().eq_ignore_ascii_case(name))
    }

    /// The chapter whose map package is `map` (case-insensitive: Kismet opens
    /// `AG-DarkCave` for the file `AG-Darkcave`, SAVE.md §6.1).
    #[must_use]
    pub fn from_map_name(map: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.map_name().eq_ignore_ascii_case(map))
    }

    /// Time-trial slot (level value − 2) of a collectible chapter.
    #[must_use]
    pub fn time_trial_slot(self) -> Option<usize> {
        Self::WITH_COLLECTIBLES.iter().position(|c| *c == self)
    }

    /// The chapter has collectibles (and a time trial).
    #[must_use]
    pub fn has_collectibles(self) -> bool {
        self.time_trial_slot().is_some()
    }

    /// The next chapter in story order.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        let i = Self::ALL.iter().position(|c| *c == self)?;
        Self::ALL.get(i + 1).copied()
    }
}

/// `ASAMUAchievementManager.EASAMUAchievements` (SAVE.md §6.4; enumerator
/// names from `asamu-inspect class Startup.upk
/// asamu.ASAMUAchievementManager`). CONFIRMED (class model).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(non_camel_case_types, missing_docs)]
pub enum Achievement {
    MADDIE_CHALLENGE,
    SANCTUARY_GRAPPLE_CHALLENGE,
    VILLAGE_GRAPPLE_CHALLENGE,
    CHASMS_GRAPPLE_CHALLENGE,
    STARHAVEN_GRAPPLE_CHALLENGE,
    ICECAVE_GRAPPLE_CHALLENGE,
    FLOOR_IS_LAVA,
    ALL_GOLD_MEDALS,
    ALL_COLLECTIBLES_FOUND,
    SANCTUARY_NO_FAIL,
    VILLAGE_NO_FAIL,
    CHASMS_NO_FAIL,
    STARHAVEN_NO_FAIL,
    ICECAVE_NO_FAIL,
    INTERACT_ALL_STORY,
}

impl Achievement {
    /// Every achievement in enum order.
    pub const ALL: [Achievement; 15] = [
        Achievement::MADDIE_CHALLENGE,
        Achievement::SANCTUARY_GRAPPLE_CHALLENGE,
        Achievement::VILLAGE_GRAPPLE_CHALLENGE,
        Achievement::CHASMS_GRAPPLE_CHALLENGE,
        Achievement::STARHAVEN_GRAPPLE_CHALLENGE,
        Achievement::ICECAVE_GRAPPLE_CHALLENGE,
        Achievement::FLOOR_IS_LAVA,
        Achievement::ALL_GOLD_MEDALS,
        Achievement::ALL_COLLECTIBLES_FOUND,
        Achievement::SANCTUARY_NO_FAIL,
        Achievement::VILLAGE_NO_FAIL,
        Achievement::CHASMS_NO_FAIL,
        Achievement::STARHAVEN_NO_FAIL,
        Achievement::ICECAVE_NO_FAIL,
        Achievement::INTERACT_ALL_STORY,
    ];

    /// Enum index (0–14).
    #[must_use]
    pub fn index(self) -> u8 {
        // ALL is in declaration order; the position always exists.
        Self::ALL
            .iter()
            .position(|a| *a == self)
            .and_then(|i| u8::try_from(i).ok())
            .unwrap_or(0)
    }

    /// The achievement with enum index `index`.
    #[must_use]
    pub fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(usize::from(index)).copied()
    }

    /// Steam achievement id = index + 1 (SAVE.md §6.4).
    #[must_use]
    pub fn steam_id(self) -> u32 {
        u32::from(self.index()) + 1
    }

    /// The enumerator name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Achievement::MADDIE_CHALLENGE => "MADDIE_CHALLENGE",
            Achievement::SANCTUARY_GRAPPLE_CHALLENGE => "SANCTUARY_GRAPPLE_CHALLENGE",
            Achievement::VILLAGE_GRAPPLE_CHALLENGE => "VILLAGE_GRAPPLE_CHALLENGE",
            Achievement::CHASMS_GRAPPLE_CHALLENGE => "CHASMS_GRAPPLE_CHALLENGE",
            Achievement::STARHAVEN_GRAPPLE_CHALLENGE => "STARHAVEN_GRAPPLE_CHALLENGE",
            Achievement::ICECAVE_GRAPPLE_CHALLENGE => "ICECAVE_GRAPPLE_CHALLENGE",
            Achievement::FLOOR_IS_LAVA => "FLOOR_IS_LAVA",
            Achievement::ALL_GOLD_MEDALS => "ALL_GOLD_MEDALS",
            Achievement::ALL_COLLECTIBLES_FOUND => "ALL_COLLECTIBLES_FOUND",
            Achievement::SANCTUARY_NO_FAIL => "SANCTUARY_NO_FAIL",
            Achievement::VILLAGE_NO_FAIL => "VILLAGE_NO_FAIL",
            Achievement::CHASMS_NO_FAIL => "CHASMS_NO_FAIL",
            Achievement::STARHAVEN_NO_FAIL => "STARHAVEN_NO_FAIL",
            Achievement::ICECAVE_NO_FAIL => "ICECAVE_NO_FAIL",
            Achievement::INTERACT_ALL_STORY => "INTERACT_ALL_STORY",
        }
    }

    /// The achievement named `name` (exact).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// Extras unlocked by the collectible total (SAVE.md §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extra {
    /// ≥ 10: beam colour.
    BeamColour,
    /// ≥ 15: goat mode.
    GoatMode,
    /// ≥ 20: Midas mode.
    MidasMode,
    /// ≥ 25: parkour mode.
    ParkourMode,
}

impl Extra {
    /// The extras in threshold order ([`EXTRA_THRESHOLDS`]).
    pub const ALL: [Extra; 4] = [
        Extra::BeamColour,
        Extra::GoatMode,
        Extra::MidasMode,
        Extra::ParkourMode,
    ];

    /// The collectible total that unlocks it.
    #[must_use]
    pub fn threshold(self) -> usize {
        match self {
            Extra::BeamColour => EXTRA_THRESHOLDS[0],
            Extra::GoatMode => EXTRA_THRESHOLDS[1],
            Extra::MidasMode => EXTRA_THRESHOLDS[2],
            Extra::ParkourMode => EXTRA_THRESHOLDS[3],
        }
    }
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// `PlayerProgression.bin` semantics (SAVE.md §4.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progression {
    /// Chapters entered at least once, in first-entry order (`unlockedLevels`).
    pub unlocked: Vec<ChapterId>,
    /// Collected collectibles per chapter: actor paths relative to the map
    /// (e.g. `TheWorld.PersistentLevel.ASAMUCollectible_3`).
    pub collectibles: BTreeMap<ChapterId, BTreeSet<String>>,
    /// Optional story items: `<level file name><parent actor path or None>`
    /// keys as the original builds them (SAVE.md §6.3, Q5).
    pub story_items: BTreeSet<String>,
    /// Unlocked achievements.
    pub achievements: BTreeSet<Achievement>,
    /// `bFinishedGame` (set in the Epilogue; unlocks time trial).
    pub finished_game: bool,
}

/// What a collectible pick-up changed ([`SaveSession::on_collectible`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectibleOutcome {
    /// The collectible was not recorded before.
    pub new: bool,
    /// Collected in this chapter now.
    pub chapter_count: usize,
    /// Collected in total now.
    pub total: usize,
    /// The total just reached an extra's threshold (HUD notice).
    pub extra_unlocked: Option<Extra>,
    /// `ALL_COLLECTIBLES_FOUND` was unlocked by this pick-up.
    pub all_found: bool,
}

impl Progression {
    /// A chapter is selectable: Workshop always, others once entered
    /// (SAVE.md §6.2).
    #[must_use]
    pub fn is_unlocked(&self, chapter: ChapterId) -> bool {
        chapter == ChapterId::Workshop || self.unlocked.contains(&chapter)
    }

    /// Records a chapter as entered; `true` if it was new.
    pub fn unlock(&mut self, chapter: ChapterId) -> bool {
        if self.unlocked.contains(&chapter) {
            false
        } else {
            self.unlocked.push(chapter);
            true
        }
    }

    /// Collectibles found in `chapter`.
    #[must_use]
    pub fn collectible_count(&self, chapter: ChapterId) -> usize {
        self.collectibles.get(&chapter).map_or(0, BTreeSet::len)
    }

    /// Collectibles found in total.
    #[must_use]
    pub fn collectible_total(&self) -> usize {
        self.collectibles.values().map(BTreeSet::len).sum()
    }

    /// Records a collectible (no duplicates; SAVE.md §6.3) and reports the
    /// notices the original raises. Does not touch the disk.
    pub fn add_collectible(&mut self, chapter: ChapterId, key: &str) -> CollectibleOutcome {
        let new = self
            .collectibles
            .entry(chapter)
            .or_default()
            .insert(key.to_owned());
        let chapter_count = self.collectible_count(chapter);
        let total = self.collectible_total();
        let mut out = CollectibleOutcome {
            new,
            chapter_count,
            total,
            ..CollectibleOutcome::default()
        };
        if new {
            out.extra_unlocked = Extra::ALL.into_iter().find(|e| e.threshold() == total);
            if chapter_count >= COLLECTIBLES_PER_CHAPTER && total >= TOTAL_COLLECTIBLES {
                out.all_found = self
                    .achievements
                    .insert(Achievement::ALL_COLLECTIBLES_FOUND);
            }
        }
        out
    }

    /// Records an optional story item; returns (new, `INTERACT_ALL_STORY`
    /// unlocked now).
    pub fn add_story_item(&mut self, key: &str) -> (bool, bool) {
        let new = self.story_items.insert(key.to_owned());
        let all = new
            && self.story_items.len() >= TOTAL_STORY_ITEMS
            && self.achievements.insert(Achievement::INTERACT_ALL_STORY);
        (new, all)
    }

    /// The extras unlocked by the current total.
    #[must_use]
    pub fn extras(&self) -> Vec<Extra> {
        let total = self.collectible_total();
        Extra::ALL
            .into_iter()
            .filter(|e| total >= e.threshold())
            .collect()
    }

    /// Time trial is available once the game was finished (SAVE.md §6.2).
    #[must_use]
    pub fn time_trial_unlocked(&self) -> bool {
        self.finished_game
    }
}

/// `GeneralSave.bin` semantics (SAVE.md §4.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct General {
    /// The chapter last entered (`currentLevelIndex`); `None` after New Game
    /// or a reset. Continue opens its map.
    pub current: Option<ChapterId>,
    /// Kismet "saved strings": named integer flags written by
    /// `SeqAct_EditOrAddSaveString` and read by `SeqAct_GetSaveStringValue`
    /// (shipped IDs: `NarratorCollectable`, `RevealAirship`).
    pub flags: BTreeMap<String, i32>,
}

impl General {
    /// `SeqAct_GetSaveStringValue`: the flag's value (`None` = output 1,
    /// absent).
    #[must_use]
    pub fn save_string(&self, id: &str) -> Option<i32> {
        self.flags.get(id).copied()
    }
}

/// The ability part of the snapshot (the `GrappleGun` entry, SAVE.md §4.3;
/// ABILITIES.md A-CP-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abilities {
    /// `AvailableGrappleAmount`: capacity (`iMaxGrapples`; 32 767 =
    /// unlimited, GRAPPLE.md G-CT-2).
    pub max_grapples: i32,
    /// `IsRocketBootsAvailable`.
    pub rocket_boots: bool,
    /// `IsGrappleGunAvailable`: the grapple latch (G-IN-4).
    pub grapple_enabled: bool,
}

/// `SaveGame.bin` semantics (SAVE.md §4.3): written at every new latest
/// checkpoint, loaded at every story map's start (also after a chapter
/// change, which carries the checkpoint table and abilities over).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Map that wrote it (`PersistentMapFileName`).
    pub map: String,
    /// Latest completed checkpoint index per chapter
    /// (`latestCompletedCheckpointPerLevel`).
    pub checkpoints: BTreeMap<ChapterId, i32>,
    /// Grapple capacity / boots / latch at save time (`None` when saved from
    /// a world without a player pawn, e.g. the menu).
    pub abilities: Option<Abilities>,
    /// Savable actors keyed by their path relative to the map (collectible
    /// taken, crystal state, rock placement, …). Entries are opaque objects
    /// owned by the world/Kismet workstreams; unknown paths are skipped on
    /// load, as in the original.
    pub actors: BTreeMap<String, Value>,
    /// Kismet events/variables/Matinee state keyed by object path (opaque;
    /// owned by the Kismet host).
    pub kismet: BTreeMap<String, Value>,
}

impl Snapshot {
    /// The snapshot New Game and chapter select write: saved from the menu
    /// world, so an empty checkpoint table and no ability entry
    /// (SAVE.md §5).
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            map: FRONT_END_MAP.to_owned(),
            ..Self::default()
        }
    }
}

/// Time-trial medals (SAVE.md §6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Medal {
    /// Within the gold target.
    Gold,
    /// Within the silver target.
    Silver,
    /// Within the bronze target.
    Bronze,
}

/// `TimeTrialSavefile.LevelTargetScores` in seconds (gold, silver, bronze)
/// per time-trial slot. CONFIRMED (cdo; SAVE.md §6.5).
pub const TIME_TRIAL_TARGETS: [[f32; 3]; 5] = [
    [260.0, 290.0, 320.0],
    [200.0, 220.0, 270.0],
    [240.0, 270.0, 320.0],
    [480.0, 540.0, 650.0],
    [810.0, 960.0, 1150.0],
];

/// `TTS.bin` semantics (SAVE.md §4.4): best time per collectible chapter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeTrialTimes {
    /// Best time in seconds per chapter (absent = no time).
    pub best: BTreeMap<ChapterId, f32>,
}

/// What finishing a time trial changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeTrialOutcome {
    /// The run was stored as the new best.
    pub new_best: bool,
    /// Medal of the (possibly unchanged) best time.
    pub medal: Option<Medal>,
    /// `ALL_GOLD_MEDALS` was unlocked by this run.
    pub all_gold: bool,
}

impl TimeTrialTimes {
    /// The medal of `seconds` in `chapter`: the first target ≥ the time;
    /// times ≤ 0 earn none.
    #[must_use]
    pub fn medal_for(chapter: ChapterId, seconds: f32) -> Option<Medal> {
        let targets = TIME_TRIAL_TARGETS.get(chapter.time_trial_slot()?)?;
        if seconds <= 0.0 || !seconds.is_finite() {
            return None;
        }
        [Medal::Gold, Medal::Silver, Medal::Bronze]
            .into_iter()
            .zip(targets)
            .find(|(_, t)| **t >= seconds)
            .map(|(m, _)| m)
    }

    /// The best time's medal.
    #[must_use]
    pub fn medal(&self, chapter: ChapterId) -> Option<Medal> {
        self.best
            .get(&chapter)
            .and_then(|t| Self::medal_for(chapter, *t))
    }

    /// Stores `seconds` when faster than the best or when there is none;
    /// returns whether it was stored.
    pub fn record(&mut self, chapter: ChapterId, seconds: f32) -> bool {
        if !chapter.has_collectibles() || !seconds.is_finite() || seconds <= 0.0 {
            return false;
        }
        match self.best.get(&chapter) {
            Some(best) if *best <= seconds => false,
            _ => {
                self.best.insert(chapter, seconds);
                true
            }
        }
    }

    /// Gold in all five chapters.
    #[must_use]
    pub fn all_gold(&self) -> bool {
        ChapterId::WITH_COLLECTIBLES
            .into_iter()
            .all(|c| self.medal(c) == Some(Medal::Gold))
    }
}

/// The menu's time format `MM:SS:hh` (minutes, seconds, hundredths); times
/// of 5999.59 s or more show `99:99:99` (SAVE.md §6.5).
#[must_use]
pub fn format_trial_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "00:00:00".to_owned();
    }
    if seconds >= 5999.59 {
        return "99:99:99".to_owned();
    }
    let whole = seconds.floor();
    let minutes = (whole / 60.0).floor();
    let secs = whole - minutes * 60.0;
    let hundredths = ((seconds - whole) * 100.0).floor().min(99.0);
    format!(
        "{:02}:{:02}:{:02}",
        minutes as u32, secs as u32, hundredths as u32
    )
}

/// User settings (SAVE.md §7; the original keeps them in ini files, we keep
/// one JSON document next to the saves).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Horizontal field of view, degrees. Default 90: `FOV=90` in the shipped
    /// `DefaultSettings.ini` (`[ASAMU.ASAMUSettingsManager]`), CONFIRMED
    /// (config); ≤ 0 falls back to 90 as in the original.
    pub fov_degrees: f32,
    /// Mouse look multiplier on the app's radians-per-count (ours: the
    /// original's `PlayerInput` scaling is not ported, see `apps/asamu`).
    pub mouse_sensitivity: f32,
    /// Invert vertical mouse look (not set in the shipped ini; ours: off).
    pub invert_mouse: bool,
    /// `MasterGroupVolume`, default 1.0 (config).
    pub master_volume: f32,
    /// `MusicGroupVolume`, default 0.8 (config).
    pub music_volume: f32,
    /// `SFXGroupVolume`, default 0.8 (config).
    pub sfx_volume: f32,
    /// `VoiceGroupVolume`, default 0.8 (config).
    pub voice_volume: f32,
    /// Fullscreen (borderless). Default off: `Fullscreen=False` in the
    /// shipped `DefaultSystemSettings.ini` (config).
    pub fullscreen: bool,
    /// Window size in logical pixels (`None` = the app's default).
    pub resolution: Option<[u32; 2]>,
    /// Subtitles. Default on: `bSubtitlesEnabled=True` in the shipped
    /// `BaseEngine.ini` (config).
    pub subtitles: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fov_degrees: 90.0,
            mouse_sensitivity: 1.0,
            invert_mouse: false,
            master_volume: 1.0,
            music_volume: 0.8,
            sfx_volume: 0.8,
            voice_volume: 0.8,
            fullscreen: false,
            resolution: None,
            subtitles: true,
        }
    }
}

impl Settings {
    /// FOV range offered by our menu (ours).
    pub const FOV_RANGE: [f32; 2] = [60.0, 120.0];
    /// Mouse multiplier range offered by our menu (ours).
    pub const SENSITIVITY_RANGE: [f32; 2] = [0.1, 5.0];
    /// Smallest / largest window size accepted (ours).
    pub const RESOLUTION_RANGE: [u32; 2] = [320, 16_384];

    /// The settings with every value in range (non-finite values back to
    /// their defaults; FOV ≤ 0 → 90 as in the original).
    #[must_use]
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let clamp = |v: f32, [lo, hi]: [f32; 2], default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        let fov = if self.fov_degrees.is_finite() && self.fov_degrees > 0.0 {
            self.fov_degrees
        } else {
            d.fov_degrees
        };
        let volume = |v: f32, default: f32| clamp(v, [0.0, 1.0], default);
        let [rlo, rhi] = Self::RESOLUTION_RANGE;
        Self {
            fov_degrees: clamp(fov, Self::FOV_RANGE, d.fov_degrees),
            mouse_sensitivity: clamp(
                self.mouse_sensitivity,
                Self::SENSITIVITY_RANGE,
                d.mouse_sensitivity,
            ),
            invert_mouse: self.invert_mouse,
            master_volume: volume(self.master_volume, d.master_volume),
            music_volume: volume(self.music_volume, d.music_volume),
            sfx_volume: volume(self.sfx_volume, d.sfx_volume),
            voice_volume: volume(self.voice_volume, d.voice_volume),
            fullscreen: self.fullscreen,
            resolution: self
                .resolution
                .filter(|r| r.iter().all(|v| (rlo..=rhi).contains(v))),
            subtitles: self.subtitles,
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Errors of the save system.
#[derive(Debug, Error)]
pub enum SaveError {
    /// Reading or writing a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// The text is not valid JSON.
    #[error("not valid JSON: {0}")]
    Json(#[from] json::JsonError),
    /// The JSON is not the expected document.
    #[error("invalid {kind} document: {reason}")]
    Invalid {
        /// Document kind.
        kind: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// Written by a newer version of this program.
    #[error("{kind} document has format_version {found}, newer than the supported {supported}")]
    NewerVersion {
        /// Document kind.
        kind: &'static str,
        /// Version in the file.
        found: u32,
        /// Highest version we read.
        supported: u32,
    },
    /// The file is larger than [`MAX_DOCUMENT_BYTES`].
    #[error("{0} is larger than {MAX_DOCUMENT_BYTES} bytes")]
    TooLarge(PathBuf),
    /// The document's file was unreadable and could not be set aside
    /// (rename and copy both failed), so this session never overwrites it.
    #[error("{0} could not be read or set aside, so it is not overwritten")]
    Protected(PathBuf),
    /// No save directory could be determined (no home / app-data dir).
    #[error("no user data directory (set ASAMU_SAVE_DIR)")]
    NoDirectory,
}

fn invalid(kind: &'static str, reason: impl Into<String>) -> SaveError {
    SaveError::Invalid {
        kind,
        reason: reason.into(),
    }
}

/// A document we store: its file, its tag and its JSON mapping.
pub trait Document: Sized {
    /// Short kind name (`progression`, …).
    const KIND: &'static str;
    /// File name inside the save directory.
    const FILE: &'static str;
    /// The JSON fields (without the `format` / `format_version` envelope).
    fn to_fields(&self) -> BTreeMap<String, Value>;
    /// Decodes the fields of an up-to-date document. Missing optional
    /// fields take their defaults; unknown fields are ignored.
    ///
    /// # Errors
    /// [`SaveError::Invalid`] when a field has the wrong shape.
    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError>;

    /// The `format` tag.
    #[must_use]
    fn format_tag() -> String {
        format!("asamu-decomp/{}", Self::KIND)
    }
}

/// Serializes a document with its envelope.
///
/// # Errors
/// A non-finite number in the document.
pub fn encode_document<D: Document>(doc: &D) -> Result<String, SaveError> {
    let mut fields = doc.to_fields();
    fields.insert("format".into(), Value::String(D::format_tag()));
    fields.insert("format_version".into(), Value::from(FORMAT_VERSION));
    Ok(json::to_pretty_string(&Value::Object(fields))?)
}

/// One step of the upgrade chain: turns a document of version `n` into
/// version `n + 1` (fields only).
pub type UpgradeStep = fn(BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, SaveError>;

/// The upgrade chain for `kind` documents: entry `i` upgrades version
/// `i + 1` to `i + 2`. Version 1 is the first format, so the chain is empty
/// for every kind; a future format change adds its step here (and bumps
/// [`FORMAT_VERSION`]). Additive changes need no step: missing fields take
/// defaults.
#[must_use]
pub fn upgrade_steps(kind: &str) -> &'static [UpgradeStep] {
    let _ = kind;
    &[]
}

/// Runs `steps` on fields of version `from` up to `current`.
///
/// # Errors
/// A missing step or a failing step.
pub fn upgrade_document(
    kind: &'static str,
    mut fields: BTreeMap<String, Value>,
    from: u32,
    current: u32,
    steps: &[UpgradeStep],
) -> Result<BTreeMap<String, Value>, SaveError> {
    let mut version = from;
    while version < current {
        // Version 0 does not exist (the first format is 1): no step.
        let step = version
            .checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| steps.get(i))
            .ok_or_else(|| invalid(kind, format!("no upgrade from format_version {version}")))?;
        fields = step(fields)?;
        version = version
            .checked_add(1)
            .ok_or_else(|| invalid(kind, "format_version overflow"))?;
    }
    Ok(fields)
}

/// Parses a document text (envelope check, upgrade chain, decode).
///
/// # Errors
/// Malformed JSON, wrong tag, unsupported or newer version, bad fields.
pub fn decode_document<D: Document>(text: &str) -> Result<D, SaveError> {
    decode_with_steps(text, FORMAT_VERSION, upgrade_steps(D::KIND))
}

fn decode_with_steps<D: Document>(
    text: &str,
    current: u32,
    steps: &[UpgradeStep],
) -> Result<D, SaveError> {
    let Value::Object(mut fields) = json::parse(text)? else {
        return Err(invalid(D::KIND, "not a JSON object"));
    };
    match fields.remove("format") {
        Some(Value::String(tag)) if tag == D::format_tag() => {}
        Some(Value::String(tag)) => {
            return Err(invalid(D::KIND, format!("format is {tag:?}")));
        }
        _ => return Err(invalid(D::KIND, "missing format tag")),
    }
    let version = fields
        .remove("format_version")
        .and_then(|v| v.as_u32())
        .filter(|v| *v >= 1)
        .ok_or_else(|| invalid(D::KIND, "missing or invalid format_version"))?;
    if version > current {
        return Err(SaveError::NewerVersion {
            kind: D::KIND,
            found: version,
            supported: current,
        });
    }
    let fields = upgrade_document(D::KIND, fields, version, current, steps)?;
    D::from_fields(&fields)
}

fn opt_bool(
    fields: &BTreeMap<String, Value>,
    key: &str,
    kind: &'static str,
) -> Result<Option<bool>, SaveError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid(kind, format!("{key} is not a boolean"))),
    }
}

fn opt_f32(
    fields: &BTreeMap<String, Value>,
    key: &str,
    kind: &'static str,
) -> Result<Option<f32>, SaveError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f32()
            .map(Some)
            .ok_or_else(|| invalid(kind, format!("{key} is not a number"))),
    }
}

fn opt_object<'a>(
    fields: &'a BTreeMap<String, Value>,
    key: &str,
    kind: &'static str,
) -> Result<Option<&'a BTreeMap<String, Value>>, SaveError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_object()
            .map(Some)
            .ok_or_else(|| invalid(kind, format!("{key} is not an object"))),
    }
}

fn opt_array<'a>(
    fields: &'a BTreeMap<String, Value>,
    key: &str,
    kind: &'static str,
) -> Result<&'a [Value], SaveError> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(v) => v
            .as_array()
            .ok_or_else(|| invalid(kind, format!("{key} is not an array"))),
    }
}

fn chapter_key(name: &str, kind: &'static str) -> Result<ChapterId, SaveError> {
    ChapterId::from_enum_name(name)
        .ok_or_else(|| invalid(kind, format!("unknown chapter {name:?}")))
}

fn string_items(items: &[Value], key: &str, kind: &'static str) -> Result<Vec<String>, SaveError> {
    items
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(kind, format!("{key} holds a non-string")))
        })
        .collect()
}

impl Document for Progression {
    const KIND: &'static str = "progression";
    const FILE: &'static str = "progression.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert(
            "unlocked_chapters".into(),
            Value::Array(
                self.unlocked
                    .iter()
                    .map(|c| Value::from(c.enum_name()))
                    .collect(),
            ),
        );
        f.insert(
            "collectibles".into(),
            Value::Object(
                self.collectibles
                    .iter()
                    .filter(|(_, keys)| !keys.is_empty())
                    .map(|(c, keys)| {
                        (
                            c.enum_name().to_owned(),
                            Value::Array(keys.iter().map(|k| Value::from(k.as_str())).collect()),
                        )
                    })
                    .collect(),
            ),
        );
        f.insert(
            "story_items".into(),
            Value::Array(
                self.story_items
                    .iter()
                    .map(|k| Value::from(k.as_str()))
                    .collect(),
            ),
        );
        f.insert(
            "achievements".into(),
            Value::Array(
                self.achievements
                    .iter()
                    .map(|a| Value::from(a.name()))
                    .collect(),
            ),
        );
        f.insert("finished_game".into(), Value::Bool(self.finished_game));
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let kind = Self::KIND;
        let mut p = Progression::default();
        for name in string_items(
            opt_array(fields, "unlocked_chapters", kind)?,
            "unlocked_chapters",
            kind,
        )? {
            p.unlock(chapter_key(&name, kind)?);
        }
        if let Some(map) = opt_object(fields, "collectibles", kind)? {
            for (chapter, keys) in map {
                let chapter = chapter_key(chapter, kind)?;
                let keys = keys
                    .as_array()
                    .ok_or_else(|| invalid(kind, "collectibles entry is not an array"))?;
                let set = p.collectibles.entry(chapter).or_default();
                set.extend(string_items(keys, "collectibles", kind)?);
            }
        }
        p.story_items = string_items(opt_array(fields, "story_items", kind)?, "story_items", kind)?
            .into_iter()
            .collect();
        // Unknown achievement names (from a newer build) are ignored.
        p.achievements = string_items(
            opt_array(fields, "achievements", kind)?,
            "achievements",
            kind,
        )?
        .iter()
        .filter_map(|n| Achievement::from_name(n))
        .collect();
        p.finished_game = opt_bool(fields, "finished_game", kind)?.unwrap_or(false);
        Ok(p)
    }
}

impl Document for General {
    const KIND: &'static str = "general";
    const FILE: &'static str = "general.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert(
            "current_chapter".into(),
            self.current
                .map_or(Value::Null, |c| Value::from(c.enum_name())),
        );
        f.insert(
            "saved_strings".into(),
            Value::Object(
                self.flags
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::from(*v)))
                    .collect(),
            ),
        );
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let kind = Self::KIND;
        let current = match fields.get("current_chapter") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(chapter_key(s, kind)?),
            Some(_) => return Err(invalid(kind, "current_chapter is not a string")),
        };
        let mut flags = BTreeMap::new();
        if let Some(map) = opt_object(fields, "saved_strings", kind)? {
            for (k, v) in map {
                let v = v
                    .as_i32()
                    .ok_or_else(|| invalid(kind, format!("saved string {k:?} is not an int")))?;
                flags.insert(k.clone(), v);
            }
        }
        Ok(Self { current, flags })
    }
}

impl Document for Snapshot {
    const KIND: &'static str = "snapshot";
    const FILE: &'static str = "snapshot.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert("map".into(), Value::from(self.map.as_str()));
        f.insert(
            "checkpoints".into(),
            Value::Object(
                self.checkpoints
                    .iter()
                    .map(|(c, i)| (c.enum_name().to_owned(), Value::from(*i)))
                    .collect(),
            ),
        );
        f.insert(
            "abilities".into(),
            self.abilities.map_or(Value::Null, |a| {
                let mut m = BTreeMap::new();
                m.insert("max_grapples".into(), Value::from(a.max_grapples));
                m.insert("rocket_boots".into(), Value::Bool(a.rocket_boots));
                m.insert("grapple_enabled".into(), Value::Bool(a.grapple_enabled));
                Value::Object(m)
            }),
        );
        f.insert("actors".into(), Value::Object(self.actors.clone()));
        f.insert("kismet".into(), Value::Object(self.kismet.clone()));
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let kind = Self::KIND;
        let map = match fields.get("map") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err(invalid(kind, "map is missing or not a string")),
        };
        let mut checkpoints = BTreeMap::new();
        if let Some(table) = opt_object(fields, "checkpoints", kind)? {
            for (c, i) in table {
                let i = i
                    .as_i32()
                    .ok_or_else(|| invalid(kind, format!("checkpoint of {c:?} is not an int")))?;
                checkpoints.insert(chapter_key(c, kind)?, i);
            }
        }
        let abilities = match opt_object(fields, "abilities", kind)? {
            None => None,
            Some(a) => Some(Abilities {
                max_grapples: a
                    .get("max_grapples")
                    .and_then(Value::as_i32)
                    .ok_or_else(|| invalid(kind, "abilities.max_grapples is not an int"))?,
                rocket_boots: opt_bool(a, "rocket_boots", kind)?.unwrap_or(false),
                grapple_enabled: opt_bool(a, "grapple_enabled", kind)?.unwrap_or(true),
            }),
        };
        let actors = opt_object(fields, "actors", kind)?
            .cloned()
            .unwrap_or_default();
        let kismet = opt_object(fields, "kismet", kind)?
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            map,
            checkpoints,
            abilities,
            actors,
            kismet,
        })
    }
}

impl Document for TimeTrialTimes {
    const KIND: &'static str = "time_trial";
    const FILE: &'static str = "time_trial.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert(
            "best_seconds".into(),
            Value::Object(
                self.best
                    .iter()
                    .map(|(c, t)| (c.enum_name().to_owned(), Value::from_f32(*t)))
                    .collect(),
            ),
        );
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let kind = Self::KIND;
        let mut best = BTreeMap::new();
        if let Some(map) = opt_object(fields, "best_seconds", kind)? {
            for (c, t) in map {
                let chapter = chapter_key(c, kind)?;
                let t = t
                    .as_f32()
                    .filter(|t| *t > 0.0)
                    .ok_or_else(|| invalid(kind, format!("time of {c:?} is not positive")))?;
                if chapter.has_collectibles() {
                    best.insert(chapter, t);
                }
            }
        }
        Ok(Self { best })
    }
}

impl Document for Settings {
    const KIND: &'static str = "settings";
    const FILE: &'static str = "settings.json";

    fn to_fields(&self) -> BTreeMap<String, Value> {
        let mut f = BTreeMap::new();
        f.insert("fov_degrees".into(), Value::from_f32(self.fov_degrees));
        f.insert(
            "mouse_sensitivity".into(),
            Value::from_f32(self.mouse_sensitivity),
        );
        f.insert("invert_mouse".into(), Value::Bool(self.invert_mouse));
        f.insert("master_volume".into(), Value::from_f32(self.master_volume));
        f.insert("music_volume".into(), Value::from_f32(self.music_volume));
        f.insert("sfx_volume".into(), Value::from_f32(self.sfx_volume));
        f.insert("voice_volume".into(), Value::from_f32(self.voice_volume));
        f.insert("fullscreen".into(), Value::Bool(self.fullscreen));
        f.insert(
            "resolution".into(),
            self.resolution.map_or(Value::Null, |[w, h]| {
                Value::Array(vec![Value::from(w), Value::from(h)])
            }),
        );
        f.insert("subtitles".into(), Value::Bool(self.subtitles));
        f
    }

    fn from_fields(fields: &BTreeMap<String, Value>) -> Result<Self, SaveError> {
        let kind = Self::KIND;
        let d = Settings::default();
        let resolution = match fields.get("resolution") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => match items.as_slice() {
                [w, h] => Some([
                    w.as_u32()
                        .ok_or_else(|| invalid(kind, "resolution width"))?,
                    h.as_u32()
                        .ok_or_else(|| invalid(kind, "resolution height"))?,
                ]),
                _ => return Err(invalid(kind, "resolution is not [width, height]")),
            },
            Some(_) => return Err(invalid(kind, "resolution is not an array")),
        };
        Ok(Settings {
            fov_degrees: opt_f32(fields, "fov_degrees", kind)?.unwrap_or(d.fov_degrees),
            mouse_sensitivity: opt_f32(fields, "mouse_sensitivity", kind)?
                .unwrap_or(d.mouse_sensitivity),
            invert_mouse: opt_bool(fields, "invert_mouse", kind)?.unwrap_or(d.invert_mouse),
            master_volume: opt_f32(fields, "master_volume", kind)?.unwrap_or(d.master_volume),
            music_volume: opt_f32(fields, "music_volume", kind)?.unwrap_or(d.music_volume),
            sfx_volume: opt_f32(fields, "sfx_volume", kind)?.unwrap_or(d.sfx_volume),
            voice_volume: opt_f32(fields, "voice_volume", kind)?.unwrap_or(d.voice_volume),
            fullscreen: opt_bool(fields, "fullscreen", kind)?.unwrap_or(d.fullscreen),
            resolution,
            subtitles: opt_bool(fields, "subtitles", kind)?.unwrap_or(d.subtitles),
        }
        .sanitized())
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The result of loading one document.
#[derive(Debug)]
pub enum LoadOutcome<T> {
    /// No file (first launch).
    Missing,
    /// Read and decoded.
    Loaded(T),
    /// The file exists but cannot be used; it was renamed to `quarantined`
    /// (when the rename succeeded) so that the next save does not destroy it.
    Unreadable {
        /// Why.
        error: SaveError,
        /// Where the file went.
        quarantined: Option<PathBuf>,
    },
}

impl<T> LoadOutcome<T> {
    /// The document, if one was loaded.
    pub fn loaded(self) -> Option<T> {
        match self {
            LoadOutcome::Loaded(t) => Some(t),
            _ => None,
        }
    }
}

/// A directory of documents (our saves and settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStore {
    dir: PathBuf,
}

impl SaveStore {
    /// A store in `dir` (created on the first write).
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Our user-local data root: `ASAMU_SAVE_DIR` when set, else
    /// `asamu-decomp/` in the platform's per-user application-data folder
    /// (macOS `~/Library/Application Support`, Windows `%LOCALAPPDATA%`,
    /// elsewhere `$XDG_DATA_HOME` or `~/.local/share`), the same base the
    /// importer's default output uses. Saves go to `<root>/saves/`, settings
    /// to `<root>/settings.json`.
    #[must_use]
    pub fn default_root() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("ASAMU_SAVE_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        let base = if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        } else if cfg!(target_os = "windows") {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        }?;
        Some(base.join("asamu-decomp"))
    }

    /// Path of a document.
    #[must_use]
    pub fn path_of<D: Document>(&self) -> PathBuf {
        self.dir.join(D::FILE)
    }

    /// Loads a document (quarantining an unreadable file).
    pub fn load<D: Document>(&self) -> LoadOutcome<D> {
        let path = self.path_of::<D>();
        let text = match read_bounded(&path) {
            Ok(Some(text)) => text,
            Ok(None) => return LoadOutcome::Missing,
            Err(error) => {
                // A file we cannot even read (permissions, not UTF-8, too
                // large): try to move it aside as well.
                let quarantined = quarantine(&path);
                return LoadOutcome::Unreadable { error, quarantined };
            }
        };
        match decode_document::<D>(&text) {
            Ok(doc) => LoadOutcome::Loaded(doc),
            Err(error) => LoadOutcome::Unreadable {
                quarantined: quarantine(&path),
                error,
            },
        }
    }

    /// Writes a document atomically.
    ///
    /// # Errors
    /// I/O failures, a non-finite number.
    pub fn save<D: Document>(&self, doc: &D) -> Result<(), SaveError> {
        let text = encode_document(doc)?;
        write_atomic(&self.path_of::<D>(), text.as_bytes())
    }
}

fn io_err(path: &Path, source: std::io::Error) -> SaveError {
    SaveError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The file's text, `None` when it does not exist.
fn read_bounded(path: &Path) -> Result<Option<String>, SaveError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path, e)),
    };
    if meta.len() > MAX_DOCUMENT_BYTES {
        return Err(SaveError::TooLarge(path.to_path_buf()));
    }
    // Read at most one byte past the limit: the file may have grown since
    // the size check.
    let file = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, MAX_DOCUMENT_BYTES + 1),
        &mut bytes,
    )
    .map_err(|e| io_err(path, e))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(SaveError::TooLarge(path.to_path_buf()));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid("document", "not UTF-8 text"))
}

/// Highest `<n>` tried for `<stem>.corrupt-<n>.<ext>`.
const MAX_QUARANTINE_SLOTS: u32 = 999;

/// Sets an unreadable file aside under the first free
/// `<stem>.corrupt-<n>.<ext>`: renamed, or — when the rename fails — copied
/// (the original then stays in place). `None` when neither worked; the
/// caller must then not overwrite `path` ([`SaveError::Protected`]).
fn quarantine(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let ext = path
        .extension()
        .map_or_else(String::new, |e| format!(".{}", e.to_string_lossy()));
    let dir = path.parent()?;
    let target = (1..=MAX_QUARANTINE_SLOTS)
        .map(|n| dir.join(format!("{stem}.corrupt-{n}{ext}")))
        .find(|candidate| !candidate.exists())?;
    if std::fs::rename(path, &target).is_ok() {
        return Some(target);
    }
    // A regular file only: never copy a directory or a device.
    let is_file = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    (is_file && std::fs::copy(path, &target).is_ok()).then_some(target)
}

/// Temporary file in the same directory, flushed to disk, renamed over the
/// target: a crash leaves either the old or the new file, never a torn one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let dir = path
        .parent()
        .ok_or_else(|| invalid("document", "path has no directory"))?;
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("document", "path has no file name"))?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(".{name}.tmp"));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
        file.write_all(bytes).map_err(|e| io_err(&tmp, e))?;
        file.sync_all().map_err(|e| io_err(&tmp, e))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| io_err(path, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// ---------------------------------------------------------------------------
// Session: the lifecycle of SAVE.md §5
// ---------------------------------------------------------------------------

/// Story play or time trial (time trial never writes the snapshot or the
/// chapter pointer, SAVE.md §5 / ABILITIES.md A-TT-2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayMode {
    /// Story (New Game, Continue, chapter select, Kismet transitions).
    #[default]
    Story,
    /// Time trial.
    TimeTrial,
}

/// What [`SaveSession::begin_level`] decided for a level start.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelStart {
    /// The chapter the map belongs to (`None`: menu or unknown map).
    pub chapter: Option<ChapterId>,
    /// The snapshot to apply at the player's spawn
    /// ([`Game::apply_snapshot`]); `None` in time trial or when there is none.
    pub snapshot: Option<Snapshot>,
    /// The chapter was entered for the first time.
    pub newly_unlocked: bool,
}

/// One problem met while opening the saves (for logs / the UI).
#[derive(Debug)]
pub struct LoadIssue {
    /// Document kind.
    pub kind: &'static str,
    /// What happened.
    pub error: SaveError,
    /// Where the unreadable file was moved.
    pub quarantined: Option<PathBuf>,
}

/// The save state of one run of the program and its lifecycle hooks.
///
/// Every hook updates the in-memory model first and then writes the
/// affected document; a failed write is returned as an error and the
/// in-memory state stays authoritative (the next successful write includes
/// it).
#[derive(Debug)]
pub struct SaveSession {
    store: Option<SaveStore>,
    /// Progression (chapters, collectibles, story items, achievements).
    pub progression: Progression,
    /// Chapter pointer and Kismet flags.
    pub general: General,
    /// The checkpoint snapshot, if any.
    pub snapshot: Option<Snapshot>,
    /// Time-trial best times.
    pub time_trial: TimeTrialTimes,
    legible: bool,
    mode: PlayMode,
    issues: Vec<LoadIssue>,
    /// Kinds whose unreadable file could not be set aside: never written
    /// by this session, so the file survives for manual recovery.
    protected: BTreeSet<&'static str>,
}

impl SaveSession {
    /// A session that never touches the disk (tests, development runs).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            store: None,
            progression: Progression::default(),
            general: General::default(),
            snapshot: None,
            time_trial: TimeTrialTimes::default(),
            legible: false,
            mode: PlayMode::Story,
            issues: Vec::new(),
            protected: BTreeSet::new(),
        }
    }

    /// Opens the saves in `store` (the original's menu start, SAVE.md §5
    /// row 1): progression, then general; "legible" (Continue possible) only
    /// when both loaded. A missing or unreadable document starts from its
    /// defaults and is written back at once (the other document is kept —
    /// our fix of quirk Q2); unreadable files are quarantined first. The
    /// snapshot and the time-trial times load too (an unreadable one is
    /// quarantined and treated as absent).
    pub fn open(store: SaveStore) -> Self {
        let mut s = Self {
            store: Some(store.clone()),
            ..Self::in_memory()
        };
        let mut both = true;
        match store.load::<Progression>() {
            LoadOutcome::Loaded(p) => s.progression = p,
            outcome => {
                both = false;
                s.note(Progression::KIND, outcome);
            }
        }
        match store.load::<General>() {
            LoadOutcome::Loaded(g) => s.general = g,
            outcome => {
                both = false;
                s.note(General::KIND, outcome);
            }
        }
        s.legible = both;
        if !both {
            // The original rewrites both files after a reset (a file that
            // could not be set aside is left alone: `write` refuses it).
            if let Err(error) = s.write(&s.progression) {
                s.issues.push(LoadIssue {
                    kind: Progression::KIND,
                    error,
                    quarantined: None,
                });
            }
            if let Err(error) = s.write(&s.general) {
                s.issues.push(LoadIssue {
                    kind: General::KIND,
                    error,
                    quarantined: None,
                });
            }
        }
        match store.load::<Snapshot>() {
            LoadOutcome::Loaded(snap) => s.snapshot = Some(snap),
            outcome => s.note(Snapshot::KIND, outcome),
        }
        match store.load::<TimeTrialTimes>() {
            LoadOutcome::Loaded(t) => s.time_trial = t,
            outcome => s.note(TimeTrialTimes::KIND, outcome),
        }
        s
    }

    fn note<T>(&mut self, kind: &'static str, outcome: LoadOutcome<T>) {
        if let LoadOutcome::Unreadable { error, quarantined } = outcome {
            if quarantined.is_none() {
                self.protected.insert(kind);
            }
            self.issues.push(LoadIssue {
                kind,
                error,
                quarantined,
            });
        }
    }

    /// The problems met by [`Self::open`]: unreadable documents (and where
    /// they were moved) and failed write-backs. Later write failures are
    /// returned by the hooks instead.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }

    /// The backing store (`None` for an in-memory session).
    #[must_use]
    pub fn store(&self) -> Option<&SaveStore> {
        self.store.as_ref()
    }

    /// Both progression and general loaded at open time.
    #[must_use]
    pub fn legible(&self) -> bool {
        self.legible
    }

    /// The current play mode.
    #[must_use]
    pub fn mode(&self) -> PlayMode {
        self.mode
    }

    /// Continue is offered: a legible save with a chapter pointer (ours: the
    /// original also needed the legible save, but could point at the front
    /// end after a reset, quirk Q6).
    #[must_use]
    pub fn can_continue(&self) -> bool {
        self.legible && self.general.current.is_some()
    }

    /// The chapter Continue opens (`LevelFileNames[currentLevelIndex]`).
    #[must_use]
    pub fn continue_target(&self) -> Option<ChapterId> {
        self.can_continue()
            .then_some(self.general.current)
            .flatten()
    }

    fn write<D: Document>(&self, doc: &D) -> Result<(), SaveError> {
        match &self.store {
            Some(store) if self.protected.contains(D::KIND) => {
                Err(SaveError::Protected(store.path_of::<D>()))
            }
            Some(store) => store.save(doc),
            None => Ok(()),
        }
    }

    /// Main menu "New Game": a fresh snapshot (empty checkpoint table, no
    /// abilities) and a cleared chapter pointer (flags kept); everything else
    /// survives (SAVE.md finding 9). Returns the chapter to open (Workshop).
    ///
    /// # Errors
    /// A failed write (the in-memory state is updated anyway).
    pub fn new_game(&mut self) -> Result<ChapterId, SaveError> {
        self.mode = PlayMode::Story;
        self.snapshot = Some(Snapshot::fresh());
        self.general.current = None;
        self.legible = true;
        let a = self.write(&Snapshot::fresh());
        let b = self.write(&self.general);
        a.and(b).map(|()| ChapterId::Workshop)
    }

    /// Chapter select: a fresh snapshot, then the chapter's map opens
    /// (SAVE.md §5). Locked chapters are refused (`Ok(false)`).
    ///
    /// # Errors
    /// A failed write.
    pub fn start_chapter(&mut self, chapter: ChapterId) -> Result<bool, SaveError> {
        if !self.progression.is_unlocked(chapter) {
            return Ok(false);
        }
        self.mode = PlayMode::Story;
        self.snapshot = Some(Snapshot::fresh());
        self.legible = true;
        self.write(&Snapshot::fresh()).map(|()| true)
    }

    /// Continue: story mode; the caller opens [`Self::continue_target`]'s
    /// map. Returns it.
    #[must_use]
    pub fn continue_game(&mut self) -> Option<ChapterId> {
        let target = self.continue_target()?;
        self.mode = PlayMode::Story;
        Some(target)
    }

    /// Starts time-trial play (no snapshot, no pointer); `false` while time
    /// trial is locked or for a chapter without one.
    pub fn start_time_trial(&mut self, chapter: ChapterId) -> bool {
        if !self.progression.time_trial_unlocked() || !chapter.has_collectibles() {
            return false;
        }
        self.mode = PlayMode::TimeTrial;
        true
    }

    /// A map started (`InitSaveManagerForLevel` + the pawn's save load):
    /// in story mode a chapter map is added to the unlocked chapters
    /// (progression written on every chapter start) and becomes the chapter
    /// pointer (general written); the snapshot to apply at the spawn is
    /// returned. `title` is the map's `WorldInfo.Title` (the original matches
    /// it against the chapter names); the map name is the fallback. The
    /// front-end map and time trial change nothing.
    ///
    /// # Errors
    /// A failed write (the returned decision is still valid; use
    /// [`Self::begin_level_lossy`] to get both).
    pub fn begin_level(&mut self, map: &str, title: Option<&str>) -> Result<LevelStart, SaveError> {
        let (start, result) = self.begin_level_lossy(map, title);
        result.map(|()| start)
    }

    /// [`Self::begin_level`], returning the decision even when a write
    /// failed.
    pub fn begin_level_lossy(
        &mut self,
        map: &str,
        title: Option<&str>,
    ) -> (LevelStart, Result<(), SaveError>) {
        let chapter = chapter_of_map(map, title);
        let mut start = LevelStart {
            chapter,
            snapshot: None,
            newly_unlocked: false,
        };
        if self.mode == PlayMode::TimeTrial {
            return (start, Ok(()));
        }
        start.snapshot = self.snapshot.clone();
        let Some(chapter) = chapter else {
            return (start, Ok(()));
        };
        start.newly_unlocked = self.progression.unlock(chapter);
        self.general.current = Some(chapter);
        let a = self.write(&self.progression);
        let b = self.write(&self.general);
        (start, a.and(b))
    }

    /// A new latest checkpoint (ABILITIES.md A-CP-3): store and write the
    /// snapshot (not in time trial).
    ///
    /// # Errors
    /// A failed write.
    pub fn on_checkpoint_saved(&mut self, snapshot: Snapshot) -> Result<(), SaveError> {
        if self.mode == PlayMode::TimeTrial {
            return Ok(());
        }
        let result = self.write(&snapshot);
        self.snapshot = Some(snapshot);
        result
    }

    /// A collectible was picked up in `chapter` (`key` = its actor path
    /// relative to the map). Hidden in time trial (no effect). Writes the
    /// progression when new.
    ///
    /// # Errors
    /// A failed write.
    pub fn on_collectible(
        &mut self,
        chapter: ChapterId,
        key: &str,
    ) -> Result<CollectibleOutcome, SaveError> {
        if self.mode == PlayMode::TimeTrial {
            return Ok(CollectibleOutcome {
                chapter_count: self.progression.collectible_count(chapter),
                total: self.progression.collectible_total(),
                ..CollectibleOutcome::default()
            });
        }
        let out = self.progression.add_collectible(chapter, key);
        if out.new {
            self.write(&self.progression)?;
        }
        Ok(out)
    }

    /// An optional story item was registered; returns (new, all eleven found
    /// now). Writes the progression when new.
    ///
    /// # Errors
    /// A failed write.
    pub fn on_story_item(&mut self, key: &str) -> Result<(bool, bool), SaveError> {
        let out = self.progression.add_story_item(key);
        if out.0 {
            self.write(&self.progression)?;
        }
        Ok(out)
    }

    /// An achievement was earned; `true` when new (progression written).
    /// The platform unlock (Steam id [`Achievement::steam_id`]) is the
    /// caller's.
    ///
    /// # Errors
    /// A failed write.
    pub fn on_achievement(&mut self, achievement: Achievement) -> Result<bool, SaveError> {
        let new = self.progression.achievements.insert(achievement);
        if new {
            self.write(&self.progression)?;
        }
        Ok(new)
    }

    /// Kismet `SeqAct_EditOrAddSaveString`: set the flag and rewrite the
    /// general save at once.
    ///
    /// # Errors
    /// A failed write.
    pub fn on_save_string(&mut self, id: &str, value: i32) -> Result<(), SaveError> {
        self.general.flags.insert(id.to_owned(), value);
        self.write(&self.general)
    }

    /// Kismet `SeqAct_SetGameFinished`: the finished flag (unlocks time
    /// trial).
    ///
    /// # Errors
    /// A failed write.
    pub fn on_game_finished(&mut self) -> Result<(), SaveError> {
        self.progression.finished_game = true;
        self.write(&self.progression)
    }

    /// A time trial finished in `chapter` after `seconds`: stored if it is
    /// the best; the file is written at every finish; five golds unlock
    /// `ALL_GOLD_MEDALS`.
    ///
    /// # Errors
    /// A failed write.
    pub fn on_time_trial_end(
        &mut self,
        chapter: ChapterId,
        seconds: f32,
    ) -> Result<TimeTrialOutcome, SaveError> {
        let new_best = self.time_trial.record(chapter, seconds);
        let medal = self.time_trial.medal(chapter);
        let mut out = TimeTrialOutcome {
            new_best,
            medal,
            all_gold: false,
        };
        self.write(&self.time_trial)?;
        if self.time_trial.all_gold() {
            out.all_gold = self.on_achievement(Achievement::ALL_GOLD_MEDALS)?;
        }
        Ok(out)
    }
}

/// The chapter of a map: its `WorldInfo.Title` matched against the chapter
/// names (the original's rule), else its file name; the front end is never a
/// chapter (its title is also `Workshop`).
#[must_use]
pub fn chapter_of_map(map: &str, title: Option<&str>) -> Option<ChapterId> {
    if map.eq_ignore_ascii_case(FRONT_END_MAP) {
        return None;
    }
    title
        .and_then(ChapterId::from_enum_name)
        .or_else(|| ChapterId::from_map_name(map))
}

// ---------------------------------------------------------------------------
// Game integration
// ---------------------------------------------------------------------------

/// The checkpoint index that became the level's latest during the tick of
/// `report` (`WorldEvent::CheckpointSaved`): the original saves the game
/// then (ABILITIES.md A-CP-3), so callers write [`Game::capture_snapshot`]
/// through [`SaveSession::on_checkpoint_saved`].
#[must_use]
pub fn checkpoint_saved(report: &crate::TickReport) -> Option<i32> {
    report.world.iter().find_map(|e| match e {
        asamu_world::WorldEvent::CheckpointSaved { index } => Some(index),
        _ => None,
    })
}

/// What [`Game::apply_snapshot`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotApplied {
    /// The chapter of the running map.
    pub chapter: Option<ChapterId>,
    /// The checkpoint index the Kismet `SaveGameState_SeqEvent_SavedGameStateLoaded`
    /// events receive (−1 = none stored for this chapter).
    pub checkpoint_index: i32,
    /// The player was reset to a checkpoint.
    pub reset_to_checkpoint: bool,
}

impl Game {
    /// The converted map's package name, if this game runs one.
    #[must_use]
    pub fn map_name(&self) -> Option<&str> {
        self.scene.as_ref().map(|s| s.map.map.as_str())
    }

    /// The chapter of the running map ([`chapter_of_map`]); `None` for
    /// hand-made levels and non-chapter maps.
    #[must_use]
    pub fn chapter(&self) -> Option<ChapterId> {
        let s = self.scene.as_ref()?;
        chapter_of_map(&s.map.map, s.map.world.title.as_deref())
    }

    /// The level's latest registered checkpoint index (converted levels).
    #[must_use]
    pub fn latest_checkpoint_index(&self) -> Option<i32> {
        self.scene
            .as_ref()
            .and_then(|s| s.runtime.checkpoints.latest)
    }

    /// The snapshot the original writes at a new latest checkpoint: the map,
    /// the checkpoint table (`previous`'s entries carried over, this
    /// chapter's latest updated), and the grapple capacity, boots and latch
    /// from the pawn's gun (none without the script layer). Actor and Kismet
    /// entries are carried from `previous` only when it was written by the
    /// same map; the world and Kismet workstreams add theirs.
    #[must_use]
    pub fn capture_snapshot(&self, previous: Option<&Snapshot>) -> Snapshot {
        let map = self
            .map_name()
            .map_or_else(|| self.level.name.clone(), str::to_owned);
        let mut snap = Snapshot {
            checkpoints: previous.map(|p| p.checkpoints.clone()).unwrap_or_default(),
            ..Snapshot::default()
        };
        if let Some(p) = previous.filter(|p| p.map.eq_ignore_ascii_case(&map)) {
            snap.actors = p.actors.clone();
            snap.kismet = p.kismet.clone();
        }
        if let (Some(chapter), Some(latest)) = (self.chapter(), self.latest_checkpoint_index()) {
            snap.checkpoints.insert(chapter, latest);
        }
        if self.player.script.started {
            let gun = &self.player.script.gun;
            snap.abilities = Some(Abilities {
                max_grapples: gun.max_grapples,
                rocket_boots: self.player.script.boots.enabled,
                grapple_enabled: gun.can_grapple,
            });
        }
        snap.map = map;
        snap
    }

    /// Applies a loaded snapshot at the player's spawn (ABILITIES.md A-CP-5):
    /// grapple capacity, boots and latch from the snapshot, then the level's
    /// own start abilities on top (the original's level Kismet runs after
    /// the save load and sets its chapter's values, SAVE.md §5 carry-over);
    /// this chapter's latest checkpoint from the table, and on converted
    /// levels the player reset to the checkpoint the respawn lookup returns
    /// (the first checkpoint when the table has no entry, A-CP-4). Hand-made
    /// levels only take the abilities. Story mode (Workshop, Epilogue) set
    /// at load is kept.
    pub fn apply_snapshot(&mut self, snapshot: &Snapshot) -> SnapshotApplied {
        if let Some(a) = snapshot.abilities {
            grapple_gun::set_max_grapples(&mut self.player, a.max_grapples);
            rocket_boots::enable_rocket_boots(&mut self.player, a.rocket_boots);
            grapple_gun::enable_grapple(&mut self.player, a.grapple_enabled);
        }
        apply_level_abilities(&mut self.player, &self.level);
        let chapter = self.chapter();
        let latest = chapter.and_then(|c| snapshot.checkpoints.get(&c).copied());
        let mut reset = false;
        if let Some(s) = &mut self.scene {
            s.runtime.checkpoints.latest = latest;
            let mut events = Vec::new();
            let count = self.respawn_count;
            let _ = self.scene_reset_player(&mut events);
            // A load is not a respawn; what the teleport touched (volumes,
            // the checkpoint at the spawn point, a death) is reported with
            // the first tick, as the original's teleport fires its touches.
            self.respawn_count = count;
            events.retain(|e| !matches!(e, asamu_world::WorldEvent::PlayerRespawned));
            if let Some(s) = &mut self.scene {
                s.pending.extend(events);
            }
            reset = true;
        }
        SnapshotApplied {
            chapter,
            checkpoint_index: latest.unwrap_or(-1),
            reset_to_checkpoint: reset,
        }
    }
}

// ---------------------------------------------------------------------------
// Original saves (out of scope)
// ---------------------------------------------------------------------------

/// Why the original's save files cannot be imported.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImportError {
    /// Not implemented: three of the four files are AES-256-ECB encrypted
    /// with a key compiled into the original executable (SAVE.md §3.3), which
    /// this repository does not ship; the reader (SAVE.md §9.4: key from the
    /// user's own executable checked against the published SHA-256
    /// fingerprint, tagged-property reader in names-as-strings mode, UE3 JSON
    /// dialect) belongs in `asamu-ue3` + an importer subcommand.
    #[error(
        "importing the original's save files is not implemented (needs the key from your own \
         executable; see docs/reverse-engineering/SAVE.md section 9.4)"
    )]
    NotImplemented,
}

/// Imports the original game's saves from `dir`
/// (`…/A Story About My Uncle/ASAMU/Saves/`). Documented stub: always
/// [`ImportError::NotImplemented`].
///
/// # Errors
/// Always.
pub fn import_original_saves(dir: &Path) -> Result<SaveSession, ImportError> {
    let _ = dir;
    Err(ImportError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile_lite::TempDir, SaveStore) {
        let dir = tempfile_lite::TempDir::new();
        let store = SaveStore::new(dir.path().join("saves"));
        (dir, store)
    }

    /// A tiny self-cleaning temporary directory (`tempfile` is not a
    /// dependency of this crate).
    mod tempfile_lite {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let dir = std::env::temp_dir()
                    .join(format!("asamu-save-test-{}-{n}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn sample_progression() -> Progression {
        let mut p = Progression::default();
        p.unlock(ChapterId::Workshop);
        p.unlock(ChapterId::Sanctuary);
        p.add_collectible(
            ChapterId::Sanctuary,
            "TheWorld.PersistentLevel.ASAMUCollectible_3",
        );
        p.add_story_item("AG-WorkshopNone");
        p.achievements.insert(Achievement::MADDIE_CHALLENGE);
        p.achievements.insert(Achievement::INTERACT_ALL_STORY);
        p
    }

    fn sample_snapshot() -> Snapshot {
        let mut s = Snapshot {
            map: "AG-ParadiseCave".into(),
            ..Snapshot::default()
        };
        s.checkpoints.insert(ChapterId::Sanctuary, 7);
        s.checkpoints.insert(ChapterId::Workshop, 0);
        s.abilities = Some(Abilities {
            max_grapples: 32_767,
            rocket_boots: false,
            grapple_enabled: true,
        });
        s.actors.insert(
            "TheWorld.PersistentLevel.ASAMURechargeCrystal_2".into(),
            json::parse(r#"{"currentState": 1}"#).unwrap(),
        );
        s.kismet.insert(
            "Main_Sequence.SeqEvent_Touch_4".into(),
            json::parse(r#"{"TriggerCount": 2, "bEnabled": false, "ActivationTime": 0.25}"#)
                .unwrap(),
        );
        s
    }

    #[test]
    fn json_round_trips_and_is_deterministic() {
        let text =
            r#"{"b": [1, -2.5, 1e3, true, null, "x\"y\\z\n\u00e9\ud83d\ude00"], "a": {}, "c": []}"#;
        let v = json::parse(text).unwrap();
        let out = json::to_pretty_string(&v).unwrap();
        assert!(out.starts_with("{\n  \"a\": {},"), "{out}");
        assert_eq!(json::parse(&out).unwrap(), v);
        assert_eq!(
            json::to_pretty_string(&json::parse(&out).unwrap()).unwrap(),
            out
        );
        let s = v.get("b").unwrap().as_array().unwrap()[5].as_str().unwrap();
        assert_eq!(s, "x\"y\\z\n\u{e9}\u{1F600}");
        assert_eq!(
            v.get("b").unwrap().as_array().unwrap()[2].as_i32(),
            Some(1000)
        );
    }

    #[test]
    fn json_rejects_malformed_input() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "{\"a\":1,}",
            "01",
            "1.",
            "-",
            "1e",
            "1e999",
            "nul",
            "\"abc",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"a\u{1}b\"",
            "[] []",
            "{1:2}",
            "NaN",
        ] {
            assert!(json::parse(bad).is_err(), "{bad:?}");
        }
        let deep = "[".repeat(json::MAX_DEPTH + 1) + &"]".repeat(json::MAX_DEPTH + 1);
        assert_eq!(json::parse(&deep), Err(json::JsonError::TooDeep));
        let ok = "[".repeat(json::MAX_DEPTH) + &"]".repeat(json::MAX_DEPTH);
        assert!(json::parse(&ok).is_ok());
        assert_eq!(
            json::to_pretty_string(&Value::Number(f64::NAN)),
            Err(json::JsonError::NonFinite)
        );
    }

    #[test]
    fn json_fuzz_truncations_never_panic() {
        let doc = encode_document(&sample_snapshot()).unwrap();
        let body = doc.trim_end();
        for end in 0..body.len() {
            if let Some(prefix) = body.get(..end) {
                // Every strict prefix of an object is invalid.
                assert!(decode_document::<Snapshot>(prefix).is_err(), "prefix {end}");
            }
        }
        // Byte flips: never a panic, either an error or some document.
        let bytes = doc.as_bytes();
        for i in (0..bytes.len()).step_by(7) {
            let mut b = bytes.to_vec();
            b[i] ^= 0x5A;
            if let Ok(text) = String::from_utf8(b) {
                let _ = decode_document::<Snapshot>(&text);
            }
        }
    }

    #[test]
    fn f32_values_round_trip_exactly_and_read_short() {
        for x in [
            0.8f32,
            0.1,
            1.0 / 3.0,
            260.0,
            123.456_79,
            f32::MIN_POSITIVE,
            3.4e38,
        ] {
            let v = Value::from_f32(x);
            let text = json::to_pretty_string(&v).unwrap();
            assert_eq!(
                json::parse(&text).unwrap().as_f32(),
                Some(x),
                "{x} -> {text}"
            );
        }
        assert_eq!(
            json::to_pretty_string(&Value::from_f32(0.8)).unwrap(),
            "0.8\n"
        );
    }

    #[test]
    fn documents_round_trip() {
        let p = sample_progression();
        assert_eq!(
            decode_document::<Progression>(&encode_document(&p).unwrap()).unwrap(),
            p
        );
        let mut g = General {
            current: Some(ChapterId::StarHaven),
            ..General::default()
        };
        g.flags.insert("RevealAirship".into(), 1);
        g.flags.insert("NarratorCollectable".into(), 0);
        assert_eq!(
            decode_document::<General>(&encode_document(&g).unwrap()).unwrap(),
            g
        );
        let s = sample_snapshot();
        assert_eq!(
            decode_document::<Snapshot>(&encode_document(&s).unwrap()).unwrap(),
            s
        );
        let fresh = Snapshot::fresh();
        assert_eq!(
            decode_document::<Snapshot>(&encode_document(&fresh).unwrap()).unwrap(),
            fresh
        );
        let mut t = TimeTrialTimes::default();
        t.record(ChapterId::IceCave, 801.25);
        t.record(ChapterId::Sanctuary, 259.99);
        assert_eq!(
            decode_document::<TimeTrialTimes>(&encode_document(&t).unwrap()).unwrap(),
            t
        );
        let settings = Settings {
            fov_degrees: 100.0,
            mouse_sensitivity: 1.5,
            invert_mouse: true,
            master_volume: 0.5,
            music_volume: 0.0,
            sfx_volume: 1.0,
            voice_volume: 0.3,
            fullscreen: true,
            resolution: Some([1920, 1080]),
            subtitles: false,
        };
        assert_eq!(
            decode_document::<Settings>(&encode_document(&settings).unwrap()).unwrap(),
            settings
        );
        // Output is stable (sorted keys).
        assert_eq!(
            encode_document(&s).unwrap(),
            encode_document(&s.clone()).unwrap()
        );
        let text = encode_document(&g).unwrap();
        assert!(
            text.contains("\"format\": \"asamu-decomp/general\""),
            "{text}"
        );
        assert!(text.contains("\"format_version\": 1"), "{text}");
    }

    #[test]
    fn decoding_checks_the_envelope_and_versions() {
        let g = encode_document(&General::default()).unwrap();
        // Wrong document kind.
        assert!(matches!(
            decode_document::<Progression>(&g),
            Err(SaveError::Invalid { .. })
        ));
        // Missing version, version 0, a newer version.
        let no_version = g.replace("\"format_version\": 1,\n", "");
        assert!(matches!(
            decode_document::<General>(&no_version),
            Err(SaveError::Invalid { .. })
        ));
        let v0 = g.replace("\"format_version\": 1", "\"format_version\": 0");
        assert!(decode_document::<General>(&v0).is_err());
        let v9 = g.replace("\"format_version\": 1", "\"format_version\": 9");
        assert!(matches!(
            decode_document::<General>(&v9),
            Err(SaveError::NewerVersion {
                found: 9,
                supported: 1,
                ..
            })
        ));
        // Missing optional fields take defaults; unknown fields are ignored.
        let minimal =
            r#"{"format": "asamu-decomp/progression", "format_version": 1, "future": [1]}"#;
        assert_eq!(
            decode_document::<Progression>(minimal).unwrap(),
            Progression::default()
        );
        // Wrong field shapes are errors, not silent defaults.
        let bad =
            r#"{"format": "asamu-decomp/general", "format_version": 1, "current_chapter": 3}"#;
        assert!(decode_document::<General>(bad).is_err());
        let bad =
            r#"{"format": "asamu-decomp/general", "format_version": 1, "current_chapter": "Mars"}"#;
        assert!(decode_document::<General>(bad).is_err());
        let bad = r#"{"format": "asamu-decomp/snapshot", "format_version": 1}"#;
        assert!(decode_document::<Snapshot>(bad).is_err(), "map is required");
        assert!(decode_document::<General>("[]").is_err());
    }

    #[test]
    fn version_migration_runs_the_upgrade_chain_in_order() {
        // Version 1 is the first format: the real chain is empty, so a v1
        // document loads unchanged.
        assert!(upgrade_steps(Progression::KIND).is_empty());
        // The chain mechanism, exercised with test-only steps for a
        // hypothetical current version 3: v1 renamed `chapters` to
        // `unlocked_chapters` in v2, and v3 added `finished_game` from a
        // legacy `done` flag.
        fn v1_to_v2(mut f: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, SaveError> {
            if let Some(c) = f.remove("chapters") {
                f.insert("unlocked_chapters".into(), c);
            }
            Ok(f)
        }
        fn v2_to_v3(mut f: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, SaveError> {
            let done = f.remove("done").and_then(|d| d.as_bool()).unwrap_or(false);
            f.insert("finished_game".into(), Value::Bool(done));
            Ok(f)
        }
        let steps: [UpgradeStep; 2] = [v1_to_v2, v2_to_v3];
        let v1 = r#"{"format": "asamu-decomp/progression", "format_version": 1,
                     "chapters": ["Workshop", "Village"], "done": true}"#;
        let p: Progression = decode_with_steps(v1, 3, &steps).unwrap();
        assert_eq!(p.unlocked, vec![ChapterId::Workshop, ChapterId::Village]);
        assert!(p.finished_game);
        let v2 = r#"{"format": "asamu-decomp/progression", "format_version": 2,
                     "unlocked_chapters": ["Workshop"], "done": false}"#;
        let p: Progression = decode_with_steps(v2, 3, &steps).unwrap();
        assert_eq!(p.unlocked, vec![ChapterId::Workshop]);
        assert!(!p.finished_game);
        // A gap in the chain is an error, not a silent pass.
        assert!(decode_with_steps::<Progression>(v1, 3, &steps[..1]).is_err());
        // Newer than current is refused.
        assert!(matches!(
            decode_with_steps::<Progression>(v2, 1, &[]),
            Err(SaveError::NewerVersion {
                found: 2,
                supported: 1,
                ..
            })
        ));
    }

    #[test]
    fn store_round_trip_is_atomic_and_leaves_no_temp_files() {
        let (_tmp, store) = temp_store();
        assert!(matches!(store.load::<Progression>(), LoadOutcome::Missing));
        let p = sample_progression();
        store.save(&p).unwrap();
        assert_eq!(store.load::<Progression>().loaded(), Some(p.clone()));
        // Overwrite.
        let mut p2 = p;
        p2.finished_game = true;
        store.save(&p2).unwrap();
        assert_eq!(store.load::<Progression>().loaded(), Some(p2));
        let names: Vec<String> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["progression.json".to_owned()]);
    }

    #[test]
    fn corrupt_and_newer_files_are_quarantined_not_overwritten() {
        let (_tmp, store) = temp_store();
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.path_of::<General>();
        std::fs::write(&path, "{\"format\": \"asamu-decomp/general\", \"format_ver").unwrap();
        match store.load::<General>() {
            LoadOutcome::Unreadable {
                error: SaveError::Json(_),
                quarantined: Some(q),
            } => {
                assert!(q.ends_with("general.corrupt-1.json"), "{}", q.display());
                assert!(q.exists());
            }
            other => panic!("{other:?}"),
        }
        assert!(!path.exists(), "moved aside");
        // A newer version is preserved as well, under the next free name.
        let newer = encode_document(&General::default())
            .unwrap()
            .replace("\"format_version\": 1", "\"format_version\": 2");
        std::fs::write(&path, newer).unwrap();
        match store.load::<General>() {
            LoadOutcome::Unreadable {
                error: SaveError::NewerVersion { .. },
                quarantined: Some(q),
            } => assert!(q.ends_with("general.corrupt-2.json")),
            other => panic!("{other:?}"),
        }
        // Not UTF-8.
        std::fs::write(&path, [0xFF, 0xFE, 0x00]).unwrap();
        assert!(matches!(
            store.load::<General>(),
            LoadOutcome::Unreadable { .. }
        ));
    }

    #[test]
    fn upgrade_from_a_nonexistent_version_zero_is_an_error_not_a_panic() {
        fn id(f: BTreeMap<String, Value>) -> Result<BTreeMap<String, Value>, SaveError> {
            Ok(f)
        }
        let steps: [UpgradeStep; 1] = [id];
        assert!(upgrade_document("progression", BTreeMap::new(), 0, 2, &steps).is_err());
        assert!(upgrade_document("progression", BTreeMap::new(), 1, 2, &steps).is_ok());
        assert!(upgrade_document("progression", BTreeMap::new(), u32::MAX, u32::MAX, &[]).is_ok());
    }

    #[test]
    fn oversized_files_are_refused_and_set_aside() {
        let (_tmp, store) = temp_store();
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.path_of::<Snapshot>();
        // A sparse file just over the limit (no 16 MiB write).
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_DOCUMENT_BYTES + 1)
            .unwrap();
        match store.load::<Snapshot>() {
            LoadOutcome::Unreadable {
                error: SaveError::TooLarge(_),
                quarantined: Some(q),
            } => assert!(q.ends_with("snapshot.corrupt-1.json")),
            other => panic!("{other:?}"),
        }
        assert!(!path.exists());
    }

    #[test]
    fn failed_writes_keep_the_old_file_and_leave_no_temporary_file() {
        let (_tmp, store) = temp_store();
        std::fs::create_dir_all(store.dir()).unwrap();
        // A stale temporary file from a crash does not get in the way.
        let tmp = store.dir().join(".general.json.tmp");
        std::fs::write(&tmp, "half a docu").unwrap();
        store.save(&General::default()).unwrap();
        assert!(!tmp.exists());
        assert!(store.load::<General>().loaded().is_some());
        // The rename onto the target fails (a directory is in the way): an
        // error, the target untouched, the temporary file removed.
        let target = store.path_of::<Progression>();
        std::fs::create_dir_all(target.join("keep")).unwrap();
        assert!(matches!(
            store.save(&sample_progression()),
            Err(SaveError::Io { .. })
        ));
        assert!(target.join("keep").is_dir());
        assert!(!store.dir().join(".progression.json.tmp").exists());
    }

    #[test]
    fn unreadable_files_that_cannot_be_set_aside_are_never_overwritten() {
        let (_tmp, store) = temp_store();
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.path_of::<Progression>();
        std::fs::write(&path, "not json").unwrap();
        // Every quarantine slot taken: the file cannot be moved aside.
        for n in 1..=MAX_QUARANTINE_SLOTS {
            std::fs::write(
                store.dir().join(format!("progression.corrupt-{n}.json")),
                "",
            )
            .unwrap();
        }
        let mut s = SaveSession::open(store.clone());
        assert!(!s.legible());
        assert!(
            s.issues()
                .iter()
                .any(|i| i.kind == "progression" && i.quarantined.is_none())
        );
        assert!(
            s.issues()
                .iter()
                .any(|i| matches!(i.error, SaveError::Protected(_))),
            "the write-back was refused"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        // Later progression writes are refused too; other documents work.
        assert!(matches!(
            s.begin_level("AG-Workshop", Some("Workshop")),
            Err(SaveError::Protected(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        assert_eq!(
            store.load::<General>().loaded().and_then(|g| g.current),
            Some(ChapterId::Workshop)
        );
        assert!(s.on_save_string("RevealAirship", 1).is_ok());
    }

    #[test]
    fn json_parser_survives_arbitrary_bytes() {
        // Deterministic pseudo-random inputs (LCG) over a JSON-ish alphabet.
        let alphabet = b"{}[]\",:\\/0123456789.eE+-tfnrulasb \n\tu\xc3\xa9";
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        for _ in 0..4000 {
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            if let Ok(text) = String::from_utf8(bytes) {
                if let Ok(v) = json::parse(&text) {
                    // Whatever parses writes and parses back identically.
                    let out = json::to_pretty_string(&v).unwrap();
                    assert_eq!(json::parse(&out).unwrap(), v, "{text:?}");
                }
                let _ = decode_document::<Settings>(&text);
            }
        }
    }

    #[test]
    fn session_first_launch_writes_defaults_and_disables_continue() {
        let (_tmp, store) = temp_store();
        let s = SaveSession::open(store.clone());
        assert!(!s.legible());
        assert!(!s.can_continue());
        assert!(s.snapshot.is_none());
        assert!(store.path_of::<Progression>().exists());
        assert!(store.path_of::<General>().exists());
        // Second launch: both read back fine, but no chapter pointer yet.
        let s = SaveSession::open(store);
        assert!(s.legible());
        assert!(!s.can_continue(), "no pointer: Continue stays off (Q6 fix)");
        assert!(s.issues().is_empty());
    }

    #[test]
    fn story_flow_new_game_checkpoint_continue() {
        let (_tmp, store) = temp_store();
        let mut s = SaveSession::open(store.clone());
        assert_eq!(s.new_game().unwrap(), ChapterId::Workshop);
        let start = s.begin_level("AG-Workshop", Some("Workshop")).unwrap();
        assert_eq!(start.chapter, Some(ChapterId::Workshop));
        assert!(start.newly_unlocked);
        assert_eq!(start.snapshot, Some(Snapshot::fresh()));
        // Kismet transition into the next chapter.
        let start = s.begin_level("AG-ParadiseCave", Some("Sanctuary")).unwrap();
        assert_eq!(start.chapter, Some(ChapterId::Sanctuary));
        let mut snap = start.snapshot.unwrap();
        snap.map = "AG-ParadiseCave".into();
        snap.checkpoints.insert(ChapterId::Sanctuary, 4);
        s.on_checkpoint_saved(snap.clone()).unwrap();
        s.on_save_string("NarratorCollectable", 1).unwrap();
        // Next program run: Continue opens Sanctuary with that snapshot.
        let mut s = SaveSession::open(store);
        assert!(s.can_continue());
        assert_eq!(s.continue_game(), Some(ChapterId::Sanctuary));
        assert_eq!(s.snapshot, Some(snap));
        assert_eq!(
            s.progression.unlocked,
            vec![ChapterId::Workshop, ChapterId::Sanctuary]
        );
        assert_eq!(s.general.save_string("NarratorCollectable"), Some(1));
        assert_eq!(s.general.save_string("RevealAirship"), None);
    }

    #[test]
    fn new_game_resets_only_snapshot_and_pointer() {
        let mut s = SaveSession::in_memory();
        s.begin_level("AG-ParadiseCave", None).unwrap();
        s.on_collectible(ChapterId::Sanctuary, "A").unwrap();
        s.on_achievement(Achievement::FLOOR_IS_LAVA).unwrap();
        s.on_story_item("AG-ParadiseCaveX").unwrap();
        s.on_save_string("RevealAirship", 1).unwrap();
        s.on_game_finished().unwrap();
        s.on_time_trial_end(ChapterId::Sanctuary, 300.0).unwrap();
        s.on_checkpoint_saved(sample_snapshot()).unwrap();
        s.new_game().unwrap();
        assert_eq!(s.snapshot, Some(Snapshot::fresh()));
        assert_eq!(s.general.current, None);
        assert_eq!(
            s.general.save_string("RevealAirship"),
            Some(1),
            "flags kept"
        );
        assert!(s.progression.is_unlocked(ChapterId::Sanctuary));
        assert_eq!(s.progression.collectible_total(), 1);
        assert!(
            s.progression
                .achievements
                .contains(&Achievement::FLOOR_IS_LAVA)
        );
        assert_eq!(s.progression.story_items.len(), 1);
        assert!(s.progression.finished_game);
        assert!(s.time_trial.best.contains_key(&ChapterId::Sanctuary));
        assert!(!s.can_continue());
    }

    #[test]
    fn unreadable_progression_keeps_general_and_vice_versa() {
        let (_tmp, store) = temp_store();
        let mut s = SaveSession::open(store.clone());
        s.begin_level("AG-Darkcave", Some("DarkCave")).unwrap();
        s.on_collectible(ChapterId::DarkCave, "C1").unwrap();
        std::fs::write(store.path_of::<Progression>(), "garbage").unwrap();
        let s = SaveSession::open(store.clone());
        assert!(
            !s.legible(),
            "Continue disabled for this run, as in the original"
        );
        assert_eq!(
            s.general.current,
            Some(ChapterId::DarkCave),
            "general kept (Q2 fix)"
        );
        assert_eq!(s.progression, Progression::default());
        assert_eq!(s.issues().len(), 1);
        assert_eq!(s.issues()[0].kind, "progression");
        assert!(s.issues()[0].quarantined.is_some());
        // Both were written back: the next run is legible again.
        let s = SaveSession::open(store);
        assert!(s.legible());
        assert!(s.can_continue());
    }

    #[test]
    fn chapter_select_requires_unlock_and_overwrites_the_snapshot() {
        let mut s = SaveSession::in_memory();
        assert!(
            s.start_chapter(ChapterId::Workshop).unwrap(),
            "always unlocked"
        );
        assert!(!s.start_chapter(ChapterId::IceCave).unwrap());
        s.begin_level("AG-IceCave", Some("IceCave")).unwrap();
        s.on_checkpoint_saved(sample_snapshot()).unwrap();
        assert!(s.start_chapter(ChapterId::IceCave).unwrap());
        assert_eq!(s.snapshot, Some(Snapshot::fresh()));
        assert_eq!(s.general.current, Some(ChapterId::IceCave), "pointer kept");
    }

    #[test]
    fn front_end_and_unknown_maps_are_not_chapters() {
        assert_eq!(chapter_of_map("ASAMUFrontEndMap", Some("Workshop")), None);
        assert_eq!(chapter_of_map("asamufrontendmap", None), None);
        assert_eq!(
            chapter_of_map("AG-DarkCave", None),
            Some(ChapterId::DarkCave)
        );
        assert_eq!(chapter_of_map("TheCore", None), None);
        assert_eq!(
            chapter_of_map("Whatever", Some("starhaven")),
            Some(ChapterId::StarHaven)
        );
        let mut s = SaveSession::in_memory();
        let start = s.begin_level(FRONT_END_MAP, Some("Workshop")).unwrap();
        assert_eq!(start.chapter, None);
        assert!(s.progression.unlocked.is_empty());
        assert_eq!(s.general.current, None);
    }

    #[test]
    fn time_trial_never_writes_snapshot_or_pointer() {
        let mut s = SaveSession::in_memory();
        assert!(
            !s.start_time_trial(ChapterId::Sanctuary),
            "locked before finishing"
        );
        s.on_game_finished().unwrap();
        assert!(!s.start_time_trial(ChapterId::Workshop), "no trial there");
        assert!(s.start_time_trial(ChapterId::Sanctuary));
        assert_eq!(s.mode(), PlayMode::TimeTrial);
        let start = s.begin_level("AG-ParadiseCave", Some("Sanctuary")).unwrap();
        assert_eq!(start.snapshot, None);
        assert_eq!(s.general.current, None);
        assert!(s.progression.unlocked.is_empty());
        s.on_checkpoint_saved(sample_snapshot()).unwrap();
        assert_eq!(s.snapshot, None);
        let c = s.on_collectible(ChapterId::Sanctuary, "X").unwrap();
        assert!(!c.new, "collectibles are hidden in time trial");
        // Story play again after New Game.
        s.new_game().unwrap();
        assert_eq!(s.mode(), PlayMode::Story);
    }

    #[test]
    fn collectible_notices_and_all_found() {
        let mut p = Progression::default();
        let mut notices = Vec::new();
        let mut all = 0;
        for chapter in ChapterId::WITH_COLLECTIBLES {
            for i in 0..COLLECTIBLES_PER_CHAPTER {
                let key = format!("TheWorld.PersistentLevel.ASAMUCollectible_{i}");
                let out = p.add_collectible(chapter, &key);
                assert!(out.new);
                assert!(!p.add_collectible(chapter, &key).new, "no duplicates");
                if let Some(e) = out.extra_unlocked {
                    notices.push((out.total, e));
                }
                all += usize::from(out.all_found);
            }
        }
        assert_eq!(
            notices,
            vec![
                (10, Extra::BeamColour),
                (15, Extra::GoatMode),
                (20, Extra::MidasMode),
                (25, Extra::ParkourMode)
            ]
        );
        assert_eq!(all, 1);
        assert!(
            p.achievements
                .contains(&Achievement::ALL_COLLECTIBLES_FOUND)
        );
        assert_eq!(p.extras().len(), 4);
        assert_eq!(p.collectible_total(), TOTAL_COLLECTIBLES);
    }

    #[test]
    fn story_items_unlock_the_achievement_at_eleven() {
        let mut p = Progression::default();
        for i in 0..TOTAL_STORY_ITEMS - 1 {
            assert_eq!(p.add_story_item(&format!("k{i}")), (true, false));
        }
        assert_eq!(p.add_story_item("k0"), (false, false));
        assert_eq!(p.add_story_item("last"), (true, true));
        assert!(p.achievements.contains(&Achievement::INTERACT_ALL_STORY));
    }

    #[test]
    fn time_trial_medals_records_and_all_gold() {
        assert_eq!(
            TimeTrialTimes::medal_for(ChapterId::Sanctuary, 260.0),
            Some(Medal::Gold)
        );
        assert_eq!(
            TimeTrialTimes::medal_for(ChapterId::Sanctuary, 260.01),
            Some(Medal::Silver)
        );
        assert_eq!(
            TimeTrialTimes::medal_for(ChapterId::Sanctuary, 320.0),
            Some(Medal::Bronze)
        );
        assert_eq!(TimeTrialTimes::medal_for(ChapterId::Sanctuary, 320.5), None);
        assert_eq!(TimeTrialTimes::medal_for(ChapterId::Sanctuary, 0.0), None);
        assert_eq!(TimeTrialTimes::medal_for(ChapterId::Workshop, 1.0), None);
        let mut s = SaveSession::in_memory();
        let out = s.on_time_trial_end(ChapterId::IceCave, 900.0).unwrap();
        assert!(out.new_best);
        assert_eq!(out.medal, Some(Medal::Silver));
        let out = s.on_time_trial_end(ChapterId::IceCave, 1100.0).unwrap();
        assert!(!out.new_best, "slower runs are not stored");
        assert_eq!(out.medal, Some(Medal::Silver), "medal of the kept best");
        assert_eq!(s.time_trial.best[&ChapterId::IceCave], 900.0);
        let mut gold = 0;
        for (chapter, [g, _, _]) in ChapterId::WITH_COLLECTIBLES
            .into_iter()
            .zip(TIME_TRIAL_TARGETS)
        {
            gold += usize::from(s.on_time_trial_end(chapter, g - 1.0).unwrap().all_gold);
        }
        assert_eq!(gold, 1);
        assert!(
            s.progression
                .achievements
                .contains(&Achievement::ALL_GOLD_MEDALS)
        );
    }

    #[test]
    fn trial_time_format() {
        assert_eq!(format_trial_time(0.0), "00:00:00");
        assert_eq!(format_trial_time(61.257), "01:01:25");
        assert_eq!(format_trial_time(5999.58), "99:59:57");
        assert_eq!(format_trial_time(5999.59), "99:99:99");
        assert_eq!(format_trial_time(f64::NAN), "00:00:00");
    }

    #[test]
    fn identifiers_match_the_original_tables() {
        for (i, c) in ChapterId::ALL.into_iter().enumerate() {
            assert_eq!(c.level_index(), i as i32 + 1);
            assert_eq!(ChapterId::from_level_index(c.level_index()), Some(c));
            assert_eq!(ChapterId::from_enum_name(c.enum_name()), Some(c));
            assert_eq!(
                ChapterId::from_map_name(&c.map_name().to_uppercase()),
                Some(c)
            );
        }
        assert_eq!(ChapterId::from_level_index(0), None);
        assert_eq!(ChapterId::Workshop.next(), Some(ChapterId::Sanctuary));
        assert_eq!(ChapterId::Epilogue.next(), None);
        assert_eq!(ChapterId::Sanctuary.time_trial_slot(), Some(0));
        assert_eq!(ChapterId::IceCave.time_trial_slot(), Some(4));
        for (i, a) in Achievement::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(a.index()), i);
            assert_eq!(a.steam_id(), i as u32 + 1);
            assert_eq!(Achievement::from_name(a.name()), Some(a));
            assert_eq!(Achievement::from_index(a.index()), Some(a));
        }
        assert_eq!(Achievement::INTERACT_ALL_STORY.steam_id(), 15);
    }

    #[test]
    fn settings_are_sanitized() {
        let s = Settings {
            fov_degrees: -1.0,
            mouse_sensitivity: f32::NAN,
            master_volume: 3.0,
            music_volume: -2.0,
            resolution: Some([10, 10]),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(s.fov_degrees, 90.0, "≤ 0 falls back to 90");
        assert_eq!(s.mouse_sensitivity, 1.0);
        assert_eq!(s.master_volume, 1.0);
        assert_eq!(s.music_volume, 0.0);
        assert_eq!(s.resolution, None);
        assert_eq!(
            Settings {
                fov_degrees: 500.0,
                ..Settings::default()
            }
            .sanitized()
            .fov_degrees,
            120.0
        );
        let text =
            r#"{"format": "asamu-decomp/settings", "format_version": 1, "fov_degrees": 1000}"#;
        assert_eq!(
            decode_document::<Settings>(text).unwrap().fov_degrees,
            120.0
        );
    }

    #[test]
    fn graybox_snapshot_capture_and_apply() {
        let mut g = Game::graybox().unwrap();
        assert_eq!(g.chapter(), None);
        assert_eq!(g.map_name(), None);
        let snap = g.capture_snapshot(None);
        let a = snap.abilities.unwrap();
        assert_eq!(a.max_grapples, 3, "graybox test configuration");
        assert!(a.rocket_boots);
        assert!(snap.checkpoints.is_empty());
        // A snapshot with other abilities: restored, then the level's own
        // start abilities (graybox: 3 grapples, boots on) on top.
        let other = Snapshot {
            abilities: Some(Abilities {
                max_grapples: 1,
                rocket_boots: false,
                grapple_enabled: false,
            }),
            ..Snapshot::fresh()
        };
        let applied = g.apply_snapshot(&other);
        assert_eq!(applied.chapter, None);
        assert_eq!(applied.checkpoint_index, -1);
        assert!(!applied.reset_to_checkpoint);
        assert_eq!(g.player().script.gun.max_grapples, 3);
        assert!(g.player().script.boots.enabled);
        assert!(
            !g.player().script.gun.can_grapple,
            "latch from the snapshot"
        );
        // Carry-over of another chapter's table entries.
        let prev = sample_snapshot();
        let next = g.capture_snapshot(Some(&prev));
        assert_eq!(next.checkpoints, prev.checkpoints);
        assert!(
            next.actors.is_empty(),
            "actors only carry within the same map"
        );
    }

    /// A synthetic converted "AG-ParadiseCave" (title `Sanctuary`): a floor,
    /// a player start and checkpoints with indices 0, 1, 2 along +x.
    fn sanctuary_fixture() -> (Game, [u32; 3]) {
        use asamu_world::fixtures::{Place, SceneFixture, json};
        use asamu_world::scene::{self, LoadOptions, MemorySource};
        use glam::Vec3;
        let mut src = MemorySource::new();
        let mut s = SceneFixture::new("AG-ParadiseCave", -10_000.0).with_title("Sanctuary");
        s.set_bsp(
            vec![
                [-5000.0, -5000.0, 0.0],
                [5000.0, -5000.0, 0.0],
                [5000.0, 5000.0, 0.0],
                [-5000.0, 5000.0, 0.0],
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        s.player_start(Vec3::new(-2000.0, 0.0, 80.0), 0);
        let mut ids = [0u32; 3];
        for (i, id) in ids.iter_mut().enumerate() {
            let x = 1000.0 * i as f32;
            let slot = s.checkpoint(
                Place::at(Vec3::new(x, 500.0, 80.0)),
                60.0,
                60.0,
                i as i32,
                json!({}),
            );
            *id = scene::actor_id(0, slot).unwrap();
        }
        s.write(&mut src);
        let loaded = scene::load_map(&src, "AG-ParadiseCave", &LoadOptions::default()).unwrap();
        let mut g =
            Game::from_loaded_map(loaded, asamu_player::PlayerParams::asamu_original(), 60.0)
                .unwrap();
        g.start();
        (g, ids)
    }

    #[test]
    fn converted_level_snapshot_capture_and_apply() {
        use asamu_world::WorldEvent;
        let (mut g, ids) = sanctuary_fixture();
        assert_eq!(g.chapter(), Some(ChapterId::Sanctuary));
        assert_eq!(g.map_name(), Some("AG-ParadiseCave"));
        assert_eq!(g.latest_checkpoint_index(), None);
        // Kismet-style activation of checkpoint 1: the tick reports the save.
        assert!(g.trigger_checkpoint(ids[1]));
        let r = g.tick(&asamu_player::InputFrame::default()).unwrap();
        assert!(r.world.contains(&WorldEvent::CheckpointSaved { index: 1 }));
        assert_eq!(checkpoint_saved(&r), Some(1));
        let idle = g.tick(&asamu_player::InputFrame::default()).unwrap();
        assert_eq!(checkpoint_saved(&idle), None);
        let mut previous = sample_snapshot();
        previous.map = "AG-Workshop".into();
        let snap = g.capture_snapshot(Some(&previous));
        assert_eq!(snap.map, "AG-ParadiseCave");
        assert_eq!(snap.checkpoints.get(&ChapterId::Sanctuary), Some(&1));
        assert_eq!(
            snap.checkpoints.get(&ChapterId::Workshop),
            Some(&0),
            "carried"
        );
        assert!(
            snap.actors.is_empty(),
            "another map's actors are not carried"
        );
        let a = snap.abilities.unwrap();
        assert_eq!(
            a.max_grapples, 2,
            "level-start capacity of the stand-in table"
        );
        assert!(!a.rocket_boots);

        // Continue: a fresh game of the same map with that snapshot resets
        // the player to checkpoint 1 (A-CP-5) without counting a respawn.
        let (mut g2, _) = sanctuary_fixture();
        let mut snap2 = snap.clone();
        snap2.checkpoints.insert(ChapterId::Sanctuary, 2);
        let applied = g2.apply_snapshot(&snap2);
        assert_eq!(applied.chapter, Some(ChapterId::Sanctuary));
        assert_eq!(applied.checkpoint_index, 2);
        assert!(applied.reset_to_checkpoint);
        assert_eq!(g2.latest_checkpoint_index(), Some(2));
        assert_eq!(g2.respawn_count(), 0);
        let p = g2.player().position;
        assert!(
            (p.x - 2000.0).abs() < 1e-3 && (p.y - 500.0).abs() < 1e-3,
            "{p}"
        );
        assert_eq!(g2.active_checkpoint(), Some(ids[2]));
        // The first tick reports what the load's teleport touched (here the
        // checkpoint at the spawn point: activated, no new save), never a
        // respawn.
        let first = g2.tick(&asamu_player::InputFrame::default()).unwrap();
        assert!(
            !first.world.contains(&WorldEvent::PlayerRespawned),
            "a load is not a respawn"
        );
        assert!(!first.respawned);
        assert_eq!(checkpoint_saved(&first), None, "no save at the load");
        assert!(first.world.contains(&WorldEvent::CheckpointActivated {
            id: ids[2],
            index: 2
        }));

        // New Game / chapter select: the fresh snapshot has no entry, so the
        // reset goes to the first checkpoint (A-CP-4) and the index is -1.
        let (mut g3, _) = sanctuary_fixture();
        let applied = g3.apply_snapshot(&Snapshot::fresh());
        assert_eq!(applied.checkpoint_index, -1);
        let p = g3.player().position;
        assert!(
            (p.x - 0.0).abs() < 1e-3 && (p.y - 500.0).abs() < 1e-3,
            "{p}"
        );
        assert_eq!(g3.player().script.gun.max_grapples, 2, "level start kept");
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// user-local `asamu-import levels` output): every converted chapter map
    /// is recognised as its chapter from its own `WorldInfo.Title` (the front
    /// end is not a chapter); New Game / chapter select (fresh snapshot)
    /// reset the player to the first checkpoint, which sits next to the
    /// `PlayerStart`; Continue (a table entry) resets to that checkpoint.
    #[test]
    fn real_data_chapters_titles_and_snapshot_resets() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        if let Ok(g) = Game::load_level(&root, FRONT_END_MAP) {
            assert_eq!(g.chapter(), None, "the front end is not a chapter");
        }
        let mut checked = 0;
        for chapter in ChapterId::ALL {
            let Ok(mut g) = Game::load_level(&root, chapter.map_name()) else {
                eprintln!("{}: not converted, skipped", chapter.map_name());
                continue;
            };
            let title = g.scene_map().and_then(|m| m.world.title.clone());
            assert_eq!(
                title.as_deref(),
                Some(chapter.enum_name()),
                "WorldInfo.Title"
            );
            assert_eq!(g.chapter(), Some(chapter));
            let start = g.player().position;
            let applied = g.apply_snapshot(&Snapshot::fresh());
            assert_eq!(applied.checkpoint_index, -1);
            let first = g.player().position;
            let d = (first - start).truncate().length();
            eprintln!(
                "{}: first checkpoint {d:.0} UU from the PlayerStart",
                chapter.map_name()
            );
            // Continue at the highest checkpoint whose index equals its
            // sorted position (A-CP-4 uses the index as a position).
            let defs = g
                .scene_map()
                .map(|m| m.actors.checkpoints.clone())
                .unwrap_or_default();
            let mut sorted: Vec<_> = defs.iter().collect();
            sorted.sort_by_key(|c| c.index);
            let target = sorted
                .iter()
                .enumerate()
                .filter(|(i, c)| usize::try_from(c.index).ok() == Some(*i))
                .map(|(_, c)| *c)
                .next_back();
            if let Some(cp) = target {
                let (mut g2, mut snap) = (
                    Game::load_level(&root, chapter.map_name()).unwrap(),
                    Snapshot::fresh(),
                );
                snap.checkpoints.insert(chapter, cp.index);
                let applied = g2.apply_snapshot(&snap);
                assert_eq!(applied.checkpoint_index, cp.index);
                assert_eq!(g2.latest_checkpoint_index(), Some(cp.index));
                assert_eq!(g2.respawn_count(), 0, "a load is not a respawn");
                let p = g2.player().position;
                let off = (p - cp.spawn_location).truncate().length();
                eprintln!(
                    "{}: continue at checkpoint {} lands {off:.1} UU from its spawn point",
                    chapter.map_name(),
                    cp.index
                );
                assert!(off < 1.0, "{}: {off} UU off", chapter.map_name());
            }
            checked += 1;
        }
        eprintln!("{checked} chapter maps checked");
    }

    #[test]
    fn import_of_original_saves_is_a_documented_stub() {
        assert_eq!(
            import_original_saves(Path::new("/nonexistent")).err(),
            Some(ImportError::NotImplemented)
        );
    }
}
