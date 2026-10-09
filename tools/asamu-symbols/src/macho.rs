//! Mach-O symbol table reader built on the `object` crate.
//!
//! Reads every `nlist_64` entry directly (no `nm` subprocess), so it works on any
//! host OS. Regular entries become [`Symbol`]s; STABS entries are folded into a
//! sanitized debug map ([`crate::provenance`]) that attributes symbols to
//! compilation units. All input is treated as hostile: every lookup is checked,
//! malformed entries are counted instead of aborting, and no serialized length
//! is trusted beyond what `object` has already bounds-checked.

use std::collections::{BTreeMap, HashMap};

use object::macho;
use object::read::macho::{LoadCommandVariant, MachOFile64, Nlist};
use object::{Endianness, FileKind, Object, ObjectSection, SectionKind};
use serde::Serialize;

use crate::provenance::{CompileUnit, sanitize_unit};

/// Errors from reading a binary.
#[derive(Debug)]
pub enum ReadError {
    /// The bytes are not a Mach-O image this tool understands.
    UnsupportedFormat(String),
    /// A fat (universal) binary without an x86_64 slice.
    NoX86_64Slice,
    /// The `object` crate rejected the image.
    Object(object::read::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::UnsupportedFormat(k) => write!(f, "unsupported file format: {k}"),
            ReadError::NoX86_64Slice => write!(f, "universal binary has no x86_64 slice"),
            ReadError::Object(e) => write!(f, "malformed Mach-O: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<object::read::Error> for ReadError {
    fn from(e: object::read::Error) -> Self {
        ReadError::Object(e)
    }
}

/// Symbol kind as requested by the analysis (derived from the section type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SymKind {
    /// Defined in an executable-code section.
    Text,
    /// Defined in an initialised data / constant section.
    Data,
    /// Defined in a zero-fill section (`__bss`, `__common`).
    Bss,
    /// Undefined (imported from a dylib).
    Undefined,
    /// Absolute, indirect, common or in an unknown section.
    Other,
}

impl SymKind {
    /// Lowercase id.
    pub fn id(self) -> &'static str {
        match self {
            SymKind::Text => "text",
            SymKind::Data => "data",
            SymKind::Bss => "bss",
            SymKind::Undefined => "undefined",
            SymKind::Other => "other",
        }
    }
}

/// Linkage scope (`N_EXT` set = global).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// `N_EXT` set (nm prints an uppercase letter).
    Global,
    /// `N_EXT` clear (nm prints a lowercase letter).
    Local,
}

/// A section of the image (index = Mach-O section ordinal − 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionInfo {
    /// Segment name, e.g. `__TEXT`.
    pub segment: String,
    /// Section name, e.g. `__text`.
    pub name: String,
    /// Kind assigned to symbols defined in it.
    pub kind: SymKind,
    /// Virtual address.
    pub addr: u64,
    /// Size in bytes.
    pub size: u64,
}

/// Bytes of one initialised data section, kept for pointer-table decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionBytes {
    /// Virtual address of the first byte.
    pub addr: u64,
    /// Section contents (bounds-checked copy).
    pub bytes: Vec<u8>,
}

/// One regular (non-STABS) symbol table entry.
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Name exactly as stored in the string table (lossy UTF-8).
    pub raw: String,
    /// `n_value` (address for defined symbols, size for common symbols).
    pub value: u64,
    /// Mach-O section ordinal (`n_sect`; 0 = none).
    pub sect: u8,
    /// Kind.
    pub kind: SymKind,
    /// Scope.
    pub scope: Scope,
    /// `N_PEXT` bit.
    pub private_extern: bool,
    /// The letter `nm` would print.
    pub nm_type: char,
    /// Two-level-namespace library ordinal for undefined symbols.
    pub library_ordinal: Option<u8>,
    /// Index into [`Image::units`] from the debug map, if attributed.
    pub unit: Option<u32>,
}

