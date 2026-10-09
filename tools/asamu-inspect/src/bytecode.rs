//! Bytecode subcommands: `disasm`, `calls` and `bytecode-coverage`.
//!
//! - `disasm` prints the decoded token tree of one function, state or class.
//!   That is the original game's logic: **local use only**. Never commit,
//!   paste or paraphrase its output.
//! - `calls` prints, per function/state of a class, which functions and
//!   natives are called and which numeric/name constants appear. Those are
//!   identifiers and numbers, suitable for evidence notes.
//! - `bytecode-coverage` decodes every bytecode-carrying export of the script
//!   packages in a cooked folder and prints the structural checks
//!   (`docs/reverse-engineering/BYTECODE.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::PackageIndex;
use asamu_ue3::bytecode::{
    self, ArityCheck, BytecodeCoverage, Expr, ExprKind, Layout, NativeTable, Script,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::script::ScriptKind;
use asamu_ue3::types::FName;
use clap::Subcommand;
use serde::Serialize;

use crate::print_json;

/// Bytecode subcommands (flattened into the top-level command list).
#[derive(Subcommand)]
pub enum BytecodeCmd {
    /// Disassemble the bytecode of a function, state or class into a token
    /// tree. LOCAL USE ONLY: the output is the original game's logic and must
    /// never be committed or quoted.
    Disasm {
        /// Package file that contains the object.
        file: PathBuf,
        /// Object path (e.g. asamu.ASAMUPawn.Tick, Pawn.Dying) or `#N` for export N.
        object: String,
    },
    /// Per function/state of a class: functions and natives called, and the
    /// float/int/name constants used (identifiers and numbers only).
    Calls {
        /// Package file that contains the class.
        file: PathBuf,
        /// Class path or bare class name.
        class: String,
    },
    /// Decode every bytecode-carrying export of the script packages (.u and
    /// .upk) in a cooked folder and report the structural checks.
    BytecodeCoverage {
        /// Cooked folder (e.g. .../CookedMac).
        dir: PathBuf,
    },
}

/// Run a bytecode subcommand.
pub fn run(cmd: &BytecodeCmd, json: bool) -> Result<()> {
    match cmd {
        BytecodeCmd::Disasm { file, object } => cmd_disasm(file, object, json),
        BytecodeCmd::Calls { file, class } => cmd_calls(file, class, json),
        BytecodeCmd::BytecodeCoverage { dir } => cmd_coverage(dir, json),
    }
}

// ---------------------------------------------------------------- helpers

fn open_set(file: &Path) -> Result<(PackageSet, Arc<LoadedPackage>)> {
    PackageSet::for_file(file).with_context(|| format!("opening {}", file.display()))
}

fn resolve(lp: &LoadedPackage, path: &str) -> Result<usize> {
    if let Some(n) = path.strip_prefix('#') {
        let i: usize = n
            .parse()
            .with_context(|| format!("bad export index {path:?}"))?;
        if i >= lp.package.exports.len() {
            bail!(
                "export index {i} out of range ({} exports in {})",
                lp.package.exports.len(),
                lp.name
            );
        }
        return Ok(i);
    }
    lp.find(path)
        .with_context(|| format!("no object {path:?} in {}", lp.name))
}

/// Script packages (`.u`, `.upk`) directly inside `dir`, sorted.
fn script_package_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|x| x.to_string_lossy().to_ascii_lowercase())
                    .is_some_and(|x| matches!(x.as_str(), "u" | "upk"))
        })
        .collect();
    v.sort();
    v
}

/// Native index table from every `.u` package next to `file` (natives are
/// declared in the native script packages; indices are global).
fn native_table(set: &PackageSet, file: &Path) -> NativeTable {
    let mut t = NativeTable::default();
    let dir = file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    for f in script_package_files(&dir) {
        let is_u = f
            .extension()
            .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("u"));
        if !is_u {
            continue;
        }
        if let Ok(lp) = set.open_file(&f) {
            t.add_package(&lp.package, &lp.name);
        }
    }
    // The file itself (e.g. Startup.upk holds the asamu natives' functions).
    if let Ok(lp) = set.open_file(file) {
        t.add_package(&lp.package, &lp.name);
    }
    t
}

struct Names<'a> {
    lp: &'a LoadedPackage,
    natives: &'a NativeTable,
}

