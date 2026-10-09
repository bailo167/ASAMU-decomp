//! The ASAMU pawn script layer (`asamu_player::pawn`) against the rules of
//! `docs/reverse-engineering/ABILITIES.md` (rule ids in the test names and
//! comments), run with the original parameters and the native-physics port.
//! Expected numbers are derived from the spec's formulas (closed form or the
//! spec's own re-simulated figures, ABILITIES.md §3 "Model").

mod common;

use asamu_player::grapple;
use asamu_player::movement::Landing;
use asamu_player::pawn::{
    self, JUMP_RELEASE_MULTIPLIER, LandCue, LandingHandler, PawnScript, PawnStateName,
    PowerJumpEvent, PowerJumpStateName, ZOOM_STEP,
};
use asamu_player::{
    BoxWorld, InputFrame, PlaceholderMovement, PlayerParams, PlayerState, StepEvents,
    Ue3PawnMovement, step_with,
};
use common::{DT, SplitMix64, flat_world};
use glam::Vec3;

fn original() -> PlayerParams {
    PlayerParams::asamu_original()
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, DT);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

/// A started pawn standing on `world` at the origin (one settling tick, so
/// the native 2.15 uu hover is established).
fn standing(params: &PlayerParams, world: &BoxWorld) -> PlayerState {
    let mut s = common::standing(params, world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, params);
    tick(&mut s, &InputFrame::default(), params, world);
    assert!(s.grounded);
    s
}

/// A started pawn falling freely at `z` with `velocity`.
fn airborne(params: &PlayerParams, z: f32, velocity: Vec3) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, z), 0.0);
    pawn::start(&mut s, params);
    s.velocity = velocity;
    s
}

fn input() -> InputFrame {
    InputFrame::default()
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

fn sprint_forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        sprint_held: true,
        ..InputFrame::default()
    }
}

fn hspeed(s: &PlayerState) -> f32 {
    s.horizontal_speed()
}

fn pawn_params(p: &PlayerParams) -> &asamu_player::PawnParams {
    p.pawn.as_ref().expect("original params have pawn params")
}

/// Runs until grounded (at most `max` ticks); returns the events of the
/// landing tick.
fn until_landed(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
    max: usize,
) -> StepEvents {
    for _ in 0..max {
        let e = tick(s, input, params, world);
        if s.grounded {
            return e;
        }
    }
    panic!("did not land: {s:?}");
}

// ---------------------------------------------------------------------------
// Start, walking, sprint (A-WK-1…4).
// ---------------------------------------------------------------------------

#[test]
fn pawn_start_sets_the_original_run_time_values() {
    let params = original();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
    assert!(!s.script.started);
    pawn::start(&mut s, &params);
    let sc = &s.script;
    assert!(sc.started);
    assert_eq!(sc.ground_speed, 440.0, "GroundSpeed = MoveSpeed");
    assert_eq!(sc.air_control, 0.3);
    assert_eq!(sc.jump_z, 1000.0);
    assert_eq!(sc.eye_height, 38.0);
    assert_eq!(sc.fov, 90.0);
    assert!(sc.zoom_enabled);
    assert_eq!(sc.code.state, PawnStateName::Idle);
    assert_eq!(sc.power_jump.state, PowerJumpStateName::Ready);
    assert_eq!(sc.move_input_lock, 0);
    assert_eq!(s.view_location(&params), Vec3::new(0.0, 0.0, 138.0));
    assert_eq!(s.fov(&params), 90.0);
    // Without pawn params nothing happens.
    let mut raw = PlayerState::new(Vec3::ZERO, 0.0);
    pawn::start(&mut raw, &PlayerParams::placeholder());
    assert!(!raw.script.started);
}

#[test]
fn a_wk_walk_440_sprint_880_and_release() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    for _ in 0..60 {
        tick(&mut s, &forward(), &params, &world);
        // Walking velocity is re-derived from the f32 displacement.
        assert!(hspeed(&s) <= 440.0 + 0.02, "{}", hspeed(&s));
    }
    assert!((hspeed(&s) - 440.0).abs() < 0.05, "{}", hspeed(&s));
    // Sprint press while walking applies 880 at once.
    tick(&mut s, &sprint_forward(), &params, &world);
    assert!(s.script.sprint.active && !s.script.sprint.armed);
    assert_eq!(s.script.ground_speed, 880.0);
    for _ in 0..60 {
        tick(&mut s, &sprint_forward(), &params, &world);
        assert!(hspeed(&s) <= 880.0 + 0.02, "{}", hspeed(&s));
    }
    assert!((hspeed(&s) - 880.0).abs() < 0.05, "{}", hspeed(&s));
    // Release: back to 440, the walking cap applies at once.
    tick(&mut s, &forward(), &params, &world);
    assert!(!s.script.sprint.active && !s.script.sprint.armed);
    assert_eq!(s.script.ground_speed, 440.0);
    assert!(hspeed(&s) <= 440.0 + 0.02, "{}", hspeed(&s));
}

#[test]
fn a_wk_sprint_applies_when_standing_and_analog_magnitude_is_ignored() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let sprint = InputFrame {
        sprint_held: true,
        ..input()
    };
    tick(&mut s, &sprint, &params, &world);
    assert!(s.script.sprint.active, "rule 2: also when standing still");
    // A-WK-1: the acceleration is normalised, so a light stick tilt walks
    // as fast as a full one.
    let mut t = standing(&params, &world);
    let light = InputFrame {
        move_forward: 0.3,
        ..input()
    };
    for _ in 0..60 {
        tick(&mut t, &light, &params, &world);
    }
    assert!((hspeed(&t) - 440.0).abs() < 0.05);
}

