//! Rule-by-rule tests of the original rocket boots
//! (`asamu_player::rocket_boots`) against `docs/reverse-engineering/ABILITIES.md`
//! §7 (A-RB-1…8) and the move-input lock rules of §8. Expected values come
//! from the spec's timeline and formulas; worlds are synthetic.

mod common;

use asamu_player::events::SimEvent;
use asamu_player::pawn::{self, PawnStateName};
use asamu_player::rocket_boots::{self, BootsEvent, BootsStateName, rotate_by_aim};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, step_with,
};
use common::{DT, flat_world};
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

fn space() -> InputFrame {
    InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    }
}

fn hold_space() -> InputFrame {
    InputFrame {
        jump_held: true,
        ..InputFrame::default()
    }
}

fn idle() -> InputFrame {
    InputFrame::default()
}

/// A started pawn falling in an empty world (no floor; near the origin so
/// `f32` positions keep the velocity refinement precise), boots enabled.
fn falling(params: &PlayerParams, velocity: Vec3) -> PlayerState {
    let mut s = PlayerState::new(Vec3::ZERO, 0.0);
    pawn::start(&mut s, params);
    rocket_boots::enable_rocket_boots(&mut s, true);
    s.velocity = velocity;
    s
}

#[test]
fn a_rb_1_boots_start_disabled_and_kismet_enables_them() {
    let params = original();
    let world = BoxWorld::new();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 50_000.0), 0.0);
    pawn::start(&mut s, &params);
    assert!(!s.script.boots.enabled, "bEnabled starts false");
    assert_eq!(tick(&mut s, &space(), &params, &world).boots, None);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
    rocket_boots::enable_rocket_boots(&mut s, true);
    tick(&mut s, &idle(), &params, &world);
    assert_eq!(
        tick(&mut s, &space(), &params, &world).boots,
        Some(BootsEvent::Started)
    );
}

#[test]
fn a_rb_2_only_falling_and_not_in_story_mode() {
    let params = original();
    // On the ground the jump wins (physics is still Walking at the key
    // event; the jump happens at the controller move).
    let world = flat_world();
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, &params);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let e = tick(&mut s, &space(), &params, &world);
    assert!(e.jumped);
    assert_eq!(e.boots, None);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
    // In story mode nothing.
    let mut st = falling(&params, Vec3::ZERO);
    let _ = pawn::enter_story_mode(&mut st, &params);
    assert_eq!(
        tick(&mut st, &space(), &params, &BoxWorld::new()).boots,
        None
    );
}

