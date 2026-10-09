//! UE3 bulk data records (`FUntypedBulkData`) as serialized by package
//! version 868: the 16-byte record header, inline payloads, payloads stored in
//! a separate texture file cache (`.tfc`), and the compressed-payload layout.
//!
//! ```text
//! u32 BulkDataFlags        see [`flags`]
//! i32 ElementCount         number of elements (bytes, for byte bulk data)
//! i32 BulkDataSizeOnDisk   bytes of the stored payload (compressed size when compressed;
//!                          -1 for an unused record stored "separately")
//! i32 BulkDataOffsetInFile absolute offset of the payload: in the uncompressed package
//!                          stream for inline records, in the .tfc file otherwise
//! [SizeOnDisk payload bytes, only when STORE_IN_SEPARATE_FILE is clear]
//! ```
//!
//! A compressed payload (flag [`flags::COMPRESSED_LZO`]) has the same layout
//! as a compressed package chunk:
//!
//! ```text
//! u32 Tag (0x9E2A83C1) | u32 BlockSize | u32 CompressedSize | u32 UncompressedSize
//! ceil(UncompressedSize / BlockSize) x { u32 CompressedSize, u32 UncompressedSize }
//! blocks, back to back, each one complete LZO1X stream
//! ```
//!
//! Evidence (CONFIRMED over every texture of the macOS build; see
//! `docs/reverse-engineering/TEXTURES.md`): every record parses, every inline
//! record's offset equals its own absolute stream position, every `.tfc`
//! record lies inside its file, and every LZO payload decompresses to exactly
//! `ElementCount` bytes.
//!
//! All input is hostile: sizes and offsets are checked before use, allocations
//! are bounded by [`MAX_BULK_SIZE`], and malformed data yields a
//! [`BulkError`] instead of a panic.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::error::Ue3Error;
use crate::lzo::{self, LzoError};
use crate::reader::Reader;
use crate::summary::PACKAGE_TAG;

/// Serialized size of a bulk data record header.
pub const BULK_RECORD_SIZE: usize = 16;

/// Size of the fixed header of a compressed payload.
pub const COMPRESSED_HEADER_SIZE: usize = 16;

/// Size of one block-table entry of a compressed payload.
pub const COMPRESSED_BLOCK_ENTRY_SIZE: usize = 8;

/// Largest payload (stored or decompressed) this module will read or
/// produce. The largest real texture mip is far below this (a 4096 x 4096
/// four-byte texel mip would be 64 MiB).
pub const MAX_BULK_SIZE: usize = 256 * 1024 * 1024;

/// Largest accepted `BlockSize` of a compressed payload. Shipped payloads use
/// 128 KiB.
pub const MAX_COMPRESSED_BLOCK_SIZE: u32 = 16 * 1024 * 1024;

/// Output bytes reserved up front per stored byte when decompressing a
/// payload (the vector still grows to the real size; this only stops a
/// small hostile header from reserving [`MAX_BULK_SIZE`] at once).
const INITIAL_RESERVE_RATIO: usize = 16;

/// Bulk data flag bits. Values are read from the data; names follow UE3
/// conventions. Bits marked CONFIRMED occur in the shipped textures with
/// the stated effect; the others are TENTATIVE names that never occur.
pub mod flags {
    /// The payload is not inline but in a separate file (here: a `.tfc`
    /// texture file cache named by the texture's `TextureFileCacheName`).
    /// CONFIRMED.
    pub const STORE_IN_SEPARATE_FILE: u32 = 0x01;
    /// zlib-compressed payload. TENTATIVE (never set in this build).
    pub const COMPRESSED_ZLIB: u32 = 0x02;
    /// Serialize elements one by one. TENTATIVE (never set).
    pub const FORCE_SINGLE_ELEMENT: u32 = 0x04;
    /// Payload freed after first use. TENTATIVE (never set).
    pub const SINGLE_USE: u32 = 0x08;
    /// LZO1X-compressed payload (UE3 chunk layout). CONFIRMED.
    pub const COMPRESSED_LZO: u32 = 0x10;
    /// The payload was stripped by the cooker: nothing is stored. CONFIRMED.
    pub const UNUSED: u32 = 0x20;
    /// LZX-compressed payload (Xbox 360). TENTATIVE (never set).
    pub const COMPRESSED_LZX: u32 = 0x80;

