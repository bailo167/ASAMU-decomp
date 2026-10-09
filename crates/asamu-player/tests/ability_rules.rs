//! Verification-pass tests of the ASAMU script layer: rules of
//! `docs/reverse-engineering/ABILITIES.md` (and the controller rules of
//! `GRAPPLE.md` around the grapple gun) that `pawn_script.rs` did not cover.
//! Each test names its rule id. Expected behaviour comes from the spec text
//! only. The grapple gun's own rules are in `grapple_gun.rs`.

mod common;

use asamu_player::grapple_gun;
use asamu_player::movement::place_on_floor;
use asamu_player::pawn::{self, JUMP_RELEASE_MULTIPLIER, PawnStateName, PowerJumpEvent};
use asamu_player::{BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement};
use asamu_player::{LandingHandler, step_with};
use common::{DT, flat_world};
use glam::Vec3;

fn original() -> PlayerParams {
    PlayerParams::asamu_original()
}

fn tick_dt(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
    dt: f32,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, dt);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
) -> StepEvents {
    tick_dt(s, input, params, world, DT)
}

/// A flat floor plus a wide grapple-able ceiling slab 1200–1300 uu up.
fn ceiling_world() -> BoxWorld {
    flat_world().with_box(
        Vec3::new(-3000.0, -3000.0, 1200.0),
        Vec3::new(3000.0, 3000.0, 1300.0),
        true,
    )
}

/// A flat floor plus a wide grapple-able wall 4500 uu ahead (+X), far
/// enough that an attached pawn is pulled for well over a second before the
/// proximity release.
fn far_wall_world() -> BoxWorld {
    flat_world().with_box(
        Vec3::new(4500.0, -3000.0, 0.0),
        Vec3::new(4600.0, 3000.0, 3000.0),
        true,
    )
}

/// A started pawn standing on the ground (z = 0) at the origin, looking
/// `pitch` radians up, with a grapple capacity of 3 (the gun starts with
/// 0 until Kismet sets it, G-CT-2).
fn standing_looking_up(params: &PlayerParams, world: &BoxWorld, pitch: f32) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
    assert!(place_on_floor(&mut s, &params.movement, world, 200.0));
    assert!(s.position.z < 100.0);
    pawn::start(&mut s, params);
    grapple_gun::set_max_grapples(&mut s, 3);
    let look = InputFrame {
        look_pitch_delta: pitch,
        ..InputFrame::default()
    };
    tick(&mut s, &look, params, world);
    assert!(s.grounded);
    s
}

fn jump_press(extra: InputFrame) -> InputFrame {
    InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..extra
    }
}

fn held(jump: bool, fire: bool) -> InputFrame {
    InputFrame {
        jump_held: jump,
        grapple_held: fire,
        ..InputFrame::default()
    }
}

fn input() -> InputFrame {
    InputFrame::default()
}

fn kinematics(s: &PlayerState) -> (Vec3, Vec3, bool) {
    (s.position, s.velocity, s.grounded)
}

// ---------------------------------------------------------------------------
// A-JP-3: the pawn leaves `Jumped`/`ReleasedJump` on grapple attach
// (`Shooting`) and release (`Release`).
// ---------------------------------------------------------------------------

#[test]
fn a_jp_3_grapple_attach_enters_shooting_and_ends_the_damping_loop() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    tick(&mut s, &jump_press(input()), &params, &world);
    // Release Space: `ReleasedJump`, first ×0.7 now, next wake-up 6 ticks
    // later (Sleep(0.1) at 60 Hz).
    tick(&mut s, &held(false, false), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    // Attach in the next tick.
    let e = tick(&mut s, &held(false, true), &params, &world);
    assert!(e.gun.attached.is_some(), "{e:?}");
    assert_eq!(s.script.code.state, PawnStateName::Shooting);
    assert!(s.velocity.z > 100.0, "still rising fast at the attach");
    // No further ×0.7 step: the motion equals that of a pawn whose script
    // state never had a damping loop (forced to `Idle`), across the tick
    // where the loop would have woken.
    let mut undamped = s;
    undamped.script.code = pawn::PawnCode::default();
    for _ in 0..10 {
        tick(&mut s, &held(false, true), &params, &world);
        tick(&mut undamped, &held(false, true), &params, &world);
        assert_eq!(s.script.code.state, PawnStateName::Shooting);
        assert_eq!(kinematics(&s), kinematics(&undamped));
    }
}

