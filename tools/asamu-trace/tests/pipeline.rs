//! End-to-end: synthetic raw recording → convert → replay → compare → report,
//! through the library and through the command-line tool.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::process::Command;

use asamu_player::Trace;
use asamu_player::trace::CompareTolerances;
use asamu_trace::compare::{CompareSummary, Verdict, compare_traces};
use asamu_trace::convert::{ConvertOptions, convert};
use asamu_trace::raw::RawFile;
use asamu_trace::replay::{ReplayOptions, StartPolicy, replay};
use asamu_trace::segments::{clean_runs, is_clean_start};

/// The converted trace's yaw is `units × 2π/65536`; our simulation adds the
/// same look deltas in `f32` and wraps, which can differ in the last bit.
const ANGLE_EPS: f64 = 1e-6;

#[test]
fn convert_replay_compare_reproduces_our_own_run() {
    let (raw, ours) = common::scripted_recording(120, 70_000);
    let segs = convert(&raw, &ConvertOptions::default()).unwrap();
    assert_eq!(segs.len(), 1);
    let original = &segs[0].trace;
    assert_eq!(original.meta.tick_rate, Some(60.0));
    assert_eq!(original.samples.len(), 121);
    // The converted inputs are exactly the inputs our run consumed.
    for (a, b) in original.samples.iter().zip(&ours.samples) {
        assert_eq!(a.input, b.input, "tick {}", a.tick);
        assert_eq!(a.position, b.position, "tick {}", a.tick);
        assert_eq!(a.grounded, b.grounded, "tick {}", a.tick);
    }
    assert!(
        original.samples.iter().any(|s| !s.grounded),
        "the script jumps"
    );

    let r = replay(original, &ReplayOptions::default()).unwrap();
    assert_eq!(r.stopped_at_gap, None);
    let tol = CompareTolerances {
        angle: ANGLE_EPS,
        ..CompareTolerances::default()
    };
    let s = compare_traces("original", original, "replay", &r.trace, &tol);
    assert!(
        matches!(s.verdict, Verdict::Exact | Verdict::WithinTolerance),
        "{}",
        asamu_trace::compare::render_text(&s)
    );
    assert_eq!(s.diff.matched, 121);
    assert_eq!(s.diff.position.max, 0.0);
    assert_eq!(s.diff.velocity.max, 0.0);
    assert_eq!(s.diff.grounded_mismatches, 0);
    assert_eq!(s.diff.grapple_state_mismatches, 0);
    assert_eq!(s.diff.input_mismatches, 0);
    assert!(s.diff.yaw.max <= ANGLE_EPS);

    // A perturbed "original" diverges where it was perturbed.
    let mut bent = original.clone();
    for t in bent.samples.iter_mut().skip(80) {
        t.position.x += 2.0;
    }
    let s = compare_traces("bent", &bent, "replay", &r.trace, &tol);
    assert_eq!(s.verdict, Verdict::Diverged);
    assert_eq!(s.first_exceedance["position"].tick, 80);
}