#[test]
fn a_rb_3_4_charge_and_boost_timeline() {
    // A12: falling, aim level, tap Space: ×1.0/0.8/0.6/0.4/0.2 at 0.1 s
    // steps (6 ticks at 60 Hz), 0.6 s without writes, then 41 boost writes
    // 0.05 s apart (3 ticks): 2500 → 1250 along the aim plus the corkscrew
    // (1000 → 0, one turn per second, starting up).
    let params = original();
    let world = BoxWorld::new();
    let mut s = falling(&params, Vec3::new(300.0, 0.0, -400.0));
    let e = tick(&mut s, &space(), &params, &world);
    assert_eq!(e.boots, Some(BootsEvent::Started));
    assert!(
        e.kismet
            .contains(&SimEvent::PlayerRocketBoosted { boosting: false })
    );
    assert_eq!(s.script.move_input_lock, 1, "lock taken (it was 0)");
    let v0 = s.script.boots.v0;
    assert_eq!(s.velocity, v0, "first charge write is V0 × 1.0");
    // Charge writes.
    // Writes at ticks 6, 12, 18 and 24 (Sleep(0.1) = 6 ticks at 60 Hz).
    let mut tau = 0.1_f32;
    for write_tick in [6, 12, 18, 24] {
        for _ in 0..6 {
            tick(&mut s, &idle(), &params, &world);
        }
        let expected = v0 * ((1.0 - tau * 2.0) / 1.0);
        assert_eq!(s.velocity, expected, "charge write at tick {write_tick}");
        tau += 0.1;
    }
    // No sixth (zero) write: the float accumulator reaches exactly 0.5.
    let mut float_tau = 0.0_f32;
    let mut writes = 0;
    while float_tau < 0.5 {
        float_tau += 0.1;
        writes += 1;
    }
    assert_eq!(writes, 5);
    // Gap: only falling physics until the first boost write at tick 60.
    let mut boost_tick = None;
    for t in 25..80 {
        let e = tick(&mut s, &idle(), &params, &world);
        if e.kismet
            .contains(&SimEvent::PlayerRocketBoosted { boosting: true })
        {
            boost_tick = Some(t);
            break;
        }
    }
    assert_eq!(boost_tick, Some(60));
    // First boost write: â·2500 + R(â)·(1, 0, 1000) with â = +X (level).
    let aim = s.script.boots.aim;
    assert!((aim - Vec3::X).length() < 1e-6, "{aim}");
    assert!(
        (s.velocity - Vec3::new(2501.0, 0.0, 1000.0)).length() < 1e-3,
        "{}",
        s.velocity
    );
    // Every 3rd tick a write; check the formula at the 11th write (τ = 0.5,
    // s = 0.75, θ = 2π·0.5 = π: half a turn, the corkscrew points down).
    let mut write = 1;
    let mut t = 60;
    while write <= 10 {
        for _ in 0..3 {
            tick(&mut s, &idle(), &params, &world);
            t += 1;
        }
        write += 1;
    }
    assert_eq!(t, 90);
    let s_rem = 0.75_f32;
    let expected = Vec3::X * 2500.0 * (1.0 + s_rem) / 2.0
        + rotate_by_aim(Vec3::new(s_rem, 0.0, -1000.0 * s_rem), Vec3::X);
    assert!(
        (s.velocity - expected).length() < 0.5,
        "{} vs {expected}",
        s.velocity
    );
    // The boost ends after the 41st write (tick 180), at the next wake-up.
    let mut end_tick = None;
    for t in 91..200 {
        let e = tick(&mut s, &idle(), &params, &world);
        if e.boots == Some(BootsEvent::Finished) {
            end_tick = Some(t);
            break;
        }
    }
    assert_eq!(end_tick, Some(183));
    assert_eq!(s.script.boots.state, BootsStateName::Unavailable);
    assert_eq!(s.script.move_input_lock, 0, "lock released at the end");
    // The last write left ≈ 1250·â (corkscrew ≈ 0).
    assert!((s.velocity.x - 1250.0).abs() < 70.0, "{}", s.velocity);
}

#[test]
fn a_rb_5_one_boost_per_airtime_exhausted_sound_and_landing_rearms() {
    // A14.
    let params = original();
    let world = flat_world();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 4000.0), 0.0);
    pawn::start(&mut s, &params);
    rocket_boots::enable_rocket_boots(&mut s, true);
    tick(&mut s, &idle(), &params, &world);
    tick(&mut s, &space(), &params, &world);
    // Let the boost finish (3.05 s).
    for _ in 0..190 {
        tick(&mut s, &idle(), &params, &world);
        if s.script.boots.state == BootsStateName::Unavailable {
            break;
        }
    }
    assert_eq!(s.script.boots.state, BootsStateName::Unavailable);
    let e = tick(&mut s, &space(), &params, &world);
    assert_eq!(e.boots, Some(BootsEvent::Exhausted));
    assert_eq!(
        s.script.boots.state,
        BootsStateName::UnavailableAndPlayedSound
    );
    // Back to `Unavailable` after boostExhaustedDelay (1.0 s), no boost.
    for _ in 0..62 {
        tick(&mut s, &idle(), &params, &world);
    }
    assert_eq!(s.script.boots.state, BootsStateName::Unavailable);
    // Landing re-arms.
    for _ in 0..2000 {
        if tick(&mut s, &idle(), &params, &world).landing.is_some() {
            break;
        }
    }
    assert!(s.grounded);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
}