    /// Every compression bit.
    pub const ANY_COMPRESSION: u32 = COMPRESSED_ZLIB | COMPRESSED_LZO | COMPRESSED_LZX;

    /// Names of the known bits.
    pub const NAMES: &[(u32, &str)] = &[
        (STORE_IN_SEPARATE_FILE, "StoreInSeparateFile"),
        (COMPRESSED_ZLIB, "SerializeCompressedZLIB"),
        (FORCE_SINGLE_ELEMENT, "ForceSingleElementSerialization"),
        (SINGLE_USE, "SingleUse"),
        (COMPRESSED_LZO, "SerializeCompressedLZO"),
        (UNUSED, "Unused"),
        (COMPRESSED_LZX, "SerializeCompressedLZX"),
    ];

    /// Names of the bits set in `value`; unknown bits are shown as hex.
    pub fn describe(value: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = value;
        for &(bit, name) in NAMES {
            if value & bit != 0 {
                out.push(name.to_owned());
                rest &= !bit;
            }
        }
        if rest != 0 {
            out.push(format!("{rest:#x}"));
        }
        out
    }
}

/// Errors from reading, resolving or decompressing bulk data.
#[derive(Debug, Error)]
pub enum BulkError {
    /// A low-level read failed.
    #[error(transparent)]
    Ue3(#[from] Ue3Error),

    /// A record field holds a value the format does not allow.
    #[error("malformed bulk data record at payload offset {offset}: {detail}")]
    Malformed {
        /// Payload offset of the record header.
        offset: usize,
        /// What is wrong.
        detail: String,
    },

    /// A compressed payload's header or block table is inconsistent.
    #[error("malformed compressed bulk payload: {0}")]
    BadCompressed(String),

    /// LZO decompression of one block failed.
    #[error("LZO decompression failed in bulk payload block {block}: {source}")]
    Lzo {
        /// Block index.
        block: usize,
        /// Decompressor error.
        #[source]
        source: LzoError,
    },

    /// The record uses a compression method this reader does not implement.
    #[error("unsupported bulk data compression (flags {flags:#x})")]
    UnsupportedCompression {
        /// Record flags.
        flags: u32,
    },

    /// A size exceeds [`MAX_BULK_SIZE`] or the expected size.
    #[error("bulk payload of {size} bytes exceeds the limit of {limit} bytes")]
    TooLarge {
        /// Declared size.
        size: u64,
        /// Limit.
        limit: u64,
    },

    /// The record is not loadable from the given source (unused, or stored
    /// separately without a file cache).
    #[error("bulk payload not available: {0}")]
    NotAvailable(String),

    /// A stored payload lies outside its file.
    #[error("bulk payload {offset}+{size} lies outside {what} of {len} bytes")]
    OutOfRange {
        /// What the payload was read from.
        what: String,
        /// Start offset.
        offset: u64,
        /// Stored size.
        size: u64,
        /// File or stream length.
        len: u64,
    },

    /// A file cache could not be found or read.
    #[error("texture file cache {name}: {detail}")]
    FileCache {
        /// Cache name.
        name: String,
        /// What went wrong.
        detail: String,
    },
}

/// Where a record's payload is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum BulkStorage {
    /// The cooker stripped the payload ([`flags::UNUSED`]): nothing to load.
    Unused,
    /// Inline in the package, `SizeOnDisk` bytes right after the header
    /// (possibly zero bytes).
    Inline,
    /// In a separate file (a `.tfc` texture file cache).
    SeparateFile,
}

/// Payload compression of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum BulkCompression {
    /// Stored as is.
    None,
    /// LZO1X in the UE3 chunk layout.
    Lzo,
    /// zlib (not implemented; never used by this build).
    Zlib,
    /// LZX (not implemented; never used by this build).
    Lzx,
    /// More than one compression bit set (invalid).
    Conflicting,
}