#[test]
fn a_wk_1_acceleration_uses_the_yaw_before_this_ticks_look_update() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let turn = InputFrame {
        move_forward: 1.0,
        look_yaw_delta: core::f32::consts::FRAC_PI_2,
        ..input()
    };
    tick(&mut s, &turn, &params, &world);
    assert!((s.yaw - core::f32::consts::FRAC_PI_2).abs() < 1e-6);
    assert!(
        s.velocity.x > 1.0,
        "accelerated along the old forward: {}",
        s.velocity
    );
    assert!(s.velocity.y.abs() < 1e-3, "{}", s.velocity);
    tick(&mut s, &forward(), &params, &world);
    assert!(
        s.velocity.y > 1.0,
        "now along the new forward: {}",
        s.velocity
    );
}

#[test]
fn a_wk_4_sprint_pressed_in_the_air_arms_and_the_landing_applies_it() {
    let params = original();
    let world = flat_world();
    let mut s = airborne(&params, 200.0, Vec3::ZERO);
    let sprint = InputFrame {
        sprint_held: true,
        ..input()
    };
    tick(&mut s, &sprint, &params, &world);
    assert!(s.script.sprint.armed && !s.script.sprint.active);
    assert_eq!(s.script.ground_speed, 440.0);
    let e = until_landed(&mut s, &sprint, &params, &world, 300);
    let landing = e.landing.expect("landing reaction");
    assert_eq!(landing.handler, LandingHandler::Normal);
    assert!(landing.sprint_applied);
    assert!(s.script.sprint.active && !s.script.sprint.armed);
    assert_eq!(s.script.ground_speed, 880.0);
}

#[test]
fn a_wk_4_releasing_sprint_in_the_air_disarms() {
    let params = original();
    let world = flat_world();
    let mut s = airborne(&params, 200.0, Vec3::ZERO);
    let sprint = InputFrame {
        sprint_held: true,
        ..input()
    };
    tick(&mut s, &sprint, &params, &world);
    assert!(s.script.sprint.armed);
    tick(&mut s, &input(), &params, &world);
    assert!(!s.script.sprint.armed);
    let e = until_landed(&mut s, &input(), &params, &world, 300);
    assert!(!e.landing.expect("landing").sprint_applied);
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn a2_sprint_jump_resumes_sprint_on_landing_without_losing_speed() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    for _ in 0..90 {
        tick(&mut s, &sprint_forward(), &params, &world);
    }
    assert!((hspeed(&s) - 880.0).abs() < 0.05);
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..sprint_forward()
    };
    let e = tick(&mut s, &jump, &params, &world);
    assert!(e.jumped);
    // A-JP-2: the jump removes the sprint and arms sprint-after-landing.
    assert_eq!(s.script.ground_speed, 440.0);
    assert!(!s.script.sprint.active && s.script.sprint.armed);
    assert_eq!(s.script.code.state, PawnStateName::Jumped);
    let held = InputFrame {
        jump_held: true,
        ..sprint_forward()
    };
    let mut ticks = 0;
    loop {
        ticks += 1;
        assert!(ticks < 400);
        let e = tick(&mut s, &held, &params, &world);
        if s.grounded {
            let landing = e.landing.expect("landing");
            assert!(landing.sprint_applied);
            break;
        }
        // A-AC-1 BoundSpeed: no horizontal gain above the current speed.
        assert!(hspeed(&s) <= 880.0 + 0.05, "{}", hspeed(&s));
        assert!(hspeed(&s) >= 879.9, "{}", hspeed(&s));
    }
    // `Landed` runs inside the physics, so the rest of the landing tick
    // already walks with the 880 cap (no drop to 440).
    assert_eq!(s.script.ground_speed, 880.0);
    assert!(hspeed(&s) >= 879.0, "{}", hspeed(&s));
    tick(&mut s, &held, &params, &world);
    assert!((hspeed(&s) - 880.0).abs() < 0.05);
}

// ---------------------------------------------------------------------------
// Jump (A-JP-1…4).
// ---------------------------------------------------------------------------

/// Apex height above the start of a jump pressed in tick 1; `release` is the
/// tick in which the key is released (`None`: held throughout).
fn jump_apex(release: Option<usize>, tap_in_press_tick: bool) -> f32 {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let z0 = s.position.z;
    let mut apex = z0;
    for t in 1..400 {
        let held = !tap_in_press_tick && release.is_none_or(|r| t < r);
        let i = InputFrame {
            jump_pressed: t == 1,
            jump_held: held,
            ..input()
        };
        let e = tick(&mut s, &i, &params, &world);
        if t == 1 {
            assert!(e.jumped);
        }
        apex = apex.max(s.position.z);
        if t > 1 && s.grounded {
            return apex - z0;
        }
    }
    panic!("never landed");
}

#[test]
fn a3_full_jump_takes_off_at_jump_z_and_peaks_near_481() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..input()
    };
    tick(&mut s, &jump, &params, &world);
    // JumpZ, then one tick of the doubled effective gravity (−1040 uu/s²).
    assert!(
        (s.velocity.z - (1000.0 - 1040.0 * DT)).abs() < 0.01,
        "{}",
        s.velocity.z
    );
    assert!(!s.grounded);
    // Closed form (NATIVE_PHYSICS.md 4.6 / PARITY.md): n = 58 ticks to the
    // apex at JumpZ·n·h − 520·(n·h)² = 480.76.
    let apex = jump_apex(None, false);
    assert!((apex - 480.76).abs() < 0.5, "{apex}");
}

#[test]
fn a4_releasing_the_jump_damps_by_0_7_every_0_1_s() {
    // ABILITIES.md §3 model: release 0.1 / 0.2 / 0.4 / 0.7 s after the jump
    // → apex ≈ 198 / 266 / 371 / 461 uu (6 ticks per 0.1 s at 60 Hz).
    for (release_after, expected) in [(6, 198.4), (12, 265.8), (24, 371.4), (42, 460.8)] {
        let apex = jump_apex(Some(1 + release_after), false);
        assert!(
            (apex - expected).abs() < 0.6,
            "release +{release_after}: {apex}"
        );
    }
    // Press and release inside the same tick: the release event precedes the
    // jump attempt, so the pawn is not in `Jumped` yet and nothing is damped.
    let tap = jump_apex(None, true);
    assert!((tap - 480.76).abs() < 0.5, "{tap}");
    // Release one tick after the press: the shortest damped jump.
    let quick = jump_apex(Some(2), false);
    assert!((quick - 134.7).abs() < 0.6, "{quick}");
}

