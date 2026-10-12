//! Regression tests on the recordings of the original: the comparisons of
//! `docs/PARITY_FINDINGS.md` section 4 (WP-T2), at the tolerances of its
//! section 5.
//!
//! Needs the three recordings of 2026-10-10 in `ASAMU_TRACE_RAW_DIR` (file
//! names containing `WS1`, `DC1` and `PLAY2`; they stay local) and an
//! `asamu-import` output with simple collision (mesh manifest version 2) in
//! `ASAMU_CONVERTED_DIR`; every test skips with a line otherwise, as on CI.
//! The last tests need the recording of 2026-10-12 (`KM1`: keyboard and
//! mouse, so the move keys are in every record, 78 frames a second,
//! recorder 0.2.0) with its `.stats.json`, and skip without it.
//!
//! Each test replays a stretch of a recording through our simulation
//! (`asamu_trace::replay`: the recording's inputs, each frame with its own
//! length, on the converted level) and compares it with the original's
//! samples. Expected values are the recording's own numbers; tolerances are
//! the measured noise of section 5 and nothing wider:
//!
//! - walking, horizontal position: 4 spacings of `f32` at the larger
//!   coordinate; horizontal velocity: 2 quanta (one spacing per frame
//!   length);
//! - vertical position while walking: section 5 has none. On the Workshop's
//!   flat BSP floor the recording itself holds two adjacent `f32` values
//!   (1,563 and 254 records), so one spacing there; elsewhere the width of
//!   the native hover band, 0.5 uu (`MAX_FLOOR_DIST − MIN_FLOOR_DIST`,
//!   NATIVE_PHYSICS.md 3.3: the original leaves a walking pawn alone
//!   anywhere inside it);
//! - vertical velocity in free fall: 2 quanta of twice the size;
//! - a free-running flight of `N` ticks: velocity `4 × 0.78 × quantum ×
//!   √N`, position the larger of 4 spacings and `4 × 0.45 × quantum × dt ×
//!   N^1.5` (the findings' TENTATIVE random-walk bound);
//! - the anchor of a press in the air: 0.15 uu.
//!
//! **A test that does not pass with the simulation as it is stays here,
//! marked `#[ignore]` with the measured numbers and the reason.** Nothing
//! was widened or left out to make a test pass. Run the ignored ones with
//! `-- --ignored --nocapture` to measure them again.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::OnceLock;

use asamu_core::glam::Vec3;
use asamu_player::Trace;
use asamu_player::trace::{TraceGrappleState, TraceSample};
use asamu_player::ue3_movement::{MAX_FLOOR_DIST, MIN_FLOOR_DIST};
use asamu_trace::convert::{ConvertOptions, convert};
use asamu_trace::raw::RawFile;
use asamu_trace::replay::{ReplayLevel, ReplayOptions, ReplayResult, StartPolicy, replay};
use asamu_trace::segments::{EventKind, recorded_events};
use asamu_trace::sidecar::{RecorderLog, sidecar_path};
use asamu_trace::state::StateTimeline;

/// The segments of the recording whose file name contains `key`, read once.
fn segments(key: &'static str) -> Option<&'static [Trace]> {
    static CACHE: [OnceLock<Option<Vec<Trace>>>; 4] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    let slot = ["WS1", "DC1", "PLAY2", "KM1"]
        .iter()
        .position(|k| *k == key)?;
    CACHE[slot]
        .get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os("ASAMU_TRACE_RAW_DIR")?);
            let path = std::fs::read_dir(&dir)
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        n.ends_with(".raw.jsonl") && n.contains(key)
                    })
                })
                .min()?;
            let raw = RawFile::from_str_lines(&std::fs::read_to_string(&path).ok()?).unwrap();
            // The recorder's log (recorder 0.2.0: which records were written
            // without their input), when the recording has one.
            let log = sidecar_path(&path)
                .filter(|p| p.is_file())
                .map(|p| RecorderLog::read(&p).unwrap());
            Some(
                convert(
                    &raw,
                    &ConvertOptions {
                        log,
                        ..ConvertOptions::default()
                    },
                )
                .unwrap()
                .into_iter()
                .map(|s| s.trace)
                .collect(),
            )
        })
        .as_deref()
}

fn converted_map(level: &str) -> Option<(PathBuf, String)> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let map = std::fs::read_dir(dir.join("levels"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".scene.json").map(str::to_owned))
        .find(|n| n.eq_ignore_ascii_case(level))?;
    Some((dir, map))
}

/// One segment of a recording and its converted level, or `None` (after a
/// line that says why) when the data is not there.
fn segment(key: &'static str, index: usize) -> Option<(&'static Trace, PathBuf, String)> {
    let Some(all) = segments(key) else {
        eprintln!(
            "skipped: no {key} recording in ASAMU_TRACE_RAW_DIR (unset or another directory)"
        );
        return None;
    };
    let trace = &all[index];
    let Some((dir, map)) = trace.meta.level.as_deref().and_then(converted_map) else {
        eprintln!("skipped: no converted level for {key} in ASAMU_CONVERTED_DIR");
        return None;
    };
    Some((trace, dir, map))
}

/// How a stretch is replayed.
#[derive(Clone, Copy, Default)]
struct How {
    story_mode: Option<bool>,
    force: bool,
    one_step: bool,
}

/// The original's stretch and our replay of it: `ticks` ticks from tick
/// `from` (to the end without `ticks`).
struct Stretch {
    original: &'static Trace,
    ours: ReplayResult,
}

