//! Tests of the native-physics port (`Ue3PawnMovement`) against closed-form
//! consequences of `docs/reverse-engineering/NATIVE_PHYSICS.md`.
//!
//! Every expected value below is derived independently in the test (formulas
//! or a separate re-statement of the spec rule), never by calling the code
//! under test. Parameter values set here are test inputs, not game values.

mod common;

use asamu_player::movement::{LocomotionIntent, MovementModel, place_on_floor};
use asamu_player::trace::{TraceMeta, record_run_with};
use asamu_player::ue3_movement::{
    MAX_ITERATIONS, MIN_TICK_TIME, PawnPhysicsState, PhysicsMode, TickStats, Ue3PawnMovement,
};
use asamu_player::{
    BoxWorld, CollisionShape, CollisionWorld, HalfSpace, InputFrame, MovementModelKind,
    PlayerParams, PlayerState, SlopeWorld, StepEvents, step_with,
};
use common::SplitMix64;
use glam::Vec3;

const DT: f32 = 1.0 / 60.0;
const HOVER: f32 = 2.15;

fn no_input() -> LocomotionIntent {
    LocomotionIntent::default()
}

fn towards(dir: Vec3) -> LocomotionIntent {
    LocomotionIntent {
        wish_dir: dir,
        wish_scale: 1.0,
        jump: false,
    }
}

fn jump() -> LocomotionIntent {
    LocomotionIntent {
        jump: true,
        ..LocomotionIntent::default()
    }
}

fn tick<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    intent: LocomotionIntent,
    params: &PlayerParams,
    world: &W,
    dt: f32,
) -> (StepEvents, TickStats) {
    let mut events = StepEvents::default();
    let stats = Ue3PawnMovement.advance_with_stats(
        state,
        &intent,
        Vec3::ZERO,
        &params.movement,
        world,
        dt,
        &mut events,
    );
    (events, stats)
}

fn half_height(params: &PlayerParams) -> f32 {
    params.movement.capsule_half_height.value
}

fn trace_shape(params: &PlayerParams) -> CollisionShape {
    CollisionShape {
        radius: params.movement.capsule_radius.value,
        half_height: half_height(params),
    }
}

/// A walking state hovering `HOVER` above a flat floor at `floor_z`, based,
/// floor normal +Z.
fn standing_on(params: &PlayerParams, floor_z: f32, x: f32, y: f32) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(x, y, floor_z + half_height(params) + HOVER), 0.0);
    s.grounded = true;
    s.pawn = PawnPhysicsState {
        floor: Vec3::Z,
        based: true,
        force_floor_check: false,
    };
    s
}

/// Vertical gap between the cylinder bottom and the first surface below.
fn hover_gap<W: CollisionWorld + ?Sized>(world: &W, params: &PlayerParams, pos: Vec3) -> f32 {
    world
        .sweep_capsule(pos, pos - Vec3::Z * 1000.0, trace_shape(params))
        .map_or(f32::INFINITY, |h| h.distance)
}

/// Independent statement of the spec's sub-step rule (1.4) in f32:
/// the sequence of sub-step lengths for `dt` within the 8-step budget, and the
/// time left over.
fn expected_substeps(dt: f32) -> (Vec<f32>, f32) {
    let mut out = Vec::new();
    let mut remaining = dt;
    while remaining > 0.0 && out.len() < 8 {
        let step = if remaining > 0.05 {
            (remaining * 0.5).min(0.05)
        } else {
            remaining
        };
        out.push(step);
        remaining -= step;
    }
    (out, remaining.max(0.0))
}

/// Braking average of spec 2.3 evaluated in f64.
fn braking_reference(v0: f64, dt: f64, friction: f64) -> f64 {
    let mut v = v0;
    let mut avg = 0.0;
    let mut t = dt;
    while t > 0.0 {
        let h = t.min(0.03);
        t -= h;
        v *= 1.0 - 2.0 * h * friction;
        if v * v0 > 0.0 {
            avg += v * h / dt;
        }
    }
    if avg * v0 < 0.0 || avg * avg < 100.0 {
        0.0
    } else {
        avg
    }
}

fn assert_accounting(stats: &TickStats, dt: f32) {
    let total = stats.consumed_time() - stats.returned_time + stats.dropped_time;
    assert!(
        (total - dt).abs() <= 1e-5 * dt.max(1.0),
        "consumed {} - returned {} + dropped {} != dt {dt}",
        stats.consumed_time(),
        stats.returned_time,
        stats.dropped_time
    );
    assert!(stats.substeps().len() <= MAX_ITERATIONS as usize);
}

// ---------------------------------------------------------------------------
// Velocity update
// ---------------------------------------------------------------------------

#[test]
fn walking_braking_curve_matches_the_piecewise_formula() {
    let params = PlayerParams::default();
    let friction = params.movement.ground_friction.value;
    let world = BoxWorld::new().with_ground(0.0, false);
    for dt in [1.0 / 62.0, 1.0 / 30.0, 0.045] {
        let mut s = standing_on(&params, 0.0, 0.0, 0.0);
        s.velocity = Vec3::new(400.0, 0.0, 0.0);
        let mut ticks = 0;
        while s.velocity != Vec3::ZERO {
            let before = s;
            let (_, stats) = tick(&mut s, no_input(), &params, &world, dt);
            assert_accounting(&stats, dt);
            let expected = braking_reference(
                f64::from(before.velocity.x),
                f64::from(dt),
                f64::from(friction),
            );
            // The walking velocity is re-derived from the displacement.
            assert!(
                (f64::from(s.velocity.x) - expected).abs() < 2e-3,
                "dt {dt} tick {ticks}: {} vs {expected}",
                s.velocity.x
            );
            assert!((s.position.x - before.position.x - s.velocity.x * dt).abs() < 1e-4);
            assert_eq!(s.velocity.y, 0.0);
            assert_eq!(s.velocity.z, 0.0);
            assert!(s.velocity.x >= 0.0, "never reverses");
            ticks += 1;
            assert!(ticks < 200, "braking terminates");
        }
        assert!(ticks > 3, "dt {dt}: braking took {ticks} ticks");
        assert!(s.grounded);
    }
}

