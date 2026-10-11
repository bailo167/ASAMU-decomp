//! End-to-end: synthetic raw recording → convert → replay → compare → report,
//! through the library and through the command-line tool.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::process::Command;

use asamu_player::Trace;
use asamu_player::trace::{CompareTolerances, TraceGrappleState, compare};
use asamu_player::ue3_movement::TARGET_FLOOR_DIST;
use asamu_trace::compare::{
    CompareOptions, CompareSummary, FovPolicy, Verdict, compare_traces, compare_traces_with,
};
use asamu_trace::convert::{ConvertOptions, convert};
use asamu_trace::raw::RawFile;
use asamu_trace::replay::{
    Attaches, EventAction, EventPolicy, ReplayOptions, ReplayResult, StartPolicy, replay,
};
use asamu_trace::segments::{EventKind, clean_runs, is_clean_start, recorded_events};
use asamu_trace::state::StateTimeline;

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

/// A flight on the grapple to the graybox's first grapple point: look up
/// at it, hold the button until the gun lets go close to the anchor, turn
/// the view down a little and press again while still next to the point,
/// which attaches and releases inside one frame (tick 90).
fn grapple_flight(n: usize) -> Vec<common::Step> {
    (1..=n)
        .map(|i| common::Step {
            d_pitch: match i {
                3 => 3950,
                89 => -3000,
                _ => 0,
            },
            grapple: (10..87).contains(&i) || (90..110).contains(&i),
            ..common::Step::default()
        })
        .collect()
}

/// Walking backwards off the start platform: the pawn falls below the
/// level's kill height and is respawned at the start (a teleport).
fn walk_off_the_edge(n: usize) -> Vec<common::Step> {
    (1..=n)
        .map(|i| common::Step {
            forward: if i <= 60 { -1 } else { 0 },
            ..common::Step::default()
        })
        .collect()
}

fn has(r: &ReplayResult, text: &str) -> bool {
    r.trace.meta.notes.iter().any(|n| n.contains(text))
}

fn one_step() -> ReplayOptions {
    ReplayOptions {
        one_step: true,
        ..ReplayOptions::default()
    }
}