impl Names<'_> {
    fn obj(&self, idx: PackageIndex) -> String {
        if idx.is_null() {
            return "None".to_owned();
        }
        self.lp
            .ref_path(idx)
            .ok()
            .flatten()
            .unwrap_or_else(|| format!("<bad ref {}>", idx.0))
    }

    fn short(&self, idx: PackageIndex) -> String {
        self.lp
            .package
            .object_name(idx)
            .unwrap_or_else(|_| format!("<bad ref {}>", idx.0))
    }

    fn name(&self, n: FName) -> String {
        self.lp
            .package
            .try_fname(n)
            .unwrap_or_else(|_| format!("<bad name {}>", n.index))
    }

    fn native(&self, index: u16) -> String {
        match self.natives.get(index) {
            Some(n) if n.is_operator() => {
                format!("#{index} {} [{}]", n.qualified, n.friendly_name)
            }
            Some(n) => format!("#{index} {}", n.qualified),
            None => format!("#{index} <no script function>"),
        }
    }
}

fn kind_of(lp: &LoadedPackage, i: usize) -> Option<ScriptKind> {
    ScriptKind::of_export(&lp.package, i).filter(|k| k.is_struct())
}

// ---------------------------------------------------------------- disasm

#[derive(Serialize)]
struct DisasmReport<'a> {
    object: String,
    kind: String,
    export_index: usize,
    storage_size: usize,
    memory_size: usize,
    declared_memory_size: i32,
    label_table_offset: Option<u16>,
    validation: bytecode::Validation,
    /// Decoded statements (raw operands: package indices, FNames, native indices).
    script: &'a Script,
    /// Package index -> qualified path for every object operand.
    objects: BTreeMap<i32, String>,
    /// FName "index:number" -> text for every name operand.
    names: BTreeMap<String, String>,
    /// Native index -> script function for every native token.
    natives: BTreeMap<u16, String>,
}

fn operand_text(e: &Expr, n: &Names<'_>, script: &Script) -> String {
    use ExprKind as K;
    let target = |t: u16| {
        let t = usize::from(t);
        match script.storage_offset_of(t) {
            Some(s) => format!("-> m{t} (@{s})"),
            None => format!("-> m{t} (not a token)"),
        }
    };
    match &e.kind {
        K::LocalVariable { property }
        | K::InstanceVariable { property }
        | K::DefaultVariable { property }
        | K::StateVariable { property }
        | K::LocalOutVariable { property }
        | K::NativeParm { property }
        | K::ReturnNothing { property }
        | K::EatReturnValue { property, .. } => n.short(*property),
        K::Switch {
            property,
            value_size,
            ..
        } => format!("{} size={value_size}", n.short(*property)),
        K::Case { next: Some(t), .. } => target(*t),
        K::Case { next: None, .. } => "default".to_owned(),
        K::Jump { target: t }
        | K::JumpIfNot { target: t, .. }
        | K::JumpIfNotEditorOnly { target: t } => target(*t),
        K::Iterator { end, .. } | K::DynArrayIterator { end, .. } => {
            format!("end {}", target(*end))
        }
        K::Assert {
            line, debug_only, ..
        } => format!("line={line} debug={debug_only}"),
        K::LabelTable { labels, .. } => labels
            .iter()
            .map(|l| format!("{}=m{}", n.name(l.name), l.offset))
            .collect::<Vec<_>>()
            .join(", "),
        K::Context(c) | K::ClassContext(c) => format!(
            "skip={} rvalue={} size={}",
            c.skip,
            n.short(c.rvalue_property),
            c.rvalue_size
        ),
        K::MetaCast { class, .. }
        | K::DynamicCast { class, .. }
        | K::InterfaceCast { class, .. } => n.obj(*class),
        K::PrimitiveCast { cast, .. } => format!("cast={cast:#04x}"),
        K::Skip { skip, .. } => format!("skip={skip}"),
        K::VirtualFunction { name, .. } | K::GlobalFunction { name, .. } => n.name(*name),
        K::FinalFunction { function, .. } => n.obj(*function),
        K::DelegateFunction {
            local,
            property,
            name,
            ..
        } => format!("{} via {} local={local}", n.name(*name), n.short(*property)),
        K::NativeFunction { index, .. } => n.native(*index),
        K::IntConst { value } => value.to_string(),
        K::FloatConst { value } => format!("{value:?}"),
        K::ByteConst { value } | K::IntConstByte { value } => value.to_string(),
        K::StringConst { value } | K::UnicodeStringConst { value } => format!("{value:?}"),
        K::ObjectConst { object } => n.obj(*object),
        K::NameConst { name } => format!("'{}'", n.name(*name)),
        K::RotationConst { pitch, yaw, roll } => format!("({pitch}, {yaw}, {roll})"),
        K::VectorConst { x, y, z } => format!("({x:?}, {y:?}, {z:?})"),
        K::StructCmp { struct_, .. } => n.obj(*struct_),
        K::StructMember {
            property,
            struct_,
            copy,
            modified,
            ..
        } => format!(
            "{}.{} copy={copy} modified={modified}",
            n.short(*struct_),
            n.short(*property)
        ),
        K::DynArrayAddItem { skip, .. }
        | K::DynArrayRemoveItem { skip, .. }
        | K::DynArrayInsertItem { skip, .. }
        | K::DynArrayFind { skip, .. }
        | K::DynArrayFindStruct { skip, .. }
        | K::DynArraySort { skip, .. } => format!("skip={skip}"),
        K::DebugInfo {
            version,
            line,
            pos,
            opcode,
        } => format!("v{version} line={line} pos={pos} op={opcode:#04x}"),
        K::DelegateProperty { function, property } => {
            format!("{} via {}", n.name(*function), n.short(*property))
        }
        K::InstanceDelegate { function } => n.name(*function),
        K::Conditional {
            skip_true,
            skip_false,
            ..
        } => format!("skips={skip_true}/{skip_false}"),
        K::DefaultParmValue { size, .. } => format!("size={size}"),
        _ => String::new(),
    }
}