/// One parsed bulk data record header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BulkDataRecord {
    /// `BulkDataFlags`.
    pub flags: u32,
    /// `ElementCount`.
    pub element_count: i32,
    /// `BulkDataSizeOnDisk`.
    pub size_on_disk: i32,
    /// `BulkDataOffsetInFile`.
    pub offset_in_file: i32,
    /// Payload offset (within the export payload) of the 16-byte header.
    pub header_offset: usize,
}

impl BulkDataRecord {
    /// Where the payload is stored.
    pub fn storage(&self) -> BulkStorage {
        if self.flags & flags::UNUSED != 0 {
            BulkStorage::Unused
        } else if self.flags & flags::STORE_IN_SEPARATE_FILE != 0 {
            BulkStorage::SeparateFile
        } else {
            BulkStorage::Inline
        }
    }

    /// Payload compression.
    pub fn compression(&self) -> BulkCompression {
        match self.flags & flags::ANY_COMPRESSION {
            0 => BulkCompression::None,
            flags::COMPRESSED_LZO => BulkCompression::Lzo,
            flags::COMPRESSED_ZLIB => BulkCompression::Zlib,
            flags::COMPRESSED_LZX => BulkCompression::Lzx,
            _ => BulkCompression::Conflicting,
        }
    }

    /// True when the payload bytes follow the header inside the export
    /// payload (separate-file bit clear), whether or not the record is unused.
    pub fn has_inline_bytes(&self) -> bool {
        self.flags & flags::STORE_IN_SEPARATE_FILE == 0
    }

    /// Payload offset (within the export payload) of the inline bytes.
    pub fn inline_start(&self) -> usize {
        self.header_offset.saturating_add(BULK_RECORD_SIZE)
    }

    /// Number of stored bytes (0 for an unused or negative-size record).
    pub fn stored_len(&self) -> usize {
        usize::try_from(self.size_on_disk).unwrap_or(0)
    }

    /// Decompressed payload size for `element_size`-byte elements, when the
    /// count is non-negative and the product fits.
    pub fn uncompressed_len(&self, element_size: usize) -> Option<usize> {
        usize::try_from(self.element_count)
            .ok()?
            .checked_mul(element_size)
    }

    /// True when the record stores no data at all (unused, or zero elements).
    pub fn is_empty(&self) -> bool {
        self.storage() == BulkStorage::Unused || self.element_count == 0
    }

    /// Names of the set flag bits.
    pub fn flag_names(&self) -> Vec<String> {
        flags::describe(self.flags)
    }

    /// For an inline record: does `BulkDataOffsetInFile` equal the absolute
    /// stream position of its bytes, given the export's `SerialOffset`?
    pub fn inline_offset_matches(&self, serial_offset: i64) -> bool {
        let Ok(start) = i64::try_from(self.inline_start()) else {
            return false;
        };
        serial_offset.checked_add(start) == Some(i64::from(self.offset_in_file))
    }
}

