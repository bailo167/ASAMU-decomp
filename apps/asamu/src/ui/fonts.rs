//! A system font behind the built-in one, so that the game's 14 languages
//! display.
//!
//! Bevy's built-in font (a subset of Fira Mono) has the 95 printable ASCII
//! characters and nothing else (its character map, read locally). The
//! original's texts need more: a converted install's localization uses 92
//! distinct characters outside ASCII over its 14 languages and the credits,
//! 47 of them in Latin-1 Supplement, 44 in Latin Extended-A, and the right
//! single quotation mark (counted on a local conversion; nothing is copied
//! from it). Without a second font those characters are simply not drawn.
//!
//! No font file is shipped. At start-up the app looks for a font that is
//! already on the machine ([`font_dirs`], [`preferred_names`]: monospaced
//! faces first, since the built-in font is monospaced), checks in its
//! character map that it has the characters the languages need ([`PROBE`],
//! [`covers`]) and registers it with the text renderer. A text that holds a
//! character the built-in font lacks is then drawn with the system font
//! first and the built-in one behind it ([`font_source`]): the whole string
//! in one face, instead of single letters from another. Pure ASCII text
//! keeps the built-in font, so the English screens look as before.
//!
//! `ASAMU_FONT=<file>` names the font to use instead of the search. When no
//! usable font is found the app says so once in its log and runs as before
//! (ASCII only).
//!
//! The readers here take untrusted bytes (any file a user names, any file in
//! a font folder): every offset and count is checked, nothing is trusted
//! beyond the size cap, and a malformed file is "not usable", never a panic.

use std::path::{Path, PathBuf};

use bevy::prelude::*;

/// Environment variable naming a font file to use as the fallback.
pub(crate) const FONT_ENV: &str = "ASAMU_FONT";

/// Largest font file read (ours): the usual text faces are 0.1 to 15 MiB;
/// the multi-script collections that are larger are not needed here.
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;

/// Characters a fallback font must have: at least one that each of the 13
/// translated languages needs and ASCII lacks (BRA/POR ã, CZE ř, DEU ß,
/// ESN ¿, FIN ä, FRA œ, HUN õ, ITA ì, NLD ë, POL ł, SLO ľ, TUR ğ ı İ ş) and
/// the typographic apostrophe. Chosen from the character inventory of a
/// converted install (see the module docs); a font with these has, in
/// practice, the two Latin blocks the texts are written in.
pub(crate) const PROBE: [char; 16] = [
    '\u{e3}', '\u{159}', '\u{df}', '\u{bf}', '\u{e4}', '\u{153}', '\u{f5}', '\u{ec}', '\u{eb}',
    '\u{142}', '\u{13e}', '\u{11f}', '\u{131}', '\u{130}', '\u{15f}', '\u{2019}',
];

/// Whether `text` holds a character the built-in font cannot draw: anything
/// outside printable ASCII that is not white space or a control character.
#[must_use]
pub(crate) fn needs_fallback(text: &str) -> bool {
    text.chars()
        .any(|c| !matches!(c, ' '..='~') && !c.is_whitespace() && !c.is_control())
}

/// The platform whose font folders are searched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux and the other Unix systems.
    Unix,
}

impl Platform {
    /// The platform this build runs on.
    #[must_use]
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Unix
        }
    }
}

