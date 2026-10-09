//! Error type for reading converted data.

use std::path::PathBuf;

use thiserror::Error;

/// Errors while reading user-local converted data.
///
/// Converted files are written by `asamu-import` on the user's machine, but
/// the runtime still treats them as untrusted input: every problem is an
/// error value, never a panic.
#[derive(Debug, Error)]
pub enum AssetError {
    /// A file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file is larger than the reader accepts.
    #[error("{path} is {size} bytes, more than the {limit}-byte limit for this file type")]
    TooLarge {
        /// File.
        path: PathBuf,
        /// Its size.
        size: u64,
        /// The limit.
        limit: u64,
    },
    /// A JSON document did not parse or did not match the expected shape.
    #[error("parsing {path}: {source}")]
    Json {
        /// File (or a description of the in-memory source).
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
    /// A document declares an unexpected format or version.
    #[error("{path}: expected {expected}, found {found}")]
    Format {
        /// File.
        path: PathBuf,
        /// What the reader understands.
        expected: String,
        /// What the file declares.
        found: String,
    },
    /// A path stored inside a manifest is not a safe relative path.
    #[error("unsafe path {path:?} in a manifest: {reason}")]
    UnsafePath {
        /// The offending path.
        path: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// The requested level has no scene file in the converted directory.
    #[error(
        "no converted scene for level {level:?} in {dir} (run `asamu-import levels --map {level}`)"
    )]
    LevelNotFound {
        /// Level name as requested.
        level: String,
        /// The `levels/` directory searched.
        dir: PathBuf,
    },
}

/// Result alias for this crate.
pub type AssetResult<T> = Result<T, AssetError>;
