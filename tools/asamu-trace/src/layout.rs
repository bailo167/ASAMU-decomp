//! Static self-checks of the trace recorder's layout files.
//!
//! `tools/trace-recorder/layout_mac_x86_64.json` lists every symbol, field
//! offset and native structure member the recorder reads from the running
//! game. These checks make sure the recorder cannot silently read the wrong
//! bytes:
//!
//! 1. **schema**: well-formed entries (hex offsets, known kinds, bool bits);
//! 2. **native_layout**: every field marked `native_layout` equals its row in
//!    `docs/reverse-engineering/data/defaults/native_layout.json` (offset,
//!    kind, size, bit); a dotted field lies inside its parent field;
//! 3. **defaults**: every script-class field exists with a matching type in
//!    the class's defaults data file, and every sentinel's expected value is
//!    the class default recorded there;
//! 4. **native_code**: every field or structure justified by native code has
//!    an evidence entry whose instruction bytes contain the offset;
//! 5. **python**: every literal `off`/`bit`/`st`/`st_size`/`st_attr`/`sym`
//!    lookup in the recorder's Python sources names an entry of the layout;
//! 6. **binary** (when the executable is present): every symbol is defined
//!    in the right segment with enough room for the read, data initial values
//!    match, every evidence byte pattern occurs in its function (and before
//!    the named call when required), and call counts match.
//!
//! `tools/trace-recorder/layout_win_x86.json` is the same table for the
//! 32-bit Windows build, whose executable has no symbols. Its checks use
//! groups named `win32:*`:
//!
//! 1. **win32:schema**: as above, with 4-byte pointers, an `image` block and
//!    an RVA for every symbol;
//! 2. **win32:layout**: every field (script classes included) equals its row
//!    in `docs/reverse-engineering/data/win32/native_layout_win32.json`, the
//!    table computed by the Win32 layout rules; a dotted field equals the sum
//!    of its member path; the native structures agree with those rows;
//! 3. **win32:mac**: the Windows layout reads exactly what the Mac layout
//!    reads (fields, kinds, bits, structures, symbols, sentinels);
//! 4. **win32:defaults** and **win32:python**: as for the Mac layout;
//! 5. **win32:native_code**: every evidence entry is well formed, its offset
//!    is the rule-derived offset of its field and appears in its instruction
//!    bytes, and an entry for a symbol holds that symbol's address;
//! 6. **win32:data**: symbols, image values and evidence anchors agree with
//!    the Win32 binary-analysis data (`globals.json`, `functions.json`,
//!    `image.json`, `class_sizes.json`);
//! 7. **win32:binary** (when a local copy of the Windows executable is
//!    present): the PE header is this build's, every symbol lies in its
//!    section, every evidence instruction is at its RVA inside the function
//!    its anchor identifies (native function name table, vtable slot, tail
//!    jump, a UTF-16 literal only that function references, or a direct call
//!    from a function identified one of those ways) with no function
//!    boundary (two-byte `int3` padding) between that function's first byte
//!    and the instruction, the member and class name literals an entry
//!    lists are used next to its instruction, ordering and call counts match.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::macho::{MachO, calls_to, find_all};

/// Expected value of `schema`.
pub const LAYOUT_SCHEMA: &str = "asamu-decomp/recorder-layout/v1";
/// Layout file, relative to the repository root.
pub const LAYOUT_PATH: &str = "tools/trace-recorder/layout_mac_x86_64.json";
/// Python sources scanned by the `python` check, relative to the root.
pub const PYTHON_SOURCES: &[&str] = &[
    "tools/trace-recorder/asamu_recorder_core.py",
    "tools/trace-recorder/asamu_lldb.py",
];
/// Native layout data, relative to the root.
pub const NATIVE_LAYOUT_PATH: &str = "docs/reverse-engineering/data/defaults/native_layout.json";
/// Defaults data directory, relative to the root.
pub const DEFAULTS_DIR: &str = "docs/reverse-engineering/data/defaults";

/// A symbol the recorder resolves.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutSymbol {
    /// Key the recorder core hands to the front end's symbol resolver: the
    /// Mach-O name (leading underscore included) on the Mac build, the
    /// symbol's own name on the Windows build.
    pub mangled: String,
    /// `data` or `function`.
    pub kind: String,
    /// Bytes read at the symbol (data).
    #[serde(default)]
    pub read: Option<u64>,
    /// Value type (data).
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    /// What the recorder uses it for.
    #[serde(default)]
    pub role: Option<String>,
    /// Initial `f64` value stored in the file (checked when present).
    #[serde(default)]
    pub file_value_f64: Option<f64>,
    /// Windows: hex RVA (runtime address = module base + RVA).
    #[serde(default)]
    pub rva: Option<String>,
    /// Windows, functions: hex RVA of the function's last instruction.
    #[serde(default)]
    pub end_rva: Option<String>,
    /// Windows: PE section that holds the symbol.
    #[serde(default)]
    pub section: Option<String>,
    /// Windows, functions: calling convention, in words.
    #[serde(default)]
    pub abi: Option<String>,
}

/// A field the recorder reads.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutField {
    /// Script class (`Package.Class`).
    pub class: String,
    /// Property name (dotted for struct members).
    pub name: String,
    /// Hex offset.
    pub offset: String,
    /// Kind (as in native_layout.json).
    pub kind: String,
    /// Size in bytes.
    pub size: u64,
    /// Bit within the 32-bit word (bools).
    #[serde(default)]
    pub bit: Option<u32>,
    /// `native_layout`, `layout_rule` or `native_code`.
    pub evidence: String,
    /// Free-form note.
    #[serde(default)]
    pub note: Option<String>,
}

/// A field whose live value must equal the class default.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sentinel {
    /// `pawn`, `gun` or `input`.
    pub object: String,
    /// Layout class of the field.
    pub class: String,
    /// Field name.
    pub name: String,
    /// Expected value.
    pub expected: f64,
    /// Defaults data file that records the value.
    pub data_file: String,
}

/// How a function of the symbol-less Windows executable is identified.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAnchor {
    /// A function of `functions.json` (found by the binary scan).
    #[serde(default)]
    pub function: Option<String>,
    /// Entry of the native function name table (`<Class>exec<Name>`).
    #[serde(default)]
    pub exec: Option<String>,
    /// Native class (C++ name) whose vtable holds the function at `slot`.
    #[serde(default)]
    pub vtable: Option<String>,
    /// Native class whose vtable slot `slot` tail-jumps to the function.
    #[serde(default)]
    pub tail_of_vtable: Option<String>,
    /// Vtable slot number.
    #[serde(default)]
    pub slot: Option<u64>,
    /// A UTF-16 literal of the executable that only this function
    /// references: every use of the literal's address in `.text` lies in it.
    #[serde(default)]
    pub literal: Option<String>,
    /// The function is the target of a direct call in the function this
    /// nested anchor identifies (the nested anchor carries `rva`).
    #[serde(default)]
    pub called_by: Option<Box<EvidenceAnchor>>,
    /// Nested anchors: hex RVA of the function the anchor identifies.
    #[serde(default)]
    pub rva: Option<String>,
}

/// Instruction bytes that show an offset in native code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEvidence {
    /// What the bytes show.
    pub what: String,
    /// Function containing them (Mach-O name; a plain name on Windows).
    pub function: String,
    /// Hex bytes.
    pub bytes: String,
    /// The bytes must come before the first call to this function.
    #[serde(default)]
    pub before_call_to: Option<String>,
    /// Field this supports (`Package.Class.Field`).
    #[serde(default)]
    pub field: Option<String>,
    /// Native structure this supports.
    #[serde(default, rename = "struct")]
    pub structure: Option<String>,
    /// Windows: hex offset of `field` that the instruction shows.
    #[serde(default)]
    pub offset: Option<String>,
    /// Windows: bit of the 32-bit word at `offset` (bool fields).
    #[serde(default)]
    pub bit: Option<u32>,
    /// Windows: symbol whose address the instruction holds.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Windows: how the function is identified.
    #[serde(default)]
    pub anchor: Option<EvidenceAnchor>,
    /// Windows: hex RVA of the function's first instruction.
    #[serde(default)]
    pub function_rva: Option<String>,
    /// Windows: hex RVA of the instruction.
    #[serde(default)]
    pub rva: Option<String>,
    /// Windows: UTF-16 literals (the member's own name, its class) whose
    /// addresses the function uses right next to the instruction: the
    /// executable itself names the field the offset belongs to.
    #[serde(default)]
    pub near_literals: Vec<String>,
}

/// Header values that identify the Windows executable (`image`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutImage {
    /// Module (file) name of the executable.
    pub module: String,
    /// Preferred image base (hex).
    pub image_base: String,
    /// PE time stamp.
    pub time_date_stamp: u64,
    /// PE size of image.
    pub size_of_image: u64,
    /// How a runtime address is formed.
    pub address_rule: String,
}

/// A call-count check.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEvidence {
    /// What it shows.
    pub what: String,
    /// Caller (Mach-O name).
    pub caller: String,
    /// Callee (Mach-O name).
    pub callee: String,
    /// Number of direct calls.
    pub count: usize,
}

/// The layout file.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecorderLayout {
    /// [`LAYOUT_SCHEMA`].
    pub schema: String,
    /// Layout id.
    pub id: String,
    /// Build description.
    pub build: String,
    /// Game build id.
    pub game_build: String,
    /// Executable path inside the install.
    pub executable: String,
    /// Pointer size.
    pub pointer_size: u64,
    /// Windows: header values of the executable.
    #[serde(default)]
    pub image: Option<LayoutImage>,
    /// Notes.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Symbols by recorder name.
    pub symbols: BTreeMap<String, LayoutSymbol>,
    /// Native structures (members and attributes).
    pub structs: BTreeMap<String, serde_json::Value>,
    /// Fields.
    pub fields: Vec<LayoutField>,
    /// Sentinels.
    #[serde(default)]
    pub sentinels: Vec<Sentinel>,
    /// Native code evidence.
    #[serde(default)]
    pub native_evidence: Vec<NativeEvidence>,
    /// Call-count evidence.
    #[serde(default)]
    pub calls: Vec<CallEvidence>,
}

impl RecorderLayout {
    /// Parses the layout JSON.
    ///
    /// # Errors
    /// JSON errors.
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// The field `class.name`.
    #[must_use]
    pub fn field(&self, class: &str, name: &str) -> Option<&LayoutField> {
        self.fields
            .iter()
            .find(|f| f.class == class && f.name == name)
    }
}

/// One check result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Check group (`schema`, `native_layout`, ...).
    pub group: &'static str,
    /// What was checked and the outcome.
    pub what: String,
    /// Passed.
    pub ok: bool,
}

fn check(out: &mut Vec<Check>, group: &'static str, ok: bool, what: String) {
    out.push(Check { group, what, ok });
}

fn hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

const KINDS: &[&str] = &[
    "Object",
    "Class",
    "Name",
    "Struct",
    "Byte",
    "Float",
    "Int",
    "Bool",
    "Array",
    "Component",
    "Str",
    "Interface",
    "Delegate",
    "Map",
];
const EVIDENCE: &[&str] = &["native_layout", "layout_rule", "native_code"];

/// Well-formed fields and structures (both builds).
fn schema_fields(l: &RecorderLayout, group: &'static str, out: &mut Vec<Check>) {
    let mut seen = BTreeSet::new();
    for f in &l.fields {
        let id = format!("{}.{}", f.class, f.name);
        let mut problems = Vec::new();
        if !seen.insert(id.clone()) {
            problems.push("duplicate");
        }
        if hex(&f.offset).is_none() {
            problems.push("offset is not hex");
        }
        if !KINDS.contains(&f.kind.as_str()) {
            problems.push("unknown kind");
        }
        if f.size == 0 {
            problems.push("zero size");
        }
        if f.kind == "Bool" && (f.size != 4 || f.bit.is_none_or(|b| b >= 32)) {
            problems.push("bool needs size 4 and a bit < 32");
        }
        if f.kind != "Bool" && f.bit.is_some() {
            problems.push("bit on a non-bool");
        }
        if !EVIDENCE.contains(&f.evidence.as_str()) {
            problems.push("unknown evidence");
        }
        check(
            out,
            group,
            problems.is_empty(),
            format!(
                "field {id}: {}",
                if problems.is_empty() {
                    "ok".to_owned()
                } else {
                    problems.join(", ")
                }
            ),
        );
    }
    for (name, s) in &l.structs {
        let ok = s.get("size").and_then(serde_json::Value::as_u64).is_some()
            && s.get("members")
                .and_then(serde_json::Value::as_object)
                .is_some();
        check(out, group, ok, format!("struct {name}"));
    }
}

/// Check 1: well-formed entries.
#[must_use]
pub fn check_schema(l: &RecorderLayout) -> Vec<Check> {
    let mut out = Vec::new();
    check(
        &mut out,
        "schema",
        l.schema == LAYOUT_SCHEMA,
        format!("schema {:?}", l.schema),
    );
    check(
        &mut out,
        "schema",
        l.pointer_size == 8,
        format!("pointer size {}", l.pointer_size),
    );
    for (name, s) in &l.symbols {
        let ok = s.mangled.starts_with('_')
            && (s.kind == "function" || (s.kind == "data" && s.read.is_some_and(|r| r > 0)));
        check(
            &mut out,
            "schema",
            ok,
            format!("symbol {name} ({})", s.mangled),
        );
    }
    schema_fields(l, "schema", &mut out);
    out
}

