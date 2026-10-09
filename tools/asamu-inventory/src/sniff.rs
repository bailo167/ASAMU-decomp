//! File-type detection from the first bytes of a file ("magic sniffing").
//!
//! Every reader is bounds-checked: the header slice may be shorter than any structure we
//! look for, in which case the check simply fails.

use serde::{Serialize, Serializer};

/// Bytes of each file kept for sniffing.
pub const SNIFF_BYTES: usize = 4096;

/// UE3 package file tag, little-endian on disk (`C1 83 2A 9E`).
pub const UE3_TAG_LE: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];
/// The same tag byte-swapped (big-endian console packages).
pub const UE3_TAG_BE: [u8; 4] = [0x9E, 0x2A, 0x83, 0xC1];

/// Detected file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileType {
    /// Zero-length file.
    Empty,
    /// UE3 package: tag `C1 83 2A 9E`, then u16 file version and u16 licensee version.
    Ue3Package,
    /// UE3 package with the byte-swapped tag (big-endian platforms).
    Ue3PackageBigEndian,
    /// UE3 tag followed by a zero "version" whose u32 is a power-of-two block size: the
    /// compressed-chunk header used by texture file caches (`.tfc`).
    Ue3CompressedChunks,
    /// UE3 global shader cache (`BMSG` magic).
    Ue3GlobalShaderCache,
    /// Thin Mach-O image.
    MachO,
    /// Universal ("fat") Mach-O.
    MachOFat,
    /// DOS/PE executable (`MZ`).
    Pe,
    /// ELF image.
    Elf,
    /// PNG image.
    Png,
    /// Windows bitmap.
    Bmp,
    /// Truevision TGA (header heuristic).
    Tga,
    /// Windows icon.
    Ico,
    /// Windows cursor.
    Cur,
    /// GIF image.
    Gif,
    /// JPEG image.
    Jpeg,
    /// XML text (`<?xml`).
    Xml,
    /// XML property list.
    XmlPlist,
    /// Binary property list (`bplist00`).
    BinaryPlist,
    /// Text with a UTF-16LE byte-order mark.
    Utf16LeText,
    /// Text with a UTF-16BE byte-order mark.
    Utf16BeText,
    /// UTF-8 text (BOM or non-ASCII but valid UTF-8).
    Utf8Text,
    /// 7-bit ASCII text.
    AsciiText,
    /// Bink video (`BIK`).
    Bink,
    /// Bink 2 video (`KB2`).
    Bink2,
    /// Flash movie (`FWS`/`CWS`/`ZWS`).
    Swf,
    /// Scaleform GFx movie (`GFX`/`CFX`).
    Gfx,
    /// Ogg container (`OggS`).
    Ogg,
    /// RIFF/WAVE audio.
    Wav,
    /// Other RIFF container.
    Riff,
    /// zlib stream (CMF/FLG header check).
    Zlib,
    /// FaceFX data (`FACE`).
    FaceFx,
    /// Compiled HTML Help (`ITSF`).
    Chm,
    /// Rich Text Format (`{\rtf`).
    Rtf,
    /// Symbolic link (not followed).
    Symlink,
    /// Nothing recognised.
    Unknown,
}