/// A one-step replay of a recording that our own simulation made is that
/// recording, tick for tick and bit for bit: the resynchronisation writes
/// back what is already there, so nothing it leaves alone can drift either.
#[test]
fn one_step_replay_of_our_own_run_is_exact_for_every_tick() {
    for (name, steps, start) in [
        (
            "walk, sprint, jump, a missed grapple",
            common::script(200),
            None,
        ),
        ("two acts", two_acts(330), None),
        (
            "two acts from the rest between them",
            two_acts(330),
            Some(180),
        ),
        ("a grapple flight", grapple_flight(130), None),
    ] {
        let (raw, ours) = common::scripted_steps(&steps, 2_000, false);
        let fake = common::fake_original(&raw, &ours);
        let opts = ReplayOptions {
            start_tick: start,
            ..one_step()
        };
        let r = replay(&fake, &opts).unwrap();
        assert!(r.one_step, "{name}");
        assert_eq!(r.stopped_at_event, None, "{name}");
        let from = start.unwrap_or(0) as usize;
        assert_eq!(r.trace.samples.len(), ours.samples.len() - from, "{name}");
        // Every field of every tick (the first sample is the start state;
        // the fixed path's clock restarts with the replay).
        for (mine, theirs) in r.trace.samples.iter().zip(&ours.samples[from..]) {
            assert_eq!(
                asamu_player::TraceSample { time: 0.0, ..*mine },
                asamu_player::TraceSample {
                    time: 0.0,
                    ..*theirs
                },
                "{name}: tick {}",
                theirs.tick
            );
        }
        let d = compare(&fake, &r.trace);
        assert_eq!(d.matched, ours.samples.len() - from, "{name}");
        assert_eq!(
            (
                d.position.max,
                d.velocity.max,
                d.yaw.max,
                d.pitch.max,
                d.fov.max
            ),
            (0.0, 0.0, 0.0, 0.0, 0.0),
            "{name}"
        );
        assert_eq!(
            (
                d.grapple_anchor.max,
                d.grapple_state_mismatches,
                d.grounded_mismatches,
                d.input_mismatches
            ),
            (0.0, 0, 0, 0),
            "{name}"
        );
        // It says what it is, and that it had nothing to put right.
        assert!(
            has(
                &r,
                &format!(
                    "one-step: each of {} tick(s) starts from the input trace's previous sample",
                    ours.samples.len() - from - 1
                )
            ),
            "{name}: {:#?}",
            r.trace.meta.notes
        );
        assert!(has(&r, "one-step: not resynchronised"), "{name}");
        for never in [
            "the physics mode was set",
            "was created",
            "was released",
            "anchor was moved",
        ] {
            assert!(!has(&r, never), "{name}: {never}");
        }
        assert!(
            has(
                &r,
                "our GroundSpeed, AirControl, the sprint flag, the used-grapple count, the \
                 grapple capacity equal(s) the recording's"
            ) && has(&r, "our eye height equals the recorded EyeHeight"),
            "{name}: {:#?}",
            r.trace.meta.notes
        );
        let s = compare_traces("fake", &fake, "one-step", &r.trace, &Default::default());
        assert!(s.first_exceedance.is_empty(), "{name}");
        if start.is_none() {
            assert_eq!(s.verdict, Verdict::Exact, "{name}");
        }
        assert!(s.one_step, "{name}");
    }
    // The flight really is one: the pawn is attached for a while, and the
    // second attach never shows in a sample.
    let (raw, ours) = common::scripted_steps(&grapple_flight(130), 2_000, false);
    let attached = ours
        .samples
        .iter()
        .filter(|s| s.grapple_state == TraceGrappleState::Attached)
        .count();
    assert!(attached > 50, "{attached}");
    let fake = common::fake_original(&raw, &ours);
    let r = replay(&fake, &one_step()).unwrap();
    assert_eq!(
        r.attaches,
        Some(Attaches {
            original: vec![10, 90],
            ours: vec![10, 90]
        })
    );
    // The trace as converted (its view angles went through rotator units,
    // which can move a yaw by one bit) is within that bit's effect.
    let converted = &convert(&raw, &ConvertOptions::default()).unwrap()[0].trace;
    let r = replay(converted, &one_step()).unwrap();
    let d = compare(converted, &r.trace);
    assert!(
        d.position.max < 1e-4 && d.velocity.max < 1e-2 && d.yaw.max < ANGLE_EPS,
        "{d:?}"
    );
}

/// What a one-step replay is for: an original that differs from us in one
/// tick. A free-running replay carries the difference to the end; a
/// one-step replay shows it in that tick only.
#[test]
fn one_step_replay_shows_a_difference_in_its_own_tick_only() {
    // Walking straight along the start platform.
    let walk: Vec<common::Step> = (1..=70)
        .map(|_| common::Step {
            forward: 1,
            ..common::Step::default()
        })
        .collect();
    let (raw, ours) = common::scripted_steps(&walk, 500, false);
    // An "original" that is moved 5 uu along its path in tick 30 and walks
    // on from there (5 uu is 0.68 ticks of walking: no teleport).
    let mut bent = common::fake_original(&raw, &ours);
    for s in &mut bent.samples[30..] {
        s.position.x += 5.0;
    }
    // (5 uu added to an f32 coordinate is 5 uu to within its rounding.)
    let five = |x: f64| (x - 5.0).abs() < 1e-4;
    let free = replay(&bent, &ReplayOptions::default()).unwrap();
    let d = compare(&bent, &free.trace);
    assert!(five(d.position.max), "{}", d.position.max);
    assert!(
        five(d.position.mean * 71.0 / 41.0),
        "5 uu off on every tick from 30: mean {}",
        d.position.mean
    );
    let stepped = replay(&bent, &one_step()).unwrap();
    let d = compare(&bent, &stepped.trace);
    assert!(five(d.position.max), "{}", d.position.max);
    assert_eq!(d.position.max_tick, Some(30));
    let later = stepped.trace.samples[31..]
        .iter()
        .zip(&bent.samples[31..])
        .map(|(a, b)| f64::from((a.position - b.position).length()))
        .fold(0.0, f64::max);
    assert!(
        later < 1e-3,
        "after tick 30 each tick starts from the moved original: {later}"
    );
    let tol = CompareTolerances {
        position: 0.01,
        ..CompareTolerances::default()
    };
    let s = compare_traces("bent", &bent, "one-step", &stepped.trace, &tol);
    let c = s.components.unwrap();
    assert!(five(c.position_horizontal.max));
    assert_eq!(c.position_vertical.max, 0.0, "the difference is sideways");
    assert_eq!(s.first_exceedance["position_horizontal"].tick, 30);
    assert!(!s.first_exceedance.contains_key("position_vertical"));
}