/// Read one record header and, for inline records, skip over its payload.
///
/// Validation: inline records need `0 <= SizeOnDisk <= remaining`; used
/// separate-file records need a non-negative offset and size; every record
/// needs a non-negative `ElementCount` and at most one compression bit;
/// unknown flag bits are rejected.
pub fn read_bulk_record(r: &mut Reader<'_>) -> Result<BulkDataRecord, BulkError> {
    let header_offset = r.position();
    let flags_value = r.read_u32()?;
    let element_count = r.read_i32()?;
    let size_on_disk = r.read_i32()?;
    let offset_in_file = r.read_i32()?;
    let rec = BulkDataRecord {
        flags: flags_value,
        element_count,
        size_on_disk,
        offset_in_file,
        header_offset,
    };
    let bad = |detail: String| BulkError::Malformed {
        offset: header_offset,
        detail,
    };
    let known = flags::NAMES.iter().fold(0u32, |acc, (bit, _)| acc | bit);
    if flags_value & !known != 0 {
        return Err(bad(format!(
            "unknown flag bits {:#x}",
            flags_value & !known
        )));
    }
    if rec.compression() == BulkCompression::Conflicting {
        return Err(bad(format!(
            "more than one compression bit in {flags_value:#x}"
        )));
    }
    if element_count < 0 {
        return Err(bad(format!("negative element count {element_count}")));
    }
    if rec.has_inline_bytes() {
        let size = usize::try_from(size_on_disk)
            .map_err(|_| bad(format!("negative inline size {size_on_disk}")))?;
        if size > r.remaining() {
            return Err(bad(format!(
                "inline size {size} exceeds the {} remaining payload bytes",
                r.remaining()
            )));
        }
        r.skip(size)?;
    } else if rec.storage() == BulkStorage::SeparateFile && (size_on_disk < 0 || offset_in_file < 0)
    {
        return Err(bad(format!(
            "separate-file record with size {size_on_disk} at offset {offset_in_file}"
        )));
    }
    Ok(rec)
}

/// Header and block table of a compressed payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompressedLayout {
    /// `BlockSize`.
    pub block_size: u32,
    /// Header total compressed size (sum of the blocks' compressed sizes).
    pub compressed_size: u32,
    /// Header total uncompressed size.
    pub uncompressed_size: u32,
    /// Block table: (compressed, uncompressed) sizes.
    pub blocks: Vec<(u32, u32)>,
    /// Bytes taken by the header and the block table.
    pub header_len: usize,
}

impl CompressedLayout {
    /// Total bytes the payload occupies (header, table and blocks).
    pub fn total_len(&self) -> usize {
        self.header_len
            .saturating_add(self.compressed_size as usize)
    }
}

/// Parse and validate the header and block table of a compressed payload.
///
/// Checks: the tag, `0 < BlockSize <= MAX_COMPRESSED_BLOCK_SIZE`, a block
/// count of `ceil(UncompressedSize / BlockSize)`, every block but the last
/// exactly `BlockSize` bytes uncompressed, block sums equal to the header
/// totals, and the whole payload inside `bytes`.
pub fn parse_compressed(bytes: &[u8]) -> Result<CompressedLayout, BulkError> {
    let bad = |m: String| BulkError::BadCompressed(m);
    let mut r = Reader::new(bytes);
    let tag = r.read_u32()?;
    if tag != PACKAGE_TAG {
        return Err(bad(format!(
            "tag {tag:#010x}, expected {PACKAGE_TAG:#010x}"
        )));
    }
    let block_size = r.read_u32()?;
    let compressed_size = r.read_u32()?;
    let uncompressed_size = r.read_u32()?;
    if block_size == 0 || block_size > MAX_COMPRESSED_BLOCK_SIZE {
        return Err(bad(format!("block size {block_size}")));
    }
    if uncompressed_size as usize > MAX_BULK_SIZE {
        return Err(BulkError::TooLarge {
            size: u64::from(uncompressed_size),
            limit: MAX_BULK_SIZE as u64,
        });
    }
    let count = uncompressed_size.div_ceil(block_size) as usize;
    let count_offset = r.position();
    r.check_count(
        "compressed bulk block",
        count_offset,
        count,
        COMPRESSED_BLOCK_ENTRY_SIZE,
    )?;
    let mut blocks = Vec::with_capacity(count);
    let (mut sum_c, mut sum_u) = (0u64, 0u64);
    for i in 0..count {
        let c = r.read_u32()?;
        let u = r.read_u32()?;
        let last = i + 1 == count;
        if !last && u != block_size {
            return Err(bad(format!(
                "block {i} holds {u} uncompressed bytes, expected the block size {block_size}"
            )));
        }
        if u == 0 || u > block_size {
            return Err(bad(format!("block {i} uncompressed size {u}")));
        }
        if c == 0 && u != 0 {
            return Err(bad(format!("block {i} has no compressed bytes")));
        }
        sum_c += u64::from(c);
        sum_u += u64::from(u);
        blocks.push((c, u));
    }
    if sum_c != u64::from(compressed_size) || sum_u != u64::from(uncompressed_size) {
        return Err(bad(format!(
            "block sums {sum_c}/{sum_u} differ from header totals {compressed_size}/{uncompressed_size}"
        )));
    }
    let header_len = r.position();
    let total = (header_len as u64).saturating_add(u64::from(compressed_size));
    if total > bytes.len() as u64 {
        return Err(bad(format!(
            "payload needs {total} bytes, only {} available",
            bytes.len()
        )));
    }
    Ok(CompressedLayout {
        block_size,
        compressed_size,
        uncompressed_size,
        blocks,
        header_len,
    })
}

