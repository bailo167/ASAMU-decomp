//! Trace comparison with tolerances and a machine-readable summary.
//!
//! Wraps [`asamu_player::trace::compare_with`] (per-field max/mean/RMS, the
//! first divergence over all fields, unmatched ticks) and adds the first tick
//! at which **each** field exceeded its tolerance, the input traces'
//! identities and a verdict. The summary is JSON (`asamu-trace-compare` v1)
//! so [`crate::report`] can turn several comparisons into a markdown table.
//! Tolerances are an analysis choice per study, not a property of the game.
//!
//! # Time
//!
//! Traces are aligned by **tick index**, never by time: sample `k` of both
//! is the state after the same input, whatever the frame lengths were. That
//! is also right for variable-rate traces (`tick_rate: null`), as long as
//! both sides stepped tick `k` with the same frame length — which a
//! per-sample replay does and a fixed-rate replay of a variable-rate
//! recording does not. [`TimingSummary`] checks it from the sample times:
//! the frame lengths of both traces over the ticks they share, the ticks
//! where the two lengths differ ([`crate::timestep::same_length`]) and the
//! largest difference in elapsed time. A mismatch does not change the
//! verdict (the fields still are what they are); the text and the report
//! flag it, because per-tick errors between runs with different frame
//! lengths measure the step difference, not the simulation.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use asamu_player::Trace;
use asamu_player::trace::{CompareTolerances, TraceDiff, TraceField, TraceSource, compare_with};
use serde::{Deserialize, Serialize};

use crate::timestep::{StepStats, finite, same_length};

/// Value of [`CompareSummary::format`].
pub const SUMMARY_FORMAT: &str = "asamu-trace-compare";
/// Version of the summary format.
pub const SUMMARY_VERSION: u32 = 1;

/// Identity of one compared trace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceInfo {
    /// Display name (usually the file name).
    pub name: String,
    /// Producer.
    pub source: TraceSource,
    /// Level.
    pub level: Option<String>,
    /// Game build.
    pub game_build: Option<String>,
    /// Tick rate, Hz.
    pub tick_rate: Option<f32>,
    /// Sample count.
    pub samples: usize,
    /// First tick.
    pub first_tick: Option<u64>,
    /// Last tick.
    pub last_tick: Option<u64>,
}

impl TraceInfo {
    /// Identity of `trace`, shown as `name`.
    #[must_use]
    pub fn of(name: &str, trace: &Trace) -> Self {
        Self {
            name: name.to_owned(),
            source: trace.meta.source,
            level: trace.meta.level.clone(),
            game_build: trace.meta.game_build.clone(),
            tick_rate: trace.meta.tick_rate,
            samples: trace.samples.len(),
            first_tick: trace.samples.first().map(|s| s.tick),
            last_tick: trace.samples.last().map(|s| s.tick),
        }
    }
}

/// First tick at which one field exceeded its tolerance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Exceedance {
    /// Tick.
    pub tick: u64,
    /// Error at that tick (1.0 for flags and categories).
    pub error: f64,
}

/// The first tick both traces stepped with different frame lengths.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepMismatch {
    /// Tick.
    pub tick: u64,
    /// Frame length of the tick in the reference trace, seconds.
    pub a: f64,
    /// Frame length of the tick in the trace under test, seconds.
    pub b: f64,
}

/// Frame lengths of two compared traces (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimingSummary {
    /// Frame lengths of the reference trace (all its consecutive ticks).
    pub a: StepStats,
    /// Frame lengths of the trace under test.
    pub b: StepStats,
    /// Ticks whose frame length is known in both traces (the tick and the
    /// one before it are in both).
    pub compared: usize,
    /// Of those, the ticks with different lengths.
    pub mismatches: usize,
    /// The first of them.
    pub first_mismatch: Option<StepMismatch>,
    /// Largest difference in elapsed time over the matched ticks, seconds
    /// (elapsed = time since the first matched tick, in each trace).
    pub max_time_skew: f64,
    /// The tick of that largest difference.
    pub max_time_skew_tick: Option<u64>,
}