impl Stretch {
    fn of(key: &'static str, seg: usize, from: u64, ticks: Option<u64>, how: How) -> Option<Self> {
        let (original, dir, map) = segment(key, seg)?;
        let ours = replay(
            original,
            &ReplayOptions {
                level: ReplayLevel::Converted {
                    dir,
                    map,
                    kismet: false,
                },
                start_tick: Some(from),
                max_ticks: ticks,
                story_mode: how.story_mode,
                start: if how.force {
                    StartPolicy::Force
                } else {
                    StartPolicy::Refuse
                },
                one_step: how.one_step,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        // The harness: the stretch asked for, with the recording's inputs
        // and frame lengths.
        assert_eq!(ours.start_tick, from);
        if let Some(n) = ticks.filter(|_| !how.one_step) {
            assert_eq!(ours.trace.samples.len() as u64, n + 1, "{key} seg{seg}");
        }
        let out = Self { original, ours };
        for b in out.ours.trace.samples.iter().skip(1) {
            let a = out.orig(b.tick);
            assert_eq!(b.input, a.input, "{key} seg{seg} tick {}", b.tick);
            assert_eq!(b.time, a.time, "{key} seg{seg} tick {}", b.tick);
        }
        Some(out)
    }

    fn orig(&self, tick: u64) -> &TraceSample {
        let i = self
            .original
            .samples
            .binary_search_by_key(&tick, |s| s.tick)
            .unwrap();
        &self.original.samples[i]
    }

    fn ours(&self, tick: u64) -> &TraceSample {
        let s = &self.ours.trace.samples;
        &s[s.binary_search_by_key(&tick, |s| s.tick).unwrap()]
    }

    /// The ticks of our replay after its start sample.
    fn ticks(&self) -> impl Iterator<Item = u64> + '_ {
        self.ours.trace.samples.iter().skip(1).map(|s| s.tick)
    }

    /// The frame length of `tick`, seconds.
    fn dt(&self, tick: u64) -> f64 {
        self.orig(tick).time - self.orig(tick - 1).time
    }
}

/// Spacing of `f32` at `|x|`.
fn ulp32(x: f32) -> f64 {
    let a = x.abs();
    f64::from(f32::from_bits(a.to_bits() + 1)) - f64::from(a)
}

/// Spacing of `f32` at the larger horizontal coordinate of `p`.
fn spacing_h(p: Vec3) -> f64 {
    ulp32(p.x.abs().max(p.y.abs()))
}

fn horizontal(v: Vec3) -> f64 {
    f64::from(v.x).hypot(f64::from(v.y))
}

/// Width of the native hover band, uu (NATIVE_PHYSICS.md 3.3).
fn band() -> f64 {
    f64::from(MAX_FLOOR_DIST) - f64::from(MIN_FLOOR_DIST)
}

/// The largest horizontal position and velocity differences of the walking
/// ticks of `s`, each as a multiple of its section 5 tolerance (4 spacings;
/// 2 quanta), with their ticks; and the largest absolute differences.
struct Walk {
    position: (f64, u64),
    velocity: (f64, u64),
    position_uu: (f64, u64),
    velocity_uu: (f64, u64),
    vertical_uu: (f64, u64),
    flags: Vec<u64>,
    /// The first tick with a horizontal number outside its tolerance.
    first_outside: Option<u64>,
}

fn walk(s: &Stretch) -> Walk {
    let mut w = Walk {
        position: (0.0, 0),
        velocity: (0.0, 0),
        position_uu: (0.0, 0),
        velocity_uu: (0.0, 0),
        vertical_uu: (0.0, 0),
        flags: Vec::new(),
        first_outside: None,
    };
    let keep = |slot: &mut (f64, u64), v: f64, t: u64| {
        if v > slot.0 {
            *slot = (v, t);
        }
    };
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        if a.grounded != b.grounded || a.grapple_state != b.grapple_state {
            w.flags.push(t);
        }
        let q = spacing_h(a.position);
        let (dp, dv) = (
            horizontal(b.position - a.position),
            horizontal(b.velocity - a.velocity),
        );
        keep(&mut w.position, dp / (4.0 * q), t);
        keep(&mut w.velocity, dv / (2.0 * q / s.dt(t)), t);
        if w.first_outside.is_none() && (dp > 4.0 * q || dv > 2.0 * q / s.dt(t)) {
            w.first_outside = Some(t);
        }
        keep(&mut w.position_uu, dp, t);
        keep(&mut w.velocity_uu, dv, t);
        keep(
            &mut w.vertical_uu,
            (f64::from(b.position.z) - f64::from(a.position.z)).abs(),
            t,
        );
    }
    w
}

fn describe(name: &str, w: &Walk) -> String {
    format!(
        "{name}: horizontal position at most {:.6} uu (tick {}), {:.2} times the tolerance at tick \
         {}; horizontal velocity at most {:.6} uu/s (tick {}), {:.2} times the tolerance at tick {}; \
         vertical position at most {:.6} uu (tick {}); first tick with a horizontal number outside \
         its tolerance: {:?}; flags differ on {} tick(s){}",
        w.position_uu.0,
        w.position_uu.1,
        w.position.0,
        w.position.1,
        w.velocity_uu.0,
        w.velocity_uu.1,
        w.velocity.0,
        w.velocity.1,
        w.vertical_uu.0,
        w.vertical_uu.1,
        w.first_outside,
        w.flags.len(),
        w.flags
            .first()
            .map_or_else(String::new, |t| format!(" (first {t})"))
    )
}

const WORKSHOP: How = How {
    // The Workshop's 132 uu/s is a console speed, which shows no story
    // mode; the whole recording is in story mode (findings V10).
    story_mode: Some(true),
    force: false,
    one_step: false,
};

const FORCED: How = How {
    story_mode: None,
    force: true,
    one_step: false,
};

// ---------------------------------------------------------------------------
// The Workshop (WS1): standing height, the door, the light beam.
// ---------------------------------------------------------------------------

