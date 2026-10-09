//! LZO1X decompression (bounds-checked, safe Rust).
//!
//! STUB: owned by the LZO workstream; replaced by a real implementation.
//! The public API below is fixed and relied upon by the package reader.

use thiserror::Error;

/// Errors produced while decompressing an LZO1X stream.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LzoError {
    /// The decompressor is not implemented yet (stub marker).
    #[error("LZO1X decompression not implemented yet")]
    Unimplemented,
}

/// Decompress one LZO1X block. `dst_len` is the exact expected output size
/// (known from the UE3 block table); producing more or fewer bytes is an error.
pub fn decompress(src: &[u8], dst_len: usize) -> Result<Vec<u8>, LzoError> {
    let _ = (src, dst_len);
    Err(LzoError::Unimplemented)
}
