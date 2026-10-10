//! Variable frame lengths end to end: a synthetic recording made **without a
//! fixed time step** (every frame its own `DeltaSeconds`, as the original
//! runs without benchmark mode) → convert → replay → compare → report,
//! through the library and through the command-line tool.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::process::Command;

use asamu_player::Trace;
use asamu_player::trace::CompareTolerances;
use asamu_trace::compare::{CompareSummary, Verdict, compare_traces, render_text};
use asamu_trace::convert::{
    ConvertOptions, ORIGINAL_MAX_FRAME, ORIGINAL_MIN_FRAME, convert, original_frame_length,
    raw_timing,
};
use asamu_trace::raw::RawFile;
use asamu_trace::replay::{ReplayOptions, Stepping, replay};
use asamu_trace::timestep::{StepStats, frame_lengths};

/// The converted trace's yaw is `units × 2π/65536`; our simulation adds the
/// same look deltas in `f32` and wraps, which can differ in the last bit.
const ANGLE_EPS: f64 = 1e-6;

fn tolerances() -> CompareTolerances {
    CompareTolerances {
        angle: ANGLE_EPS,
        ..CompareTolerances::default()
    }
}

#[test]
fn convert_keeps_every_frame_length_exactly() {
    let lengths = common::jittery_lengths(1, 200, 60.0);
    let (raw, ours) = common::scripted_recording_variable(&lengths, 40_000);
    let segs = convert(&raw, &ConvertOptions::default()).unwrap();
    assert_eq!(segs.len(), 1);
    let t = &segs[0].trace;
    assert_eq!(t.meta.tick_rate, None, "no fixed rate");
    assert_eq!(t.samples.len(), 201);
    // time = the f64 sum of the f32 DeltaSeconds values, sample by sample.
    let mut time = 0.0_f64;
    assert_eq!(t.samples[0].time, 0.0);
    for (s, d) in t.samples.iter().skip(1).zip(&lengths) {
        time += f64::from(*d);
        assert_eq!(s.time.to_bits(), time.to_bits(), "tick {}", s.tick);
    }
    // ... from which every length comes back bit for bit, also after the
    // trace went through its file format.
    let reread = Trace::from_jsonl_str(&t.to_jsonl_string().unwrap()).unwrap();
    assert_eq!(&reread, t);
    let back: Vec<f32> = frame_lengths(&reread.samples)
        .map(|(_, d)| d as f32)
        .collect();
    assert_eq!(back.len(), lengths.len());
    for (i, (a, b)) in back.iter().zip(&lengths).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "frame {i}");
    }
    // The inputs and states are those of our run.
    for (a, b) in t.samples.iter().zip(&ours.samples) {
        assert_eq!(a.input, b.input, "tick {}", a.tick);
        assert_eq!(a.position, b.position, "tick {}", a.tick);
        assert_eq!(a.time.to_bits(), b.time.to_bits(), "tick {}", a.tick);
    }
    let st = StepStats::of_samples(&t.samples);
    assert_eq!(st.steps, 200);
    assert!(!st.uniform());
    assert!(st.min > 0.015 && st.max < 0.06, "{st:?}");

    // The raw timing: every frame's tick argument is the next record's
    // DeltaSeconds (dilation 1, nothing clamped).
    let rt = raw_timing(&raw);
    assert_eq!(rt.lengths.steps, 200);
    assert_eq!(
        (rt.arg_checked, rt.arg_equal, rt.arg_clamped),
        (200, 200, 0)
    );

    // One frame of another length is enough to lose the fixed rate; all
    // equal keeps it.
    let (mut fixed, _) = common::scripted_recording(50, 7);
    let t = &convert(&fixed, &ConvertOptions::default()).unwrap()[0].trace;
    assert_eq!(t.meta.tick_rate, Some(60.0));
    fixed.records[30].world.as_mut().unwrap().delta_seconds = 0.016_666_7;
    let t = &convert(&fixed, &ConvertOptions::default()).unwrap()[0].trace;
    assert_eq!(t.meta.tick_rate, None);
    assert_eq!(t.samples.len(), 51);
    let rt = raw_timing(&fixed);
    assert_eq!((rt.arg_checked, rt.arg_equal), (50, 49));
}