impl FileType {
    /// Stable identifier used in JSON and summaries.
    pub fn as_str(self) -> &'static str {
        match self {
            FileType::Empty => "empty",
            FileType::Ue3Package => "ue3-package",
            FileType::Ue3PackageBigEndian => "ue3-package-be",
            FileType::Ue3CompressedChunks => "ue3-compressed-chunks",
            FileType::Ue3GlobalShaderCache => "ue3-global-shader-cache",
            FileType::MachO => "mach-o",
            FileType::MachOFat => "mach-o-fat",
            FileType::Pe => "pe",
            FileType::Elf => "elf",
            FileType::Png => "png",
            FileType::Bmp => "bmp",
            FileType::Tga => "tga",
            FileType::Ico => "ico",
            FileType::Cur => "cur",
            FileType::Gif => "gif",
            FileType::Jpeg => "jpeg",
            FileType::Xml => "xml",
            FileType::XmlPlist => "xml-plist",
            FileType::BinaryPlist => "binary-plist",
            FileType::Utf16LeText => "utf16le-text",
            FileType::Utf16BeText => "utf16be-text",
            FileType::Utf8Text => "utf8-text",
            FileType::AsciiText => "ascii-text",
            FileType::Bink => "bink",
            FileType::Bink2 => "bink2",
            FileType::Swf => "swf",
            FileType::Gfx => "gfx",
            FileType::Ogg => "ogg",
            FileType::Wav => "wav",
            FileType::Riff => "riff",
            FileType::Zlib => "zlib",
            FileType::FaceFx => "facefx",
            FileType::Chm => "chm",
            FileType::Rtf => "rtf",
            FileType::Symlink => "symlink",
            FileType::Unknown => "unknown",
        }
    }

    /// Native executable image of any platform.
    pub fn is_executable_image(self) -> bool {
        matches!(
            self,
            FileType::MachO | FileType::MachOFat | FileType::Pe | FileType::Elf
        )
    }

    /// Raster image formats.
    pub fn is_image(self) -> bool {
        matches!(
            self,
            FileType::Png
                | FileType::Bmp
                | FileType::Tga
                | FileType::Ico
                | FileType::Cur
                | FileType::Gif
                | FileType::Jpeg
        )
    }
}

impl Serialize for FileType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Result of sniffing one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sniff {
    /// Detected type.
    pub kind: FileType,
    /// Short human detail (architecture, dimensions, chunk header, unknown magic, ...).
    pub detail: Option<String>,
    /// UE3 package file version (u16 after the tag).
    pub ue3_version: Option<u16>,
    /// UE3 package licensee version (u16 after the file version).
    pub ue3_licensee: Option<u16>,
}

impl Sniff {
    fn new(kind: FileType) -> Self {
        Sniff {
            kind,
            detail: None,
            ue3_version: None,
            ue3_licensee: None,
        }
    }

    fn with_detail(kind: FileType, detail: impl Into<String>) -> Self {
        Sniff {
            detail: Some(detail.into()),
            ..Sniff::new(kind)
        }
    }
}

fn bytes<const N: usize>(head: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    head.get(offset..end)?.try_into().ok()
}

fn le_u16(head: &[u8], offset: usize) -> Option<u16> {
    bytes::<2>(head, offset).map(u16::from_le_bytes)
}

fn be_u16(head: &[u8], offset: usize) -> Option<u16> {
    bytes::<2>(head, offset).map(u16::from_be_bytes)
}

fn le_u32(head: &[u8], offset: usize) -> Option<u32> {
    bytes::<4>(head, offset).map(u32::from_le_bytes)
}

fn be_u32(head: &[u8], offset: usize) -> Option<u32> {
    bytes::<4>(head, offset).map(u32::from_be_bytes)
}

fn le_i32(head: &[u8], offset: usize) -> Option<i32> {
    bytes::<4>(head, offset).map(i32::from_le_bytes)
}

fn mach_cpu_name(cpu: u32) -> String {
    match cpu {
        7 => "i386".to_string(),
        0x0100_0007 => "x86_64".to_string(),
        12 => "arm".to_string(),
        0x0100_000C => "arm64".to_string(),
        0x0200_000C => "arm64_32".to_string(),
        18 => "ppc".to_string(),
        0x0100_0012 => "ppc64".to_string(),
        other => format!("cpu{other:#x}"),
    }
}

fn mach_filetype_name(filetype: u32) -> String {
    match filetype {
        1 => "object".to_string(),
        2 => "execute".to_string(),
        6 => "dylib".to_string(),
        8 => "bundle".to_string(),
        other => format!("filetype{other}"),
    }
}