/// Check 2: against `native_layout.json`.
#[must_use]
pub fn check_native_layout(l: &RecorderLayout, native: &serde_json::Value) -> Vec<Check> {
    let mut out = Vec::new();
    let classes = native.get("classes").and_then(serde_json::Value::as_object);
    let row = |class: &str, name: &str| -> Option<(u64, String, u64, Option<u64>)> {
        let fields = classes?.get(class)?.get("fields")?.as_array()?;
        fields.iter().find_map(|r| {
            let r = r.as_array()?;
            (r.get(1)?.as_str()? == name).then(|| {
                Some((
                    hex(r.first()?.as_str()?)?,
                    r.get(2)?.as_str()?.to_owned(),
                    r.get(3)?.as_u64()?,
                    r.get(4)?.as_u64(),
                ))
            })?
        })
    };
    for f in &l.fields {
        let id = format!("{}.{}", f.class, f.name);
        let listed = classes.is_some_and(|c| c.contains_key(&f.class));
        let off = hex(&f.offset).unwrap_or(u64::MAX);
        match (f.evidence.as_str(), f.name.split_once('.')) {
            ("native_layout", None) => match row(&f.class, &f.name) {
                Some((o, kind, size, bit)) => {
                    let ok =
                        o == off && kind == f.kind && size == f.size && bit == f.bit.map(u64::from);
                    check(
                        &mut out,
                        "native_layout",
                        ok,
                        format!(
                            "{id}: layout {:#x} {} {} {:?} vs native_layout {o:#x} {kind} {size} {bit:?}",
                            off, f.kind, f.size, f.bit
                        ),
                    );
                }
                None => check(
                    &mut out,
                    "native_layout",
                    false,
                    format!("{id}: not in native_layout.json"),
                ),
            },
            (_, Some((parent, _))) if listed => match row(&f.class, parent) {
                Some((o, _, size, _)) => {
                    let ok = off >= o && off.saturating_add(f.size) <= o.saturating_add(size);
                    check(
                        &mut out,
                        "native_layout",
                        ok,
                        format!("{id}: {off:#x}+{} inside {parent} {o:#x}+{size}", f.size),
                    );
                }
                None => check(
                    &mut out,
                    "native_layout",
                    false,
                    format!("{id}: parent {parent} not in native_layout.json"),
                ),
            },
            (_, Some(_)) => {
                check(
                    &mut out,
                    "native_layout",
                    false,
                    format!("{id}: dotted field of an unlisted class"),
                );
            }
            (_, None) => {
                // layout_rule / native_code fields must not shadow a listed class.
                check(
                    &mut out,
                    "native_layout",
                    !listed,
                    format!(
                        "{id}: {} evidence {}",
                        f.evidence,
                        if listed {
                            "for a class native_layout.json lists (use native_layout)"
                        } else {
                            "for a class native_layout.json does not list"
                        }
                    ),
                );
            }
        }
    }
    out
}

fn type_matches(kind: &str, size: u64, ty: &str) -> bool {
    match kind {
        "Bool" => ty == "bool",
        "Int" => ty == "int",
        "Float" => ty == "float",
        "Name" => ty == "name",
        "Byte" => ty == "byte" || ty.chars().next().is_some_and(char::is_uppercase),
        "Struct" => size == 12 && (ty == "Vector" || ty == "Rotator"),
        "Array" => ty.starts_with("array<"),
        "Object" | "Class" | "Component" => {
            ![
                "bool", "int", "float", "name", "byte", "string", "Vector", "Rotator",
            ]
            .contains(&ty)
                && !ty.starts_with("array<")
                && !ty.starts_with("interface")
        }
        _ => false,
    }
}

fn data_file(dir: &Path, short: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join(format!("{short}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn property<'a>(data: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    data.get("properties")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(serde_json::Value::as_str) == Some(name))
}

/// Check 3: against the per-class defaults data.
#[must_use]
pub fn check_defaults(l: &RecorderLayout, defaults_dir: &Path) -> Vec<Check> {
    defaults_checks(l, defaults_dir, "defaults")
}

fn defaults_checks(l: &RecorderLayout, defaults_dir: &Path, group: &'static str) -> Vec<Check> {
    let mut out = Vec::new();
    for f in l.fields.iter().filter(|f| f.class.starts_with("asamu.")) {
        let id = format!("{}.{}", f.class, f.name);
        let short = f.class.trim_start_matches("asamu.");
        let Some(data) = data_file(defaults_dir, short) else {
            check(&mut out, group, false, format!("{id}: no {short}.json"));
            continue;
        };
        let class_ok =
            data.get("class").and_then(serde_json::Value::as_str) == Some(f.class.as_str());
        let p = property(&data, &f.name);
        let declared = p
            .and_then(|p| p.get("declared_in"))
            .and_then(serde_json::Value::as_str);
        let ty = p
            .and_then(|p| p.get("type"))
            .and_then(serde_json::Value::as_str);
        let ok = class_ok
            && declared == Some(f.class.as_str())
            && ty.is_some_and(|t| type_matches(&f.kind, f.size, t));
        check(
            &mut out,
            group,
            ok,
            format!(
                "{id}: declared in {declared:?}, type {ty:?}, layout kind {}",
                f.kind
            ),
        );
    }
    for s in &l.sentinels {
        let id = format!("{}.{}", s.class, s.name);
        let field_ok = l
            .field(&s.class, &s.name)
            .is_some_and(|f| f.kind == "Float");
        let short = s.data_file.trim_end_matches(".json");
        let value = data_file(defaults_dir, short)
            .as_ref()
            .and_then(|d| property(d, &s.name).cloned())
            .and_then(|p| p.get("value").and_then(serde_json::Value::as_f64));
        let ok = field_ok
            && value.is_some_and(|v| (v as f32).to_bits() == (s.expected as f32).to_bits());
        check(
            &mut out,
            group,
            ok,
            format!(
                "sentinel {id} ({}): expected {} vs {} value {value:?}",
                s.object, s.expected, s.data_file
            ),
        );
    }
    out
}

fn offset_bytes_present(bytes: &[u8], offset: u64) -> bool {
    let d32 = u32::try_from(offset).map(u32::to_le_bytes).ok();
    let d32_hit = d32.is_some_and(|d| !find_all(bytes, &d).is_empty());
    let d8_hit = offset < 0x80 && u8::try_from(offset).is_ok_and(|b| bytes.contains(&b));
    d32_hit || d8_hit
}

/// Check 4: native-code justifications are linked to evidence.
#[must_use]
pub fn check_native_code_links(l: &RecorderLayout) -> Vec<Check> {
    native_code_links(l, "native_code")
}

fn native_code_links(l: &RecorderLayout, group: &'static str) -> Vec<Check> {
    let mut out = Vec::new();
    for e in &l.native_evidence {
        check(
            &mut out,
            group,
            hex_bytes(&e.bytes).is_some_and(|b| !b.is_empty()),
            format!("evidence bytes of {:?}", e.what),
        );
    }
    for f in l.fields.iter().filter(|f| f.evidence == "native_code") {
        let id = format!("{}.{}", f.class, f.name);
        let off = hex(&f.offset).unwrap_or(u64::MAX);
        let ok = l.native_evidence.iter().any(|e| {
            e.field.as_deref() == Some(id.as_str())
                && hex_bytes(&e.bytes).is_some_and(|b| offset_bytes_present(&b, off))
        });
        check(
            &mut out,
            group,
            ok,
            format!("{id}: an evidence entry shows offset {off:#x}"),
        );
    }
    for (name, s) in &l.structs {
        let claims = s
            .get("evidence")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|e| e.starts_with("native_code"));
        if claims {
            let ok = l
                .native_evidence
                .iter()
                .any(|e| e.structure.as_deref() == Some(name.as_str()));
            check(
                &mut out,
                group,
                ok,
                format!("struct {name}: evidence entry present"),
            );
        }
    }
    out
}

/// A literal layout lookup found in Python source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Lookup {
    /// `off`, `bit`, `st`, `st_size`, `st_attr` or `sym`.
    pub func: String,
    /// First argument.
    pub a: String,
    /// Second argument, if any.
    pub b: Option<String>,
}

fn quoted(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((rest[..end].to_owned(), &rest[end + 1..]))
}

/// Finds literal lookups like `L.off("Engine.Actor", "Location")` in `src`.
#[must_use]
pub fn python_lookups(src: &str) -> Vec<Lookup> {
    let mut out = Vec::new();
    for func in ["off", "bit", "st", "st_size", "st_attr", "sym"] {
        let pat = format!(".{func}(");
        let mut from = 0;
        while let Some(i) = src[from..].find(&pat) {
            let start = from + i + pat.len();
            from = start;
            let rest = src[start..].trim_start();
            let Some((a, rest)) = quoted(rest) else {
                continue;
            };
            let rest = rest.trim_start();
            let (b, rest) = match rest.strip_prefix(',') {
                Some(r) => match quoted(r.trim_start()) {
                    Some((b, r)) => (Some(b), r),
                    None => continue,
                },
                None => (None, rest),
            };
            if rest.trim_start().starts_with(')') {
                out.push(Lookup {
                    func: func.to_owned(),
                    a,
                    b,
                });
            }
        }
    }
    out.sort();
    out
}

/// Check 5: every literal lookup in the Python sources exists in the layout.
/// Returns the checks and the number of lookups found.
#[must_use]
pub fn check_python(l: &RecorderLayout, sources: &[(String, String)]) -> (Vec<Check>, usize) {
    python_checks(l, sources, "python")
}

fn python_checks(
    l: &RecorderLayout,
    sources: &[(String, String)],
    group: &'static str,
) -> (Vec<Check>, usize) {
    let mut out = Vec::new();
    let mut count = 0;
    let symbols_by_name: BTreeSet<&str> = l.symbols.keys().map(String::as_str).collect();
    for (file, src) in sources {
        for lk in python_lookups(src) {
            count += 1;
            let b = lk.b.as_deref().unwrap_or("");
            let ok = match lk.func.as_str() {
                "off" => l.field(&lk.a, b).is_some(),
                "bit" => l.field(&lk.a, b).is_some_and(|f| f.kind == "Bool"),
                "st" => l
                    .structs
                    .get(&lk.a)
                    .and_then(|s| s.get("members"))
                    .and_then(|m| m.get(b))
                    .is_some(),
                "st_size" => l.structs.get(&lk.a).and_then(|s| s.get("size")).is_some(),
                "st_attr" => l.structs.get(&lk.a).and_then(|s| s.get(b)).is_some(),
                "sym" => symbols_by_name.contains(lk.a.as_str()),
                _ => false,
            };
            check(
                &mut out,
                group,
                ok,
                format!(
                    "{file}: {}({:?}{})",
                    lk.func,
                    lk.a,
                    lk.b.as_ref()
                        .map(|b| format!(", {b:?}"))
                        .unwrap_or_default()
                ),
            );
        }
    }
    (out, count)
}

/// Check 6: against the executable.
#[must_use]
pub fn check_binary(l: &RecorderLayout, m: &MachO) -> Vec<Check> {
    let mut out = Vec::new();
    for (name, s) in &l.symbols {
        let Some(sym) = m.symbol(&s.mangled) else {
            check(
                &mut out,
                "binary",
                false,
                format!("symbol {name} ({}) is not defined", s.mangled),
            );
            continue;
        };
        let seg = m.segment_of(sym.value).map(|g| g.name.as_str());
        let want = if s.kind == "function" {
            "__TEXT"
        } else {
            "__DATA"
        };
        check(
            &mut out,
            "binary",
            seg == Some(want),
            format!("symbol {name} at {:#x} in {seg:?} (want {want})", sym.value),
        );
        if let Some(read) = s.read {
            let ext = m.extent(&s.mangled);
            check(
                &mut out,
                "binary",
                ext.is_some_and(|e| e >= read),
                format!("symbol {name}: {read} bytes read, {ext:?} bytes before the next symbol"),
            );
        }
        if let Some(v) = s.file_value_f64 {
            let got = m
                .bytes_at(sym.value, 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(f64::from_le_bytes);
            check(
                &mut out,
                "binary",
                got.is_some_and(|g| g.to_bits() == v.to_bits()),
                format!("symbol {name}: initial value {got:?} (want {v})"),
            );
        }
    }
    for e in &l.native_evidence {
        let Some(code) = m.function_bytes(&e.function) else {
            check(
                &mut out,
                "binary",
                false,
                format!("{:?}: function {} not found", e.what, e.function),
            );
            continue;
        };
        let needle = hex_bytes(&e.bytes).unwrap_or_default();
        let hits = find_all(code, &needle);
        let mut ok = !hits.is_empty();
        let mut detail = format!("{} occurrence(s) in {}", hits.len(), e.function);
        if let Some(callee) = &e.before_call_to {
            let base = m.symbol(&e.function).map_or(0, |s| s.value);
            let target = m.symbol(callee).map(|s| s.value);
            let first_call = target.and_then(|t| calls_to(code, base, t).first().copied());
            ok = ok && first_call.is_some_and(|c| hits.first().is_some_and(|h| *h < c));
            detail.push_str(&format!(
                ", first at +{:?}, first call to {callee} at +{first_call:?}",
                hits.first()
            ));
        }
        check(&mut out, "binary", ok, format!("{:?}: {detail}", e.what));
    }
    for c in &l.calls {
        let code = m.function_bytes(&c.caller);
        let base = m.symbol(&c.caller).map(|s| s.value);
        let target = m.symbol(&c.callee).map(|s| s.value);
        let n = match (code, base, target) {
            (Some(code), Some(base), Some(t)) => Some(calls_to(code, base, t).len()),
            _ => None,
        };
        check(
            &mut out,
            "binary",
            n == Some(c.count),
            format!("{:?}: {n:?} direct call(s) (want {})", c.what, c.count),
        );
    }
    out
}

/// The original executable, if present: `ASAMU_ORIGINAL_DIR` or the default
/// macOS Steam library, joined with the layout's `executable`.
#[must_use]
pub fn default_executable(l: &RecorderLayout) -> Option<PathBuf> {
    let root = std::env::var_os("ASAMU_ORIGINAL_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                PathBuf::from(h).join(
                    "Library/Application Support/Steam/steamapps/common/A Story About My Uncle",
                )
            })
        })?;
    let p = root.join(&l.executable);
    p.is_file().then_some(p)
}