#[test]
fn a_jp_3_damping_steps_are_frame_quantised_and_end_in_falling_state() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let press = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..input()
    };
    tick(&mut s, &press, &params, &world);
    let held = InputFrame {
        jump_held: true,
        ..input()
    };
    for _ in 0..4 {
        tick(&mut s, &held, &params, &world);
    }
    // Release: ×0.7 in this tick's pawn code, before physics.
    let vz = s.velocity.z;
    tick(&mut s, &input(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    let expected = vz * JUMP_RELEASE_MULTIPLIER - 1040.0 * DT;
    assert!(
        (s.velocity.z - expected).abs() < 0.01,
        "{} vs {expected}",
        s.velocity.z
    );
    // The next five ticks: gravity only; the sixth: ×0.7 again.
    for _ in 0..5 {
        let before = s.velocity.z;
        tick(&mut s, &input(), &params, &world);
        assert!((s.velocity.z - (before - 1040.0 * DT)).abs() < 0.01);
    }
    let before = s.velocity.z;
    tick(&mut s, &input(), &params, &world);
    let expected = before * JUMP_RELEASE_MULTIPLIER - 1040.0 * DT;
    assert!(
        (s.velocity.z - expected).abs() < 0.01,
        "{} vs {expected}",
        s.velocity.z
    );
    // Once V.z ≤ 0.05 at a wake-up the loop ends in FallingState.
    let mut n = 0;
    while s.script.code.state == PawnStateName::ReleasedJump {
        n += 1;
        assert!(n < 120);
        tick(&mut s, &input(), &params, &world);
    }
    assert_eq!(s.script.code.state, PawnStateName::FallingState);
    assert!(!s.grounded && s.velocity.z < 0.05);
}

#[test]
fn a5_tapping_jump_while_rising_without_a_jump_damps() {
    let params = original();
    let world = flat_world();
    // Rising without having jumped (e.g. thrown up by a grapple release).
    let mut s = airborne(&params, 500.0, Vec3::new(0.0, 0.0, 800.0));
    let press = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..input()
    };
    let e = tick(&mut s, &press, &params, &world);
    assert!(!e.jumped, "no double jump (A-JP-4)");
    assert!((s.velocity.z - (800.0 - 1040.0 * DT)).abs() < 0.01);
    assert_eq!(
        s.script.code.state,
        PawnStateName::Jumped,
        "failed attempt still enters Jumped"
    );
    let vz = s.velocity.z;
    tick(&mut s, &input(), &params, &world);
    assert!((s.velocity.z - (vz * 0.7 - 1040.0 * DT)).abs() < 0.01);
}

#[test]
fn a_jp_2_failed_jump_still_removes_an_active_sprint() {
    let params = original();
    // Sprinting on a ledge, then walking off it (no jump): falling with the
    // sprint still active.
    let world = BoxWorld::new().with_box(
        Vec3::new(-500.0, -500.0, -100.0),
        Vec3::new(30.0, 500.0, 0.0),
        false,
    );
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, &params);
    for _ in 0..60 {
        tick(&mut s, &sprint_forward(), &params, &world);
        if !s.grounded {
            break;
        }
    }
    assert!(!s.grounded && s.script.sprint.active);
    let press = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..sprint_forward()
    };
    let vz = s.velocity.z;
    let e = tick(&mut s, &press, &params, &world);
    assert!(!e.jumped);
    assert!(s.velocity.z < vz, "no impulse");
    assert!(!s.script.sprint.active && s.script.sprint.armed);
    assert_eq!(s.script.ground_speed, 440.0);
}

// ---------------------------------------------------------------------------
// Air control (A-AC-1/2) and landing (§5).
// ---------------------------------------------------------------------------

#[test]
fn a6_air_control_is_0_3_until_the_first_normal_landing_then_0_35() {
    let params = original();
    let world = flat_world();
    // Air steering adds 2·AccelRate·AirControl·dt per tick (the falling
    // refinement doubles it; NATIVE_PHYSICS.md 4.6).
    let mut s = airborne(&params, 5000.0, Vec3::new(100.0, 0.0, 0.0));
    tick(&mut s, &forward(), &params, &world);
    let gain = s.velocity.x - 100.0;
    assert!((gain - 2.0 * 2048.0 * 0.3 * DT).abs() < 0.01, "{gain}");
    // Land once, then fling the pawn up again.
    let mut s = airborne(&params, 100.0, Vec3::ZERO);
    until_landed(&mut s, &input(), &params, &world, 300);
    assert_eq!(s.script.air_control, 0.35);
    s.position.z = 5000.0;
    s.grounded = false;
    s.velocity = Vec3::new(100.0, 0.0, 0.0);
    tick(&mut s, &forward(), &params, &world);
    let gain = s.velocity.x - 100.0;
    assert!((gain - 2.0 * 2048.0 * 0.35 * DT).abs() < 0.01, "{gain}");
}

fn landing(vz: f32, not_landable: bool) -> Landing {
    Landing {
        hit_normal: Vec3::Z,
        velocity: Vec3::new(0.0, 0.0, vz),
        location: Vec3::new(1.0, 2.0, 77.0),
        not_landable,
    }
}

fn landed_script(params: &PlayerParams) -> PawnScript {
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 500.0), 0.0);
    pawn::start(&mut s, params);
    s.script
}