impl TimingSummary {
    /// `true` when no compared tick had different frame lengths.
    #[must_use]
    pub fn aligned(&self) -> bool {
        self.mismatches == 0
    }
}

/// Overall outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every matched tick agrees exactly and no tick is unmatched.
    Exact,
    /// No field exceeded its tolerance on any matched tick.
    WithinTolerance,
    /// Some field exceeded its tolerance.
    Diverged,
    /// The traces share no tick.
    NoOverlap,
}

/// The result of [`compare_traces`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareSummary {
    /// Always [`SUMMARY_FORMAT`].
    pub format: String,
    /// Always [`SUMMARY_VERSION`].
    pub version: u32,
    /// The reference trace (usually the original).
    pub a: TraceInfo,
    /// The trace under test (usually our replay).
    pub b: TraceInfo,
    /// Tolerances used.
    pub tolerances: CompareTolerances,
    /// Statistics from [`compare_with`].
    pub diff: TraceDiff,
    /// First exceedance per field (`position`, `velocity`, `yaw`, `pitch`,
    /// `fov`, `grapple_anchor`, `rope_length`, `grapple_state`, `grounded`,
    /// `input`); fields that never exceeded are absent.
    pub first_exceedance: BTreeMap<String, Exceedance>,
    /// Verdict.
    pub verdict: Verdict,
    /// Frame lengths of both traces and whether they agree (absent in
    /// summaries written before it existed).
    #[serde(default)]
    pub timing: Option<TimingSummary>,
}

fn field_name(f: TraceField) -> &'static str {
    match f {
        TraceField::Position => "position",
        TraceField::Velocity => "velocity",
        TraceField::Yaw => "yaw",
        TraceField::Pitch => "pitch",
        TraceField::Fov => "fov",
        TraceField::GrappleAnchor => "grapple_anchor",
        TraceField::RopeLength => "rope_length",
        TraceField::GrappleState => "grapple_state",
        TraceField::Grounded => "grounded",
        TraceField::Input => "input",
    }
}

fn vec_err(a: asamu_core::glam::Vec3, b: asamu_core::glam::Vec3) -> f64 {
    (a.as_dvec3() - b.as_dvec3()).length()
}

fn angle_err(a: f32, b: f32) -> f64 {
    let d = (f64::from(a) - f64::from(b)).rem_euclid(core::f64::consts::TAU);
    d.min(core::f64::consts::TAU - d)
}