/// All checks of a repository checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutReport {
    /// Every check, in group order: the Mac layout's groups, then the
    /// Windows layout's (`win32:*`).
    pub checks: Vec<Check>,
    /// Literal lookups found in the Python sources.
    pub python_lookups: usize,
    /// The Mac executable that was checked, if any.
    pub binary: Option<PathBuf>,
    /// The Windows executable that was checked, if any.
    pub win32_binary: Option<PathBuf>,
}

impl LayoutReport {
    /// The failed checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }
}

/// Runs every check on the repository at `root`: the Mac layout, then the
/// Windows layout. The binary checks of a layout run when its executable
/// exists: `binary` (a Mach-O or a PE file, told apart by its first bytes),
/// else the default install (Mac) or a local copy (Windows).
///
/// # Errors
/// Missing or malformed repository files, or an unreadable executable.
pub fn run_all(root: &Path, binary: Option<&Path>) -> Result<LayoutReport> {
    let text = std::fs::read_to_string(root.join(LAYOUT_PATH))
        .with_context(|| format!("reading {LAYOUT_PATH}"))?;
    let l = RecorderLayout::from_json(&text).context("parsing the recorder layout")?;
    let native: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(NATIVE_LAYOUT_PATH))
            .with_context(|| format!("reading {NATIVE_LAYOUT_PATH}"))?,
    )?;
    let mut sources = Vec::new();
    for p in PYTHON_SOURCES {
        let s = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
        sources.push(((*p).to_owned(), s));
    }
    let (mac_binary, win32_binary) = match binary {
        Some(p) if looks_like_pe(p) => (None, Some(p)),
        other => (other, None),
    };
    let mut checks = check_schema(&l);
    checks.extend(check_native_layout(&l, &native));
    checks.extend(check_defaults(&l, &root.join(DEFAULTS_DIR)));
    checks.extend(check_native_code_links(&l));
    let (py, python_lookups) = check_python(&l, &sources);
    checks.extend(py);
    let exe = mac_binary
        .map(Path::to_path_buf)
        .or_else(|| default_executable(&l));
    if let Some(p) = &exe {
        let data = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let m = MachO::parse(data).with_context(|| format!("parsing {}", p.display()))?;
        checks.extend(check_binary(&l, &m));
    }
    let (win, win32_exe) = run_win32(root, &l, &sources, win32_binary)?;
    checks.extend(win);
    Ok(LayoutReport {
        checks,
        python_lookups,
        binary: exe,
        win32_binary: win32_exe,
    })
}

// ----------------------------------------------------------------- Win32

/// Windows layout file, relative to the repository root.
pub const WIN32_LAYOUT_PATH: &str = "tools/trace-recorder/layout_win_x86.json";
/// Win32 binary-analysis data directory, relative to the root.
pub const WIN32_DATA_DIR: &str = "docs/reverse-engineering/data/win32";
/// Rule-derived Win32 field layout, inside [`WIN32_DATA_DIR`].
pub const WIN32_NATIVE_LAYOUT_FILE: &str = "native_layout_win32.json";
/// Local (git-ignored) copy of the Windows executable, relative to the root.
pub const WIN32_LOCAL_EXECUTABLE: &str = "research/local/win/ASAMU-Win32-Shipping.exe";
/// Environment variable naming a copy of the Windows executable.
pub const WIN32_EXECUTABLE_ENV: &str = "ASAMU_WIN32_EXE";
/// Longest function the evidence checks accept (bytes between a function's
/// first instruction and an evidence instruction).
const MAX_FUNCTION_SPAN: u64 = 0x4000;
/// How far before an evidence instruction a `near_literals` use may lie.
const NEAR_LITERAL_BEFORE: u32 = 0x80;
/// How far after the start of an evidence instruction it may lie.
const NEAR_LITERAL_AFTER: u32 = 0x40;
/// Deepest nesting of `called_by` anchors.
const MAX_ANCHOR_DEPTH: u8 = 4;

/// A minimal reader of 32-bit PE images: header values, sections, bytes at
/// an RVA and the lookups the Windows evidence anchors need.
pub mod pe {
    use anyhow::{Context, Result, bail};

    const MAX_SECTIONS: u16 = 96;
    /// Machine type of an i386 image.
    pub const MACHINE_I386: u16 = 0x014C;

    /// A section header.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Section {
        /// Name (`.text`, `.data`, ...).
        pub name: String,
        /// RVA of the section.
        pub rva: u32,
        /// Size in memory.
        pub virtual_size: u32,
        /// File offset of the initialised part.
        pub raw_offset: u32,
        /// Size of the initialised part in the file.
        pub raw_size: u32,
    }

    /// A parsed PE32 image.
    #[derive(Clone, Debug)]
    pub struct Pe {
        data: Vec<u8>,
        /// COFF machine type.
        pub machine: u16,
        /// COFF time stamp.
        pub time_date_stamp: u32,
        /// Preferred image base.
        pub image_base: u32,
        /// Size of the loaded image.
        pub size_of_image: u32,
        /// Section headers.
        pub sections: Vec<Section>,
    }

    fn rd<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N]> {
        let end = off.checked_add(N).context("offset overflow")?;
        let s = b
            .get(off..end)
            .with_context(|| format!("read of {N} bytes at {off:#x} is out of bounds"))?;
        let mut out = [0_u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    fn u16le(b: &[u8], off: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(rd(b, off)?))
    }

    fn u32le(b: &[u8], off: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(rd(b, off)?))
    }

    /// Offset of the first occurrence of `needle` in `hay` at or after `from`.
    fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        let first = *needle.first()?;
        let mut i = from;
        while let Some(rest) = hay.get(i..) {
            let p = rest.iter().position(|b| *b == first)?;
            let at = i.checked_add(p)?;
            if hay.get(at..at.checked_add(needle.len())?) == Some(needle) {
                return Some(at);
            }
            i = at.checked_add(1)?;
        }
        None
    }

    impl Pe {
        /// Parses a PE32 (32-bit) image.
        ///
        /// # Errors
        /// Truncated or malformed headers, or a PE32+ image.
        pub fn parse(data: Vec<u8>) -> Result<Self> {
            if data.get(..2) != Some(b"MZ".as_slice()) {
                bail!("not a PE image (no MZ header)");
            }
            let pe = usize::try_from(u32le(&data, 0x3C)?)?;
            if rd::<4>(&data, pe)? != *b"PE\0\0" {
                bail!("not a PE image (no PE signature)");
            }
            let coff = pe.checked_add(4).context("offset overflow")?;
            let machine = u16le(&data, coff)?;
            let count = u16le(&data, coff + 2)?;
            let time_date_stamp = u32le(&data, coff + 4)?;
            let opt_size = usize::from(u16le(&data, coff + 16)?);
            let opt = coff + 20;
            if u16le(&data, opt)? != 0x010B {
                bail!("not a PE32 image (optional header magic)");
            }
            let image_base = u32le(&data, opt + 28)?;
            let size_of_image = u32le(&data, opt + 56)?;
            if count > MAX_SECTIONS {
                bail!("{count} sections");
            }
            let table = opt.checked_add(opt_size).context("offset overflow")?;
            let mut sections = Vec::new();
            for i in 0..usize::from(count) {
                let h = table + 40 * i;
                let raw: [u8; 8] = rd(&data, h)?;
                let len = raw.iter().position(|b| *b == 0).unwrap_or(8);
                let s = Section {
                    name: String::from_utf8_lossy(&raw[..len]).into_owned(),
                    virtual_size: u32le(&data, h + 8)?,
                    rva: u32le(&data, h + 12)?,
                    raw_size: u32le(&data, h + 16)?,
                    raw_offset: u32le(&data, h + 20)?,
                };
                let end = u64::from(s.raw_offset) + u64::from(s.raw_size);
                if end > data.len() as u64 {
                    bail!("section {} extends past the end of the file", s.name);
                }
                sections.push(s);
            }
            Ok(Self {
                data,
                machine,
                time_date_stamp,
                image_base,
                size_of_image,
                sections,
            })
        }

        /// The section called `name`.
        #[must_use]
        pub fn section(&self, name: &str) -> Option<&Section> {
            self.sections.iter().find(|s| s.name == name)
        }

        /// The section whose memory range holds `len` bytes at `rva`
        /// (initialised or zero-fill).
        #[must_use]
        pub fn section_of(&self, rva: u32, len: u64) -> Option<&Section> {
            self.sections.iter().find(|s| {
                let size = u64::from(s.virtual_size.max(s.raw_size));
                let start = u64::from(s.rva);
                u64::from(rva) >= start && u64::from(rva) + len <= start + size
            })
        }

        /// `len` bytes at `rva`, if they are all stored in the file.
        #[must_use]
        pub fn bytes_at(&self, rva: u32, len: usize) -> Option<&[u8]> {
            let s = self.sections.iter().find(|s| {
                rva >= s.rva && u64::from(rva - s.rva) + len as u64 <= u64::from(s.raw_size)
            })?;
            let off = usize::try_from(s.raw_offset).ok()? + usize::try_from(rva - s.rva).ok()?;
            self.data.get(off..off.checked_add(len)?)
        }

        /// The little-endian `u32` at `rva`.
        #[must_use]
        pub fn u32_at(&self, rva: u32) -> Option<u32> {
            let b = self.bytes_at(rva, 4)?;
            Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }

        /// RVA of the address `va` (a preferred-base address stored in the
        /// file), if it lies inside the image.
        #[must_use]
        pub fn rva_of(&self, va: u32) -> Option<u32> {
            let rva = va.checked_sub(self.image_base)?;
            (rva < self.size_of_image).then_some(rva)
        }

        /// The function a vtable slot points at: `vtable_rva` is the RVA of
        /// slot 0, slots are 4-byte addresses into `.text`.
        #[must_use]
        pub fn vtable_slot(&self, vtable_rva: u32, slot: u64) -> Option<u32> {
            let at = u64::from(vtable_rva).checked_add(slot.checked_mul(4)?)?;
            let f = self.rva_of(self.u32_at(u32::try_from(at).ok()?)?)?;
            (self.section_of(f, 1)?.name == ".text").then_some(f)
        }

        /// The function registered under `name` in the native function name
        /// table: the NUL-terminated ANSI literal `name` is referenced by a
        /// 4-byte-aligned address that is followed by the function's address.
        /// `None` unless exactly one function is found.
        #[must_use]
        pub fn exec_function(&self, name: &str) -> Option<u32> {
            let mut literal = Vec::with_capacity(name.len() + 2);
            literal.push(0);
            literal.extend_from_slice(name.as_bytes());
            literal.push(0);
            let mut found: Option<u32> = None;
            for s in &self.sections {
                let raw = self.bytes_at(s.rva, usize::try_from(s.raw_size).ok()?)?;
                let mut from = 0;
                while let Some(at) = find_from(raw, &literal, from) {
                    from = at + 1;
                    let rva = s.rva.checked_add(u32::try_from(at + 1).ok()?)?;
                    let va = self.image_base.checked_add(rva)?.to_le_bytes();
                    for t in &self.sections {
                        let table = self.bytes_at(t.rva, usize::try_from(t.raw_size).ok()?)?;
                        let mut k = 0;
                        while let Some(p) = find_from(table, &va, k) {
                            k = p + 1;
                            if p % 4 != 0 {
                                continue;
                            }
                            let Some(f) = table
                                .get(p + 4..p + 8)
                                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                .and_then(|va| self.rva_of(va))
                            else {
                                continue;
                            };
                            if self.section_of(f, 1).is_none_or(|x| x.name != ".text") {
                                continue;
                            }
                            if found.is_some_and(|x| x != f) {
                                return None;
                            }
                            found = Some(f);
                        }
                    }
                }
            }
            found
        }

        /// RVAs of the direct calls (`E8 rel32`) in `start..end` whose target
        /// is `target`.
        #[must_use]
        pub fn calls(&self, start: u32, end: u32, target: u32) -> Vec<u32> {
            self.relative(0xE8, start, end, target)
        }

        /// RVAs of the direct jumps (`E9 rel32`) in `start..end` whose target
        /// is `target`.
        #[must_use]
        pub fn jumps(&self, start: u32, end: u32, target: u32) -> Vec<u32> {
            self.relative(0xE9, start, end, target)
        }

        /// RVAs of the NUL-terminated UTF-16LE literal `text` in the data
        /// sections. A match starts at an even offset and follows a NUL unit
        /// (the end of the previous literal), so a longer literal that
        /// merely ends with `text` does not count.
        #[must_use]
        pub fn utf16_literals(&self, text: &str) -> Vec<u32> {
            let mut out = Vec::new();
            if text.is_empty() {
                return out;
            }
            let mut needle: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            needle.extend_from_slice(&[0, 0]);
            for s in self.sections.iter().filter(|s| s.name != ".text") {
                let Some(raw) = usize::try_from(s.raw_size)
                    .ok()
                    .and_then(|len| self.bytes_at(s.rva, len))
                else {
                    continue;
                };
                let mut from = 0;
                while let Some(at) = find_from(raw, &needle, from) {
                    from = at + 1;
                    let follows_nul = at == 0
                        || at
                            .checked_sub(2)
                            .and_then(|b| raw.get(b..at))
                            .is_some_and(|b| b == [0, 0]);
                    if at % 2 != 0 || !follows_nul {
                        continue;
                    }
                    if let Some(rva) = u32::try_from(at).ok().and_then(|a| s.rva.checked_add(a)) {
                        out.push(rva);
                    }
                }
            }
            out
        }

        /// RVAs in `start..end` of `.text` at which the preferred-base
        /// address of `target` is stored (an absolute 4-byte operand).
        #[must_use]
        pub fn address_refs(&self, start: u32, end: u32, target: u32) -> Vec<u32> {
            let mut out = Vec::new();
            let (Some(va), Some(text)) =
                (self.image_base.checked_add(target), self.section(".text"))
            else {
                return out;
            };
            let lo = start.max(text.rva);
            let hi = end.min(text.rva.saturating_add(text.raw_size));
            let Some(code) = hi
                .checked_sub(lo)
                .and_then(|l| usize::try_from(l).ok())
                .and_then(|l| self.bytes_at(lo, l))
            else {
                return out;
            };
            let needle = va.to_le_bytes();
            let mut from = 0;
            while let Some(at) = find_from(code, &needle, from) {
                from = at + 1;
                if let Some(r) = u32::try_from(at).ok().and_then(|a| lo.checked_add(a)) {
                    out.push(r);
                }
            }
            out
        }

        /// Every place in `.text` that stores the address of `target`.
        #[must_use]
        pub fn text_refs(&self, target: u32) -> Vec<u32> {
            self.section(".text").map_or_else(Vec::new, |t| {
                self.address_refs(t.rva, t.rva.saturating_add(t.raw_size), target)
            })
        }

        /// RVA just past the code of the function that starts at `start`:
        /// the first two-byte `int3` padding, at most `max` bytes on.
        #[must_use]
        pub fn padded_end(&self, start: u32, max: u32) -> u32 {
            let Some(text) = self.section(".text") else {
                return start;
            };
            let end = start
                .saturating_add(max)
                .min(text.rva.saturating_add(text.raw_size));
            let Some(code) = end
                .checked_sub(start)
                .and_then(|l| usize::try_from(l).ok())
                .and_then(|l| self.bytes_at(start, l))
            else {
                return start;
            };
            let cut = code
                .windows(2)
                .position(|w| w == [0xCC, 0xCC])
                .unwrap_or(code.len());
            start.saturating_add(u32::try_from(cut).unwrap_or(0))
        }

        fn relative(&self, opcode: u8, start: u32, end: u32, target: u32) -> Vec<u32> {
            let mut out = Vec::new();
            // Clamp the range to the bytes its section stores in the file.
            let Some(stored) = self
                .sections
                .iter()
                .find(|s| start >= s.rva && start - s.rva < s.raw_size)
                .map(|s| s.raw_size - (start - s.rva))
            else {
                return out;
            };
            let Some(len) = end
                .checked_sub(start)
                .map(|l| l.min(stored))
                .and_then(|l| usize::try_from(l).ok())
            else {
                return out;
            };
            let Some(code) = self.bytes_at(start, len) else {
                return out;
            };
            for (i, w) in code.windows(5).enumerate() {
                if w[0] != opcode {
                    continue;
                }
                let rel = i32::from_le_bytes([w[1], w[2], w[3], w[4]]);
                let Ok(i) = u32::try_from(i) else { break };
                let next = start.wrapping_add(i).wrapping_add(5);
                if next.wrapping_add_signed(rel) == target {
                    out.push(start.wrapping_add(i));
                }
            }
            out
        }
    }

    /// Builds a small synthetic PE32 image (tests): `.text` with `code` at
    /// `text_rva`, `.rdata` with `rdata` at `rdata_rva`, and `.data` with
    /// `data` at `data_rva` followed by `bss` zero-fill bytes.
    #[cfg(test)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn synthetic(
        image_base: u32,
        time_date_stamp: u32,
        text_rva: u32,
        code: &[u8],
        rdata_rva: u32,
        rdata: &[u8],
        data_rva: u32,
        data: &[u8],
        bss: u32,
    ) -> Vec<u8> {
        fn p16(v: &mut Vec<u8>, x: u16) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        fn p32(v: &mut Vec<u8>, x: u32) {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let pe = 0x40_u32;
        let opt_size = 224_u16;
        let headers = pe + 4 + 20 + u32::from(opt_size) + 3 * 40;
        let text_off = headers;
        let rdata_off = text_off + code.len() as u32;
        let data_off = rdata_off + rdata.len() as u32;
        let size_of_image = data_rva + data.len() as u32 + bss;
        let mut v = vec![0_u8; pe as usize];
        v[0] = b'M';
        v[1] = b'Z';
        v[0x3C..0x40].copy_from_slice(&pe.to_le_bytes());
        v.extend_from_slice(b"PE\0\0");
        p16(&mut v, MACHINE_I386);
        p16(&mut v, 3);
        p32(&mut v, time_date_stamp);
        p32(&mut v, 0);
        p32(&mut v, 0);
        p16(&mut v, opt_size);
        p16(&mut v, 0x0122);
        let opt = v.len();
        v.resize(opt + usize::from(opt_size), 0);
        v[opt..opt + 2].copy_from_slice(&0x010B_u16.to_le_bytes());
        v[opt + 28..opt + 32].copy_from_slice(&image_base.to_le_bytes());
        v[opt + 56..opt + 60].copy_from_slice(&size_of_image.to_le_bytes());
        for (name, rva, vsize, off, rsize) in [
            (
                ".text",
                text_rva,
                code.len() as u32,
                text_off,
                code.len() as u32,
            ),
            (
                ".rdata",
                rdata_rva,
                rdata.len() as u32,
                rdata_off,
                rdata.len() as u32,
            ),
            (
                ".data",
                data_rva,
                data.len() as u32 + bss,
                data_off,
                data.len() as u32,
            ),
        ] {
            let mut n = [0_u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            v.extend_from_slice(&n);
            p32(&mut v, vsize);
            p32(&mut v, rva);
            p32(&mut v, rsize);
            p32(&mut v, off);
            v.extend_from_slice(&[0_u8; 16]);
        }
        v.extend_from_slice(code);
        v.extend_from_slice(rdata);
        v.extend_from_slice(data);
        v
    }
}

use pe::Pe;

/// The repository data a Windows layout is checked against.
#[derive(Clone, Debug, PartialEq)]
pub struct Win32Data {
    /// `native_layout_win32.json`: fields computed by the Win32 rules.
    pub native_layout: serde_json::Value,
    /// `globals.json`.
    pub globals: serde_json::Value,
    /// `functions.json`.
    pub functions: serde_json::Value,
    /// `image.json`.
    pub image: serde_json::Value,
    /// `class_sizes.json` (vtable RVAs of the native classes).
    pub class_sizes: serde_json::Value,
}

fn json_file(path: &Path) -> Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

impl Win32Data {
    /// Reads the five data files from `dir`.
    ///
    /// # Errors
    /// A missing or malformed file.
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            native_layout: json_file(&dir.join(WIN32_NATIVE_LAYOUT_FILE))?,
            globals: json_file(&dir.join("globals.json"))?,
            functions: json_file(&dir.join("functions.json"))?,
            image: json_file(&dir.join("image.json"))?,
            class_sizes: json_file(&dir.join("class_sizes.json"))?,
        })
    }

    fn named<'a>(
        list: &'a serde_json::Value,
        key: &str,
        name: &str,
    ) -> Option<&'a serde_json::Value> {
        list.get(key)?
            .as_array()?
            .iter()
            .find(|x| x.get("name").and_then(serde_json::Value::as_str) == Some(name))
    }

    /// The entry of `globals.json` called `name`.
    #[must_use]
    pub fn global(&self, name: &str) -> Option<&serde_json::Value> {
        Self::named(&self.globals, "globals", name)
    }

    /// The entry of `functions.json` called `name`.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&serde_json::Value> {
        Self::named(&self.functions, "functions", name)
    }

    /// RVA of the vtable of the native class `cpp_name` (`class_sizes.json`).
    #[must_use]
    pub fn vtable_rva(&self, cpp_name: &str) -> Option<u32> {
        let columns = self.class_sizes.get("columns")?.as_array()?;
        let col = |name: &str| columns.iter().position(|c| c.as_str() == Some(name));
        let (cpp, vt) = (col("cpp_name")?, col("vtable_rva")?);
        self.class_sizes
            .get("classes")?
            .as_array()?
            .iter()
            .find(|r| r.get(cpp).and_then(serde_json::Value::as_str) == Some(cpp_name))
            .and_then(|r| r.get(vt)?.as_u64())
            .and_then(|v| u32::try_from(v).ok())
    }
}