#[test]
fn landing_table_normal_handler() {
    let params = original();
    let p = pawn_params(&params);
    let mut sc = landed_script(&params);
    sc.move_input_lock = 2;
    sc.sprint.armed = true;
    sc.code.state = PawnStateName::ReleasedJump;
    sc.old_z = 500.0;
    let o = pawn::on_landed(&mut sc, &params, p, &landing(-2100.0, false));
    assert_eq!(o.handler, LandingHandler::Normal);
    assert!(o.hard && o.sound && o.sprint_applied);
    assert_eq!(o.cue, Some(LandCue::Heavy));
    assert_eq!(sc.move_input_lock, 1, "one lock released");
    assert!(sc.sprint.active && !sc.sprint.armed);
    assert_eq!(sc.ground_speed, 880.0);
    assert_eq!(
        sc.code.state,
        PawnStateName::HasLanded,
        "ends the jump damping"
    );
    assert_eq!(sc.air_control, 0.35);
    assert_eq!(sc.old_z, 77.0, "eye baseline reset below -200");

    // Thresholds: hard < -2000; sound ≤ -500; cues < -2500 and < -1250.
    let cases = [
        (-2600.0, true, true, Some(LandCue::FallingDamage)),
        (-2000.0, false, true, Some(LandCue::Heavy)),
        (-1250.0, false, true, None),
        (-500.0, false, true, None),
        (-499.0, false, false, None),
    ];
    for (vz, hard, sound, cue) in cases {
        let mut sc = landed_script(&params);
        let o = pawn::on_landed(&mut sc, &params, p, &landing(vz, false));
        assert_eq!((o.hard, o.sound, o.cue), (hard, sound, cue), "vz {vz}");
    }
    // Eye baseline only below -200; the lock never underflows.
    let mut sc = landed_script(&params);
    sc.old_z = 500.0;
    pawn::on_landed(&mut sc, &params, p, &landing(-200.0, false));
    assert_eq!(sc.old_z, 500.0);
    assert_eq!(sc.move_input_lock, 0);
}

#[test]
fn landing_table_not_landable_and_story_handlers() {
    let params = original();
    let p = pawn_params(&params);
    // NotLandable: the normal handler returns at once.
    let mut sc = landed_script(&params);
    sc.move_input_lock = 1;
    sc.sprint.armed = true;
    sc.code.state = PawnStateName::Jumped;
    let before = sc;
    let o = pawn::on_landed(&mut sc, &params, p, &landing(-3000.0, true));
    assert_eq!(o.handler, LandingHandler::NotLandable);
    assert!(!o.hard && !o.sound && !o.sprint_applied);
    assert_eq!(sc, before, "nothing changes on a NotLandable floor");

    // Story mode: no NotLandable check, lock released, the armed sprint is
    // applied but stays armed, no AirControl/state/eye-baseline change, no
    // hard landing.
    let mut st = PlayerState::new(Vec3::new(0.0, 0.0, 500.0), 0.0);
    pawn::start(&mut st, &params);
    pawn::enter_story_mode(&mut st, &params);
    let mut sc = st.script;
    sc.move_input_lock = 1;
    sc.sprint.armed = true;
    sc.old_z = 500.0;
    let o = pawn::on_landed(&mut sc, &params, p, &landing(-2600.0, true));
    assert_eq!(o.handler, LandingHandler::Story);
    assert!(!o.hard && o.sound && o.sprint_applied);
    assert_eq!(o.cue, Some(LandCue::FallingDamage));
    assert_eq!(sc.move_input_lock, 0);
    assert!(sc.sprint.active && sc.sprint.armed);
    assert_eq!(sc.air_control, 0.3);
    assert_eq!(sc.code.state, PawnStateName::StoryState);
    assert_eq!(sc.old_z, 500.0);
}

#[test]
fn a7_no_falling_damage_terminal_velocity_10000_and_no_landing_slowdown() {
    let params = original();
    let world = flat_world();
    let mut s = airborne(&params, 60_000.0, Vec3::new(440.0, 0.0, 0.0));
    let mut max_speed = 0.0_f32;
    let e = loop {
        let e = tick(&mut s, &forward(), &params, &world);
        max_speed = max_speed.max(s.velocity.length());
        if s.grounded {
            break e;
        }
        assert!(s.position.z > -10.0);
    };
    // `fTerminalVelocity` (10 000) replaces the volume's 4000 at pawn start.
    assert!(max_speed <= 10_000.0 + 0.5, "{max_speed}");
    assert!(max_speed > 9_990.0, "{max_speed}");
    let landing = e.landing.expect("landing");
    assert!(landing.hard && landing.sound);
    assert_eq!(landing.cue, Some(LandCue::FallingDamage));
    // Alive and walking, horizontal speed kept (no ×0.1 slowdown).
    assert!(s.grounded);
    assert!(hspeed(&s) > 430.0, "{}", hspeed(&s));
}

// ---------------------------------------------------------------------------
// Eye height, bob, camera (A-CM-1…4).
// ---------------------------------------------------------------------------

#[test]
fn a21_eye_height_absorbs_a_step_and_relaxes_back() {
    let params = original();
    let world = flat_world().with_box(
        Vec3::new(100.0, -500.0, 0.0),
        Vec3::new(3000.0, 500.0, 20.0),
        false,
    );
    let mut s = standing(&params, &world);
    let k = 10.0 * DT; // min(0.9, 10·dt / CustomTimeDilation)
    let mut stepped = None;
    for t in 0..240 {
        let z0 = s.position.z;
        let eye0 = s.script.eye_height;
        tick(&mut s, &forward(), &params, &world);
        let dz = s.position.z - z0;
        if s.grounded {
            // Walking rule: (EyeHeight − ΔZ)(1 − k) + 38k.
            let expected = ((eye0 - s.position.z + z0) * (1.0 - k) + 38.0 * k).max(-22.0);
            assert!((s.script.eye_height - expected).abs() < 1e-3, "tick {t}");
        }
        if stepped.is_none() && dz > 15.0 {
            stepped = Some(t);
            assert!(
                s.script.eye_height < 38.0 - 10.0,
                "dipped: {}",
                s.script.eye_height
            );
        }
    }
    let t = stepped.expect("stepped up");
    assert!(t < 30);
    assert!(
        (s.script.eye_height - 38.0).abs() < 0.01,
        "{}",
        s.script.eye_height
    );
}