/// WS1 from tick 1500, 62 ticks (to the tick before the door): the pawn
/// stands at the recorded height and walks the recorded line.
///
/// The height: the Workshop floor is BSP at z 72; the pawn's half height is
/// 44; the floor check holds a reported 2.15 uu; and a BSP floor is
/// reported 1.0 uu nearer than it is (the pull-back of a 28 uu check,
/// min(0.1 × 28, 1.0); NATIVE_PHYSICS.md 10.2). 72 + 44 + 2.15 + 1.0 =
/// 119.15, the recorded height (findings V18: 1,817 records). The recording
/// holds that height as two adjacent `f32` values, so the tolerance is one
/// spacing.
#[test]
fn ws1_1500_the_workshop_height_and_the_walk_to_the_door() {
    let Some(s) = Stretch::of("WS1", 0, 1500, Some(62), WORKSHOP) else {
        return;
    };
    let height = 119.15_f32;
    let one = ulp32(height);
    let mut exact = 0;
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        assert!(
            (f64::from(a.position.z) - f64::from(height)).abs() <= one,
            "the recording at tick {t}: {}",
            a.position.z
        );
        assert!(
            (f64::from(b.position.z) - f64::from(height)).abs() <= one,
            "ours at tick {t}: {}",
            b.position.z
        );
        exact += usize::from(b.position.z == height);
    }
    let w = walk(&s);
    println!(
        "{}; our height is the f32 nearest 119.15 on {exact} of 62 ticks",
        describe("WS1 1500+62", &w)
    );
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
}

/// WS1 from tick 1500, 150 ticks: the pawn meets the door at tick 1563 and
/// stops where the original stops.
///
/// The recorded x at ticks 1563, 1564 and 1565 is 303.1922, 303.0848 and
/// 303.0847 (findings V11; the test first checks the recording against
/// these). The tolerance, 0.001 uu, is the plan's for this comparison: the
/// contact rule (a reported hit 0.1 uu, or a tenth of the move, short of
/// the door's body) predicts these advances to 0.0001 uu, and positions
/// near 303 are recorded to 0.00003. The velocity of a walking tick is its
/// displacement over the frame length, so the same tolerance on the speed
/// at 1563 is 0.001 uu over that frame.
#[test]
fn ws1_1500_the_door_stops_the_pawn_at_tick_1563_where_it_stops_the_original() {
    let Some(s) = Stretch::of("WS1", 0, 1500, Some(150), WORKSHOP) else {
        return;
    };
    for (tick, x) in [(1563, 303.1922_f64), (1564, 303.0848), (1565, 303.0847)] {
        let (a, b) = (s.orig(tick), s.ours(tick));
        assert!(
            (f64::from(a.position.x) - x).abs() < 1e-4,
            "the recording at {tick}: {}",
            a.position.x
        );
        println!(
            "tick {tick}: x {} (original {}), speed {:.4} (original {:.4})",
            b.position.x,
            a.position.x,
            horizontal(b.velocity),
            horizontal(a.velocity)
        );
        assert!(
            (f64::from(b.position.x) - x).abs() <= 1e-3,
            "ours at {tick}: {}",
            b.position.x
        );
        assert!(b.grounded && a.grounded, "tick {tick}");
    }
    // No horizontal divergence at the contact tick: position and speed.
    let (a, b) = (s.orig(1563), s.ours(1563));
    assert!(horizontal(b.position - a.position) <= 1e-3);
    assert!(horizontal(b.velocity - a.velocity) <= 1e-3 / s.dt(1563));
}

/// The same 150 ticks at the section 5 walking tolerance on every tick.
#[test]
#[ignore = "measured 2026-10-12: inside the tolerance up to tick 1586, the door contact of tick \
            1563 included. At tick 1587 the position is 0.000153 uu off against a tolerance of \
            0.000122 (5 spacings against 4). At the next wall, where one tick of our rules from \
            the original's state is 0.011 to 0.108 uu off on each of the ticks 1606 to 1610, the \
            difference reaches 0.120313 uu (tick 1630) and 8.028886 uu/s (tick 1631). The first \
            is drift at the level of float spacing; the second wall contact is not explained"]
fn ws1_1500_150_ticks_inside_the_walking_tolerance() {
    let Some(s) = Stretch::of("WS1", 0, 1500, Some(150), WORKSHOP) else {
        return;
    };
    let w = walk(&s);
    println!("{}", describe("WS1 1500+150", &w));
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
}

/// WS1 from tick 2673 to the end (327 ticks): the pawn walks through the
/// light beam at tick 2986 at the original's 132 uu/s (ours was deflected
/// to 68 uu/s there, findings V20), and stays on the recorded line.
#[test]
fn ws1_2673_no_deflection_at_the_light_beam() {
    let Some(s) = Stretch::of("WS1", 0, 2673, Some(327), WORKSHOP) else {
        return;
    };
    let (a, b) = (s.orig(2986), s.ours(2986));
    println!(
        "tick 2986: speed {} (original {})",
        horizontal(b.velocity),
        horizontal(a.velocity)
    );
    // The recorded speed there is the Workshop's console speed, 440 × 0.3.
    assert!((horizontal(a.velocity) - 132.0).abs() < 0.01);
    let quantum = spacing_h(a.position) / s.dt(2986);
    assert!(horizontal(b.velocity - a.velocity) <= 2.0 * quantum);
    let w = walk(&s);
    println!("{}", describe("WS1 2673+327", &w));
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
    // A flat BSP floor: one spacing of the height (see the first test).
    assert!(
        w.vertical_uu.0 <= ulp32(s.orig(2986).position.z),
        "vertical: {} at tick {}",
        w.vertical_uu.0,
        w.vertical_uu.1
    );
}

