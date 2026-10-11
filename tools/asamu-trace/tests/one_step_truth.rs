//! Does the one-step (resynchronised) replay report the truth?
//!
//! `pipeline.rs` shows that a one-step replay of our own run is that run.
//! That alone does not show that the mode would **report** a difference
//! correctly: a harness that starts a tick from the wrong sample, steps it
//! with a neighbouring frame length, writes the recorded script state a
//! tick late or early, or drops the start sample's input can still
//! reproduce a run in which nothing changes. These tests give it
//! "originals" that differ from our simulation in a known way and check
//! that the one-step errors are the ones the rule predicts, tick by tick:
//!
//! - an original with another gravity (a **test-only copy** of the
//!   parameters; the crates' are untouched): every falling tick is off by
//!   the amount the falling rule gives for that tick's own frame length,
//!   and by nothing more;
//! - an original whose `JumpZ` a level script changes for one record only:
//!   the tick after that record, and no other, uses it;
//! - a start sample with a held button: the release edge of the next tick
//!   is our simulation's to find, and when the start sample's input is
//!   missing the state check says so at that tick;
//! - a recording whose inputs are paired with the states one tick late: the
//!   errors sit at the ticks where the input changes, not everywhere and
//!   not nowhere;
//! - a recording that passes through our collision: the ticks that start
//!   inside it are listed.
//!
//! Expected values come from the rules (NATIVE_PHYSICS.md 4.3 and 4.6 for
//! falling, ABILITIES.md for the jump and the sprint speeds), never from a
//! replay of our own code.

#![allow(clippy::unwrap_used)]

mod common;

use asamu_player::trace::{CompareTolerances, compare};
use asamu_player::{PlayerParams, Trace, TraceSample};
use asamu_trace::compare::{Verdict, compare_traces};
use asamu_trace::replay::{ReplayOptions, ReplayResult, Stepping, replay};
use asamu_trace::state::StateTimeline;

fn one_step() -> ReplayOptions {
    ReplayOptions {
        one_step: true,
        ..ReplayOptions::default()
    }
}

fn has(r: &ReplayResult, text: &str) -> bool {
    r.trace.meta.notes.iter().any(|n| n.contains(text))
}

/// A standing jump with the button held until long after the landing (no
/// release while rising, so no jump damping), nothing else.
fn standing_jump(n: usize) -> Vec<common::Step> {
    (1..=n)
        .map(|i| common::Step {
            jump: i >= 5,
            ..common::Step::default()
        })
        .collect()
}

/// The world gravity of the test-only "original" (ours: the class and
/// config value of `PlayerParams::asamu_original()`).
const WRONG_GRAVITY_Z: f32 = -624.0;

/// What the falling rule gives for one tick of free fall that starts from
/// the same state with gravity `g` (ours) and `g_wrong` (the original's),
/// ours minus the original's (NATIVE_PHYSICS.md 4.3 and 4.6: the move uses
/// `V0 + g·dt`, the velocity is refined to `V0 + 2·g·dt`):
/// `(Δz, ΔVz) = ((g − g')·dt², 2·(g − g')·dt)`.
fn predicted_fall_difference(g: f64, g_wrong: f64, dt: f64) -> (f64, f64) {
    ((g - g_wrong) * dt * dt, 2.0 * (g - g_wrong) * dt)
}

/// Compares every tick of `ours` (a replay of `original`) in which the pawn
/// is in free fall before and after on both sides with
/// [`predicted_fall_difference`] for that tick's own frame length.
struct FallCheck {
    /// Ticks compared.
    checked: usize,
    /// Largest |ΔVz| among them.
    largest: f64,
    /// Ticks whose difference is not the predicted one (within f32
    /// rounding), first one described.
    misfits: usize,
    first_misfit: Option<String>,
}