#[test]
fn eye_height_is_clamped_at_minus_half_the_collision_height() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    s.script.eye_height = -100.0;
    tick(&mut s, &input(), &params, &world);
    assert_eq!(s.script.eye_height, -22.0);
    // Off the ground there is no clamp, only relaxation towards 38.
    let mut a = airborne(&params, 5000.0, Vec3::ZERO);
    a.script.eye_height = -100.0;
    tick(&mut a, &input(), &params, &world);
    let k = 10.0 * DT;
    assert!((a.script.eye_height - (-100.0 * (1.0 - k) + 38.0 * k)).abs() < 1e-3);
}

#[test]
fn fast_landing_resets_the_eye_baseline() {
    let params = original();
    let world = flat_world();
    let mut s = airborne(&params, 400.0, Vec3::ZERO);
    let e = until_landed(&mut s, &input(), &params, &world, 300);
    assert!(e.landed.expect("impact") < -200.0);
    // The fall of the landing tick is not absorbed into the eye height.
    assert!(s.script.eye_height > 35.0, "{}", s.script.eye_height);
}

#[test]
fn ceiling_probe_keeps_the_view_below_a_low_ceiling() {
    let params = original();
    // Floor at 0; the walking pawn hovers 2.15 uu, so its centre is at
    // 46.15. A ceiling 44.5 above the centre: the 12-uu probe stops at
    // 44.5 − 12 = 32.5.
    let ceiling = 46.15 + 44.5;
    let world = flat_world().with_box(
        Vec3::new(-500.0, -500.0, ceiling),
        Vec3::new(500.0, 500.0, ceiling + 50.0),
        false,
    );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 44.1), 0.0);
    assert!(asamu_player::movement::place_on_floor(
        &mut s,
        &params.movement,
        &world,
        1.0
    ));
    pawn::start(&mut s, &params);
    for _ in 0..10 {
        tick(&mut s, &input(), &params, &world);
    }
    assert!(s.grounded && (s.position.z - 46.15).abs() < 1e-3);
    let room = ceiling - s.position.z - 12.0;
    assert!(
        (s.script.eye_height - room).abs() < 1e-3,
        "{} vs {room}",
        s.script.eye_height
    );
}

#[test]
fn walk_bob_amplitudes_rates_and_view_location() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    // Standing: idle phase rate 0.2, no offset.
    let b0 = s.script.bob_time;
    tick(&mut s, &input(), &params, &world);
    assert!((s.script.bob_time - b0 - 0.2 * DT).abs() < 1e-6);
    assert_eq!(s.script.walk_bob, Vec3::ZERO);
    let (mut lateral, mut vertical) = (0.0_f32, 0.0_f32);
    for _ in 0..300 {
        let b = s.script.bob_time;
        tick(&mut s, &forward(), &params, &world);
        if hspeed(&s) > 439.0 {
            // Walking normally: phase rate 0.65.
            assert!((s.script.bob_time - b - 0.65 * DT).abs() < 1e-5);
        }
        lateral = lateral.max(s.script.walk_bob.y.abs());
        vertical = vertical.max(s.script.walk_bob.z.abs());
        assert!(s.script.walk_bob.x.abs() < 1e-3, "yaw 0: lateral is ±Y");
    }
    // Bob 0.01: ±0.01·440 = 4.4 lateral, ±0.0075·440 = 3.3 vertical.
    assert!(lateral <= 4.4 + 1e-3 && lateral > 4.2, "{lateral}");
    assert!(vertical <= 3.3 + 1e-3 && vertical > 3.1, "{vertical}");
    // Sprinting: 0.85.
    tick(&mut s, &sprint_forward(), &params, &world);
    let b = s.script.bob_time;
    tick(&mut s, &sprint_forward(), &params, &world);
    assert!((s.script.bob_time - b - 0.85 * DT).abs() < 1e-5);
    // The view point and the grapple trace origin include eye height + bob.
    let view = s.position + Vec3::new(0.0, 0.0, s.script.eye_height) + s.script.walk_bob;
    assert_eq!(s.view_location(&params), view);
    assert_eq!(grapple::eye_position(&s, &params), view);
    // Off the ground: phase reset, offset decays by (1 − 8·dt).
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..input()
    };
    let bob = s.script.walk_bob;
    tick(&mut s, &jump, &params, &world);
    assert_eq!(s.script.bob_time, 0.0);
    let expected = bob * (1.0 - 8.0 * DT);
    assert!((s.script.walk_bob - expected).length() < 1e-5);
}

#[test]
fn weapon_bob_off_scales_the_walk_bob_by_0_1() {
    let mut params = original();
    if let Some(p) = params.pawn.as_mut() {
        p.weapon_bob.value = false;
    }
    let world = flat_world();
    let mut s = standing(&params, &world);
    let mut lateral = 0.0_f32;
    for _ in 0..300 {
        tick(&mut s, &forward(), &params, &world);
        lateral = lateral.max(s.script.walk_bob.y.abs());
    }
    assert!(lateral <= 0.44 + 1e-4 && lateral > 0.42, "{lateral}");
}

#[test]
fn a_cm_2_view_pitch_is_limited_to_18000_rotator_units() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let up = InputFrame {
        look_pitch_delta: 3.0,
        ..input()
    };
    tick(&mut s, &up, &params, &world);
    let limit = (18_000.0_f32 * 360.0 / 65_536.0).to_radians();
    assert_eq!(s.pitch, limit);
    let down = InputFrame {
        look_pitch_delta: -6.0,
        ..input()
    };
    tick(&mut s, &down, &params, &world);
    assert_eq!(s.pitch, -limit);
}

// ---------------------------------------------------------------------------
// Story mode and zoom (A-ST-1…4).
// ---------------------------------------------------------------------------