#[test]
fn engine_frame_clamp_and_time_dilation_in_the_raw_check() {
    assert_eq!(original_frame_length(1.0 / 60.0, 1.0), 1.0 / 60.0);
    assert_eq!(original_frame_length(1.0 / 60.0, 0.5), 1.0 / 120.0);
    assert_eq!(original_frame_length(2.0, 1.0), ORIGINAL_MAX_FRAME);
    assert_eq!(original_frame_length(0.3, 2.0), ORIGINAL_MAX_FRAME);
    assert_eq!(original_frame_length(1e-5, 1.0), ORIGINAL_MIN_FRAME);
    assert_eq!(original_frame_length(0.0, 1.0), ORIGINAL_MIN_FRAME);
    assert_eq!(original_frame_length(0.4, 1.0), 0.4);
    // A recording with slow motion and a hitch: DeltaSeconds is the dilated,
    // clamped length; conversion uses it.
    let lengths = common::jittery_lengths(4, 40, 60.0);
    let (mut raw, _) = common::scripted_recording_variable(&lengths, 1);
    for r in &mut raw.records[10..20] {
        r.world.as_mut().unwrap().time_dilation = 0.5;
    }
    for i in 10..20 {
        let arg = raw.records[i].dt_arg.unwrap();
        raw.records[i + 1].world.as_mut().unwrap().delta_seconds = arg * 0.5;
    }
    raw.records[30].dt_arg = Some(1.7);
    raw.records[31].world.as_mut().unwrap().delta_seconds = 0.4;
    let rt = raw_timing(&raw);
    assert_eq!((rt.arg_checked, rt.arg_equal, rt.arg_clamped), (40, 40, 1));
    assert_eq!(rt.lengths.max, f64::from(0.4_f32));
    let t = &convert(&raw, &ConvertOptions::default()).unwrap()[0].trace;
    assert_eq!(
        (t.samples[31].time - t.samples[30].time) as f32,
        0.4,
        "the clamped length is the tick's length"
    );
    assert_eq!(
        (t.samples[15].time - t.samples[14].time) as f32,
        lengths[14] * 0.5
    );
    // A build that steps differently shows up as a mismatch.
    raw.records[5].world.as_mut().unwrap().delta_seconds *= 1.5;
    assert_eq!(raw_timing(&raw).arg_equal, 39);
}

