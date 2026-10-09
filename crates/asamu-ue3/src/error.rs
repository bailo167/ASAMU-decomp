//! Error type shared by every part of the package reader.

use std::path::PathBuf;

use thiserror::Error;

use crate::lzo::LzoError;

/// Result alias used throughout the crate.
pub type Result<T, E = Ue3Error> = std::result::Result<T, E>;

/// Everything that can go wrong while reading a package.
///
/// Parsing never panics on malformed input; every failure surfaces as one of
/// these variants with enough context (offsets, indices) to locate the problem.
#[derive(Debug, Error)]
pub enum Ue3Error {
    /// The file could not be read.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A read ran past the end of the available bytes.
    #[error(
        "unexpected end of data at offset {offset}: need {needed} bytes, {available} available"
    )]
    UnexpectedEof {
        /// Offset where the read started.
        offset: usize,
        /// Bytes requested.
        needed: usize,
        /// Bytes left.
        available: usize,
    },

    /// A seek target lies outside the data.
    #[error("seek to {target} is outside data of length {len}")]
    BadSeek {
        /// Requested position.
        target: usize,
        /// Data length.
        len: usize,
    },

    /// A serialized scalar has a value the format does not allow
    /// (e.g. a negative count or offset).
    #[error("invalid {what} at offset {offset}: {value}")]
    InvalidValue {
        /// Field description.
        what: &'static str,
        /// Offset of the field.
        offset: usize,
        /// Raw value.
        value: i64,
    },

    /// An array count cannot possibly fit in the bytes that remain.
    #[error(
        "{what} count {count} at offset {offset} needs at least {needed} bytes but only {remaining} remain"
    )]
    CountTooLarge {
        /// Array description.
        what: &'static str,
        /// Offset of the count field.
        offset: usize,
        /// Serialized count.
        count: usize,
        /// Minimum bytes the count implies (saturating).
        needed: usize,
        /// Bytes remaining after the count field.
        remaining: usize,
    },

    /// A serialized FString is malformed.
    #[error("invalid FString at offset {offset}: {reason}")]
    InvalidString {
        /// Offset of the length field.
        offset: usize,
        /// What is wrong.
        reason: &'static str,
    },

    /// The file does not start with the UE3 package tag.
    #[error("not a UE3 package: tag {found:#010x} (expected 0x9E2A83C1)")]
    BadTag {
        /// The tag that was found.
        found: u32,
    },

    /// The file starts with the byte-swapped tag (big-endian console package).
    #[error("big-endian (byte-swapped) packages are not supported")]
    BigEndian,

    /// File/licensee version is not the one this reader understands.
    #[error(
        "unsupported package version {file_version}/{licensee_version} (only 868/0 is supported)"
    )]
    UnsupportedVersion {
        /// File version.
        file_version: u16,
        /// Licensee version.
        licensee_version: u16,
    },

    /// The package uses a compression method this reader does not implement.
    #[error("unsupported compression flags {flags:#x} ({method})")]
    UnsupportedCompression {
        /// Raw CompressionFlags.
        flags: u32,
        /// Human-readable method name.
        method: &'static str,
    },

    /// A compressed chunk (summary entry, chunk header or block table) is inconsistent.
    #[error("compressed chunk {chunk}: {reason}")]
    BadChunk {
        /// Chunk index in the summary table.
        chunk: usize,
        /// What is wrong.
        reason: String,
    },

    /// LZO decompression of one block failed.
    #[error("LZO decompression failed in chunk {chunk} block {block}: {source}")]
    Lzo {
        /// Chunk index.
        chunk: usize,
        /// Block index within the chunk.
        block: usize,
        /// Decompressor error.
        #[source]
        source: LzoError,
    },

    /// A region (table, export payload, header) lies outside the stream.
    #[error("{what} out of bounds: offset {offset} + size {size} exceeds stream length {len}")]
    OutOfBounds {
        /// Region description.
        what: String,
        /// Region start.
        offset: u64,
        /// Region size.
        size: u64,
        /// Stream length.
        len: usize,
    },

    /// An FName refers to a name-table entry that does not exist.
    #[error("name index {index} out of range (name table has {count} entries) in {context}")]
    BadNameIndex {
        /// Serialized name index.
        index: i32,
        /// Name-table size.
        count: usize,
        /// Where the FName was found.
        context: String,
    },

    /// A package index refers to an import or export that does not exist.
    #[error(
        "package index {index} out of range ({imports} imports, {exports} exports) in {context}"
    )]
    BadPackageIndex {
        /// Serialized package index.
        index: i32,
        /// Import-table size.
        imports: usize,
        /// Export-table size.
        exports: usize,
        /// Where the index was found.
        context: String,
    },

    /// An outer chain is longer than the depth limit (or loops).
    #[error("outer chain starting at package index {start} exceeds depth {limit} (cycle?)")]
    OuterChainTooDeep {
        /// Package index the walk started from.
        start: i32,
        /// Depth limit.
        limit: usize,
    },

    /// A decompression worker thread could not be started or panicked.
    #[error("decompression worker failed: {0}")]
    Worker(String),

    /// A declared size exceeds the configured safety limit.
    #[error("{what} of {size} bytes exceeds the limit of {limit} bytes")]
    TooLarge {
        /// What was too large.
        what: &'static str,
        /// Declared size.
        size: u64,
        /// Limit.
        limit: u64,
    },
}