/// Compares `a` (reference) with `b` under `tol`.
#[must_use]
pub fn compare_traces(
    a_name: &str,
    a: &Trace,
    b_name: &str,
    b: &Trace,
    tol: &CompareTolerances,
) -> CompareSummary {
    let diff = compare_with(a, b, tol);
    let mut first: BTreeMap<String, Exceedance> = BTreeMap::new();
    let mut note = |field: TraceField, tick: u64, err: f64, limit: f64| {
        if err > limit.max(0.0) {
            first
                .entry(field_name(field).to_owned())
                .or_insert(Exceedance { tick, error: err });
        }
    };
    let mut timing = TimingSummary {
        a: StepStats::of_samples(&a.samples),
        b: StepStats::of_samples(&b.samples),
        compared: 0,
        mismatches: 0,
        first_mismatch: None,
        max_time_skew: 0.0,
        max_time_skew_tick: None,
    };
    // Times of the first and of the previous matched tick, in both traces.
    let mut first_matched: Option<(f64, f64)> = None;
    let mut prev_matched: Option<(u64, f64, f64)> = None;
    let (mut i, mut j) = (0, 0);
    while i < a.samples.len() && j < b.samples.len() {
        let (sa, sb) = (&a.samples[i], &b.samples[j]);
        if sa.tick < sb.tick {
            i += 1;
            continue;
        }
        if sb.tick < sa.tick {
            j += 1;
            continue;
        }
        let t = sa.tick;
        let (a0, b0) = *first_matched.get_or_insert((sa.time, sb.time));
        let skew = finite(((sa.time - a0) - (sb.time - b0)).abs());
        if skew > timing.max_time_skew {
            timing.max_time_skew = skew;
            timing.max_time_skew_tick = Some(t);
        }
        if let Some((pt, pa, pb)) = prev_matched
            && pt.checked_add(1) == Some(t)
        {
            let (da, db) = (finite(sa.time - pa), finite(sb.time - pb));
            timing.compared += 1;
            if !same_length(da, db) {
                timing.mismatches += 1;
                timing.first_mismatch.get_or_insert(StepMismatch {
                    tick: t,
                    a: da,
                    b: db,
                });
            }
        }
        prev_matched = Some((t, sa.time, sb.time));
        note(
            TraceField::Position,
            t,
            vec_err(sa.position, sb.position),
            tol.position,
        );
        note(
            TraceField::Velocity,
            t,
            vec_err(sa.velocity, sb.velocity),
            tol.velocity,
        );
        note(TraceField::Yaw, t, angle_err(sa.yaw, sb.yaw), tol.angle);
        note(
            TraceField::Pitch,
            t,
            (f64::from(sa.pitch) - f64::from(sb.pitch)).abs(),
            tol.angle,
        );
        note(
            TraceField::Fov,
            t,
            (f64::from(sa.fov) - f64::from(sb.fov)).abs(),
            tol.fov,
        );
        if sa.grapple_state != sb.grapple_state {
            note(TraceField::GrappleState, t, 1.0, 0.0);
        } else {
            if let (Some(pa), Some(pb)) = (sa.grapple_anchor, sb.grapple_anchor) {
                note(TraceField::GrappleAnchor, t, vec_err(pa, pb), tol.anchor);
            }
            if let (Some(la), Some(lb)) = (sa.rope_length, sb.rope_length) {
                note(
                    TraceField::RopeLength,
                    t,
                    (f64::from(la) - f64::from(lb)).abs(),
                    tol.anchor,
                );
            }
        }
        if sa.grounded != sb.grounded {
            note(TraceField::Grounded, t, 1.0, 0.0);
        }
        if sa.input != sb.input {
            note(TraceField::Input, t, 1.0, 0.0);
        }
        i += 1;
        j += 1;
    }
    let verdict = if diff.matched == 0 && (!a.samples.is_empty() || !b.samples.is_empty()) {
        Verdict::NoOverlap
    } else if diff.is_exact() {
        Verdict::Exact
    } else if diff.first_divergence.is_none() {
        Verdict::WithinTolerance
    } else {
        Verdict::Diverged
    };
    CompareSummary {
        format: SUMMARY_FORMAT.to_owned(),
        version: SUMMARY_VERSION,
        a: TraceInfo::of(a_name, a),
        b: TraceInfo::of(b_name, b),
        tolerances: *tol,
        diff,
        first_exceedance: first,
        verdict,
        timing: Some(timing),
    }
}

/// How a trace steps, for people: its fixed rate, else its frame lengths.
#[must_use]
pub fn describe_steps(info: &TraceInfo, stats: &StepStats) -> String {
    match info.tick_rate {
        Some(rate) => format!("{rate} Hz"),
        None if stats.steps == 0 => "no fixed rate".to_owned(),
        None => format!("variable {}", stats.describe()),
    }
}