#[test]
fn g_ix_1_releasing_space_while_attached_does_nothing() {
    let params = original();
    let world = far_wall_world();
    let start = standing_looking_up(&params, &world, 0.0);
    // Jump with Space held, attach while rising, then (a) release Space
    // while attached or (b) keep holding it: identical motion.
    let run = |release_space_at: Option<usize>| {
        let mut s = start;
        tick(&mut s, &jump_press(input()), &params, &world);
        tick(&mut s, &held(true, false), &params, &world);
        tick(&mut s, &held(true, true), &params, &world);
        assert!(s.is_grapple_attached());
        assert_eq!(s.script.code.state, PawnStateName::Shooting);
        let mut out = Vec::new();
        for t in 0..30 {
            let space = release_space_at.is_none_or(|r| t < r);
            tick(&mut s, &held(space, true), &params, &world);
            out.push(kinematics(&s));
        }
        (out, s.script.code.state)
    };
    let (released, state) = run(Some(2));
    let (kept, _) = run(None);
    assert_eq!(state, PawnStateName::Shooting);
    assert_eq!(released, kept);
}

#[test]
fn a_jp_3_grapple_release_enters_release_so_a_held_space_never_damps() {
    let params = original();
    let world = ceiling_world();
    let start = standing_looking_up(&params, &world, 1.2);
    // Jump (Space held), attach, release the grapple while still rising,
    // then (a) release Space or (b) keep holding it: identical motion,
    // because the pawn is in `Release`, not `Jumped`.
    let run = |release_space: bool| {
        let mut s = start;
        tick(&mut s, &jump_press(input()), &params, &world);
        tick(&mut s, &held(true, true), &params, &world);
        for _ in 0..3 {
            tick(&mut s, &held(true, true), &params, &world);
        }
        let e = tick(&mut s, &held(true, false), &params, &world);
        assert!(e.gun.released.is_some(), "{e:?}");
        assert_eq!(s.script.code.state, PawnStateName::Release);
        assert!(s.velocity.z > 100.0, "rising after the release");
        let mut out = Vec::new();
        for _ in 0..20 {
            tick(&mut s, &held(!release_space, false), &params, &world);
            out.push(kinematics(&s));
        }
        assert_eq!(s.script.code.state, PawnStateName::Release);
        out
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn a5_tap_after_a_grapple_release_damps_the_rise() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    tick(&mut s, &jump_press(input()), &params, &world);
    for _ in 0..4 {
        tick(&mut s, &held(false, true), &params, &world);
    }
    tick(&mut s, &held(false, false), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::Release);
    // The tick after the release: a Space tap is a failed jump attempt that
    // enters `Jumped`; its release damps at once.
    let e = tick(&mut s, &jump_press(input()), &params, &world);
    assert!(!e.jumped);
    assert_eq!(s.script.code.state, PawnStateName::Jumped);
    let vz = s.velocity.z;
    assert!(vz > 100.0);
    tick(&mut s, &input(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    let expected = vz * JUMP_RELEASE_MULTIPLIER - 1040.0 * DT;
    assert!(
        (s.velocity.z - expected).abs() < 0.01,
        "{} vs {expected}",
        s.velocity.z
    );
}

// ---------------------------------------------------------------------------
// Controller states around the grapple (GRAPPLE.md G-PH-1 / G-RL-7).
// ---------------------------------------------------------------------------

#[test]
fn g_ph_1_move_input_does_not_accelerate_while_attached() {
    let params = original();
    let world = far_wall_world();
    let start = standing_looking_up(&params, &world, 0.0);
    let run = |steer: f32| {
        let mut s = start;
        tick(&mut s, &jump_press(input()), &params, &world);
        let mut out = Vec::new();
        for _ in 0..40 {
            let i = InputFrame {
                move_forward: steer,
                move_right: steer,
                grapple_held: true,
                ..InputFrame::default()
            };
            tick(&mut s, &i, &params, &world);
            assert!(s.is_grapple_attached());
            out.push(kinematics(&s));
        }
        out
    };
    assert_eq!(run(1.0), run(0.0));
}

#[test]
fn g_rl_7_release_tick_has_no_move_no_look_and_drops_the_jump_flag() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    tick(&mut s, &jump_press(input()), &params, &world);
    for _ in 0..4 {
        tick(&mut s, &held(false, true), &params, &world);
    }
    let reference = s;
    // Release tick with look, move and a jump press: the look delta is
    // dropped, there is no air acceleration and no jump attempt.
    let busy = InputFrame {
        move_forward: 1.0,
        look_yaw_delta: 0.3,
        look_pitch_delta: -0.2,
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &busy, &params, &world);
    assert!(e.gun.released.is_some());
    assert!(!e.jumped);
    assert_eq!((s.yaw, s.pitch), (reference.yaw, reference.pitch));
    assert_eq!(s.script.code.state, PawnStateName::Release, "no `Jumped`");
    assert!(!s.script.release_gap);
    // The same tick without any input gives the same motion.
    let mut quiet = reference;
    tick(&mut quiet, &held(false, false), &params, &world);
    assert_eq!(kinematics(&s), kinematics(&quiet));
    // The next tick is `PlayerWalking` again: look and air control work.
    let mut a = s;
    let mut b = s;
    tick(&mut a, &busy, &params, &world);
    tick(&mut b, &held(true, false), &params, &world);
    assert_ne!(a.yaw, s.yaw);
    assert!(a.velocity.x > b.velocity.x, "air control is back");
    assert_eq!(a.script.code.state, PawnStateName::Jumped);
}

// ---------------------------------------------------------------------------
// Story mode (A-ST-1/2, G-AC-0).
// ---------------------------------------------------------------------------

#[test]
fn a_st_2_fire_never_grapples_in_story_mode() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    let _ = pawn::enter_story_mode(&mut s, &params);
    let fire = held(false, true);
    let e = tick(&mut s, &fire, &params, &world);
    assert_eq!(
        e.gun.fire,
        Some(grapple_gun::FireOutcome::StoryMode { interacted: false })
    );
    assert!(e.gun.attached.is_none() && !s.is_grapple_attached());
    // Leaving story mode with the button still held does not fire again
    // (the press closed the latch, G-IN-3).
    pawn::exit_story_mode(&mut s, &params);
    for _ in 0..12 {
        let e = tick(&mut s, &fire, &params, &world);
        assert!(e.gun.fire.is_none() && e.gun.attached.is_none());
    }
    // Release; once the weapon's refire check has returned it to `Active`
    // (G-IN-2), a new press grapples at once.
    for _ in 0..10 {
        tick(&mut s, &input(), &params, &world);
    }
    let e = tick(&mut s, &fire, &params, &world);
    assert!(e.gun.attached.is_some(), "{e:?}");
    assert_eq!(s.script.code.state, PawnStateName::Shooting);
}

#[test]
fn a_st_1_story_entry_releases_the_grapple_into_story_state() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    tick(&mut s, &jump_press(input()), &params, &world);
    tick(&mut s, &held(true, true), &params, &world);
    assert!(s.is_grapple_attached());
    let released = pawn::enter_story_mode(&mut s, &params);
    assert_eq!(
        released.gun.released.map(|r| r.reason),
        Some(grapple_gun::ReleaseReason::Story)
    );
    assert!(!s.is_grapple_attached() && !s.pawn.flying);
    assert_eq!(s.script.code.state, PawnStateName::StoryState);
    // The release puts the controller into `ReleaseGrapple` for one tick.
    let yaw = s.yaw;
    let look = InputFrame {
        look_yaw_delta: 0.4,
        grapple_held: true,
        ..InputFrame::default()
    };
    tick(&mut s, &look, &params, &world);
    assert_eq!(s.yaw, yaw);
    tick(&mut s, &look, &params, &world);
    assert_ne!(s.yaw, yaw);
}