/// Counts gathered while reading.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReadStats {
    /// Total `nlist` entries (`LC_SYMTAB.nsyms`).
    pub nlist_total: u64,
    /// STABS entries (`n_type & N_STAB != 0`).
    pub stab_entries: u64,
    /// STABS entries by type name.
    pub stab_by_type: BTreeMap<String, u64>,
    /// Entries whose name offset was invalid.
    pub invalid_name_offsets: u64,
    /// Entries whose name was not valid UTF-8 (decoded lossily).
    pub non_utf8_names: u64,
    /// Debug-map compilation units.
    pub debug_map_units: u64,
    /// `N_FUN` / `N_STSYM` addresses claimed by more than one unit (first kept).
    pub debug_map_address_conflicts: u64,
    /// `N_GSYM` names claimed by more than one unit (first kept).
    pub debug_map_name_conflicts: u64,
    /// Regular symbols attributed to a unit.
    pub symbols_with_unit: u64,
}

/// Parsed image.
#[derive(Debug, Clone)]
pub struct Image {
    /// File size in bytes.
    pub file_size: u64,
    /// `LC_UUID` as uppercase hex with dashes, if present.
    pub uuid: Option<String>,
    /// Sections (index = ordinal − 1).
    pub sections: Vec<SectionInfo>,
    /// Basenames of dependent dylibs in ordinal order (ordinal = index + 1).
    pub dylibs: Vec<String>,
    /// Regular symbols in symbol-table order.
    pub symbols: Vec<Symbol>,
    /// Sanitized debug-map compilation units.
    pub units: Vec<CompileUnit>,
    /// Contents of initialised non-code sections (`__DATA,__data`,
    /// `__TEXT,__cstring`, ...), used to decode UE3 native tables.
    pub data: Vec<SectionBytes>,
    /// Read statistics.
    pub stats: ReadStats,
}

impl Image {
    /// Read `len` bytes at virtual address `addr` from a retained data section.
    pub fn read_at(&self, addr: u64, len: usize) -> Option<&[u8]> {
        for sec in &self.data {
            let size = u64::try_from(sec.bytes.len()).ok()?;
            let end = sec.addr.checked_add(size)?;
            if addr >= sec.addr && addr < end {
                let start = usize::try_from(addr.checked_sub(sec.addr)?).ok()?;
                let stop = start.checked_add(len)?;
                return sec.bytes.get(start..stop);
            }
        }
        None
    }

    /// Read a little-endian `u64` at `addr`.
    pub fn read_u64(&self, addr: u64) -> Option<u64> {
        let b = self.read_at(addr, 8)?;
        let arr: [u8; 8] = b.try_into().ok()?;
        Some(u64::from_le_bytes(arr))
    }

