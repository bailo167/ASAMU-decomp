//! Minimal Mach-O reader: segments and the symbol table (`nm`'s view).
//!
//! Just enough to check the recorder's symbols and code evidence against the
//! original executable: 64-bit little-endian images (thin, or the x86_64
//! slice of a universal file), `LC_SEGMENT_64` and `LC_SYMTAB`. Hostile-input
//! discipline: every offset and length is bounds-checked; malformed input is
//! an error, never a panic.

use anyhow::{Context, Result, bail};

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const FAT_MAGIC: u32 = 0xCAFE_BABE;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const N_STAB: u8 = 0xE0;
const N_TYPE: u8 = 0x0E;
const N_SECT: u8 = 0x0E;
/// Upper bound on load commands and symbols (the real file has 135,093).
const MAX_SYMBOLS: u32 = 10_000_000;
const MAX_COMMANDS: u32 = 100_000;

/// A segment (`LC_SEGMENT_64`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Segment name (`__TEXT`, `__DATA`, ...).
    pub name: String,
    /// VM address.
    pub vmaddr: u64,
    /// VM size.
    pub vmsize: u64,
    /// File offset (relative to the image start).
    pub fileoff: u64,
    /// Bytes backed by the file.
    pub filesize: u64,
}

/// A symbol defined in a section (`N_SECT`, not a debugging entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// Name as stored (with the Mach-O leading underscore).
    pub name: String,
    /// Address.
    pub value: u64,
    /// Section number (1-based).
    pub sect: u8,
}

/// A parsed image.
#[derive(Clone, Debug)]
pub struct MachO {
    image: Vec<u8>,
    /// Segments in load-command order.
    pub segments: Vec<Segment>,
    /// Defined symbols sorted by address (then name).
    pub symbols: Vec<Symbol>,
}

fn rd<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N]> {
    let end = off.checked_add(N).context("offset overflow")?;
    let s = b.get(off..end).context("truncated Mach-O")?;
    let mut a = [0_u8; N];
    a.copy_from_slice(s);
    Ok(a)
}

fn u32le(b: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(rd::<4>(b, off)?))
}

fn u64le(b: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(rd::<8>(b, off)?))
}

fn u32be(b: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(rd::<4>(b, off)?))
}

fn us(v: u64) -> Result<usize> {
    usize::try_from(v).context("value does not fit in usize")
}

fn cstr(b: &[u8], off: usize) -> Result<String> {
    let s = b.get(off..).context("string offset out of range")?;
    let end = s
        .iter()
        .position(|&c| c == 0)
        .context("unterminated string")?;
    Ok(String::from_utf8_lossy(&s[..end]).into_owned())
}

