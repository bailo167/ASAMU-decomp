//! Bytecode decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts are
//! asserted; no bytecode, listing or source text is printed or written.
//!
//! This is the acceptance test for `docs/reverse-engineering/BYTECODE.md`:
//! every Function, State and Class with bytecode in the 12 `.u` packages and
//! `Startup.upk` decodes to exactly `ScriptStorageSize` bytes, the memory
//! total equals `ScriptBytecodeSize`, every absolute target lands on a token
//! boundary and every relative skip covers the expression it skips.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use asamu_ue3::bytecode::{
    ArityCheck, BytecodeCoverage, Layout, NativeTable, check_call_arity, package_bytecode_coverage,
};
use asamu_ue3::model::PackageSet;

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

/// The script packages: every `.u` plus the `.upk` files (only `Startup.upk`
/// holds script objects; the others contribute nothing).
fn script_packages(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
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

struct All {
    coverage: BytecodeCoverage,
    arity: ArityCheck,
    natives: NativeTable,
    u_packages: usize,
    narrow_memory_match: usize,
}

fn run(dir: &Path) -> All {
    let set = PackageSet::new(&[dir.to_path_buf()]);
    let mut natives = NativeTable::default();
    let mut lps = Vec::new();
    let mut u_packages = 0;
    for p in script_packages(dir) {
        if p.extension().is_some_and(|x| x == "u") {
            u_packages += 1;
        }
        let lp = set.open_file(&p).unwrap();
        natives.add_package(&lp.package, &lp.name);
        lps.push(lp);
    }
    let mut coverage = BytecodeCoverage::default();
    let mut arity = ArityCheck::default();
    let mut narrow_memory_match = 0;
    for lp in &lps {
        let c = package_bytecode_coverage(&lp.package, &lp.name, Layout::SHIPPED);
        coverage.absorb(&c);
        arity.absorb(&check_call_arity(&set, lp, &natives, Layout::SHIPPED));
        narrow_memory_match += package_bytecode_coverage(&lp.package, &lp.name, Layout::STORAGE)
            .total()
            .memory_match;
    }
    All {
        coverage,
        arity,
        natives,
        u_packages,
        narrow_memory_match,
    }
}

#[test]
fn every_script_decodes_exactly_with_valid_targets_and_skips() {
    let dir = require_data!();
    let all = run(&dir);
    let c = &all.coverage;
    assert_eq!(all.u_packages, 12);
    assert!(c.failures.is_empty(), "{:#?}", c.failures);

    // Per kind: every struct with bytecode decodes exactly, matches the
    // declared memory size, validates and ends with EndOfScript.
    let kind = |k: &str| c.kinds.get(k).cloned().unwrap_or_default();
    let (class, state, function, sstruct) = (
        kind("Class"),
        kind("State"),
        kind("Function"),
        kind("ScriptStruct"),
    );
    assert_eq!((class.total, class.with_bytecode), (2521, 79));
    assert_eq!((state.total, state.with_bytecode), (211, 211));
    assert_eq!((function.total, function.with_bytecode), (12511, 12511));
    assert_eq!((sstruct.total, sstruct.with_bytecode), (848, 0));
    for k in [&class, &state, &function] {
        assert_eq!(k.exact, k.with_bytecode);
        assert_eq!(k.memory_match, k.with_bytecode);
        assert_eq!(k.clean, k.with_bytecode);
        assert_eq!(k.end_of_script, k.with_bytecode);
    }
    let t = c.total();
    assert_eq!(t.with_bytecode, 12_801);
    assert_eq!(c.storage_bytes, 1_749_984);
    assert_eq!(c.memory_bytes, 2_508_276);
    assert_eq!(c.tokens, 417_163);

    // Targets and skips.
    assert_eq!(c.targets, 23_533);
    assert_eq!(c.bad_targets, 0);
    assert_eq!(c.targets_on_statements, c.targets);
    assert_eq!(c.skips, 39_146);
    assert_eq!(c.bad_skips, 0);
    assert_eq!(c.loop_context_skips, 227);
    assert_eq!(c.object_size_context_skips, 244);
    assert_eq!(c.object_size_context_skips_on_outer, 244);

    // Labels and replication offsets.
    assert_eq!(c.label_tables, 100);
    assert_eq!(c.label_terminators_none, 100);
    assert_eq!(c.label_table_offsets, 100);
    assert_eq!(c.label_table_offsets_ok, 100);
    assert_eq!(c.rep_offsets, 284);
    assert_eq!(c.rep_offsets_ok, 284);

    // Tree-shape cross-checks.
    assert_eq!(c.suspicious_statements, 0);
    assert_eq!(c.operands, 189_573);
    assert_eq!(c.operand_violations, 0);
    assert_eq!(c.local_operands, 68_500);
    assert_eq!(c.local_operands_ok, c.local_operands);
    assert_eq!(c.variable_contexts, 16_316);
    assert_eq!(c.variable_contexts_ok, c.variable_contexts);
    assert!(c.max_depth <= 32, "max depth {}", c.max_depth);

    // Native token forms and the token set actually used.
    assert_eq!(c.native_forms.get("one-byte"), Some(&44_086));
    assert_eq!(c.native_forms.get("two-byte"), Some(&2_074));
    assert_eq!(c.token_counts.len(), 81);
    for unused in [
        "DebugInfo",
        "JumpIfNotEditorOnly",
        "NotEqual_DelFunc",
        "Unknown",
    ] {
        assert!(!c.token_counts.contains_key(unused), "{unused} occurs");
    }

    // Arity: one argument expression per parameter for every bound call.
    assert_eq!(all.natives.len(), 202);
    assert!(all.natives.conflicts.is_empty());
    let a = &all.arity;
    assert!(a.failures.is_empty(), "{:#?}", a.failures);
    assert_eq!(
        (a.final_calls, a.final_ok, a.final_unresolved),
        (8_233, 8_233, 0)
    );
    assert_eq!(
        (a.native_calls, a.native_ok, a.native_unresolved),
        (46_160, 46_160, 0)
    );

    // Object references are 8 bytes in memory: with 4-byte references only
    // the scripts without any object operand reproduce ScriptBytecodeSize.
    assert!(all.narrow_memory_match < t.with_bytecode / 2);
    eprintln!(
        "bytecode: {} structs, {} storage / {} memory bytes, {} tokens; {} of them match with 4-byte refs",
        t.with_bytecode, c.storage_bytes, c.memory_bytes, c.tokens, all.narrow_memory_match
    );
}