/// Hex of the first (up to) four bytes, for unknown files.
pub fn magic_hex(head: &[u8]) -> String {
    head.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn sniff_mach_thin(head: &[u8]) -> Option<Sniff> {
    let magic = bytes::<4>(head, 0)?;
    let little = match magic {
        [0xCF, 0xFA, 0xED, 0xFE] | [0xCE, 0xFA, 0xED, 0xFE] => true,
        [0xFE, 0xED, 0xFA, 0xCF] | [0xFE, 0xED, 0xFA, 0xCE] => false,
        _ => return None,
    };
    let read = |off| {
        if little {
            le_u32(head, off)
        } else {
            be_u32(head, off)
        }
    };
    let cpu = read(4)?;
    let filetype = read(12)?;
    Some(Sniff::with_detail(
        FileType::MachO,
        format!("{} {}", mach_cpu_name(cpu), mach_filetype_name(filetype)),
    ))
}

fn sniff_mach_fat(head: &[u8]) -> Option<Sniff> {
    let magic = bytes::<4>(head, 0)?;
    let entry_size: usize = match magic {
        [0xCA, 0xFE, 0xBA, 0xBE] => 20,
        [0xCA, 0xFE, 0xBA, 0xBF] => 32,
        _ => return None,
    };
    let count = be_u32(head, 4)?;
    // Java class files share CAFEBABE; their "count" is a large version number.
    if count == 0 || count > 32 {
        return None;
    }
    let mut arches = Vec::new();
    for i in 0..usize::try_from(count).ok()? {
        let offset = i.checked_mul(entry_size)?.checked_add(8)?;
        match be_u32(head, offset) {
            Some(cpu) => arches.push(mach_cpu_name(cpu)),
            None => arches.push("?".to_string()),
        }
    }
    Some(Sniff::with_detail(
        FileType::MachOFat,
        format!("fat {}", arches.join("+")),
    ))
}

fn sniff_pe(head: &[u8]) -> Option<Sniff> {
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let detail = le_u32(head, 0x3C)
        .and_then(|e| usize::try_from(e).ok())
        .and_then(|e| {
            if bytes::<4>(head, e)? != *b"PE\0\0" {
                return None;
            }
            let machine = le_u16(head, e.checked_add(4)?)?;
            Some(match machine {
                0x014C => "pe i386".to_string(),
                0x8664 => "pe x86_64".to_string(),
                0xAA64 => "pe arm64".to_string(),
                other => format!("pe machine {other:#06x}"),
            })
        })
        .unwrap_or_else(|| "mz".to_string());
    Some(Sniff::with_detail(FileType::Pe, detail))
}

fn sniff_ue3(head: &[u8]) -> Option<Sniff> {
    let tag = bytes::<4>(head, 0)?;
    if tag == UE3_TAG_LE {
        let version = le_u16(head, 4)?;
        let licensee = le_u16(head, 6)?;
        if version == 0 {
            // Not a package summary: a compressed-chunk header (tag, block size, then the
            // compressed/uncompressed sizes of the first chunk).
            let block = le_u32(head, 4)?;
            if block.is_power_of_two() && block >= 0x400 {
                let detail = match (le_u32(head, 8), le_u32(head, 12)) {
                    (Some(c), Some(u)) => {
                        format!("chunk block_size={block} first_chunk={c}->{u}")
                    }
                    _ => format!("chunk block_size={block}"),
                };
                return Some(Sniff::with_detail(FileType::Ue3CompressedChunks, detail));
            }
        }
        return Some(Sniff {
            ue3_version: Some(version),
            ue3_licensee: Some(licensee),
            ..Sniff::new(FileType::Ue3Package)
        });
    }
    if tag == UE3_TAG_BE {
        return Some(Sniff {
            ue3_version: be_u16(head, 4),
            ue3_licensee: be_u16(head, 6),
            ..Sniff::new(FileType::Ue3PackageBigEndian)
        });
    }
    None
}

fn sniff_bmp(head: &[u8], file_size: u64) -> Option<Sniff> {
    if head.get(..2)? != b"BM" {
        return None;
    }
    let declared = u64::from(le_u32(head, 2)?);
    let reserved_zero = le_u32(head, 6)? == 0;
    let pixel_offset = u64::from(le_u32(head, 10)?);
    if !(declared == file_size || (reserved_zero && pixel_offset < file_size)) {
        return None;
    }
    let detail = match (
        le_u32(head, 14),
        le_i32(head, 18),
        le_i32(head, 22),
        le_u16(head, 28),
    ) {
        (Some(hdr), Some(w), Some(h), Some(bpp)) if hdr >= 40 => {
            Some(format!("{}x{} {bpp}bpp", w, h.unsigned_abs()))
        }
        _ => None,
    };
    Some(Sniff {
        detail,
        ..Sniff::new(FileType::Bmp)
    })
}

fn sniff_ico(head: &[u8], file_size: u64) -> Option<Sniff> {
    let kind = match bytes::<4>(head, 0)? {
        [0, 0, 1, 0] => FileType::Ico,
        [0, 0, 2, 0] => FileType::Cur,
        _ => return None,
    };
    let count = le_u16(head, 4)?;
    if count == 0 {
        return None;
    }
    let size = u64::from(le_u32(head, 14)?);
    let offset = u64::from(le_u32(head, 18)?);
    let dir_end = 6u64.checked_add(16u64.checked_mul(u64::from(count))?)?;
    if size == 0 || offset < dir_end || offset.checked_add(size)? > file_size {
        return None;
    }
    if kind == FileType::Ico && *head.get(9)? != 0 {
        return None;
    }
    Some(Sniff::with_detail(kind, format!("{count} images")))
}

fn sniff_tga(head: &[u8], file_size: u64, ext: Option<&str>) -> Option<Sniff> {
    if head.len() < 18 {
        return None;
    }
    let id_len = u64::from(*head.first()?);
    let cmap_type = *head.get(1)?;
    let image_type = *head.get(2)?;
    let width = le_u16(head, 12)?;
    let height = le_u16(head, 14)?;
    let depth = *head.get(16)?;
    let plausible = cmap_type <= 1
        && matches!(image_type, 1 | 2 | 3 | 9 | 10 | 11)
        && width > 0
        && height > 0
        && matches!(depth, 8 | 15 | 16 | 24 | 32);
    if !plausible {
        return None;
    }
    let ext_says_tga = ext == Some("tga");
    // Without the extension, require an uncompressed image whose size adds up exactly.
    let exact_size = cmap_type == 0
        && matches!(image_type, 2 | 3)
        && u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|px| px.checked_mul(u64::from(depth).div_ceil(8)))
            .and_then(|b| b.checked_add(18))
            .and_then(|b| b.checked_add(id_len))
            == Some(file_size);
    if !(ext_says_tga || exact_size) {
        return None;
    }
    Some(Sniff::with_detail(
        FileType::Tga,
        format!("{width}x{height} {depth}bpp type{image_type}"),
    ))
}

