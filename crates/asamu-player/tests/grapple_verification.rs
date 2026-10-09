//! Adversarial verification of the original grapple gun, power jump and
//! rocket boots against `docs/reverse-engineering/GRAPPLE.md` and
//! `ABILITIES.md` (verification pass 2): boundary values of the force law,
//! the cap and the range checks, releases inside the attach tick, ability
//! combinations inside one tick, `dt` extremes, hostile input and state
//! completeness (determinism through a serialise/deserialise round trip).
//! Every expected value comes from the spec's rules and formulas; the worlds
//! are synthetic boxes.

mod common;

use asamu_player::events::SimEvent;
use asamu_player::grapple_gun::{self, FireOutcome, ReleaseReason, WeaponState};
use asamu_player::pawn::{self, PawnStateName, PowerJumpEvent};
use asamu_player::rocket_boots::{self, BootsEvent, BootsStateName};
use asamu_player::sim::MAX_STEP_DT;
use asamu_player::ue3_movement::MIN_TICK_TIME;
use asamu_player::world::{ActorClass, ActorTag, Surface};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, step_with,
};
use common::{DT, SplitMix64};
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

fn fire() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

fn idle() -> InputFrame {
    InputFrame::default()
}

/// A started pawn at rest in the air, looking along (`yaw`, `pitch`), with
/// capacity `max_grapples`.
fn floating(
    params: &PlayerParams,
    position: Vec3,
    yaw: f32,
    pitch: f32,
    max_grapples: i32,
) -> PlayerState {
    let mut s = PlayerState::new(position, yaw);
    s.pitch = pitch;
    pawn::start(&mut s, params);
    grapple_gun::set_max_grapples(&mut s, max_grapples);
    s
}

/// Pitch that makes the eye (38 above the centre) look at a point at the
/// centre's height `distance` ahead.
fn level_pitch(distance: f32) -> f32 {
    (-38.0_f32).atan2(distance)
}

/// A grapple-able wall whose face is at `x` (facing −X).
fn wall_at(x: f32) -> BoxWorld {
    BoxWorld::new().with_box(
        Vec3::new(x, -5000.0, -5000.0),
        Vec3::new(x + 100.0, 5000.0, 8000.0),
        true,
    )
}

/// A cube target of half size `h` at `c`.
fn cube(world: BoxWorld, c: Vec3, h: f32, surface: Surface) -> BoxWorld {
    world.with_surface_box(c - Vec3::splat(h), c + Vec3::splat(h), true, surface)
}

fn flower(id: u32) -> Surface {
    Surface {
        actor: Some(id),
        class: ActorClass::GlowFlower,
        tag: ActorTag::None,
    }
}

/// The pull increment of one gun tick, `dt · unit · 10⁷ / d`, computed the
/// way GRAPPLE.md G-PH-2 states it.
fn pull(dt: f32, d: f32) -> f32 {
    dt * 10_000.0 / (d / 1000.0)
}

// ---------------------------------------------------------------------------
// Force law and cap (G-PH-2/3).
// ---------------------------------------------------------------------------

#[test]
fn g_ph_2_pull_is_ten_million_over_d_at_the_spec_reference_distances() {
    // G-PH-2: 2 000 uu/s² at 5000 uu, 10 000 at 1000, 50 000 at 200. From
    // rest the attach tick's physics moves nothing (flying, no gravity), so
    // the stored velocity after the first gun tick is exactly one pull.
    let params = original();
    for (d, accel) in [
        (4999.0_f32, 1.0e7_f32 / 4999.0),
        (1000.0, 10_000.0),
        (201.0, 1.0e7 / 201.0),
    ] {
        let world = wall_at(d);
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, level_pitch(d), 3);
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(e.gun.attached.is_some(), "{d}");
        assert!(e.gun.released.is_none(), "{d}: outside 200, no release");
        let measured = s.script.gun.distance;
        assert!((measured - d).abs() < 0.01, "{d}: {measured}");
        let expected = pull(DT, measured);
        assert!(
            (s.velocity.x - expected).abs() <= expected * 1e-5,
            "{d}: {} vs {expected}",
            s.velocity.x
        );
        assert!((s.velocity.x / DT - accel).abs() <= accel * 1e-4, "{d}");
        assert!(s.velocity.y.abs() < 1e-3 && s.velocity.z.abs() < 1e-2);
    }
}

#[test]
fn g_ph_3_the_cap_limits_the_3d_magnitude_and_keeps_the_direction() {
    // The flying update caps |V| at AirSpeed = 2000 (3-D, not per axis) and
    // never changes the direction (drag and cap are both scalar). A flower
    // keeps the pull away (instant-release target), so only physics acts.
    let params = original();
    let world = cube(
        BoxWorld::new(),
        Vec3::new(400.0, 0.0, 1038.0),
        30.0,
        flower(5),
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let v0 = Vec3::new(-2000.0, 2500.0, -1500.0);
    s.velocity = v0;
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(e.gun.attached.unwrap().instant_release);
    assert!((s.speed() - 2000.0).abs() < 0.05, "{}", s.speed());
    let dir = s.velocity.normalize();
    assert!(
        (dir - v0.normalize()).length() < 1e-4,
        "{dir} vs {}",
        v0.normalize()
    );
    // Below the cap only the drag (1 − 0.15·dt)² acts, again per magnitude.
    let mut slow = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let v1 = Vec3::new(-600.0, 700.0, 800.0);
    slow.velocity = v1;
    tick(&mut slow, &fire(), &params, &world);
    let f = 1.0 - 0.15 * DT;
    assert!(
        (slow.velocity - v1 * f * f).length() < 1e-2,
        "{} vs {}",
        slow.velocity,
        v1 * f * f
    );
}

#[test]
fn g_ph_7_no_gravity_and_no_terminal_velocity_clamp_while_attached() {
    // An attached pawn moving straight down at the cap: no gravity is added
    // (only the drag acts on V.z) — and neither gravity nor the falling
    // terminal velocity (10 000) applies to flying.
    let params = original();
    let world = cube(
        BoxWorld::new(),
        Vec3::new(400.0, 0.0, 1038.0),
        30.0,
        flower(6),
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 30_000.0), 0.0, 0.0, 3);
    s.position = Vec3::new(0.0, 0.0, 1000.0);
    s.velocity = Vec3::new(0.0, 0.0, -2000.0);
    tick(&mut s, &fire(), &params, &world);
    let f = 1.0 - 0.15 * DT;
    assert!(
        (s.velocity.z + 2000.0 * f * f).abs() < 0.05,
        "{}",
        s.velocity
    );
    assert_eq!(s.velocity.x, 0.0);
    // The flower's 0.05 s timer releases at the end of the 3rd tick: until
    // then only drag acts on V.z (no −1040 uu/s² of gravity, G-PH-7)...
    for i in 0..2 {
        let vz = s.velocity.z;
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(
            (s.velocity.z - vz * f * f).abs() < 0.05,
            "{i}: {}",
            s.velocity
        );
        assert_eq!(e.gun.released.is_some(), i == 1);
    }
    // ... and the first falling tick adds the effective gravity
    // (2 × −520 uu/s², NATIVE_PHYSICS.md 4.6) and no drag.
    let vz = s.velocity.z;
    tick(&mut s, &fire(), &params, &world);
    assert!(
        (s.velocity.z - (vz - 1040.0 * DT)).abs() < 0.05,
        "{}",
        s.velocity
    );
}