// ---------------------------------------------------------------------------
// Darkcave and BeautifulCity (DC1): a start inside a prop, flat walk, slope.
// ---------------------------------------------------------------------------

/// DC1 segment 3 from tick 742: the original walks off at tick 744; ours
/// could not move (it started inside the triangles of a prop that does not
/// block the original's pawn, findings P1 and V8).
#[test]
fn dc1_seg3_742_the_pawn_moves_at_tick_744() {
    let Some(s) = Stretch::of("DC1", 3, 742, Some(30), FORCED) else {
        return;
    };
    assert!(!s.ours.start_overlaps, "the start overlaps our collision");
    let moved = |t: u64, of: &dyn Fn(u64) -> Vec3| horizontal(of(t) - of(742));
    let ours = |t: u64| s.ours(t).position;
    let original = |t: u64| s.orig(t).position;
    println!(
        "moved by tick 744: ours {:.4} uu, original {:.4} uu; by tick 772: ours {:.3}, original {:.3}",
        moved(744, &ours),
        moved(744, &original),
        moved(772, &ours),
        moved(772, &original)
    );
    assert_eq!(moved(743, &original), 0.0);
    assert!(moved(744, &original) > 0.0);
    assert_eq!(moved(743, &ours), 0.0);
    assert!(moved(744, &ours) > 0.0);
    // And by as much as the original, to the walking tolerance.
    let (a, b) = (s.orig(744), s.ours(744));
    assert!(horizontal(b.position - a.position) <= 4.0 * spacing_h(a.position));
}

/// The same stretch at the section 5 walking tolerance on every tick, and
/// within the hover band of the original's height.
#[test]
fn dc1_seg3_742_30_ticks_inside_the_walking_tolerance() {
    let Some(s) = Stretch::of("DC1", 3, 742, Some(30), FORCED) else {
        return;
    };
    let w = walk(&s);
    println!("{}", describe("DC1 seg3 742+30", &w));
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
    assert!(
        w.vertical_uu.0 <= band(),
        "vertical: tick {}",
        w.vertical_uu.1
    );
}

/// A walk of DC1 segment 0 (AG-ParadiseCave), from a standing start: the
/// comparison, with the replay's start notes printed.
fn paradise_cave_walk(from: u64, ticks: u64) -> Option<(Stretch, Walk)> {
    let s = Stretch::of("DC1", 0, from, Some(ticks), How::default())?;
    let w = walk(&s);
    println!("{}", describe(&format!("DC1 seg0 {from}+{ticks}"), &w));
    for n in s
        .ours
        .trace
        .meta
        .notes
        .iter()
        .filter(|n| n.starts_with("start:") || n.starts_with("warning:"))
    {
        println!("    | {n}");
    }
    Some((s, w))
}

/// The largest height difference of the ticks from `from` on, uu, and its
/// tick.
fn vertical_from(s: &Stretch, from: u64) -> (f64, u64) {
    s.ticks()
        .filter(|t| *t >= from)
        .map(|t| {
            let (a, b) = (s.orig(t), s.ours(t));
            ((f64::from(b.position.z) - f64::from(a.position.z)).abs(), t)
        })
        .fold((0.0, 0), |m, e| if e.0 > m.0 { e } else { m })
}

/// DC1 segment 0 from tick 1010, 68 ticks: a walk up a gentle slope, on the
/// recorded line (the horizontal numbers at the section 5 walking
/// tolerance; they were bit-equal before the second wave too, findings
/// V12).
#[test]
fn dc1_seg0_1010_a_walk_on_the_recorded_line() {
    let Some((_, w)) = paradise_cave_walk(1010, 68) else {
        return;
    };
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
}

/// The same walk once the original's pawn has been lifted to its standing
/// height (from tick 1016): our height is the original's to the width of
/// the hover band.
///
/// The original's pawn was put down by a respawn at tick 997 and landed at
/// tick 1000. A landing leaves the pawn a tenth of the last fall move plus
/// 2.15 uu above the floor (here 2.255 uu above mesh triangles, which a
/// floor check reports 2.8 uu nearer than they are: NATIVE_PHYSICS.md
/// 10.2), which is below the hover band. A pawn at rest is left alone; with
/// its first steps the floor check lifts it in two stages: by 2.15 uu at
/// tick 1015 (the recorded rise is 2.1484 uu, 2.15 at the spacing of
/// positions near 97,000) and to the band at tick 1016.
#[test]
fn dc1_seg0_1010_the_height_once_the_original_stands_in_the_band() {
    let Some((s, _)) = paradise_cave_walk(1010, 68) else {
        return;
    };
    // The recording: at rest to tick 1014, lifted in tick 1015.
    let z = |t: u64| f64::from(s.orig(t).position.z);
    assert_eq!(z(1014), z(1010));
    assert!((z(1015) - z(1014) - 2.15).abs() <= 2.0 * ulp32(s.orig(1015).position.z));
    let (worst, at) = vertical_from(&s, 1016);
    println!("DC1 seg0 1016..=1078: height at most {worst:.6} uu off (tick {at})");
    assert!(worst <= band(), "vertical: {worst} uu at tick {at}");
}

/// The same walk on every tick from the replay's start.
#[test]
#[ignore = "measured 2026-10-12: our height is 2.148438 uu above the original's on ticks 1011 to \
            1014 and 0.570312 uu above it on tick 1015; from tick 1016 on it is within 0.039062 \
            uu. The original's pawn rests at the height its landing at tick 1000 left, below the \
            hover band, until its first step at tick 1015; a replay's start runs our floor check \
            in its first tick, so our pawn makes the same two lifts four ticks early. A matter of \
            how a replay starts (the harness), not of the movement rules: \
            `dc1_seg0_1010_the_height_once_the_original_stands_in_the_band` is the same \
            comparison from tick 1016"]