/// The font folders of `platform`, most specific first. `var` reads an
/// environment variable (a parameter so that tests need no real one).
#[must_use]
pub(crate) fn font_dirs(platform: Platform, var: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<PathBuf> {
    let home = var("HOME");
    let mut dirs = Vec::new();
    match platform {
        Platform::Windows => {
            let windows = var("WINDIR")
                .or_else(|| var("SystemRoot"))
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            dirs.push(windows.join("Fonts"));
            if let Some(local) = var("LOCALAPPDATA") {
                dirs.push(local.join("Microsoft").join("Windows").join("Fonts"));
            }
        }
        Platform::MacOs => {
            dirs.push(PathBuf::from("/System/Library/Fonts"));
            dirs.push(PathBuf::from("/System/Library/Fonts/Supplemental"));
            dirs.push(PathBuf::from("/Library/Fonts"));
            if let Some(home) = &home {
                dirs.push(home.join("Library").join("Fonts"));
            }
        }
        Platform::Unix => {
            match var("XDG_DATA_HOME") {
                Some(data) => dirs.push(data.join("fonts")),
                None => {
                    if let Some(home) = &home {
                        dirs.push(home.join(".local").join("share").join("fonts"));
                    }
                }
            }
            if let Some(home) = &home {
                dirs.push(home.join(".fonts"));
            }
            dirs.push(PathBuf::from("/usr/local/share/fonts"));
            dirs.push(PathBuf::from("/usr/share/fonts"));
        }
    }
    dirs
}

/// File names to look for, best first: monospaced faces (the built-in font
/// is one), then the platform's general text faces. All of these are fonts
/// the systems install themselves; none is shipped with the app.
#[must_use]
pub(crate) fn preferred_names(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::Windows => &[
            "consola.ttf",
            "CascadiaMono.ttf",
            "cour.ttf",
            "segoeui.ttf",
            "tahoma.ttf",
            "arial.ttf",
        ],
        Platform::MacOs => &[
            "Menlo.ttc",
            "SFNSMono.ttf",
            "Courier New.ttf",
            "Arial Unicode.ttf",
            "Arial.ttf",
            "Helvetica.ttc",
        ],
        Platform::Unix => &[
            "DejaVuSansMono.ttf",
            "LiberationMono-Regular.ttf",
            "NotoSansMono-Regular.ttf",
            "UbuntuMono-R.ttf",
            "FreeMono.ttf",
            "DejaVuSans.ttf",
            "LiberationSans-Regular.ttf",
            "NotoSans-Regular.ttf",
        ],
    }
}

/// How far the search goes below a font folder, and how many folder entries
/// it looks at in all (ours: font folders are shallow; the caps keep a
/// strange folder, or a link loop, from stalling the start).
const SEARCH_DEPTH: usize = 4;
const SEARCH_ENTRIES: usize = 20_000;

/// Every file below `dirs` (folders in order, then breadth-first within each;
/// links to folders are not followed) whose name is one of `names`,
/// compared without case, ordered by the position of its name in `names`.
#[must_use]
pub(crate) fn find_named(dirs: &[PathBuf], names: &[&str]) -> Vec<PathBuf> {
    let mut found: Vec<(usize, PathBuf)> = Vec::new();
    let mut budget = SEARCH_ENTRIES;
    for dir in dirs {
        let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
            std::collections::VecDeque::from([(dir.clone(), 0)]);
        while let Some((folder, depth)) = queue.pop_front() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if depth < SEARCH_DEPTH {
                        queue.push_back((entry.path(), depth + 1));
                    }
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if let Some(rank) = names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
                    found.push((rank, entry.path()));
                }
            }
        }
    }
    // Stable: equal names keep the folder order.
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, path)| path).collect()
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn usize_at(data: &[u8], offset: usize) -> Option<usize> {
    usize::try_from(u32_at(data, offset)?).ok()
}