/// Decompress an LZO payload (`bytes` must be exactly the stored payload)
/// into exactly `expected_len` bytes.
pub fn decompress_lzo(bytes: &[u8], expected_len: usize) -> Result<Vec<u8>, BulkError> {
    if expected_len > MAX_BULK_SIZE {
        return Err(BulkError::TooLarge {
            size: expected_len as u64,
            limit: MAX_BULK_SIZE as u64,
        });
    }
    let layout = parse_compressed(bytes)?;
    if layout.uncompressed_size as usize != expected_len {
        return Err(BulkError::BadCompressed(format!(
            "header declares {} uncompressed bytes, the record expects {expected_len}",
            layout.uncompressed_size
        )));
    }
    if layout.total_len() != bytes.len() {
        return Err(BulkError::BadCompressed(format!(
            "payload occupies {} bytes but {} are stored",
            layout.total_len(),
            bytes.len()
        )));
    }
    // The header's sizes are hostile: a few hundred bytes can claim
    // `MAX_BULK_SIZE` of output. Reserve no more than the stored bytes can
    // plausibly expand to and let real output grow the vector.
    let mut out =
        Vec::with_capacity(expected_len.min(bytes.len().saturating_mul(INITIAL_RESERVE_RATIO)));
    let mut pos = layout.header_len;
    for (i, &(c, u)) in layout.blocks.iter().enumerate() {
        let end = pos
            .checked_add(c as usize)
            .filter(|&e| e <= bytes.len())
            .ok_or_else(|| BulkError::BadCompressed(format!("block {i} runs past the payload")))?;
        let block = bytes
            .get(pos..end)
            .ok_or_else(|| BulkError::BadCompressed(format!("block {i} runs past the payload")))?;
        let data = lzo::decompress(block, u as usize)
            .map_err(|source| BulkError::Lzo { block: i, source })?;
        out.extend_from_slice(&data);
        pos = end;
    }
    Ok(out)
}

/// Turn stored payload bytes into exactly `expected_len` element bytes,
/// decompressing when the record says so.
pub fn decode_payload(
    rec: &BulkDataRecord,
    stored: &[u8],
    expected_len: usize,
) -> Result<Vec<u8>, BulkError> {
    match rec.compression() {
        BulkCompression::None => {
            if stored.len() != expected_len {
                return Err(BulkError::Malformed {
                    offset: rec.header_offset,
                    detail: format!(
                        "uncompressed payload holds {} bytes, expected {expected_len}",
                        stored.len()
                    ),
                });
            }
            Ok(stored.to_vec())
        }
        BulkCompression::Lzo => decompress_lzo(stored, expected_len),
        _ => Err(BulkError::UnsupportedCompression { flags: rec.flags }),
    }
}

/// Stored bytes of an inline record, borrowed from its export payload.
pub fn inline_bytes<'a>(rec: &BulkDataRecord, payload: &'a [u8]) -> Result<&'a [u8], BulkError> {
    if !rec.has_inline_bytes() {
        return Err(BulkError::NotAvailable(
            "record is stored in a separate file".to_owned(),
        ));
    }
    let start = rec.inline_start();
    let len = rec.stored_len();
    start
        .checked_add(len)
        .and_then(|end| payload.get(start..end))
        .ok_or(BulkError::OutOfRange {
            what: "export payload".to_owned(),
            offset: start as u64,
            size: len as u64,
            len: payload.len() as u64,
        })
}