fn dc1_seg0_1010_a_walk_within_the_hover_band() {
    let Some((s, _)) = paradise_cave_walk(1010, 68) else {
        return;
    };
    for t in 1011..=1016 {
        println!(
            "    tick {t}: ours {:+.6} uu from the original",
            f64::from(s.ours(t).position.z) - f64::from(s.orig(t).position.z)
        );
    }
    let (worst, at) = vertical_from(&s, 0);
    assert!(worst <= band(), "vertical: {worst} uu at tick {at}");
}

/// DC1 segment 0 from tick 3724, 52 ticks: a slope. Our height is the
/// original's to the width of the hover band on every tick (before the
/// second wave: up to 4.4 uu low, findings P4).
#[test]
fn dc1_seg0_3724_a_slope_within_the_hover_band() {
    let Some((s, w)) = paradise_cave_walk(3724, 52) else {
        return;
    };
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    let (worst, at) = vertical_from(&s, 0);
    assert!(worst <= band(), "vertical: {worst} uu at tick {at}");
}

/// The same slope at the section 5 walking tolerance.
#[test]
#[ignore = "measured 2026-10-12: the horizontal position leaves the tolerance of 4 spacings \
            (0.0625 uu) at tick 3759 and is 0.169010 uu off at tick 3768 (2.70 times the \
            tolerance); the velocity stays inside (at most 1.193866 uu/s, 0.63 times). No tick of \
            the stretch is outside the one-step tolerance, so this is drift: differences of a \
            spacing or two per tick that add up on the slope"]
fn dc1_seg0_3724_a_slope_inside_the_walking_tolerance() {
    let Some((_, w)) = paradise_cave_walk(3724, 52) else {
        return;
    };
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
}

// ---------------------------------------------------------------------------
// The grapple (PLAY2): a volume that is switched off, anchors, the release
// distance.
// ---------------------------------------------------------------------------

/// The free-running flight tolerances of section 5 at tick `t` of `s`, `n`
/// ticks after the pawn last walked: (position, velocity), for horizontal
/// numbers.
fn flight_tolerance(s: &Stretch, t: u64, n: u64) -> (f64, f64) {
    let (q, dt, n) = (spacing_h(s.orig(t).position), s.dt(t), n as f64);
    let quantum = q / dt;
    (
        (4.0 * q).max(4.0 * 0.45 * quantum * dt * n.powf(1.5)),
        4.0 * 0.78 * quantum * n.sqrt(),
    )
}

/// PLAY2 segment 4 from tick 3110: the grapple flight of ticks 3112 to 3225
/// passes through `DynamicBlockingVolume_6`, which is switched off in the
/// original (`bEnabled` false, findings P5 and V13). Ours was deflected at
/// tick 3145 (861 uu/s against 1,152).
#[test]
fn play2_seg4_3110_the_flight_is_not_blocked_at_tick_3145() {
    let Some(s) = Stretch::of("PLAY2", 4, 3110, Some(150), How::default()) else {
        return;
    };
    // The pawn walks at tick 3111 and is attached from 3112.
    assert!(s.orig(3111).grounded && !s.orig(3112).grounded);
    for t in 3112..=3225 {
        assert_eq!(s.orig(t).grapple_state, TraceGrappleState::Attached);
        assert_eq!(s.ours(t).grapple_state, TraceGrappleState::Attached, "{t}");
    }
    for t in 3145..=3148 {
        let (a, b) = (s.orig(t), s.ours(t));
        let (tp, tv) = flight_tolerance(&s, t, t - 3111);
        let (dp, dv) = (
            horizontal(b.position - a.position),
            horizontal(b.velocity - a.velocity),
        );
        println!(
            "tick {t}: speed {:.3} (original {:.3}); position off {dp:.5} uu (tolerance {tp:.5}), \
             velocity {dv:.5} uu/s (tolerance {tv:.5})",
            horizontal(b.velocity),
            horizontal(a.velocity)
        );
        assert!(dp <= tp && dv <= tv, "tick {t}");
    }
}

/// The same flight to its end (the release at tick 3226 and the fall after
/// it) inside the free-running flight tolerance on every tick.
#[test]
#[ignore = "measured 2026-10-12: inside the tolerance through the flight (ticks 3112 to 3225) and \
            its release, up to tick 3231. From tick 3232 the fall's horizontal velocity leaves the \
            bound: 3.612012 uu/s against 0.536917 at tick 3259, and the position 0.960284 uu \
            against 0.799267 at tick 3260. One tick of our rules from the original's state is \
            0.0007 to 0.0015 uu and 0.09 to 0.18 uu/s off on each falling tick from 3231 to 3261 \
            (about 2,030 uu/s, stick held): a small difference in the air steering at that speed, \
            not explained"]
