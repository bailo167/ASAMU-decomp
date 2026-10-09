//! Minimal little-endian byte writer, used to re-serialize the package summary
//! for the synthesized uncompressed stream.

use crate::types::{FName, Guid};

/// Growable little-endian output buffer.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// Empty writer.
    pub fn new() -> Self {
        Writer::default()
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True when nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Finish and return the bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Append raw bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Append a `u8`.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Append a `u16`.
    pub fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }

    /// Append a `u32`.
    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    /// Append an `i32`.
    pub fn i32(&mut self, v: i32) {
        self.bytes(&v.to_le_bytes());
    }

    /// Append a `u64`.
    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    /// Append a GUID.
    pub fn guid(&mut self, g: Guid) {
        self.u32(g.a);
        self.u32(g.b);
        self.u32(g.c);
        self.u32(g.d);
    }

    /// Append an FName.
    pub fn fname(&mut self, n: FName) {
        self.i32(n.index);
        self.i32(n.number);
    }

    /// Append an FString. Latin-1 when every char is `<= U+00FF`, otherwise UTF-16LE.
    /// Returns `false` (writing nothing) if the length does not fit in an `i32`.
    pub fn fstring(&mut self, s: &str) -> bool {
        if s.is_empty() {
            self.i32(0);
            return true;
        }
        if s.chars().all(|c| u32::from(c) <= 0xFF) {
            let Some(len) = s.chars().count().checked_add(1) else {
                return false;
            };
            let Ok(len) = i32::try_from(len) else {
                return false;
            };
            self.i32(len);
            for c in s.chars() {
                // Checked above: every char fits in one byte.
                self.u8(u8::try_from(u32::from(c)).unwrap_or(b'?'));
            }
            self.u8(0);
        } else {
            let units: Vec<u16> = s.encode_utf16().collect();
            let Some(len) = units.len().checked_add(1) else {
                return false;
            };
            let Ok(len) = i32::try_from(len) else {
                return false;
            };
            self.i32(-len);
            for u in units {
                self.u16(u);
            }
            self.u16(0);
        }
        true
    }
}