/// Heuristic text classification of a header slice. `truncated` means the file continues
/// past `head`, so a multi-byte UTF-8 sequence may be cut at the end.
fn sniff_text(head: &[u8], truncated: bool) -> Option<FileType> {
    if head.is_empty() {
        return None;
    }
    let bad_control = |b: u8| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1A | 0x1B);
    if head.iter().any(|&b| b == 0 || bad_control(b) || b == 0x7F) {
        return None;
    }
    if head.is_ascii() {
        return Some(FileType::AsciiText);
    }
    match std::str::from_utf8(head) {
        Ok(_) => Some(FileType::Utf8Text),
        // A truncated trailing sequence (error_len() == None) is fine when the file goes on.
        Err(e) if truncated && e.error_len().is_none() => Some(FileType::Utf8Text),
        Err(_) => None,
    }
}

fn xml_kind(text: &[u8]) -> Option<FileType> {
    let start = text.iter().position(|b| !b.is_ascii_whitespace())?;
    let body = text.get(start..)?;
    if !body.starts_with(b"<?xml") {
        return None;
    }
    let window = body.get(..body.len().min(512))?;
    let has = |needle: &[u8]| window.windows(needle.len()).any(|w| w == needle);
    if has(b"<!DOCTYPE plist") || has(b"<plist") {
        Some(FileType::XmlPlist)
    } else {
        Some(FileType::Xml)
    }
}