fn play2_seg4_3110_150_ticks_inside_the_flight_tolerance() {
    let Some(s) = Stretch::of("PLAY2", 4, 3110, Some(150), How::default()) else {
        return;
    };
    let mut worst = (0.0_f64, 0.0_f64, 0_u64, 0_u64);
    let mut flags = Vec::new();
    for t in 3112..=3260 {
        let (a, b) = (s.orig(t), s.ours(t));
        if a.grounded != b.grounded || a.grapple_state != b.grapple_state {
            flags.push(t);
        }
        let (tp, tv) = flight_tolerance(&s, t, t - 3111);
        let (dp, dv) = (
            horizontal(b.position - a.position) / tp,
            horizontal(b.velocity - a.velocity) / tv,
        );
        if dp > worst.0 {
            (worst.0, worst.2) = (dp, t);
        }
        if dv > worst.1 {
            (worst.1, worst.3) = (dv, t);
        }
    }
    println!(
        "PLAY2 seg4 3110+150: horizontal position at most {:.2} times the tolerance (tick {}), \
         velocity {:.2} times (tick {}); flags differ on {flags:?}",
        worst.0, worst.2, worst.1, worst.3
    );
    for t in [worst.2, worst.3] {
        let (a, b) = (s.orig(t), s.ours(t));
        let (tp, tv) = flight_tolerance(&s, t, t - 3111);
        println!(
            "    tick {t}: position {:.6} uu off (tolerance {tp:.6}), velocity {:.6} uu/s off \
             (tolerance {tv:.6}); walking {} (original {}), attached {} (original {})",
            horizontal(b.position - a.position),
            horizontal(b.velocity - a.velocity),
            b.grounded,
            a.grounded,
            b.grapple_state == TraceGrappleState::Attached,
            a.grapple_state == TraceGrappleState::Attached
        );
    }
    let first = (3112..=3260).find(|t| {
        let (a, b) = (s.orig(*t), s.ours(*t));
        let (tp, tv) = flight_tolerance(&s, *t, *t - 3111);
        horizontal(b.position - a.position) > tp || horizontal(b.velocity - a.velocity) > tv
    });
    println!("    first tick outside: {first:?}");
    assert!(flags.is_empty());
    assert!(worst.0 <= 1.0 && worst.1 <= 1.0);
}

/// PLAY2 segment 5 from tick 6526 (in the air): the anchor of the grapple
/// is the original's, to 0.15 uu, on every attached tick. Ours was 3.9968
/// uu beyond it on all 25 (the hit on mesh triangles is reported 4.0 uu
/// before the surface, findings V16 and P3).
#[test]
fn play2_seg5_6526_the_anchor_is_the_recorded_one() {
    let Some(s) = Stretch::of("PLAY2", 5, 6526, Some(60), FORCED) else {
        return;
    };
    let (mut attached, mut worst) = (0, 0.0_f32);
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        assert_eq!(a.grapple_state, b.grapple_state, "tick {t}");
        if let (Some(x), Some(y)) = (a.grapple_anchor, b.grapple_anchor) {
            attached += 1;
            worst = worst.max((y - x).length());
        }
    }
    println!("PLAY2 seg5 6526+60: anchor at most {worst:.6} uu off on {attached} attached ticks");
    assert_eq!(attached, 25);
    assert!(worst <= 0.15);
}

/// An attach of a recording: the tick, and whether the target stood still.
struct Attach {
    key: &'static str,
    seg: usize,
    tick: u64,
    /// Our anchor of that tick (one tick of our rules from the original's
    /// state before it) against the recorded one, uu.
    error: Option<f32>,
}

/// Every attach of DC1 and PLAY2 whose button press found the pawn in the
/// air and whose target stands still (the anchor is the same on every
/// attached tick), each with our anchor: a one-step replay fires our trace
/// from the original's own position and view.
fn airborne_anchors() -> Option<Vec<Attach>> {
    let mut out = Vec::new();
    for key in ["DC1", "PLAY2"] {
        let count = segments(key).map(<[Trace]>::len);
        let Some(count) = count else {
            eprintln!("skipped: no {key} recording in ASAMU_TRACE_RAW_DIR");
            return None;
        };
        for seg in 0..count {
            let how = How {
                story_mode: None,
                force: true,
                one_step: true,
            };
            let first = segments(key)?[seg].samples[0].tick;
            let s = Stretch::of(key, seg, first, None, how)?;
            let o = &s.original.samples;
            for k in 1..o.len() {
                let attached = |i: usize| o[i].grapple_state == TraceGrappleState::Attached;
                if !attached(k) || attached(k - 1) || o[k - 1].grounded {
                    continue;
                }
                let anchor = o[k].grapple_anchor.unwrap();
                let still = (k..o.len()).take_while(|i| attached(*i)).all(|i| {
                    o[i].grapple_anchor
                        .is_some_and(|a| (a - anchor).length() <= 0.01)
                });
                if !still {
                    continue;
                }
                let ours = s
                    .ours
                    .trace
                    .samples
                    .binary_search_by_key(&o[k].tick, |x| x.tick)
                    .ok()
                    .and_then(|i| s.ours.trace.samples[i].grapple_anchor);
                out.push(Attach {
                    key,
                    seg,
                    tick: o[k].tick,
                    error: ours.map(|a| (a - anchor).length()),
                });
            }
        }
    }
    Some(out)
}