/// The `time step:` lines of [`render_text`]: one line when both traces
/// stepped alike, a warning with the first differing tick otherwise.
fn render_timing(o: &mut String, s: &CompareSummary) {
    let Some(t) = &s.timing else {
        return;
    };
    let _ = writeln!(
        o,
        "time step: a {}, b {}",
        describe_steps(&s.a, &t.a),
        describe_steps(&s.b, &t.b)
    );
    if t.aligned() {
        let _ = writeln!(
            o,
            "frame lengths agree on {} compared tick(s); max elapsed-time difference {:.3e} s",
            t.compared, t.max_time_skew
        );
    } else {
        let first = t.first_mismatch.map_or_else(String::new, |m| {
            format!(
                " (first at tick {}: a {:.6} s, b {:.6} s)",
                m.tick, m.a, m.b
            )
        });
        let _ = writeln!(
            o,
            "warning: frame lengths differ on {} of {} compared tick(s){first}; elapsed time \
             differs by up to {:.6} s (tick {}). The traces were not stepped alike, so the \
             per-tick errors include the step difference",
            t.mismatches,
            t.compared,
            t.max_time_skew,
            t.max_time_skew_tick
                .map_or_else(|| "-".to_owned(), |k| k.to_string())
        );
    }
}

/// A human-readable report of a summary.
#[must_use]
pub fn render_text(s: &CompareSummary) -> String {
    let mut o = String::new();
    let d = &s.diff;
    let _ = writeln!(
        o,
        "a: {} ({:?}, {} samples)",
        s.a.name, s.a.source, s.a.samples
    );
    let _ = writeln!(
        o,
        "b: {} ({:?}, {} samples)",
        s.b.name, s.b.source, s.b.samples
    );
    let _ = writeln!(
        o,
        "matched ticks {}, only in a {}, only in b {}",
        d.matched, d.only_in_a, d.only_in_b
    );
    render_timing(&mut o, s);
    let t = &s.tolerances;
    let _ = writeln!(
        o,
        "tolerances: position {} uu, velocity {} uu/s, angle {} rad, fov {} deg, anchor {} uu",
        t.position, t.velocity, t.angle, t.fov, t.anchor
    );
    let _ = writeln!(
        o,
        "{:<15} {:>7} {:>14} {:>10} {:>14} {:>14} {:>15}",
        "field", "count", "max", "max tick", "mean", "rms", "first exceeded"
    );
    for (name, f, unit) in [
        ("position", &d.position, "uu"),
        ("velocity", &d.velocity, "uu/s"),
        ("yaw", &d.yaw, "rad"),
        ("pitch", &d.pitch, "rad"),
        ("fov", &d.fov, "deg"),
        ("grapple_anchor", &d.grapple_anchor, "uu"),
        ("rope_length", &d.rope_length, "uu"),
    ] {
        let first = s
            .first_exceedance
            .get(name)
            .map_or_else(|| "-".to_owned(), |e| e.tick.to_string());
        let max_tick = f.max_tick.map_or_else(|| "-".to_owned(), |t| t.to_string());
        let _ = writeln!(
            o,
            "{:<15} {:>7} {:>14.6} {:>10} {:>14.6} {:>14.6} {:>15}  {unit}",
            name, f.count, f.max, max_tick, f.mean, f.rms, first
        );
    }
    for (name, n) in [
        ("grapple_state", d.grapple_state_mismatches),
        ("grounded", d.grounded_mismatches),
        ("input", d.input_mismatches),
    ] {
        let first = s
            .first_exceedance
            .get(name)
            .map_or_else(|| "-".to_owned(), |e| e.tick.to_string());
        let _ = writeln!(o, "{name:<15} {n:>7} mismatching ticks; first {first}");
    }
    match d.first_divergence {
        Some(fd) => {
            let _ = writeln!(
                o,
                "first divergence: tick {} field {} error {}",
                fd.tick,
                field_name(fd.field),
                fd.error
            );
        }
        None => {
            let _ = writeln!(o, "first divergence: none");
        }
    }
    let _ = writeln!(o, "verdict: {:?}", s.verdict);
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_core::glam::Vec3;
    use asamu_player::trace::TraceGrappleState;
    use asamu_player::{InputFrame, TraceMeta, TraceSample};

    fn trace(n: u64) -> Trace {
        Trace {
            meta: TraceMeta::runtime(Some("t".into()), Some(60.0)),
            samples: (0..n)
                .map(|tick| TraceSample {
                    tick,
                    time: tick as f64 / 60.0,
                    input: InputFrame::default(),
                    position: Vec3::new(tick as f32, 0.0, 45.0),
                    velocity: Vec3::ZERO,
                    yaw: 0.0,
                    pitch: 0.0,
                    fov: 90.0,
                    grapple_state: TraceGrappleState::Idle,
                    grapple_anchor: None,
                    rope_length: None,
                    grounded: true,
                })
                .collect(),
        }
    }

    #[test]
    fn exact_within_and_diverged() {
        let a = trace(10);
        let s = compare_traces("a", &a, "b", &a, &CompareTolerances::default());
        assert_eq!(s.verdict, Verdict::Exact);
        assert!(s.first_exceedance.is_empty());
        assert_eq!(s.a.samples, 10);
        assert_eq!(s.a.last_tick, Some(9));

        let mut b = a.clone();
        b.samples[3].position.y += 0.5;
        b.samples[5].velocity.x += 3.0;
        b.samples[6].velocity.x += 30.0;
        b.samples[7].grounded = false;
        b.samples[8].yaw = 3.0;
        b.samples[8].input.jump_pressed = true;
        let tol = CompareTolerances {
            position: 1.0,
            velocity: 10.0,
            angle: 0.1,
            fov: 0.0,
            anchor: 0.0,
        };
        let s = compare_traces("a", &a, "b", &b, &tol);
        assert_eq!(s.verdict, Verdict::Diverged);
        assert!(!s.first_exceedance.contains_key("position"), "0.5 < 1.0");
        assert_eq!(
            s.first_exceedance["velocity"],
            Exceedance {
                tick: 6,
                error: 30.0
            }
        );
        assert_eq!(s.first_exceedance["grounded"].tick, 7);
        assert_eq!(s.first_exceedance["yaw"].tick, 8);
        assert_eq!(s.first_exceedance["input"].tick, 8);
        assert_eq!(s.diff.first_divergence.map(|d| d.tick), Some(6));
        assert_eq!(s.diff.position.max, 0.5);

        let loose = CompareTolerances {
            position: 1.0,
            velocity: 100.0,
            angle: 4.0,
            fov: 1.0,
            anchor: 1.0,
        };
        let mut c = a.clone();
        c.samples[3].position.y += 0.5;
        let s = compare_traces("a", &a, "c", &c, &loose);
        assert_eq!(s.verdict, Verdict::WithinTolerance);

        let text = render_text(&s);
        assert!(text.contains("verdict: WithinTolerance"), "{text}");
        assert!(text.contains("position"), "{text}");
        let json = serde_json::to_string(&s).unwrap();
        let back: CompareSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.verdict, s.verdict);
        assert_eq!(back.diff.position.max, s.diff.position.max);
    }

    /// Deterministic pseudo-random numbers for the property test.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }

        fn f32(&mut self, scale: f32) -> f32 {
            (self.next() as f32 / u32::MAX as f32 - 0.5) * 2.0 * scale
        }
    }

    #[test]
    fn known_errors_and_angle_wrap() {
        let a = trace(4);
        let mut b = a.clone();
        b.samples[1].position += Vec3::new(3.0, 4.0, 0.0);
        b.samples[2].position += Vec3::new(0.0, 0.0, 12.0);
        b.samples[3].velocity = Vec3::new(1.0, 2.0, 2.0);
        // Yaw across the ±π seam: 0.02 rad apart, not 2π − 0.02.
        let mut a2 = a.clone();
        a2.samples[0].yaw = core::f32::consts::PI - 0.01;
        b.samples[0].yaw = -core::f32::consts::PI + 0.01;
        let s = compare_traces("a", &a2, "b", &b, &CompareTolerances::default());
        let d = &s.diff;
        assert_eq!(d.position.max, 12.0);
        assert_eq!(d.position.max_tick, Some(2));
        assert_eq!(d.position.mean, (0.0 + 5.0 + 12.0 + 0.0) / 4.0);
        assert!((d.position.rms - ((25.0 + 144.0) / 4.0_f64).sqrt()).abs() < 1e-12);
        assert_eq!(d.velocity.max, 3.0);
        assert!((d.yaw.max - 0.02).abs() < 1e-6, "{}", d.yaw.max);
        assert_eq!(s.first_exceedance["position"].tick, 1);
        assert_eq!(s.first_exceedance["position"].error, 5.0);

        // NaN and negative tolerances are strict (as in compare_with).
        for bad in [f64::NAN, -1.0] {
            let tol = CompareTolerances {
                position: bad,
                velocity: 100.0,
                angle: 4.0,
                fov: 1.0,
                anchor: 1.0,
            };
            let s = compare_traces("a", &a, "b", &b, &tol);
            assert_eq!(s.first_exceedance["position"].tick, 1);
            assert_eq!(s.diff.first_divergence.map(|f| f.tick), Some(1));
            assert_eq!(s.verdict, Verdict::Diverged);
        }
    }

    /// The per-field first exceedances agree with `compare_with`: the overall
    /// first divergence is the earliest of them, and each recorded error is
    /// the error at that tick.
    #[test]
    fn first_exceedances_agree_with_compare_with() {
        let mut r = Lcg(42);
        for round in 0..200 {
            let n = 2 + u64::from(r.below(40));
            let a = trace(n);
            let mut b = a.clone();
            if round % 7 == 0 {
                // Misaligned ticks: some only in a, some only in b.
                b.samples.retain(|s| s.tick % 5 != 3);
                for s in &mut b.samples {
                    if s.tick % 11 == 10 {
                        s.tick += 100;
                    }
                }
                b.samples.sort_by_key(|s| s.tick);
                b.samples.dedup_by_key(|s| s.tick);
            }
            for s in &mut b.samples {
                match r.below(9) {
                    0 => s.position.x += r.f32(3.0),
                    1 => s.velocity.z += r.f32(30.0),
                    2 => s.yaw = r.f32(3.1),
                    3 => s.pitch += r.f32(0.2),
                    4 => s.fov = 60.0 + r.f32(20.0),
                    5 => s.grounded = !s.grounded,
                    6 => s.input.jump_held = true,
                    7 => {
                        s.grapple_state = TraceGrappleState::Attached;
                        s.grapple_anchor = Some(Vec3::splat(r.f32(10.0)));
                    }
                    _ => {}
                }
            }
            let tol = CompareTolerances {
                position: f64::from(r.below(3)),
                velocity: f64::from(r.below(30)),
                angle: f64::from(r.below(3)) * 0.1,
                fov: f64::from(r.below(10)),
                anchor: f64::from(r.below(5)),
            };
            let s = compare_traces("a", &a, "b", &b, &tol);
            let earliest = s.first_exceedance.values().map(|e| e.tick).min();
            assert_eq!(
                s.diff.first_divergence.map(|f| f.tick),
                earliest,
                "round {round}"
            );
            if let Some(fd) = s.diff.first_divergence {
                let e = &s.first_exceedance[field_name(fd.field)];
                assert_eq!((e.tick, e.error), (fd.tick, fd.error), "round {round}");
            }
            for (name, stats) in [
                ("position", &s.diff.position),
                ("velocity", &s.diff.velocity),
                ("yaw", &s.diff.yaw),
                ("pitch", &s.diff.pitch),
                ("fov", &s.diff.fov),
            ] {
                if let Some(e) = s.first_exceedance.get(name) {
                    assert!(e.error <= stats.max, "round {round} {name}");
                }
            }
            let consistent = match s.verdict {
                Verdict::Exact => s.diff.is_exact() && s.first_exceedance.is_empty(),
                Verdict::WithinTolerance => s.first_exceedance.is_empty() && !s.diff.is_exact(),
                Verdict::Diverged => !s.first_exceedance.is_empty(),
                Verdict::NoOverlap => s.diff.matched == 0,
            };
            assert!(consistent, "round {round}: {:?}", s.verdict);
        }
    }

    /// `trace(n)` with the given frame lengths (time = their sum) and no
    /// fixed rate.
    fn variable(lengths: &[f64]) -> Trace {
        let mut t = trace(lengths.len() as u64 + 1);
        t.meta.tick_rate = None;
        let mut time = 0.0;
        for (s, d) in t.samples.iter_mut().skip(1).zip(lengths) {
            time += d;
            s.time = time;
        }
        t
    }

    #[test]
    fn timing_of_fixed_and_variable_traces() {
        // Two fixed 60 Hz traces: one with times tick / 60, one with the sum
        // of the f32 step (as a converted recording has it). Same steps.
        let a = trace(600);
        let mut b = a.clone();
        let d60 = f64::from(1.0_f32 / 60.0);
        for s in &mut b.samples {
            s.time = s.tick as f64 * d60;
        }
        let s = compare_traces("a", &a, "b", &b, &CompareTolerances::default());
        let t = s.timing.unwrap();
        assert!(t.aligned());
        assert_eq!((t.compared, t.mismatches), (599, 0));
        assert_eq!(t.first_mismatch, None);
        assert!(t.max_time_skew > 0.0 && t.max_time_skew < 1e-6, "{t:?}");
        assert_eq!(t.a.steps, 599);
        assert!(t.a.uniform() && t.b.uniform());
        assert_eq!(s.verdict, Verdict::Exact, "times are not a compared field");
        let text = render_text(&s);
        assert!(text.contains("time step: a 60 Hz, b 60 Hz\n"), "{text}");
        assert!(
            text.contains("frame lengths agree on 599 compared tick(s)"),
            "{text}"
        );
        assert!(!text.contains("warning"), "{text}");

        // A variable-rate trace against itself (a per-sample replay), with
        // another time base: aligned, no skew.
        let lengths: Vec<f64> = (0..50).map(|i| 0.012 + 0.001 * f64::from(i % 7)).collect();
        let v = variable(&lengths);
        let mut w = v.clone();
        for s in &mut w.samples {
            s.time += 1000.0;
        }
        let s = compare_traces("v", &v, "w", &w, &CompareTolerances::default());
        let t = s.timing.unwrap();
        assert!(t.aligned());
        assert_eq!(t.compared, 50);
        assert!(t.max_time_skew < 1e-9, "{t:?}");
        assert!((t.a.min - 0.012).abs() < 1e-12 && (t.a.max - 0.018).abs() < 1e-12);
        assert!(!t.a.uniform());
        let text = render_text(&s);
        assert!(
            text.contains(
                "time step: a variable 0.012000..0.018000 s (mean 0.014940 s), b variable 0.012000.."
            ),
            "{text}"
        );

        // The same recording against a fixed 60 Hz replay of it: every tick
        // was stepped with another length. The verdict still is about the
        // fields; the timing says the runs are not comparable tick by tick.
        let fixed = trace(51);
        let s = compare_traces("v", &v, "fixed", &fixed, &CompareTolerances::default());
        let t = s.timing.unwrap();
        assert!(!t.aligned());
        assert_eq!((t.compared, t.mismatches), (50, 50));
        let m = t.first_mismatch.unwrap();
        assert_eq!(m.tick, 1);
        assert!((m.a - 0.012).abs() < 1e-12 && (m.b - 1.0 / 60.0).abs() < 1e-12);
        let elapsed: f64 = lengths.iter().sum();
        assert!((t.max_time_skew - (50.0 / 60.0 - elapsed).abs()).abs() < 1e-9);
        assert_eq!(t.max_time_skew_tick, Some(50));
        assert_eq!(s.verdict, Verdict::Exact);
        let text = render_text(&s);
        assert!(
            text.contains("time step: a variable 0.012000..0.018000 s (mean 0.014940 s), b 60 Hz"),
            "{text}"
        );
        assert!(
            text.contains(
                "warning: frame lengths differ on 50 of 50 compared tick(s) (first at tick 1: \
                 a 0.012000 s, b 0.016667 s)"
            ),
            "{text}"
        );

        // Lengths are only compared where both traces have the tick and the
        // one before it; one differing tick is found.
        let mut gappy = v.clone();
        gappy.samples.remove(20);
        gappy.samples[30].time += 0.004;
        let s = compare_traces("v", &v, "gappy", &gappy, &CompareTolerances::default());
        let t = s.timing.unwrap();
        // 50 ticks, minus ticks 20 and 21 (tick 20 is missing in b).
        assert_eq!(t.compared, 48);
        // b's sample 30 is tick 31: its length and the next one's changed.
        assert_eq!(t.mismatches, 2);
        assert_eq!(t.first_mismatch.unwrap().tick, 31);
        assert_eq!(t.b.steps, 48, "b's own lengths skip its gap");

        // The summary keeps the timing through JSON; one written before the
        // field existed still reads.
        let json = serde_json::to_string(&s).unwrap();
        let back: CompareSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timing, s.timing);
        let mut old: serde_json::Value = serde_json::from_str(&json).unwrap();
        old.as_object_mut().unwrap().remove("timing");
        let back: CompareSummary = serde_json::from_value(old).unwrap();
        assert_eq!(back.timing, None);
        assert!(!render_text(&back).contains("time step"));

        // Degenerate times never make the summary unwritable.
        let mut wild = v.clone();
        wild.samples[5].time = 1e308;
        wild.samples[6].time = -1e308;
        let s = compare_traces("v", &v, "wild", &wild, &CompareTolerances::default());
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            serde_json::from_str::<CompareSummary>(&json).is_ok(),
            "{json}"
        );
        let none = Trace::new(v.meta.clone());
        let s = compare_traces("n", &none, "n", &none, &CompareTolerances::default());
        let t = s.timing.unwrap();
        assert_eq!((t.compared, t.a.steps, t.max_time_skew_tick), (0, 0, None));
        assert!(render_text(&s).contains("time step: a no fixed rate, b no fixed rate"));
    }

    #[test]
    fn grapple_and_overlap() {
        let a = trace(4);
        let mut b = a.clone();
        b.samples[1].grapple_state = TraceGrappleState::Attached;
        b.samples[1].grapple_anchor = Some(Vec3::ONE);
        let mut a2 = a.clone();
        a2.samples[2].grapple_state = TraceGrappleState::Attached;
        a2.samples[2].grapple_anchor = Some(Vec3::ZERO);
        b.samples[2].grapple_state = TraceGrappleState::Attached;
        b.samples[2].grapple_anchor = Some(Vec3::new(0.0, 3.0, 4.0));
        let s = compare_traces("a", &a2, "b", &b, &CompareTolerances::default());
        assert_eq!(s.first_exceedance["grapple_state"].tick, 1);
        assert_eq!(s.first_exceedance["grapple_anchor"].error, 5.0);

        let mut late = a.clone();
        for t in &mut late.samples {
            t.tick += 100;
        }
        let s = compare_traces("a", &a, "late", &late, &CompareTolerances::default());
        assert_eq!(s.verdict, Verdict::NoOverlap);
        let e = Trace::new(a.meta.clone());
        assert_eq!(
            compare_traces("e", &e, "e", &e, &CompareTolerances::default()).verdict,
            Verdict::Exact
        );
    }
}
