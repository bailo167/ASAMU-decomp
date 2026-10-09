//! Static self-checks of the trace recorder (tools/trace-recorder):
//! every symbol and offset its Python sources use exists in its layout file,
//! the layout agrees with `native_layout.json` and the defaults data, and —
//! when the original executable is installed — with the executable's symbol
//! table and code bytes. The binary part skips cleanly when the install is
//! absent (set `ASAMU_ORIGINAL_DIR` to the folder containing
//! `A Story About My Uncle.app`).

#![allow(clippy::unwrap_used)]

mod common;

use asamu_trace::layout::{self, RecorderLayout, default_executable, run_all};

fn report(checks: &[layout::Check]) -> String {
    checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| format!("[{}] {}", c.group, c.what))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn layout_matches_repository_data_and_python_usage() {
    let root = common::repo_root();
    // run_all also runs the binary group when the install is present; the
    // next test checks that group on its own.
    let r = run_all(&root, None).unwrap();
    let failures = report(&r.checks);
    assert!(failures.is_empty(), "failed checks:\n{failures}");
    // Every group ran.
    for group in [
        "schema",
        "native_layout",
        "defaults",
        "native_code",
        "python",
    ] {
        assert!(
            r.checks.iter().any(|c| c.group == group),
            "no {group} checks ran"
        );
    }
    // The pattern scan really found the recorder's lookups (it has well over
    // 80; a broken scan would find few).
    assert!(
        r.python_lookups >= 80,
        "only {} lookups found",
        r.python_lookups
    );
    let text = std::fs::read_to_string(root.join(layout::LAYOUT_PATH)).unwrap();
    let l = RecorderLayout::from_json(&text).unwrap();
    // Every layout field is used by the Python sources, and nothing else is
    // read (keeps the layout file honest).
    let mut used = std::collections::BTreeSet::new();
    for p in layout::PYTHON_SOURCES {
        let src = std::fs::read_to_string(root.join(p)).unwrap();
        for lk in layout::python_lookups(&src) {
            if let Some(b) = lk.b {
                used.insert((lk.a, b));
            }
        }
    }
    for f in &l.fields {
        assert!(
            used.contains(&(f.class.clone(), f.name.clone())),
            "layout field {}.{} is never looked up",
            f.class,
            f.name
        );
    }
}

#[test]
fn layout_matches_the_executable_when_installed() {
    let root = common::repo_root();
    let text = std::fs::read_to_string(root.join(layout::LAYOUT_PATH)).unwrap();
    let l = RecorderLayout::from_json(&text).unwrap();
    let Some(exe) = default_executable(&l) else {
        eprintln!("skipped: the original executable is not installed (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let r = run_all(&root, Some(&exe)).unwrap();
    let binary: Vec<_> = r
        .checks
        .iter()
        .filter(|c| c.group == "binary")
        .cloned()
        .collect();
    assert!(!binary.is_empty());
    let failures = report(&binary);
    assert!(failures.is_empty(), "binary checks failed:\n{failures}");
    // Every symbol, evidence entry and call check produced a result.
    let expected = l.symbols.len()
        + l.symbols.values().filter(|s| s.read.is_some()).count()
        + l.symbols
            .values()
            .filter(|s| s.file_value_f64.is_some())
            .count()
        + l.native_evidence.len()
        + l.calls.len();
    assert_eq!(binary.len(), expected);
}