type Row = (u64, String, u64, Option<u64>);

fn row_in(fields: Option<&serde_json::Value>, name: &str) -> Option<Row> {
    fields?.as_array()?.iter().find_map(|r| {
        let r = r.as_array()?;
        (r.get(1)?.as_str()? == name).then(|| {
            Some((
                hex(r.first()?.as_str()?)?,
                r.get(2)?.as_str()?.to_owned(),
                r.get(3)?.as_u64()?,
                r.get(4)?.as_u64(),
            ))
        })?
    })
}

/// The row of `class.name` in the rule-derived layout (own fields of a
/// class, members of a struct, or a single row of a class whose whole
/// layout is not listed).
fn win32_row(native: &serde_json::Value, container: &str, name: &str) -> Option<Row> {
    ["classes", "structs", "other_fields"]
        .iter()
        .find_map(|section| row_in(native.get(section)?.get(container)?.get("fields"), name))
}

fn hex32(s: Option<&String>) -> Option<u32> {
    hex(s?).and_then(|v| u32::try_from(v).ok())
}

/// An optional RVA on one line (`0x00635450` or `none`).
fn show_rva(v: Option<u32>) -> String {
    v.map_or_else(|| "none".to_owned(), |v| format!("{v:#010x}"))
}

impl EvidenceAnchor {
    /// The anchor in words.
    fn describe(&self) -> String {
        let slot = self.slot.unwrap_or(u64::MAX);
        if let Some(f) = &self.function {
            format!("functions.json {f}")
        } else if let Some(e) = &self.exec {
            format!("native function name {e}")
        } else if let Some(c) = &self.vtable {
            format!("vtable of {c}, slot {slot}")
        } else if let Some(c) = &self.tail_of_vtable {
            format!("tail jump of vtable of {c}, slot {slot}")
        } else if let Some(t) = &self.literal {
            format!("only user of the literal {t:?}")
        } else if let Some(c) = &self.called_by {
            format!(
                "called by the function at {} ({})",
                c.rva.as_deref().unwrap_or("?"),
                c.describe()
            )
        } else {
            "empty".to_owned()
        }
    }

    /// Exactly one way of identifying the function, with the members that
    /// way needs. A nested anchor (`called_by`) also names its function.
    fn well_formed(&self, nested: bool, depth: u8) -> bool {
        let kinds = [
            self.function.is_some(),
            self.exec.is_some(),
            self.vtable.is_some(),
            self.tail_of_vtable.is_some(),
            self.literal.is_some(),
            self.called_by.is_some(),
        ]
        .iter()
        .filter(|k| **k)
        .count();
        kinds == 1
            && (self.vtable.is_some() || self.tail_of_vtable.is_some()) == self.slot.is_some()
            && self.literal.as_ref().is_none_or(|t| !t.is_empty())
            && nested == self.rva.is_some()
            && nested == hex32(self.rva.as_ref()).is_some()
            && self
                .called_by
                .as_ref()
                .is_none_or(|c| depth < MAX_ANCHOR_DEPTH && c.well_formed(true, depth + 1))
    }

    /// Whether the data files know what the anchor names. `start` is the
    /// RVA the layout gives for the anchored function.
    fn known_to(&self, start: Option<u64>, d: &Win32Data) -> bool {
        if let Some(f) = &self.function {
            d.function(f)
                .is_some_and(|x| x.get("rva").and_then(serde_json::Value::as_u64) == start)
        } else if let Some(c) = self.vtable.as_ref().or(self.tail_of_vtable.as_ref()) {
            d.vtable_rva(c).is_some()
        } else if let Some(c) = &self.called_by {
            c.known_to(c.rva.as_deref().and_then(hex), d)
        } else {
            self.exec.is_some() || self.literal.is_some()
        }
    }
}

/// UTF-16 literals of the image and where `.text` uses them, looked up once
/// per literal.
#[derive(Default)]
struct Literals {
    rvas: BTreeMap<String, Vec<u32>>,
    refs: BTreeMap<String, Vec<u32>>,
}

impl Literals {
    /// Where the literal `text` is stored.
    fn rvas(&mut self, pe: &Pe, text: &str) -> Vec<u32> {
        self.rvas
            .entry(text.to_owned())
            .or_insert_with(|| pe.utf16_literals(text))
            .clone()
    }

    /// Every place in `.text` that uses the address of the literal `text`.
    fn refs(&mut self, pe: &Pe, text: &str) -> Vec<u32> {
        if let Some(r) = self.refs.get(text) {
            return r.clone();
        }
        let mut out: Vec<u32> = self
            .rvas(pe, text)
            .iter()
            .flat_map(|r| pe.text_refs(*r))
            .collect();
        out.sort_unstable();
        out.dedup();
        self.refs.insert(text.to_owned(), out.clone());
        out
    }

    /// Whether the address of the literal `text` is used within
    /// [`NEAR_LITERAL_BEFORE`] bytes before or [`NEAR_LITERAL_AFTER`] bytes
    /// after `rva`.
    fn used_near(&mut self, pe: &Pe, text: &str, rva: u32) -> bool {
        let lo = rva.saturating_sub(NEAR_LITERAL_BEFORE);
        let hi = rva.saturating_add(NEAR_LITERAL_AFTER);
        self.rvas(pe, text)
            .iter()
            .any(|l| !pe.address_refs(lo, hi, *l).is_empty())
    }
}