#[test]
fn ground_acceleration_reaches_max_speed_at_the_closed_form_tick() {
    let mut params = PlayerParams::default();
    // Test inputs: AccelRate·dt = 50 uu/s per tick, cap 475 → 9.5 ticks.
    params.movement.ground_acceleration.value = 3000.0;
    params.movement.max_ground_speed.value = 475.0;
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    let dir = Vec3::new(0.6, 0.8, 0.0);
    let mut reached = None;
    for n in 1..=20 {
        tick(&mut s, towards(dir), &params, &world, DT);
        let expected = (50.0 * n as f32).min(475.0);
        let speed = s.velocity.length();
        assert!(
            (speed - expected).abs() < 0.02,
            "tick {n}: {speed} vs {expected}"
        );
        // Aligned acceleration: no turning friction, direction preserved.
        assert!((s.velocity.normalize() - dir).length() < 1e-4);
        if reached.is_none() && speed > 474.9 {
            reached = Some(n);
        }
    }
    assert_eq!(reached, Some(10));
}

#[test]
fn analog_input_magnitude_is_discarded() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let run = |forward: f32| {
        let mut s = standing_on(&params, 0.0, 0.0, 0.0);
        let input = InputFrame {
            move_forward: forward,
            ..InputFrame::default()
        };
        for _ in 0..30 {
            step_with(
                &MovementModelKind::Ue3Pawn,
                &mut s,
                &input,
                &params,
                &world,
                DT,
            );
        }
        s
    };
    let full = run(1.0);
    let light = run(0.3);
    assert_eq!(full.position, light.position);
    assert_eq!(full.velocity, light.velocity);
    assert!((full.velocity.x - params.movement.max_ground_speed.value).abs() < 0.02);
}

// ---------------------------------------------------------------------------
// Falling and jumping
// ---------------------------------------------------------------------------

#[test]
fn jump_follows_the_doubled_gravity_closed_form() {
    let mut params = PlayerParams::default();
    params.movement.jump_velocity.value = 420.0;
    let jz = 420.0_f64;
    let g = f64::from(params.movement.world_gravity_z.value); // −520 (config)
    assert_eq!(g, -520.0);
    let h = f64::from(DT);
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    let z0 = f64::from(s.position.z);

    // Refined scheme: sub-step k moves by (V0 + (2k−1)·g·h)·h and ends with
    // V0 + 2·g·k·h, so z_n = J·n·h + g·h²·n², v_n = J + 2·g·n·h.
    let mut heights = Vec::new();
    let mut landed_at = None;
    for n in 1..=60 {
        let (events, _) = tick(
            &mut s,
            if n == 1 { jump() } else { no_input() },
            &params,
            &world,
            DT,
        );
        assert_eq!(events.jumped, n == 1);
        if let Some(impact) = events.landed {
            landed_at = Some((n, impact));
            break;
        }
        let nf = f64::from(n);
        let z = jz * nf * h + g * h * h * nf * nf;
        let v = jz + 2.0 * g * nf * h;
        assert!(
            (f64::from(s.position.z) - z0 - z).abs() < 0.02,
            "tick {n}: z {} vs {}",
            f64::from(s.position.z) - z0,
            z
        );
        assert!(
            (f64::from(s.velocity.z) - v).abs() < 0.05,
            "tick {n}: vz {} vs {v}",
            s.velocity.z
        );
        assert!(!s.grounded);
        heights.push(f64::from(s.position.z) - z0);
    }
    // Discrete apex: the integer nearest J/(2·|g|·h) = 24.23.
    let apex_tick = heights
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i + 1);
    assert_eq!(apex_tick, Some(24));
    let apex = heights[23];
    let continuous = jz * jz / (4.0 * -g); // 84.807…
    assert!((apex - 84.8).abs() < 0.02, "apex {apex}");
    assert!((apex - continuous).abs() <= -g * h * h / 4.0 + 0.02);
    // Single gravity would give J²/(2|g|) ≈ 169.6: clearly not the case.
    assert!(apex < 0.6 * jz * jz / (2.0 * -g));

    // Lands in tick 49 (z_n reaches −HOVER between n = 48 and 49); the
    // landing velocity is the semi-implicit velocity of that sub-step,
    // J + (2n − 1)·g·h.
    let (n, impact) = landed_at.expect("landed");
    assert_eq!(n, 49);
    let expected_impact = jz + (2.0 * 49.0 - 1.0) * g * h;
    assert!(
        (f64::from(impact) - expected_impact).abs() < 0.05,
        "{impact} vs {expected_impact}"
    );
    assert!(s.grounded);
    assert_eq!(
        s.velocity,
        Vec3::ZERO,
        "walking zeroes Z; no horizontal motion"
    );
    // The carried time walked and re-established the hover.
    assert!((f64::from(s.position.z) - z0).abs() < 1e-3);
}