#[test]
fn a_st_story_mode_speed_no_jump_no_sprint_and_exit() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    tick(&mut s, &sprint_forward(), &params, &world);
    assert!(s.script.sprint.active);
    s.script.gun.attached = Some(asamu_player::grapple_gun::Attachment::default());
    s.script.gun.grapple_location = Vec3::new(0.0, 0.0, 500.0);
    s.pawn.flying = true;
    s.grounded = false;
    s.script.power_jump.state = PowerJumpStateName::Charging;
    let released = pawn::enter_story_mode(&mut s, &params);
    assert_eq!(s.script.code.state, PawnStateName::StoryState);
    assert!(
        !s.script.sprint.active && !s.script.sprint.armed,
        "sprint stopped"
    );
    assert!(!s.is_grapple_attached(), "grapple released");
    assert!(released.gun.released.is_some());
    assert_eq!(s.script.power_jump.state, PowerJumpStateName::Canceled);
    assert_eq!(s.script.ground_speed, 264.0, "MoveSpeed × 0.6");
    // Sprint keys are ignored; walking reaches 264.
    for _ in 0..60 {
        let i = InputFrame {
            sprint_held: true,
            ..forward()
        };
        tick(&mut s, &i, &params, &world);
        assert_eq!(s.script.ground_speed, 264.0);
    }
    assert!((hspeed(&s) - 264.0).abs() < 0.05, "{}", hspeed(&s));
    // Jump attempts do nothing at all.
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..input()
    };
    let e = tick(&mut s, &jump, &params, &world);
    assert!(!e.jumped && s.grounded);
    assert_eq!(s.script.code.state, PawnStateName::StoryState);
    // `use` only works in story mode.
    let use_key = InputFrame {
        use_pressed: true,
        ..input()
    };
    assert!(tick(&mut s, &use_key, &params, &world).use_requested);
    pawn::exit_story_mode(&mut s, &params);
    assert_eq!(s.script.code.state, PawnStateName::Idle);
    assert_eq!(s.script.ground_speed, 440.0);
    assert!(!tick(&mut s, &use_key, &params, &world).use_requested);
    // Exiting outside story mode changes nothing.
    let before = s.script;
    pawn::exit_story_mode(&mut s, &params);
    assert_eq!(s.script, before);
}

fn zoom_fov(i: i32) -> f32 {
    90.0 - (90.0 - 50.0) * (ZOOM_STEP / 0.3) * i as f32
}

#[test]
fn a20_zoom_in_19_steps_hold_and_out_18_steps() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    pawn::enter_story_mode(&mut s, &params);
    let held = InputFrame {
        power_jump_held: true,
        ..input()
    };
    let mut fovs = Vec::new();
    for _ in 0..25 {
        tick(&mut s, &held, &params, &world);
        fovs.push(s.fov(&params));
    }
    assert_eq!(s.script.code.state, PawnStateName::Zooming);
    for (i, f) in fovs.iter().take(19).enumerate() {
        assert_eq!(*f, zoom_fov(i as i32), "zoom-in step {i}");
    }
    assert!((fovs[18] - 51.6).abs() < 1e-3);
    assert!(
        fovs[19..].iter().all(|&f| f == 50.0),
        "held at zoomFOV: {fovs:?}"
    );
    assert_ne!(
        s.script.power_jump.state,
        PowerJumpStateName::Charging,
        "no power jump"
    );
    // Release: 18 steps back from 50, then 90 and back to StoryState.
    let mut out = Vec::new();
    for _ in 0..20 {
        tick(&mut s, &input(), &params, &world);
        out.push(s.fov(&params));
    }
    for (j, f) in out.iter().take(18).enumerate() {
        let i = 18 - j as i32;
        let expected = 90.0 - (90.0 - 50.0) * (ZOOM_STEP / 0.3) * i as f32;
        assert_eq!(*f, expected, "zoom-out step {j}");
    }
    assert_eq!(out[18], 90.0);
    assert_eq!(s.script.code.state, PawnStateName::StoryState);
}

#[test]
fn zoom_release_during_zoom_in_and_disable_and_story_exit() {
    let params = original();
    let world = flat_world();
    let held = InputFrame {
        power_jump_held: true,
        ..input()
    };
    // Release during the zoom-in: the next step is still taken, then the
    // zoom-out starts from the FOV reached.
    let mut s = standing(&params, &world);
    pawn::enter_story_mode(&mut s, &params);
    for _ in 0..6 {
        tick(&mut s, &held, &params, &world);
    }
    assert_eq!(s.fov(&params), zoom_fov(5));
    tick(&mut s, &input(), &params, &world);
    let reached = zoom_fov(6);
    let expected = 90.0 - (90.0 - reached) * (ZOOM_STEP / 0.3) * 18.0;
    assert_eq!(s.fov(&params), expected);
    let mut last = s.fov(&params);
    for _ in 0..30 {
        tick(&mut s, &input(), &params, &world);
        assert!(s.fov(&params) >= last);
        last = s.fov(&params);
    }
    assert_eq!(last, 90.0);
    // Disabling the zoom while zoomed zooms out; zoom is unavailable after.
    let mut s = standing(&params, &world);
    pawn::enter_story_mode(&mut s, &params);
    for _ in 0..25 {
        tick(&mut s, &held, &params, &world);
    }
    pawn::set_zoom_available(&mut s, false);
    for _ in 0..25 {
        tick(&mut s, &held, &params, &world);
    }
    assert_eq!(s.fov(&params), 90.0);
    assert_eq!(s.script.code.state, PawnStateName::StoryState);
    tick(&mut s, &input(), &params, &world);
    tick(&mut s, &held, &params, &world);
    assert_eq!(
        s.script.code.state,
        PawnStateName::StoryState,
        "zoom disabled"
    );
    // Leaving story mode while zoomed restores the settings FOV.
    let mut s = standing(&params, &world);
    pawn::enter_story_mode(&mut s, &params);
    for _ in 0..25 {
        tick(&mut s, &held, &params, &world);
    }
    assert_eq!(s.fov(&params), 50.0);
    pawn::exit_story_mode(&mut s, &params);
    assert_eq!(s.fov(&params), 90.0);
    assert_eq!(s.script.code.state, PawnStateName::Idle);
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn power_jump_key_outside_story_mode_never_zooms() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let held = InputFrame {
        power_jump_held: true,
        ..input()
    };
    for _ in 0..30 {
        tick(&mut s, &held, &params, &world);
        assert_eq!(s.fov(&params), 90.0);
    }
    assert_eq!(s.script.power_jump.state, PowerJumpStateName::Charging);
}

