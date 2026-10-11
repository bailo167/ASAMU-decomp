//! The eight comparisons of `docs/PARITY_FINDINGS.md` (the converter
//! report's table), free-running and one step at a time, with the
//! horizontal and vertical numbers; and the events of the recordings
//! (teleports, level-script state changes, attaches by the used-grapple
//! counter).
//!
//! Needs the three recordings of 2026-10-10 in `ASAMU_TRACE_RAW_DIR` (file
//! names containing `WS1`, `DC1` and `PLAY2`; they stay local) and an
//! `asamu-import` output in `ASAMU_CONVERTED_DIR`; skips otherwise. Run with
//! `--nocapture` for the numbers.
//!
//! What is asserted is the harness, not the simulation: every replay gets
//! the original's inputs and frame lengths, a one-step replay is marked and
//! never carries an error of one tick into the next start state, the FOV of
//! these recordings stays out of the verdict, and the components add up.
//! The simulation's own numbers are printed. They diverge today
//! (PARITY_FINDINGS.md section 2 says where and why); no tolerance here was
//! chosen to make anything pass.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use asamu_player::Trace;
use asamu_player::trace::CompareTolerances;
use asamu_trace::compare::{AxisStats, CompareSummary, compare_traces};
use asamu_trace::convert::{ConvertOptions, convert};
use asamu_trace::raw::RawFile;
use asamu_trace::replay::{ReplayLevel, ReplayOptions, ReplayResult, replay};
use asamu_trace::segments::{EventKind, recorded_events};
use asamu_trace::state::StateTimeline;