#[test]
fn free_fall_long_tick_uses_eight_substeps_and_drops_the_rest() {
    let params = PlayerParams::default();
    let world = BoxWorld::new();
    let g = params.movement.world_gravity_z.value;
    for dt in [0.5_f32, 0.3, 0.12, 0.07, 1.0] {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
        let (_, stats) = tick(&mut s, no_input(), &params, &world, dt);
        let (steps, left) = expected_substeps(dt);
        let got: Vec<f32> = stats.substeps().iter().map(|s| s.time).collect();
        assert_eq!(got, steps, "dt {dt}");
        assert!(
            stats
                .substeps()
                .iter()
                .all(|s| s.mode == PhysicsMode::Falling)
        );
        assert!((stats.dropped_time - left).abs() < 1e-7, "dt {dt}");
        assert_eq!(stats.returned_time, 0.0);
        assert_accounting(&stats, dt);
        // Closed form of the refined scheme from rest: v = 2·g·Σh and
        // z = z0 + Σ_k (2·g·T_{k−1} + g·h_k)·h_k with T the elapsed time.
        let mut elapsed = 0.0_f64;
        let mut z = 1000.0_f64;
        for &h in &steps {
            let h = f64::from(h);
            z += (2.0 * f64::from(g) * elapsed + f64::from(g) * h) * h;
            elapsed += h;
        }
        let v = 2.0 * f64::from(g) * elapsed;
        assert!(
            (f64::from(s.velocity.z) - v).abs() < 0.05,
            "dt {dt}: {}",
            s.velocity.z
        );
        assert!(
            (f64::from(s.position.z) - z).abs() < 5e-3,
            "dt {dt}: {}",
            s.position.z
        );
    }
    // dt = 0.5: eight 0.05 s steps, 0.1 s dropped, Δz = g·h²·n² = −83.2.
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    let (_, stats) = tick(&mut s, no_input(), &params, &world, 0.5);
    assert_eq!(stats.substeps().len(), 8);
    assert!((stats.dropped_time - 0.1).abs() < 1e-6);
    assert!((s.position.z - (1000.0 - 83.2)).abs() < 5e-3);
    assert!((s.velocity.z + 416.0).abs() < 0.05);
}

#[test]
fn long_walking_tick_drops_time_and_under_reports_velocity() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let speed = params.movement.max_ground_speed.value;
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    let (_, stats) = tick(&mut s, towards(Vec3::X), &params, &world, 0.5);
    assert_eq!(stats.count(PhysicsMode::Walking), 8);
    assert!((stats.dropped_time - 0.1).abs() < 1e-6);
    assert_accounting(&stats, 0.5);
    // CalcVelocity ran once with the whole 0.5 s (capped at GroundSpeed), the
    // pawn moved 8 × 0.05 s, and V = displacement / 0.5 = 0.8 · GroundSpeed.
    assert!(
        (s.position.x - 0.4 * speed).abs() < 1e-3,
        "{}",
        s.position.x
    );
    assert!(
        (s.velocity.x - 0.8 * speed).abs() < 1e-2,
        "{}",
        s.velocity.x
    );
    assert!(s.grounded);
}

#[test]
fn slices_below_the_minimum_are_ignored() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    s.velocity = Vec3::new(300.0, 0.0, 0.0);
    let before = s;
    let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, 0.00029);
    assert_eq!(s, before, "a 0.29 ms tick does nothing at all");
    assert!(stats.substeps().is_empty());
    assert!((stats.dropped_time - 0.00029).abs() < 1e-9);
    let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, MIN_TICK_TIME);
    assert_eq!(stats.substeps().len(), 1);
    assert_ne!(s.velocity, before.velocity);
    // Non-finite / non-positive dt: no-op.
    for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let before = s;
        let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, dt);
        assert_eq!(s, before);
        assert_eq!(stats, TickStats::default());
    }
}

#[test]
fn terminal_velocity_clamps_the_3d_speed() {
    let mut params = PlayerParams::default();
    params.movement.terminal_velocity.value = 500.0;
    let world = BoxWorld::new();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 5000.0), 0.0);
    s.velocity = Vec3::new(300.0, 0.0, 0.0);
    for _ in 0..120 {
        tick(&mut s, no_input(), &params, &world, DT);
        assert!(s.velocity.length() <= 500.0 + 1e-3, "{}", s.velocity);
    }
    assert!((s.velocity.length() - 500.0).abs() < 1e-3);
}

// ---------------------------------------------------------------------------
// Air control
// ---------------------------------------------------------------------------

fn airborne(velocity: Vec3) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    s.velocity = velocity;
    s
}

#[test]
fn air_control_limiter_on_and_off() {
    let mut params = PlayerParams::default();
    params.movement.ground_acceleration.value = 3000.0;
    params.movement.air_control.value = 0.25;
    params.movement.max_ground_speed.value = 450.0;
    let world = BoxWorld::new();
    let a = 3000.0_f32;
    let ac = 0.25_f32;

    // Limit on, 10 ≤ |v_xy| < GroundSpeed: |A| clamped to AccelRate·AirControl;
    // the refinement doubles it: Δv = 2·750·dt.
    params.movement.limit_fall_accel.value = true;
    let mut s = airborne(Vec3::new(200.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::Y), &params, &world, DT);
    assert!(
        (s.velocity.y - 2.0 * a * ac * DT).abs() < 1e-2,
        "{}",
        s.velocity
    );
    assert!((s.velocity.x - 200.0).abs() < 1e-2);

    // Low-speed boost: from rest the limit is A·AC + (10 − 0)/dt.
    let mut s = airborne(Vec3::ZERO);
    tick(&mut s, towards(Vec3::Y), &params, &world, DT);
    let boosted = a * ac + 10.0 / DT;
    assert!(
        (s.velocity.y - 2.0 * boosted * DT).abs() < 1e-2,
        "{}",
        s.velocity
    );

    // Above GroundSpeed with tiny air control: the limit is 1 uu/s².
    let mut tiny = params.clone();
    tiny.movement.air_control.value = 0.04;
    let mut s = airborne(Vec3::new(600.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::Y), &tiny, &world, DT);
    assert!((s.velocity.y - 2.0 * DT).abs() < 1e-3, "{}", s.velocity);

    // Limit off: the script acceleration (AccelRate · direction) is used as is.
    params.movement.limit_fall_accel.value = false;
    let mut s = airborne(Vec3::new(200.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::Y), &params, &world, DT);
    assert!((s.velocity.y - 2.0 * a * DT).abs() < 1e-2, "{}", s.velocity);
}