/// The `cmap` table of the first face in `data` (a TrueType or OpenType
/// file, or a collection of them).
fn cmap_table(data: &[u8]) -> Option<&[u8]> {
    let face = match data.get(..4)? {
        b"ttcf" => {
            if u32_at(data, 8)? == 0 {
                return None;
            }
            usize_at(data, 12)?
        }
        [0, 1, 0, 0] | b"OTTO" | b"true" => 0,
        _ => return None,
    };
    let tables = usize::from(u16_at(data, face.checked_add(4)?)?);
    for i in 0..tables {
        let record = face.checked_add(12)?.checked_add(i.checked_mul(16)?)?;
        if data.get(record..record.checked_add(4)?)? == b"cmap" {
            let offset = usize_at(data, record.checked_add(8)?)?;
            let length = usize_at(data, record.checked_add(12)?)?;
            return data.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

/// Glyph of `c` in a format 4 subtable (`None`: malformed; `Some(0)`: no
/// glyph).
fn glyph_format4(sub: &[u8], c: u32) -> Option<u32> {
    let Ok(c) = u16::try_from(c) else {
        return Some(0);
    };
    let seg_x2 = usize::from(u16_at(sub, 6)?);
    let ends = 14usize;
    let starts = ends.checked_add(seg_x2)?.checked_add(2)?;
    let deltas = starts.checked_add(seg_x2)?;
    let ranges = deltas.checked_add(seg_x2)?;
    for i in 0..seg_x2 / 2 {
        let end = u16_at(sub, ends + 2 * i)?;
        if end < c {
            continue;
        }
        let start = u16_at(sub, starts + 2 * i)?;
        if start > c {
            return Some(0);
        }
        let delta = u16_at(sub, deltas + 2 * i)?;
        let range_at = ranges + 2 * i;
        let range = usize::from(u16_at(sub, range_at)?);
        if range == 0 {
            return Some(u32::from(c.wrapping_add(delta)));
        }
        let at = range_at
            .checked_add(range)?
            .checked_add(2 * usize::from(c - start))?;
        let glyph = u16_at(sub, at)?;
        return Some(if glyph == 0 {
            0
        } else {
            u32::from(glyph.wrapping_add(delta))
        });
    }
    Some(0)
}

/// Glyph of `c` in a format 12 subtable.
fn glyph_format12(sub: &[u8], c: u32) -> Option<u32> {
    let groups = usize_at(sub, 12)?;
    for i in 0..groups {
        let at = 16usize.checked_add(i.checked_mul(12)?)?;
        let start = u32_at(sub, at)?;
        let end = u32_at(sub, at.checked_add(4)?)?;
        if c < start {
            return Some(0);
        }
        if c <= end {
            return Some(u32_at(sub, at.checked_add(8)?)?.wrapping_add(c - start));
        }
    }
    Some(0)
}

/// Whether the first face of the font file `data` maps every character of
/// `chars` to a glyph, by its Unicode character maps (formats 4 and 12).
/// `false` for anything that is not a readable font.
#[must_use]
pub(crate) fn covers(data: &[u8], chars: &[char]) -> bool {
    let Some(cmap) = cmap_table(data) else {
        return false;
    };
    let Some(count) = u16_at(cmap, 2) else {
        return false;
    };
    // Unicode subtables: platform 0 (any encoding), or platform 3 with
    // encoding 1 (BMP) or 10 (full).
    let mut subtables = Vec::new();
    for i in 0..usize::from(count) {
        let record = 4 + 8 * i;
        let (Some(platform), Some(encoding), Some(offset)) = (
            u16_at(cmap, record),
            u16_at(cmap, record + 2),
            usize_at(cmap, record + 4),
        ) else {
            return false;
        };
        if !(platform == 0 || (platform == 3 && matches!(encoding, 1 | 10))) {
            continue;
        }
        if let Some(sub) = cmap.get(offset..)
            && let Some(format) = u16_at(sub, 0)
            && matches!(format, 4 | 12)
        {
            subtables.push((format, sub));
        }
    }
    if subtables.is_empty() {
        return false;
    }
    chars.iter().all(|c| {
        subtables.iter().any(|(format, sub)| {
            let glyph = match format {
                4 => glyph_format4(sub, u32::from(*c)),
                _ => glyph_format12(sub, u32::from(*c)),
            };
            glyph.is_some_and(|g| g != 0)
        })
    })
}

/// Reads the font file `path` if it is a regular file of a sane size whose
/// first face has every character of [`PROBE`].
///
/// # Errors
/// A sentence saying why the file is not used.
pub(crate) fn read_usable_font(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    if meta.len() > MAX_FONT_BYTES {
        return Err(format!(
            "{}: {} bytes, more than the {MAX_FONT_BYTES} this app reads",
            path.display(),
            meta.len()
        ));
    }
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The file may have grown between the two calls.
    if data.len() as u64 > MAX_FONT_BYTES {
        return Err(format!("{}: too large", path.display()));
    }
    if !covers(&data, &PROBE) {
        return Err(format!(
            "{}: not a font with the characters the game's languages need",
            path.display()
        ));
    }
    Ok(data)
}

/// The first usable font: the file `ASAMU_FONT` names, else the best match
/// in the platform's font folders. `Err` lists what was tried.
fn find_font(
    platform: Platform,
    var: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<(PathBuf, Vec<u8>), String> {
    let mut problems = Vec::new();
    let mut candidates: Vec<PathBuf> = var(FONT_ENV).into_iter().collect();
    candidates.extend(find_named(
        &font_dirs(platform, var),
        preferred_names(platform),
    ));
    for path in candidates {
        match read_usable_font(&path) {
            Ok(data) => return Ok((path, data)),
            Err(problem) => problems.push(problem),
        }
    }
    if problems.is_empty() {
        problems.push("none of the usual fonts is installed".to_owned());
    }
    Err(problems.join("; "))
}

/// The system font behind the built-in one (`None`: not found, text stays
/// ASCII only).
#[derive(Resource, Debug, Default)]
pub(crate) struct FallbackFont {
    /// The registered font.
    pub handle: Option<Handle<Font>>,
    /// The file it was read from.
    pub path: Option<PathBuf>,
}

/// Start-up: finds a system font and registers it.
pub(crate) fn load_fallback_font(
    fonts: Option<ResMut<Assets<Font>>>,
    mut fallback: ResMut<FallbackFont>,
) {
    let Some(mut fonts) = fonts else {
        return;
    };
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    match find_font(Platform::current(), &var) {
        Ok((path, data)) => {
            info!(
                "text: {} is used for characters outside ASCII (set {FONT_ENV} to choose another font)",
                path.display()
            );
            fallback.handle = Some(fonts.add(Font::from_bytes(data)));
            fallback.path = Some(path);
        }
        Err(problems) => warn!(
            "text: no system font for characters outside ASCII, so translated text will have \
             gaps ({problems}); set {FONT_ENV} to a .ttf or .otf file"
        ),
    }
}

/// The font of a text: the system font first, the built-in one behind it,
/// for a text that needs it and when there is one; else the built-in font.
#[must_use]
pub(crate) fn font_source(needs: bool, fallback: Option<&Handle<Font>>) -> FontSource {
    match fallback {
        Some(handle) if needs => FontSource::List(vec![
            FontSource::Handle(handle.clone()),
            FontSource::default(),
        ]),
        _ => FontSource::default(),
    }
}

/// Gives every text the font [`font_source`] chooses for its content. Texts
/// with a font of their own (neither the built-in one nor this module's
/// list) are left alone.
#[allow(clippy::type_complexity)]
pub(crate) fn apply_fallback_font(
    fallback: Res<FallbackFont>,
    mut texts: Query<(AnyOf<(&Text, &TextSpan)>, &mut TextFont)>,
) {
    let Some(handle) = fallback.handle.as_ref() else {
        return;
    };
    let plain = FontSource::default();
    let with_fallback = font_source(true, Some(handle));
    for ((text, span), mut font) in &mut texts {
        if font.font != plain && font.font != with_fallback {
            continue;
        }
        let content = text.map(|t| t.0.as_str()).or(span.map(|s| s.0.as_str()));
        let wanted = if content.is_some_and(needs_fallback) {
            &with_fallback
        } else {
            &plain
        };
        if font.font != *wanted {
            font.font = wanted.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font file of our own with nothing but a character map: an sfnt
    /// header, one table record and a `cmap` with one Unicode subtable.
    fn sfnt(cmap: &[u8]) -> Vec<u8> {
        let mut f = vec![0, 1, 0, 0];
        f.extend_from_slice(&1u16.to_be_bytes()); // one table
        f.extend_from_slice(&[0; 6]); // search fields (not read)
        f.extend_from_slice(b"cmap");
        f.extend_from_slice(&[0; 4]); // checksum (not read)
        f.extend_from_slice(&28u32.to_be_bytes());
        f.extend_from_slice(&(cmap.len() as u32).to_be_bytes());
        f.extend_from_slice(cmap);
        f
    }

    /// A `cmap` header with one subtable for (platform, encoding).
    fn cmap(platform: u16, encoding: u16, subtable: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&platform.to_be_bytes());
        c.extend_from_slice(&encoding.to_be_bytes());
        c.extend_from_slice(&12u32.to_be_bytes());
        c.extend_from_slice(subtable);
        c
    }

    /// A format 4 subtable mapping each `(start, end)` range to glyphs
    /// `start + delta ..` (no glyph index array), plus the closing segment.
    fn format4(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut segments: Vec<(u16, u16, u16)> = ranges.to_vec();
        segments.push((0xffff, 0xffff, 1));
        let n = segments.len() as u16;
        let mut s = Vec::new();
        for v in [4u16, 0, 0, n * 2, 0, 0, 0] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        for (_, end, _) in &segments {
            s.extend_from_slice(&end.to_be_bytes());
        }
        s.extend_from_slice(&0u16.to_be_bytes());
        for (start, _, _) in &segments {
            s.extend_from_slice(&start.to_be_bytes());
        }
        for (_, _, delta) in &segments {
            s.extend_from_slice(&delta.to_be_bytes());
        }
        for _ in &segments {
            s.extend_from_slice(&0u16.to_be_bytes());
        }
        let length = s.len() as u16;
        s[2..4].copy_from_slice(&length.to_be_bytes());
        s
    }

    /// A format 12 subtable with the given `(start, end, first glyph)`
    /// groups.
    fn format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(&12u16.to_be_bytes());
        s.extend_from_slice(&0u16.to_be_bytes());
        s.extend_from_slice(&((16 + 12 * groups.len()) as u32).to_be_bytes());
        s.extend_from_slice(&0u32.to_be_bytes());
        s.extend_from_slice(&(groups.len() as u32).to_be_bytes());
        for (start, end, glyph) in groups {
            for v in [start, end, glyph] {
                s.extend_from_slice(&v.to_be_bytes());
            }
        }
        s
    }

    /// The Latin blocks and general punctuation, as a text font has them.
    fn latin_font() -> Vec<u8> {
        sfnt(&cmap(
            3,
            1,
            &format4(&[(0x20, 0x7e, 1), (0xa0, 0x17f, 1), (0x2010, 0x2027, 1)]),
        ))
    }

    #[test]
    fn ascii_needs_no_fallback_and_translated_text_does() {
        for plain in [
            "",
            "PRESS [LeftMouseButton] TO INTERACT",
            "a\nb\tc ~",
            "1/5",
        ] {
            assert!(!needs_fallback(plain), "{plain:?}");
        }
        for text in [
            "Pokra\u{10d}ovat",
            "\u{bf}Qu\u{e9}?",
            "na\u{ef}ve",
            "it\u{2019}s",
        ] {
            assert!(needs_fallback(text), "{text:?}");
        }
        // Every probe character is one the built-in font lacks.
        for c in PROBE {
            assert!(needs_fallback(&c.to_string()), "{c:?}");
        }
    }

    #[test]
    fn character_maps_are_read() {
        let font = latin_font();
        assert!(covers(&font, &PROBE));
        assert!(covers(&font, &['A', '~', '\u{17f}']));
        assert!(!covers(&font, &['\u{180}']), "outside the segments");
        assert!(!covers(&font, &['\u{1f600}']), "outside the BMP");
        // An ASCII-only font, like the built-in one, is refused.
        let ascii = sfnt(&cmap(3, 1, &format4(&[(0x20, 0x7e, 1)])));
        assert!(covers(&ascii, &['A']));
        assert!(!covers(&ascii, &PROBE));
        // A segment whose glyphs are all "missing" (delta maps to glyph 0).
        let missing = sfnt(&cmap(
            3,
            1,
            &format4(&[(0xe3, 0xe3, 0u16.wrapping_sub(0xe3))]),
        ));
        assert!(!covers(&missing, &['\u{e3}']));
        // Format 12, platform 0, and the same inside a collection.
        let full = sfnt(&cmap(
            0,
            4,
            &format12(&[
                (0x20, 0x17f, 1),
                (0x2019, 0x2019, 400),
                (0x1f600, 0x1f600, 0),
            ]),
        ));
        assert!(covers(&full, &PROBE));
        assert!(!covers(&full, &['\u{1f600}']), "mapped to glyph 0");
        let mut collection = b"ttcf".to_vec();
        collection.extend_from_slice(&[0, 1, 0, 0]);
        collection.extend_from_slice(&1u32.to_be_bytes());
        collection.extend_from_slice(&16u32.to_be_bytes());
        let face_at = collection.len();
        collection.extend_from_slice(&full);
        // Table offsets in a collection count from the start of the file.
        let offset_at = face_at + 12 + 8;
        let offset = (face_at + 28) as u32;
        collection[offset_at..offset_at + 4].copy_from_slice(&offset.to_be_bytes());
        assert!(covers(&collection, &PROBE));
        // A symbol character map (platform 3, encoding 0) is not Unicode.
        let symbol = sfnt(&cmap(
            3,
            0,
            &format4(&[(0x20, 0x17f, 1), (0x2019, 0x2019, 1)]),
        ));
        assert!(!covers(&symbol, &PROBE));
    }

    #[test]
    fn malformed_fonts_are_refused_without_panicking() {
        let font = latin_font();
        assert!(!covers(&[], &PROBE));
        assert!(!covers(b"not a font at all", &PROBE));
        assert!(!covers(b"ttcf", &PROBE));
        // Every truncation.
        for len in 0..font.len() {
            assert!(!covers(&font[..len], &PROBE), "{len}");
            assert!(!covers(&font[..len], &['A']), "{len}");
        }
        // Every single-byte corruption, and a few values per byte: no panic,
        // whatever the answer.
        for i in 0..font.len() {
            for v in [0x00, 0x7f, 0xff] {
                let mut bad = font.clone();
                bad[i] = v;
                let _ = covers(&bad, &PROBE);
            }
        }
        // Counts and offsets far beyond the data.
        let mut huge = sfnt(&cmap(3, 10, &format12(&[(0x20, 0x17f, 1)])));
        let groups_at = 28 + 12 + 12;
        huge[groups_at..groups_at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!covers(&huge, &PROBE));
        let mut far = latin_font();
        far[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(!covers(&far, &PROBE));
    }

    #[test]
    fn font_folders_follow_the_platform() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| PathBuf::from(v))
            }
        };
        let windows = font_dirs(
            Platform::Windows,
            &env(&[("WINDIR", "W:/Win"), ("LOCALAPPDATA", "U:/Local")]),
        );
        assert_eq!(windows[0], PathBuf::from("W:/Win").join("Fonts"));
        assert!(windows[1].starts_with("U:/Local"));
        // Nothing set: the usual place.
        assert_eq!(
            font_dirs(Platform::Windows, &env(&[])),
            vec![PathBuf::from(r"C:\Windows").join("Fonts")]
        );
        let mac = font_dirs(Platform::MacOs, &env(&[("HOME", "/people/x")]));
        assert_eq!(mac[0], PathBuf::from("/System/Library/Fonts"));
        assert_eq!(mac.last(), Some(&PathBuf::from("/people/x/Library/Fonts")));
        let unix = font_dirs(
            Platform::Unix,
            &env(&[("HOME", "/people/x"), ("XDG_DATA_HOME", "/data")]),
        );
        assert_eq!(unix[0], PathBuf::from("/data/fonts"));
        assert!(unix.contains(&PathBuf::from("/usr/share/fonts")));
        assert_eq!(
            font_dirs(Platform::Unix, &env(&[("HOME", "/people/x")]))[0],
            PathBuf::from("/people/x/.local/share/fonts")
        );
        for platform in [Platform::Windows, Platform::MacOs, Platform::Unix] {
            let names = preferred_names(platform);
            assert!(names.len() >= 4, "{platform:?}");
            assert!(
                names
                    .iter()
                    .all(|n| [".ttf", ".ttc", ".otf"].iter().any(|e| n.ends_with(e)))
            );
        }
    }

    /// A folder of our own under the system's temporary folder, removed on
    /// drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "asamu-fonts-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_search_prefers_the_better_name_and_checks_the_file() {
        let tmp = TempDir::new("search");
        let fonts = tmp.0.join("fonts");
        let deep = fonts.join("truetype").join("dejavu");
        std::fs::create_dir_all(&deep).unwrap();
        // The best name holds no usable font; the second best does, in a
        // sub-folder and in another letter case.
        std::fs::write(fonts.join("DejaVuSansMono.ttf"), b"junk").unwrap();
        std::fs::write(deep.join("liberationmono-regular.TTF"), latin_font()).unwrap();
        std::fs::write(deep.join("Unrelated.ttf"), latin_font()).unwrap();
        let names = preferred_names(Platform::Unix);
        let found = find_named(&[tmp.0.join("missing"), fonts.clone()], names);
        assert_eq!(
            found,
            vec![
                fonts.join("DejaVuSansMono.ttf"),
                deep.join("liberationmono-regular.TTF")
            ]
        );
        assert!(read_usable_font(&found[0]).is_err());
        assert_eq!(read_usable_font(&found[1]).unwrap(), latin_font());
        assert!(read_usable_font(&fonts).is_err(), "a folder");
        assert!(read_usable_font(&tmp.0.join("nothing.ttf")).is_err());

        // The whole choice, with the folders of a made-up Unix home.
        let home = tmp.0.join("home");
        let user_fonts = home.join(".fonts");
        std::fs::create_dir_all(&user_fonts).unwrap();
        std::fs::write(user_fonts.join("FreeMono.ttf"), latin_font()).unwrap();
        let home_for_env = home.clone();
        let var = move |name: &str| (name == "HOME").then(|| home_for_env.clone());
        // Whatever the machine running the test has installed may rank
        // higher; the made-up home's font is found when nothing else is.
        let (path, data) = find_font(Platform::Unix, &var).unwrap();
        assert!(covers(&data, &PROBE));
        assert!(path.is_file());
        // `ASAMU_FONT` goes first; an unusable one falls through to the
        // search.
        let chosen = tmp.0.join("chosen.otf");
        std::fs::write(&chosen, latin_font()).unwrap();
        let home_for_env = home.clone();
        let chosen_for_env = chosen.clone();
        let var = move |name: &str| match name {
            "HOME" => Some(home_for_env.clone()),
            FONT_ENV => Some(chosen_for_env.clone()),
            _ => None,
        };
        assert_eq!(find_font(Platform::Unix, &var).unwrap().0, chosen);
        std::fs::write(&chosen, b"junk").unwrap();
        assert_ne!(find_font(Platform::Unix, &var).unwrap().0, chosen);
    }

    #[test]
    fn texts_get_the_system_font_only_when_they_need_it() {
        let mut fonts = Assets::<Font>::default();
        let handle = fonts.add(Font::from_bytes(latin_font()));
        assert_eq!(font_source(false, Some(&handle)), FontSource::default());
        assert_eq!(font_source(true, None), FontSource::default());
        let with = font_source(true, Some(&handle));
        assert_eq!(
            with,
            FontSource::List(vec![
                FontSource::Handle(handle.clone()),
                FontSource::default()
            ])
        );

        let mut app = App::new();
        app.insert_resource(FallbackFont {
            handle: Some(handle.clone()),
            path: None,
        })
        .add_systems(Update, apply_fallback_font);
        let ascii = app.world_mut().spawn(Text::new("New Game")).id();
        let czech = app.world_mut().spawn(Text::new("Pokra\u{10d}ovat")).id();
        let span = app.world_mut().spawn(TextSpan::new("\u{15e}imdi")).id();
        let own = FontSource::Family("Some Family".into());
        let custom = app
            .world_mut()
            .spawn((
                Text::new("\u{e9}"),
                TextFont {
                    font: own.clone(),
                    ..default()
                },
            ))
            .id();
        app.update();
        let font = |app: &App, e: Entity| app.world().get::<TextFont>(e).unwrap().font.clone();
        assert_eq!(font(&app, ascii), FontSource::default());
        assert_eq!(font(&app, czech), with);
        assert_eq!(font(&app, span), with);
        assert_eq!(font(&app, custom), own, "a font of its own is kept");
        // The text changes (a subtitle line, a language switch): the font
        // follows, both ways.
        app.world_mut().get_mut::<Text>(czech).unwrap().0 = "Continue".into();
        app.world_mut().get_mut::<Text>(ascii).unwrap().0 = "Nov\u{e1} hra".into();
        app.update();
        assert_eq!(font(&app, czech), FontSource::default());
        assert_eq!(font(&app, ascii), with);
        // Nothing is written when nothing changes (no needless re-layout).
        app.update();
        app.world_mut().clear_trackers();
        app.update();
        assert!(
            !app.world()
                .entity(ascii)
                .get_ref::<TextFont>()
                .unwrap()
                .is_changed()
        );

        // Without a system font every text keeps the built-in one.
        let mut app = App::new();
        app.init_resource::<FallbackFont>()
            .add_systems(Update, apply_fallback_font);
        let czech = app.world_mut().spawn(Text::new("Pokra\u{10d}ovat")).id();
        app.update();
        assert_eq!(font(&app, czech), FontSource::default());
    }
}
