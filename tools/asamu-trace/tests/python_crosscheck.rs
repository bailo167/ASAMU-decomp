//! The Python recorder (tools/trace-recorder) against the Rust reference:
//! its own self-tests (with the Mac and the Windows layout), its LLDB front
//! end against a stand-in `lldb` module, its Windows front end against a
//! fake 32-bit image, and its raw → canonical converter against
//! `asamu_trace::convert` on pseudo-random recordings. Skips when no
//! `python3` is on the PATH.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use asamu_player::Trace;
use asamu_trace::convert::{ConvertOptions, INIT_NOTE_PREFIX, MoveInput, convert, output_names};
use asamu_trace::move_input::MoveFrame;
use asamu_trace::raw::{RawBinding, RawFile};
use asamu_trace::state::STATE_NOTE_PREFIX;

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
    // The same core with the Windows layout (4-byte pointers, UTF-16).
    let win_layout = dir.join("layout_win_x86.json");
    let out = run_python(
        py,
        &[
            core.as_os_str(),
            "selftest".as_ref(),
            "--layout".as_ref(),
            win_layout.as_os_str(),
        ],
    );
    assert!(
        out.contains("selftest ok (layout win-x86-steam-1822049, 4-byte pointers"),
        "{out}"
    );
}

/// The Windows front end on its fake 32-bit image (frame order played one
/// event per memory read; no Windows needed).
#[test]
fn python_windows_front_end_tests() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let glue = recorder_dir().join("test_win_glue.py");
    let out = run_python(py, &[glue.as_os_str()]);
    assert!(out.contains("win glue test ok"), "{out}");
}

/// The notes agree: text for text, and the two JSON notes (the state
/// timeline and the older `init:` note) value for value.
fn same_notes(py: &[String], rs: &[String]) {
    assert_eq!(py.len(), rs.len(), "notes differ:\n{py:#?}\n{rs:#?}");
    for (a, b) in py.iter().zip(rs) {
        let json = [INIT_NOTE_PREFIX, STATE_NOTE_PREFIX]
            .into_iter()
            .find_map(|p| Some((a.strip_prefix(p)?, b.strip_prefix(p)?)));
        match json {
            Some((ja, jb)) => {
                let va: serde_json::Value = serde_json::from_str(ja).unwrap();
                let vb: serde_json::Value = serde_json::from_str(jb).unwrap();
                assert_eq!(va, vb, "JSON notes differ");
            }
            None => assert_eq!(a, b),
        }
    }
}

fn crosscheck(py: &str, raw: &RawFile, dir: &Path, stem: &str) -> usize {
    crosscheck_with(py, raw, dir, stem, &ConvertOptions::default())
}