fn list(attaches: &[&Attach]) -> String {
    attaches
        .iter()
        .map(|a| {
            format!(
                "{} seg{} tick {}: {}",
                a.key,
                a.seg,
                a.tick,
                a.error
                    .map_or_else(|| "not attached".to_owned(), |e| format!("{e:.4} uu"))
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The presses in the air on a target that stands still: 52 in these
/// recordings (findings V16), of which 47 lay on the analysts' aim line to
/// 0.05 uu. At least those 47 anchors are the original's to 0.15 uu.
#[test]
fn the_airborne_anchors_are_the_recorded_ones() {
    let Some(all) = airborne_anchors() else {
        return;
    };
    let off: Vec<&Attach> = all
        .iter()
        .filter(|a| a.error.is_none_or(|e| e > 0.15))
        .collect();
    let mut errors: Vec<f32> = all.iter().filter_map(|a| a.error).collect();
    errors.sort_by(f32::total_cmp);
    println!(
        "{} presses in the air on a still target; anchor within 0.15 uu on {}; median {:.6} uu, \
         largest {:.6} uu; the others: {}",
        all.len(),
        all.len() - off.len(),
        errors[errors.len() / 2],
        errors[errors.len() - 1],
        list(&off)
    );
    assert_eq!(all.len(), 52);
    assert!(all.len() - off.len() >= 47, "{}", list(&off));
}

/// All 52 of them.
#[test]
#[ignore = "measured 2026-10-12: 49 of 52 within 0.15 uu (median 0.003937 uu). The other three: \
            DC1 seg0 tick 5146, 0.2519 uu; DC1 seg0 tick 6607, 0.3639 uu; PLAY2 seg1 tick 1457, \
            0.7419 uu. They are the three presses the grapple analysis found off its aim line \
            (findings section 7: sideways 1.88, 0.69 and 0.15 uu with exact angles); not explained"]
fn every_airborne_anchor_is_the_recorded_one() {
    let Some(all) = airborne_anchors() else {
        return;
    };
    let off: Vec<&Attach> = all
        .iter()
        .filter(|a| a.error.is_none_or(|e| e > 0.15))
        .collect();
    assert!(off.is_empty(), "{}", list(&off));
}

/// PLAY2 segment 8, the six presses of ticks 155 to 277 (155, 183, 211,
/// 237, 264 and 277; the used-grapple counter rises in the tick after each):
/// the pawn stands 197.0 to 199.9 uu from the reported hit, inside the gun's
/// release distance, so the original attaches and lets go in one frame (the
/// counter rises, no sample is attached). Ours measured 200.0 to 202.9 uu
/// to its own hit, stayed attached and flew (findings P3).
///
/// One tick of our rules from the original's state before each: we attach
/// in that tick, are idle after it, and our velocity is the original's to
/// the one-step tolerance of a grapple tick, 0.5 uu/s.
#[test]
fn play2_seg8_the_presses_at_the_release_distance_let_go_in_the_press_frame() {
    let how = How {
        story_mode: None,
        force: true,
        one_step: true,
    };
    let Some(first) = segments("PLAY2").map(|s| s[8].samples[0].tick) else {
        eprintln!("skipped: no PLAY2 recording in ASAMU_TRACE_RAW_DIR");
        return;
    };
    let Some(s) = Stretch::of("PLAY2", 8, first, None, how) else {
        return;
    };
    let timeline = StateTimeline::from_notes(&s.original.meta.notes).unwrap();
    let rises: Vec<u64> = recorded_events(&s.original.samples, timeline.as_ref())
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::GrappleAttachedWithinFrame)
        .map(|e| e.tick)
        .filter(|t| (156..=278).contains(t))
        .collect();
    // The recording: the counter rises in the tick after each press.
    assert_eq!(rises, [156, 184, 212, 238, 265, 278]);
    let attaches = s.ours.attaches.as_ref().unwrap();
    let mut worst = (0.0_f64, 0_u64);
    for t in &rises {
        let (a, b) = (s.orig(*t), s.ours(*t));
        assert!(attaches.ours.contains(t), "no attach of ours at tick {t}");
        assert_eq!(b.grapple_state, TraceGrappleState::Idle, "tick {t}");
        assert_eq!(a.grounded, b.grounded, "tick {t}");
        let dv = f64::from((b.velocity - a.velocity).length());
        if dv > worst.0 {
            worst = (dv, *t);
        }
    }
    println!(
        "PLAY2 seg8: 6 presses released in their frame; largest velocity difference after that \
         frame {:.4} uu/s at tick {}",
        worst.0, worst.1
    );
    assert!(worst.0 <= 0.5);
}

// ---------------------------------------------------------------------------
// Free fall.
// ---------------------------------------------------------------------------

/// The largest vertical velocity difference of the falling ticks of `s` up
/// to the original's landing, in the section 5 quanta (2 quanta of twice
/// the size are the tolerance), and the landing ticks of both sides.
fn fall(s: &Stretch) -> (f64, u64, Option<u64>, Option<u64>) {
    let mut worst = (0.0_f64, 0_u64);
    let landing = |of: &dyn Fn(u64) -> bool| s.ticks().find(|t| of(*t));
    let original = landing(&|t| s.orig(t).grounded);
    let ours = landing(&|t| s.ours(t).grounded);
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        if a.grounded || a.grapple_state == TraceGrappleState::Attached {
            break;
        }
        let quantum = 2.0 * ulp32(a.position.z) / s.dt(t);
        let e = (f64::from(b.velocity.z) - f64::from(a.velocity.z)).abs() / quantum;
        if e > worst.0 {
            worst = (e, t);
        }
    }
    (worst.0, worst.1, original, ours)
}

/// PLAY2 segment 4 from tick 3014: a jump (the pawn leaves the ground at
/// tick 3036) and its fall. The vertical velocity is the original's to 2
/// quanta on every tick in the air, and the pawn lands in the original's
/// tick.
#[test]
fn play2_seg4_3014_a_jump_falls_and_lands_as_recorded() {
    let Some(s) = Stretch::of("PLAY2", 4, 3014, Some(96), How::default()) else {
        return;
    };
    let mut worst = (0.0_f64, 0_u64);
    let mut air = 0;
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        assert_eq!(a.grounded, b.grounded, "tick {t}");
        if a.grounded {
            continue;
        }
        air += 1;
        let quantum = 2.0 * ulp32(a.position.z) / s.dt(t);
        let e = (f64::from(b.velocity.z) - f64::from(a.velocity.z)).abs() / quantum;
        if e > worst.0 {
            worst = (e, t);
        }
    }
    println!(
        "PLAY2 seg4 3014+96: {air} ticks in the air; vertical velocity at most {:.3} doubled \
         quanta off (tick {})",
        worst.0, worst.1
    );
    assert!(air > 30);
    assert!(worst.0 <= 2.0);
}

/// The five respawns of the recordings (DC1 segment 0 at ticks 997 and
/// 5746, PLAY2 segment 1 at 5946, PLAY2 segment 5 at 2393 and 6779): the
/// pawn is put above its checkpoint and falls. From the first record after
/// the teleport, our fall has the original's vertical velocity to 2 quanta
/// until the original lands, and lands in the same tick.
#[test]
fn the_five_respawn_falls_land_in_the_recorded_tick() {
    let mut seen = 0;
    for (key, seg) in [("DC1", 0), ("PLAY2", 1), ("PLAY2", 5)] {
        let Some((original, ..)) = segment(key, seg) else {
            return;
        };
        let timeline = StateTimeline::from_notes(&original.meta.notes).unwrap();
        let teleports: Vec<u64> = recorded_events(&original.samples, timeline.as_ref())
            .unwrap()
            .iter()
            .filter(|e| e.kind == EventKind::Teleport)
            .map(|e| e.tick)
            .collect();
        for tick in teleports {
            let Some(s) = Stretch::of(key, seg, tick, Some(40), FORCED) else {
                return;
            };
            assert!(!s.orig(tick).grounded, "{key} seg{seg} {tick}: falling");
            let (worst, at, original, ours) = fall(&s);
            println!(
                "{key} seg{seg} respawn at tick {tick}: lands at {original:?} (ours {ours:?}); \
                 vertical velocity at most {worst:.3} doubled quanta off (tick {at})"
            );
            assert!(original.is_some(), "{key} seg{seg} {tick}");
            assert_eq!(ours, original, "{key} seg{seg} {tick}: landing tick");
            assert!(worst <= 2.0, "{key} seg{seg} {tick}");
            seen += 1;
        }
    }
    assert_eq!(seen, 5);
}

// ---------------------------------------------------------------------------
// The keyboard recording of 2026-10-12 (KM1): exact move keys, 78 frames a
// second, the camera's FOV lock.
// ---------------------------------------------------------------------------

/// KM1 segment 0 (AG-Workshop) from tick 3311, 800 ticks: the zoom key is
/// held twice for about two seconds and tapped twice (session item 2 of
/// findings section 6), then the pawn walks.
///
/// The recorded FOV is the camera's locked FOV (recorder 0.2.0; findings N1
/// and V19), so it shows the zoom: 90 at rest, 50 fully zoomed. Our FOV is
/// the recorded one on every sample, and the walk is the recorded one to
/// the section 5 walking tolerance.
#[test]
fn km1_workshop_3311_the_zoom_is_the_recorded_one() {
    let Some(s) = Stretch::of("KM1", 0, 3311, Some(800), WORKSHOP) else {
        return;
    };
    let (mut zoomed, mut fully, mut differs) = (0, 0, 0);
    let mut worst = (0.0_f32, 0_u64);
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        zoomed += usize::from(a.fov != 90.0);
        fully += usize::from(a.fov == 50.0);
        differs += usize::from(a.fov.to_bits() != b.fov.to_bits());
        let d = (b.fov - a.fov).abs();
        if d > worst.0 {
            worst = (d, t);
        }
    }
    let w = walk(&s);
    println!(
        "KM1 seg0 3311+800: the recorded FOV is not 90 on {zoomed} samples (50 on {fully}); ours \
         differs in a bit on {differs}, by at most {} (tick {}). {}",
        worst.0,
        worst.1,
        describe("The walk", &w)
    );
    // The recording: two holds to 50 and two taps.
    assert!(zoomed > 300 && fully > 200, "{zoomed} {fully}");
    assert_eq!(differs, 0, "FOV: {} off at tick {}", worst.0, worst.1);
    assert!(w.flags.is_empty(), "{:?}", w.flags);
    assert!(w.position.0 <= 1.0, "position: tick {}", w.position.1);
    assert!(w.velocity.0 <= 1.0, "velocity: tick {}", w.velocity.1);
}