#[test]
fn variable_recording_replays_exactly_with_its_own_frame_lengths() {
    let lengths = common::jittery_lengths(2, 120, 60.0);
    let (raw, _) = common::scripted_recording_variable(&lengths, 70_000);
    let segs = convert(&raw, &ConvertOptions::default()).unwrap();
    let original = &segs[0].trace;
    assert_eq!(original.meta.tick_rate, None);
    assert!(
        original.samples.iter().any(|s| !s.grounded),
        "the script jumps"
    );

    let r = replay(original, &ReplayOptions::default()).unwrap();
    assert_eq!(r.stepping, Stepping::PerSample);
    assert_eq!(r.stopped_at_gap, None);
    assert_eq!(r.trace.meta.tick_rate, None);
    let var = r.variable.unwrap();
    assert_eq!((var.lengths.steps, var.clamped, var.no_op), (120, 0, 0));
    let s = compare_traces("original", original, "replay", &r.trace, &tolerances());
    assert!(
        matches!(s.verdict, Verdict::Exact | Verdict::WithinTolerance),
        "{}",
        render_text(&s)
    );
    assert_eq!(s.diff.matched, 121);
    assert_eq!(s.diff.position.max, 0.0);
    assert_eq!(s.diff.velocity.max, 0.0);
    assert_eq!(s.diff.grounded_mismatches, 0);
    assert_eq!(s.diff.grapple_state_mismatches, 0);
    assert_eq!(s.diff.input_mismatches, 0);
    assert!(s.diff.yaw.max <= ANGLE_EPS);
    let timing = s.timing.unwrap();
    assert!(timing.aligned());
    assert_eq!(timing.compared, 120);
    assert_eq!(
        timing.max_time_skew, 0.0,
        "our samples carry the input's times"
    );
    for (a, b) in original.samples.iter().zip(&r.trace.samples) {
        assert_eq!(a.time.to_bits(), b.time.to_bits());
    }

    // Deterministic.
    let again = replay(original, &ReplayOptions::default()).unwrap();
    assert_eq!(
        r.trace.to_jsonl_string().unwrap(),
        again.trace.to_jsonl_string().unwrap()
    );

    // Replayed with fixed 60 Hz ticks instead, the same inputs give another
    // run, and the comparison says the two were not stepped alike.
    let fixed = replay(
        original,
        &ReplayOptions {
            tick_rate: Some(60.0),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert_eq!(fixed.stepping, Stepping::Fixed(60.0));
    let s = compare_traces("original", original, "fixed", &fixed.trace, &tolerances());
    assert_eq!(s.verdict, Verdict::Diverged);
    assert!(s.diff.position.max > 0.01, "{}", s.diff.position.max);
    let timing = s.timing.unwrap();
    assert!(!timing.aligned());
    assert_eq!(timing.compared, 120);
    assert!(timing.mismatches > 100, "{timing:?}");
    assert!(render_text(&s).contains("warning: frame lengths differ"));
}

#[test]
fn hitches_longer_than_our_step_bound_are_reported() {
    let mut lengths = common::jittery_lengths(5, 90, 60.0);
    // The original clamps frames to 0.4 s; ours to 0.25 s.
    lengths[40] = 0.4;
    lengths[41] = 0.3;
    let (raw, _) = common::scripted_recording_variable(&lengths, 1);
    let original = &convert(&raw, &ConvertOptions::default()).unwrap()[0].trace;
    let r = replay(original, &ReplayOptions::default()).unwrap();
    let var = r.variable.unwrap();
    assert_eq!((var.clamped, var.first_clamped), (2, Some(41)));
    assert_eq!(var.lengths.max, f64::from(0.4_f32));
    assert!(
        r.trace
            .meta
            .notes
            .iter()
            .any(|n| n.starts_with("warning: 2 tick(s) longer than 0.25 s")),
        "{:#?}",
        r.trace.meta.notes
    );
    // The fake original ran on the same simulation, so it still matches.
    let s = compare_traces("original", original, "replay", &r.trace, &tolerances());
    assert_eq!(s.diff.position.max, 0.0);
    assert!(s.timing.unwrap().aligned());
}

/// Frames of equal length: the per-sample replay is the fixed tick's run,
/// sample for sample, at every rate (the per-sample path takes its `dt` from
/// the differences of the sample times, the fixed path from the clock), from
/// level start and from a tick in the middle, for a runtime trace and for a
/// converted fixed-step recording.
#[test]
fn equal_frame_lengths_replay_exactly_like_fixed_ticks_at_any_rate() {
    use asamu_game::Game;
    use asamu_player::{PlayerParams, TraceSample};

    let untimed = |t: &Trace| -> Vec<TraceSample> {
        t.samples
            .iter()
            .map(|s| TraceSample { time: 0.0, ..*s })
            .collect()
    };
    let steps = common::script(240);
    for rate in [24.0_f64, 30.0, 50.0, 60.0, 62.0, 75.0, 120.0, 144.0, 240.0] {
        let level = Game::graybox().unwrap().level().clone();
        let mut g = Game::new(level, PlayerParams::asamu_original(), rate).unwrap();
        g.start();
        g.start_recording();
        for (i, s) in steps.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| &steps[j]);
            g.tick(&common::input(s, prev)).unwrap();
        }
        let ours = g.stop_recording().unwrap();
        assert_eq!(ours.meta.tick_rate, Some(rate as f32));
        assert!(
            ours.samples.iter().any(|s| !s.grounded),
            "{rate} Hz: the script jumps"
        );
        let fixed = replay(&ours, &ReplayOptions::default()).unwrap();
        assert_eq!(fixed.stepping, Stepping::Fixed(rate));
        assert_eq!(fixed.trace.samples, ours.samples, "{rate} Hz: fixed replay");
        for (start_tick, max_ticks) in [(None, None), (Some(100), Some(90))] {
            let opts = |variable_dt| ReplayOptions {
                variable_dt,
                start_tick,
                max_ticks,
                ..ReplayOptions::default()
            };
            let a = replay(&ours, &opts(false)).unwrap();
            let b = replay(&ours, &opts(true)).unwrap();
            assert_eq!(b.stepping, Stepping::PerSample);
            let var = b.variable.unwrap();
            assert_eq!((var.clamped, var.no_op), (0, 0), "{rate} Hz");
            assert!(var.lengths.uniform(), "{rate} Hz: {var:?}");
            assert_eq!(
                untimed(&a.trace),
                untimed(&b.trace),
                "{rate} Hz from {start_tick:?}: per-sample replay differs from the fixed one"
            );
            let s = compare_traces(
                "fixed",
                &a.trace,
                "per-sample",
                &b.trace,
                &CompareTolerances::default(),
            );
            assert_eq!(s.verdict, Verdict::Exact, "{rate} Hz: {}", render_text(&s));
            assert!(s.timing.unwrap().aligned(), "{rate} Hz");
        }
        // The same with the rate removed from the trace: per-sample by default.
        let mut v = ours.clone();
        v.meta.tick_rate = None;
        let own = replay(&v, &ReplayOptions::default()).unwrap();
        assert_eq!(own.stepping, Stepping::PerSample);
        assert_eq!(untimed(&own.trace), untimed(&ours), "{rate} Hz: no rate");
    }

    // A converted fixed-step recording (benchmark mode at 60 Hz): its sample
    // times are sums of the f32 step, not tick / rate.
    let (raw, _) = common::scripted_recording(200, 3_000);
    let original = &convert(&raw, &ConvertOptions::default()).unwrap()[0].trace;
    assert_eq!(original.meta.tick_rate, Some(60.0));
    let fixed = replay(original, &ReplayOptions::default()).unwrap();
    let per_sample = replay(
        original,
        &ReplayOptions {
            variable_dt: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert_eq!(untimed(&fixed.trace), untimed(&per_sample.trace));
    for (a, b) in frame_lengths(&per_sample.trace.samples).zip(frame_lengths(&original.samples)) {
        assert_eq!(a, b, "our samples carry the input's times");
        assert_eq!((a.1 as f32).to_bits(), (1.0_f32 / 60.0).to_bits());
    }
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

fn summary(path: &Path) -> CompareSummary {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn command_line_variable_rate_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let lengths = common::jittery_lengths(9, 90, 60.0);
    let (raw, _) = common::scripted_recording_variable(&lengths, 2_000);
    let raw_path = d.join("win.raw.jsonl");
    write_raw(&raw, &raw_path);

    let listed = run_ok(bin().args(["convert", "--list"]).arg(&raw_path));
    assert!(
        listed.contains("segment 0: frames 2000..=2090 (91 samples"),
        "{listed}"
    );
    assert!(
        listed.contains("tick rate None, frame lengths 0.0"),
        "{listed}"
    );
    run_ok(bin().arg("convert").arg(&raw_path));
    let trace = d.join("win.trace.jsonl");
    let v = run_ok(bin().arg("validate").arg(&raw_path).arg(&trace));
    assert!(
        v.contains("benchmarking Some(false), frame lengths 0.0"),
        "{v}"
    );
    assert!(
        v.contains("DeltaSeconds = clamp(tick argument x TimeDilation, 0.0005, 0.4) on 90 of 90 frames (0 clamped)"),
        "{v}"
    );
    assert!(v.contains("tick rate None, 91 samples"), "{v}");
    assert!(v.contains("0 gap(s), frame lengths 0.0"), "{v}");

    // No --tick-rate needed: the trace's own frame lengths are used.
    let ours = d.join("win.replay.jsonl");
    let own_summary = d.join("win.summary.json");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&trace)
            .arg("--out")
            .arg(&ours)
            .args(["--compare", "--tol-angle", "1e-6", "--json"])
            .arg(&own_summary),
    );
    assert!(
        text.contains("91 samples, per-sample frame lengths 0.0"),
        "{text}"
    );
    assert!(
        text.contains("frame lengths agree on 90 compared tick(s)"),
        "{text}"
    );
    let s = summary(&own_summary);
    assert!(matches!(
        s.verdict,
        Verdict::Exact | Verdict::WithinTolerance
    ));
    assert_eq!(s.diff.position.max, 0.0);
    assert_eq!(s.b.tick_rate, None);
    assert!(s.timing.unwrap().aligned());
    // Byte-identical when run again.
    let ours2 = d.join("win.replay2.jsonl");
    run_ok(bin().arg("replay").arg(&trace).arg("--out").arg(&ours2));
    assert_eq!(
        std::fs::read(&ours).unwrap(),
        std::fs::read(&ours2).unwrap()
    );

    // --tick-rate still forces fixed ticks; the comparison flags the step.
    let fixed = d.join("win.fixed60.jsonl");
    let fixed_summary = d.join("win.fixed60.json");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&trace)
            .arg("--out")
            .arg(&fixed)
            .args(["--tick-rate", "60", "--compare", "--tol-position", "1e30"])
            .args(["--tol-velocity", "1e30", "--tol-angle", "1e30", "--json"])
            .arg(&fixed_summary),
    );
    assert!(text.contains("91 samples, fixed 60 Hz ticks"), "{text}");
    assert!(text.contains("warning: frame lengths differ on"), "{text}");
    let s = summary(&fixed_summary);
    assert_eq!(s.b.tick_rate, Some(60.0));
    assert!(!s.timing.unwrap().aligned());

    // Both at once is a usage error; --kismet needs --converted as before.
    let refused = bin()
        .arg("replay")
        .arg(&trace)
        .arg("--out")
        .arg(d.join("x.jsonl"))
        .args(["--tick-rate", "60", "--variable-dt"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));

    // The report names the time step of each comparison.
    let md_path = d.join("parity.md");
    run_ok(
        bin()
            .arg("report")
            .arg(&own_summary)
            .arg(&fixed_summary)
            .arg("--out")
            .arg(&md_path),
    );
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.contains("| Verdict | Time step |"), "{md}");
    let row = |name: &str| {
        md.lines()
            .find(|l| l.starts_with(&format!("| {name} |")))
            .unwrap()
    };
    assert!(row("win.summary").contains("| variable 0.0"), "{md}");
    assert!(!row("win.summary").contains("differs"), "{md}");
    assert!(
        row("win.fixed60").contains("| **differs**: a variable 0.0"),
        "{md}"
    );
    assert!(row("win.fixed60").ends_with("b 60 Hz |"), "{md}");
    assert!(
        md.contains("Frame lengths differ between the two traces, so these errors include the step difference: win.fixed60 ("),
        "{md}"
    );
}