#[test]
fn bound_speed_keeps_the_pre_refinement_speed_but_lets_it_creep() {
    let mut params = PlayerParams::default();
    params.movement.ground_acceleration.value = 3000.0;
    params.movement.air_control.value = 0.25;
    params.movement.max_ground_speed.value = 450.0;
    params.movement.limit_fall_accel.value = true;
    let world = BoxWorld::new();
    let s0 = 600.0_f64;
    let mut s = airborne(Vec3::new(600.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::Y), &params, &world, DT);
    // V1 = bound(V0 + A·h) with |A| = 750, then V = 2·V1 − V0.
    let h = f64::from(DT);
    let (x1, y1) = (s0, 750.0 * h);
    let len = (x1 * x1 + y1 * y1).sqrt();
    let (bx, by) = (x1 * s0 / len, y1 * s0 / len);
    let (vx, vy) = (2.0 * bx - s0, 2.0 * by);
    assert!(
        (f64::from(s.velocity.x) - vx).abs() < 2e-2,
        "{} vs {vx}",
        s.velocity.x
    );
    assert!(
        (f64::from(s.velocity.y) - vy).abs() < 2e-2,
        "{} vs {vy}",
        s.velocity.y
    );
    let speed = f64::from(s.velocity.truncate().length());
    assert!(
        speed > s0,
        "the refinement lets the bounded speed creep: {speed}"
    );
}

#[test]
fn air_control_wall_probe_disables_steering_into_walls() {
    let mut params = PlayerParams::default();
    params.movement.limit_fall_accel.value = true;
    let r = params.movement.capsule_radius.value;
    // A wall 5 uu beyond the cylinder in +Y.
    let world = BoxWorld::new().with_box(
        Vec3::new(-1000.0, r + 5.0, -2000.0),
        Vec3::new(1000.0, r + 50.0, 3000.0),
        false,
    );
    let mut s = airborne(Vec3::new(200.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::Y), &params, &world, DT);
    assert_eq!(s.velocity.y, 0.0, "TickAirControl = 0 → no acceleration");
    // Steering away from the wall is unaffected.
    let mut s = airborne(Vec3::new(200.0, 0.0, 0.0));
    tick(&mut s, towards(Vec3::NEG_Y), &params, &world, DT);
    let expected =
        -2.0 * params.movement.ground_acceleration.value * params.movement.air_control.value * DT;
    assert!((s.velocity.y - expected).abs() < 1e-2, "{}", s.velocity);
}

// ---------------------------------------------------------------------------
// Walking: floor, hover, steps, slopes
// ---------------------------------------------------------------------------

#[test]
fn hover_band_is_corrected_to_2_15() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let h = half_height(&params);
    let probe = params.movement.step_height.value + 2.0;
    for gap in [
        0.05_f32, 0.5, 1.0, 1.85, 1.95, 2.15, 2.3, 2.39, 2.45, 5.0, 19.5,
    ] {
        for (based, force) in [(true, true), (false, false), (true, false)] {
            let mut s = standing_on(&params, 0.0, 0.0, 0.0);
            s.position.z = h + gap;
            s.pawn.based = based;
            s.pawn.force_floor_check = force;
            tick(&mut s, no_input(), &params, &world, DT);
            let got = s.position.z - h;
            let expected = if based && !force {
                gap // zero move on a static base: floor trace skipped
            } else if !based || !(1.9..=2.4).contains(&gap) {
                HOVER // new base → snap; else push up / snap down
            } else {
                gap // inside the band on the current base
            };
            assert!(
                (got - expected).abs() < 1e-4,
                "gap {gap} based {based} force {force}: {got} vs {expected}"
            );
            assert!(s.grounded);
            assert!(s.pawn.based && !s.pawn.force_floor_check);
            assert_eq!(s.velocity, Vec3::ZERO);
        }
    }
    // Beyond the probe (MaxStepHeight + 2): no floor → falling.
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    s.position.z = h + probe + 0.5;
    s.pawn.force_floor_check = true;
    let (events, _) = tick(&mut s, no_input(), &params, &world, DT);
    assert!(!s.grounded && events.left_ground && !s.pawn.based);
}

#[test]
fn steps_up_onto_a_box_of_max_step_height_but_not_much_higher() {
    let params = PlayerParams::default();
    let step = params.movement.step_height.value;
    let h = half_height(&params);
    let r = params.movement.capsule_radius.value;
    // Stepping raises the pawn by MaxStepHeight + 2 from its 2.15 hover, so
    // ledges up to MaxStepHeight + 4.15 (less the pull-back) are climbable.
    for (height, climbs) in [
        (step * 0.5, true),
        (step, true),
        (step + 4.0, true),
        (step + 4.3, false),
        (step + 20.0, false),
    ] {
        let world = BoxWorld::new().with_ground(0.0, false).with_box(
            Vec3::new(100.0, -500.0, 0.0),
            Vec3::new(3000.0, 500.0, height),
            false,
        );
        let mut s = standing_on(&params, 0.0, 0.0, 0.0);
        for _ in 0..60 {
            let (events, _) = tick(&mut s, towards(Vec3::X), &params, &world, DT);
            assert!(s.grounded, "height {height}: never leaves the ground");
            assert!(!events.left_ground);
        }
        if climbs {
            assert!(s.position.x > 200.0, "height {height}: {}", s.position);
            assert!(
                (s.position.z - (height + h + HOVER)).abs() < 1e-3,
                "height {height}: {}",
                s.position
            );
            assert!((s.velocity.x - params.movement.max_ground_speed.value).abs() < 0.05);
        } else {
            assert!(s.position.x < 100.0 - r, "height {height}: {}", s.position);
            assert!((s.position.z - (h + HOVER)).abs() < 1e-3, "{}", s.position);
            assert!(
                s.velocity.x.abs() < 5.0,
                "pressed against the wall: {}",
                s.velocity
            );
        }
    }
}