/// Win32 check 1: well-formed entries.
#[must_use]
pub fn check_win32_schema(l: &RecorderLayout) -> Vec<Check> {
    const G: &str = "win32:schema";
    let mut out = Vec::new();
    check(
        &mut out,
        G,
        l.schema == LAYOUT_SCHEMA,
        format!("schema {:?}", l.schema),
    );
    check(
        &mut out,
        G,
        l.pointer_size == 4,
        format!("pointer size {}", l.pointer_size),
    );
    let image_ok = l.image.as_ref().is_some_and(|i| {
        hex(&i.image_base).is_some()
            && i.time_date_stamp != 0
            && i.size_of_image != 0
            && l.executable.ends_with(&i.module)
    });
    check(&mut out, G, image_ok, "image block".to_owned());
    for (name, s) in &l.symbols {
        let rva = hex32(s.rva.as_ref());
        let ok = !s.mangled.is_empty()
            && rva.is_some()
            && match s.kind.as_str() {
                "function" => {
                    s.section.as_deref() == Some(".text")
                        && hex32(s.end_rva.as_ref()).is_some_and(|e| rva.is_some_and(|r| e > r))
                }
                "data" => s.read.is_some_and(|r| r > 0) && s.section.as_deref() == Some(".data"),
                _ => false,
            };
        check(&mut out, G, ok, format!("symbol {name} (rva {:?})", s.rva));
    }
    schema_fields(l, G, &mut out);
    for e in &l.native_evidence {
        let rva = hex32(e.rva.as_ref());
        let start = hex32(e.function_rva.as_ref());
        let anchor_ok = e
            .anchor
            .as_ref()
            .is_none_or(|a| a.well_formed(false, 0) && start.is_some());
        let within = match (rva, start) {
            // The instruction lies inside the function.
            (Some(r), Some(s)) => r >= s && u64::from(r - s) < MAX_FUNCTION_SPAN,
            // A scanned instruction sequence: no function start is known,
            // so no anchor can be given either.
            (Some(_), None) => e.anchor.is_none(),
            _ => false,
        };
        let subject = usize::from(e.field.is_some())
            + usize::from(e.structure.is_some())
            + usize::from(e.symbol.is_some());
        let ok = anchor_ok
            && within
            && subject <= 1
            && e.field.is_some() == e.offset.is_some()
            && (e.bit.is_none() || e.field.is_some())
            && e.near_literals.iter().all(|t| !t.is_empty());
        check(
            &mut out,
            G,
            ok,
            format!("evidence {:?}: rva, function and anchor", e.what),
        );
    }
    out
}

/// Win32 check 2: every field and structure against the rule-derived layout
/// (`native_layout_win32.json`).
#[must_use]
pub fn check_win32_layout(l: &RecorderLayout, native: &serde_json::Value) -> Vec<Check> {
    const G: &str = "win32:layout";
    let mut out = Vec::new();
    let ptr = l.pointer_size;
    let st = |name: &str, member: &str| -> Option<u64> {
        l.structs.get(name)?.get("members")?.get(member)?.as_u64()
    };
    let st_size = |name: &str| -> Option<u64> { l.structs.get(name)?.get("size")?.as_u64() };
    for f in &l.fields {
        let id = format!("{}.{}", f.class, f.name);
        let off = hex(&f.offset);
        let derived: Option<Row> = if f.name.contains('.') {
            // A dotted field: the sum of its member path, each step a row.
            native
                .get("member_paths")
                .and_then(|m| m.get(&id))
                .and_then(|p| p.get("steps"))
                .and_then(serde_json::Value::as_array)
                .and_then(|steps| {
                    let names: Vec<&str> = f.name.split('.').collect();
                    if steps.len() != names.len() {
                        return None;
                    }
                    let mut total = 0_u64;
                    let mut last = None;
                    for (i, (step, name)) in steps.iter().zip(&names).enumerate() {
                        let container = step.get(0)?.as_str()?;
                        if step.get(1)?.as_str()? != *name || (i == 0 && container != f.class) {
                            return None;
                        }
                        let row = win32_row(native, container, name)?;
                        if hex(step.get(2)?.as_str()?)? != row.0 {
                            return None;
                        }
                        total = total.checked_add(row.0)?;
                        last = Some(row);
                    }
                    last.map(|(_, kind, size, bit)| (total, kind, size, bit))
                })
        } else {
            win32_row(native, &f.class, &f.name)
        };
        let ok = derived.as_ref().is_some_and(|(o, kind, size, bit)| {
            Some(*o) == off && *kind == f.kind && *size == f.size && *bit == f.bit.map(u64::from)
        });
        check(
            &mut out,
            G,
            ok,
            format!(
                "{id}: layout {:?} {} {} {:?} vs rules {derived:?}",
                off, f.kind, f.size, f.bit
            ),
        );
        // Sizes that follow from the pointer size.
        let size_ok = match f.kind.as_str() {
            "Object" | "Class" | "Component" => f.size == ptr,
            "Array" => Some(f.size) == st_size("TArray"),
            "Str" => Some(f.size) == st_size("FString"),
            "Name" => Some(f.size) == st_size("FName"),
            _ => true,
        };
        check(
            &mut out,
            G,
            size_ok,
            format!("{id}: {} of {} bytes", f.kind, f.size),
        );
    }
    let tarray = st("TArray", "data") == Some(0)
        && st("TArray", "count") == Some(ptr)
        && st("TArray", "max") == Some(ptr + 4)
        && st_size("TArray") == Some(ptr + 8);
    check(
        &mut out,
        G,
        tarray,
        "struct TArray: {data, count, max}".to_owned(),
    );
    let fstring = st("FString", "data") == Some(0)
        && st("FString", "count") == Some(ptr)
        && st_size("FString") == st_size("TArray");
    check(&mut out, G, fstring, "struct FString: a TArray".to_owned());
    let fname = st("FName", "index") == Some(0)
        && st("FName", "number") == Some(4)
        && st_size("FName") == Some(8);
    check(
        &mut out,
        G,
        fname,
        "struct FName: {index, number}".to_owned(),
    );
    let key_bind = native
        .get("structs")
        .and_then(|s| s.get("Engine.Input.KeyBind"));
    let member = |name: &str| -> Option<u64> { row_in(key_bind?.get("fields"), name).map(|r| r.0) };
    let key_bind_ok = key_bind
        .and_then(|k| k.get("size"))
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|s| Some(s) == st_size("KeyBind"))
        && member("Name").is_some_and(|o| Some(o) == st("KeyBind", "Name"))
        && member("Command").is_some_and(|o| Some(o) == st("KeyBind", "Command"));
    check(
        &mut out,
        G,
        key_bind_ok,
        "struct KeyBind vs Engine.Input.KeyBind".to_owned(),
    );
    out
}

/// Win32 check 3: the Windows layout reads what the Mac layout reads.
#[must_use]
pub fn check_win32_same_reads(mac: &RecorderLayout, win: &RecorderLayout) -> Vec<Check> {
    const G: &str = "win32:mac";
    let mut out = Vec::new();
    let fields = |l: &RecorderLayout| -> Vec<(String, String, String, Option<u32>)> {
        l.fields
            .iter()
            .map(|f| (f.class.clone(), f.name.clone(), f.kind.clone(), f.bit))
            .collect()
    };
    check(
        &mut out,
        G,
        fields(mac) == fields(win),
        format!(
            "same fields, kinds and bits in the same order ({} vs {})",
            mac.fields.len(),
            win.fields.len()
        ),
    );
    check(
        &mut out,
        G,
        mac.sentinels == win.sentinels,
        format!("same {} sentinels", mac.sentinels.len()),
    );
    let symbols = |l: &RecorderLayout| -> Vec<(String, String, Option<String>)> {
        l.symbols
            .iter()
            .map(|(n, s)| (n.clone(), s.kind.clone(), s.ty.clone()))
            .collect()
    };
    check(
        &mut out,
        G,
        symbols(mac) == symbols(win),
        "same symbols, kinds and value types".to_owned(),
    );
    let structs = |l: &RecorderLayout| -> Vec<(String, Vec<String>, Vec<String>)> {
        l.structs
            .iter()
            .map(|(n, s)| {
                let keys = |v: Option<&serde_json::Value>| -> Vec<String> {
                    v.and_then(serde_json::Value::as_object)
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default()
                };
                (n.clone(), keys(s.get("members")), keys(Some(s)))
            })
            .collect()
    };
    check(
        &mut out,
        G,
        structs(mac) == structs(win),
        "same structures, members and attributes".to_owned(),
    );
    let calls = |l: &RecorderLayout| -> Vec<usize> { l.calls.iter().map(|c| c.count).collect() };
    check(
        &mut out,
        G,
        calls(mac) == calls(win) && !win.calls.is_empty(),
        "same call-count checks".to_owned(),
    );
    out
}

/// Win32 check 5: evidence entries against the rule-derived layout.
#[must_use]
pub fn check_win32_native_code(l: &RecorderLayout, native: &serde_json::Value) -> Vec<Check> {
    const G: &str = "win32:native_code";
    let mut out = native_code_links(l, G);
    let image_base = l.image.as_ref().and_then(|i| hex(&i.image_base));
    for e in &l.native_evidence {
        let bytes = hex_bytes(&e.bytes).unwrap_or_default();
        if let (Some(field), Some(offset)) = (&e.field, &e.offset) {
            let off = hex(offset);
            let row = field
                .rsplit_once('.')
                .and_then(|(class, name)| win32_row(native, class, name));
            let ok = row
                .as_ref()
                .is_some_and(|(o, _, _, bit)| Some(*o) == off && *bit == e.bit.map(u64::from))
                && off.is_some_and(|o| offset_bytes_present(&bytes, o));
            check(
                &mut out,
                G,
                ok,
                format!(
                    "{field}: instruction offset {offset} bit {:?} vs rules {row:?}",
                    e.bit
                ),
            );
        }
        if let Some(symbol) = &e.symbol {
            let address = l
                .symbols
                .get(symbol)
                .and_then(|s| hex(s.rva.as_ref()?))
                .zip(image_base)
                .and_then(|(rva, base)| u32::try_from(base.checked_add(rva)?).ok());
            let ok = address.is_some_and(|a| !find_all(&bytes, &a.to_le_bytes()).is_empty());
            check(
                &mut out,
                G,
                ok,
                format!(
                    "{symbol}: the instruction holds its address {}",
                    show_rva(address)
                ),
            );
        }
    }
    // Every symbol the recorder reads is tied to an instruction.
    for (name, s) in &l.symbols {
        if s.kind == "data" {
            let ok = l
                .native_evidence
                .iter()
                .any(|e| e.symbol.as_deref() == Some(name.as_str()));
            check(
                &mut out,
                G,
                ok,
                format!("symbol {name}: evidence entry present"),
            );
        }
    }
    out
}

/// Win32 check 6: against the binary-analysis data files.
#[must_use]
pub fn check_win32_data(l: &RecorderLayout, d: &Win32Data) -> Vec<Check> {
    const G: &str = "win32:data";
    let mut out = Vec::new();
    fn as_u64(v: Option<&serde_json::Value>) -> Option<u64> {
        v.and_then(serde_json::Value::as_u64)
    }
    fn as_str(v: Option<&serde_json::Value>) -> Option<&str> {
        v.and_then(serde_json::Value::as_str)
    }
    let image_ok = l.image.as_ref().is_some_and(|i| {
        as_str(d.image.get("image_base")).and_then(hex) == hex(&i.image_base)
            && as_u64(d.image.get("time_date_stamp")) == Some(i.time_date_stamp)
            && as_u64(d.image.get("size_of_image")) == Some(i.size_of_image)
            && as_str(d.image.get("executable")) == Some(l.executable.as_str())
    });
    check(
        &mut out,
        G,
        image_ok,
        "image block vs image.json".to_owned(),
    );
    let same_build = [&d.native_layout, &d.globals, &d.functions, &d.image]
        .iter()
        .all(|j| as_str(j.get("game_build")) == Some(l.game_build.as_str()));
    check(
        &mut out,
        G,
        same_build,
        format!("game build {} in every data file", l.game_build),
    );
    for (name, s) in &l.symbols {
        let rva = hex(s.rva.as_deref().unwrap_or(""));
        let ok = if s.kind == "function" {
            d.function(name).is_some_and(|f| {
                as_u64(f.get("rva")) == rva
                    && as_u64(f.get("end_rva")) == hex(s.end_rva.as_deref().unwrap_or(""))
                    && as_str(f.get("calling_convention")) == s.abi.as_deref()
            })
        } else {
            d.global(name).is_some_and(|g| {
                as_u64(g.get("rva")) == rva
                    && as_u64(g.get("size")) == s.read
                    && as_str(g.get("type")) == s.ty.as_deref()
                    && as_str(g.get("section")) == s.section.as_deref()
                    && s.file_value_f64.is_none_or(|v| {
                        g.get("file_value_f64")
                            .and_then(serde_json::Value::as_f64)
                            .is_some_and(|x| x.to_bits() == v.to_bits())
                    })
            })
        };
        check(
            &mut out,
            G,
            ok,
            format!("symbol {name} (rva {:?}) vs the data files", s.rva),
        );
    }
    for e in &l.native_evidence {
        let Some(a) = &e.anchor else { continue };
        let start = hex(e.function_rva.as_deref().unwrap_or(""));
        let ok = a.known_to(start, d);
        check(
            &mut out,
            G,
            ok,
            format!("anchor of {} at {:?}: {}", e.function, e.rva, a.describe()),
        );
    }
    let total = d
        .native_layout
        .get("validation")
        .and_then(|v| v.get("class_sizes"));
    let sizes_ok = total.is_some_and(|c| {
        as_u64(c.get("different")) == Some(0)
            && as_u64(c.get("equal")) == as_u64(c.get("native_script_classes_compared"))
            && as_u64(c.get("equal")).is_some_and(|n| n > 0)
    });
    check(
        &mut out,
        G,
        sizes_ok,
        format!(
            "the Win32 rules reproduce every compared native class size ({:?})",
            total.and_then(|c| c.get("equal"))
        ),
    );
    out
}

/// [`MAX_FUNCTION_SPAN`] as an RVA distance.
fn function_span() -> u32 {
    u32::try_from(MAX_FUNCTION_SPAN).unwrap_or(u32::MAX)
}