#[test]
fn a_jp_3_story_entry_ends_the_damping_loop() {
    let params = original();
    let world = flat_world();
    let mut s = standing_looking_up(&params, &world, 0.0);
    tick(&mut s, &jump_press(input()), &params, &world);
    tick(&mut s, &input(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    let _ = pawn::enter_story_mode(&mut s, &params);
    let mut vz = vec![s.velocity.z];
    for _ in 0..8 {
        tick(&mut s, &input(), &params, &world);
        vz.push(s.velocity.z);
    }
    for w in vz.windows(2) {
        assert!(
            (w[1] - (w[0] - 1040.0 * DT)).abs() < 0.01,
            "gravity only: {vz:?}"
        );
    }
}

#[test]
fn a_ac_2_story_landings_keep_air_control_0_3() {
    let params = original();
    let world = flat_world();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 300.0), 0.0);
    pawn::start(&mut s, &params);
    let _ = pawn::enter_story_mode(&mut s, &params);
    let mut handler = None;
    for _ in 0..300 {
        let e = tick(&mut s, &input(), &params, &world);
        if let Some(l) = e.landing {
            handler = Some(l.handler);
            break;
        }
    }
    assert_eq!(handler, Some(LandingHandler::Story));
    assert_eq!(s.script.air_control, 0.3);
    // After leaving story mode the next normal landing sets 0.35.
    pawn::exit_story_mode(&mut s, &params);
    tick(&mut s, &jump_press(input()), &params, &world);
    let mut normal = None;
    for _ in 0..300 {
        let e = tick(&mut s, &held(true, false), &params, &world);
        if let Some(l) = e.landing {
            normal = Some(l.handler);
            break;
        }
    }
    assert_eq!(normal, Some(LandingHandler::Normal));
    assert_eq!(s.script.air_control, 0.35);
}