// ---------------------------------------------------------------------------
// Range checks (G-AC-5, G-TG-1).
// ---------------------------------------------------------------------------

#[test]
fn g_ac_5_the_range_check_is_strict_at_exactly_5000_and_the_trace_ends_at_16384() {
    let params = original();
    let s0 = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    assert_eq!(s0.view_direction(), Vec3::X, "level view along +X");
    let eye = s0.view_location(&params);
    assert_eq!(eye, Vec3::new(0.0, 0.0, 1038.0));
    // Exactly 5000 from the eye: `distance < fMaxDistance` fails.
    let world = wall_at(5000.0);
    let aim = grapple_gun::aim(&s0, &params, &world).unwrap();
    assert_eq!(aim.distance, 5000.0);
    assert!(!aim.acceptable);
    let mut s = s0;
    assert_eq!(
        tick(&mut s, &fire(), &params, &world).gun.fire,
        Some(FireOutcome::Failed)
    );
    assert!(s.script.gun.has_grappled, "a range failure marks the press");
    // The next float below 5000 attaches.
    let just_inside = f32::from_bits(5000.0_f32.to_bits() - 1);
    let world = wall_at(just_inside);
    let mut s = s0;
    assert_eq!(
        tick(&mut s, &fire(), &params, &world).gun.fire,
        Some(FireOutcome::Attached)
    );
    // Inside the 16 384 uu trace but beyond 5000: a hit actor, out of range.
    let world = wall_at(16_000.0);
    let aim = grapple_gun::aim(&s0, &params, &world).unwrap();
    assert!(aim.impact.hit.is_some());
    // Beyond the trace: no hit actor; the impact location is the trace end.
    let world = wall_at(16_500.0);
    let aim = grapple_gun::aim(&s0, &params, &world).unwrap();
    assert!(aim.impact.hit.is_none());
    assert_eq!(aim.impact.location, eye + Vec3::X * 16_384.0);
    let mut s = s0;
    assert_eq!(
        tick(&mut s, &fire(), &params, &world).gun.fire,
        Some(FireOutcome::Failed)
    );
}

#[test]
fn g_ix_3_the_boost_break_is_strictly_beyond_5000_and_uses_the_last_measured_distance() {
    // The boost write checks the distance the gun measured in its previous
    // tick (`vDistance`); attaching does not refresh it, so in the attach
    // tick a stale value from the previous grapple decides (GRAPPLE.md
    // G-IX-3 / ABILITIES.md A-RB-4 break rule: "last measured by the gun").
    let params = original();
    let world = wall_at(3000.0);
    for (stale, breaks) in [(5000.0_f32, false), (5000.5, true)] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        rocket_boots::enable_rocket_boots(&mut s, true);
        s.velocity = Vec3::new(0.0, 0.0, -100.0);
        let space = InputFrame {
            jump_pressed: true,
            jump_held: true,
            ..InputFrame::default()
        };
        tick(&mut s, &space, &params, &world);
        // Wait for the boost phase, then stop one tick before a write.
        while s.script.boots.aim == Vec3::ZERO {
            tick(&mut s, &idle(), &params, &world);
        }
        while s.script.boots.sleep.is_some_and(|r| r - DT >= 0.5 * DT) {
            tick(&mut s, &idle(), &params, &world);
        }
        // The next tick's boots run writes; attach in its input event with a
        // stale distance left by an earlier grapple.
        s.script.gun.distance = stale;
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(e.gun.attached.is_some());
        let ended = s.script.boots.state == BootsStateName::Unavailable;
        assert_eq!(ended, breaks, "stale distance {stale}");
        // The gun measured the real distance afterwards.
        assert!(s.script.gun.distance < 3100.0);
    }
}

// ---------------------------------------------------------------------------
// Releases inside the attach tick (G-RL-2/3, G-TM-4).
// ---------------------------------------------------------------------------