fn is_zlib(head: &[u8]) -> bool {
    match (head.first(), head.get(1)) {
        (Some(&cmf), Some(&flg)) => {
            cmf == 0x78
                && matches!(flg, 0x01 | 0x5E | 0x9C | 0xDA)
                && (u16::from(cmf) << 8 | u16::from(flg)) % 31 == 0
        }
        _ => false,
    }
}

/// Sniff a file from its first bytes. `head` is at most [`SNIFF_BYTES`] long; `file_size` is
/// the full size; `ext` is the lowercase extension (used only by weak heuristics).
pub fn sniff(head: &[u8], file_size: u64, ext: Option<&str>) -> Sniff {
    if file_size == 0 {
        return Sniff::new(FileType::Empty);
    }
    if let Some(s) = sniff_ue3(head) {
        return s;
    }
    if head.starts_with(b"BMSG") {
        let detail = le_u32(head, 4).map(|v| format!("u32@4={v}"));
        return Sniff {
            detail,
            ..Sniff::new(FileType::Ue3GlobalShaderCache)
        };
    }
    if let Some(s) = sniff_mach_thin(head).or_else(|| sniff_mach_fat(head)) {
        return s;
    }
    if let Some(s) = sniff_pe(head) {
        return s;
    }
    if head.starts_with(b"\x7FELF") {
        let class = match head.get(4) {
            Some(1) => "elf32",
            Some(2) => "elf64",
            _ => "elf",
        };
        return Sniff::with_detail(FileType::Elf, class);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        let detail = match (be_u32(head, 16), be_u32(head, 20)) {
            (Some(w), Some(h)) => Some(format!("{w}x{h}")),
            _ => None,
        };
        return Sniff {
            detail,
            ..Sniff::new(FileType::Png)
        };
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Sniff::new(FileType::Gif);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Sniff::new(FileType::Jpeg);
    }
    if head.starts_with(b"bplist00") {
        return Sniff::new(FileType::BinaryPlist);
    }
    if head.starts_with(b"BIK") {
        return Sniff::new(FileType::Bink);
    }
    if head.starts_with(b"KB2") {
        return Sniff::new(FileType::Bink2);
    }
    for (magic, kind) in [
        (b"FWS", FileType::Swf),
        (b"CWS", FileType::Swf),
        (b"ZWS", FileType::Swf),
        (b"GFX", FileType::Gfx),
        (b"CFX", FileType::Gfx),
    ] {
        if head.starts_with(magic) {
            let detail = head.get(3).map(|v| format!("version {v}"));
            return Sniff {
                detail,
                ..Sniff::new(kind)
            };
        }
    }
    if head.starts_with(b"OggS") {
        return Sniff::new(FileType::Ogg);
    }
    if head.starts_with(b"RIFF") {
        return match bytes::<4>(head, 8) {
            Some(fourcc) if &fourcc == b"WAVE" => Sniff::new(FileType::Wav),
            Some(fourcc) => Sniff::with_detail(
                FileType::Riff,
                String::from_utf8_lossy(&fourcc).into_owned(),
            ),
            None => Sniff::new(FileType::Riff),
        };
    }
    if head.starts_with(b"FACE") {
        return Sniff::new(FileType::FaceFx);
    }
    if head.starts_with(b"ITSF") {
        return Sniff::new(FileType::Chm);
    }
    if head.starts_with(b"{\\rtf") {
        return Sniff::new(FileType::Rtf);
    }
    if let Some(s) = sniff_bmp(head, file_size) {
        return s;
    }
    if let Some(s) = sniff_ico(head, file_size) {
        return s;
    }
    if let Some(s) = sniff_tga(head, file_size, ext) {
        return s;
    }
    if head.starts_with(&[0xFF, 0xFE]) {
        return Sniff::new(FileType::Utf16LeText);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Sniff::new(FileType::Utf16BeText);
    }
    let truncated = u64::try_from(head.len()).is_ok_and(|n| n < file_size);
    if let Some(rest) = head.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Sniff::new(xml_kind(rest).unwrap_or(FileType::Utf8Text));
    }
    if let Some(text_kind) = sniff_text(head, truncated) {
        return Sniff::new(xml_kind(head).unwrap_or(text_kind));
    }
    if is_zlib(head) {
        return Sniff::new(FileType::Zlib);
    }
    Sniff::with_detail(FileType::Unknown, format!("magic={}", magic_hex(head)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(head: &[u8]) -> FileType {
        sniff(head, head.len() as u64, None).kind
    }

    #[test]
    fn ue3_package_reads_version_and_licensee() {
        let mut head = UE3_TAG_LE.to_vec();
        head.extend_from_slice(&868u16.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&[0; 8]);
        let s = sniff(&head, 1000, Some("u"));
        assert_eq!(s.kind, FileType::Ue3Package);
        assert_eq!((s.ue3_version, s.ue3_licensee), (Some(868), Some(0)));
        // Truncated to the tag alone: reported as unknown rather than panicking.
        assert_eq!(kind(&UE3_TAG_LE), FileType::Unknown);
    }

    #[test]
    fn ue3_big_endian_tag() {
        let mut head = UE3_TAG_BE.to_vec();
        head.extend_from_slice(&[0x03, 0x64, 0x00, 0x01]);
        let s = sniff(&head, 100, None);
        assert_eq!(s.kind, FileType::Ue3PackageBigEndian);
        assert_eq!((s.ue3_version, s.ue3_licensee), (Some(868), Some(1)));
    }

    #[test]
    fn ue3_compressed_chunk_header() {
        let mut head = UE3_TAG_LE.to_vec();
        head.extend_from_slice(&0x0002_0000u32.to_le_bytes());
        head.extend_from_slice(&6970u32.to_le_bytes());
        head.extend_from_slice(&8192u32.to_le_bytes());
        let s = sniff(&head, 100_000, Some("tfc"));
        assert_eq!(s.kind, FileType::Ue3CompressedChunks);
        assert_eq!(
            s.detail.as_deref(),
            Some("chunk block_size=131072 first_chunk=6970->8192")
        );
    }

    #[test]
    fn global_shader_cache() {
        let mut head = b"BMSG".to_vec();
        head.extend_from_slice(&868u32.to_le_bytes());
        let s = sniff(&head, 100, Some("bin"));
        assert_eq!(s.kind, FileType::Ue3GlobalShaderCache);
        assert_eq!(s.detail.as_deref(), Some("u32@4=868"));
    }

    #[test]
    fn mach_o_thin_and_fat() {
        let mut thin = vec![0xCF, 0xFA, 0xED, 0xFE];
        thin.extend_from_slice(&0x0100_0007u32.to_le_bytes());
        thin.extend_from_slice(&3u32.to_le_bytes());
        thin.extend_from_slice(&2u32.to_le_bytes());
        let s = sniff(&thin, 100, None);
        assert_eq!(s.kind, FileType::MachO);
        assert_eq!(s.detail.as_deref(), Some("x86_64 execute"));

        let mut fat = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2];
        for cpu in [7u32, 0x0100_0007] {
            fat.extend_from_slice(&cpu.to_be_bytes());
            fat.extend_from_slice(&[0; 16]);
        }
        let s = sniff(&fat, 100, None);
        assert_eq!(s.kind, FileType::MachOFat);
        assert_eq!(s.detail.as_deref(), Some("fat i386+x86_64"));

        // Truncated fat header: arch list degrades to '?'.
        let s = sniff(&fat[..12], 100, None);
        assert_eq!(s.detail.as_deref(), Some("fat i386+?"));
        // Java class file (CAFEBABE + big version) is not Mach-O.
        let java = [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52];
        assert_ne!(kind(&java), FileType::MachOFat);
    }

    #[test]
    fn pe_and_elf() {
        let mut pe = vec![0u8; 0x48];
        pe[0] = b'M';
        pe[1] = b'Z';
        pe[0x3C] = 0x40;
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        pe[0x44..0x46].copy_from_slice(&0x014Cu16.to_le_bytes());
        let s = sniff(&pe, 1000, Some("exe"));
        assert_eq!(s.kind, FileType::Pe);
        assert_eq!(s.detail.as_deref(), Some("pe i386"));
        assert_eq!(sniff(b"MZ", 2, None).detail.as_deref(), Some("mz"));
        // e_lfanew pointing far outside the header must not panic.
        let mut bad = pe.clone();
        bad[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(sniff(&bad, 1000, None).detail.as_deref(), Some("mz"));
        assert_eq!(kind(b"\x7FELF\x02\x01\x01"), FileType::Elf);
    }

    #[test]
    fn images() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&16u32.to_be_bytes());
        png.extend_from_slice(&8u32.to_be_bytes());
        let s = sniff(&png, 100, None);
        assert_eq!((s.kind, s.detail.as_deref()), (FileType::Png, Some("16x8")));

        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&70u32.to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&4i32.to_le_bytes());
        bmp.extend_from_slice(&(-2i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        let s = sniff(&bmp, 70, Some("bmp"));
        assert_eq!(
            (s.kind, s.detail.as_deref()),
            (FileType::Bmp, Some("4x2 24bpp"))
        );
        // "BM" text that does not satisfy the size/reserved checks is not a bitmap.
        assert_eq!(kind(b"BM is text\n"), FileType::AsciiText);

        let mut ico = vec![0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0];
        ico.extend_from_slice(&40u32.to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        let s = sniff(&ico, 62, Some("ico"));
        assert_eq!(s.kind, FileType::Ico);
        // Offset beyond the file: rejected.
        assert_ne!(sniff(&ico, 30, Some("ico")).kind, FileType::Ico);

        assert_eq!(kind(b"GIF89a...."), FileType::Gif);
        assert_eq!(kind(&[0xFF, 0xD8, 0xFF, 0xE0]), FileType::Jpeg);
    }

    #[test]
    fn tga_heuristic_and_cur_disambiguation() {
        // 00 00 02 00 ... looks like a CUR header with zero images; it is a TGA.
        let mut tga = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        tga.extend_from_slice(&2u16.to_le_bytes());
        tga.extend_from_slice(&2u16.to_le_bytes());
        tga.extend_from_slice(&[32, 8]);
        let size = 18 + 2 * 2 * 4;
        let s = sniff(&tga, size, Some("tga"));
        assert_eq!(s.kind, FileType::Tga);
        assert_eq!(s.detail.as_deref(), Some("2x2 32bpp type2"));
        // Without the extension the exact-size rule still accepts it...
        assert_eq!(sniff(&tga, size, None).kind, FileType::Tga);
        // ...but not when the size does not add up.
        assert_ne!(sniff(&tga, size + 5, None).kind, FileType::Tga);
    }

    #[test]
    fn text_kinds() {
        assert_eq!(kind(b"[Core.System]\r\nPaths=..\n"), FileType::AsciiText);
        assert_eq!(kind("caf\u{e9}\n".as_bytes()), FileType::Utf8Text);
        assert_eq!(kind(b"\xEF\xBB\xBFhello"), FileType::Utf8Text);
        assert_eq!(kind(&[0xFF, 0xFE, b'[', 0]), FileType::Utf16LeText);
        assert_eq!(kind(&[0xFE, 0xFF, 0, b'/']), FileType::Utf16BeText);
        assert_eq!(kind(b"<?xml version=\"1.0\"?>\r\n<Game/>"), FileType::Xml);
        assert_eq!(
            kind(b"<?xml version=\"1.0\"?>\n<!DOCTYPE plist PUBLIC>\n<plist>"),
            FileType::XmlPlist
        );
        assert_eq!(kind(b"bplist00\xd4\x00"), FileType::BinaryPlist);
        assert_eq!(kind(b"{\\rtf1\\ansi"), FileType::Rtf);
        // Truncated multi-byte sequence at the end of a longer file is still text.
        let head = "ab\u{e9}".as_bytes();
        assert_eq!(sniff(&head[..3], 100, None).kind, FileType::Utf8Text);
        assert_ne!(sniff(&head[..3], 3, None).kind, FileType::Utf8Text);
        // Binary with control bytes is not text.
        assert_eq!(kind(&[1, 0, 0, 0, 0xC7, 0x85]).as_str(), "unknown");
    }

    #[test]
    fn media_and_misc() {
        assert_eq!(kind(b"BIKi...."), FileType::Bink);
        assert_eq!(kind(b"KB2j"), FileType::Bink2);
        assert_eq!(kind(b"FWS\x09"), FileType::Swf);
        assert_eq!(kind(b"CWS\x0a"), FileType::Swf);
        let s = sniff(b"GFX\x0a", 4, Some("gfx"));
        assert_eq!(
            (s.kind, s.detail.as_deref()),
            (FileType::Gfx, Some("version 10"))
        );
        assert_eq!(kind(b"OggS\0\x02"), FileType::Ogg);
        assert_eq!(kind(b"RIFF\0\0\0\0WAVEfmt "), FileType::Wav);
        assert_eq!(kind(b"RIFF\0\0\0\0AVI "), FileType::Riff);
        assert_eq!(kind(b"RIFF"), FileType::Riff);
        assert_eq!(kind(b"FACE\xcc\x06\0\0"), FileType::FaceFx);
        assert_eq!(kind(b"ITSF\x03\0\0\0"), FileType::Chm);
        assert_eq!(kind(&[0x78, 0x9C, 0x03, 0x00, 0x00]), FileType::Zlib);
        assert_eq!(sniff(&[], 0, None).kind, FileType::Empty);
    }

    #[test]
    fn unknown_reports_magic() {
        let s = sniff(&[1, 0, 0, 0, 0xC7, 0x85, 0x92, 0xEF], 8, Some("bin"));
        assert_eq!(s.kind, FileType::Unknown);
        assert_eq!(s.detail.as_deref(), Some("magic=01000000"));
    }

    #[test]
    fn every_prefix_of_every_sample_is_safe() {
        let samples: Vec<Vec<u8>> = vec![
            {
                let mut v = UE3_TAG_LE.to_vec();
                v.extend_from_slice(&[0, 0, 2, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
                v
            },
            vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 3, 1, 0, 0, 7],
            vec![0xCF, 0xFA, 0xED, 0xFE, 7, 0, 0, 1],
            b"MZ\x90\0".to_vec(),
            b"BM\x10\0\0\0\0\0\0\0\x0a\0\0\0".to_vec(),
            vec![
                0, 0, 1, 0, 5, 0, 1, 1, 1, 0, 1, 0, 1, 0, 1, 0, 0, 0, 255, 255, 255, 255,
            ],
            vec![
                0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 32, 0,
            ],
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0".to_vec(),
        ];
        for sample in &samples {
            for end in 0..=sample.len() {
                for size in [0u64, end as u64, u64::MAX] {
                    let _ = sniff(&sample[..end], size, Some("tga"));
                    let _ = sniff(&sample[..end], size, None);
                }
            }
        }
    }
}