// ---------------------------------------------------------------------------
// Jump flag, sprint, power jump.
// ---------------------------------------------------------------------------

#[test]
fn a_jp_1_a_jump_press_just_before_touchdown_is_not_buffered() {
    let params = original();
    let world = flat_world();
    // Find the tick in which a falling pawn lands.
    let drop = || {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 200.0), 0.0);
        pawn::start(&mut s, &params);
        s
    };
    let mut probe = drop();
    let mut landing_tick = None;
    for t in 0..300 {
        tick(&mut probe, &input(), &params, &world);
        if probe.grounded {
            landing_tick = Some(t);
            break;
        }
    }
    let landing_tick = landing_tick.expect("lands");
    // Press Space in the tick before the landing tick (still falling at the
    // controller move): the attempt fails and the flag is not kept.
    let mut s = drop();
    for t in 0..landing_tick + 30 {
        let i = if t + 1 == landing_tick {
            jump_press(input())
        } else {
            held(t + 1 > landing_tick, false)
        };
        let e = tick(&mut s, &i, &params, &world);
        assert!(!e.jumped, "tick {t}");
    }
    assert!(s.grounded && s.velocity.z == 0.0);
    // The landing ended the `Jumped` state of the failed attempt.
    assert_eq!(s.script.code.state, PawnStateName::HasLanded);
}

#[test]
fn a_wk_4_sprint_press_while_attached_only_arms() {
    let params = original();
    let world = ceiling_world();
    let mut s = standing_looking_up(&params, &world, 1.2);
    tick(&mut s, &held(false, true), &params, &world);
    assert!(s.is_grapple_attached());
    let sprint = InputFrame {
        sprint_held: true,
        grapple_held: true,
        ..InputFrame::default()
    };
    tick(&mut s, &sprint, &params, &world);
    assert!(s.script.sprint.armed && !s.script.sprint.active);
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn a_pj_4_power_jump_ends_in_falling_state_so_a_held_space_never_damps_it() {
    let params = original();
    let world = flat_world();
    // Land with Space held (press in the air), so Space is down when the
    // power jump fires; releasing it afterwards must not damp (FallingState).
    let fire = |release_space_after: bool| {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 120.0), 0.0);
        pawn::start(&mut s, &params);
        let press = InputFrame {
            jump_pressed: true,
            jump_held: true,
            power_jump_held: true,
            ..InputFrame::default()
        };
        tick(&mut s, &press, &params, &world);
        let hold = InputFrame {
            jump_held: true,
            power_jump_held: true,
            ..InputFrame::default()
        };
        for _ in 0..60 {
            tick(&mut s, &hold, &params, &world);
        }
        assert!(s.grounded && s.script.power_jump.charged);
        let release_pj = InputFrame {
            jump_held: true,
            ..InputFrame::default()
        };
        let e = tick(&mut s, &release_pj, &params, &world);
        assert_eq!(
            e.power_jump,
            Some(PowerJumpEvent::Fired {
                leap: false,
                jumped: true
            })
        );
        assert_eq!(s.script.code.state, PawnStateName::FallingState);
        let mut out = Vec::new();
        for _ in 0..30 {
            tick(&mut s, &held(!release_space_after, false), &params, &world);
            out.push(kinematics(&s));
        }
        (out, s)
    };
    let (released, s) = fire(true);
    let (kept, _) = fire(false);
    assert_eq!(released, kept, "power jumps are not damped");
    // A fresh tap while rising damps it (A-JP-3 consequence 1).
    let mut t = s;
    assert!(t.velocity.z > 100.0);
    tick(&mut t, &jump_press(input()), &params, &world);
    let vz = t.velocity.z;
    tick(&mut t, &input(), &params, &world);
    assert!((t.velocity.z - (vz * JUMP_RELEASE_MULTIPLIER - 1040.0 * DT)).abs() < 0.01);
}