fn check_fall_ticks(original: &Trace, ours: &Trace) -> FallCheck {
    let p = PlayerParams::asamu_original();
    let g = f64::from(p.movement.world_gravity_z.value)
        * f64::from(p.movement.custom_gravity_scaling.value);
    let g_wrong = f64::from(WRONG_GRAVITY_Z) * f64::from(p.movement.custom_gravity_scaling.value);
    assert!(g != g_wrong);
    assert_eq!(original.samples.len(), ours.samples.len());
    let mut out = FallCheck {
        checked: 0,
        largest: 0.0,
        misfits: 0,
        first_misfit: None,
    };
    for k in 1..original.samples.len() {
        let (before, a, b): (&TraceSample, &TraceSample, &TraceSample) = (
            &original.samples[k - 1],
            &original.samples[k],
            &ours.samples[k],
        );
        assert_eq!((a.tick, a.input), (b.tick, b.input));
        if before.grounded || a.grounded || b.grounded {
            continue;
        }
        // The frame length of this tick: the recording's own.
        let dt = a.time - before.time;
        assert!(dt > 0.0 && dt < f64::from(asamu_player::MAX_STEP_DT));
        let (want_dz, want_dvz) = predicted_fall_difference(g, g_wrong, dt);
        let dz = f64::from(b.position.z) - f64::from(a.position.z);
        let dvz = f64::from(b.velocity.z) - f64::from(a.velocity.z);
        // f32 positions below 512 uu are 3e-5 apart; a velocity is twice a
        // displacement over the frame length, so 4e-3 uu/s per side.
        // Gravity is vertical: nothing sideways.
        let fits = (dz - want_dz).abs() < 2e-4
            && (dvz - want_dvz).abs() < 0.02
            && (b.position.x, b.position.y, b.velocity.x, b.velocity.y)
                == (a.position.x, a.position.y, a.velocity.x, a.velocity.y);
        if !fits {
            out.misfits += 1;
            out.first_misfit.get_or_insert_with(|| {
                format!(
                    "tick {}: dz {dz} for {want_dz}, dVz {dvz} for {want_dvz} (dt {dt})",
                    a.tick
                )
            });
        }
        out.checked += 1;
        out.largest = out.largest.max(dvz.abs());
    }
    out
}