/// The `.tfc` texture file caches next to a set of packages, by
/// case-insensitive cache name (the file stem, e.g. `Textures`).
#[derive(Debug, Clone, Default)]
pub struct TextureFileCaches {
    files: BTreeMap<String, (PathBuf, u64)>,
}

impl TextureFileCaches {
    /// Discover the `*.tfc` files directly inside `dirs` (not recursive).
    /// Earlier directories win on name clashes; missing ones are skipped.
    pub fn discover<P: AsRef<Path>>(dirs: &[P]) -> TextureFileCaches {
        let mut files = BTreeMap::new();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(d.as_ref()) else {
                continue;
            };
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                let is_tfc = p
                    .extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("tfc"));
                if !is_tfc {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(&p) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                if let Some(stem) = p.file_stem() {
                    files
                        .entry(stem.to_string_lossy().to_ascii_lowercase())
                        .or_insert((p.clone(), meta.len()));
                }
            }
        }
        TextureFileCaches { files }
    }

    /// Register a cache file explicitly (tests, unusual layouts).
    pub fn insert(&mut self, name: &str, path: PathBuf, len: u64) {
        self.files.insert(name.to_ascii_lowercase(), (path, len));
    }

    /// Known cache names (lower case) with their paths and lengths.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path, u64)> {
        self.files
            .iter()
            .map(|(k, (p, l))| (k.as_str(), p.as_path(), *l))
    }

    /// Path and length of the cache called `name`.
    pub fn get(&self, name: &str) -> Option<(&Path, u64)> {
        self.files
            .get(&name.to_ascii_lowercase())
            .map(|(p, l)| (p.as_path(), *l))
    }

    /// True when no cache was found.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Read the stored bytes of a separate-file record from cache `name`,
    /// after checking that they lie inside the file.
    pub fn read_stored(&self, name: &str, rec: &BulkDataRecord) -> Result<Vec<u8>, BulkError> {
        if rec.storage() != BulkStorage::SeparateFile {
            return Err(BulkError::NotAvailable(format!(
                "record is not stored in a separate file ({:?})",
                rec.storage()
            )));
        }
        let (path, len) = self.get(name).ok_or_else(|| BulkError::FileCache {
            name: name.to_owned(),
            detail: "no such .tfc file next to the packages".to_owned(),
        })?;
        let offset = u64::try_from(rec.offset_in_file).map_err(|_| BulkError::Malformed {
            offset: rec.header_offset,
            detail: format!("negative file offset {}", rec.offset_in_file),
        })?;
        let size = rec.stored_len();
        if size > MAX_BULK_SIZE {
            return Err(BulkError::TooLarge {
                size: size as u64,
                limit: MAX_BULK_SIZE as u64,
            });
        }
        if offset.checked_add(size as u64).is_none_or(|end| end > len) {
            return Err(BulkError::OutOfRange {
                what: format!("{name}.tfc"),
                offset,
                size: size as u64,
                len,
            });
        }
        let io = |e: std::io::Error| BulkError::FileCache {
            name: name.to_owned(),
            detail: e.to_string(),
        };
        let mut f = File::open(path).map_err(io)?;
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf).map_err(io)?;
        Ok(buf)
    }
}

/// The stored (possibly compressed) bytes of `rec`: inline records from
/// `payload` (the export payload the record was read from), separate-file
/// records from cache `cache_name` in `caches`.
pub fn read_stored(
    rec: &BulkDataRecord,
    payload: &[u8],
    caches: Option<&TextureFileCaches>,
    cache_name: Option<&str>,
) -> Result<Vec<u8>, BulkError> {
    match rec.storage() {
        BulkStorage::Unused => Err(BulkError::NotAvailable(
            "record is unused (stripped by the cooker)".to_owned(),
        )),
        BulkStorage::Inline => Ok(inline_bytes(rec, payload)?.to_vec()),
        BulkStorage::SeparateFile => {
            let caches = caches.ok_or_else(|| {
                BulkError::NotAvailable("record needs a texture file cache".to_owned())
            })?;
            let name = cache_name.ok_or_else(|| {
                BulkError::NotAvailable("record needs a texture file cache name".to_owned())
            })?;
            caches.read_stored(name, rec)
        }
    }
}

