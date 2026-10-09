//! Bounds-checked little-endian cursor over untrusted bytes.
//!
//! Every read checks the remaining length first and returns
//! [`Ue3Error::UnexpectedEof`] instead of panicking. Array counts are validated
//! against the bytes that remain (`count * min_element_size <= remaining`)
//! *before* anything is allocated, so a hostile count cannot trigger a huge
//! allocation.

use crate::error::{Result, Ue3Error};
use crate::types::{FName, Guid, PackageIndex};

/// Cursor over a byte slice. Cheap to copy around; never panics.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Cursor at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// Cursor positioned at `pos` (which may equal `data.len()`).
    pub fn at(data: &'a [u8], pos: usize) -> Result<Self> {
        let mut r = Reader::new(data);
        r.seek(pos)?;
        Ok(r)
    }

    /// Current offset from the start of the data.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Total length of the underlying data.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when the underlying data is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Bytes left after the cursor.
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// The underlying data.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Move the cursor to an absolute position (`<= len`).
    pub fn seek(&mut self, target: usize) -> Result<()> {
        if target > self.data.len() {
            return Err(Ue3Error::BadSeek {
                target,
                len: self.data.len(),
            });
        }
        self.pos = target;
        Ok(())
    }

    /// Advance by `n` bytes.
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.read_bytes(n).map(|_| ())
    }

    /// Borrow the next `n` bytes and advance.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let available = self.remaining();
        if n > available {
            return Err(Ue3Error::UnexpectedEof {
                offset: self.pos,
                needed: n,
                available,
            });
        }
        let end = self.pos + n; // cannot overflow: n <= len - pos
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.read_bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// Read a `u8`.
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_array::<1>()?[0])
    }

    /// Read a little-endian `u16`.
    pub fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    /// Read a little-endian `u32`.
    pub fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    /// Read a little-endian `i32`.
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.read_array()?))
    }

    /// Read a little-endian `u64`.
    pub fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    /// Read a little-endian `i64`.
    pub fn read_i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.read_array()?))
    }

    /// Read a little-endian `f32`.
    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.read_array()?))
    }

    /// Read an `i32` that must be `>= 0` (offsets, sizes, counts) and return it as `u32`.
    pub fn read_non_negative(&mut self, what: &'static str) -> Result<u32> {
        let offset = self.pos;
        let v = self.read_i32()?;
        u32::try_from(v).map_err(|_| Ue3Error::InvalidValue {
            what,
            offset,
            value: i64::from(v),
        })
    }

    /// Read a 16-byte GUID.
    pub fn read_guid(&mut self) -> Result<Guid> {
        Ok(Guid {
            a: self.read_u32()?,
            b: self.read_u32()?,
            c: self.read_u32()?,
            d: self.read_u32()?,
        })
    }

    /// Read an FName (`i32` name index + `i32` number). The index is not validated here.
    pub fn read_fname(&mut self) -> Result<FName> {
        Ok(FName {
            index: self.read_i32()?,
            number: self.read_i32()?,
        })
    }

    /// Read a package index (`i32`).
    pub fn read_package_index(&mut self) -> Result<PackageIndex> {
        Ok(PackageIndex(self.read_i32()?))
    }

    /// Read a UE3 FString.
    ///
    /// `len > 0`: `len` single-byte characters including a trailing NUL, decoded
    /// as Latin-1 (UE3 writes the ANSI code page; every shipped name is pure
    /// ASCII, so the mapping of bytes >= 0x80 is not verified against real data).
    /// `len < 0`: `-len` UTF-16LE code units including a trailing NUL.
    /// `len == 0`: empty string.
    pub fn read_fstring(&mut self) -> Result<String> {
        let offset = self.pos;
        let len = self.read_i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let n = usize::try_from(len).map_err(|_| Ue3Error::InvalidString {
                offset,
                reason: "length does not fit in usize",
            })?;
            let bytes = self.read_bytes(n)?;
            let Some((&last, body)) = bytes.split_last() else {
                return Err(Ue3Error::InvalidString {
                    offset,
                    reason: "empty body",
                });
            };
            if last != 0 {
                return Err(Ue3Error::InvalidString {
                    offset,
                    reason: "missing NUL terminator",
                });
            }
            // Latin-1: every byte maps to the code point of the same value.
            Ok(body.iter().map(|&b| char::from(b)).collect())
        } else {
            let units = len.checked_neg().ok_or(Ue3Error::InvalidString {
                offset,
                reason: "length i32::MIN",
            })?;
            let units = usize::try_from(units).map_err(|_| Ue3Error::InvalidString {
                offset,
                reason: "length does not fit in usize",
            })?;
            let byte_len = units.checked_mul(2).ok_or(Ue3Error::InvalidString {
                offset,
                reason: "length overflow",
            })?;
            let bytes = self.read_bytes(byte_len)?;
            let (body, last) = bytes.split_at(byte_len - 2); // units >= 1 so byte_len >= 2
            if last != [0, 0] {
                return Err(Ue3Error::InvalidString {
                    offset,
                    reason: "missing UTF-16 NUL terminator",
                });
            }
            let code_units = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c));
            char::decode_utf16(code_units)
                .collect::<std::result::Result<String, _>>()
                .map_err(|_| Ue3Error::InvalidString {
                    offset,
                    reason: "unpaired UTF-16 surrogate",
                })
        }
    }

    /// Read an `i32` array count and verify that `count * min_element_size`
    /// bytes can still follow. Negative counts are rejected.
    pub fn read_count(&mut self, what: &'static str, min_element_size: usize) -> Result<usize> {
        let offset = self.pos;
        let raw = self.read_i32()?;
        let count = usize::try_from(raw).map_err(|_| Ue3Error::InvalidValue {
            what,
            offset,
            value: i64::from(raw),
        })?;
        self.check_count(what, offset, count, min_element_size)?;
        Ok(count)
    }

    /// Verify that `count` elements of at least `min_element_size` bytes fit in
    /// the remaining data. `offset` is reported in the error.
    pub fn check_count(
        &self,
        what: &'static str,
        offset: usize,
        count: usize,
        min_element_size: usize,
    ) -> Result<()> {
        let min = min_element_size.max(1);
        let remaining = self.remaining();
        match count.checked_mul(min) {
            Some(needed) if needed <= remaining => Ok(()),
            needed => Err(Ue3Error::CountTooLarge {
                what,
                offset,
                count,
                needed: needed.unwrap_or(usize::MAX),
                remaining,
            }),
        }
    }

    /// Read a `TArray<T>`: an `i32` count followed by `count` elements read by `f`.
    /// `min_element_size` is the smallest possible serialized size of one element
    /// (must be a true lower bound; used to reject impossible counts up front).
    pub fn read_tarray<T>(
        &mut self,
        what: &'static str,
        min_element_size: usize,
        mut f: impl FnMut(&mut Self) -> Result<T>,
    ) -> Result<Vec<T>> {
        let count = self.read_count(what, min_element_size)?;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(f(self)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_little_endian() {
        let data = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_u8().unwrap(), 1);
        assert_eq!(r.read_u16().unwrap(), 0x0302);
        assert_eq!(r.read_u32().unwrap(), 0x0706_0504);
        assert_eq!(r.remaining(), 2);
        assert!(matches!(
            r.read_u32(),
            Err(Ue3Error::UnexpectedEof {
                offset: 7,
                needed: 4,
                available: 2
            })
        ));
        // A failed read does not move the cursor.
        assert_eq!(r.position(), 7);
        assert_eq!(r.read_u16().unwrap(), 0x0908);
        assert!(r.read_u8().is_err());
    }

    #[test]
    fn seek_bounds() {
        let data = [0u8; 4];
        assert!(Reader::at(&data, 4).is_ok());
        assert!(Reader::at(&data, 5).is_err());
        let mut r = Reader::new(&data);
        assert!(r.skip(5).is_err());
        assert!(r.skip(4).is_ok());
        assert_eq!(r.remaining(), 0);
    }

    fn fstr(len: i32, body: &[u8]) -> Vec<u8> {
        let mut v = len.to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn fstring_variants() {
        assert_eq!(
            Reader::new(&fstr(5, b"None\0")).read_fstring().unwrap(),
            "None"
        );
        assert_eq!(Reader::new(&fstr(0, b"")).read_fstring().unwrap(), "");
        // Latin-1 high byte.
        assert_eq!(
            Reader::new(&fstr(2, &[0xE9, 0])).read_fstring().unwrap(),
            "\u{e9}"
        );
        // UTF-16: "Hi" + NUL.
        assert_eq!(
            Reader::new(&fstr(-3, &[b'H', 0, b'i', 0, 0, 0]))
                .read_fstring()
                .unwrap(),
            "Hi"
        );
        // Missing terminators.
        assert!(Reader::new(&fstr(4, b"None")).read_fstring().is_err());
        assert!(
            Reader::new(&fstr(-2, &[b'H', 0, b'i', 0]))
                .read_fstring()
                .is_err()
        );
        // Lone surrogate.
        assert!(
            Reader::new(&fstr(-2, &[0x00, 0xD8, 0, 0]))
                .read_fstring()
                .is_err()
        );
        // Length larger than data, and pathological lengths.
        assert!(Reader::new(&fstr(100, b"abc\0")).read_fstring().is_err());
        assert!(Reader::new(&fstr(i32::MAX, b"")).read_fstring().is_err());
        assert!(Reader::new(&fstr(i32::MIN, b"")).read_fstring().is_err());
        assert!(Reader::new(&fstr(-i32::MAX, b"")).read_fstring().is_err());
    }

    #[test]
    fn counts_are_capped_by_remaining_bytes() {
        let mut data = 3i32.to_le_bytes().to_vec();
        data.extend_from_slice(&[0u8; 12]);
        let mut r = Reader::new(&data);
        let v = r.read_tarray("u32s", 4, |r| r.read_u32()).unwrap();
        assert_eq!(v.len(), 3);

        let data = i32::MAX.to_le_bytes();
        let mut r = Reader::new(&data);
        assert!(matches!(
            r.read_tarray("huge", 4, |r| r.read_u32()),
            Err(Ue3Error::CountTooLarge { .. })
        ));

        let data = (-1i32).to_le_bytes();
        let mut r = Reader::new(&data);
        assert!(matches!(
            r.read_count("neg", 1),
            Err(Ue3Error::InvalidValue { value: -1, .. })
        ));
    }
}