/// KM1 segment 0 from tick 4063: the sprint key is held while walking
/// forward (ticks 4070 to 4176 and 4232 to 4367; session item 2: "whether
/// sprint overwrites 132"). The original keeps the Workshop's 132 uu/s
/// (`real_recordings.rs` has the recording's side). Ours does too: on every
/// tick of the two holds our speed is the original's to 2 velocity quanta
/// and never above 132.
#[test]
fn km1_workshop_4063_a_held_sprint_key_leaves_the_speed_at_132() {
    let Some(s) = Stretch::of("KM1", 0, 4063, Some(310), WORKSHOP) else {
        return;
    };
    let (mut held, mut worst) = (0, (0.0_f64, 0_u64));
    for t in s.ticks() {
        let (a, b) = (s.orig(t), s.ours(t));
        assert_eq!(a.grounded, b.grounded, "tick {t}");
        if !(a.input.sprint_held && a.input.move_forward > 0.0) {
            continue;
        }
        held += 1;
        let quantum = spacing_h(a.position) / s.dt(t);
        // 440 × 0.3, and what rounding a position adds to a speed.
        assert!(
            horizontal(a.velocity) <= 132.0 + quantum,
            "the recording, {t}"
        );
        assert!(horizontal(b.velocity) <= 132.0 + quantum, "ours, tick {t}");
        // Two quanta, and the spacing of the two velocity values the
        // difference is computed from.
        let slack = 2.0 * ulp32(a.velocity.x.abs().max(a.velocity.y.abs()));
        let e = (horizontal(b.velocity - a.velocity) - slack) / (2.0 * quantum);
        if e > worst.0 {
            worst = (e, t);
        }
    }
    let w = walk(&s);
    println!(
        "KM1 seg0 4063+310: sprint and forward held on {held} ticks; velocity at most {:.2} times \
         the tolerance there (tick {}). {}",
        worst.0,
        worst.1,
        describe("The whole stretch", &w)
    );
    assert!(held > 200, "{held}");
    assert!(worst.0 <= 1.0, "velocity: tick {}", worst.1);
}