#[test]
fn step_up_repeats_to_climb_several_stairs_in_one_substep() {
    let mut params = PlayerParams::default();
    // Fast runner (test inputs): 2000 uu/s → 33.3 uu per 1/60 s sub-step.
    params.movement.max_ground_speed.value = 2000.0;
    params.movement.ground_acceleration.value = 1.0e6;
    let step_h = params.movement.step_height.value;
    let h = half_height(&params);
    // Stairs: treads 10 uu deep, risers 12 uu high (≤ MaxStepHeight).
    let (tread, riser, count) = (10.0_f32, 12.0_f32, 20);
    assert!(riser <= step_h);
    let mut world = BoxWorld::new().with_ground(0.0, false);
    for k in 0..count {
        let x0 = 100.0 + tread * k as f32;
        world = world.with_box(
            Vec3::new(x0, -300.0, 0.0),
            Vec3::new(x0 + tread, 300.0, riser * (k + 1) as f32),
            false,
        );
    }
    let top = riser * count as f32;
    world = world.with_box(
        Vec3::new(100.0 + tread * count as f32, -300.0, 0.0),
        Vec3::new(5000.0, 300.0, top),
        false,
    );
    let mut s = standing_on(&params, 0.0, 40.0, 0.0);
    let mut ticks_to_top = None;
    for n in 1..=40 {
        tick(&mut s, towards(Vec3::X), &params, &world, DT);
        assert!(s.grounded, "tick {n}: {}", s.position);
        if ticks_to_top.is_none() && (s.position.z - (top + h + HOVER)).abs() < 1e-3 {
            ticks_to_top = Some(n);
        }
    }
    // |Delta|²·Time > 144 lets one sub-step climb several risers, so the
    // 200 uu of stairs take about 200/33 ≈ 6–7 ticks, not one tick per riser.
    let n = ticks_to_top.expect("reached the top");
    assert!(n < count / 2, "took {n} ticks for {count} risers");
}

#[test]
fn walkable_ramp_keeps_full_horizontal_speed_and_the_hover_band() {
    let params = PlayerParams::default();
    let speed = params.movement.max_ground_speed.value;
    // Normal z = 0.8 (36.87°): walkable for WalkableFloorZ 0.7; no slope
    // slide since 0.8 · friction ≥ 3.3 for the default friction.
    assert!(0.8 * params.movement.ground_friction.value >= 3.3);
    let world = SlopeWorld::new(BoxWorld::new().with_ground(0.0, false))
        .with_half_space(HalfSpace::ramp_x(0.8, Vec3::new(100.0, 0.0, 0.0), false).unwrap());
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    s.velocity = Vec3::new(speed, 0.0, 0.0);
    let mut climbed = 0;
    for _ in 0..90 {
        tick(&mut s, towards(Vec3::X), &params, &world, DT);
        assert!(s.grounded, "{}", s.position);
        assert!(
            (s.velocity.truncate().length() - speed).abs() < 0.05,
            "{}",
            s.velocity
        );
        assert_eq!(s.velocity.z, 0.0);
        let gap = hover_gap(&world, &params, s.position);
        assert!((1.9..=2.4).contains(&gap), "hover {gap} at {}", s.position);
        if s.position.x > 140.0 {
            climbed += 1;
            assert!((s.pawn.floor.z - 0.8).abs() < 1e-6);
        }
    }
    assert!(climbed > 30 && s.position.z > 200.0, "{}", s.position);
    // And back down: still grounded at full speed.
    for _ in 0..30 {
        tick(&mut s, towards(Vec3::NEG_X), &params, &world, DT);
        assert!(s.grounded, "{}", s.position);
    }
    assert!((s.velocity.x + speed).abs() < 0.05, "{}", s.velocity);
}

/// A pawn resting on an infinite ramp with normal z `nz`, settled.
fn settled_on_ramp(params: &PlayerParams, nz: f32) -> (SlopeWorld, PlayerState) {
    let world = SlopeWorld::new(BoxWorld::new())
        .with_half_space(HalfSpace::ramp_x(nz, Vec3::ZERO, false).expect("valid ramp"));
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 200.0), 0.0);
    for _ in 0..240 {
        tick(&mut s, no_input(), params, &world, DT);
        if s.grounded && s.velocity == Vec3::ZERO {
            break;
        }
    }
    assert!(s.grounded, "landed on the ramp nz {nz}");
    (world, s)
}

#[test]
fn slope_slide_triggers_only_below_its_thresholds() {
    // Floor normal z = 0.8: threshold friction 3.3 / 0.8 = 4.125.
    for (nz, friction, slides) in [
        (0.8_f32, 4.0_f32, true),
        (0.8, 4.2, false),
        (0.985, 0.1, true),
        (0.995, 0.1, false),
        (0.8, 0.2, true),
    ] {
        let mut params = PlayerParams::default();
        params.movement.ground_friction.value = friction;
        let (world, mut s) = settled_on_ramp(&params, nz);
        for _ in 0..3 {
            let before = s;
            tick(&mut s, no_input(), &params, &world, DT);
            let moved = s.position - before.position;
            if slides {
                // g' = g·dt/(2·max(F, 0.5))·dt; slide = g'·(Z − n·n_z).
                let g = f64::from(params.movement.world_gravity_z.value);
                let dt = f64::from(DT);
                let gp = g * dt / (2.0 * f64::from(friction).max(0.5)) * dt;
                let n = s.pawn.floor;
                let (nx, nzz) = (f64::from(n.x), f64::from(n.z));
                let ex = -gp * nx * nzz;
                let ez = gp * (1.0 - nzz * nzz);
                assert!(
                    (f64::from(moved.x) - ex).abs() < 1e-4 + 1e-3 * ex.abs(),
                    "nz {nz} F {friction}: dx {} vs {ex}",
                    moved.x
                );
                assert!(
                    (f64::from(moved.z) - ez).abs() < 1e-4 + 1e-3 * ez.abs(),
                    "nz {nz} F {friction}: dz {} vs {ez}",
                    moved.z
                );
                assert!(moved.x < 0.0 && moved.z < 0.0, "downhill");
                // Walking velocity = displacement/dt with Z = 0.
                assert!((s.velocity.x - moved.x / DT).abs() < 1e-2);
                assert_eq!(s.velocity.z, 0.0);
            } else {
                assert_eq!(moved, Vec3::ZERO, "nz {nz} F {friction}");
            }
            assert!(s.grounded);
        }
        let gap = hover_gap(&world, &params, s.position);
        assert!((1.9..=2.4).contains(&gap), "nz {nz}: hover {gap}");
    }
}