/// An original that falls under another gravity than ours. One step at a
/// time, every falling tick is off by exactly what the rule gives for one
/// tick; the free-running replay piles the same difference up.
#[test]
fn a_wrong_gravity_shows_as_the_predicted_error_of_each_tick() {
    let steps = standing_jump(200);
    let wrong = || common::graybox_with(|p| p.movement.world_gravity_z.value = WRONG_GRAVITY_Z);
    assert_eq!(
        PlayerParams::asamu_original()
            .movement
            .world_gravity_z
            .value,
        -520.0,
        "the crates' own parameters are not what this test changes"
    );

    // Fixed 60 Hz.
    let (raw, theirs) = common::scripted_steps_on(wrong(), &steps, 4_000, false, |_, _| {});
    let fake = common::fake_original(&raw, &theirs);
    assert_eq!(fake.meta.tick_rate, Some(60.0));
    let stepped = replay(&fake, &one_step()).unwrap();
    assert!(matches!(stepped.stepping, Stepping::Fixed(_)));
    let mut timed = stepped.trace.clone();
    for (s, o) in timed.samples.iter_mut().zip(&fake.samples) {
        s.time = o.time;
    }
    let c = check_fall_ticks(&fake, &timed);
    assert_eq!(c.misfits, 0, "fixed: {:?}", c.first_misfit);
    // The flight lasts 2·JumpZ / (2·|g'|) = 1000 / 624 s = 96 ticks.
    assert!(c.checked >= 90, "{}", c.checked);
    let dt = f64::from(1.0_f32 / 60.0);
    let per_tick = 2.0 * 104.0 * dt;
    assert!((c.largest - per_tick).abs() < 0.02, "{}", c.largest);
    // The same original, free-running: the difference of every tick adds
    // up (2·104 uu/s² over about 1.5 s of common flight).
    let free = replay(&fake, &ReplayOptions::default()).unwrap();
    let d = compare(&fake, &free.trace);
    assert!(d.velocity.max > 80.0 * per_tick, "{}", d.velocity.max);
    // Both replays say what they are.
    assert!(has(&stepped, "one-step: each of 200 tick(s)"));
    assert!(!has(&free, "one-step:"));
    // Nothing of this is a collision matter.
    assert!(stepped.resync_overlaps.is_empty());

    // Every frame with its own length: each tick is off by the amount of
    // its own frame length, so a neighbouring length would not fit
    // (1040 uu/s² × the difference of two lengths, 0.5 uu/s at this jitter).
    let mut r = common::Lcg(77);
    let mut lengths: Vec<f32> = Vec::new();
    while lengths.len() < steps.len() {
        let d = 1.0 / 60.0 * (1.0 + r.f32(0.06));
        if lengths
            .last()
            .is_none_or(|last: &f32| (last - d).abs() > 2e-5)
        {
            lengths.push(d);
        }
    }
    let (raw, theirs) = common::scripted_variable_on(&wrong(), &steps, &lengths, 4_000);
    let fake = common::fake_original(&raw, &theirs);
    assert_eq!(fake.meta.tick_rate, None);
    let stepped = replay(&fake, &one_step()).unwrap();
    assert_eq!(stepped.stepping, Stepping::PerSample);
    let c = check_fall_ticks(&fake, &stepped.trace);
    assert_eq!(c.misfits, 0, "per sample: {:?}", c.first_misfit);
    assert!(c.checked >= 90, "{}", c.checked);
    let longest = lengths.iter().copied().fold(0.0_f32, f32::max);
    assert!(c.largest <= 2.0 * 104.0 * f64::from(longest) + 0.02);
    // The prediction is that sharp: with the frame length of the tick
    // before, most ticks would be off by more than the check allows.
    let neighbour_fits = (2..lengths.len())
        .filter(|k| 2.0 * 520.0 * f64::from((lengths[*k] - lengths[*k - 1]).abs()) < 0.02)
        .count();
    assert!(neighbour_fits < lengths.len() / 20, "{neighbour_fits}");
    let s = compare_traces(
        "fake",
        &fake,
        "one-step",
        &stepped.trace,
        &Default::default(),
    );
    assert!(s.one_step && s.timing.unwrap().aligned());
    assert_eq!(s.verdict, Verdict::Diverged);
    // And the check does catch a step with another length: the same
    // one-step replay forced to ticks of 1/60 s (every tick then runs with
    // a length that is not its own) misses the prediction on most ticks,
    // and the comparison says that the frame lengths differ.
    let forced = replay(
        &fake,
        &ReplayOptions {
            tick_rate: Some(60.0),
            ..one_step()
        },
    )
    .unwrap();
    let mut timed = forced.trace.clone();
    for (s, o) in timed.samples.iter_mut().zip(&fake.samples) {
        s.time = o.time;
    }
    let c = check_fall_ticks(&fake, &timed);
    assert!(
        c.misfits * 2 > c.checked,
        "{} of {} ticks miss the prediction",
        c.misfits,
        c.checked
    );
    let s = compare_traces("fake", &fake, "forced", &forced.trace, &Default::default());
    assert!(!s.timing.unwrap().aligned());
}