#[test]
fn g_rl_2_attaching_inside_the_release_distance_releases_in_the_same_tick() {
    // The fire runs in the input event; the same tick's gun tick measures
    // d < 200, adds the pull, halves and releases (GRAPPLE.md G-PH-2 order).
    let params = original();
    let d = 150.0_f32;
    let world = wall_at(d);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, level_pitch(d), 3);
    let e = tick(&mut s, &fire(), &params, &world);
    let a = e.gun.attached.expect("attached");
    let r = e.gun.released.expect("released in the attach tick");
    assert_eq!(r.reason, ReleaseReason::Proximity);
    assert!((a.anchor - Vec3::new(d, 0.0, 1000.0)).length() < 0.01);
    // Velocity: (0 + one pull) / 2.
    let measured = s.script.gun.distance;
    assert!((measured - d).abs() < 0.01);
    let expected = pull(DT, measured) * 0.5;
    assert!(
        (r.velocity.x - expected).abs() <= expected * 1e-5,
        "{} vs {expected}",
        r.velocity.x
    );
    assert_eq!(s.velocity, r.velocity);
    // Budget spent, Falling, pawn `Release`, controller `ReleaseGrapple`
    // pending, released flag already cleared by the same gun tick (G-RL-6).
    assert_eq!(s.script.gun.times_grappled, 1);
    assert!(!s.pawn.flying && !s.grounded && !s.is_grapple_attached());
    assert_eq!(s.script.code.state, PawnStateName::Release);
    assert!(s.script.release_gap);
    assert!(!s.script.gun.released);
    assert_eq!(s.script.air_speed, 2000.0);
    assert_eq!(
        e.kismet.iter().collect::<Vec<_>>(),
        vec![
            SimEvent::PlayerGrappled { originator: None },
            SimEvent::PlayerReleasedGrapple
        ]
    );
    // Next tick: the `ReleaseGrapple` controller tick drops the look input;
    // holding the button does not re-fire (latch); falling physics with
    // gravity (2 × −520) starts from the halved velocity.
    let yaw = s.yaw;
    let v = s.velocity;
    let e = tick(
        &mut s,
        &InputFrame {
            look_yaw_delta: 0.3,
            ..fire()
        },
        &params,
        &world,
    );
    assert_eq!(s.yaw, yaw, "one tick without look (G-RL-7)");
    assert_eq!(e.gun.fire, None);
    assert!(
        (s.velocity.z - (v.z - 1040.0 * DT)).abs() < 0.05,
        "{}",
        s.velocity
    );
}

#[test]
fn g_rl_3_the_instant_release_timer_is_strict_and_frame_quantised() {
    // 0.05 s one-shot timer counted from the attach tick's gun tick:
    // 60 Hz → 3rd tick (index 2); 20 Hz (dt == 0.05 exactly) → 2nd tick
    // (0.05 is not > 0.05); 15 Hz → in the attach tick itself.
    let params = original();
    let world = cube(
        BoxWorld::new(),
        Vec3::new(1500.0, 0.0, 1038.0),
        50.0,
        flower(3),
    );
    for (dt, release_index) in [(1.0_f32 / 60.0, 2_u32), (1.0 / 20.0, 1), (1.0 / 15.0, 0)] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let mut released_at = None;
        for t in 0..10 {
            let e = tick_dt(&mut s, &fire(), &params, &world, dt);
            if t == 0 {
                assert!(e.gun.attached.unwrap().instant_release);
            }
            if let Some(r) = e.gun.released {
                assert_eq!(r.reason, ReleaseReason::InstantTimer);
                assert!(e.kismet.contains(&SimEvent::ActorUngrappled { actor: 3 }));
                released_at = Some(t);
                break;
            }
            assert_eq!(s.velocity, Vec3::ZERO, "no pull, no gravity: {dt}");
        }
        assert_eq!(released_at, Some(release_index), "dt {dt}");
        assert_eq!(s.script.gun.instant_release_timer, None);
    }
}

#[test]
fn g_rl_6_the_proximity_release_is_suspended_while_boosting_even_inside_200() {
    // Boost charge running, then attach (G-IX-3): the charge keeps writing
    // absolute velocities while flying, the gun neither pulls nor releases
    // while the boots are `Boosting` — not even inside 200 uu — and the pull
    // resumes after the boost ends.
    let params = original();
    let world = wall_at(600.0);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(600.0),
        3,
    );
    rocket_boots::enable_rocket_boots(&mut s, true);
    s.velocity = Vec3::new(300.0, 0.0, -400.0);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &space, &params, &world);
    assert_eq!(e.boots, Some(BootsEvent::Started));
    let v0 = s.script.boots.v0;
    // Attach during the charge.
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(e.gun.attached.is_some());
    assert!(s.pawn.flying);
    // Charge writes at ticks 6, 12, 18, 24 after the press: absolute
    // V0·(1 − 2τ), nothing added by the gun afterwards.
    let mut t = 1;
    let mut tau = 0.1_f32;
    for write_tick in [6, 12, 18, 24] {
        while t < write_tick {
            let before = s.velocity;
            tick(&mut s, &fire(), &params, &world);
            t += 1;
            if t < write_tick {
                // Physics only: drag (or a wall) can only slow it down.
                assert!(s.speed() <= before.length() + 1e-3, "tick {t}");
            }
        }
        assert_eq!(
            s.velocity,
            v0 * (1.0 - tau * 2.0),
            "charge write {write_tick}"
        );
        tau += 0.1;
    }
    // Through the boost: no pull and no proximity release while boosting.
    let mut inside = 0;
    let mut finished = false;
    for _ in 0..200 {
        let e = tick(&mut s, &fire(), &params, &world);
        if s.script.boots.state == BootsStateName::Boosting {
            assert!(e.gun.released.is_none(), "suspended while boosting");
            assert!(s.is_grapple_attached());
            if s.script.gun.distance < 200.0 {
                inside += 1;
            }
        } else {
            finished = e.boots == Some(BootsEvent::Finished) || finished;
            break;
        }
    }
    assert!(finished, "the boost ran to its end");
    assert!(inside > 0, "the boost pressed the pawn within 200 uu");
    // The boots ended in their state code before the gun's tick, so the same
    // tick (or a later one) pulls again and releases once within 200.
    let mut released = s.script.gun.attached.is_none();
    for _ in 0..120 {
        if released {
            break;
        }
        released = tick(&mut s, &fire(), &params, &world)
            .gun
            .released
            .is_some_and(|r| r.reason == ReleaseReason::Proximity);
    }
    assert!(released, "the proximity release resumes after the boost");
}

