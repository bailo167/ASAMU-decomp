//! The first gameplay slice, scripted end to end:
//! walk → jump → grapple target → grapple → swing/accelerate → release →
//! preserve momentum → land. Placeholder physics; this checks the pipeline and
//! determinism, not parity with the original.

mod common;

use asamu_player::grapple::eye_position;
use asamu_player::trace::{TraceSample, record_run};
use asamu_player::{
    BoxWorld, GrappleEvent, InputFrame, PlayerParams, PlayerState, StepEvents, Trace, TraceMeta,
    compare, step,
};
use common::{DT, look_at, standing, standing_z};
use glam::Vec3;

/// Hand-made test geometry (not original content).
fn slice_world() -> BoxWorld {
    BoxWorld::new()
        // Pit floor far below, so a failed run terminates.
        .with_ground(-3000.0, false)
        // Start platform, ending at x = 100.
        .with_box(
            Vec3::new(-800.0, -400.0, -100.0),
            Vec3::new(100.0, 400.0, 0.0),
            false,
        )
        // Grapple hook block above the gap.
        .with_box(
            Vec3::new(1150.0, -50.0, 850.0),
            Vec3::new(1250.0, 50.0, 950.0),
            true,
        )
        // Landing platform, lower than the start.
        .with_box(
            Vec3::new(1900.0, -1500.0, -400.0),
            Vec3::new(6000.0, 1500.0, -300.0),
            false,
        )
}

const HOOK_CENTER: Vec3 = Vec3::new(1200.0, 0.0, 900.0);
const START_X: f32 = -400.0;
const WALK_TICKS: u64 = 60;
const JUMP_TICK: u64 = WALK_TICKS + 1;
const GRAPPLE_TICK: u64 = JUMP_TICK + 9;
const MAX_TICKS: u64 = 1200;

struct SliceRun {
    trace: Trace,
    events: Vec<(u64, StepEvents)>,
    release: Option<(u64, PlayerState, Vec3)>,
}

/// Runs the scripted slice. The script reads the state only to decide its
/// next input (aim at the hook, release after the bottom of the swing), so the
/// recorded inputs fully determine the run.
fn run_slice(params: &PlayerParams, world: &BoxWorld) -> SliceRun {
    let fov = params.camera.fov_degrees.value;
    let mut s = standing(params, world, START_X, 0.0, 0.0);
    let mut trace = Trace::new(TraceMeta::runtime(
        Some("slice-test (hand-made)".into()),
        Some(60.0),
    ));
    trace.samples.push(TraceSample::capture(
        0,
        0.0,
        &InputFrame::default(),
        &s,
        fov,
    ));
    let mut events = Vec::new();
    let mut release = None;
    let mut attached_once = false;
    let mut landed_after_release: Option<u64> = None;

    for tick in 1..=MAX_TICKS {
        let mut input = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        if tick == JUMP_TICK {
            input.jump_pressed = true;
            input.jump_held = true;
        }
        if tick == GRAPPLE_TICK {
            let (yaw, pitch) = look_at(eye_position(&s, params), HOOK_CENTER);
            input.look_yaw_delta = yaw - s.yaw;
            input.look_pitch_delta = pitch - s.pitch;
        }
        let attached = s.grapple.is_attached();
        let past_bottom = attached
            && s.grapple
                .anchor()
                .is_some_and(|a| s.position.x > a.x + 250.0)
            && s.velocity.z > 0.0;
        input.grapple_held = tick >= GRAPPLE_TICK && !past_bottom && (attached || !attached_once);
        if release.is_some() {
            input.grapple_held = false;
        }

        let before = s;
        let ev = step(&mut s, &input, params, world, DT);
        if let Some(GrappleEvent::Attached { .. }) = ev.grapple {
            attached_once = true;
        }
        if let Some(GrappleEvent::Released { velocity }) = ev.grapple {
            release = Some((tick, before, velocity));
        }
        if release.is_some() && ev.landed.is_some() && landed_after_release.is_none() {
            landed_after_release = Some(tick);
        }
        trace.samples.push(TraceSample::capture(
            tick,
            tick as f64 * f64::from(DT),
            &input,
            &s,
            fov,
        ));
        events.push((tick, ev));
        // Stand around for half a second after landing, then stop.
        if landed_after_release.is_some_and(|t| tick >= t + 30) {
            break;
        }
    }
    SliceRun {
        trace,
        events,
        release,
    }
}

fn first_tick(events: &[(u64, StepEvents)], pred: impl Fn(&StepEvents) -> bool) -> Option<u64> {
    events.iter().find(|(_, e)| pred(e)).map(|(t, _)| *t)
}

fn describe(run: &SliceRun) -> String {
    let mut out = String::new();
    for s in run.trace.samples.iter().step_by(10) {
        out.push_str(&format!(
            "t{:4} pos({:8.1},{:6.1},{:8.1}) vel({:7.1},{:6.1},{:7.1}) g={} {:?}\n",
            s.tick,
            s.position.x,
            s.position.y,
            s.position.z,
            s.velocity.x,
            s.velocity.y,
            s.velocity.z,
            s.grounded,
            s.grapple_state
        ));
    }
    out
}