/// A level script changes `JumpZ` for exactly one record, the one before
/// the jump. The tick of the jump starts from that record, so it (and no
/// other) must use the changed value: a resynchronisation that wrote the
/// state a tick late or a tick early would jump with the usual `JumpZ`.
#[test]
fn the_recorded_state_of_the_previous_sample_is_the_one_a_tick_starts_with() {
    const JUMP_TICK: usize = 30;
    const SCRIPTED_JUMP_Z: f32 = 700.0;
    // The JumpZ our pawn has before the script changes it (the class default).
    let usual_seen = std::cell::Cell::new(f32::NAN);
    let steps: Vec<common::Step> = (1..=120)
        .map(|i| common::Step {
            jump: i >= JUMP_TICK,
            ..common::Step::default()
        })
        .collect();
    let game = asamu_game::Game::graybox().unwrap();
    let (raw, theirs) = common::scripted_steps_on(game, &steps, 800, false, |tick, g| {
        // In record 29 only (after tick 29, before tick 30's jump; back
        // after it).
        if tick == JUMP_TICK - 1 {
            usual_seen.set(g.player().script.jump_z);
            g.player_mut().script.jump_z = SCRIPTED_JUMP_Z;
        } else if tick == JUMP_TICK {
            g.player_mut().script.jump_z = usual_seen.get();
        }
    });
    let usual = usual_seen.get();
    // ABILITIES.md A-JP-1: the jump is 1000 uu/s.
    assert_eq!(usual, 1000.0);
    let fake = common::fake_original(&raw, &theirs);
    let timeline = StateTimeline::from_notes(&fake.meta.notes)
        .unwrap()
        .unwrap();
    let jump_z = |tick: usize| timeline.at(tick as u64).unwrap().unwrap().jump_z.unwrap();
    assert_eq!(
        (
            jump_z(JUMP_TICK - 2),
            jump_z(JUMP_TICK - 1),
            jump_z(JUMP_TICK)
        ),
        (usual, SCRIPTED_JUMP_Z, usual)
    );
    assert!(fake.samples[JUMP_TICK - 1].grounded && !fake.samples[JUMP_TICK].grounded);
    // A JumpZ change is not one of the events that end a replay's validity.
    let stepped = replay(&fake, &one_step()).unwrap();
    assert!(stepped.events.is_empty());
    assert_eq!(stepped.trace.samples.len(), fake.samples.len());
    // One step at a time: the jump tick starts from record 29, with its
    // JumpZ. Every tick is the original's.
    let d = compare(&fake, &stepped.trace);
    assert_eq!(
        (d.position.max, d.velocity.max, d.grounded_mismatches),
        (0.0, 0.0, 0),
        "{:?}",
        stepped.trace.meta.notes
    );
    // Without the recorded script state (`use_init` off: position,
    // velocity, view and physics mode only) the same one-step replay jumps
    // with the usual JumpZ: this recording does tell the two apart.
    let bare = replay(
        &fake,
        &ReplayOptions {
            use_init: false,
            ..one_step()
        },
    )
    .unwrap();
    let d = compare(&fake, &bare.trace);
    assert_eq!(d.first_divergence.map(|x| x.tick), Some(JUMP_TICK as u64));
    // Free-running, our pawn never hears of the change (its start state is
    // record 0's) and jumps with the usual JumpZ: the take-off velocities
    // differ by the difference of the two values (a jump sets the vertical
    // velocity to JumpZ; the rest of the tick is the same on both sides).
    let free = replay(&fake, &ReplayOptions::default()).unwrap();
    let (a, b) = (&fake.samples[JUMP_TICK], &free.trace.samples[JUMP_TICK]);
    let dvz = f64::from(b.velocity.z) - f64::from(a.velocity.z);
    let want = f64::from(usual) - f64::from(SCRIPTED_JUMP_Z);
    assert!((dvz - want).abs() < 0.05, "{dvz} for {want}");
    let before = compare(&fake, &free.trace);
    assert_eq!(
        before.first_divergence.map(|x| x.tick),
        Some(JUMP_TICK as u64)
    );
}