#[test]
fn a_pj_3_power_jump_release_while_attached_cancels() {
    let params = original();
    let world = far_wall_world();
    let mut s = standing_looking_up(&params, &world, 0.0);
    let charge = InputFrame {
        power_jump_held: true,
        grapple_held: true,
        ..InputFrame::default()
    };
    for _ in 0..40 {
        tick(&mut s, &charge, &params, &world);
    }
    assert!(s.is_grapple_attached() && s.script.power_jump.charged);
    // Key-up while flying (attached): not Walking → cancel.
    let e = tick(&mut s, &held(false, true), &params, &world);
    assert_eq!(e.power_jump, Some(PowerJumpEvent::Canceled));
}

#[test]
fn a_pj_6_power_leap_apex_is_about_270() {
    let params = original();
    let world = flat_world();
    let mut s = standing_looking_up(&params, &world, 0.0);
    let sprint = InputFrame {
        move_forward: 1.0,
        sprint_held: true,
        ..InputFrame::default()
    };
    for _ in 0..60 {
        tick(&mut s, &sprint, &params, &world);
    }
    let charge = InputFrame {
        power_jump_held: true,
        ..sprint
    };
    for _ in 0..40 {
        tick(&mut s, &charge, &params, &world);
    }
    let e = tick(&mut s, &sprint, &params, &world);
    assert_eq!(
        e.power_jump,
        Some(PowerJumpEvent::Fired {
            leap: true,
            jumped: true
        })
    );
    let z0 = s.position.z;
    let mut apex = z0;
    while !s.grounded {
        tick(&mut s, &sprint, &params, &world);
        apex = apex.max(s.position.z);
    }
    // 750² / (4 · 520) ≈ 270.4 (ABILITIES.md A-PJ-6, effective gravity
    // −1040 uu/s²).
    assert!((apex - z0 - 270.4).abs() < 1.0, "{}", apex - z0);
}

// ---------------------------------------------------------------------------
// Timing model (§15).
// ---------------------------------------------------------------------------

#[test]
fn s15_damping_steps_are_three_ticks_apart_at_30_hz() {
    let params = original();
    let world = flat_world();
    let dt = 1.0 / 30.0;
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, &params);
    tick_dt(&mut s, &input(), &params, &world, dt);
    tick_dt(&mut s, &jump_press(input()), &params, &world, dt);
    // Release: ×0.7 now; then gravity only for two ticks; ×0.7 in the third.
    let mut deltas = Vec::new();
    let mut v = s.velocity.z;
    for _ in 0..7 {
        tick_dt(&mut s, &input(), &params, &world, dt);
        deltas.push(s.velocity.z - (v - 1040.0 * dt));
        v = s.velocity.z;
    }
    let damped: Vec<bool> = deltas.iter().map(|d| d.abs() > 1.0).collect();
    assert_eq!(
        damped,
        vec![true, false, false, true, false, false, true],
        "{deltas:?}"
    );
}

#[test]
fn grapple_gun_runs_are_bit_reproducible() {
    let params = original();
    let world = ceiling_world();
    let mut start = standing_looking_up(&params, &world, 1.0);
    asamu_player::rocket_boots::enable_rocket_boots(&mut start, true);
    let mut rng = common::SplitMix64(0x0C0F_FEE5);
    let inputs: Vec<InputFrame> = (0..900)
        .map(|_| InputFrame {
            move_forward: rng.range(-1.0, 1.0),
            move_right: rng.range(-1.0, 1.0),
            look_yaw_delta: rng.range(-0.05, 0.05),
            look_pitch_delta: rng.range(-0.03, 0.03),
            jump_pressed: rng.chance(0.05),
            jump_held: rng.chance(0.5),
            grapple_held: rng.chance(0.7),
            sprint_held: rng.chance(0.3),
            power_jump_held: rng.chance(0.2),
            use_pressed: false,
        })
        .collect();
    let run = || {
        let mut s = start;
        let mut events = Vec::new();
        for (i, input) in inputs.iter().enumerate() {
            if i == 400 {
                let _ = pawn::enter_story_mode(&mut s, &params);
            }
            if i == 500 {
                pawn::exit_story_mode(&mut s, &params);
            }
            events.push(tick(&mut s, input, &params, &world));
        }
        (serde_json::to_string(&s).expect("ser"), events)
    };
    let (a, ea) = run();
    let (b, eb) = run();
    assert_eq!(a, b);
    assert_eq!(ea, eb);
    assert!(ea.iter().any(|e| e.gun.attached.is_some()));
    assert!(ea.iter().any(|e| e.gun.released.is_some()));
}