// ---------------------------------------------------------------------------
// Ability combinations inside one tick.
// ---------------------------------------------------------------------------

/// A sprinting pawn running along +X on a floor at z = 0 with a charged
/// power jump (key held), facing a grapple-able wall far ahead.
fn sprinting_with_charged_power_jump(params: &PlayerParams, world: &BoxWorld) -> PlayerState {
    let mut s = common::standing(params, world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, params);
    grapple_gun::set_max_grapples(&mut s, 3);
    let run = InputFrame {
        move_forward: 1.0,
        sprint_held: true,
        power_jump_held: true,
        ..InputFrame::default()
    };
    for _ in 0..60 {
        tick(&mut s, &run, params, world);
    }
    assert!(s.grounded && s.script.sprint.active);
    assert!(s.script.power_jump.charged, "0.6 s charge done");
    s
}

#[test]
fn power_leap_released_in_the_attach_tick_writes_its_velocity_while_flying() {
    // Key-up (walking → `Jumping`) and a fire press in the same tick: the
    // fire attaches in the input event (Flying, lock −1 → 0), then the
    // power-jump actor runs after physics: the leap's jump attempt fails
    // (not walking) but its velocity writes are unconditional (A-PJ-4:
    // horizontal × 2, V.z = 750) and it takes the move-input lock; the pawn
    // state becomes `FallingState`. The gun then adds its pull.
    let params = original();
    let world = BoxWorld::new().with_ground(0.0, false).with_box(
        Vec3::new(3000.0, -2000.0, 0.0),
        Vec3::new(3100.0, 2000.0, 3000.0),
        true,
    );
    let mut s = sprinting_with_charged_power_jump(&params, &world);
    s.pitch = level_pitch(3000.0 - s.position.x);
    let mut probe = s;
    let release_and_fire = InputFrame {
        move_forward: 1.0,
        sprint_held: true,
        power_jump_held: false,
        grapple_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &release_and_fire, &params, &world);
    assert!(e.gun.attached.is_some());
    assert_eq!(
        e.power_jump,
        Some(PowerJumpEvent::Fired {
            leap: true,
            jumped: false
        })
    );
    assert!(s.pawn.flying && s.is_grapple_attached(), "still attached");
    assert_eq!(s.script.move_input_lock, 1, "attach −1 (floor 0), leap +1");
    assert_eq!(s.script.code.state, PawnStateName::FallingState);
    assert!(!s.script.sprint.active && s.script.sprint.armed);
    // Reconstruct: the same tick without the power-jump key-up gives the
    // physics velocity V_p plus the pull P; with the leap it is
    // (2·V_p.x, 2·V_p.y, 750) + P.
    let no_leap = InputFrame {
        power_jump_held: true,
        ..release_and_fire
    };
    tick(&mut probe, &no_leap, &params, &world);
    let p = Vec3::new(pull(DT, s.script.gun.distance), 0.0, 0.0);
    let vp = probe.velocity - p;
    let expected = Vec3::new(vp.x * 2.0, vp.y * 2.0, 750.0) + p;
    assert!(
        (s.velocity - expected).length() < 0.5,
        "{} vs {expected}",
        s.velocity
    );
}

#[test]
fn g_at_5_a_grapple_ends_the_power_leap_lock_and_air_control_returns_after_release() {
    // Power leap (lock 1, no air acceleration), grapple mid-air (lock 0),
    // release: after the one-tick controller gap the move axes accelerate
    // the falling pawn again (ABILITIES.md A-IL-1, GRAPPLE.md G-AT-5).
    let params = original();
    let world = BoxWorld::new().with_ground(0.0, false).with_box(
        Vec3::new(3000.0, -2000.0, 0.0),
        Vec3::new(3100.0, 2000.0, 3000.0),
        true,
    );
    let mut s = sprinting_with_charged_power_jump(&params, &world);
    let leap = InputFrame {
        move_forward: 1.0,
        sprint_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &leap, &params, &world);
    assert_eq!(
        e.power_jump,
        Some(PowerJumpEvent::Fired {
            leap: true,
            jumped: true
        })
    );
    assert_eq!(s.script.move_input_lock, 1);
    // A few ticks up, aim at the wall, grapple, release two ticks later.
    for _ in 0..3 {
        tick(&mut s, &leap, &params, &world);
    }
    let attach = InputFrame {
        grapple_held: true,
        ..leap
    };
    let e = tick(&mut s, &attach, &params, &world);
    assert!(e.gun.attached.is_some());
    assert_eq!(s.script.move_input_lock, 0);
    tick(&mut s, &attach, &params, &world);
    let e = tick(&mut s, &leap, &params, &world);
    assert_eq!(e.gun.released.unwrap().reason, ReleaseReason::Button);
    // The release tick itself is the controller gap; the next one steers.
    let strafe = InputFrame {
        move_right: 1.0,
        ..InputFrame::default()
    };
    let vy = s.velocity.y;
    tick(&mut s, &strafe, &params, &world);
    assert!(
        s.velocity.y > vy + 1.0,
        "air control is back: {} -> {}",
        vy,
        s.velocity.y
    );
}