impl MachO {
    /// Parses `data` (thin 64-bit image or universal file with an x86_64 slice).
    ///
    /// # Errors
    /// Unsupported or malformed input.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let image = if u32be(&data, 0)? == FAT_MAGIC {
            let n = u32be(&data, 4)?;
            let mut slice = None;
            for i in 0..n.min(64) {
                let base = 8 + 20 * us(u64::from(i))?;
                if u32be(&data, base)? == CPU_TYPE_X86_64 {
                    let off = us(u64::from(u32be(&data, base + 8)?))?;
                    let size = us(u64::from(u32be(&data, base + 12)?))?;
                    let end = off.checked_add(size).context("slice overflow")?;
                    slice = Some(data.get(off..end).context("slice out of range")?.to_vec());
                    break;
                }
            }
            slice.context("universal file without an x86_64 slice")?
        } else {
            data
        };
        if u32le(&image, 0)? != MH_MAGIC_64 {
            bail!("not a 64-bit little-endian Mach-O image");
        }
        let ncmds = u32le(&image, 16)?;
        if ncmds > MAX_COMMANDS {
            bail!("implausible load command count {ncmds}");
        }
        let mut segments = Vec::new();
        let mut symtab = None;
        let mut off = 32_usize;
        for _ in 0..ncmds {
            let cmd = u32le(&image, off)?;
            let size = us(u64::from(u32le(&image, off + 4)?))?;
            if size < 8 {
                bail!("load command of size {size}");
            }
            match cmd {
                LC_SEGMENT_64 => {
                    let raw = rd::<16>(&image, off + 8)?;
                    let end = raw.iter().position(|&c| c == 0).unwrap_or(16);
                    segments.push(Segment {
                        name: String::from_utf8_lossy(&raw[..end]).into_owned(),
                        vmaddr: u64le(&image, off + 24)?,
                        vmsize: u64le(&image, off + 32)?,
                        fileoff: u64le(&image, off + 40)?,
                        filesize: u64le(&image, off + 48)?,
                    });
                }
                LC_SYMTAB => {
                    symtab = Some((
                        u32le(&image, off + 8)?,
                        u32le(&image, off + 12)?,
                        u32le(&image, off + 16)?,
                        u32le(&image, off + 20)?,
                    ));
                }
                _ => {}
            }
            off = off.checked_add(size).context("load command overflow")?;
        }
        let (symoff, nsyms, stroff, strsize) = symtab.context("no LC_SYMTAB")?;
        if nsyms > MAX_SYMBOLS {
            bail!("implausible symbol count {nsyms}");
        }
        let strs_start = us(u64::from(stroff))?;
        let strs_end = strs_start
            .checked_add(us(u64::from(strsize))?)
            .context("string table overflow")?;
        let strs = image
            .get(strs_start..strs_end)
            .context("string table out of range")?;
        let mut symbols = Vec::new();
        for i in 0..nsyms {
            let e = us(u64::from(symoff))?
                .checked_add(16 * us(u64::from(i))?)
                .context("symbol offset overflow")?;
            let strx = us(u64::from(u32le(&image, e)?))?;
            let [n_type] = rd::<1>(&image, e + 4)?;
            let [n_sect] = rd::<1>(&image, e + 5)?;
            let value = u64le(&image, e + 8)?;
            if n_type & N_STAB != 0 || n_type & N_TYPE != N_SECT {
                continue;
            }
            symbols.push(Symbol {
                name: cstr(strs, strx)?,
                value,
                sect: n_sect,
            });
        }
        symbols.sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
        Ok(Self {
            image,
            segments,
            symbols,
        })
    }

    /// The defined symbol named `name` (with its leading underscore).
    #[must_use]
    pub fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// Distance from `name` to the next defined symbol at a higher address.
    #[must_use]
    pub fn extent(&self, name: &str) -> Option<u64> {
        let s = self.symbol(name)?;
        self.symbols
            .iter()
            .find(|o| o.value > s.value)
            .map(|o| o.value - s.value)
    }

    /// The segment containing `addr`.
    #[must_use]
    pub fn segment_of(&self, addr: u64) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| addr >= s.vmaddr && addr - s.vmaddr < s.vmsize)
    }

    /// `len` file-backed bytes at VM address `addr`.
    #[must_use]
    pub fn bytes_at(&self, addr: u64, len: u64) -> Option<&[u8]> {
        let seg = self.segment_of(addr)?;
        let rel = addr - seg.vmaddr;
        if rel.checked_add(len)? > seg.filesize {
            return None;
        }
        let start = usize::try_from(seg.fileoff.checked_add(rel)?).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        self.image.get(start..end)
    }

    /// The bytes of function `name`: from its address to the next symbol
    /// (at most 1 MiB).
    #[must_use]
    pub fn function_bytes(&self, name: &str) -> Option<&[u8]> {
        let s = self.symbol(name)?;
        let len = self.extent(name)?.min(1 << 20);
        self.bytes_at(s.value, len)
    }
}

/// Offsets of every occurrence of `needle` in `hay`.
#[must_use]
pub fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    hay.windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Offsets (within `code`, which starts at `base`) of `call rel32`
/// instructions whose target is `target`.
#[must_use]
pub fn calls_to(code: &[u8], base: u64, target: u64) -> Vec<usize> {
    let mut out = Vec::new();
    for i in 0..code.len().saturating_sub(4) {
        if code[i] != 0xE8 {
            continue;
        }
        let Some(rel) = code.get(i + 1..i + 5) else {
            break;
        };
        let rel = i32::from_le_bytes([rel[0], rel[1], rel[2], rel[3]]);
        let next = base.wrapping_add(i as u64 + 5);
        if next.wrapping_add_signed(i64::from(rel)) == target {
            out.push(i);
        }
    }
    out
}