/// The start sample's input is the level of the buttons before the first
/// replayed tick. With the sprint button held in it and released in the
/// next tick, our simulation finds the release itself; without it (the
/// sample's input dropped) it cannot, and the replay says so at that tick.
#[test]
fn the_start_samples_input_gives_the_first_tick_its_button_edges() {
    // The release is in the first replayed tick: its edge is the start
    // sample's level against that tick's (the sprint is edge-driven).
    const START: u64 = 30;
    const RELEASE: u64 = 31;
    let steps: Vec<common::Step> = (1..=110)
        .map(|i| common::Step {
            sprint: i < RELEASE as usize,
            forward: i32::from(i >= 40),
            ..common::Step::default()
        })
        .collect();
    let (raw, ours) = common::scripted_steps(&steps, 300, false);
    let fake = common::fake_original(&raw, &ours);
    let pawn = PlayerParams::asamu_original().pawn.unwrap();
    let walk = f64::from(pawn.move_speed.value);
    let sprint = walk * f64::from(pawn.sprint_speed_multiplier.value);
    assert!(fake.samples[START as usize].input.sprint_held);
    assert!(!fake.samples[RELEASE as usize].input.sprint_held);
    let from_start = |trace: &Trace, one_step: bool| {
        replay(
            trace,
            &ReplayOptions {
                start_tick: Some(START),
                one_step,
                ..ReplayOptions::default()
            },
        )
        .unwrap()
    };
    let speed = |r: &ReplayResult, tick: u64| {
        let s = r.trace.samples.iter().find(|s| s.tick == tick).unwrap();
        f64::from(s.velocity.truncate().length())
    };
    for mode in [false, true] {
        let r = from_start(&fake, mode);
        assert!(has(&r, "buttons held: sprint"), "{:#?}", r.trace.meta.notes);
        let d = compare(&fake, &r.trace);
        assert_eq!((d.position.max, d.velocity.max), (0.0, 0.0), "{mode}");
        // The pawn walks at the walking speed after the release (class
        // default fMoveSpeed), not at the sprint speed.
        assert!((speed(&r, 110) - walk).abs() < 0.01, "{}", speed(&r, 110));
        assert!(
            has(
                &r,
                "state check: after each of 80 tick(s) our GroundSpeed, AirControl, the sprint \
                 flag"
            ),
            "{mode}: {:#?}",
            r.trace.meta.notes
        );
    }

    // The same recording with the start sample's input lost.
    let mut dropped = fake.clone();
    dropped.samples[START as usize].input.sprint_held = false;
    // Free-running, our pawn never stops sprinting.
    let r = from_start(&dropped, false);
    assert!((speed(&r, 110) - sprint).abs() < 0.01, "{}", speed(&r, 110));
    assert!(
        has(
            &r,
            &format!(
                "state check: the sprint flag differs from the recording's after 80 of 80 \
                 tick(s) (first at tick {RELEASE}: ours 1, recorded 0)"
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    // One step at a time the recorded flag is written before every later
    // tick, so the walk is the recording's; but the tick of the release is
    // ours alone, and the state check reports it: once, at that tick.
    let r = from_start(&dropped, true);
    assert!((speed(&r, 110) - walk).abs() < 0.01, "{}", speed(&r, 110));
    assert!(
        has(
            &r,
            &format!(
                "state check: the sprint flag differs from the recording's after 1 of 80 \
                 tick(s) (first at tick {RELEASE}: ours 1, recorded 0)"
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
    assert!(
        has(
            &r,
            &format!(
                "state check: GroundSpeed differs from the recording's after 1 of 80 tick(s) \
                 (first at tick {RELEASE}: ours {sprint}, recorded {walk})"
            )
        ),
        "{:#?}",
        r.trace.meta.notes
    );
}

/// A recording whose inputs are paired with the states one tick late (each
/// sample carries the input of the tick before). One step at a time, a tick
/// can only be wrong where that input differs from the right one: at the
/// ticks where the input changes. The mode neither hides the misalignment
/// nor spreads it over the run.
#[test]
fn a_recording_misaligned_by_one_tick_is_wrong_where_the_input_changes() {
    // Walking only (no jump, no grapple: their state codes are not
    // resynchronised and would carry a wrong tick on).
    let steps: Vec<common::Step> = (1..=90)
        .map(|i| common::Step {
            forward: i32::from((5..=60).contains(&i)),
            right: i32::from((25..40).contains(&i)),
            d_yaw: if i == 12 { 2048 } else { 0 },
            ..common::Step::default()
        })
        .collect();
    let (raw, ours) = common::scripted_steps(&steps, 600, false);
    let fake = common::fake_original(&raw, &ours);
    let mut late = fake.clone();
    for k in (1..late.samples.len()).rev() {
        late.samples[k].input = fake.samples[k - 1].input;
    }
    // Where the late input is not the right one: from the script alone.
    let changes: Vec<u64> = (1..fake.samples.len())
        .filter(|k| fake.samples[*k].input != fake.samples[*k - 1].input)
        .map(|k| k as u64)
        .collect();
    assert_eq!(changes, [5, 12, 13, 25, 40, 61]);
    let wrong_ticks = |r: &ReplayResult| -> Vec<u64> {
        r.trace
            .samples
            .iter()
            .zip(&late.samples)
            .filter(|(b, a)| {
                assert_eq!(a.tick, b.tick);
                (b.position, b.velocity, b.yaw, b.pitch, b.grounded)
                    != (a.position, a.velocity, a.yaw, a.pitch, a.grounded)
            })
            .map(|(b, _)| b.tick)
            .collect()
    };
    let stepped = replay(&late, &one_step()).unwrap();
    let wrong = wrong_ticks(&stepped);
    assert_eq!(wrong.first(), Some(&5), "the walk begins a tick late");
    assert!(
        wrong.iter().all(|t| changes.contains(t)),
        "{wrong:?} outside {changes:?}"
    );
    assert!(wrong.len() >= 4, "{wrong:?}");
    let s = compare_traces(
        "late",
        &late,
        "one-step",
        &stepped.trace,
        &Default::default(),
    );
    assert_eq!(s.verdict, Verdict::Diverged);
    assert_eq!(s.diff.input_mismatches, 0, "the inputs are the recording's");
    // Free-running the first wrong tick is the same, and the run never
    // comes back.
    let free = replay(&late, &ReplayOptions::default()).unwrap();
    let wrong_free = wrong_ticks(&free);
    assert_eq!(wrong_free.first(), Some(&5));
    assert!(wrong_free.len() > 60, "{}", wrong_free.len());
    // The recording as it should be is reproduced in both modes.
    for opts in [one_step(), ReplayOptions::default()] {
        let r = replay(&fake, &opts).unwrap();
        let d = compare(&fake, &r.trace);
        assert_eq!((d.position.max, d.velocity.max, d.yaw.max), (0.0, 0.0, 0.0));
    }
}

/// A recording that passes through our collision (the original walks where
/// ours has geometry): a one-step replay lists the ticks that start inside
/// it, because their errors are not one tick of our movement rules.
#[test]
fn one_step_ticks_that_start_inside_our_collision_are_listed() {
    let steps: Vec<common::Step> = (1..=70)
        .map(|i| common::Step {
            forward: i32::from(i >= 5),
            ..common::Step::default()
        })
        .collect();
    let (raw, ours) = common::scripted_steps(&steps, 100, false);
    let fake = common::fake_original(&raw, &ours);
    // As recorded, no tick starts inside our collision.
    let clean = replay(&fake, &one_step()).unwrap();
    assert!(clean.resync_overlaps.is_empty());
    assert!(!has(&clean, "start inside our collision"));
    // Samples 30..=40 are 10 uu inside the start platform (the half height
    // of the pawn is 44 and it rests 2.15 uu above the floor; 10 uu in one
    // tick is no teleport).
    let mut sunk = fake.clone();
    for s in &mut sunk.samples[30..=40] {
        s.position.z -= 10.0;
    }
    let stepped = replay(&sunk, &one_step()).unwrap();
    // The ticks that start from those samples.
    assert_eq!(stepped.resync_overlaps, (31..=41).collect::<Vec<u64>>());
    assert!(!stepped.start_overlaps, "the start itself is free");
    assert!(
        has(
            &stepped,
            "one-step: warning: 11 of 70 tick(s) start inside our collision (the pawn's shape at \
             the recording's previous sample overlaps our geometry; first tick 31)"
        ) && has(
            &stepped,
            "Tick(s) 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41"
        ),
        "{:#?}",
        stepped.trace.meta.notes
    );
    // The comparison carries the warning with it.
    let tol = CompareTolerances::default();
    let s = compare_traces("sunk", &sunk, "one-step", &stepped.trace, &tol);
    assert!(
        s.harness_notes
            .iter()
            .any(|n| n.starts_with("one-step: warning: 11 of 70 tick(s) start inside")),
        "{:#?}",
        s.harness_notes
    );
    // Every tick that starts from a free sample of the walk before the
    // sunk stretch is still the recording's; the errors begin at tick 30,
    // where the recording itself goes into the floor.
    let first_wrong = stepped
        .trace
        .samples
        .iter()
        .zip(&sunk.samples)
        .find(|(b, a)| b.position != a.position)
        .map(|(b, _)| b.tick);
    assert_eq!(first_wrong, Some(30));
    // A free-running replay restarts nowhere: it has no such list.
    let free = replay(&sunk, &ReplayOptions::default()).unwrap();
    assert!(free.resync_overlaps.is_empty());
    assert!(!has(&free, "start inside our collision"));
}