// ---------------------------------------------------------------------------
// Power jump and leap (A-PJ-1…6), move-input lock (A-IL-1).
// ---------------------------------------------------------------------------

/// Holds the power-jump key from tick 1, releases it in tick `release`
/// (with `extra` held as well); returns the events of every tick.
fn power_jump_run(
    s: &mut PlayerState,
    params: &PlayerParams,
    world: &BoxWorld,
    release: usize,
    extra: InputFrame,
) -> Vec<StepEvents> {
    let mut events = Vec::new();
    for t in 1..=release {
        let i = InputFrame {
            power_jump_held: t < release,
            ..extra
        };
        events.push(tick(s, &i, params, world));
    }
    events
}

#[test]
fn a9_power_jump_charges_in_0_6_s_and_cancels_if_released_early() {
    let params = original();
    let world = flat_world();
    // Charged in the 36th tick after the press tick (Sleep(0.6) at 60 Hz).
    let mut s = standing(&params, &world);
    let events = power_jump_run(&mut s, &params, &world, 40, input());
    let charged: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e.power_jump == Some(PowerJumpEvent::Charged))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(charged, vec![37]);
    // Released in the tick the charge completes: the key event comes first,
    // so the release cancels.
    let mut s = standing(&params, &world);
    let events = power_jump_run(&mut s, &params, &world, 37, input());
    assert_eq!(events[36].power_jump, Some(PowerJumpEvent::Canceled));
    assert!(s.grounded && s.velocity.z == 0.0);
    // 0.5 s: cancel.
    let mut s = standing(&params, &world);
    let events = power_jump_run(&mut s, &params, &world, 31, input());
    assert_eq!(events[30].power_jump, Some(PowerJumpEvent::Canceled));
    tick(&mut s, &input(), &params, &world);
    assert_eq!(s.script.power_jump.state, PowerJumpStateName::Ready);
}

#[test]
fn a9_power_jump_sets_vz_1600_and_peaks_near_1231() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let events = power_jump_run(&mut s, &params, &world, 43, input());
    assert_eq!(
        events[42].power_jump,
        Some(PowerJumpEvent::Fired {
            leap: false,
            jumped: true
        })
    );
    assert!(events[42].jumped);
    // Written after the tick's physics: integrated from the next tick.
    assert_eq!(s.velocity.z, 1600.0);
    assert!(!s.grounded);
    assert_eq!(s.script.jump_z, 1000.0, "JumpZ restored");
    assert_eq!(
        s.script.code.state,
        PawnStateName::FallingState,
        "not damped"
    );
    let z0 = s.position.z;
    let mut apex = z0;
    while !s.grounded {
        tick(&mut s, &input(), &params, &world);
        apex = apex.max(s.position.z);
    }
    // 1600²/(4·520) ≈ 1230.8 (ABILITIES.md A-PJ-6).
    assert!((apex - z0 - 1230.76).abs() < 1.0, "{}", apex - z0);
}

#[test]
fn a11_power_jump_released_in_the_air_cancels() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let held = InputFrame {
        power_jump_held: true,
        ..input()
    };
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        power_jump_held: true,
        ..input()
    };
    tick(&mut s, &held, &params, &world);
    tick(&mut s, &jump, &params, &world);
    for _ in 0..40 {
        tick(&mut s, &held, &params, &world);
    }
    assert!(
        !s.grounded && s.script.power_jump.charged,
        "charging continues in the air"
    );
    let e = tick(&mut s, &input(), &params, &world);
    assert_eq!(e.power_jump, Some(PowerJumpEvent::Canceled));
}

#[test]
fn a10_power_leap_doubles_horizontal_speed_and_locks_move_input_until_landing() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    for _ in 0..90 {
        tick(&mut s, &sprint_forward(), &params, &world);
    }
    let h0 = hspeed(&s);
    assert!((h0 - 880.0).abs() < 0.05);
    let events = power_jump_run(&mut s, &params, &world, 43, sprint_forward());
    assert_eq!(
        events[42].power_jump,
        Some(PowerJumpEvent::Fired {
            leap: true,
            jumped: true
        })
    );
    assert_eq!(s.velocity.z, 750.0);
    let h1 = hspeed(&s);
    assert!((h1 - 2.0 * 880.0).abs() < 1.0, "{h1}");
    assert_eq!(s.script.move_input_lock, 1);
    assert!(s.script.sprint.armed && !s.script.sprint.active);
    // No air control while locked: the horizontal velocity is kept.
    let steer = InputFrame {
        move_right: 1.0,
        ..sprint_forward()
    };
    let mut landed = None;
    for _ in 0..200 {
        let e = tick(&mut s, &steer, &params, &world);
        if s.grounded {
            landed = Some(e);
            break;
        }
        // Only f32 rounding of the displacement-derived velocity changes it.
        assert!((hspeed(&s) - h1).abs() < 0.2, "{} vs {h1}", hspeed(&s));
        assert!(s.velocity.y.abs() < 1e-3);
    }
    let e = landed.expect("landed");
    assert!(e.landing.expect("landing").sprint_applied, "sprint resumes");
    assert_eq!(s.script.move_input_lock, 0);
    assert_eq!(s.script.ground_speed, 880.0);
}

#[test]
fn power_jump_while_sprinting_in_place_is_vertical() {
    // The leap needs a non-zero velocity component (Z included).
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let sprint = InputFrame {
        sprint_held: true,
        ..input()
    };
    tick(&mut s, &sprint, &params, &world);
    assert!(s.script.sprint.active && s.velocity == Vec3::ZERO);
    let events = power_jump_run(&mut s, &params, &world, 43, sprint);
    assert_eq!(
        events[42].power_jump,
        Some(PowerJumpEvent::Fired {
            leap: false,
            jumped: true
        })
    );
    assert_eq!(s.velocity.z, 1600.0);
    assert!(
        s.script.sprint.armed,
        "the inner jump attempt removed the sprint"
    );
}