/// Builds a small synthetic Mach-O image (tests): one `__TEXT` segment with
/// `code` at `text_addr`, one `__DATA` segment with `data` at `data_addr`,
/// and the given defined symbols.
#[cfg(test)]
#[must_use]
pub fn synthetic(
    text_addr: u64,
    code: &[u8],
    data_addr: u64,
    data: &[u8],
    symbols: &[(&str, u64, u8)],
) -> Vec<u8> {
    fn p32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn p64(v: &mut Vec<u8>, x: u64) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    let header = 32_u32;
    let seg = 72_u32;
    let symtab = 24_u32;
    let cmds = 2 * seg + symtab;
    let code_off = u64::from(header + cmds);
    let data_off = code_off + code.len() as u64;
    let sym_off = data_off + data.len() as u64;
    let mut strings = vec![0_u8];
    let mut idx = Vec::new();
    for (n, _, _) in symbols {
        idx.push(strings.len() as u32);
        strings.extend_from_slice(n.as_bytes());
        strings.push(0);
    }
    let str_off = sym_off + 16 * symbols.len() as u64;
    let mut v = Vec::new();
    p32(&mut v, MH_MAGIC_64);
    p32(&mut v, CPU_TYPE_X86_64);
    p32(&mut v, 3);
    p32(&mut v, 2);
    p32(&mut v, 3);
    p32(&mut v, cmds);
    p32(&mut v, 0);
    p32(&mut v, 0);
    for (name, addr, off, len) in [
        ("__TEXT", text_addr, code_off, code.len() as u64),
        ("__DATA", data_addr, data_off, data.len() as u64),
    ] {
        p32(&mut v, LC_SEGMENT_64);
        p32(&mut v, seg);
        let mut n = [0_u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        v.extend_from_slice(&n);
        p64(&mut v, addr);
        p64(&mut v, len.max(0x1000));
        p64(&mut v, off);
        p64(&mut v, len);
        p32(&mut v, 7);
        p32(&mut v, 7);
        p32(&mut v, 0);
        p32(&mut v, 0);
    }
    p32(&mut v, LC_SYMTAB);
    p32(&mut v, symtab);
    p32(&mut v, sym_off as u32);
    p32(&mut v, symbols.len() as u32);
    p32(&mut v, str_off as u32);
    p32(&mut v, strings.len() as u32);
    v.extend_from_slice(code);
    v.extend_from_slice(data);
    for ((_, value, sect), strx) in symbols.iter().zip(idx) {
        p32(&mut v, strx);
        v.push(N_SECT | 0x01);
        v.push(*sect);
        v.extend_from_slice(&0_u16.to_le_bytes());
        p64(&mut v, *value);
    }
    v.extend_from_slice(&strings);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        // f: call g (e8 rel32) ; g: ret ; data: 8-byte double, then a u32
        let mut code = vec![0x55, 0xE8, 0, 0, 0, 0, 0xC3];
        // call at offset 1 (base 0x1000) → next = 0x1006, target g = 0x1006: rel 0
        code[2..6].copy_from_slice(&0_i32.to_le_bytes());
        code.extend_from_slice(&[0xC3, 0x90]);
        let mut data = 0.5_f64.to_le_bytes().to_vec();
        data.extend_from_slice(&7_u32.to_le_bytes());
        synthetic(
            0x1000,
            &code,
            0x2000,
            &data,
            &[
                ("_f", 0x1000, 1),
                ("_g", 0x1006, 1),
                ("_D", 0x2000, 2),
                ("_E", 0x2008, 2),
            ],
        )
    }

    #[test]
    fn parses_synthetic_image() {
        let m = MachO::parse(image()).unwrap();
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.symbols.len(), 4);
        assert_eq!(m.symbol("_g").unwrap().value, 0x1006);
        assert_eq!(m.extent("_f"), Some(6));
        assert_eq!(m.extent("_D"), Some(8));
        assert_eq!(m.extent("_E"), None);
        assert_eq!(m.segment_of(0x2004).unwrap().name, "__DATA");
        assert_eq!(m.bytes_at(0x2000, 8).unwrap(), 0.5_f64.to_le_bytes());
        assert!(
            m.bytes_at(0x2008, 8).is_none(),
            "past the file-backed bytes"
        );
        let f = m.function_bytes("_f").unwrap();
        assert_eq!(f.len(), 6);
        assert_eq!(calls_to(f, 0x1000, 0x1006), [1]);
        assert!(calls_to(f, 0x1000, 0x1007).is_empty());
        assert_eq!(find_all(f, &[0xE8, 0]), [1]);
        assert!(find_all(f, &[]).is_empty());
        assert!(find_all(&[1, 2], &[1, 2, 3]).is_empty());
        // Backward calls (negative rel32) and calls ending at the last byte.
        let back = [0x90, 0x90, 0xE8, 0xF9, 0xFF, 0xFF, 0xFF];
        assert_eq!(calls_to(&back, 0x2000, 0x2000), [2]);
        assert!(
            calls_to(&[0xE8, 0, 0, 0], 0, 5).is_empty(),
            "truncated call"
        );
        assert!(calls_to(&[], 0, 0).is_empty());
        // Wrapping arithmetic near the top of the address space.
        assert_eq!(calls_to(&[0xE8, 0, 0, 0, 0], u64::MAX - 2, 2), [0]);
    }

    #[test]
    fn rejects_malformed_input() {
        let good = image();
        for n in [0, 3, 31, 40, 100, good.len() - 1] {
            assert!(
                MachO::parse(good[..n].to_vec()).is_err(),
                "truncated at {n}"
            );
        }
        let mut bad = good.clone();
        bad[0] = 0;
        assert!(MachO::parse(bad).is_err());
        let mut huge = good.clone();
        huge[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(MachO::parse(huge).is_err());
        // Corrupt string index (first symbol's n_strx): out of range.
        let m = MachO::parse(good.clone()).unwrap();
        assert_eq!(m.symbols.len(), 4);
        let symoff = u32::from_le_bytes(good[32 + 144 + 8..32 + 144 + 12].try_into().unwrap());
        let mut bad_str = good.clone();
        let at = symoff as usize;
        bad_str[at..at + 4].copy_from_slice(&0x7FFF_FFFF_u32.to_le_bytes());
        assert!(MachO::parse(bad_str).is_err());
        // Symbol table and string table pointing past the end of the file.
        let mut bad_sym = good.clone();
        bad_sym[32 + 144 + 8..32 + 144 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(MachO::parse(bad_sym).is_err());
        let mut bad_strtab = good.clone();
        bad_strtab[32 + 144 + 20..32 + 144 + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(MachO::parse(bad_strtab).is_err());
        // A load command size of zero, and one running past the end.
        let mut zero = good.clone();
        zero[36..40].copy_from_slice(&0_u32.to_le_bytes());
        assert!(MachO::parse(zero).is_err());
        let mut past = good.clone();
        past[36..40].copy_from_slice(&0x00FF_FFFF_u32.to_le_bytes());
        assert!(MachO::parse(past).is_err());
        let mut fat = Vec::new();
        fat.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        fat.extend_from_slice(&1_u32.to_be_bytes());
        fat.extend_from_slice(&CPU_TYPE_X86_64.to_be_bytes());
        fat.extend_from_slice(&3_u32.to_be_bytes());
        fat.extend_from_slice(&64_u32.to_be_bytes());
        fat.extend_from_slice(&(good.len() as u32).to_be_bytes());
        fat.extend_from_slice(&0_u32.to_be_bytes());
        fat.resize(64, 0);
        fat.extend_from_slice(&good);
        let m = MachO::parse(fat.clone()).unwrap();
        assert_eq!(m.symbols.len(), 4);
        let mut arm = fat;
        arm[8..12].copy_from_slice(&0x0100_000C_u32.to_be_bytes());
        assert!(MachO::parse(arm).is_err());
    }
}