/// Expected element bytes of `rec` for `element_size`-byte elements,
/// bounded by [`MAX_BULK_SIZE`].
pub fn expected_len(rec: &BulkDataRecord, element_size: usize) -> Result<usize, BulkError> {
    let expected = rec
        .uncompressed_len(element_size)
        .ok_or_else(|| BulkError::Malformed {
            offset: rec.header_offset,
            detail: format!("element count {} overflows", rec.element_count),
        })?;
    if expected > MAX_BULK_SIZE {
        return Err(BulkError::TooLarge {
            size: expected as u64,
            limit: MAX_BULK_SIZE as u64,
        });
    }
    Ok(expected)
}

/// Load the element bytes of `rec` (see [`read_stored`] for where they come
/// from), decompressing when the record says so. The result has exactly
/// `ElementCount * element_size` bytes.
pub fn load(
    rec: &BulkDataRecord,
    payload: &[u8],
    caches: Option<&TextureFileCaches>,
    cache_name: Option<&str>,
    element_size: usize,
) -> Result<Vec<u8>, BulkError> {
    let expected = expected_len(rec, element_size)?;
    let stored = read_stored(rec, payload, caches, cache_name)?;
    decode_payload(rec, &stored, expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(flags: u32, count: i32, size: i32, offset: i32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&flags.to_le_bytes());
        v.extend_from_slice(&count.to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&offset.to_le_bytes());
        v
    }

    #[test]
    fn inline_record_skips_payload() {
        let mut b = record(0, 3, 3, 100);
        b.extend_from_slice(&[1, 2, 3, 9]);
        let mut r = Reader::new(&b);
        let rec = read_bulk_record(&mut r).unwrap();
        assert_eq!(r.position(), 19);
        assert_eq!(rec.storage(), BulkStorage::Inline);
        assert_eq!(inline_bytes(&rec, &b).unwrap(), &[1, 2, 3]);
        assert!(rec.inline_offset_matches(84));
        assert!(!rec.inline_offset_matches(85));
        assert_eq!(load(&rec, &b, None, None, 1).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn separate_and_unused_records() {
        let b = record(0x21, 0, -1, -1);
        let rec = read_bulk_record(&mut Reader::new(&b)).unwrap();
        assert_eq!(rec.storage(), BulkStorage::Unused);
        assert!(load(&rec, &b, None, None, 1).is_err());
        let b = record(0x11, 64, 40, 1000);
        let rec = read_bulk_record(&mut Reader::new(&b)).unwrap();
        assert_eq!(rec.storage(), BulkStorage::SeparateFile);
        assert_eq!(rec.compression(), BulkCompression::Lzo);
        assert!(matches!(
            load(&rec, &b, None, Some("x"), 1),
            Err(BulkError::NotAvailable(_))
        ));
        assert_eq!(
            rec.flag_names(),
            vec!["StoreInSeparateFile", "SerializeCompressedLZO"]
        );
    }

    #[test]
    fn rejects_bad_records() {
        for b in [
            record(0x40, 0, 0, 0),  // unknown bit
            record(0x12, 0, 0, 0),  // two compression bits
            record(0, -1, 0, 0),    // negative count
            record(0, 4, 4, 0),     // inline bytes missing
            record(0, 0, -5, 0),    // negative inline size
            record(0x01, 4, -1, 0), // separate, negative size
            record(0x01, 4, 4, -1), // separate, negative offset
        ] {
            assert!(read_bulk_record(&mut Reader::new(&b)).is_err(), "{b:?}");
        }
        assert!(read_bulk_record(&mut Reader::new(&[0u8; 15])).is_err());
    }

    #[test]
    fn describe_unknown_bits() {
        assert_eq!(flags::describe(0x101), vec!["StoreInSeparateFile", "0x100"]);
    }
}