#[test]
fn a_il_1_lock_counter_zeroes_movement_and_grapple_attach_releases_it() {
    let params = original();
    let world = flat_world().with_box(
        Vec3::new(400.0, -100.0, 0.0),
        Vec3::new(500.0, 100.0, 400.0),
        true,
    );
    let mut s = standing(&params, &world);
    asamu_player::grapple_gun::set_max_grapples(&mut s, 3);
    s.script.move_input_lock = 1;
    for _ in 0..30 {
        tick(&mut s, &forward(), &params, &world);
    }
    assert_eq!(hspeed(&s), 0.0, "move axes zeroed while locked");
    // A grapple attach releases one lock (G-AT-5).
    let fire = InputFrame {
        grapple_held: true,
        ..input()
    };
    let e = tick(&mut s, &fire, &params, &world);
    assert!(e.gun.attached.is_some(), "{e:?}");
    assert_eq!(s.script.move_input_lock, 0);
    // While attached the jump flag is discarded (G-PH-1).
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        grapple_held: true,
        ..input()
    };
    let state_before = s.script.code.state;
    let e = tick(&mut s, &jump, &params, &world);
    assert!(!e.jumped);
    assert_eq!(s.script.code.state, state_before);
}

// ---------------------------------------------------------------------------
// Pipeline properties.
// ---------------------------------------------------------------------------

fn random_inputs(seed: u64, n: usize) -> Vec<InputFrame> {
    let mut rng = SplitMix64(seed);
    let (mut jump, mut sprint, mut power, mut grapple) = (false, false, false, false);
    (0..n)
        .map(|_| {
            let was_jump = jump;
            if rng.chance(0.05) {
                jump = !jump;
            }
            if rng.chance(0.02) {
                sprint = !sprint;
            }
            if rng.chance(0.015) {
                power = !power;
            }
            if rng.chance(0.01) {
                grapple = !grapple;
            }
            InputFrame {
                move_forward: rng.range(-1.0, 1.0),
                move_right: if rng.chance(0.3) {
                    rng.range(-1.0, 1.0)
                } else {
                    0.0
                },
                look_yaw_delta: rng.range(-0.05, 0.05),
                look_pitch_delta: rng.range(-0.03, 0.03),
                jump_pressed: jump && !was_jump,
                jump_held: jump,
                grapple_held: grapple,
                sprint_held: sprint,
                power_jump_held: power,
                use_pressed: rng.chance(0.01),
            }
        })
        .collect()
}

#[test]
fn scripted_runs_are_bit_reproducible_and_resumable() {
    let params = original();
    let world = flat_world()
        .with_box(
            Vec3::new(300.0, -300.0, 0.0),
            Vec3::new(600.0, 300.0, 24.0),
            false,
        )
        .with_box(
            Vec3::new(-900.0, 400.0, 0.0),
            Vec3::new(-700.0, 600.0, 900.0),
            true,
        );
    let inputs = random_inputs(0xA5A5_0001, 1500);
    let run = |from: PlayerState, inputs: &[InputFrame]| {
        let mut s = from;
        for (i, input) in inputs.iter().enumerate() {
            if i == 700 {
                pawn::enter_story_mode(&mut s, &params);
            }
            if i == 900 {
                pawn::exit_story_mode(&mut s, &params);
            }
            tick(&mut s, input, &params, &world);
        }
        s
    };
    let start = standing(&params, &world);
    let a = run(start, &inputs);
    let b = run(start, &inputs);
    assert_eq!(
        serde_json::to_string(&a).expect("ser"),
        serde_json::to_string(&b).expect("ser")
    );
    // Snapshot after 600 ticks, resume from the deserialized state.
    let mid = run(start, &inputs[..600]);
    let json = serde_json::to_string(&mid).expect("ser");
    let resumed: PlayerState = serde_json::from_str(&json).expect("de");
    assert_eq!(resumed, mid);
    let mut c = resumed;
    for (i, input) in inputs[600..].iter().enumerate() {
        if i + 600 == 700 {
            pawn::enter_story_mode(&mut c, &params);
        }
        if i + 600 == 900 {
            pawn::exit_story_mode(&mut c, &params);
        }
        tick(&mut c, input, &params, &world);
    }
    assert_eq!(c, a);
}

#[test]
fn placeholder_model_with_original_params_runs_the_script_layer() {
    let params = original();
    let world = flat_world();
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    for _ in 0..90 {
        step_with(
            &PlaceholderMovement,
            &mut s,
            &sprint_forward(),
            &params,
            &world,
            DT,
        );
    }
    assert!(s.script.started && s.script.sprint.active);
    assert!((hspeed(&s) - 880.0).abs() < 1.0, "{}", hspeed(&s));
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..sprint_forward()
    };
    let e = step_with(&PlaceholderMovement, &mut s, &jump, &params, &world, DT);
    assert!(e.jumped);
    assert!(s.script.sprint.armed);
    let mut landed = false;
    for _ in 0..400 {
        let e = step_with(
            &PlaceholderMovement,
            &mut s,
            &sprint_forward(),
            &params,
            &world,
            DT,
        );
        if let Some(l) = e.landing {
            assert!(l.sprint_applied);
            landed = true;
            break;
        }
    }
    assert!(landed);
    assert_eq!(s.script.ground_speed, 880.0);
}

#[test]
fn hostile_dt_and_inputs_stay_finite() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    let mut rng = SplitMix64(0xBAD5_EED5);
    for i in 0..2000 {
        let mut input = random_inputs(i, 1)[0];
        if rng.chance(0.02) {
            input.move_forward = f32::NAN;
            input.look_yaw_delta = f32::INFINITY;
        }
        let dt = match i % 7 {
            0 => rng.range(0.0001, 0.3),
            1 => f32::NAN,
            2 => -1.0,
            _ => DT,
        };
        step_with(&Ue3PawnMovement, &mut s, &input, &params, &world, dt);
        assert!(s.is_finite(), "{s:?}");
    }
}