fn cmd_disasm(file: &Path, object: &str, json: bool) -> Result<()> {
    let (set, lp) = open_set(file)?;
    let i = resolve(&lp, object)?;
    let Some(kind) = kind_of(&lp, i) else {
        bail!(
            "{} is a {}, not a Function, State, Class or ScriptStruct",
            lp.qualified(i)?,
            lp.package.export_class_name(i)?
        );
    };
    let bc = bytecode::export_bytecode(&lp.package, i)?;
    let script = bytecode::decode(bc.bytes, Layout::SHIPPED).with_context(|| {
        format!(
            "decoding the bytecode of {}",
            lp.qualified(i).unwrap_or_default()
        )
    })?;
    let natives = native_table(&set, file);
    let n = Names {
        lp: &lp,
        natives: &natives,
    };
    let validation = script.validate();
    eprintln!(
        "asamu-inspect: disassembly of the original game's code — LOCAL USE ONLY, never commit or quote it"
    );
    if json {
        let mut objects = BTreeMap::new();
        for o in script.object_operands() {
            objects.insert(o.object.0, n.obj(o.object));
        }
        let mut names = BTreeMap::new();
        let mut natives_used = BTreeMap::new();
        script.walk(&mut |e, _| {
            let fname = match &e.kind {
                ExprKind::VirtualFunction { name, .. }
                | ExprKind::GlobalFunction { name, .. }
                | ExprKind::DelegateFunction { name, .. }
                | ExprKind::NameConst { name }
                | ExprKind::InstanceDelegate { function: name }
                | ExprKind::DelegateProperty { function: name, .. } => Some(*name),
                _ => None,
            };
            if let Some(f) = fname {
                names.insert(format!("{}:{}", f.index, f.number), n.name(f));
            }
            if let ExprKind::LabelTable { labels, .. } = &e.kind {
                for l in labels {
                    names.insert(
                        format!("{}:{}", l.name.index, l.name.number),
                        n.name(l.name),
                    );
                }
            }
            if let ExprKind::NativeFunction { index, .. } = &e.kind {
                natives_used.insert(*index, n.native(*index));
            }
        });
        return print_json(&DisasmReport {
            object: lp.qualified(i)?,
            kind: kind.name().to_owned(),
            export_index: i,
            storage_size: script.storage_size,
            memory_size: script.memory_size,
            declared_memory_size: bc.memory_size,
            label_table_offset: bc.label_table_offset,
            validation,
            script: &script,
            objects,
            names,
            natives: natives_used,
        });
    }
    println!(
        "object       {}  ({} export {i})",
        lp.qualified(i)?,
        kind.name()
    );
    println!(
        "bytecode     {} B storage, {} B memory (declared {})",
        script.storage_size, script.memory_size, bc.memory_size
    );
    if let Some(lto) = bc.label_table_offset
        && lto != 0xFFFF
    {
        println!("label table  m{lto}");
    }
    println!(
        "checks       {} tokens, {} targets ({} bad), {} skips ({} bad)",
        validation.tokens,
        validation.targets,
        validation.bad_targets.len(),
        validation.skips,
        validation.bad_skips.len()
    );
    println!("{:>6} {:>6}  token", "@disk", "@mem");
    for st in &script.statements {
        st.walk(&mut |e, depth| {
            println!(
                "{:>6} {:>6}  {}{} {}",
                e.offset,
                e.mem_offset,
                "  ".repeat(depth),
                bytecode::token_name(e.token),
                operand_text(e, &n, &script)
            );
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- calls

#[derive(Serialize, Default)]
struct CallsRow {
    /// Function or state path relative to the class (`State.Function`).
    name: String,
    kind: String,
    export_index: usize,
    native: bool,
    bytecode_storage: usize,
    /// Qualified function path -> calls (`FinalFunction`).
    final_calls: BTreeMap<String, usize>,
    /// Function name -> calls (`VirtualFunction`, resolved by name at run time).
    virtual_calls: BTreeMap<String, usize>,
    /// Function name -> calls (`GlobalFunction`).
    global_calls: BTreeMap<String, usize>,
    /// Delegate name -> calls.
    delegate_calls: BTreeMap<String, usize>,
    /// "#index function [operator]" -> calls.
    natives: BTreeMap<String, usize>,
    /// Float constants (including vector components) -> occurrences.
    floats: BTreeMap<String, usize>,
    /// Integer constants -> occurrences.
    ints: BTreeMap<i32, usize>,
    /// Name constants -> occurrences.
    names: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct CallsReport {
    class: String,
    functions: Vec<CallsRow>,
    totals: CallsRow,
}

fn merge<K: Ord + Clone>(into: &mut BTreeMap<K, usize>, from: &BTreeMap<K, usize>) {
    for (k, v) in from {
        *into.entry(k.clone()).or_default() += v;
    }
}

fn cmd_calls(file: &Path, class: &str, json: bool) -> Result<()> {
    let (set, lp) = open_set(file)?;
    let ci = resolve(&lp, class)?;
    if kind_of(&lp, ci) != Some(ScriptKind::Class) {
        bail!("{} is not a class", lp.qualified(ci)?);
    }
    let class_ref = PackageIndex::from_export(ci).context("export index")?;
    let natives = native_table(&set, file);
    let n = Names {
        lp: &lp,
        natives: &natives,
    };
    let pkg = &lp.package;
    let class_path = pkg.export_path(ci)?;
    let mut rows = Vec::new();
    for i in 0..pkg.exports.len() {
        let Some(kind) = kind_of(&lp, i) else {
            continue;
        };
        if !matches!(kind, ScriptKind::Function | ScriptKind::State) && i != ci {
            continue;
        }
        let Some(me) = PackageIndex::from_export(i) else {
            continue;
        };
        // Functions and states of the class (also functions inside its states).
        let chain = pkg.outer_chain(me).unwrap_or_default();
        if i != ci && !chain.iter().skip(1).any(|o| *o == class_ref) {
            continue;
        }
        let path = pkg.export_path(i).unwrap_or_default();
        let name = path
            .strip_prefix(&class_path)
            .map(|s| s.trim_start_matches('.').to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(class)".to_owned());
        let mut row = CallsRow {
            name,
            kind: kind.name().to_owned(),
            export_index: i,
            ..CallsRow::default()
        };
        let bc = match bytecode::export_bytecode(pkg, i) {
            Ok(b) => b,
            Err(e) => {
                row.error = Some(e.to_string());
                rows.push(row);
                continue;
            }
        };
        row.native = bc
            .function
            .is_some_and(|(_, f)| f & asamu_ue3::flags::function::NATIVE != 0);
        row.bytecode_storage = bc.bytes.len();
        match bytecode::decode(bc.bytes, Layout::SHIPPED) {
            Ok(script) => {
                let r = script.references();
                for (k, v) in &r.final_functions {
                    *row.final_calls.entry(n.obj(PackageIndex(*k))).or_default() += v;
                }
                let fname = |k: &(i32, i32)| {
                    n.name(FName {
                        index: k.0,
                        number: k.1,
                    })
                };
                for (k, v) in &r.virtual_functions {
                    *row.virtual_calls.entry(fname(k)).or_default() += v;
                }
                for (k, v) in &r.global_functions {
                    *row.global_calls.entry(fname(k)).or_default() += v;
                }
                for (k, v) in &r.delegate_functions {
                    *row.delegate_calls.entry(fname(k)).or_default() += v;
                }
                for (k, v) in &r.natives {
                    *row.natives.entry(n.native(*k)).or_default() += v;
                }
                for (k, v) in &r.floats {
                    *row.floats
                        .entry(format!("{:?}", f32::from_bits(*k)))
                        .or_default() += v;
                }
                merge(&mut row.ints, &r.ints);
                for (k, v) in &r.names {
                    *row.names.entry(fname(k)).or_default() += v;
                }
            }
            Err(e) => row.error = Some(e.to_string()),
        }
        rows.push(row);
    }
    let mut totals = CallsRow {
        name: "(all)".to_owned(),
        kind: "Total".to_owned(),
        export_index: ci,
        ..CallsRow::default()
    };
    for r in &rows {
        totals.bytecode_storage += r.bytecode_storage;
        merge(&mut totals.final_calls, &r.final_calls);
        merge(&mut totals.virtual_calls, &r.virtual_calls);
        merge(&mut totals.global_calls, &r.global_calls);
        merge(&mut totals.delegate_calls, &r.delegate_calls);
        merge(&mut totals.natives, &r.natives);
        merge(&mut totals.floats, &r.floats);
        merge(&mut totals.ints, &r.ints);
        merge(&mut totals.names, &r.names);
    }
    let report = CallsReport {
        class: lp.qualified(ci)?,
        functions: rows,
        totals,
    };
    if json {
        return print_json(&report);
    }
    println!("class {}", report.class);
    for r in report
        .functions
        .iter()
        .chain(std::iter::once(&report.totals))
    {
        println!(
            "\n{} {}{}  ({} B bytecode)",
            r.kind,
            r.name,
            if r.native { " [native]" } else { "" },
            r.bytecode_storage
        );
        if let Some(e) = &r.error {
            println!("  error      {e}");
        }
        print_counts("final", &r.final_calls);
        print_counts("virtual", &r.virtual_calls);
        print_counts("global", &r.global_calls);
        print_counts("delegate", &r.delegate_calls);
        print_counts("native", &r.natives);
        print_counts("float", &r.floats);
        let ints: BTreeMap<String, usize> =
            r.ints.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        print_counts("int", &ints);
        print_counts("name", &r.names);
    }
    Ok(())
}

fn print_counts(label: &str, m: &BTreeMap<String, usize>) {
    if m.is_empty() {
        return;
    }
    let items: Vec<String> = m
        .iter()
        .map(|(k, v)| {
            if *v > 1 {
                format!("{k} x{v}")
            } else {
                k.clone()
            }
        })
        .collect();
    println!("  {label:<10} {}", items.join(", "));
}

// ---------------------------------------------------------------- coverage

#[derive(Serialize)]
struct CoverageRow {
    file: String,
    coverage: Option<BytecodeCoverage>,
    arity: Option<ArityCheck>,
    error: Option<String>,
}

#[derive(Serialize)]
struct CoverageReport {
    layout: Layout,
    natives: usize,
    native_conflicts: usize,
    packages: Vec<CoverageRow>,
    total: BytecodeCoverage,
    total_arity: ArityCheck,
}

fn cmd_coverage(dir: &Path, json: bool) -> Result<()> {
    let files = script_package_files(dir);
    if files.is_empty() {
        bail!("no .u/.upk packages in {}", dir.display());
    }
    let set = PackageSet::new(&[dir.to_path_buf()]);
    let mut natives = NativeTable::default();
    let mut loaded = Vec::new();
    for f in &files {
        let label = f
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match set.open_file(f) {
            Ok(lp) => {
                natives.add_package(&lp.package, &lp.name);
                loaded.push((label, Ok(lp)));
            }
            Err(e) => loaded.push((label, Err(e.to_string()))),
        }
    }
    let mut rows = Vec::new();
    let mut total = BytecodeCoverage::default();
    let mut total_arity = ArityCheck::default();
    for (label, lp) in loaded {
        match lp {
            Ok(lp) => {
                let c = bytecode::package_bytecode_coverage(&lp.package, &label, Layout::SHIPPED);
                let a = bytecode::check_call_arity(&set, &lp, &natives, Layout::SHIPPED);
                total.absorb(&c);
                total_arity.absorb(&a);
                rows.push(CoverageRow {
                    file: label,
                    coverage: Some(c),
                    arity: Some(a),
                    error: None,
                });
            }
            Err(e) => rows.push(CoverageRow {
                file: label,
                coverage: None,
                arity: None,
                error: Some(e),
            }),
        }
    }
    let report = CoverageReport {
        layout: Layout::SHIPPED,
        natives: natives.len(),
        native_conflicts: natives.conflicts.len(),
        packages: rows,
        total,
        total_arity,
    };
    if json {
        return print_json(&report);
    }
    println!(
        "{:<32} {:>7} {:>7} {:>7} {:>7} {:>7} {:>9}",
        "package", "structs", "with bc", "exact", "memory", "clean", "storage B"
    );
    for r in &report.packages {
        match (&r.coverage, &r.error) {
            (Some(c), _) => {
                let t = c.total();
                println!(
                    "{:<32} {:>7} {:>7} {:>7} {:>7} {:>7} {:>9}",
                    r.file,
                    t.total,
                    t.with_bytecode,
                    t.exact,
                    t.memory_match,
                    t.clean,
                    c.storage_bytes
                );
            }
            (None, Some(e)) => println!("{:<32} ERROR {e}", r.file),
            (None, None) => {}
        }
    }
    let c = &report.total;
    println!();
    for (k, v) in &c.kinds {
        println!(
            "{k:<14} {} total, {} with bytecode, {} exact, {} memory size match, {} clean, {} end with EndOfScript",
            v.total, v.with_bytecode, v.exact, v.memory_match, v.clean, v.end_of_script
        );
    }
    println!(
        "bytes          {} storage, {} memory; {} tokens; max depth {}",
        c.storage_bytes, c.memory_bytes, c.tokens, c.max_depth
    );
    println!(
        "targets        {} absolute ({} bad, {} on statement starts)",
        c.targets, c.bad_targets, c.targets_on_statements
    );
    println!(
        "skips          {} relative ({} bad; {} loop contexts, {} object-size contexts, {} of them on Outer)",
        c.skips,
        c.bad_skips,
        c.loop_context_skips,
        c.object_size_context_skips,
        c.object_size_context_skips_on_outer
    );
    println!(
        "labels         {} tables ({} None terminators); LabelTableOffset {} / {} ok",
        c.label_tables, c.label_terminators_none, c.label_table_offsets_ok, c.label_table_offsets
    );
    println!(
        "RepOffset      {} / {} on class statements",
        c.rep_offsets_ok, c.rep_offsets
    );
    println!(
        "operands       {} typed ({} violations); {} / {} locals in their function; {} / {} variable contexts",
        c.operands,
        c.operand_violations,
        c.local_operands_ok,
        c.local_operands,
        c.variable_contexts_ok,
        c.variable_contexts
    );
    println!(
        "statements     {} suspicious top-level",
        c.suspicious_statements
    );
    let a = &report.total_arity;
    println!(
        "arity          final {} / {} ({} unresolved); native {} / {} ({} unresolved); {} natives in table, {} conflicts",
        a.final_ok,
        a.final_calls,
        a.final_unresolved,
        a.native_ok,
        a.native_calls,
        a.native_unresolved,
        report.natives,
        report.native_conflicts
    );
    let used: BTreeSet<&String> = c.token_counts.keys().collect();
    println!("tokens used    {}", used.len());
    for (k, v) in &c.token_counts {
        println!("  {v:>8}  {k}");
    }
    for f in c.failures.iter().chain(a.failures.iter()) {
        println!("failure        {f}");
    }
    Ok(())
}