/// A recording made with a gamepad has no move key: the move axes come from
/// the pawn's acceleration, and the replay reproduces the run.
#[test]
fn gamepad_recording_replays_through_the_derived_move_axes() {
    let (raw, ours) = common::scripted_gamepad_recording(120, 5_000);
    assert!(raw.records.iter().all(|r| {
        r.player
            .as_ref()
            .unwrap()
            .keys
            .iter()
            .all(|k| k.starts_with("Xbox"))
    }),);
    let segs = convert(&raw, &ConvertOptions::default()).unwrap();
    assert_eq!(segs.len(), 1);
    let original = &segs[0].trace;
    let notes = &original.meta.notes;
    assert!(
        notes.iter().any(|n| n.starts_with(
            "move input: derived from the pawn's Acceleration (auto: no move key in this run)"
        )),
        "{notes:#?}"
    );
    // Buttons and look are exactly the run's; the move axes are the run's
    // direction (its length is not recorded), except while the grapple holds
    // the pawn, where no steering is read.
    let mut derived = 0;
    for (a, b) in original.samples.iter().zip(&ours.samples) {
        let (i, o) = (&a.input, &b.input);
        assert_eq!(
            (
                i.jump_pressed,
                i.jump_held,
                i.grapple_held,
                i.sprint_held,
                i.use_pressed
            ),
            (
                o.jump_pressed,
                o.jump_held,
                o.grapple_held,
                o.sprint_held,
                o.use_pressed
            ),
            "tick {}",
            a.tick
        );
        assert_eq!(i.look_yaw_delta, o.look_yaw_delta, "tick {}", a.tick);
        let len = f64::from(o.move_forward).hypot(f64::from(o.move_right));
        let attached = a.grapple_state == asamu_player::trace::TraceGrappleState::Attached;
        let want = if len == 0.0 || attached {
            (0.0, 0.0)
        } else {
            derived += 1;
            (
                f64::from(o.move_forward) / len,
                f64::from(o.move_right) / len,
            )
        };
        assert!(
            (f64::from(i.move_forward) - want.0).abs() < 1e-6
                && (f64::from(i.move_right) - want.1).abs() < 1e-6,
            "tick {}: {:?} for {want:?}",
            a.tick,
            (i.move_forward, i.move_right)
        );
    }
    assert!(derived > 50, "{derived}");
    let r = replay(original, &ReplayOptions::default()).unwrap();
    let tol = CompareTolerances {
        position: 1e-3,
        velocity: 1e-2,
        angle: ANGLE_EPS,
        ..CompareTolerances::default()
    };
    let s = compare_traces("original", original, "replay", &r.trace, &tol);
    assert!(
        matches!(s.verdict, Verdict::Exact | Verdict::WithinTolerance),
        "{}",
        asamu_trace::compare::render_text(&s)
    );
    assert_eq!(s.diff.matched, 121);
    assert_eq!(s.diff.grounded_mismatches, 0);
    assert_eq!(s.diff.grapple_state_mismatches, 0);
    assert_eq!(s.diff.input_mismatches, 0);
    assert!(
        ours.samples.iter().any(|x| !x.grounded)
            && ours
                .samples
                .iter()
                .any(|x| x.position != ours.samples[0].position)
    );
    // With the keys only, the replay never walks.
    let keys = ConvertOptions {
        move_input: asamu_trace::convert::MoveInput::Keys,
        ..ConvertOptions::default()
    };
    let unmoved = &convert(&raw, &keys).unwrap()[0].trace;
    let r = replay(unmoved, &ReplayOptions::default()).unwrap();
    let s = compare_traces("original", unmoved, "replay", &r.trace, &tol);
    assert_eq!(s.verdict, Verdict::Diverged);
}

/// Sprint, a jump with its landing (which raises the air control), a grapple
/// attempt; a rest; then a second walk with a sprint, a jump and steering in
/// the air.
fn two_acts(n: usize) -> Vec<common::Step> {
    (1..=n)
        .map(|i| common::Step {
            forward: i32::from(i <= 70 || (200..260).contains(&i)),
            right: i32::from((240..256).contains(&i)),
            jump: (40..52).contains(&i) || (230..240).contains(&i),
            grapple: (60..90).contains(&i),
            sprint: (5..30).contains(&i) || (215..236).contains(&i),
            ..common::Step::default()
        })
        .collect()
}