#[test]
fn scripted_slice_walk_jump_grapple_swing_release_land() {
    let params = PlayerParams::default();
    let world = slice_world();
    let run = run_slice(&params, &world);
    let diag = describe(&run);

    let jumped = first_tick(&run.events, |e| e.jumped).expect("jumped");
    let attached = first_tick(&run.events, |e| {
        matches!(e.grapple, Some(GrappleEvent::Attached { .. }))
    })
    .unwrap_or_else(|| panic!("attached\n{diag}"));
    let (release_tick, before_release, released_velocity) =
        run.release.unwrap_or_else(|| panic!("released\n{diag}"));
    let landed = run
        .events
        .iter()
        .find(|(t, e)| *t > release_tick && e.landed.is_some())
        .map(|(t, _)| *t)
        .unwrap_or_else(|| panic!("landed after release\n{diag}"));
    assert_eq!(jumped, JUMP_TICK);
    assert_eq!(
        attached, GRAPPLE_TICK,
        "grapple fired on the aim tick\n{diag}"
    );
    assert!(jumped < attached && attached < release_tick && release_tick < landed);
    assert!(
        first_tick(&run.events, |e| matches!(
            e.grapple,
            Some(GrappleEvent::Missed { .. })
        ))
        .is_none(),
        "no misses"
    );

    // Walking reached speed before the jump.
    let pre_jump = &run.trace.samples[usize::try_from(WALK_TICKS).unwrap()];
    assert!(pre_jump.grounded);
    assert!(
        (pre_jump.velocity.truncate().length() - params.movement.max_ground_speed.value).abs()
            < 1e-2
    );

    // The swing accelerated the player beyond walking speed.
    let swing_peak = run
        .trace
        .samples
        .iter()
        .filter(|s| s.tick > attached && s.tick < release_tick)
        .map(|s| s.velocity.length())
        .fold(0.0_f32, f32::max);
    assert!(
        swing_peak > 2.0 * params.movement.max_ground_speed.value,
        "swing peak {swing_peak}\n{diag}"
    );

    // Momentum preserved: the released velocity equals the pre-release velocity
    // bit for bit, and the horizontal velocity is unchanged on the release tick.
    assert_eq!(
        released_velocity.to_array().map(f32::to_bits),
        before_release.velocity.to_array().map(f32::to_bits)
    );
    let at_release = &run.trace.samples[usize::try_from(release_tick).unwrap()];
    assert_eq!(at_release.grapple_anchor, None);
    let horizontal_after = at_release.velocity.truncate();
    let horizontal_before = before_release.velocity.truncate();
    let wish_gain =
        params.movement.air_control.value * params.movement.ground_acceleration.value * DT;
    assert!(
        (horizontal_after - horizontal_before).length() <= wish_gain + 1e-3,
        "only air control may change horizontal velocity on the release tick"
    );

    // Landed on the landing platform, carried by the swing's momentum.
    let last = run.trace.samples.last().unwrap();
    assert!(last.grounded, "{diag}");
    assert!(
        (last.position.z - standing_z(&params, -300.0)).abs() < 1e-3,
        "{diag}"
    );
    assert!(last.position.x > 1900.0, "{diag}");

    // Trace round trip through JSON Lines is lossless and compares as exact.
    let jsonl = run.trace.to_jsonl_string().unwrap();
    let back = Trace::from_jsonl_str(&jsonl).unwrap();
    assert_eq!(back, run.trace);
    let diff = compare(&run.trace, &back);
    assert!(diff.is_exact(), "{diff:?}");
    assert_eq!(diff.matched, run.trace.samples.len());
    assert_eq!(diff.first_divergence, None);
    assert_eq!(diff.position.max, 0.0);
    assert_eq!(diff.velocity.rms, 0.0);

    // Replaying only the recorded inputs reproduces the trace exactly.
    let inputs: Vec<InputFrame> = run.trace.samples.iter().skip(1).map(|s| s.input).collect();
    let initial = standing(&params, &world, START_X, 0.0, 0.0);
    let replay = record_run(
        run.trace.meta.clone(),
        &initial,
        &inputs,
        &params,
        &world,
        DT,
    );
    let diff = compare(&run.trace, &replay);
    assert!(diff.is_exact(), "replay diverged: {diff:?}");
}

#[test]
fn slice_is_deterministic_to_the_byte() {
    let params = PlayerParams::default();
    let world = slice_world();
    let a = run_slice(&params, &world).trace.to_jsonl_string().unwrap();
    let b = run_slice(&params, &world).trace.to_jsonl_string().unwrap();
    assert!(a.len() > 10_000);
    assert_eq!(a.as_bytes(), b.as_bytes());
}