#[test]
fn landing_requires_normal_z_at_least_walkable_floor_z() {
    let params = PlayerParams::default();
    let walkable = params.movement.walkable_floor_z.value;
    for (nz, lands) in [
        (walkable, true),
        (walkable + 0.01, true),
        (walkable - 0.01, false),
        (0.3, false),
    ] {
        let world = SlopeWorld::new(BoxWorld::new())
            .with_half_space(HalfSpace::ramp_x(nz, Vec3::ZERO, false).unwrap());
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 150.0), 0.0);
        let mut landed = None;
        for n in 0..180 {
            let (events, stats) = tick(&mut s, no_input(), &params, &world, DT);
            assert!(s.is_finite());
            if events.landed.is_some() {
                landed = Some((n, stats));
                break;
            }
        }
        assert_eq!(landed.is_some(), lands, "nz {nz}: {s:?}");
        if let Some((_, stats)) = landed {
            // A direct landing on the first hit gives back step·(1 − Hit.Time)
            // (a landing found only after a slide would carry nothing).
            assert!(stats.returned_time > 0.0, "nz {nz}: {stats:?}");
            assert!(s.grounded);
            assert_eq!(s.pawn.floor.z, nz, "floor = landing normal");
            for _ in 0..10 {
                tick(&mut s, no_input(), &params, &world, DT);
                assert!(s.grounded, "stays on a walkable ramp");
            }
        } else {
            assert!(!s.grounded);
            assert!(s.position.x < -10.0, "slid downhill: {}", s.position);
        }
    }
}

#[test]
fn v_crease_of_unwalkable_slopes_lands_by_the_ditch_rule() {
    let params = PlayerParams::default();
    let nz = 0.5_f32;
    assert!(nz < params.movement.walkable_floor_z.value);
    let s60 = (1.0 - nz * nz).sqrt();
    let world = SlopeWorld::new(BoxWorld::new())
        .with_half_space(
            HalfSpace::through_point(Vec3::new(-s60, 0.0, nz), Vec3::ZERO, false).unwrap(),
        )
        .with_half_space(
            HalfSpace::through_point(Vec3::new(s60, 0.0, nz), Vec3::ZERO, false).unwrap(),
        );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 200.0), 0.0);
    let mut landing = None;
    for _ in 0..120 {
        let (events, stats) = tick(&mut s, no_input(), &params, &world, DT);
        if events.landed.is_some() {
            landing = Some(stats);
            break;
        }
    }
    let stats = landing.expect("wedged in the crease counts as a landing");
    assert!(s.grounded);
    assert!(s.pawn.floor.z < params.movement.walkable_floor_z.value);
    // A landing after a slide carries zero time.
    assert_eq!(stats.count(PhysicsMode::Walking), 0);
    assert_eq!(stats.returned_time, 0.0);
    assert_accounting(&stats, DT);

    // Walking into one side of the crease: the floor is too steep and the
    // move pushes into it, the steep-slope slide hits the other unwalkable
    // side, and the step is reverted (spec 3.3/3.4; TENTATIVE reading of the
    // "wall also hit" flag): back to the start, velocity and acceleration 0.
    let before = s;
    let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, DT);
    assert_eq!(s.position, before.position);
    assert_eq!(s.velocity, Vec3::ZERO);
    assert!(s.grounded && s.pawn.based && events.landed.is_none() && !events.left_ground);
    assert_eq!(stats.count(PhysicsMode::Walking), 1);
    assert_accounting(&stats, DT);
}

// ---------------------------------------------------------------------------
// Mode transitions and time carry-over
// ---------------------------------------------------------------------------

#[test]
fn walking_off_a_ledge_carries_the_rest_of_the_tick_into_falling() {
    let params = PlayerParams::default();
    let speed = params.movement.max_ground_speed.value;
    let g = params.movement.world_gravity_z.value;
    // Platform top at z = 0 ending at x = 100; nothing below.
    let world = BoxWorld::new().with_box(
        Vec3::new(-1000.0, -500.0, -100.0),
        Vec3::new(100.0, 500.0, 0.0),
        false,
    );
    let mut s = standing_on(&params, 0.0, 110.0, 0.0);
    s.velocity = Vec3::new(speed, 0.0, 0.0);
    let z0 = s.position.z;
    let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, 0.1);
    let modes: Vec<_> = stats.substeps().iter().map(|s| (s.mode, s.time)).collect();
    assert_eq!(
        modes,
        [(PhysicsMode::Walking, 0.05), (PhysicsMode::Falling, 0.05)]
    );
    assert!(stats.returned_time.abs() < 1e-7, "free move: frac = 1");
    assert_eq!(stats.mode_changes, 1);
    assert_accounting(&stats, 0.1);
    assert!(events.left_ground && !s.grounded && !s.pawn.based);
    // Falling sub-step from Vz = 0 with the air bound at GroundSpeed:
    // V = (GroundSpeed, 0, 2·g·h), Δz = g·h².
    assert!(
        (s.position.x - (110.0 + 2.0 * 0.05 * speed)).abs() < 1e-3,
        "{}",
        s.position
    );
    assert!(
        (s.position.z - (z0 + g * 0.0025)).abs() < 1e-3,
        "{}",
        s.position
    );
    assert!(
        (s.velocity - Vec3::new(speed, 0.0, 2.0 * g * 0.05)).length() < 1e-2,
        "{}",
        s.velocity
    );
}