#[test]
fn command_line_fixed_rate_trace_with_variable_dt() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (raw, _) = common::scripted_recording(100, 1);
    let raw_path = d.join("bench.raw.jsonl");
    write_raw(&raw, &raw_path);
    let trace = d.join("bench.trace.jsonl");
    run_ok(bin().arg("convert").arg(&raw_path).arg("--out").arg(&trace));
    let fixed = d.join("fixed.jsonl");
    let per_sample = d.join("per-sample.jsonl");
    let text = run_ok(bin().arg("replay").arg(&trace).arg("--out").arg(&fixed));
    assert!(text.contains("101 samples, fixed 60 Hz ticks"), "{text}");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&trace)
            .arg("--out")
            .arg(&per_sample)
            .arg("--variable-dt"),
    );
    assert!(
        text.contains("101 samples, per-sample frame lengths 0.016667 s"),
        "{text}"
    );
    // The same run: a fixed-rate recording's frame lengths are the fixed
    // step. (Sample times differ in the last bits: tick / 60 against the sum
    // of the f32 step; compare ignores time.)
    let a = asamu_trace::read_trace(&fixed).unwrap();
    let b = asamu_trace::read_trace(&per_sample).unwrap();
    assert_eq!(a.meta.tick_rate, Some(60.0));
    assert_eq!(b.meta.tick_rate, None);
    let s = compare_traces("fixed", &a, "per-sample", &b, &CompareTolerances::default());
    assert_eq!(s.verdict, Verdict::Exact, "{}", render_text(&s));
    let timing = s.timing.unwrap();
    assert!(timing.aligned());
    assert!(timing.max_time_skew < 1e-6);
}