    /// Read a NUL-terminated printable-ASCII string of at most `max` bytes at `addr`.
    pub fn read_cstr(&self, addr: u64, max: usize) -> Option<String> {
        for sec in &self.data {
            let size = u64::try_from(sec.bytes.len()).ok()?;
            let end = sec.addr.checked_add(size)?;
            if addr >= sec.addr && addr < end {
                let start = usize::try_from(addr.checked_sub(sec.addr)?).ok()?;
                let tail = sec.bytes.get(start..)?;
                let limit = tail.len().min(max);
                let window = tail.get(..limit)?;
                let nul = window.iter().position(|b| *b == 0)?;
                let s = window.get(..nul)?;
                if s.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                    return std::str::from_utf8(s).ok().map(str::to_string);
                }
                return None;
            }
        }
        None
    }

    /// Section info for a symbol, if any.
    pub fn section_of(&self, sym: &Symbol) -> Option<&SectionInfo> {
        let idx = usize::from(sym.sect).checked_sub(1)?;
        self.sections.get(idx)
    }

    /// Dylib basename bound to an undefined symbol (two-level namespace).
    pub fn dylib_of(&self, sym: &Symbol) -> Option<&str> {
        let ord = sym.library_ordinal?;
        match ord {
            0 => Some("<self>"),
            0xfe => Some("<executable>"),
            0xff => Some("<dynamic-lookup>"),
            n => self
                .dylibs
                .get(usize::from(n).checked_sub(1)?)
                .map(String::as_str),
        }
    }

    /// Compile unit of a symbol, if attributed.
    pub fn unit_of(&self, sym: &Symbol) -> Option<&CompileUnit> {
        self.units.get(usize::try_from(sym.unit?).ok()?)
    }
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn stab_name(t: u8) -> String {
    match t {
        0x20 => "GSYM".into(),
        0x22 => "FNAME".into(),
        0x24 => "FUN".into(),
        0x26 => "STSYM".into(),
        0x28 => "LCSYM".into(),
        0x2e => "BNSYM".into(),
        0x3c => "OPT".into(),
        0x40 => "RSYM".into(),
        0x44 => "SLINE".into(),
        0x4e => "ENSYM".into(),
        0x60 => "SSYM".into(),
        0x64 => "SO".into(),
        0x66 => "OSO".into(),
        0x80 => "LSYM".into(),
        0x82 => "BINCL".into(),
        0x84 => "SOL".into(),
        0x86 => "PARAMS".into(),
        0x88 => "VERSION".into(),
        0x8a => "OLEVEL".into(),
        0xa0 => "PSYM".into(),
        0xa2 => "EINCL".into(),
        0xa4 => "ENTRY".into(),
        0xc0 => "LBRAC".into(),
        0xc2 => "EXCL".into(),
        0xe0 => "RBRAC".into(),
        0xe2 => "BCOMM".into(),
        0xe4 => "ECOMM".into(),
        0xe8 => "ECOML".into(),
        0xfe => "LENG".into(),
        other => format!("0x{other:02x}"),
    }
}

fn section_kind(kind: SectionKind) -> SymKind {
    match kind {
        SectionKind::Text => SymKind::Text,
        SectionKind::UninitializedData | SectionKind::UninitializedTls => SymKind::Bss,
        SectionKind::Data
        | SectionKind::ReadOnlyData
        | SectionKind::ReadOnlyDataWithRel
        | SectionKind::ReadOnlyString
        | SectionKind::Tls
        | SectionKind::TlsVariables
        | SectionKind::OtherString
        | SectionKind::Other
        | SectionKind::Debug
        | SectionKind::DebugString
        | SectionKind::Note
        | SectionKind::Linker
        | SectionKind::Metadata => SymKind::Data,
        _ => SymKind::Other,
    }
}

/// Parse a Mach-O image (thin x86_64/arm64 64-bit, or the x86_64 slice of a fat file).
pub fn parse(data: &[u8]) -> Result<Image, ReadError> {
    let kind = FileKind::parse(data)?;
    match kind {
        FileKind::MachO64 => parse_thin(data, data.len()),
        FileKind::MachOFat32 => {
            let fat = object::read::macho::MachOFatFile32::parse(data)?;
            for arch in fat.arches() {
                use object::read::macho::FatArch;
                if arch.cputype() == macho::CPU_TYPE_X86_64 {
                    let slice = arch.data(data)?;
                    return parse_thin(slice, data.len());
                }
            }
            Err(ReadError::NoX86_64Slice)
        }
        other => Err(ReadError::UnsupportedFormat(format!("{other:?}"))),
    }
}

/// Mutable state of the STABS debug-map walk.
#[derive(Default)]
struct DebugMapState {
    dir: String,
    file: String,
    oso: String,
    current: Option<u32>,
}