/// The segments of the recording whose file name contains `key`.
fn segments(key: &str) -> Option<Vec<Trace>> {
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
    let raw = RawFile::from_str_lines(&std::fs::read_to_string(path).ok()?).unwrap();
    Some(
        convert(&raw, &ConvertOptions::default())
            .unwrap()
            .into_iter()
            .map(|s| s.trace)
            .collect(),
    )
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

/// The tolerances the findings' comparisons were made with (an analysis
/// choice of that study, stated next to its numbers).
fn tolerances() -> CompareTolerances {
    CompareTolerances {
        position: 1.0,
        velocity: 10.0,
        angle: 0.001,
        fov: 0.1,
        anchor: 1.0,
    }
}

fn axis(a: &AxisStats) -> String {
    let at = a.max_tick.map_or_else(|| "-".to_owned(), |t| t.to_string());
    match (a.signed_min, a.signed_max) {
        (Some(lo), Some(hi)) => format!(
            "{:.4} @{at} (rms {:.4}; b - a {lo:+.4} .. {hi:+.4})",
            a.max, a.rms
        ),
        _ => format!("{:.6} @{at} (rms {:.6})", a.max, a.rms),
    }
}

fn print(name: &str, mode: &str, r: &ReplayResult, s: &CompareSummary) {
    let c = s.components.as_ref().unwrap();
    let d = &s.diff;
    println!(
        "{name} [{mode}]: {} ticks, verdict {:?}, first divergence {:?}",
        d.matched.saturating_sub(1),
        s.verdict,
        s.verdict_divergence.map(|x| (x.tick, x.field, x.error))
    );
    println!(
        "    position: horizontal {}, vertical {}",
        axis(&c.position_horizontal),
        axis(&c.position_vertical)
    );
    println!(
        "    velocity: horizontal {}, vertical {}",
        axis(&c.velocity_horizontal),
        axis(&c.velocity_vertical)
    );
    println!(
        "    yaw {:.4} units, pitch {:.4} units; FOV max {} deg ({}); anchor max {:.4} uu on {} \
         attached ticks; grounded differs on {} ticks, grapple state on {}",
        c.yaw_units.max,
        c.pitch_units.max,
        d.fov.max,
        if s.fov.as_ref().unwrap().counted {
            "counted"
        } else {
            "not counted"
        },
        d.grapple_anchor.max,
        d.grapple_anchor.count,
        d.grounded_mismatches,
        d.grapple_state_mismatches
    );
    let first: Vec<String> = s
        .first_exceedance
        .iter()
        .map(|(k, e)| format!("{k} {} ({:.4})", e.tick, e.error))
        .collect();
    println!("    first exceeded: {}", first.join(", "));
    for n in r.trace.meta.notes.iter().filter(|n| {
        n.starts_with("start:")
            || n.starts_with("warning:")
            || n.starts_with("validity:")
            || n.starts_with("attaches:")
            || n.starts_with("state check:")
            || (n.starts_with("one-step:")
                && !n.contains("resynchronised")
                && !n.contains("each of"))
    }) {
        println!("    | {n}");
    }
}

/// The largest one-step errors over the ticks that did not start inside our
/// collision ([`ReplayResult::resync_overlaps`]), each with its tick:
/// horizontal position, |vertical position|, horizontal velocity, |vertical
/// velocity|.
fn outside_overlaps(original: &Trace, stepped: &ReplayResult) -> [(f64, u64); 4] {
    let mut worst = [(0.0_f64, 0_u64); 4];
    for b in &stepped.trace.samples {
        if stepped.resync_overlaps.binary_search(&b.tick).is_ok() {
            continue;
        }
        let Ok(i) = original.samples.binary_search_by_key(&b.tick, |s| s.tick) else {
            continue;
        };
        let a = &original.samples[i];
        let dp = b.position.as_dvec3() - a.position.as_dvec3();
        let dv = b.velocity.as_dvec3() - a.velocity.as_dvec3();
        for (slot, e) in worst.iter_mut().zip([
            dp.truncate().length(),
            dp.z.abs(),
            dv.truncate().length(),
            dv.z.abs(),
        ]) {
            if e > slot.0 {
                *slot = (e, b.tick);
            }
        }
    }
    worst
}

/// One comparison of the findings: a stretch of one segment of a recording.
struct Case {
    name: &'static str,
    /// What the recording's file name contains.
    recording: &'static str,
    segment: usize,
    /// Start tick.
    from: u64,
    /// Ticks to replay (`None`: to the end).
    ticks: Option<u64>,
    /// Story mode at the start, where the recording does not show it.
    story_mode: Option<bool>,
}

#[test]
fn the_eight_comparisons_free_running_and_one_step() {
    let case = |name, recording, segment, from, ticks, story_mode| Case {
        name,
        recording,
        segment,
        from,
        ticks,
        story_mode,
    };
    let cases = [
        case("WS1 whole, story mode on", "WS1", 0, 0, None, Some(true)),
        case("walk", "DC1", 0, 1010, Some(68), None),
        case("sprint", "PLAY2", 1, 7627, Some(114), None),
        case("jump", "PLAY2", 4, 3014, Some(96), None),
        case("power jump", "PLAY2", 5, 1750, Some(155), None),
        case(
            "grapple, automatic release",
            "PLAY2",
            5,
            6920,
            Some(150),
            None,
        ),
        case("grapple, manual release", "PLAY2", 4, 3110, Some(150), None),
        case(
            "jump, then a grapple with automatic release",
            "DC1",
            0,
            3987,
            Some(60),
            None,
        ),
    ];
    let mut ran = 0;
    for Case {
        name,
        recording,
        segment,
        from,
        ticks,
        story_mode,
    } in cases
    {
        let Some(all) = segments(recording) else {
            eprintln!(
                "skipped {name}: no {recording} recording in ASAMU_TRACE_RAW_DIR (unset or \
                 another directory)"
            );
            continue;
        };
        let original = &all[segment];
        let Some((dir, map)) = original.meta.level.as_deref().and_then(converted_map) else {
            eprintln!("skipped {name}: no converted level in ASAMU_CONVERTED_DIR");
            continue;
        };
        let opts = |one_step| ReplayOptions {
            level: ReplayLevel::Converted {
                dir: dir.clone(),
                map: map.clone(),
                kismet: false,
            },
            start_tick: Some(from),
            max_ticks: ticks,
            story_mode,
            one_step,
            ..ReplayOptions::default()
        };
        let free = replay(original, &opts(false)).unwrap();
        let stepped = replay(original, &opts(true)).unwrap();
        let tol = tolerances();
        let s_free = compare_traces("original", original, "free", &free.trace, &tol);
        let s_step = compare_traces("original", original, "one-step", &stepped.trace, &tol);
        print(name, "free-running", &free, &s_free);
        print(name, "one-step", &stepped, &s_step);
        let w = outside_overlaps(original, &stepped);
        println!(
            "    one-step: {} of {} tick(s) start inside our collision; over the other ticks: \
             position horizontal {:.4} @{}, vertical {:.4} @{}; velocity horizontal {:.4} @{}, \
             vertical {:.4} @{}",
            stepped.resync_overlaps.len(),
            stepped.trace.samples.len() - 1,
            w[0].0,
            w[0].1,
            w[1].0,
            w[1].1,
            w[2].0,
            w[2].1,
            w[3].0,
            w[3].1
        );

        // The harness: same inputs, same frame lengths, the whole stretch
        // (none of the eight crosses a teleport or a level-script change).
        let asked = ticks.map_or(original.samples.len() - 1, |t| t as usize);
        for (r, s) in [(&free, &s_free), (&stepped, &s_step)] {
            assert_eq!(r.trace.samples.len(), asked + 1, "{name}");
            assert!(
                r.events.is_empty() && r.stopped_at_event.is_none(),
                "{name}"
            );
            assert_eq!(s.diff.input_mismatches, 0, "{name}");
            assert!(s.timing.as_ref().unwrap().aligned(), "{name}");
            // These recordings' FOV column is the cached view FOV.
            let fov = s.fov.as_ref().unwrap();
            assert!(!fov.counted, "{name}: {fov:?}");
            // The components are parts of the 3-D error.
            let c = s.components.as_ref().unwrap();
            assert!(c.position_horizontal.max <= s.diff.position.max, "{name}");
            assert!(c.position_vertical.max <= s.diff.position.max, "{name}");
            assert!(c.velocity_horizontal.max <= s.diff.velocity.max, "{name}");
            assert!(c.velocity_vertical.max <= s.diff.velocity.max, "{name}");
        }
        assert!(!s_free.one_step && s_step.one_step, "{name}");
        assert!(stepped.one_step && !free.one_step, "{name}");
        // Ticks that start inside our collision are listed: in order, inside
        // the replayed ticks, each with a note when there is one; a
        // free-running replay has none (it never restarts).
        assert!(free.resync_overlaps.is_empty(), "{name}");
        assert!(
            stepped.resync_overlaps.windows(2).all(|w| w[0] < w[1])
                && stepped
                    .resync_overlaps
                    .iter()
                    .all(|t| *t > from && *t <= from + asked as u64),
            "{name}"
        );
        assert_eq!(
            stepped
                .trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("one-step: warning:") && n.contains("start inside our")),
            !stepped.resync_overlaps.is_empty(),
            "{name}"
        );
        // Without those ticks no error is larger than with them.
        let c = s_step.components.as_ref().unwrap();
        for (kept, all) in w.iter().zip([
            c.position_horizontal.max,
            c.position_vertical.max,
            c.velocity_horizontal.max,
            c.velocity_vertical.max,
        ]) {
            assert!(kept.0 <= all, "{name}");
        }
        // The first tick starts from the same state in both modes.
        assert_eq!(free.trace.samples[1], stepped.trace.samples[1], "{name}");
        // Both sides count their attaches the same way.
        let (a, b) = (free.attaches.unwrap(), stepped.attaches.unwrap());
        assert_eq!(a.original, b.original, "{name}");
        ran += 1;
    }
    println!("{ran} of 8 comparisons ran");
}