/// A replay that starts at a later standing-still tick takes the script
/// state from the recording (`state:` note) and reproduces the rest of the
/// run; without that state it does not.
#[test]
fn a_replay_from_a_later_standing_tick_starts_from_the_recorded_state() {
    let (raw, ours) = common::scripted_steps(&two_acts(330), 9_000, false);
    let original = &convert(&raw, &ConvertOptions::default()).unwrap()[0].trace;
    let samples = original.samples.as_slice();
    assert!(is_clean_start(samples, 0));
    assert!(!is_clean_start(samples, 45), "in the air");
    assert!(is_clean_start(samples, 180), "{:?}", clean_runs(samples));
    assert!(!is_clean_start(samples, 235));
    // The state at the start tick is not the level's start state.
    let timeline = asamu_trace::state::StateTimeline::from_notes(&original.meta.notes)
        .unwrap()
        .unwrap();
    let (at_0, at_180) = (
        timeline.at(0).unwrap().unwrap(),
        timeline.at(180).unwrap().unwrap(),
    );
    assert_eq!(at_0.air_control, Some(0.3));
    assert_eq!(at_180.air_control, Some(0.35), "a landing raised it");
    let tol = CompareTolerances {
        angle: ANGLE_EPS,
        ..CompareTolerances::default()
    };
    let from_180 = |use_init| {
        replay(
            original,
            &ReplayOptions {
                start_tick: Some(180),
                use_init,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    let r = from_180(true);
    assert_eq!(r.trace.samples.len(), 151);
    let s = compare_traces("original", original, "replay", &r.trace, &tol);
    assert!(
        matches!(s.verdict, Verdict::Exact | Verdict::WithinTolerance),
        "{}",
        asamu_trace::compare::render_text(&s)
    );
    assert_eq!(s.diff.matched, 151);
    assert_eq!(s.diff.position.max, 0.0);
    assert_eq!(s.diff.velocity.max, 0.0);
    // Our own trace of the whole run agrees with it too.
    for (a, b) in r.trace.samples.iter().zip(&ours.samples[180..]) {
        assert_eq!(a.position, b.position, "tick {}", a.tick);
    }
    assert!(
        r.trace.samples.iter().any(|x| !x.grounded),
        "the second jump"
    );
    // Without the recorded state the second act runs with the level-start
    // air control and goes elsewhere.
    let fresh = from_180(false);
    let s = compare_traces("original", original, "fresh", &fresh.trace, &tol);
    assert_eq!(s.verdict, Verdict::Diverged);
    // A start in the air is refused, moved or forced.
    let at = |start| {
        replay(
            original,
            &ReplayOptions {
                start_tick: Some(45),
                start,
                ..ReplayOptions::default()
            },
        )
    };
    let e = at(StartPolicy::Refuse).unwrap_err().to_string();
    assert!(e.contains("the pawn is in the air"), "{e}");
    let snapped = at(StartPolicy::Snap).unwrap();
    assert!(is_clean_start(samples, snapped.start_tick as usize));
    assert!(snapped.start_tick > 45);
    assert_eq!(at(StartPolicy::Force).unwrap().start_tick, 45);
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_asamu-trace"))
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn write_raw(raw: &RawFile, path: &Path) {
    std::fs::write(path, raw.to_jsonl_string().unwrap()).unwrap();
}

#[test]
fn command_line_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (mut raw, _) = common::scripted_recording(90, 1_000);
    // A two-frame gap after record 60 makes two segments.
    for r in raw.records.iter_mut().skip(61) {
        r.frame += 2;
    }
    let raw_path = d.join("walk.raw.jsonl");
    write_raw(&raw, &raw_path);

    let listed = run_ok(bin().args(["convert", "--list"]).arg(&raw_path));
    assert!(
        listed.contains("segment 0: frames 1000..=1060 (61 samples"),
        "{listed}"
    );
    assert!(
        listed.contains("segment 1: frames 1063..=1092 (30 samples"),
        "{listed}"
    );
    let out = run_ok(bin().arg("convert").arg(&raw_path));
    assert!(out.contains("walk.seg0.trace.jsonl") && out.contains("walk.seg1.trace.jsonl"));
    let seg0 = d.join("walk.seg0.trace.jsonl");
    let single = d.join("single.trace.jsonl");
    run_ok(
        bin()
            .args(["convert", "--segment", "0", "--out"])
            .arg(&single)
            .arg(&raw_path),
    );
    assert_eq!(
        std::fs::read_to_string(&seg0).unwrap(),
        std::fs::read_to_string(&single).unwrap()
    );
    // Without --segment, --out refuses two segments.
    let refused = bin()
        .args(["convert", "--out"])
        .arg(&single)
        .arg(&raw_path)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));

    let ours = d.join("walk.replay.jsonl");
    let summary = d.join("walk.summary.json");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&seg0)
            .arg("--out")
            .arg(&ours)
            .args(["--compare", "--tol-angle", "1e-6", "--json"])
            .arg(&summary),
    );
    assert!(text.contains("61 samples"), "{text}");
    assert!(text.contains("verdict: "), "{text}");
    let s: CompareSummary =
        serde_json::from_str(&std::fs::read_to_string(&summary).unwrap()).unwrap();
    assert!(matches!(
        s.verdict,
        Verdict::Exact | Verdict::WithinTolerance
    ));
    assert_eq!(s.a.name, "walk.seg0.trace.jsonl");

    let s2 = d.join("walk.compare.json");
    run_ok(
        bin()
            .arg("compare")
            .arg(&seg0)
            .arg(&ours)
            .args(["--tol-angle", "1e-6", "--fail-on-divergence", "--json"])
            .arg(&s2),
    );
    // Strict tolerances may flag the last-bit yaw difference; a bent trace
    // always fails.
    let mut bent: Trace = asamu_trace::read_trace(&seg0).unwrap();
    bent.samples[30].velocity.z += 50.0;
    let bent_path = d.join("bent.trace.jsonl");
    asamu_trace::write_trace(&bent, &bent_path).unwrap();
    let failed = bin()
        .arg("compare")
        .arg(&bent_path)
        .arg(&ours)
        .args(["--tol-angle", "1e-6", "--fail-on-divergence"])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&failed.stdout)
            .contains("first divergence: tick 30 field velocity")
    );

    let md_path = d.join("parity.md");
    run_ok(
        bin()
            .arg("report")
            .arg(&summary)
            .arg(&s2)
            .arg("--out")
            .arg(&md_path),
    );
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        md.contains("| walk.summary | graybox | Original → Runtime | 61 |"),
        "{md}"
    );
    assert!(md.contains("| walk.compare |"), "{md}");

    let v = run_ok(bin().arg("validate").arg(&raw_path).arg(&seg0).arg(&ours));
    assert!(v.contains("raw v1"), "{v}");
    assert!(v.contains("2 convertible segment(s)"), "{v}");
    assert!(v.contains("source Original"), "{v}");
    std::fs::write(d.join("broken.jsonl"), "{\"format\":\"asamu-trace\"}\n").unwrap();
    let bad = bin()
        .arg("validate")
        .arg(d.join("broken.jsonl"))
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn command_line_refuses_bad_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (raw, _) = common::scripted_recording(20, 1);
    let raw_path = d.join("r.raw.jsonl");
    write_raw(&raw, &raw_path);
    let trace = d.join("r.trace.jsonl");
    run_ok(bin().arg("convert").arg(&raw_path).arg("--out").arg(&trace));
    let code = |cmd: &mut Command| cmd.output().unwrap().status.code();

    // Tolerances must be finite and non-negative (the summary JSON cannot
    // hold an infinity).
    for bad in ["inf", "NaN", "-1", "x"] {
        assert_eq!(
            code(
                bin()
                    .arg("compare")
                    .arg(&trace)
                    .arg(&trace)
                    .arg(format!("--tol-position={bad}"))
            ),
            Some(2),
            "{bad}"
        );
    }
    let summary = d.join("s.json");
    run_ok(
        bin()
            .arg("compare")
            .arg(&trace)
            .arg(&trace)
            .args(["--tol-velocity", "1e30", "--json"])
            .arg(&summary),
    );
    // A summary of another version is refused by report.
    let text = std::fs::read_to_string(&summary).unwrap();
    let v2 = d.join("v2.json");
    std::fs::write(&v2, text.replacen("\"version\": 1", "\"version\": 2", 1)).unwrap();
    assert_eq!(code(bin().arg("report").arg(&v2)), Some(2));
    run_ok(bin().arg("report").arg(&summary));
    // A segment that does not exist, a missing file, a replay start tick
    // that does not exist.
    assert_eq!(
        code(bin().arg("convert").arg(&raw_path).args(["--segment", "3"])),
        Some(2)
    );
    assert_eq!(
        code(bin().arg("convert").arg(d.join("missing.raw.jsonl"))),
        Some(2)
    );
    assert_eq!(
        code(
            bin()
                .arg("replay")
                .arg(&trace)
                .arg("--out")
                .arg(d.join("o.jsonl"))
                .args(["--from-tick", "999"])
        ),
        Some(2)
    );
}

#[test]
fn check_recorder_command() {
    let root = common::repo_root();
    let out = bin()
        .arg("check-recorder")
        .arg("--repo")
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains(" 0 failed"), "{text}");
}
