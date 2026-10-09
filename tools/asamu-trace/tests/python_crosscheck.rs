//! The Python recorder (tools/trace-recorder) against the Rust reference:
//! its own self-tests, its LLDB front end against a stand-in `lldb` module,
//! and its raw → canonical converter against `asamu_trace::convert` on
//! pseudo-random recordings. Skips when no `python3` is on the PATH.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use asamu_player::Trace;
use asamu_trace::convert::{ConvertOptions, INIT_NOTE_PREFIX, convert, output_names};
use asamu_trace::raw::{RawBinding, RawFile};

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .args([
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
            ])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn recorder_dir() -> PathBuf {
    common::repo_root().join("tools/trace-recorder")
}

fn run_python(py: &str, args: &[&std::ffi::OsStr]) -> String {
    let out = Command::new(py)
        .args(["-I", "-B"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "python {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn python_self_tests() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = recorder_dir();
    let core = dir.join("asamu_recorder_core.py");
    let out = run_python(py, &[core.as_os_str(), "selftest".as_ref()]);
    assert!(out.contains("selftest ok"), "{out}");
    let glue = dir.join("test_lldb_glue.py");
    let out = run_python(py, &[glue.as_os_str()]);
    assert!(out.contains("lldb glue test ok"), "{out}");
}

fn same_notes(py: &[String], rs: &[String]) {
    assert_eq!(py.len(), rs.len(), "notes differ:\n{py:#?}\n{rs:#?}");
    for (a, b) in py.iter().zip(rs) {
        match (
            a.strip_prefix(INIT_NOTE_PREFIX),
            b.strip_prefix(INIT_NOTE_PREFIX),
        ) {
            (Some(ja), Some(jb)) => {
                let va: serde_json::Value = serde_json::from_str(ja).unwrap();
                let vb: serde_json::Value = serde_json::from_str(jb).unwrap();
                assert_eq!(va, vb, "init notes differ");
            }
            _ => assert_eq!(a, b),
        }
    }
}

fn crosscheck(py: &str, raw: &RawFile, dir: &Path, stem: &str) -> usize {
    let raw_path = dir.join(format!("{stem}.raw.jsonl"));
    std::fs::write(&raw_path, raw.to_jsonl_string().unwrap()).unwrap();
    let core = recorder_dir().join("asamu_recorder_core.py");
    let out_dir = dir.join(stem);
    std::fs::create_dir_all(&out_dir).unwrap();
    let printed = run_python(
        py,
        &[
            core.as_os_str(),
            "convert".as_ref(),
            raw_path.as_os_str(),
            "--out-dir".as_ref(),
            out_dir.as_os_str(),
        ],
    );
    let rust = convert(raw, &ConvertOptions::default()).unwrap();
    let names = output_names(&format!("{stem}.raw.jsonl"), rust.len());
    assert_eq!(printed.lines().count(), rust.len(), "{printed}");
    for (seg, name) in rust.iter().zip(&names) {
        let text = std::fs::read_to_string(out_dir.join(name)).unwrap();
        let p = Trace::from_jsonl_str(&text).unwrap();
        let r = &seg.trace;
        assert_eq!(p.samples, r.samples, "{name}: samples differ");
        assert_eq!(p.meta.format, r.meta.format);
        assert_eq!(p.meta.schema_version, r.meta.schema_version);
        assert_eq!(p.meta.source, r.meta.source);
        assert_eq!(p.meta.game_build, r.meta.game_build);
        assert_eq!(p.meta.level, r.meta.level);
        assert_eq!(p.meta.tick_rate, r.meta.tick_rate);
        assert_eq!(p.meta.units, r.meta.units);
        same_notes(&p.meta.notes, &r.meta.notes);
    }
    rust.len()
}

#[test]
fn python_and_rust_converters_agree() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (scripted, _) = common::scripted_recording(80, 10);
    assert_eq!(crosscheck(py, &scripted, dir.path(), "scripted"), 1);
    let mut segments = 0;
    for (i, seed) in [1_u64, 7, 99, 2024].into_iter().enumerate() {
        let raw = common::varied_recording(seed, 400, i % 2 == 0);
        segments += crosscheck(py, &raw, dir.path(), &format!("varied{seed}"));
    }
    assert!(segments >= 8, "only {segments} segments exercised");

    // A hostile binding table (wide alias fan-out, self references, empty
    // parts): both converters stop at the same expansion budget.
    let mut raw = common::varied_recording(5, 300, true);
    let wide = |next: &str, k: usize| {
        let mut parts = vec![next.to_owned(); k];
        parts.push(String::new());
        parts.join("|")
    };
    for i in 0..6 {
        raw.header.bindings.push(RawBinding {
            name: format!("L{i}"),
            command: wide(&format!("L{}", i + 1), 12),
        });
    }
    raw.header.bindings.extend(
        [
            ("L6", "Axis aBaseY Speed=1 | Jump | L0"),
            ("W", "L0 | Axis aBaseY Speed=-1"),
            ("S", "L3 | Axis aBaseY Speed=-1 | Axis aBaseY Speed=-1"),
            ("D", "D | D | Axis aStrafe Speed=+1"),
        ]
        .into_iter()
        .map(|(n, c)| RawBinding {
            name: n.into(),
            command: c.into(),
        }),
    );
    assert!(crosscheck(py, &raw, dir.path(), "hostile") >= 1);
}