#[test]
fn landing_carries_the_unused_substep_time_into_walking() {
    let params = PlayerParams::default();
    let g = params.movement.world_gravity_z.value;
    let h = half_height(&params);
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, h + 10.0), 0.0);
    s.velocity = Vec3::new(0.0, 0.0, -300.0);
    let dt = 0.2;
    let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, dt);
    // Sub-step 1 (0.05 s): V1 = −300 + g·0.05 = −326, move −16.3 uu, contact
    // after 10 uu, pulled back 0.05 uu along the move.
    let v1 = -300.0 + g * 0.05;
    let travel = -v1 * 0.05;
    let t = (10.0 - 0.05) / travel;
    let returned = 0.05 * (1.0 - t);
    let landed = events.landed.expect("landed");
    assert!((landed - v1).abs() < 0.05, "impact {landed} vs {v1}");
    assert!(
        (stats.returned_time - returned).abs() < 1e-5,
        "{}",
        stats.returned_time
    );
    let carry = 0.15 + returned;
    let (walk, left) = expected_substeps(carry);
    let mut expected = vec![(PhysicsMode::Falling, 0.05_f32)];
    // The walking budget left after one falling sub-step is 7.
    expected.extend(walk.iter().take(7).map(|&w| (PhysicsMode::Walking, w)));
    let got: Vec<_> = stats.substeps().iter().map(|s| (s.mode, s.time)).collect();
    assert_eq!(got.len(), expected.len(), "{got:?}");
    for ((gm, gt), (em, et)) in got.iter().zip(&expected) {
        assert_eq!(gm, em);
        assert!((gt - et).abs() < 1e-6, "{got:?} vs {expected:?}");
    }
    assert!(left < 1e-6);
    assert_accounting(&stats, dt);
    assert!(s.grounded && s.pawn.based);
    assert!((s.position.z - (h + HOVER)).abs() < 1e-3, "{}", s.position);
    // Walked for the carried time only (velocity from that displacement).
    assert!(s.position.x > 0.0 && s.velocity.x > 0.0);
}

#[test]
fn jump_flags_and_left_ground_events() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = standing_on(&params, 0.0, 0.0, 0.0);
    let (events, stats) = tick(&mut s, jump(), &params, &world, DT);
    assert!(events.jumped && !events.left_ground && events.landed.is_none());
    assert_eq!(stats.mode_changes, 1);
    assert!(!s.grounded && !s.pawn.based);
    // A jump request while falling does nothing.
    let mut again = s;
    let (events, _) = tick(&mut again, jump(), &params, &world, DT);
    assert!(!events.jumped);
}

// ---------------------------------------------------------------------------
// Determinism and robustness
// ---------------------------------------------------------------------------

fn obstacle_world() -> SlopeWorld {
    SlopeWorld::new(
        BoxWorld::new()
            .with_ground(0.0, false)
            .with_box(
                Vec3::new(300.0, -200.0, 0.0),
                Vec3::new(500.0, 200.0, 15.0),
                false,
            )
            .with_box(
                Vec3::new(500.0, -200.0, 0.0),
                Vec3::new(700.0, 200.0, 30.0),
                false,
            )
            .with_box(
                Vec3::new(700.0, 150.0, 0.0),
                Vec3::new(900.0, 400.0, 300.0),
                true,
            )
            .with_box(
                Vec3::new(-400.0, -400.0, 150.0),
                Vec3::new(-200.0, 400.0, 160.0),
                true,
            ),
    )
    .with_half_space(
        HalfSpace::ramp_x(0.75, Vec3::new(1100.0, 0.0, 0.0), false).expect("valid ramp"),
    )
    .with_half_space(
        HalfSpace::through_point(Vec3::new(0.0, -0.6, 0.8), Vec3::new(0.0, 900.0, 0.0), false)
            .expect("valid plane"),
    )
}

fn scripted_inputs() -> Vec<InputFrame> {
    let mut rng = SplitMix64(0x5EED_0F0C);
    (0..900)
        .map(|i| InputFrame {
            move_forward: if i % 300 < 220 {
                1.0
            } else {
                rng.range(-1.0, 1.0)
            },
            move_right: if i % 90 < 30 {
                rng.range(-1.0, 1.0)
            } else {
                0.0
            },
            look_yaw_delta: if i % 50 == 0 {
                rng.range(-0.8, 0.8)
            } else {
                0.0
            },
            look_pitch_delta: if i % 70 == 0 {
                rng.range(-0.3, 0.3)
            } else {
                0.0
            },
            jump_pressed: i % 37 == 0,
            jump_held: false,
            grapple_held: (200..260).contains(&(i % 400)),
        })
        .collect()
}

#[test]
fn ue3_runs_are_bit_reproducible() {
    let params = PlayerParams::default();
    let world = obstacle_world();
    let inputs = scripted_inputs();
    let start = {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
        place_on_floor(&mut s, &params.movement, &world.boxes, 200.0);
        s
    };
    let record = || {
        record_run_with(
            &MovementModelKind::Ue3Pawn,
            TraceMeta::runtime(Some("ue3 determinism".into()), Some(60.0)),
            &start,
            &inputs,
            &params,
            &world,
            DT,
        )
        .to_jsonl_string()
        .unwrap()
    };
    let a = record();
    let b = record();
    assert_eq!(a, b);
    // Interleaving another simulation does not change anything; the model
    // and the kind dispatch agree bit for bit.
    let mut x = start;
    let mut y = start;
    let mut other = PlayerState::new(Vec3::new(50.0, 50.0, 400.0), 1.0);
    for input in &inputs {
        step_with(&Ue3PawnMovement, &mut x, input, &params, &world, DT);
        step_with(
            &MovementModelKind::Placeholder,
            &mut other,
            input,
            &params,
            &world,
            DT,
        );
        step_with(
            &MovementModelKind::Ue3Pawn,
            &mut y,
            input,
            &params,
            &world,
            DT,
        );
        assert_eq!(x, y);
        assert_eq!(
            x.position.to_array().map(f32::to_bits),
            y.position.to_array().map(f32::to_bits)
        );
    }
    // The scenario actually exercised both modes.
    assert!(a.contains("\"grounded\":true") && a.contains("\"grounded\":false"));
}