#[test]
fn a_rb_6_landing_during_a_boost_cancels_and_the_landing_releases_the_lock() {
    // A15.
    let params = original();
    let world = flat_world();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 150.0), 0.0);
    pawn::start(&mut s, &params);
    rocket_boots::enable_rocket_boots(&mut s, true);
    tick(&mut s, &idle(), &params, &world);
    tick(&mut s, &space(), &params, &world);
    assert_eq!(s.script.boots.state, BootsStateName::Boosting);
    assert_eq!(s.script.move_input_lock, 1);
    let mut landing = None;
    for _ in 0..120 {
        let e = tick(&mut s, &idle(), &params, &world);
        if e.landing.is_some() {
            landing = e.landing;
            break;
        }
    }
    let l = landing.expect("lands during the charge");
    assert!(l.boost_canceled);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
    assert_eq!(s.script.move_input_lock, 0);
    // No velocity writes after the cancel.
    let v = s.velocity;
    for _ in 0..30 {
        tick(&mut s, &idle(), &params, &world);
    }
    assert_eq!(s.velocity, v);
}

#[test]
fn a_rb_5_not_landable_floor_neither_rearms_nor_cancels() {
    use asamu_player::world::{ActorTag, Surface};
    let params = original();
    let world = BoxWorld::new().with_surface_box(
        Vec3::new(-1000.0, -1000.0, -100.0),
        Vec3::new(1000.0, 1000.0, 0.0),
        false,
        Surface {
            tag: ActorTag::NotLandable,
            ..Surface::default()
        },
    );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 150.0), 0.0);
    pawn::start(&mut s, &params);
    rocket_boots::enable_rocket_boots(&mut s, true);
    tick(&mut s, &idle(), &params, &world);
    tick(&mut s, &space(), &params, &world);
    let mut landed = false;
    for _ in 0..120 {
        if tick(&mut s, &idle(), &params, &world).landing.is_some() {
            landed = true;
            break;
        }
    }
    assert!(landed);
    assert_eq!(
        s.script.boots.state,
        BootsStateName::Boosting,
        "the boost keeps writing while the pawn walks"
    );
}

#[test]
fn a_rb_8_releasing_space_during_the_boost_damps_its_vertical_speed() {
    // A13: hold Space past τ = 1.0 and release during the boost: the pawn's
    // state code (before physics) multiplies V.z by 0.7 on a tick without a
    // boost write.
    let params = original();
    let world = BoxWorld::new();
    let mut s = falling(&params, Vec3::ZERO);
    let e = tick(&mut s, &space(), &params, &world);
    assert!(!e.jumped);
    assert_eq!(
        s.script.code.state,
        PawnStateName::Jumped,
        "failed jump attempt"
    );
    for _ in 1..=60 {
        tick(&mut s, &hold_space(), &params, &world);
    }
    // Tick 60 was the first boost write: V.z ≈ 1000 (corkscrew up).
    assert!(s.velocity.z > 900.0, "{}", s.velocity);
    let before = s;
    tick(&mut s, &idle(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    let expected = before.velocity.z * pawn::JUMP_RELEASE_MULTIPLIER - 1040.0 * DT;
    assert!(
        (s.velocity.z - expected).abs() < 0.05,
        "{} vs {expected}",
        s.velocity.z
    );
    assert_eq!(
        s.script.boots.state,
        BootsStateName::Boosting,
        "independent of the damping"
    );
}

#[test]
fn a_il_1_boost_during_a_leap_adds_no_lock_and_its_end_releases_the_leap_lock() {
    // A16 / ABILITIES.md §8.
    let params = original();
    let world = BoxWorld::new();
    let mut s = falling(&params, Vec3::new(800.0, 0.0, 0.0));
    s.script.move_input_lock = 1; // the leap's lock
    tick(&mut s, &space(), &params, &world);
    assert_eq!(s.script.move_input_lock, 1, "no second level");
    for _ in 0..200 {
        tick(&mut s, &idle(), &params, &world);
        if s.script.boots.state == BootsStateName::Unavailable {
            break;
        }
    }
    assert_eq!(
        s.script.move_input_lock, 0,
        "air control is back before landing"
    );
}

#[test]
fn boots_reset_returns_to_ready() {
    let params = original();
    let mut s = falling(&params, Vec3::ZERO);
    tick(&mut s, &space(), &params, &BoxWorld::new());
    assert_eq!(s.script.boots.state, BootsStateName::Boosting);
    rocket_boots::reset_boots(&mut s.script.boots);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
    assert_eq!(s.script.boots.tau, 0.0);
}