#[test]
fn g_rl_7_a_boost_started_in_the_release_gap_tick_has_no_jump_attempt() {
    // Space in the tick after a gun-tick release: the boots' key handler runs
    // in the input event (falling → boost starts), but the controller is in
    // `ReleaseGrapple`, whose tick has no move and whose exit clears the jump
    // flag — no jump attempt, so the pawn never enters `Jumped` and releasing
    // Space damps nothing (G-RL-7 with A-RB-2 / A-RB-8).
    let params = original();
    let d = 150.0;
    let world = wall_at(d);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, level_pitch(d), 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(
        e.gun.released.is_some(),
        "proximity release in the attach tick"
    );
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &space, &params, &world);
    assert_eq!(e.boots, Some(BootsEvent::Started));
    assert_eq!(s.script.code.state, PawnStateName::Release);
    let e = tick(&mut s, &idle(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::Release, "no damping");
    assert!(e.boots.is_none());
    // Contrast: the same press one tick later (after the gap) is a failed
    // jump attempt that enters `Jumped`.
    let mut later = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, level_pitch(d), 3);
    rocket_boots::enable_rocket_boots(&mut later, true);
    tick(&mut later, &fire(), &params, &world);
    tick(&mut later, &idle(), &params, &world);
    tick(&mut later, &space, &params, &world);
    assert_eq!(later.script.code.state, PawnStateName::Jumped);
}

// ---------------------------------------------------------------------------
// dt extremes and hostile input (determinism / robustness).
// ---------------------------------------------------------------------------

#[test]
fn slices_below_min_tick_time_move_nothing_but_the_gun_still_pulls() {
    // `startNewPhysics` ignores slices below 0.0003 s (NATIVE_PHYSICS.md
    // 1.3), but the gun's tick still adds `dt·10⁷/d` — at such frame times
    // velocity accumulates while the pawn stands still (an arithmetic
    // consequence shared with the original).
    let params = original();
    let world = wall_at(3000.0);
    let dt = MIN_TICK_TIME * 0.5;
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(3000.0),
        3,
    );
    let p0 = s.position;
    let e = tick_dt(&mut s, &fire(), &params, &world, dt);
    assert!(e.gun.attached.is_some());
    for _ in 0..200 {
        tick_dt(&mut s, &fire(), &params, &world, dt);
    }
    assert_eq!(s.position, p0);
    let expected = 201.0 * pull(dt, 3000.0);
    assert!(
        (s.velocity.x - expected).abs() < expected * 1e-3,
        "{} vs {expected}",
        s.velocity.x
    );
    assert!(s.is_grapple_attached());
}

#[test]
fn the_largest_dt_is_clamped_and_every_timer_and_release_stays_consistent() {
    // dt = 0.25 (the step bound) and larger values (clamped to it): the
    // instant release fires in the attach tick, the looping refire timer fires
    // twice in one tick (0.25 / 0.1, remainder kept), the held button never
    // re-fires, and no value becomes non-finite.
    let params = original();
    let world = cube(
        BoxWorld::new(),
        Vec3::new(1500.0, 0.0, 1038.0),
        50.0,
        flower(4),
    );
    let reference = {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let e = tick_dt(&mut s, &fire(), &params, &world, MAX_STEP_DT);
        assert!(e.gun.attached.is_some());
        assert_eq!(e.gun.released.unwrap().reason, ReleaseReason::InstantTimer);
        assert_eq!(s.script.gun.weapon, WeaponState::Firing, "still pending");
        let left = s.script.gun.refire_timer.unwrap();
        assert!((left - 0.05).abs() < 1e-6, "remainder {left}");
        (s, e)
    };
    for dt in [1.0_f32, 1.0e9, f32::MAX] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let e = tick_dt(&mut s, &fire(), &params, &world, dt);
        assert!(e.dt_clamped);
        assert_eq!(s, reference.0, "dt {dt}");
        assert_eq!(e.gun, reference.1.gun);
    }
    // Non-finite or non-positive dt: nothing at all, not even the press.
    for dt in [0.0_f32, -0.1, f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let before = s;
        let e = step_with(&Ue3PawnMovement, &mut s, &fire(), &params, &world, dt);
        assert_eq!(e, StepEvents::default(), "dt {dt}");
        assert_eq!(s, before);
    }
}

#[test]
fn hostile_input_and_state_never_reach_the_gun_as_non_finite_values() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let nasty = InputFrame {
        move_forward: f32::NAN,
        move_right: f32::INFINITY,
        look_yaw_delta: f32::NAN,
        look_pitch_delta: f32::NEG_INFINITY,
        grapple_held: true,
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &nasty, &params, &world);
    assert!(e.gun.attached.is_some(), "the press still works: {e:?}");
    for _ in 0..30 {
        tick(&mut s, &nasty, &params, &world);
    }
    assert!(s.script.gun.is_finite() && s.is_finite());
    // A poisoned anchor (state corrupted from outside) is rejected without
    // any change instead of spreading NaN.
    let mut bad = s;
    bad.script.gun.grapple_location = Vec3::new(f32::NAN, 0.0, 0.0);
    let snapshot = bad;
    let e = step_with(&Ue3PawnMovement, &mut bad, &fire(), &params, &world, DT);
    assert!(e.non_finite_rejected);
    assert_eq!(
        serde_json::to_string(&bad).unwrap(),
        serde_json::to_string(&snapshot).unwrap()
    );
}

#[test]
fn a_pawn_exactly_at_the_anchor_releases_without_a_nan() {
    // d == 0 (only reachable by state set-up): the pull is skipped (the
    // original would divide by zero), the proximity release still happens.
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    tick(&mut s, &fire(), &params, &world);
    assert!(s.is_grapple_attached());
    s.script.gun.grapple_location = s.position;
    s.velocity = Vec3::ZERO;
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.released.unwrap().reason, ReleaseReason::Proximity);
    assert_eq!(s.velocity, Vec3::ZERO);
    assert!(s.is_finite());
}