/// The function an anchor identifies in the image. `want` is the RVA the
/// layout gives for that function: an anchor that cannot find a function by
/// itself (a tail jump, a literal, a caller) confirms or rejects it.
fn resolve_anchor(
    a: &EvidenceAnchor,
    want: Option<u32>,
    pe: &Pe,
    d: &Win32Data,
    literals: &mut Literals,
    depth: u8,
) -> Option<u32> {
    if let Some(name) = &a.exec {
        return pe.exec_function(name);
    }
    if let Some(class) = &a.vtable {
        return pe.vtable_slot(d.vtable_rva(class)?, a.slot?);
    }
    if let Some(class) = &a.tail_of_vtable {
        // The slot's function starts with a short test and a tail jump. Look
        // at its first bytes only, up to the padding that follows a function.
        let from = pe.vtable_slot(d.vtable_rva(class)?, a.slot?)?;
        let target = want?;
        let head = (1..=64_usize)
            .rev()
            .find_map(|n| pe.bytes_at(from, n))
            .unwrap_or_default();
        let len = head
            .windows(2)
            .position(|w| w == [0xCC, 0xCC])
            .unwrap_or(head.len());
        let end = from.checked_add(u32::try_from(len).ok()?)?;
        return (!pe.jumps(from, end, target).is_empty()).then_some(target);
    }
    if let Some(text) = &a.literal {
        // Every use of the literal's address lies in the wanted function:
        // between its first byte and the padding that ends its code.
        let want = want?;
        let end = pe.padded_end(want, function_span());
        let refs = literals.refs(pe, text);
        let inside = refs.iter().all(|r| *r >= want && *r < end);
        return (!refs.is_empty() && inside).then_some(want);
    }
    if let Some(caller) = &a.called_by {
        // The caller is identified first; it must call the wanted function
        // directly before its code ends.
        let want = want?;
        if depth >= MAX_ANCHOR_DEPTH {
            return None;
        }
        let named = hex32(caller.rva.as_ref())?;
        let from = resolve_anchor(caller, Some(named), pe, d, literals, depth + 1)?;
        if from != named {
            return None;
        }
        let end = pe.padded_end(from, function_span());
        return (!pe.calls(from, end, want).is_empty()).then_some(want);
    }
    let f = d.function(a.function.as_ref()?)?;
    f.get("rva")?.as_u64().and_then(|v| u32::try_from(v).ok())
}

/// Win32 check 7: against the executable.
#[must_use]
pub fn check_win32_binary(l: &RecorderLayout, pe: &Pe, d: &Win32Data) -> Vec<Check> {
    const G: &str = "win32:binary";
    let mut out = Vec::new();
    let header_ok = pe.machine == pe::MACHINE_I386
        && l.image.as_ref().is_some_and(|i| {
            hex(&i.image_base) == Some(u64::from(pe.image_base))
                && i.time_date_stamp == u64::from(pe.time_date_stamp)
                && i.size_of_image == u64::from(pe.size_of_image)
        });
    check(
        &mut out,
        G,
        header_ok,
        format!(
            "PE header: machine {:#x}, image base {:#x}, time stamp {}, size of image {}",
            pe.machine, pe.image_base, pe.time_date_stamp, pe.size_of_image
        ),
    );
    // First and last instruction of a function symbol.
    let span = |name: &str| -> Option<(u32, u32)> {
        let s = l.symbols.get(name)?;
        Some((hex32(s.rva.as_ref())?, hex32(s.end_rva.as_ref())?))
    };
    for (name, s) in &l.symbols {
        let rva = hex32(s.rva.as_ref());
        let len = s.read.unwrap_or(1);
        let section = rva
            .and_then(|r| pe.section_of(r, len))
            .map(|x| x.name.as_str());
        check(
            &mut out,
            G,
            section.is_some() && section == s.section.as_deref(),
            format!(
                "symbol {name} at rva {:?}: {len} byte(s) in {section:?} (want {:?})",
                s.rva, s.section
            ),
        );
        if let Some(v) = s.file_value_f64 {
            let got = rva
                .and_then(|r| pe.bytes_at(r, 8))
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(f64::from_le_bytes);
            check(
                &mut out,
                G,
                got.is_some_and(|g| g.to_bits() == v.to_bits()),
                format!("symbol {name}: initial value {got:?} (want {v})"),
            );
        }
    }
    let mut literals = Literals::default();
    for e in &l.native_evidence {
        let rva = hex32(e.rva.as_ref());
        let needle = hex_bytes(&e.bytes).unwrap_or_default();
        let at = rva.and_then(|r| pe.bytes_at(r, needle.len()));
        let in_text = rva
            .and_then(|r| pe.section_of(r, needle.len() as u64))
            .is_some_and(|s| s.name == ".text");
        let mut ok = !needle.is_empty() && at == Some(needle.as_slice()) && in_text;
        let mut detail = format!("bytes {} at rva {:?}", e.bytes, e.rva);
        if let (Some(start), Some(r)) = (hex32(e.function_rva.as_ref()), rva) {
            // No function boundary (two-byte int3 padding) lies between the
            // function's first byte and the instruction.
            let end = pe.padded_end(start, function_span());
            ok = ok && r >= start && r < end;
            detail.push_str(&format!(
                ", inside the code that starts at {} and ends before {}",
                show_rva(Some(start)),
                show_rva(Some(end))
            ));
        }
        if let Some(a) = &e.anchor {
            let want = hex32(e.function_rva.as_ref());
            let found = resolve_anchor(a, want, pe, d, &mut literals, 0);
            ok = ok && found.is_some() && found == want;
            detail.push_str(&format!(
                ", function {} at {} (want {})",
                e.function,
                show_rva(found),
                show_rva(want)
            ));
        }
        if !e.near_literals.is_empty() {
            let mut missing = Vec::new();
            for text in &e.near_literals {
                if !rva.is_some_and(|r| literals.used_near(pe, text, r)) {
                    missing.push(text.as_str());
                }
            }
            ok = ok && missing.is_empty();
            detail.push_str(&format!(
                ", literals {:?} used next to it (missing: {missing:?})",
                e.near_literals
            ));
        }
        if let Some(callee) = &e.before_call_to {
            let first_call =
                span(&e.function)
                    .zip(span(callee))
                    .and_then(|((start, end), (target, _))| {
                        pe.calls(start, end, target).first().copied()
                    });
            ok = ok && first_call.is_some_and(|c| rva.is_some_and(|r| r < c));
            detail.push_str(&format!(
                ", first call to {callee} at {}",
                show_rva(first_call)
            ));
        }
        check(&mut out, G, ok, format!("{:?}: {detail}", e.what));
    }
    for c in &l.calls {
        let n = span(&c.caller)
            .zip(span(&c.callee))
            .map(|((start, end), (target, _))| pe.calls(start, end, target).len());
        check(
            &mut out,
            G,
            n == Some(c.count),
            format!("{:?}: {n:?} direct call(s) (want {})", c.what, c.count),
        );
    }
    out
}