fn parse_thin(data: &[u8], file_size: usize) -> Result<Image, ReadError> {
    let file = MachOFile64::<Endianness, &[u8]>::parse(data)?;
    let endian = file.endian();

    // Sections, indexed by ordinal - 1.
    let mut sections: Vec<SectionInfo> = Vec::new();
    let mut data_sections: Vec<SectionBytes> = Vec::new();
    for section in file.sections() {
        let ordinal = section.index().0;
        let slot = ordinal.checked_sub(1).unwrap_or(usize::MAX);
        if slot > 255 {
            continue;
        }
        while sections.len() <= slot {
            sections.push(SectionInfo {
                segment: String::new(),
                name: String::new(),
                kind: SymKind::Other,
                addr: 0,
                size: 0,
            });
        }
        let info = SectionInfo {
            segment: section
                .segment_name()
                .ok()
                .flatten()
                .unwrap_or("")
                .to_string(),
            name: section.name().unwrap_or("").to_string(),
            kind: {
                // ld64 marks `__TEXT,__const_coal` with instruction attributes
                // although it only holds coalesced constants; treat it as data.
                let k = section_kind(section.kind());
                let name = section.name().unwrap_or("");
                if k == SymKind::Text && name.contains("const") {
                    SymKind::Data
                } else {
                    k
                }
            },
            addr: section.address(),
            size: section.size(),
        };
        if info.kind == SymKind::Data
            && let Ok(bytes) = section.data()
        {
            data_sections.push(SectionBytes {
                addr: info.addr,
                bytes: bytes.to_vec(),
            });
        }
        if let Some(entry) = sections.get_mut(slot) {
            *entry = info;
        }
    }

    // Dependent dylibs in ordinal order.
    let mut dylibs = Vec::new();
    let mut commands = file.macho_load_commands()?;
    while let Some(cmd) = commands.next()? {
        if let Ok(LoadCommandVariant::Dylib(d)) = cmd.variant() {
            let name = cmd
                .string(endian, d.dylib.name)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            dylibs.push(basename(&name));
        }
    }

    let uuid = file.mach_uuid().ok().flatten().map(|u| {
        let hex: Vec<String> = u.iter().map(|b| format!("{b:02X}")).collect();
        let h = hex.concat();
        format!(
            "{}-{}-{}-{}-{}",
            h.get(0..8).unwrap_or(""),
            h.get(8..12).unwrap_or(""),
            h.get(12..16).unwrap_or(""),
            h.get(16..20).unwrap_or(""),
            h.get(20..32).unwrap_or("")
        )
    });

    let table = file.macho_symbol_table();
    let strings = table.strings();
    let mut stats = ReadStats {
        nlist_total: u64::try_from(table.len()).unwrap_or(u64::MAX),
        ..ReadStats::default()
    };

    let mut symbols = Vec::new();
    let mut units: Vec<CompileUnit> = Vec::new();
    let mut by_addr: HashMap<u64, u32> = HashMap::new();
    let mut by_name: HashMap<String, u32> = HashMap::new();
    let mut dm = DebugMapState::default();

    for nlist in table.iter() {
        let name_bytes = match nlist.name(endian, strings) {
            Ok(b) => Some(b),
            Err(_) => {
                stats.invalid_name_offsets = stats.invalid_name_offsets.saturating_add(1);
                None
            }
        };
        let name: String = match name_bytes {
            Some(b) => match std::str::from_utf8(b) {
                Ok(s) => s.to_string(),
                Err(_) => {
                    stats.non_utf8_names = stats.non_utf8_names.saturating_add(1);
                    String::from_utf8_lossy(b).into_owned()
                }
            },
            None => String::new(),
        };
        let n_type = nlist.n_type();
        let value: u64 = nlist.n_value(endian);

        if let Some(stab) = n_type.stab() {
            stats.stab_entries = stats.stab_entries.saturating_add(1);
            let entry = stats.stab_by_type.entry(stab_name(stab.0)).or_insert(0);
            *entry = entry.saturating_add(1);
            handle_stab(
                stab.0,
                &name,
                value,
                &mut dm,
                &mut units,
                &mut by_addr,
                &mut by_name,
                &mut stats,
            );
            continue;
        }

        let typ = n_type.typ();
        let ext = n_type.is_ext();
        let pext = n_type.contains(macho::N_PEXT);
        let sect = nlist.n_sect();
        let scope = if ext { Scope::Global } else { Scope::Local };
        let (kind, letter, ordinal) = if typ == macho::N_UNDF {
            if value == 0 {
                let ord = nlist.n_desc(endian).library().0;
                (SymKind::Undefined, 'U', Some(ord))
            } else {
                (SymKind::Other, 'C', None)
            }
        } else if typ == macho::N_PBUD {
            let ord = nlist.n_desc(endian).library().0;
            (SymKind::Undefined, 'U', Some(ord))
        } else if typ == macho::N_ABS {
            (SymKind::Other, 'A', None)
        } else if typ == macho::N_INDR {
            (SymKind::Other, 'I', None)
        } else if typ == macho::N_SECT {
            let info = usize::from(sect)
                .checked_sub(1)
                .and_then(|i| sections.get(i));
            match info {
                Some(s) => {
                    let letter = match (s.segment.as_str(), s.name.as_str()) {
                        ("__TEXT", "__text") => 'T',
                        ("__DATA", "__data") => 'D',
                        ("__DATA", "__bss") => 'B',
                        _ => 'S',
                    };
                    (s.kind, letter, None)
                }
                None => (SymKind::Other, '?', None),
            }
        } else {
            (SymKind::Other, '?', None)
        };
        let nm_type = if ext || letter == 'U' || letter == '?' {
            letter
        } else {
            letter.to_ascii_lowercase()
        };
        symbols.push(Symbol {
            raw: name,
            value,
            sect,
            kind,
            scope,
            private_extern: pext,
            nm_type,
            library_ordinal: ordinal,
            unit: None,
        });
    }

    // Attribute regular symbols to debug-map units: functions and statics by
    // address, globals by name.
    for sym in &mut symbols {
        if sym.kind == SymKind::Undefined {
            continue;
        }
        let by_a = if sym.sect != 0 {
            by_addr.get(&sym.value).copied()
        } else {
            None
        };
        sym.unit = by_a.or_else(|| by_name.get(&sym.raw).copied());
        if sym.unit.is_some() {
            stats.symbols_with_unit = stats.symbols_with_unit.saturating_add(1);
        }
    }
    stats.debug_map_units = u64::try_from(units.len()).unwrap_or(u64::MAX);

    Ok(Image {
        file_size: u64::try_from(file_size).unwrap_or(u64::MAX),
        uuid,
        sections,
        dylibs,
        symbols,
        units,
        data: data_sections,
        stats,
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_stab(
    t: u8,
    name: &str,
    value: u64,
    dm: &mut DebugMapState,
    units: &mut Vec<CompileUnit>,
    by_addr: &mut HashMap<u64, u32>,
    by_name: &mut HashMap<String, u32>,
    stats: &mut ReadStats,
) {
    const N_GSYM: u8 = 0x20;
    const N_FUN: u8 = 0x24;
    const N_STSYM: u8 = 0x26;
    const N_SO: u8 = 0x64;
    const N_OSO: u8 = 0x66;
    match t {
        N_SO => {
            if name.is_empty() {
                // End of a module.
                *dm = DebugMapState::default();
            } else if name.ends_with('/') {
                dm.dir = name.to_string();
                dm.file.clear();
                dm.oso.clear();
                dm.current = None;
            } else {
                dm.file = name.to_string();
                dm.oso.clear();
                dm.current = None;
            }
        }
        N_OSO => {
            dm.oso = name.to_string();
            dm.current = new_unit(dm, units);
        }
        N_FUN | N_STSYM | N_GSYM => {
            if name.is_empty() {
                return; // N_FUN end-of-function marker (value = size).
            }
            if dm.current.is_none() && !(dm.file.is_empty() && dm.dir.is_empty()) {
                dm.current = new_unit(dm, units);
            }
            let Some(unit) = dm.current else {
                return;
            };
            if t == N_GSYM {
                if by_name.contains_key(name) {
                    stats.debug_map_name_conflicts =
                        stats.debug_map_name_conflicts.saturating_add(1);
                } else {
                    by_name.insert(name.to_string(), unit);
                }
            } else {
                match by_addr.entry(value) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        stats.debug_map_address_conflicts =
                            stats.debug_map_address_conflicts.saturating_add(1);
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(unit);
                    }
                }
            }
        }
        _ => {}
    }
}

fn new_unit(dm: &DebugMapState, units: &mut Vec<CompileUnit>) -> Option<u32> {
    let idx = u32::try_from(units.len()).ok()?;
    units.push(sanitize_unit(&dm.dir, &dm.file, &dm.oso));
    Some(idx)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn rejects_non_macho() {
        assert!(parse(b"").is_err());
        assert!(parse(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0").is_err());
        assert!(parse(&[0u8; 64]).is_err());
    }

    #[test]
    fn rejects_truncated_macho_headers() {
        // MH_MAGIC_64 followed by too few bytes.
        let mut data = vec![0xcf, 0xfa, 0xed, 0xfe];
        assert!(parse(&data).is_err());
        data.extend_from_slice(&[0x07, 0x00, 0x00, 0x01]);
        assert!(parse(&data).is_err());
    }

    #[test]
    fn synthetic_minimal_macho_with_symbols() {
        let bytes = build_test_macho();
        let image = parse(&bytes).expect("synthetic image parses");
        assert_eq!(image.stats.nlist_total, 7);
        assert_eq!(image.stats.stab_entries, 4);
        assert_eq!(image.stats.debug_map_units, 1);
        assert_eq!(image.symbols.len(), 3);
        let names: Vec<&str> = image.symbols.iter().map(|s| s.raw.as_str()).collect();
        assert_eq!(names, ["_local_fn", "_GlobalFn", "_imported"]);
        assert_eq!(image.symbols[0].nm_type, 't');
        assert_eq!(image.symbols[1].nm_type, 'T');
        assert_eq!(image.symbols[1].scope, Scope::Global);
        assert_eq!(image.symbols[2].kind, SymKind::Undefined);
        assert_eq!(image.dylib_of(&image.symbols[2]), Some("libSystem.B.dylib"));
        // Debug map attributes the global function to the UE3 Engine module.
        let unit = image.unit_of(&image.symbols[1]).expect("unit");
        assert_eq!(unit.rel_path, "Engine/Src/Test.cpp");
        // Truncating the image anywhere must never panic.
        for cut in (0..bytes.len()).step_by(7) {
            let _ = parse(&bytes[..cut]);
        }
    }

    #[test]
    fn corrupted_images_never_panic() {
        let bytes = build_test_macho();
        // Deterministic xorshift so failures are reproducible.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let mut copy = bytes.clone();
            for _ in 0..4 {
                let pos =
                    usize::try_from(next() % u64::try_from(copy.len()).expect("len")).expect("pos");
                copy[pos] = u8::try_from(next() & 0xff).expect("byte");
            }
            if let Ok(image) = parse(&copy) {
                // Analysis and summary must cope with whatever parsed.
                let analysis = crate::analysis::Analysis::new(image);
                let _ = crate::summary::build(&analysis);
            }
        }
    }

    /// Build a tiny x86_64 Mach-O by hand: header, LC_SEGMENT_64 with one
    /// `__TEXT,__text` section, LC_SYMTAB, LC_LOAD_DYLIB, symbols and strings.
    pub(crate) fn build_test_macho() -> Vec<u8> {
        fn p32(v: &mut Vec<u8>, x: u32) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn p64(v: &mut Vec<u8>, x: u64) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn name16(v: &mut Vec<u8>, s: &str) {
            let mut b = [0u8; 16];
            b[..s.len()].copy_from_slice(s.as_bytes());
            v.extend_from_slice(&b);
        }
        let strtab: Vec<u8> = {
            let mut s = vec![0u8]; // index 0 = ""
            for n in [
                "/b/UnrealEngine3/Development/Src/Engine/Src/",
                "Test.cpp",
                "/b/Test.o",
                "_local_fn",
                "_GlobalFn",
                "_imported",
            ] {
                s.extend_from_slice(n.as_bytes());
                s.push(0);
            }
            s
        };
        let off = |needle: &str| -> u32 {
            let pos = strtab
                .windows(needle.len() + 1)
                .position(|w| &w[..needle.len()] == needle.as_bytes() && w[needle.len()] == 0)
                .expect("string present");
            u32::try_from(pos).expect("fits")
        };
        let dylib_name = b"/usr/lib/libSystem.B.dylib\0";
        let dylib_cmdsize = (24 + dylib_name.len()).div_ceil(8) * 8;
        let seg_cmdsize = 72 + 80;
        let symtab_cmdsize = 24;
        let sizeofcmds = seg_cmdsize + symtab_cmdsize + dylib_cmdsize;
        let header_size = 32;
        let text_off = header_size + sizeofcmds;
        let text_size = 16usize;
        let sym_off = text_off + text_size;
        let nsyms = 7usize;
        let str_off = sym_off + nsyms * 16;

        let mut v = Vec::new();
        // mach_header_64
        p32(&mut v, 0xfeed_facf);
        p32(&mut v, 0x0100_0007); // CPU_TYPE_X86_64
        p32(&mut v, 3);
        p32(&mut v, 2); // MH_EXECUTE
        p32(&mut v, 3);
        p32(&mut v, u32::try_from(sizeofcmds).expect("fits"));
        p32(&mut v, 0);
        p32(&mut v, 0);
        // LC_SEGMENT_64
        p32(&mut v, 0x19);
        p32(&mut v, u32::try_from(seg_cmdsize).expect("fits"));
        name16(&mut v, "__TEXT");
        p64(&mut v, 0x1000);
        p64(&mut v, 0x1000);
        p64(&mut v, 0);
        p64(&mut v, u64::try_from(sym_off).expect("fits"));
        p32(&mut v, 5);
        p32(&mut v, 5);
        p32(&mut v, 1);
        p32(&mut v, 0);
        // section_64
        name16(&mut v, "__text");
        name16(&mut v, "__TEXT");
        p64(&mut v, 0x1000 + u64::try_from(text_off).expect("fits"));
        p64(&mut v, u64::try_from(text_size).expect("fits"));
        p32(&mut v, u32::try_from(text_off).expect("fits"));
        p32(&mut v, 4);
        p32(&mut v, 0);
        p32(&mut v, 0);
        p32(&mut v, 0x8000_0400); // PURE_INSTRUCTIONS | SOME_INSTRUCTIONS
        p32(&mut v, 0);
        p32(&mut v, 0);
        p32(&mut v, 0);
        // LC_SYMTAB
        p32(&mut v, 0x2);
        p32(&mut v, 24);
        p32(&mut v, u32::try_from(sym_off).expect("fits"));
        p32(&mut v, u32::try_from(nsyms).expect("fits"));
        p32(&mut v, u32::try_from(str_off).expect("fits"));
        p32(&mut v, u32::try_from(strtab.len()).expect("fits"));
        // LC_LOAD_DYLIB
        p32(&mut v, 0xc);
        p32(&mut v, u32::try_from(dylib_cmdsize).expect("fits"));
        p32(&mut v, 24);
        p32(&mut v, 0);
        p32(&mut v, 0x0001_0000);
        p32(&mut v, 0x0001_0000);
        v.extend_from_slice(dylib_name);
        while v.len() < text_off {
            v.push(0);
        }
        v.extend_from_slice(&[0xc3; 16]); // text
        let text_addr = 0x1000 + u64::try_from(text_off).expect("fits");
        let mut nlist = |strx: u32, ty: u8, sect: u8, desc: u16, value: u64| {
            p32(&mut v, strx);
            v.push(ty);
            v.push(sect);
            v.extend_from_slice(&desc.to_le_bytes());
            p64(&mut v, value);
        };
        nlist(
            off("/b/UnrealEngine3/Development/Src/Engine/Src/"),
            0x64,
            0,
            0,
            0,
        );
        nlist(off("Test.cpp"), 0x64, 0, 0, 0);
        nlist(off("/b/Test.o"), 0x66, 3, 1, 0);
        nlist(off("_GlobalFn"), 0x24, 1, 0, text_addr + 4);
        nlist(off("_local_fn"), 0x0e, 1, 0, text_addr);
        nlist(off("_GlobalFn"), 0x0f, 1, 0, text_addr + 4);
        nlist(off("_imported"), 0x01, 0, 0x0100, 0);
        v.extend_from_slice(&strtab);
        v
    }
}