/// Converts `raw` with both converters under `opts` and compares every
/// trace: samples bit for bit, metadata and notes. Returns the number of
/// traces.
fn crosscheck_with(
    py: &str,
    raw: &RawFile,
    dir: &Path,
    stem: &str,
    opts: &ConvertOptions,
) -> usize {
    let raw_path = dir.join(format!("{stem}.raw.jsonl"));
    std::fs::write(&raw_path, raw.to_jsonl_string().unwrap()).unwrap();
    let core = recorder_dir().join("asamu_recorder_core.py");
    let out_dir = dir.join(stem);
    std::fs::create_dir_all(&out_dir).unwrap();
    let move_input = match opts.move_input {
        MoveInput::Auto => "auto",
        MoveInput::Keys => "keys",
        MoveInput::Acceleration => "acceleration",
    };
    let move_frame = match opts.move_frame {
        MoveFrame::Original => "original",
        MoveFrame::Yaw => "yaw",
    };
    let printed = run_python(
        py,
        &[
            core.as_os_str(),
            "convert".as_ref(),
            raw_path.as_os_str(),
            "--out-dir".as_ref(),
            out_dir.as_os_str(),
            "--move-input".as_ref(),
            move_input.as_ref(),
            "--move-frame".as_ref(),
            move_frame.as_ref(),
        ],
    );
    let rust = convert(raw, opts).unwrap();
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

    // Recordings without benchmark mode: every frame has its own length.
    // Both converters write no tick rate and the same sample times (the f64
    // sum of the f32 frame lengths).
    let lengths = common::jittery_lengths(3, 150, 60.0);
    let (variable, _) = common::scripted_recording_variable(&lengths, 500);
    assert_eq!(crosscheck(py, &variable, dir.path(), "variable"), 1);
    let rust = convert(&variable, &ConvertOptions::default()).unwrap();
    assert_eq!(rust[0].trace.meta.tick_rate, None);
    let mut variable_segments = 0;
    for seed in [11_u64, 12, 13] {
        let mut raw = common::varied_recording(seed, 300, seed % 2 == 1);
        common::with_frame_lengths(&mut raw, seed);
        let segs = convert(&raw, &ConvertOptions::default()).unwrap();
        // (A two-sample run has one frame length, which is a fixed rate.)
        assert!(
            segs.iter()
                .all(|s| s.trace.samples.len() < 3 || s.trace.meta.tick_rate.is_none())
        );
        variable_segments += crosscheck(py, &raw, dir.path(), &format!("variable{seed}"));
    }
    assert!(variable_segments >= 6, "only {variable_segments} segments");

    // A level loaded again between two consecutive frames (the world's clock
    // goes back, nothing else changes): both converters end the run there.
    let (mut reloaded, _) = common::scripted_recording(80, 10);
    let before = convert(&reloaded, &ConvertOptions::default())
        .unwrap()
        .len();
    for r in &mut reloaded.records[40..] {
        let w = r.world.as_mut().unwrap();
        w.time_seconds -= 0.5;
        w.real_time_seconds -= 0.5;
    }
    assert_eq!(
        crosscheck(py, &reloaded, dir.path(), "reloaded"),
        before + 1
    );

    // Recordings made with a gamepad: the move axes from the pawn's
    // acceleration, in both converters bit for bit; also when asked for on
    // pseudo-random recordings (pawn rotations with pitch and roll, zero,
    // unit-length, tiny and huge accelerations, flying with and without gun
    // data), in either frame, and with the keys forced.
    let (pad, _) = common::scripted_gamepad_recording(150, 3_000);
    assert_eq!(crosscheck(py, &pad, dir.path(), "pad"), 1);
    let derived = convert(&pad, &ConvertOptions::default()).unwrap();
    assert!(
        derived[0]
            .trace
            .samples
            .iter()
            .any(|s| s.input.move_forward != 0.0 && s.input.move_right != 0.0),
        "the pad run moves diagonally"
    );
    let mut moved = 0;
    for (i, seed) in [21_u64, 22, 23, 24].into_iter().enumerate() {
        let mut raw = common::varied_recording(seed, 400, i % 2 == 0);
        common::with_accelerations(&mut raw, seed);
        for (move_input, move_frame) in [
            (MoveInput::Acceleration, MoveFrame::Original),
            (MoveInput::Acceleration, MoveFrame::Yaw),
            (MoveInput::Auto, MoveFrame::Original),
            (MoveInput::Keys, MoveFrame::Original),
        ] {
            let opts = ConvertOptions {
                level: None,
                move_input,
                move_frame,
            };
            let stem = format!("accel{seed}-{move_input:?}-{move_frame:?}");
            crosscheck_with(py, &raw, dir.path(), &stem, &opts);
            if move_input == MoveInput::Acceleration {
                moved += convert(&raw, &opts)
                    .unwrap()
                    .iter()
                    .flat_map(|s| &s.trace.samples)
                    .filter(|s| s.input.move_forward != 0.0 || s.input.move_right != 0.0)
                    .count();
            }
        }
    }
    assert!(moved > 800, "only {moved} derived samples exercised");

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