/// A respawn inside a recording: the converter marks the teleport, the
/// listing shows it, and a replay ends before it (or takes the recorded
/// position, or runs through, as asked).
#[test]
fn a_teleport_ends_a_replays_validity() {
    let (raw, ours) = common::scripted_steps(&walk_off_the_edge(330), 9_000, false);
    let fake = common::fake_original(&raw, &ours);
    // Our own run respawned once; the samples show where.
    let events = recorded_events(&fake.samples, None).unwrap();
    let teleports: Vec<u64> = events
        .iter()
        .filter(|e| e.kind == EventKind::Teleport)
        .map(|e| e.tick)
        .collect();
    assert_eq!(teleports.len(), 1, "{teleports:?}");
    let t = teleports[0];
    let jump = (fake.samples[t as usize].position - fake.samples[t as usize - 1].position).length();
    assert!(jump > 1000.0, "the respawn moves the pawn {jump} uu");
    assert!(
        fake.meta.notes.iter().any(|n| *n
            == format!(
                "event: 1 teleport(s) (the pawn moved more than 10000 uu/s x the frame length in \
                 one tick: a respawn or another script move): tick(s) {t}"
            )),
        "{:#?}",
        fake.meta.notes
    );
    let listing = asamu_trace::segments::describe(&fake, true).unwrap();
    assert!(
        listing.contains(&format!("\n        {t} teleport\n")),
        "{listing}"
    );
    let with = |events, one_step| {
        replay(
            &fake,
            &ReplayOptions {
                events,
                one_step,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    // Default: the valid part only.
    let r = with(EventPolicy::Stop, false);
    assert_eq!(r.stopped_at_event, Some(t));
    assert_eq!(r.trace.samples.len() as u64, t);
    assert_eq!(r.trace.samples.last().unwrap().tick, t - 1);
    assert_eq!(r.events.len(), 1);
    assert_eq!(
        (r.events[0].tick, r.events[0].kind, r.events[0].action),
        (t, EventKind::Teleport, EventAction::Stopped)
    );
    assert!(
        has(
            &r,
            &format!(
                "validity: stopped before tick {t} (teleport): a replay of the inputs cannot \
                 reproduce it. {} of 330 tick(s) replayed",
                t - 1
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert_eq!(r.respawns, 0, "our own respawn was not reached");
    let d = compare(&fake, &r.trace);
    assert_eq!((d.matched as u64, d.only_in_a as u64), (t, 331 - t));
    assert_eq!(d.position.max, 0.0);
    // The comparison repeats the replay's note.
    let s = compare_traces("fake", &fake, "replay", &r.trace, &Default::default());
    assert!(
        s.harness_notes
            .iter()
            .any(|n| n.starts_with("validity: stopped before tick")),
        "{:#?}",
        s.harness_notes
    );
    // Taken from the recording: the replay goes on from where the original
    // is after its teleport.
    let r = with(EventPolicy::Inject, false);
    assert_eq!(r.stopped_at_event, None);
    assert_eq!(r.trace.samples.len(), 331);
    assert_eq!(r.events[0].action, EventAction::Injected);
    assert_eq!(
        r.trace.samples[t as usize].position,
        fake.samples[t as usize].position
    );
    assert!(has(
        &r,
        "injected: 1 recorded change(s) the inputs cannot reproduce"
    ));
    assert!(has(&r, &format!("{t} teleport")));
    assert!(!has(&r, "validity:"));
    // Run through, with a warning (our simulation respawns by its own rule
    // here, so this fake recording is reproduced all the same).
    let r = with(EventPolicy::Ignore, false);
    assert_eq!(r.events[0].action, EventAction::Ignored);
    assert_eq!(r.respawns, 1);
    assert!(
        has(
            &r,
            &format!(
                "warning: the replay ran through 1 event(s) the inputs cannot reproduce \
                 (--level-events ignore): {t} teleport. From tick {t} on the comparison is not \
                 valid"
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // One step at a time: the teleport's tick is left out, the ticks around
    // it are exact.
    let r = with(EventPolicy::Stop, true);
    assert_eq!(r.stopped_at_event, None);
    assert_eq!(r.events[0].action, EventAction::LeftOut);
    assert_eq!(r.trace.samples.len(), 330);
    assert!(r.trace.samples.iter().all(|s| s.tick != t));
    let d = compare(&fake, &r.trace);
    assert_eq!((d.matched, d.only_in_a, d.only_in_b), (330, 1, 0));
    assert_eq!((d.position.max, d.velocity.max), (0.0, 0.0));
    assert!(
        has(
            &r,
            &format!(
                "validity: 1 event(s) the inputs cannot reproduce; their ticks are left out of \
                 this one-step replay (not simulated; the tick after starts from the recording): \
                 {t} teleport"
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // A replay that ends before the teleport is not touched by it; one that
    // starts after it neither. A replay of our own runtime trace is never
    // checked (it reproduces its own respawn).
    let before = replay(
        &fake,
        &ReplayOptions {
            max_ticks: Some(t - 2),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(before.events.is_empty() && before.stopped_at_event.is_none());
    let mut runtime = ours.clone();
    runtime.meta.notes = fake.meta.notes.clone();
    let r = replay(&runtime, &ReplayOptions::default()).unwrap();
    assert!(r.events.is_empty() && r.stopped_at_event.is_none() && r.attaches.is_none());
    assert_eq!(r.trace.samples.len(), 331);
}

/// Level-script state changes inside a recording (story mode, the grapple
/// capacity): the replay stops before the first, or writes the recorded
/// change into our state under a note.
#[test]
fn a_level_script_state_change_ends_a_replays_validity() {
    let walk: Vec<common::Step> = (1..=120)
        .map(|i| common::Step {
            forward: i32::from(i >= 5),
            ..common::Step::default()
        })
        .collect();
    let (mut raw, ours) = common::scripted_steps(&walk, 300, false);
    // The level script enters story mode at tick 40 and raises the grapple
    // capacity at tick 80. (The positions stay our run's: only the harness
    // is under test here.)
    let pawn = asamu_player::PlayerParams::asamu_original().pawn.unwrap();
    let story_speed = pawn.move_speed.value * pawn.story_speed_multiplier.value;
    for r in &mut raw.records[40..] {
        r.player.as_mut().unwrap().ground_speed = story_speed;
    }
    for r in &mut raw.records[80..] {
        r.player
            .as_mut()
            .unwrap()
            .gun
            .as_mut()
            .unwrap()
            .max_grapples = 7;
    }
    let fake = common::fake_original(&raw, &ours);
    assert!(
        fake.meta.notes.iter().any(|n| n
            == "event: 2 level-script state change(s) (story mode, a console speed, the grapple \
                capacity or the rocket boots: changes the pawn's own rules do not make): 40 \
                story mode on, 80 grapple capacity changed"),
        "{:#?}",
        fake.meta.notes
    );
    let timeline = StateTimeline::from_notes(&fake.meta.notes).unwrap();
    let kinds: Vec<(u64, EventKind)> = recorded_events(&fake.samples, timeline.as_ref())
        .unwrap()
        .iter()
        .filter(|e| e.kind.ends_validity())
        .map(|e| (e.tick, e.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            (40, EventKind::StoryModeOn),
            (80, EventKind::MaxGrapplesChanged)
        ]
    );
    let with = |events| {
        replay(
            &fake,
            &ReplayOptions {
                events,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    let r = with(EventPolicy::Stop);
    assert_eq!(r.stopped_at_event, Some(40));
    assert_eq!(r.trace.samples.len(), 40);
    assert!(
        has(
            &r,
            "validity: stopped before tick 40 (story mode on): a replay of the inputs cannot \
             reproduce it. 39 of 120 tick(s) replayed"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // Our walk was at the walking speed until then, as the recording's.
    assert!(has(
        &r,
        "state check: after each of 39 tick(s) our GroundSpeed, AirControl"
    ));
    // Injected: our pawn is in story mode from tick 40 (it walks at the
    // story speed from there, which the class defaults give as 440 x 0.6)
    // and has the recorded capacity from tick 80.
    let r = with(EventPolicy::Inject);
    assert_eq!(r.trace.samples.len(), 121);
    assert_eq!(
        r.events
            .iter()
            .map(|e| (e.tick, e.action))
            .collect::<Vec<_>>(),
        [(40, EventAction::Injected), (80, EventAction::Injected)]
    );
    assert!(
        has(
            &r,
            "(--level-events inject): a level-script state change is written before our tick, a \
             teleport replaces our position, velocity, view and physics mode after it: 40 story \
             mode on, 80 grapple capacity changed"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(
        has(
            &r,
            "state check: after each of 120 tick(s) our GroundSpeed, AirControl, the sprint \
             flag, the used-grapple count, the grapple capacity equal(s) the recording's"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    let speed = |r: &ReplayResult, tick: usize| r.trace.samples[tick].velocity.truncate().length();
    assert!((speed(&r, 39) - 440.0).abs() < 0.01, "{}", speed(&r, 39));
    assert!((speed(&r, 119) - 264.0).abs() < 0.01, "{}", speed(&r, 119));
    // Run through: our pawn never hears of either, and the state check
    // says from where.
    let r = with(EventPolicy::Ignore);
    assert!((speed(&r, 119) - 440.0).abs() < 0.01);
    assert!(
        has(
            &r,
            "state check: GroundSpeed differs from the recording's after 81 of 120 tick(s) \
             (first at tick 40: ours 440, recorded 264)"
        ) && has(
            &r,
            "state check: the grapple capacity differs from the recording's after 41 of 120 \
             tick(s) (first at tick 80: ours 3, recorded 7)"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(has(&r, "From tick 40 on the comparison is not valid"));
    // One step at a time: both ticks are left out; every other tick starts
    // with the recorded speed, so the story speed is walked from tick 41.
    let r = replay(&fake, &one_step()).unwrap();
    assert_eq!(r.trace.samples.len(), 119);
    assert!(r.trace.samples.iter().all(|s| s.tick != 40 && s.tick != 80));
    let at = |tick: u64| {
        r.trace
            .samples
            .iter()
            .find(|s| s.tick == tick)
            .unwrap()
            .velocity
            .truncate()
            .length()
    };
    assert!((at(39) - 440.0).abs() < 0.01, "{}", at(39));
    // The first story tick starts from the recording's 440 uu/s and brakes
    // towards the story speed; it does not get faster.
    assert!(at(41) < 440.0 && at(41) >= 264.0 - 0.01, "{}", at(41));
}

/// A start whose recorded position is inside our collision, or above our
/// floor: the replay says so instead of silently standing still or
/// dropping the pawn.
#[test]
fn a_start_that_does_not_fit_our_collision_is_reported() {
    let walk: Vec<common::Step> = (1..=60)
        .map(|i| common::Step {
            forward: i32::from(i >= 5),
            ..common::Step::default()
        })
        .collect();
    // The same run recorded `dz` higher or lower than our floor has it,
    // with or without a recorded base actor at the start.
    let recorded = |dz: f32, base: Option<&str>| {
        let (mut raw, ours) = common::scripted_steps(&walk, 100, false);
        for r in &mut raw.records {
            r.player.as_mut().unwrap().location.z += dz;
        }
        raw.records[0].player.as_mut().unwrap().base = base.map(str::to_owned);
        let mut moved = ours.clone();
        for s in &mut moved.samples {
            s.position.z += dz;
        }
        common::fake_original(&raw, &moved)
    };
    let half_height = asamu_player::PlayerParams::asamu_original()
        .movement
        .capsule_half_height
        .value;
    // The start platform's top is z = 0: a walking pawn's centre rests the
    // half height and the native hover distance above it.
    let rest = half_height + TARGET_FLOOR_DIST;

    // 10 uu inside the platform.
    let inside = recorded(-10.0, None);
    let r = replay(&inside, &ReplayOptions::default()).unwrap();
    assert!(r.start_overlaps);
    let start = inside.samples[0].position;
    assert!(
        has(
            &r,
            &format!(
                "warning: the start position overlaps our collision: the pawn's shape at the \
                 recorded position ({}, {}, {}) is inside our geometry; lowered from 16 uu \
                 above, it comes to rest ",
                start.x, start.y, start.z
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(has(
        &r,
        "Our pawn may not move from here; the original stood there on its own collision"
    ));
    // Where it comes to rest is our floor's contact height for the shape:
    // the half height above the platform's top (within the sweep's skin).
    let note = r
        .trace
        .meta
        .notes
        .iter()
        .find(|n| n.contains("overlaps our collision"))
        .unwrap();
    let rest_above: f64 = note
        .split("comes to rest ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    let expected = f64::from(half_height) - f64::from(start.z);
    assert!(
        (rest_above - expected).abs() <= 0.051,
        "{rest_above} against {expected}"
    );
    assert!(has(&r, "warning: in the first tick our pawn moves"));
    // A pawn that does not overlap gets no such warning.
    let fits = recorded(0.0, None);
    let r = replay(&fits, &ReplayOptions::default()).unwrap();
    assert!(!r.start_overlaps);
    assert!(!has(&r, "overlaps our collision"));

    // 5 uu above its rest height, walking: our floor check re-seats it.
    let high = recorded(5.0, None);
    let r = replay(&high, &ReplayOptions::default()).unwrap();
    assert!(!r.start_overlaps);
    let z0 = high.samples[0].position.z;
    assert!(
        has(
            &r,
            &format!(
                "start: our floor is {:.3} uu below the pawn at the recorded position (our floor \
                 check rests a pawn 2.15 uu above its floor and leaves a based one alone between \
                 1.9 and 2.4)",
                z0 - half_height
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(has(&r, "the recording names no base actor at this tick"));
    assert_eq!(r.trace.samples[1].position.z, rest, "re-seated by the rule");
    assert!(
        has(
            &r,
            &format!(
                "warning: in the first tick our pawn moves {:+.4} uu vertically and the original \
                 {:+.4} uu while both walk: -5.0000 uu apart, more than the width of the native \
                 hover band (0.5 uu)",
                f64::from(rest) - f64::from(z0),
                f64::from(high.samples[1].position.z) - f64::from(z0)
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // Too high for the floor check to find a floor: it falls.
    let r = replay(&recorded(40.0, None), &ReplayOptions::default()).unwrap();
    assert!(
        has(
            &r,
            "start: no floor of ours within 28 uu below the recorded position (the recording \
             names no base actor at this tick): our pawn starts to fall"
        ),
        "{:#?}",
        r.trace.meta.notes
    );

    // Inside the hover band (2.3 uu above the floor, between 1.9 and 2.4):
    // a pawn with a recorded base is left where it stands, as the native
    // floor check leaves a based pawn; without one it is re-seated at 2.15.
    let in_band = half_height + 2.3;
    let dz = in_band - fits.samples[0].position.z;
    let based = recorded(dz, Some(common::FLOOR_ACTOR));
    assert_eq!(based.samples[0].position.z, in_band);
    let r = replay(&based, &ReplayOptions::default()).unwrap();
    assert!(
        has(&r, "based (recorded base StaticMeshActor_1)"),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(
        has(
            &r,
            "the recorded base actor is StaticMeshActor_1 (ours has no actor name here: not \
             comparable)"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert_eq!(r.trace.samples[1].position.z, in_band, "left alone");
    let unbased = recorded(dz, None);
    let r = replay(&unbased, &ReplayOptions::default()).unwrap();
    assert!(!has(&r, "based (recorded base"));
    assert_eq!(r.trace.samples[1].position.z, rest, "re-seated");
}

/// The eye height of the recording is part of the start state, and the FOV
/// of a converted recording stays out of the verdict unless asked for.
#[test]
fn eye_height_at_the_start_and_fov_out_of_the_verdict() {
    // A landing lowers the eye; the replay starts right after it (forced:
    // the pawn has just landed).
    let jump: Vec<common::Step> = (1..=120)
        .map(|i| common::Step {
            jump: (5..8).contains(&i),
            ..common::Step::default()
        })
        .collect();
    let (raw, ours) = common::scripted_steps(&jump, 100, false);
    let fake = common::fake_original(&raw, &ours);
    let landed = ours
        .samples
        .windows(2)
        .position(|w| !w[0].grounded && w[1].grounded)
        .expect("the jump lands")
        + 1;
    let timeline = StateTimeline::from_notes(&fake.meta.notes)
        .unwrap()
        .unwrap();
    let eye = timeline
        .at(landed as u64)
        .unwrap()
        .unwrap()
        .eye_height
        .unwrap();
    assert!(
        eye < 38.0,
        "the eye is below its standing height right after the landing: {eye}"
    );
    let from_landing = |use_init| {
        replay(
            &fake,
            &ReplayOptions {
                start_tick: Some(landed as u64),
                start: StartPolicy::Force,
                use_init,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    let r = from_landing(true);
    assert!(
        has(&r, &format!("eye_height {eye}")),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(
        has(
            &r,
            "our eye height equals the recorded EyeHeight after each of"
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // Without the recorded start state the eye starts at its standing
    // height, and the check shows the difference.
    let r = from_landing(false);
    assert!(!has(&r, "eye_height"));
    assert!(
        has(
            &r,
            "state check: our eye height differs from the recorded EyeHeight after"
        ),
        "{:#?}",
        r.trace.meta.notes
    );

    // The FOV: a converted recording says that its FOV column is the cached
    // view FOV, so a FOV difference does not decide the verdict by default.
    assert!(
        fake.meta
            .notes
            .iter()
            .any(|n| n.starts_with("fov: cached view FOV")),
        "{:#?}",
        fake.meta.notes
    );
    let r = replay(&fake, &ReplayOptions::default()).unwrap();
    let mut zoomed = r.trace.clone();
    for s in &mut zoomed.samples[50..] {
        s.fov = 50.0;
    }
    let tol = CompareTolerances {
        fov: 0.1,
        ..CompareTolerances::default()
    };
    let s = compare_traces("fake", &fake, "zoomed", &zoomed, &tol);
    assert_eq!(s.verdict, Verdict::Exact);
    assert!(!s.fov.as_ref().unwrap().counted);
    assert_eq!(s.diff.fov.max, 40.0, "reported all the same");
    let counted = compare_traces_with(
        "fake",
        &fake,
        "zoomed",
        &zoomed,
        &tol,
        &CompareOptions {
            fov: FovPolicy::Count,
        },
    );
    assert_eq!(counted.verdict, Verdict::Diverged);
    assert_eq!(counted.tolerances, s.tolerances, "no tolerance changed");
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
        md.contains("| walk.summary | graybox | Original → Runtime | free | 61 |"),
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

/// The command line of the same: the teleport in the listing, a replay that
/// stops before it, a one-step replay with its mode in the comparison and
/// the report, and the FOV switch.
#[test]
fn command_line_events_one_step_and_fov() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (raw, _) = common::scripted_steps(&walk_off_the_edge(330), 4_000, false);
    let raw_path = d.join("fall.raw.jsonl");
    write_raw(&raw, &raw_path);
    let trace = d.join("fall.trace.jsonl");
    run_ok(bin().arg("convert").arg(&raw_path).arg("--out").arg(&trace));
    let converted = asamu_trace::read_trace(&trace).unwrap();
    let t = recorded_events(&converted.samples, None)
        .unwrap()
        .iter()
        .find(|e| e.kind == EventKind::Teleport)
        .unwrap()
        .tick;
    let listing = run_ok(bin().arg("starts").arg(&trace).arg("--events"));
    assert!(listing.contains(&format!("{t} teleport")), "{listing}");
    let listed = run_ok(bin().args(["convert", "--list"]).arg(&raw_path));
    assert!(
        listed.contains("    event: 1 teleport(s)")
            && listed.contains(&format!("tick(s) {t}\n"))
            && listed.contains("    fov: cached view FOV"),
        "{listed}"
    );

    // Free-running: ends before the teleport and says so.
    let free = d.join("free.jsonl");
    let free_json = d.join("free.summary.json");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&trace)
            .arg("--out")
            .arg(&free)
            .args(["--compare", "--tol-angle", "1e-6", "--json"])
            .arg(&free_json),
    );
    assert!(
        text.contains(&format!(
            "{t} samples, fixed 60 Hz ticks, stopped before the event at tick {t}"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("  validity: stopped before tick {t} (teleport)")),
        "{text}"
    );
    assert!(!text.contains("mode: one-step"), "{text}");
    assert!(
        text.contains("fov: not counted in the verdict (the notes of trace a say"),
        "{text}"
    );
    let s: CompareSummary =
        serde_json::from_str(&std::fs::read_to_string(&free_json).unwrap()).unwrap();
    assert!(!s.one_step);
    assert!(
        s.harness_notes
            .iter()
            .any(|n| n.starts_with("validity: stopped before tick")),
        "{:#?}",
        s.harness_notes
    );
    assert_eq!(s.diff.matched as u64, t);

    // One step at a time: every tick but the teleport's.
    let stepped = d.join("stepped.jsonl");
    let stepped_json = d.join("stepped.summary.json");
    let text = run_ok(
        bin()
            .arg("replay")
            .arg(&trace)
            .arg("--out")
            .arg(&stepped)
            .args([
                "--one-step",
                "--compare",
                "--tol-position",
                "1e-3",
                "--tol-velocity",
                "1e-2",
                "--tol-angle",
                "1e-6",
                "--json",
            ])
            .arg(&stepped_json),
    );
    assert!(
        text.contains("330 samples, fixed 60 Hz ticks, one-step"),
        "{text}"
    );
    assert!(
        text.contains("mode: one-step (b restarts every tick from a's previous sample"),
        "{text}"
    );
    assert!(text.contains("  one-step: each of 329 tick(s)"), "{text}");
    assert!(
        text.contains("matched ticks 330, only in a 1, only in b 0"),
        "{text}"
    );
    assert!(text.contains("verdict: WithinTolerance"), "{text}");
    let s: CompareSummary =
        serde_json::from_str(&std::fs::read_to_string(&stepped_json).unwrap()).unwrap();
    assert!(s.one_step);
    assert_eq!(s.verdict, Verdict::WithinTolerance);

    // Run through on request; taken from the recording on request.
    for (policy, want) in [
        ("ignore", "warning: the replay ran through 1 event(s)"),
        ("inject", "injected: 1 recorded change(s)"),
    ] {
        let text = run_ok(
            bin()
                .arg("replay")
                .arg(&trace)
                .arg("--out")
                .arg(d.join("x.jsonl"))
                .args(["--level-events", policy]),
        );
        assert!(text.contains("331 samples"), "{policy}: {text}");
        assert!(text.contains(want), "{policy}: {text}");
    }
    let code = |cmd: &mut Command| cmd.output().unwrap().status.code();
    assert_eq!(
        code(
            bin()
                .arg("replay")
                .arg(&trace)
                .arg("--out")
                .arg(d.join("x.jsonl"))
                .args(["--level-events", "sometimes"])
        ),
        Some(2)
    );

    // The FOV switch of compare: a zoomed copy of our replay diverges only
    // when the FOV is counted.
    let mut zoomed = asamu_trace::read_trace(&free).unwrap();
    for s in &mut zoomed.samples[10..] {
        s.fov = 50.0;
    }
    let zoomed_path = d.join("zoomed.jsonl");
    asamu_trace::write_trace(&zoomed, &zoomed_path).unwrap();
    let compare_fov = |policy: Option<&str>| {
        let mut cmd = bin();
        cmd.arg("compare").arg(&trace).arg(&zoomed_path).args([
            "--tol-angle",
            "1e-6",
            "--tol-fov",
            "0.1",
            "--fail-on-divergence",
        ]);
        if let Some(p) = policy {
            cmd.args(["--fov-verdict", p]);
        }
        cmd.output().unwrap()
    };
    let out = compare_fov(None);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("(not counted: tick 10 field fov error 40)"),
        "{text}"
    );
    assert_eq!(compare_fov(Some("auto")).status.code(), Some(0));
    assert_eq!(compare_fov(Some("exclude")).status.code(), Some(0));
    let out = compare_fov(Some("count"));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("first divergence: tick 10 field fov"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The report: modes side by side, the FOV line, the replays' notes.
    let md = run_ok(bin().arg("report").arg(&free_json).arg(&stepped_json));
    assert!(
        md.contains("| free.summary | graybox | Original → Runtime | free |"),
        "{md}"
    );
    assert!(
        md.contains("| stepped.summary | graybox | Original → Runtime | one-step | 330 |"),
        "{md}"
    );
    assert!(
        md.contains("The FOV did not count for the verdict of: free.summary"),
        "{md}"
    );
    assert!(
        md.contains(&format!(
            "- free.summary: validity: stopped before tick {t} (teleport)"
        )),
        "{md}"
    );
    assert!(
        md.contains("- stepped.summary: one-step: each of 329 tick(s)"),
        "{md}"
    );
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