/// A local copy of the Windows executable, if present: the
/// [`WIN32_EXECUTABLE_ENV`] variable, the git-ignored copy under the
/// repository root, or `ASAMU_ORIGINAL_DIR` (a Windows install root) joined
/// with the layout's `executable`.
#[must_use]
pub fn default_win32_executable(root: &Path, l: &RecorderLayout) -> Option<PathBuf> {
    let candidates = [
        std::env::var_os(WIN32_EXECUTABLE_ENV).map(PathBuf::from),
        Some(root.join(WIN32_LOCAL_EXECUTABLE)),
        std::env::var_os("ASAMU_ORIGINAL_DIR").map(|d| PathBuf::from(d).join(&l.executable)),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// Whether `path` starts like a PE image (`MZ`).
fn looks_like_pe(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0_u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == *b"MZ"
}

/// Runs every Windows check on the repository at `root`. The binary group
/// runs when `binary` (or a local copy, see [`default_win32_executable`])
/// exists. Returns the checks and the executable that was checked.
///
/// # Errors
/// Missing or malformed repository files, or an unreadable executable.
pub fn run_win32(
    root: &Path,
    mac: &RecorderLayout,
    sources: &[(String, String)],
    binary: Option<&Path>,
) -> Result<(Vec<Check>, Option<PathBuf>)> {
    let text = std::fs::read_to_string(root.join(WIN32_LAYOUT_PATH))
        .with_context(|| format!("reading {WIN32_LAYOUT_PATH}"))?;
    let l = RecorderLayout::from_json(&text).context("parsing the Windows recorder layout")?;
    let d = Win32Data::load(&root.join(WIN32_DATA_DIR))?;
    let mut checks = check_win32_schema(&l);
    checks.extend(check_win32_layout(&l, &d.native_layout));
    checks.extend(check_win32_same_reads(mac, &l));
    checks.extend(defaults_checks(
        &l,
        &root.join(DEFAULTS_DIR),
        "win32:defaults",
    ));
    checks.extend(check_win32_native_code(&l, &d.native_layout));
    checks.extend(python_checks(&l, sources, "win32:python").0);
    checks.extend(check_win32_data(&l, &d));
    let exe = binary
        .map(Path::to_path_buf)
        .or_else(|| default_win32_executable(root, &l));
    match &exe {
        Some(p) => {
            let data = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let pe = Pe::parse(data).with_context(|| format!("parsing {}", p.display()))?;
            checks.extend(check_win32_binary(&l, &pe, &d));
        }
        None => check(
            &mut checks,
            "win32:binary",
            true,
            format!(
                "skipped: no copy of the Windows executable ({WIN32_LOCAL_EXECUTABLE} or {WIN32_EXECUTABLE_ENV})"
            ),
        ),
    }
    Ok((checks, exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macho::synthetic;

    fn layout() -> RecorderLayout {
        RecorderLayout::from_json(
            r#"{
  "schema": "asamu-decomp/recorder-layout/v1", "id": "t", "build": "t", "game_build": "t",
  "executable": "x", "pointer_size": 8,
  "symbols": {
    "G": {"mangled": "_G", "kind": "data", "read": 8, "type": "f64", "file_value_f64": 0.5},
    "F": {"mangled": "__Z1Fv", "kind": "function"},
    "T": {"mangled": "__Z1Tv", "kind": "function"}
  },
  "structs": {"TArray": {"size": 16, "members": {"data": 0, "count": 8}, "evidence": "native_code: x"}},
  "fields": [
    {"class": "Engine.Actor", "name": "Location", "offset": "0x080", "kind": "Struct", "size": 12, "evidence": "native_layout"},
    {"class": "Engine.Actor", "name": "Location.Y", "offset": "0x084", "kind": "Float", "size": 4, "evidence": "layout_rule"},
    {"class": "Engine.Player", "name": "Actor", "offset": "0x068", "kind": "Object", "size": 8, "evidence": "native_code"},
    {"class": "asamu.GrappleGun", "name": "fMaxDistance", "offset": "0x41C", "kind": "Float", "size": 4, "evidence": "layout_rule"},
    {"class": "asamu.GrappleGun", "name": "bIsGrappling", "offset": "0x3D8", "kind": "Bool", "size": 4, "bit": 0, "evidence": "layout_rule"}
  ],
  "sentinels": [{"object": "gun", "class": "asamu.GrappleGun", "name": "fMaxDistance", "expected": 5000.0, "data_file": "GrappleGun.json"}],
  "native_evidence": [
    {"what": "actor", "function": "__Z1Fv", "bytes": "4989442468", "field": "Engine.Player.Actor", "struct": "TArray", "before_call_to": "__Z1Tv"}
  ],
  "calls": [{"what": "one call", "caller": "__Z1Fv", "callee": "__Z1Tv", "count": 1}]
}"#,
        )
        .unwrap()
    }

    fn native() -> serde_json::Value {
        serde_json::json!({"classes": {"Engine.Actor": {"fields": [
            ["0x080", "Location", "Struct", 12, null, 1],
            ["0x08C", "Rotation", "Struct", 12, null, 1]
        ]}}})
    }

    #[test]
    fn schema_native_layout_links_and_python() {
        let l = layout();
        let fails = |v: Vec<Check>| v.into_iter().filter(|c| !c.ok).collect::<Vec<_>>();
        assert!(fails(check_schema(&l)).is_empty());
        assert!(fails(check_native_layout(&l, &native())).is_empty());
        assert!(fails(check_native_code_links(&l)).is_empty());

        let mut bad = l.clone();
        bad.fields[0].offset = "0x084".into();
        bad.fields[1].offset = "0x08C".into();
        bad.fields[2].evidence = "native_layout".into();
        assert_eq!(fails(check_native_layout(&bad, &native())).len(), 3);
        bad.fields[4].bit = None;
        bad.fields.push(bad.fields[3].clone());
        assert_eq!(fails(check_schema(&bad)).len(), 2);
        let mut unlinked = l.clone();
        unlinked.native_evidence[0].bytes = "90".into();
        assert_eq!(fails(check_native_code_links(&unlinked)).len(), 1);

        let src = r#"
            a = L.off("Engine.Actor", "Location")
            b = L.bit( "asamu.GrappleGun" , "bIsGrappling" )
            c = L.st("TArray", "count")
            d = L.sym("G")["mangled"]
            e = L.off(s["class"], s["name"])
            f = L.off("Engine.Actor", "Nope")
            g = layout.sym("Missing")
            h = L.bit("Engine.Actor", "Location")
        "#;
        let lk = python_lookups(src);
        assert_eq!(lk.len(), 7, "{lk:?}");
        let (checks, n) = check_python(&l, &[("x.py".into(), src.into())]);
        assert_eq!(n, 7);
        let failed: Vec<_> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.what.clone())
            .collect();
        assert_eq!(failed.len(), 3, "{failed:?}");
    }

    #[test]
    fn binary_checks_on_a_synthetic_image() {
        let l = layout();
        // F: movq %rax,0x68(%r12); call T; ret.  T: ret.
        let mut code = vec![0x49, 0x89, 0x44, 0x24, 0x68, 0xE8, 1, 0, 0, 0, 0xC3];
        code.push(0xC3);
        let data = 0.5_f64.to_le_bytes().to_vec();
        let img = synthetic(
            0x1000,
            &code,
            0x8000,
            &[data.as_slice(), &[0_u8; 8]].concat(),
            &[
                ("__Z1Fv", 0x1000, 1),
                ("__Z1Tv", 0x100B, 1),
                ("_G", 0x8000, 2),
                ("_H", 0x8008, 2),
            ],
        );
        let m = MachO::parse(img).unwrap();
        let checks = check_binary(&l, &m);
        let failed: Vec<_> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.what.clone())
            .collect();
        assert!(failed.is_empty(), "{failed:?}");

        let mut wrong = l.clone();
        wrong.symbols.get_mut("G").unwrap().file_value_f64 = Some(0.25);
        wrong.symbols.get_mut("G").unwrap().read = Some(16);
        wrong.native_evidence[0].bytes = "4989442469".into();
        wrong.calls[0].count = 2;
        wrong.symbols.insert(
            "X".into(),
            LayoutSymbol {
                mangled: "_X".into(),
                kind: "data".into(),
                read: Some(4),
                ..LayoutSymbol::default()
            },
        );
        let failed = check_binary(&wrong, &m)
            .into_iter()
            .filter(|c| !c.ok)
            .count();
        assert_eq!(failed, 5);
    }

    #[test]
    fn defaults_and_type_rules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("GrappleGun.json"),
            r#"{"class": "asamu.GrappleGun", "properties": [
                {"name": "fMaxDistance", "declared_in": "asamu.GrappleGun", "type": "float", "value": 5000.0},
                {"name": "bIsGrappling", "declared_in": "asamu.GrappleGun", "type": "bool", "value": false}
            ]}"#,
        )
        .unwrap();
        let l = layout();
        let checks = check_defaults(&l, dir.path());
        assert!(checks.iter().all(|c| c.ok), "{checks:?}");
        let mut wrong = l.clone();
        wrong.sentinels[0].expected = 5001.0;
        wrong.fields[4].kind = "Int".into();
        wrong.fields[4].bit = None;
        assert_eq!(
            check_defaults(&wrong, dir.path())
                .iter()
                .filter(|c| !c.ok)
                .count(),
            2
        );
        assert!(type_matches("Object", 8, "GrappleGunHitLocActor"));
        assert!(!type_matches("Object", 8, "float"));
        assert!(type_matches("Struct", 12, "Vector"));
        assert!(!type_matches("Struct", 16, "Vector"));
        assert!(type_matches("Array", 16, "array<name>"));
    }

    // ------------------------------------------------------------- Win32

    const BASE: u32 = 0x0040_0000;

    /// A small PE image:
    /// - `F` at 0x1000: load `[esi+0x4B0]`, load `[G]`, call `T`, `ret 4`;
    /// - `T` at 0x1020: load `[esi+0x28]`, `ret 8`;
    /// - `V` at 0x1030 (vtable slot 0): load `[edi+0x34]`, `ret`;
    /// - `J` at 0x1040 (vtable slot 1): tail jump to `V`;
    /// - `X` at 0x1050 (name table entry `AFooexecBar`): `lea eax,[esi+0x2C]`;
    /// - `L` at 0x1060: pushes the UTF-16 literal `Mem` (stored at 0x2030,
    ///   and again as the tail of `XMem` at 0x2040), loads `[esi+0x48]`.
    fn win_image(time_date_stamp: u32) -> Vec<u8> {
        let mut code = vec![0xCC_u8; 0x70];
        let put = |code: &mut Vec<u8>, at: usize, bytes: &[u8]| {
            code[at..at + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut code, 0x00, &[0x8B, 0x8E, 0xB0, 0x04, 0x00, 0x00]);
        let mut load_g = vec![0x8B, 0x0D];
        load_g.extend_from_slice(&(BASE + 0x3000).to_le_bytes());
        put(&mut code, 0x06, &load_g);
        // call T: next instruction at 0x1011, target 0x1020.
        put(&mut code, 0x0C, &[0xE8, 0x0F, 0x00, 0x00, 0x00]);
        put(&mut code, 0x11, &[0xC2, 0x04, 0x00]);
        put(&mut code, 0x20, &[0x8B, 0x46, 0x28, 0xC2, 0x08, 0x00]);
        put(&mut code, 0x30, &[0x8B, 0x47, 0x34, 0xC3]);
        // jmp V: next instruction at 0x1045, target 0x1030.
        put(&mut code, 0x40, &[0xE9, 0xEB, 0xFF, 0xFF, 0xFF]);
        put(&mut code, 0x50, &[0x8D, 0x46, 0x2C, 0xC3]);
        let mut push_literal = vec![0x68];
        push_literal.extend_from_slice(&(BASE + 0x2030).to_le_bytes());
        put(&mut code, 0x60, &push_literal);
        put(&mut code, 0x65, &[0x8B, 0x46, 0x48, 0xC3]);
        let mut rdata = vec![0_u8; 0x50];
        let wide = |t: &str| -> Vec<u8> { t.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        rdata[0x30..0x36].copy_from_slice(&wide("Mem"));
        rdata[0x40..0x48].copy_from_slice(&wide("XMem"));
        rdata[1..12].copy_from_slice(b"AFooexecBar");
        rdata[0x10..0x14].copy_from_slice(&(BASE + 0x1030).to_le_bytes());
        rdata[0x14..0x18].copy_from_slice(&(BASE + 0x1040).to_le_bytes());
        rdata[0x20..0x24].copy_from_slice(&(BASE + 0x2001).to_le_bytes());
        rdata[0x24..0x28].copy_from_slice(&(BASE + 0x1050).to_le_bytes());
        pe::synthetic(
            BASE,
            time_date_stamp,
            0x1000,
            &code,
            0x2000,
            &rdata,
            0x3000,
            &0.5_f64.to_le_bytes(),
            0x100,
        )
    }

    fn win_layout() -> RecorderLayout {
        RecorderLayout::from_json(
            r#"{
  "schema": "asamu-decomp/recorder-layout/v1", "id": "w", "build": "w", "game_build": "t",
  "executable": "Bin/x.exe", "pointer_size": 4,
  "image": {"module": "x.exe", "image_base": "0x00400000", "time_date_stamp": 1234, "size_of_image": 12552, "address_rule": "base + rva"},
  "symbols": {
    "G": {"mangled": "G", "rva": "0x00003000", "kind": "data", "read": 8, "type": "f64", "section": ".data", "file_value_f64": 0.5},
    "F": {"mangled": "F", "rva": "0x00001000", "end_rva": "0x00001011", "kind": "function", "section": ".text", "abi": "cc"},
    "T": {"mangled": "T", "rva": "0x00001020", "end_rva": "0x00001023", "kind": "function", "section": ".text", "abi": "cc"}
  },
  "structs": {
    "TArray": {"size": 12, "members": {"data": 0, "count": 4, "max": 8}, "evidence": "x"},
    "FName": {"size": 8, "members": {"index": 0, "number": 4}, "evidence": "x"},
    "FString": {"size": 12, "members": {"data": 0, "count": 4}, "char_size": 2, "evidence": "x"},
    "KeyBind": {"size": 24, "members": {"Name": 0, "Command": 8}, "evidence": "x"}
  },
  "fields": [
    {"class": "Core.Object", "name": "Outer", "offset": "0x028", "kind": "Object", "size": 4, "evidence": "native_code"},
    {"class": "Core.Object", "name": "Name", "offset": "0x02C", "kind": "Name", "size": 8, "evidence": "native_code"},
    {"class": "Core.Object", "name": "Class", "offset": "0x034", "kind": "Class", "size": 4, "evidence": "native_code"},
    {"class": "Core.Object", "name": "Cache.B", "offset": "0x044", "kind": "Float", "size": 4, "evidence": "layout_rule"},
    {"class": "Core.Object", "name": "Keys", "offset": "0x048", "kind": "Array", "size": 12, "evidence": "native_layout"},
    {"class": "Core.Object", "name": "bFlag", "offset": "0x054", "kind": "Bool", "size": 4, "bit": 3, "evidence": "native_layout"}
  ],
  "native_evidence": [
    {"what": "outer", "field": "Core.Object.Outer", "offset": "0x028", "function": "T", "anchor": {"function": "T"}, "function_rva": "0x00001020", "rva": "0x00001020", "bytes": "8b4628"},
    {"what": "class", "field": "Core.Object.Class", "offset": "0x034", "function": "V", "anchor": {"vtable": "UFoo", "slot": 0}, "function_rva": "0x00001030", "rva": "0x00001030", "bytes": "8b4734"},
    {"what": "name", "field": "Core.Object.Name", "offset": "0x02C", "function": "X", "anchor": {"exec": "AFooexecBar"}, "function_rva": "0x00001050", "rva": "0x00001050", "bytes": "8d462c"},
    {"what": "tail", "function": "V", "anchor": {"tail_of_vtable": "UFoo", "slot": 1}, "function_rva": "0x00001030", "rva": "0x00001033", "bytes": "c3"},
    {"what": "global", "symbol": "G", "function": "F", "anchor": {"function": "F"}, "function_rva": "0x00001000", "rva": "0x00001006", "bytes": "8b0d00304000", "before_call_to": "T"},
    {"what": "scan", "function": "T", "rva": "0x00001023", "bytes": "c20800"},
    {"what": "keys", "field": "Core.Object.Keys", "offset": "0x048", "function": "L", "anchor": {"literal": "Mem"}, "function_rva": "0x00001060", "rva": "0x00001065", "bytes": "8b4648", "near_literals": ["Mem"]},
    {"what": "callee", "function": "T", "anchor": {"called_by": {"function": "F", "rva": "0x00001000"}}, "function_rva": "0x00001020", "rva": "0x00001023", "bytes": "c20800"}
  ],
  "calls": [{"what": "one call", "caller": "F", "callee": "T", "count": 1}]
}"#,
        )
        .unwrap()
    }

    fn win_data() -> Win32Data {
        Win32Data {
            native_layout: serde_json::json!({
                "game_build": "t",
                "classes": {"Core.Object": {"fields": [
                    ["0x028", "Outer", "Object", 4, null, 1],
                    ["0x02C", "Name", "Name", 8, null, 1],
                    ["0x034", "Class", "Class", 4, null, 1],
                    ["0x040", "Cache", "Struct", 8, null, 1],
                    ["0x048", "Keys", "Array", 12, null, 1],
                    ["0x054", "bFlag", "Bool", 4, 3, 1]
                ]}},
                "structs": {
                    "Core.Object.Inner": {"size": 8, "fields": [
                        ["0x000", "A", "Float", 4, null, 1],
                        ["0x004", "B", "Float", 4, null, 1]
                    ]},
                    "Engine.Input.KeyBind": {"size": 24, "fields": [
                        ["0x000", "Name", "Name", 8, null, 1],
                        ["0x008", "Command", "Str", 12, null, 1]
                    ]}
                },
                "member_paths": {"Core.Object.Cache.B": {"offset": "0x044", "steps": [
                    ["Core.Object", "Cache", "0x040"],
                    ["Core.Object.Inner", "B", "0x004"]
                ]}},
                "validation": {"class_sizes": {
                    "different": 0, "equal": 5, "native_script_classes_compared": 5
                }}
            }),
            globals: serde_json::json!({"game_build": "t", "globals": [
                {"name": "G", "rva": 0x3000, "size": 8, "type": "f64", "section": ".data", "file_value_f64": 0.5}
            ]}),
            functions: serde_json::json!({"game_build": "t", "functions": [
                {"name": "F", "rva": 0x1000, "end_rva": 0x1011, "calling_convention": "cc"},
                {"name": "T", "rva": 0x1020, "end_rva": 0x1023, "calling_convention": "cc"}
            ]}),
            image: serde_json::json!({
                "game_build": "t", "image_base": "0x00400000", "time_date_stamp": 1234,
                "size_of_image": 12552, "executable": "Bin/x.exe"
            }),
            class_sizes: serde_json::json!({
                "columns": ["cpp_name", "vtable_rva"],
                "classes": [["UFoo", 0x2010]]
            }),
        }
    }

    fn failed(checks: &[Check]) -> Vec<String> {
        checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| format!("[{}] {}", c.group, c.what))
            .collect()
    }

    #[test]
    fn pe_reader_reads_sections_tables_and_calls() {
        let img = win_image(1234);
        let p = Pe::parse(img.clone()).unwrap();
        assert_eq!(
            (p.machine, p.image_base, p.time_date_stamp),
            (0x14C, BASE, 1234)
        );
        assert_eq!(p.size_of_image, 12552);
        assert_eq!(p.sections.len(), 3);
        assert_eq!(p.section(".rdata").map(|s| s.rva), Some(0x2000));
        assert_eq!(
            p.section_of(0x1000, 4).map(|s| s.name.as_str()),
            Some(".text")
        );
        // Zero-fill bytes belong to the section but are not in the file.
        assert_eq!(
            p.section_of(0x3008, 8).map(|s| s.name.as_str()),
            Some(".data")
        );
        assert!(p.bytes_at(0x3008, 8).is_none());
        assert!(p.section_of(0x3100, 16).is_none());
        assert_eq!(p.bytes_at(0x1020, 3), Some([0x8B, 0x46, 0x28].as_slice()));
        assert_eq!(p.u32_at(0x2010), Some(BASE + 0x1030));
        assert_eq!(p.rva_of(BASE + 0x1030), Some(0x1030));
        assert_eq!(p.rva_of(0x10), None);
        assert_eq!(p.vtable_slot(0x2010, 0), Some(0x1030));
        assert_eq!(p.vtable_slot(0x2010, 1), Some(0x1040));
        // Slot 2 holds zero, not an address in .text.
        assert_eq!(p.vtable_slot(0x2010, 2), None);
        assert_eq!(p.exec_function("AFooexecBar"), Some(0x1050));
        assert_eq!(p.exec_function("AFooexecBa"), None);
        assert_eq!(p.exec_function("FooexecBar"), None);
        assert_eq!(p.calls(0x1000, 0x1014, 0x1020), vec![0x100C]);
        assert!(p.calls(0x1000, 0x1014, 0x1030).is_empty());
        assert_eq!(p.jumps(0x1040, 0x1050, 0x1030), vec![0x1040]);
        assert!(p.calls(0x1014, 0x1000, 0x1020).is_empty());
        // A literal is found where it starts, not inside a longer one.
        assert_eq!(p.utf16_literals("Mem"), vec![0x2030]);
        assert_eq!(p.utf16_literals("XMem"), vec![0x2040]);
        assert!(p.utf16_literals("Me").is_empty());
        assert!(p.utf16_literals("").is_empty());
        assert_eq!(p.text_refs(0x2030), vec![0x1061]);
        assert_eq!(p.address_refs(0x1060, 0x1070, 0x2030), vec![0x1061]);
        assert!(p.address_refs(0x1000, 0x1060, 0x2030).is_empty());
        assert!(p.address_refs(0x1070, 0x1060, 0x2030).is_empty());
        assert!(p.text_refs(0x2040).is_empty());
        // F ends where its int3 padding begins.
        assert_eq!(p.padded_end(0x1000, 0x40), 0x1014);
        assert_eq!(p.padded_end(0x1000, 0x10), 0x1010);
        assert_eq!(p.padded_end(0x9000, 0x10), 0x9000);

        // Hostile input: every truncation is an error or a smaller image,
        // never a panic; lookups on what parses stay in bounds.
        for len in 0..img.len() {
            if let Ok(p) = Pe::parse(img[..len].to_vec()) {
                let _ = p.exec_function("AFooexecBar");
                let _ = p.vtable_slot(0x2010, 1);
                let _ = p.calls(0x1000, 0x1014, 0x1020);
                let _ = p.utf16_literals("Mem");
                let _ = p.text_refs(0x2030);
                let _ = p.padded_end(0x1000, 0x40);
            }
        }
        assert!(Pe::parse(b"ZM".to_vec()).is_err());
        let mut plus = img.clone();
        // Optional header magic of PE32+.
        let opt = 0x40 + 4 + 20;
        plus[opt..opt + 2].copy_from_slice(&0x020B_u16.to_le_bytes());
        assert!(Pe::parse(plus).is_err());
        let mut far = img;
        // First section's raw offset far past the end of the file.
        let table = opt + 224;
        far[table + 20..table + 24].copy_from_slice(&0x7FFF_FFF0_u32.to_le_bytes());
        assert!(Pe::parse(far).is_err());
    }

    #[test]
    fn win32_checks_pass_on_consistent_data_and_catch_each_error() {
        let l = win_layout();
        let d = win_data();
        let p = Pe::parse(win_image(1234)).unwrap();
        assert_eq!(failed(&check_win32_schema(&l)), Vec::<String>::new());
        assert_eq!(
            failed(&check_win32_layout(&l, &d.native_layout)),
            Vec::<String>::new()
        );
        assert_eq!(
            failed(&check_win32_same_reads(&l, &l)),
            Vec::<String>::new()
        );
        assert_eq!(
            failed(&check_win32_native_code(&l, &d.native_layout)),
            Vec::<String>::new()
        );
        assert_eq!(failed(&check_win32_data(&l, &d)), Vec::<String>::new());
        let binary = check_win32_binary(&l, &p, &d);
        assert_eq!(failed(&binary), Vec::<String>::new());
        // Header, 3 symbols, 1 initial value, 8 evidence entries, 1 call count.
        assert_eq!(binary.len(), 14);

        // Schema: pointer size, a symbol without an RVA, an instruction
        // before its function, an anchor without a slot.
        let mut bad = l.clone();
        bad.pointer_size = 8;
        bad.symbols.get_mut("G").unwrap().rva = None;
        bad.native_evidence[0].rva = Some("0x00001010".into());
        bad.native_evidence[1].anchor.as_mut().unwrap().slot = None;
        assert_eq!(failed(&check_win32_schema(&bad)).len(), 4);

        // Layout: a wrong offset, a wrong bit, a dotted field that is not the
        // sum of its path, a pointer of the wrong size, a wrong KeyBind.
        let mut bad = l.clone();
        bad.fields[0].offset = "0x02C".into();
        bad.fields[5].bit = Some(4);
        bad.fields[3].offset = "0x048".into();
        bad.fields[2].size = 8;
        bad.structs.get_mut("KeyBind").unwrap()["size"] = serde_json::json!(32);
        let f = failed(&check_win32_layout(&bad, &d.native_layout));
        // Class: row mismatch and pointer size are two failures.
        assert_eq!(f.len(), 6, "{f:?}");
        let mut native = d.native_layout.clone();
        native["member_paths"]["Core.Object.Cache.B"]["steps"][1][1] = serde_json::json!("A");
        assert_eq!(failed(&check_win32_layout(&l, &native)).len(), 1);

        // Mac comparison: a missing field, a different sentinel list.
        let mut other = l.clone();
        other.fields.pop();
        other.sentinels.push(Sentinel {
            object: "pawn".into(),
            class: "Core.Object".into(),
            name: "Outer".into(),
            expected: 1.0,
            data_file: "x.json".into(),
        });
        assert_eq!(failed(&check_win32_same_reads(&other, &l)).len(), 2);

        // Evidence: an offset that is not the rule offset, bytes without the
        // offset, bytes without the symbol's address, a symbol without evidence.
        let mut bad = l.clone();
        bad.native_evidence[0].offset = Some("0x02C".into());
        bad.native_evidence[1].bytes = "8b4730".into();
        bad.native_evidence[4].bytes = "8b0d00304100".into();
        let f = failed(&check_win32_native_code(&bad, &d.native_layout));
        // Outer fails its rule comparison; Class fails that and its field link.
        assert_eq!(f.len(), 4, "{f:?}");
        let mut bad = l.clone();
        bad.native_evidence.remove(4);
        assert_eq!(
            failed(&check_win32_native_code(&bad, &d.native_layout)).len(),
            1
        );

        // Data files: a moved global, an unknown anchor function, another
        // build's image, rules that do not reproduce the class sizes.
        let mut other = d.clone();
        other.globals["globals"][0]["rva"] = serde_json::json!(0x3008);
        other.functions["functions"][1]["name"] = serde_json::json!("U");
        other.image["time_date_stamp"] = serde_json::json!(1235);
        other.native_layout["validation"]["class_sizes"]["different"] = serde_json::json!(1);
        let f = failed(&check_win32_data(&l, &other));
        // G; symbol T; the two anchors that name T... one names T, one names F.
        assert_eq!(f.len(), 5, "{f:?}");

        // Binary: another build, wrong bytes, a wrong vtable slot, an unknown
        // exec name, a slot that does not tail-jump there, a wrong call count.
        let other = Pe::parse(win_image(999)).unwrap();
        assert_eq!(failed(&check_win32_binary(&l, &other, &d)).len(), 1);
        let mut bad = l.clone();
        bad.native_evidence[0].bytes = "8b4629".into();
        bad.native_evidence[1].anchor.as_mut().unwrap().slot = Some(1);
        bad.native_evidence[2].anchor.as_mut().unwrap().exec = Some("AFooexecBaz".into());
        bad.native_evidence[3].anchor.as_mut().unwrap().slot = Some(0);
        bad.calls[0].count = 2;
        bad.symbols.get_mut("G").unwrap().file_value_f64 = Some(0.25);
        bad.symbols.get_mut("G").unwrap().section = Some(".rdata".into());
        let f = failed(&check_win32_binary(&bad, &p, &d));
        assert_eq!(f.len(), 7, "{f:?}");
        // An instruction that must precede the call but follows it.
        let mut bad = l.clone();
        bad.native_evidence[4].rva = Some("0x00001011".into());
        bad.native_evidence[4].bytes = "c20400".into();
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);

        // An instruction beyond the padding that ends its function.
        let mut bad = l.clone();
        bad.native_evidence[0].rva = Some("0x00001030".into());
        bad.native_evidence[0].bytes = "8b4734".into();
        bad.native_evidence[0].field = Some("Core.Object.Class".into());
        bad.native_evidence[0].offset = Some("0x034".into());
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);

        // Literal and caller anchors. A literal the image does not hold; a
        // literal that another function uses; a member name that is not
        // used next to the instruction; a caller that does not call the
        // function; a caller that is not where the layout says.
        let mut bad = l.clone();
        bad.native_evidence[6].anchor.as_mut().unwrap().literal = Some("Me".into());
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);
        let mut bad = l.clone();
        // T is not the function that pushes the literal.
        bad.native_evidence[6].function_rva = Some("0x00001020".into());
        bad.native_evidence[6].rva = Some("0x00001023".into());
        bad.native_evidence[6].bytes = "c20800".into();
        bad.native_evidence[6].field = None;
        bad.native_evidence[6].offset = None;
        bad.native_evidence[6].near_literals.clear();
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);
        let mut bad = l.clone();
        bad.native_evidence[6].near_literals = vec!["XMem".into()];
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);
        let mut bad = l.clone();
        let caller = bad.native_evidence[7]
            .anchor
            .as_mut()
            .unwrap()
            .called_by
            .as_mut()
            .unwrap();
        caller.function = None;
        caller.vtable = Some("UFoo".into());
        caller.slot = Some(0);
        caller.rva = Some("0x00001030".into());
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);
        let mut bad = l.clone();
        bad.native_evidence[7]
            .anchor
            .as_mut()
            .unwrap()
            .called_by
            .as_mut()
            .unwrap()
            .rva = Some("0x00001004".into());
        assert_eq!(failed(&check_win32_binary(&bad, &p, &d)).len(), 1);
        // A caller whose own caller is nested too deep is refused, and the
        // schema asks a nested anchor for its RVA and a top-level one for none.
        let mut bad = l.clone();
        bad.native_evidence[7]
            .anchor
            .as_mut()
            .unwrap()
            .called_by
            .as_mut()
            .unwrap()
            .rva = None;
        bad.native_evidence[6].anchor.as_mut().unwrap().rva = Some("0x00001060".into());
        bad.native_evidence[0].near_literals = vec![String::new()];
        assert_eq!(failed(&check_win32_schema(&bad)).len(), 3);
        let mut deep = EvidenceAnchor {
            function: Some("F".into()),
            rva: Some("0x00001000".into()),
            ..EvidenceAnchor::default()
        };
        for _ in 0..=MAX_ANCHOR_DEPTH {
            deep = EvidenceAnchor {
                called_by: Some(Box::new(deep)),
                rva: Some("0x00001020".into()),
                ..EvidenceAnchor::default()
            };
        }
        deep.rva = None;
        assert!(!deep.well_formed(false, 0));
        let mut literals = Literals::default();
        assert_eq!(
            resolve_anchor(&deep, Some(0x1020), &p, &d, &mut literals, 0),
            None
        );
        // The data check follows a caller anchor to what it names.
        let mut bad = l.clone();
        bad.native_evidence[7]
            .anchor
            .as_mut()
            .unwrap()
            .called_by
            .as_mut()
            .unwrap()
            .function = Some("U".into());
        assert_eq!(failed(&check_win32_data(&bad, &d)).len(), 1);
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The committed Windows layout agrees with the committed data and the
    /// Mac layout. Needs no game files; the binary group runs only when a
    /// local copy of the Windows executable exists.
    #[test]
    fn windows_layout_matches_repository_data() {
        let root = repo_root();
        let mac =
            RecorderLayout::from_json(&std::fs::read_to_string(root.join(LAYOUT_PATH)).unwrap())
                .unwrap();
        let mut sources = Vec::new();
        for p in PYTHON_SOURCES {
            sources.push((
                (*p).to_owned(),
                std::fs::read_to_string(root.join(p)).unwrap(),
            ));
        }
        let (checks, exe) = run_win32(&root, &mac, &sources, None).unwrap();
        assert_eq!(failed(&checks), Vec::<String>::new());
        for group in [
            "win32:schema",
            "win32:layout",
            "win32:mac",
            "win32:defaults",
            "win32:native_code",
            "win32:python",
            "win32:data",
            "win32:binary",
        ] {
            assert!(
                checks.iter().any(|c| c.group == group),
                "no {group} checks ran"
            );
        }
        let win = RecorderLayout::from_json(
            &std::fs::read_to_string(root.join(WIN32_LAYOUT_PATH)).unwrap(),
        )
        .unwrap();
        // Every field of a native class is compared with a rule-derived row,
        // and most of what the recorder reads is shown by an instruction.
        let shown = win
            .fields
            .iter()
            .filter(|f| f.evidence == "native_code")
            .count();
        assert!(shown >= 25, "only {shown} fields shown by native code");
        assert!(win.native_evidence.len() >= 80);
        let binary = checks.iter().filter(|c| c.group == "win32:binary").count();
        match exe {
            Some(_) => {
                let expected = 1
                    + win.symbols.len()
                    + win
                        .symbols
                        .values()
                        .filter(|s| s.file_value_f64.is_some())
                        .count()
                    + win.native_evidence.len()
                    + win.calls.len();
                assert_eq!(binary, expected);
            }
            None => {
                eprintln!(
                    "skipped: no local copy of the Windows executable ({WIN32_LOCAL_EXECUTABLE} or {WIN32_EXECUTABLE_ENV})"
                );
                assert_eq!(binary, 1);
            }
        }
    }

    #[test]
    fn a_pe_binary_argument_goes_to_the_windows_checks() {
        let dir = tempfile::tempdir().unwrap();
        let pe_path = dir.path().join("x.exe");
        std::fs::write(&pe_path, win_image(1)).unwrap();
        let other = dir.path().join("x.bin");
        std::fs::write(&other, [0xCF, 0xFA, 0xED, 0xFE]).unwrap();
        assert!(looks_like_pe(&pe_path));
        assert!(!looks_like_pe(&other));
        assert!(!looks_like_pe(&dir.path().join("missing")));
        // A copy named by the environment or kept under research/local is
        // found; an empty checkout has none (unless the variable is set).
        let l = win_layout();
        if std::env::var_os(WIN32_EXECUTABLE_ENV).is_none()
            && std::env::var_os("ASAMU_ORIGINAL_DIR").is_none()
        {
            assert_eq!(default_win32_executable(dir.path(), &l), None);
            let local = dir.path().join(WIN32_LOCAL_EXECUTABLE);
            std::fs::create_dir_all(local.parent().unwrap()).unwrap();
            std::fs::write(&local, win_image(1)).unwrap();
            assert_eq!(default_win32_executable(dir.path(), &l), Some(local));
        }
    }
}