fn random_world(rng: &mut SplitMix64) -> SlopeWorld {
    let mut boxes = BoxWorld::new();
    if rng.chance(0.8) {
        boxes = boxes.with_ground(rng.range(-50.0, 50.0), false);
    }
    for _ in 0..(rng.next_u64() % 12) {
        let c = Vec3::new(
            rng.range(-800.0, 800.0),
            rng.range(-800.0, 800.0),
            rng.range(-50.0, 300.0),
        );
        let e = Vec3::new(
            rng.range(5.0, 300.0),
            rng.range(5.0, 300.0),
            rng.range(1.0, 120.0),
        );
        boxes = boxes.with_box(c - e, c + e, rng.chance(0.5));
    }
    let mut world = SlopeWorld::new(boxes);
    for _ in 0..(rng.next_u64() % 4) {
        let n = Vec3::new(
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(0.05, 1.0),
        );
        let p = Vec3::new(
            rng.range(-800.0, 800.0),
            rng.range(-800.0, 800.0),
            rng.range(-200.0, 100.0),
        );
        if let Some(h) = HalfSpace::through_point(n, p, rng.chance(0.5)) {
            world = world.with_half_space(h);
        }
    }
    world
}

fn random_params(rng: &mut SplitMix64) -> PlayerParams {
    let mut p = PlayerParams::default();
    let m = &mut p.movement;
    m.max_ground_speed.value = rng.range(0.0, 2000.0);
    m.ground_acceleration.value = rng.range(0.0, 10_000.0);
    m.air_control.value = rng.range(0.0, 1.0);
    m.jump_velocity.value = rng.range(0.0, 1500.0);
    m.capsule_radius.value = rng.range(1.0, 60.0);
    m.capsule_half_height.value = rng.range(10.0, 90.0);
    m.step_height.value = rng.range(0.0, m.capsule_half_height.value);
    m.walkable_floor_z.value = rng.range(0.05, 1.0);
    m.world_gravity_z.value = rng.range(-3000.0, 0.0);
    m.custom_gravity_scaling.value = rng.range(0.0, 3.0);
    m.ground_friction.value = rng.range(0.0, 40.0);
    m.terminal_velocity.value = rng.range(10.0, 20_000.0);
    m.limit_fall_accel.value = rng.chance(0.5);
    m.slope_boost_friction.value = if rng.chance(0.5) {
        0.0
    } else {
        rng.range(0.0, 2.0)
    };
    m.movement_speed_modifier.value = rng.range(0.0, 3.0);
    assert_eq!(p.validate(), Ok(()));
    p
}

#[test]
fn fuzzed_worlds_params_inputs_and_dt_never_produce_nan() {
    let mut rng = SplitMix64(0xA5A5_1234);
    let mut grounded_ticks = 0_u32;
    let mut airborne_ticks = 0_u32;
    for _case in 0..120 {
        let world = random_world(&mut rng);
        let params = random_params(&mut rng);
        let mut s = PlayerState::new(
            Vec3::new(
                rng.range(-300.0, 300.0),
                rng.range(-300.0, 300.0),
                rng.range(0.0, 600.0),
            ),
            rng.range(-3.0, 3.0),
        );
        s.velocity = Vec3::new(
            rng.range(-900.0, 900.0),
            rng.range(-900.0, 900.0),
            rng.range(-900.0, 900.0),
        );
        s.grounded = rng.chance(0.5);
        for _ in 0..150 {
            let dt = match rng.next_u64() % 10 {
                0 => rng.range(0.0, 0.001),
                1 => rng.range(0.05, 1.5),
                _ => rng.range(0.001, 0.05),
            };
            let dir =
                Vec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), 0.0).normalize_or_zero();
            let intent = LocomotionIntent {
                wish_dir: if rng.chance(0.2) { Vec3::ZERO } else { dir },
                wish_scale: 1.0,
                jump: rng.chance(0.05),
            };
            let pull = if rng.chance(0.1) {
                Vec3::new(
                    rng.range(-3000.0, 3000.0),
                    rng.range(-3000.0, 3000.0),
                    rng.range(-3000.0, 3000.0),
                )
            } else {
                Vec3::ZERO
            };
            let mut events = StepEvents::default();
            let stats = Ue3PawnMovement.advance_with_stats(
                &mut s,
                &intent,
                pull,
                &params.movement,
                &world,
                dt,
                &mut events,
            );
            assert!(s.is_finite(), "{s:?} params {:?}", params.movement);
            assert_accounting(&stats, dt);
            if s.grounded {
                grounded_ticks += 1;
            } else {
                airborne_ticks += 1;
            }
            // Keep the run in a sane region.
            if s.position.length() > 1.0e5 {
                s = PlayerState::new(Vec3::new(0.0, 0.0, 300.0), 0.0);
            }
        }
    }
    assert!(
        grounded_ticks > 1000 && airborne_ticks > 1000,
        "{grounded_ticks} {airborne_ticks}"
    );
}

#[test]
fn full_step_with_grapple_stays_finite() {
    let params = PlayerParams::default();
    let world = obstacle_world();
    let mut rng = SplitMix64(42);
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
    for _ in 0..3000 {
        let input = InputFrame {
            move_forward: rng.range(-1.0, 1.0),
            move_right: rng.range(-1.0, 1.0),
            look_yaw_delta: rng.range(-0.2, 0.2),
            look_pitch_delta: rng.range(-0.2, 0.2),
            jump_pressed: rng.chance(0.05),
            jump_held: false,
            grapple_held: rng.chance(0.6),
        };
        let events = step_with(&Ue3PawnMovement, &mut s, &input, &params, &world, DT);
        assert!(!events.non_finite_rejected);
        assert!(s.is_finite());
        assert!(
            s.position.z > -10.0,
            "never falls through the ground: {s:?}"
        );
    }
}

#[test]
fn model_kind_names_and_default() {
    assert_eq!(MovementModelKind::default(), MovementModelKind::Placeholder);
    assert_eq!(MovementModelKind::Ue3Pawn.name(), "ue3_pawn");
    assert_eq!(
        serde_json::to_string(&MovementModelKind::Ue3Pawn).unwrap(),
        "\"ue3_pawn\""
    );
    // The trait is object-safe enough to use through the enum.
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 300.0), 0.0);
    let mut events = StepEvents::default();
    MovementModelKind::Ue3Pawn.advance(
        &mut s,
        &no_input(),
        Vec3::ZERO,
        &PlayerParams::default().movement,
        &BoxWorld::new(),
        DT,
        &mut events,
    );
    assert!(s.velocity.z < 0.0);
}
