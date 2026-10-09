//! Static self-checks of the trace recorder's layout file.
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutSymbol {
    /// Mach-O name (leading underscore included).
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

/// Instruction bytes that show an offset in native code.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEvidence {
    /// What the bytes show.
    pub what: String,
    /// Function (Mach-O name) containing them.
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
            &mut out,
            "schema",
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
    for (name, s) in &l.structs {
        let ok = s.get("size").and_then(serde_json::Value::as_u64).is_some()
            && s.get("members")
                .and_then(serde_json::Value::as_object)
                .is_some();
        check(&mut out, "schema", ok, format!("struct {name}"));
    }
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
    let mut out = Vec::new();
    for f in l.fields.iter().filter(|f| f.class.starts_with("asamu.")) {
        let id = format!("{}.{}", f.class, f.name);
        let short = f.class.trim_start_matches("asamu.");
        let Some(data) = data_file(defaults_dir, short) else {
            check(
                &mut out,
                "defaults",
                false,
                format!("{id}: no {short}.json"),
            );
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
            "defaults",
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
            "defaults",
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
    let mut out = Vec::new();
    for e in &l.native_evidence {
        check(
            &mut out,
            "native_code",
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
            "native_code",
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
                "native_code",
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
                "python",
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
    /// Every check, in group order.
    pub checks: Vec<Check>,
    /// Literal lookups found in the Python sources.
    pub python_lookups: usize,
    /// The executable that was checked, if any.
    pub binary: Option<PathBuf>,
}

impl LayoutReport {
    /// The failed checks.
    #[must_use]
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.ok).collect()
    }
}

/// Runs every check on the repository at `root`; the binary checks run when
/// `binary` (or the default install) exists.
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
    let mut checks = check_schema(&l);
    checks.extend(check_native_layout(&l, &native));
    checks.extend(check_defaults(&l, &root.join(DEFAULTS_DIR)));
    checks.extend(check_native_code_links(&l));
    let (py, python_lookups) = check_python(&l, &sources);
    checks.extend(py);
    let exe = binary
        .map(Path::to_path_buf)
        .or_else(|| default_executable(&l));
    if let Some(p) = &exe {
        let data = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        let m = MachO::parse(data).with_context(|| format!("parsing {}", p.display()))?;
        checks.extend(check_binary(&l, &m));
    }
    Ok(LayoutReport {
        checks,
        python_lookups,
        binary: exe,
    })
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
                ty: None,
                role: None,
                file_value_f64: None,
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
}