/// A deterministic random world: a floor (sometimes `NotLandable`), walls,
/// grapple targets of every class, a `NotGrappleAble` slab.
fn random_world(rng: &mut SplitMix64) -> BoxWorld {
    let floor_tag = if rng.chance(0.3) {
        ActorTag::NotLandable
    } else {
        ActorTag::None
    };
    let mut w = BoxWorld::new().with_surface_box(
        Vec3::new(-20_000.0, -20_000.0, -200.0),
        Vec3::new(20_000.0, 20_000.0, 0.0),
        true,
        Surface {
            tag: floor_tag,
            ..Surface::default()
        },
    );
    let classes = [
        ActorClass::StaticMesh,
        ActorClass::InterpActor,
        ActorClass::RechargeCrystal { charged: true },
        ActorClass::RechargeCrystal { charged: false },
        ActorClass::GlowFlower,
        ActorClass::FloatingRock,
        ActorClass::FallingRock,
    ];
    for i in 0..24_u32 {
        let c = Vec3::new(
            rng.range(-4000.0, 4000.0),
            rng.range(-4000.0, 4000.0),
            rng.range(100.0, 3000.0),
        );
        let h = rng.range(20.0, 300.0);
        let class = classes[(rng.next_u64() % classes.len() as u64) as usize];
        let tag = match rng.next_u64() % 6 {
            0 => ActorTag::TopOnlyGrappleAble,
            1 => ActorTag::BottomOnlyGrappleAble,
            2 => ActorTag::GrappleInteractable,
            _ => ActorTag::None,
        };
        let surface = Surface {
            actor: Some(100 + i),
            class,
            tag,
        };
        w = w.with_surface_box(
            c - Vec3::splat(h),
            c + Vec3::splat(h),
            rng.chance(0.9),
            surface,
        );
        w.set_actor_location(100 + i, c);
    }
    w
}

#[test]
fn random_play_keeps_every_gun_and_boots_invariant() {
    // Fuzz-style: random worlds, random (partly non-finite) input, random dt
    // including slices below MIN_TICK_TIME and above the clamp. Invariants
    // that the specs imply after every tick:
    // - flying ⇔ attached (only the grapple sets PHYS_Flying, G-AT-6/G-RL-1);
    // - never walking and flying at once;
    // - the used count stays in [0, 3] (the light manager's clamp, G-CT-6),
    //   except right after an attach served by the refire check, which runs
    //   in the gun's timers after the clamp (count 4 until the next gun tick,
    //   exactly as in the original; no press can be evaluated before that);
    // - while attached, not boosting and no leap write this tick, the stored
    //   speed is at most the 2000 cap plus one pull increment (G-PH-5);
    // - the event log never overflows and the state stays finite.
    let params = original();
    for seed in [1_u64, 2, 3, 0xACE] {
        let mut rng = SplitMix64(seed);
        let world = random_world(&mut rng);
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1500.0), 0.0, 0.0, 3);
        rocket_boots::enable_rocket_boots(&mut s, true);
        let mut attaches = 0;
        let mut boosts = 0;
        for t in 0..3000 {
            let pick = rng.next_u64() % 100;
            let dt = match pick {
                0 => MIN_TICK_TIME * 0.25,
                1 => 0.4,
                2..=20 => 1.0 / 30.0,
                21..=30 => 1.0 / 144.0,
                _ => DT,
            };
            let mut input = InputFrame {
                move_forward: rng.range(-1.2, 1.2),
                move_right: rng.range(-1.2, 1.2),
                look_yaw_delta: rng.range(-0.08, 0.08),
                look_pitch_delta: rng.range(-0.06, 0.06),
                jump_pressed: rng.chance(0.04),
                jump_held: rng.chance(0.5),
                grapple_held: rng.chance(0.55),
                sprint_held: rng.chance(0.5),
                power_jump_held: rng.chance(0.2),
                use_pressed: rng.chance(0.01),
            };
            if rng.chance(0.01) {
                input.look_yaw_delta = f32::NAN;
                input.move_forward = f32::INFINITY;
            }
            if rng.chance(0.002) {
                grapple_gun::set_max_grapples(&mut s, (rng.next_u64() % 6) as i32 - 1);
            }
            let e = tick_dt(&mut s, &input, &params, &world, dt);
            if e.gun.attached.is_some() {
                attaches += 1;
            }
            if e.boots == Some(BootsEvent::BoostBegan) {
                boosts += 1;
            }
            assert_eq!(
                s.pawn.flying,
                s.script.gun.is_attached(),
                "seed {seed} tick {t}"
            );
            assert!(!(s.grounded && s.pawn.flying), "seed {seed} tick {t}");
            let used = s.script.gun.times_grappled;
            let from_refire = e.gun.attached.is_some_and(|a| a.from_refire);
            assert!(
                (0..=3).contains(&used) || (used == 4 && from_refire),
                "seed {seed} tick {t}: {used}"
            );
            assert!(!e.kismet.overflowed(), "seed {seed} tick {t}");
            let leap_write = matches!(e.power_jump, Some(PowerJumpEvent::Fired { .. }));
            let boosting = s.script.boots.state == BootsStateName::Boosting;
            if s.is_grapple_attached() && !boosting && !leap_write && e.gun.attached.is_none() {
                let used_dt = dt.min(MAX_STEP_DT);
                let bound = 2000.0 * 1.001 + pull(used_dt, s.script.gun.distance.max(1.0)) + 1.0;
                assert!(
                    s.speed() <= bound,
                    "seed {seed} tick {t}: {} > {bound}",
                    s.speed()
                );
            }
            // Respawn when lost far away (keeps the run interesting).
            if s.position.length() > 50_000.0 {
                s.position = Vec3::new(0.0, 0.0, 1500.0);
                s.velocity = Vec3::ZERO;
            }
        }
        assert!(attaches >= 5, "seed {seed}: only {attaches} attaches");
        assert!(boosts > 0 || seed == 0xACE, "seed {seed}: no boost");
    }
}