/// The events of the recordings: what the converter marks and a replay
/// stops at.
#[test]
fn events_of_the_recordings() {
    let mut total = (0, 0, 0, 0);
    let mut samples = 0;
    let mut eye_changes = 0;
    let mut marked: Vec<String> = Vec::new();
    for recording in ["WS1", "DC1", "PLAY2"] {
        let Some(all) = segments(recording) else {
            eprintln!("skipped: no {recording} recording in ASAMU_TRACE_RAW_DIR");
            continue;
        };
        for (i, trace) in all.iter().enumerate() {
            let timeline = StateTimeline::from_notes(&trace.meta.notes).unwrap();
            let events = recorded_events(&trace.samples, timeline.as_ref()).unwrap();
            let ticks = |keep: &dyn Fn(EventKind) -> bool| -> Vec<String> {
                events
                    .iter()
                    .filter(|e| keep(e.kind))
                    .map(|e| format!("{} {}", e.tick, e.kind.name()))
                    .collect()
            };
            let teleports = ticks(&|k| k == EventKind::Teleport);
            let level = ticks(&EventKind::is_level_state);
            let attaches = events.iter().filter(|e| e.kind.is_attach()).count();
            let within = ticks(&|k| k == EventKind::GrappleAttachedWithinFrame);
            println!(
                "{recording} seg{i} ({} samples): teleports {teleports:?}; level-script changes \
                 {level:?}; {attaches} attaches, inside one frame {within:?}",
                trace.samples.len()
            );
            marked.extend(
                teleports
                    .iter()
                    .chain(&level)
                    .map(|e| format!("{recording} seg{i} {e}")),
            );
            total.0 += teleports.len();
            total.1 += level.len();
            total.2 += attaches;
            total.3 += within.len();
            samples += trace.samples.len();
            // A replay that starts at the first sample of a run and is not
            // told otherwise never simulates an event's tick.
            if let Some(first) = events.iter().find(|e| e.kind.ends_validity()) {
                assert!(first.index > 0, "{recording} seg{i}");
            }
            // The eye height of every tick is in the state note.
            if let Some(t) = &timeline {
                let mut cursor = t.cursor();
                let mut last = None;
                for s in &trace.samples {
                    let eye = cursor.advance(s.tick).unwrap().and_then(|st| st.eye_height);
                    assert!(eye.is_some(), "{recording} seg{i} tick {}", s.tick);
                    if last.is_some() && last != eye {
                        eye_changes += 1;
                    }
                    last = eye;
                }
            }
        }
    }
    println!(
        "{samples} samples: {} teleports, {} level-script state changes, {} attaches ({} of them \
         inside one frame); the eye height changes on {eye_changes} ticks",
        total.0, total.1, total.2, total.3
    );
    // With all three recordings of 2026-10-10: the counts the analysts found
    // independently (docs/PARITY_FINDINGS.md N6, N3, N7; the event rules
    // were not fitted to them: the teleport limit is a class default, the
    // level-script rule the pawn's own GroundSpeed writers).
    if samples == 45_237 {
        assert_eq!(total, (5, 7, 134, 15));
        assert_eq!(
            marked,
            [
                "DC1 seg0 997 teleport",
                "DC1 seg0 5746 teleport",
                "DC1 seg0 7214 story mode on",
                "DC1 seg3 5019 story mode off",
                "PLAY2 seg0 1475 story mode on",
                "PLAY2 seg1 5946 teleport",
                "PLAY2 seg1 785 story mode off",
                "PLAY2 seg1 8250 story mode on",
                "PLAY2 seg4 2470 story mode off",
                "PLAY2 seg4 2535 grapple capacity changed",
                "PLAY2 seg5 2393 teleport",
                "PLAY2 seg5 6779 teleport",
            ]
        );
    }
}