#[test]
fn the_whole_script_state_round_trips_through_json_mid_grapple_and_boost() {
    // State completeness: serialising the state at any tick and continuing
    // from the copy gives bit-identical results (no hidden state outside
    // `PlayerState`; every timer, latch and latent sleep is serialised).
    let params = original();
    let crystal = Surface {
        actor: Some(1),
        class: ActorClass::RechargeCrystal { charged: true },
        tag: ActorTag::None,
    };
    let world = cube(
        wall_at(3500.0).with_ground(0.0, false),
        Vec3::new(800.0, 300.0, 600.0),
        40.0,
        crystal,
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 400.0), 0.0, 0.1, 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let mut rng = SplitMix64(0x5EED);
    let inputs: Vec<InputFrame> = (0..900)
        .map(|_| InputFrame {
            move_forward: rng.range(-1.0, 1.0),
            look_yaw_delta: rng.range(-0.04, 0.04),
            look_pitch_delta: rng.range(-0.03, 0.03),
            jump_pressed: rng.chance(0.03),
            jump_held: rng.chance(0.5),
            grapple_held: rng.chance(0.6),
            sprint_held: rng.chance(0.3),
            power_jump_held: rng.chance(0.15),
            ..InputFrame::default()
        })
        .collect();
    let mut checked = 0;
    for (t, input) in inputs.iter().enumerate() {
        if t % 37 == 0 {
            let json = serde_json::to_string(&s).unwrap();
            let mut copy: PlayerState = serde_json::from_str(&json).unwrap();
            assert_eq!(copy, s, "tick {t}");
            let mut original_run = s;
            for later in inputs.iter().skip(t).take(60) {
                let a = step_with(
                    &Ue3PawnMovement,
                    &mut original_run,
                    later,
                    &params,
                    &world,
                    DT,
                );
                let b = step_with(&Ue3PawnMovement, &mut copy, later, &params, &world, DT);
                assert_eq!(a, b, "tick {t}");
                assert_eq!(original_run, copy, "tick {t}");
            }
            checked += 1;
        }
        tick(&mut s, input, &params, &world);
    }
    assert!(checked > 20);
}

// ---------------------------------------------------------------------------
// The two halves of a tick (G-IN-5 / G-TM-2: input events before actors).
// ---------------------------------------------------------------------------

#[test]
fn g_tm_2_the_fire_uses_the_input_world_and_the_actors_use_the_updated_world() {
    // `begin_step` runs the input events against the world as the previous
    // tick left it; `finish_step` runs controller, physics and gun against
    // the world after the map actors ticked. Here a mover's face is at 1000
    // before and at 1100 after the map actors: the anchor is the old hit
    // point, the gun's first distance is measured to it from the new
    // physics, and `step_with` equals both halves with one world.
    let params = original();
    let old = wall_at(1000.0);
    let new = wall_at(1100.0);
    let s0 = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(1000.0),
        3,
    );
    let mut s = s0;
    let pending = asamu_player::begin_step(&mut s, &fire(), &params, &old, DT);
    assert_eq!(pending.dt(), Some(DT));
    let a = pending
        .events()
        .gun
        .attached
        .expect("attached in the input event");
    assert!((a.anchor.x - 1000.0).abs() < 0.01, "{}", a.anchor);
    assert!(s.pawn.flying, "physics switched synchronously (G-IN-5)");
    let e = asamu_player::finish_step(&Ue3PawnMovement, &mut s, pending, &params, &new);
    assert_eq!(e.gun.attached, Some(a), "the input events' events are kept");
    assert!((s.script.gun.distance - 1000.0).abs() < 0.01);
    // One world for both halves is `step_with`.
    let mut x = s0;
    let mut y = s0;
    let p = asamu_player::begin_step(&mut x, &fire(), &params, &old, DT);
    let ex = asamu_player::finish_step(&Ue3PawnMovement, &mut x, p, &params, &old);
    let ey = step_with(&Ue3PawnMovement, &mut y, &fire(), &params, &old, DT);
    assert_eq!((x, ex), (y, ey));
    // Invalid dt: nothing in either half; non-finite state: rejected.
    let mut z = s0;
    let p = asamu_player::begin_step(&mut z, &fire(), &params, &old, f32::NAN);
    assert_eq!(p.dt(), None);
    assert_eq!(
        asamu_player::finish_step(&Ue3PawnMovement, &mut z, p, &params, &old),
        StepEvents::default()
    );
    assert_eq!(z, s0);
    let mut bad = s0;
    bad.velocity.x = f32::INFINITY;
    let p = asamu_player::begin_step(&mut bad, &fire(), &params, &old, DT);
    assert_eq!(p.dt(), None);
    assert!(p.events().non_finite_rejected);
    assert!(p.events().gun.fire.is_none(), "no input event ran");
}

#[test]
fn g_rl_2_the_proximity_test_is_strict_at_exactly_200() {
    // `d < fGrappleReleaseDistance`: at exactly 200 uu the gun pulls and stays
    // attached; one float below, it pulls, halves and releases.
    let params = original();
    let world = wall_at(3000.0);
    for (d, releases) in [
        (200.0_f32, false),
        (f32::from_bits(200.0_f32.to_bits() - 1), true),
    ] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        tick(&mut s, &fire(), &params, &world);
        assert!(s.is_grapple_attached());
        // Hold the pawn still at distance `d` from the anchor (no motion in
        // this tick's flying physics: zero velocity, no gravity).
        s.velocity = Vec3::ZERO;
        s.script.gun.grapple_location = s.position + Vec3::new(d, 0.0, 0.0);
        let p = s.position;
        let e = tick(&mut s, &fire(), &params, &world);
        assert_eq!(s.position, p);
        assert_eq!(s.script.gun.distance, d);
        let pulled = pull(DT, d);
        if releases {
            assert_eq!(e.gun.released.unwrap().reason, ReleaseReason::Proximity);
            assert!((s.velocity.x - pulled * 0.5).abs() <= pulled * 1e-6);
        } else {
            assert!(e.gun.released.is_none(), "exactly 200: no release");
            assert!((s.velocity.x - pulled).abs() <= pulled * 1e-6);
        }
    }
}

#[test]
fn g_tg_1_aims_during_the_actor_ticks_use_the_camera_cache_of_the_previous_tick() {
    // The camera's `UpdateCamera` runs once per frame after every actor tick
    // group (native `UWorld::Tick`), and `GetAdjustedAim` returns its cached
    // point of view. So the boost aim (boots' state code) and a refire-served
    // fire (gun timers), both after the controller's look update, aim along
    // the view as the previous tick ended; a press in the input event sees
    // the same rotation because no look update has run yet.
    let params = original();
    // Boost aim: the boost-start tick also turns the view by 0.5 rad.
    let world = BoxWorld::new();
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 0.0), 0.3, -0.2, 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    tick(&mut s, &space, &params, &world);
    while s.script.boots.sleep.is_none_or(|r| r - DT >= 0.5 * DT)
        || s.script.boots.resume != rocket_boots::BoostLabel::BoostStart
    {
        tick(&mut s, &idle(), &params, &world);
    }
    let before = s.view_direction();
    let turn = InputFrame {
        look_yaw_delta: 0.5,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &turn, &params, &world);
    assert_eq!(e.boots, Some(BootsEvent::BoostBegan));
    assert_eq!(
        s.script.boots.aim, before,
        "the cached view, not the new one"
    );
    assert!((s.view_direction() - before).length() > 0.4);

    // Refire-served fire: press, release, re-press (served by the refire
    // check 0.1 s after the first press); the refire tick turns the view
    // away from the target cube, which the previous view still points at.
    let cube_world = cube(
        BoxWorld::new(),
        Vec3::new(2000.0, 0.0, 1038.0),
        80.0,
        Surface::default(),
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    tick(&mut s, &fire(), &params, &cube_world);
    tick(&mut s, &idle(), &params, &cube_world);
    let mut served = None;
    for _ in 0..10 {
        // Every tick starts aimed at the cube and turns 0.6 rad away from it
        // in the controller's look update (the yaw is set back between
        // ticks as a state set-up).
        s.yaw = 0.0;
        let turn_away = InputFrame {
            look_yaw_delta: 0.6,
            ..fire()
        };
        let e = tick(&mut s, &turn_away, &params, &cube_world);
        if let Some(a) = e.gun.attached {
            assert!(a.from_refire);
            served = Some((0.0_f32, a.anchor));
            break;
        }
        assert_eq!(e.gun.fire, None, "pending until the refire check");
    }
    let (start_yaw, anchor) = served.expect("the refire check served the press");
    assert_eq!(start_yaw, 0.0);
    assert!(
        (anchor.y).abs() < 1.0 && (anchor.x - 1920.0).abs() < 0.5,
        "hit the cube straight ahead: {anchor}"
    );
}

#[test]
fn g_ix_5_a_power_jump_charged_while_attached_fires_after_release_and_landing() {
    // G-IX-5: the charge can start while attached (it does not depend on
    // physics); holding the key through the grapple release and the landing,
    // a key-up while walking fires the vertical power jump (V.z = 1600).
    // A grapple during the power jump's rise then only drags the velocity
    // (below the 2000 cap) and adds the pull (G-AT-10).
    let params = original();
    // A floor, a ceiling block high above and just ahead of the start
    // (first grapple, steeply up) and a wall 1500 ahead (second grapple,
    // level).
    let world = BoxWorld::new()
        .with_ground(0.0, false)
        .with_box(
            Vec3::new(100.0, -200.0, 1500.0),
            Vec3::new(400.0, 200.0, 1600.0),
            true,
        )
        .with_box(
            Vec3::new(1500.0, -2000.0, 0.0),
            Vec3::new(1600.0, 2000.0, 4000.0),
            true,
        );
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, &params);
    grapple_gun::set_max_grapples(&mut s, 3);
    s.pitch = 1.4;
    let hold = |grapple: bool| InputFrame {
        grapple_held: grapple,
        power_jump_held: true,
        ..InputFrame::default()
    };
    // Grapple the ceiling block, then press the power-jump key while flying.
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(e.gun.attached.is_some() && s.pawn.flying, "{:?}", e.gun);
    tick(&mut s, &hold(true), &params, &world);
    assert_eq!(
        s.script.power_jump.state,
        pawn::PowerJumpStateName::Charging
    );
    assert!(s.is_grapple_attached(), "the charge started while attached");
    for _ in 0..40 {
        tick(&mut s, &hold(true), &params, &world);
    }
    assert!(s.script.power_jump.charged, "charged in the air");
    // Release the grapple (key still held) and fall back to the floor.
    let mut landed = false;
    for _ in 0..600 {
        let e = tick(&mut s, &hold(false), &params, &world);
        if e.landing.is_some() {
            landed = true;
        }
        if landed && s.grounded {
            break;
        }
    }
    assert!(landed && s.grounded);
    assert_eq!(
        s.script.power_jump.state,
        pawn::PowerJumpStateName::Charging,
        "landing does not cancel a charge"
    );
    // Key-up while walking: the vertical power jump.
    let e = tick(&mut s, &idle(), &params, &world);
    assert_eq!(
        e.power_jump,
        Some(PowerJumpEvent::Fired {
            leap: false,
            jumped: true
        })
    );
    assert_eq!(s.velocity.z, 1600.0);
    // Two ticks into the rise, grapple the wall level ahead: drag only
    // (below the cap), then the pull toward the anchor.
    s.pitch = 0.0;
    s.yaw = 0.0;
    tick(&mut s, &idle(), &params, &world);
    tick(&mut s, &idle(), &params, &world);
    let v = s.velocity;
    assert!(v.length() < 2000.0);
    let e = tick(&mut s, &fire(), &params, &world);
    let a = e.gun.attached.expect("attached during the rise");
    assert!(e.gun.released.is_none());
    let f = 1.0 - 0.15 * DT;
    let d = s.script.gun.distance;
    assert!(d > 300.0, "{d}");
    let expected = v * f * f + (a.anchor - s.position).normalize() * pull(DT, d);
    assert!(
        (s.velocity - expected).length() < 1.0,
        "{} vs {expected}",
        s.velocity
    );
    assert_eq!(s.script.code.state, PawnStateName::Shooting);
}
