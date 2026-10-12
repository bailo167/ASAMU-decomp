//! Adversarial conformance tests of `Ue3PawnMovement` against individual
//! rules of `docs/reverse-engineering/NATIVE_PHYSICS.md` that the scenario
//! tests in `ue3_movement.rs` do not pin down (each test names its spec
//! section).
//!
//! Expected values are derived in the test from the spec's formulas (in `f64`
//! where rounding matters), never by calling the code under test. Where a
//! value depends on how far the original reports a hit short of the contact
//! (the pull-back, spec 10.2), the rule is written out here ([`pull`]); the
//! widening of a move's box is the public `MOVE_RADIUS_INFLATION`. The test
//! worlds answer as BSP and the pawn is swept as a box (spec 10.1).
//! Parameter values set here are test inputs, not game values.

mod common;

use asamu_player::movement::{ClassDefaults, Landing, LocomotionIntent, PawnHooks};
use asamu_player::ue3_movement::{
    MOVE_RADIUS_INFLATION, PawnPhysicsState, PhysicsMode, TickStats, Ue3PawnMovement,
};
use asamu_player::{
    BoxWorld, CollisionShape, CollisionWorld, HalfSpace, Hit, MovementModelKind, PlayerParams,
    PlayerState, SlopeWorld, StepEvents, step_with,
};
use common::SplitMix64;
use glam::{DVec3, Vec3};

const DT: f32 = 1.0 / 60.0;
/// What the walking floor check aims at: the *reported* distance to the
/// floor (spec 3.3).
const REPORTED_HOVER: f64 = 2.15;
/// The true height of a walking pawn's bottom above a floor in these worlds:
/// the floor check is `MaxStepHeight + 2` long (20 uu for the default
/// parameters, 28 for the original's), so a BSP floor is reported
/// `pull(20) = pull(28) = 1.0` uu nearer than it is, and the pawn is held at
/// a reported 2.15: 2.15 + 1.0.
const HOVER: f32 = 3.15;

/// The distance (uu) by which the original reports a hit short of the
/// contact on a box trace `len` uu long against BSP, which is what
/// `BoxWorld` and `SlopeWorld` answer as: 0.1 uu for a trace shorter than
/// 1 uu, else a tenth of the trace, at most 1.0 uu (spec 10.2, rule K with
/// K = 1.0; `docs/PARITY_FINDINGS.md` V2). Written out here, not taken from
/// the code under test.
fn pull(len: f64) -> f64 {
    if len < 1.0 { 0.1 } else { (0.1 * len).min(1.0) }
}

/// The fraction of a move `delta` the pawn makes before a surface whose
/// exact contact is at the fraction `raw` of it: the reported hit time.
fn reported(raw: f64, delta: DVec3) -> f64 {
    (raw - pull(delta.length()) / delta.length()).clamp(0.0, 1.0)
}

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

fn hh(params: &PlayerParams) -> f32 {
    params.movement.capsule_half_height.value
}

fn radius(params: &PlayerParams) -> f32 {
    params.movement.capsule_radius.value
}

/// Half width of the box a `MoveActor` sweeps (spec 9.2: ×1.001 on X and Y).
fn move_radius(params: &PlayerParams) -> f32 {
    radius(params) * MOVE_RADIUS_INFLATION
}

fn gravity(params: &PlayerParams) -> f64 {
    f64::from(params.movement.world_gravity_z.value)
        * f64::from(params.movement.custom_gravity_scaling.value)
}

fn d(v: Vec3) -> DVec3 {
    v.as_dvec3()
}

/// Half sizes of the box of an extent trace (the floor check).
fn trace_extent(params: &PlayerParams) -> Vec3 {
    Vec3::new(radius(params), radius(params), hh(params))
}

/// Half sizes of the box of a move.
fn move_extent(params: &PlayerParams) -> Vec3 {
    Vec3::new(move_radius(params), move_radius(params), hh(params))
}

fn walking_at(position: Vec3, floor: Vec3, based: bool, force: bool) -> PlayerState {
    let mut s = PlayerState::new(position, 0.0);
    s.grounded = true;
    s.pawn = PawnPhysicsState {
        floor,
        based,
        force_floor_check: force,
        ..PawnPhysicsState::default()
    };
    s
}

fn airborne(position: Vec3, velocity: Vec3) -> PlayerState {
    let mut s = PlayerState::new(position, 0.0);
    s.velocity = velocity;
    s
}

fn modes(stats: &TickStats) -> Vec<(PhysicsMode, f32)> {
    stats.substeps().iter().map(|s| (s.mode, s.time)).collect()
}

fn assert_modes(stats: &TickStats, expected: &[(PhysicsMode, f32)]) {
    let got = modes(stats);
    assert_eq!(got.len(), expected.len(), "{got:?} vs {expected:?}");
    for ((gm, gt), (em, et)) in got.iter().zip(expected) {
        assert_eq!(gm, em, "{got:?} vs {expected:?}");
        assert!((gt - et).abs() < 1e-6, "{got:?} vs {expected:?}");
    }
}

fn assert_accounting(stats: &TickStats, dt: f32) {
    let total = stats.consumed_time() - stats.returned_time + stats.dropped_time;
    assert!(
        (total - dt).abs() <= 1e-5 * dt.max(1.0),
        "consumed {} - returned {} + dropped {} != {dt}",
        stats.consumed_time(),
        stats.returned_time,
        stats.dropped_time
    );
}

fn close(a: f64, b: f64, tol: f64, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs {b} (tol {tol})");
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

// ---------------------------------------------------------------------------
// 4.6 velocity refinement: condition and displacement source
// ---------------------------------------------------------------------------

/// 4.6: `V = 2·avg − V_old` only if `V_old.Z ≥ 0` or `avg.Z < V_old.Z`.
/// With zero gravity (test input) a pawn already moving down does not speed
/// up downwards, so the refinement is skipped and air acceleration is
/// applied once, not twice. Binary-fraction inputs make every operation
/// exact, so the comparison is exact.
#[test]
fn refinement_is_skipped_when_a_falling_pawn_does_not_speed_up_downwards() {
    let mut params = PlayerParams::default();
    params.movement.custom_gravity_scaling.value = 0.0;
    params.movement.limit_fall_accel.value = false;
    params.movement.ground_acceleration.value = 3200.0;
    let world = BoxWorld::new();
    let h = 1.0 / 64.0;
    // (V0.z, expected V.y): 3200·h = 50 per sub-step, doubled only when refined.
    for (vz, vy) in [(-64.0_f32, 50.0_f32), (0.0, 100.0), (64.0, 100.0)] {
        let mut s = airborne(Vec3::new(0.0, 0.0, 1000.0), Vec3::new(0.0, 0.0, vz));
        let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, h);
        assert_modes(&stats, &[(PhysicsMode::Falling, h)]);
        assert_eq!(s.velocity, Vec3::new(0.0, vy, vz), "V0.z {vz}");
        assert_eq!(s.position, Vec3::new(0.0, 50.0 * h, 1000.0 + vz * h));
    }
}

/// 4.5 step 4 + 4.6: hitting a ceiling while rising keeps the horizontal
/// velocity (actual average) and refines the vertical velocity from the
/// *displacement*: `2·avg.z − V_old.z`, which turns the jump into a downward
/// motion at once (a "ceiling bounce").
#[test]
fn ceiling_contact_reflects_the_vertical_velocity() {
    let params = PlayerParams::default();
    let g = gravity(&params);
    let ceiling = 200.0_f32;
    let world = BoxWorld::new().with_box(
        Vec3::new(-1000.0, -1000.0, ceiling),
        Vec3::new(1000.0, 1000.0, ceiling + 200.0),
        false,
    );
    let gap = 2.05_f64;
    let z0 = ceiling - hh(&params) - gap as f32;
    let v0 = DVec3::new(300.0, 0.0, 400.0);
    let mut s = airborne(Vec3::new(0.0, 0.0, z0), v0.as_vec3());
    let (_, stats) = tick(&mut s, no_input(), &params, &world, DT);
    assert_modes(&stats, &[(PhysicsMode::Falling, DT)]);

    let h = f64::from(DT);
    let v1 = v0 + DVec3::new(0.0, 0.0, g * h); // semi-implicit
    let adj = v1 * h;
    let t = reported(gap / adj.z, adj); // pulled back along the move
    // Slide along the ceiling = horizontal rest of the move (no height clamp
    // applies, slide.z = 0): total displacement (adj.x, 0, adj.z·t).
    let disp = DVec3::new(adj.x, 0.0, adj.z * t);
    close(f64::from(s.position.x), adj.x, 1e-4, "x");
    close(f64::from(s.position.z - z0), disp.z, 1e-4, "z");
    let avg = disp / h;
    // Horizontal: actual average (no extrapolation). Vertical: V_old.z ≥ 0
    // → refined from the displacement.
    let expected = DVec3::new(avg.x, 0.0, 2.0 * avg.z - v0.z);
    assert!(expected.z < -150.0, "bounces down: {expected}");
    close(f64::from(s.velocity.x), expected.x, 1e-2, "vx");
    close(f64::from(s.velocity.z), expected.z, 1e-2, "vz");
    assert!(!s.grounded);
}

/// 4.5 step 4 + 4.6: falling into a vertical wall keeps the doubled gravity
/// (the vertical average still exceeds `V_old.z` in magnitude) while the
/// horizontal velocity becomes the actual average of the sub-step.
#[test]
fn falling_along_a_wall_keeps_doubled_gravity_without_horizontal_extrapolation() {
    let params = PlayerParams::default();
    let g = gravity(&params);
    let world = BoxWorld::new().with_box(
        Vec3::new(100.0, -1000.0, -2000.0),
        Vec3::new(300.0, 1000.0, 3000.0),
        false,
    );
    let gap = 2.0_f32;
    let x0 = 100.0 - move_radius(&params) - gap;
    let v0 = DVec3::new(300.0, 0.0, -100.0);
    let mut s = airborne(Vec3::new(x0, 0.0, 1000.0), v0.as_vec3());
    tick(&mut s, no_input(), &params, &world, DT);

    let h = f64::from(DT);
    let v1 = v0 + DVec3::new(0.0, 0.0, g * h);
    let adj = v1 * h;
    let t = reported(f64::from(gap) / adj.x, adj);
    // Wall normal −X: slide = (0, 0, adj.z)·(1 − t); nothing is clamped
    // (the slide goes down).
    let disp = DVec3::new(adj.x * t, 0.0, adj.z);
    close(f64::from(s.position.x - x0), disp.x, 1e-4, "x");
    close(f64::from(s.position.z) - 1000.0, disp.z, 1e-4, "z");
    let avg = disp / h;
    close(f64::from(s.velocity.x), avg.x, 1e-2, "vx = average");
    close(
        f64::from(s.velocity.z),
        v0.z + 2.0 * g * h,
        1e-2,
        "vz doubled",
    );
    assert_eq!(s.velocity.y, 0.0);
}

/// 4.5 (second hit, not walkable) + 4.8: falling diagonally into an inner
/// vertical corner. The slide along the first wall hits the second wall at
/// 90°, so `TwoWallAdjust` slides along the crease (straight down); the
/// vertical displacement is exactly the unobstructed one and the refined
/// vertical velocity is doubled gravity, horizontal = actual average.
#[test]
fn falling_into_a_vertical_corner_slides_down_the_crease() {
    let params = PlayerParams::default();
    let g = gravity(&params);
    let world = BoxWorld::new()
        .with_box(
            Vec3::new(100.0, -1000.0, -2000.0),
            Vec3::new(300.0, 1000.0, 3000.0),
            false,
        )
        .with_box(
            Vec3::new(-1000.0, 100.0, -2000.0),
            Vec3::new(1000.0, 300.0, 3000.0),
            false,
        );
    let rm = move_radius(&params);
    let (gx, gy) = (5.0_f32, 2.0_f32);
    let p0 = Vec3::new(100.0 - rm - gx, 100.0 - rm - gy, 1000.0);
    let v0 = DVec3::new(600.0, 600.0, 0.0);
    let mut s = airborne(p0, v0.as_vec3());
    tick(&mut s, no_input(), &params, &world, DT);

    let h = f64::from(DT);
    let v1 = v0 + DVec3::new(0.0, 0.0, g * h);
    let adj = v1 * h;
    // First hit: the +Y wall (gap 2 < 5 along equal x/y speeds).
    let t1 = reported(f64::from(gy) / adj.y, adj);
    let slide1 = DVec3::new(adj.x, 0.0, adj.z) * (1.0 - t1);
    // Second hit: the +X wall, part-way along the slide.
    let t2 = reported((f64::from(gx) - adj.x * t1) / slide1.x, slide1);
    // TwoWallAdjust (normals −Y then −X, dot 0): crease = ±Z, flipped to the
    // desired (downward) direction: (0, 0, slide1.z·(1 − t2)).
    let disp = DVec3::new(
        adj.x * t1 + slide1.x * t2,
        adj.y * t1,
        adj.z * t1 + slide1.z * t2 + slide1.z * (1.0 - t2),
    );
    close(disp.z, adj.z, 1e-12, "algebra: vertical motion fully kept");
    let moved = d(s.position - p0);
    close(moved.x, disp.x, 1e-4, "x");
    close(moved.y, disp.y, 1e-4, "y");
    close(moved.z, disp.z, 1e-4, "z");
    let avg = disp / h;
    close(f64::from(s.velocity.x), avg.x, 1e-2, "vx");
    close(f64::from(s.velocity.y), avg.y, 1e-2, "vy");
    close(f64::from(s.velocity.z), 2.0 * g * h, 1e-2, "vz = 2·g·h");
    assert!(!s.grounded);
}

/// 4.6: the `TerminalVelocity` clamp acts on the refined velocity *after*
/// the move; the move itself used the unclamped semi-implicit velocity.
#[test]
fn terminal_velocity_clamps_after_the_move_not_before() {
    let mut params = PlayerParams::default();
    params.movement.terminal_velocity.value = 100.0;
    let g = gravity(&params);
    let world = BoxWorld::new();
    let mut s = airborne(Vec3::new(0.0, 0.0, 1000.0), Vec3::new(0.0, 0.0, -100.0));
    tick(&mut s, no_input(), &params, &world, DT);
    let h = f64::from(DT);
    close(
        f64::from(s.position.z) - 1000.0,
        (-100.0 + g * h) * h,
        1e-4,
        "moved faster than TerminalVelocity",
    );
    close(f64::from(s.velocity.z), -100.0, 1e-3, "clamped afterwards");
    assert_eq!(s.velocity.x, 0.0);
}

// ---------------------------------------------------------------------------
// 1.4 time carry-over between walking and falling
// ---------------------------------------------------------------------------

/// 4.5: a landing found only after a slide (wall, then floor) carries
/// **zero** time into walking: the rest of the tick is dropped, the
/// velocity is not re-derived and no walking sub-step runs (so the pawn rests
/// where the slide's reported hit left it, not at the hover, until the next
/// tick).
#[test]
fn landing_found_by_a_slide_carries_no_time() {
    let params = PlayerParams::default();
    let g = gravity(&params);
    let world = BoxWorld::new().with_ground(0.0, false).with_box(
        Vec3::new(50.0, -1000.0, -10.0),
        Vec3::new(250.0, 1000.0, 3000.0),
        false,
    );
    let x0 = 50.0 - move_radius(&params) - 1.0;
    let z0 = hh(&params) + 3.0;
    let v0 = DVec3::new(600.0, 0.0, -300.0);
    let mut s = airborne(Vec3::new(x0, 0.0, z0), v0.as_vec3());
    let dt = 0.1;
    let (events, stats) = tick(&mut s, no_input(), &params, &world, dt);

    let step = 0.05_f64;
    let v1 = v0 + DVec3::new(0.0, 0.0, g * step);
    let adj = v1 * step;
    // Wall first (x gap 1 → t ≈ 0.03) before the floor (z gap 3 → t ≈ 0.18).
    let t1 = reported(1.0 / adj.x, adj);
    assert!(t1 > 0.0 && 1.0 / adj.x < 3.0 / -adj.z);
    assert!(s.grounded && s.pawn.based && s.pawn.floor == Vec3::Z);
    assert_modes(&stats, &[(PhysicsMode::Falling, 0.05)]);
    assert_eq!(stats.returned_time, 0.0);
    close(f64::from(stats.dropped_time), 0.05, 1e-6, "rest dropped");
    assert_accounting(&stats, dt);
    // Landing velocity = semi-implicit velocity (no re-derivation after a
    // slide); horizontal velocity untouched.
    close(
        f64::from(events.landed.expect("landed")),
        v1.z,
        1e-3,
        "impact",
    );
    close(f64::from(s.velocity.x), v1.x, 1e-3, "vx");
    // The slide along the wall is the rest of the fall, straight down
    // (4.7: (0, 0, adj.z)·(1 − t1)); it meets the floor, and its reported
    // hit is short of it by the pull-back of a trace of the slide's length.
    let slide = -adj.z * (1.0 - t1);
    let fallen = -adj.z * t1;
    assert!(3.0 - fallen < slide, "the slide reaches the floor");
    let rest = pull(slide);
    assert!(3.0 - fallen > rest, "and stops short of it");
    close(f64::from(s.position.z - hh(&params)), rest, 1e-4, "gap");
    close(f64::from(s.position.x - x0), adj.x * t1, 1e-4, "x");
    // Next tick: the forced floor check (3.3) reports the floor at
    // rest − pull(probe), not below 0; that is under 1.9, so the pawn is
    // moved up by 2.15 minus the reported distance.
    let probe = f64::from(params.movement.step_height.value) + 2.0;
    let seen = (rest - pull(probe)).max(0.0);
    tick(&mut s, no_input(), &params, &world, DT);
    assert!(s.grounded);
    close(
        f64::from(s.position.z - hh(&params)),
        rest + REPORTED_HOVER - seen,
        1e-4,
        "hover",
    );
}

/// 1.4 (`StartFalling`): when the walking sub-step is cut short by a wall
/// and the pawn ends over a pit, the untravelled part `step·(1 − frac)` is
/// given back to falling (frac = horizontal progress / |Delta|).
#[test]
fn walking_into_a_wall_over_a_pit_returns_the_untravelled_time() {
    let params = PlayerParams::default();
    let r = radius(&params);
    let speed = params.movement.max_ground_speed.value;
    let wall_x = 100.0 + 2.0 * r + 5.0;
    let world = BoxWorld::new()
        .with_box(
            Vec3::new(-1000.0, -500.0, -100.0),
            Vec3::new(100.0, 500.0, 0.0),
            false,
        )
        .with_box(
            Vec3::new(wall_x, -500.0, -2000.0),
            Vec3::new(wall_x + 200.0, 500.0, 2000.0),
            false,
        );
    // Still supported by the edge of the box (centre within r of x = 100).
    let x0 = 100.0 + r - 1.0;
    let mut s = walking_at(
        Vec3::new(x0, 0.0, hh(&params) + HOVER),
        Vec3::Z,
        true,
        false,
    );
    s.velocity = Vec3::new(speed, 0.0, 0.0);
    let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, DT);

    let h = f64::from(DT);
    let delta = f64::from(speed) * h;
    // The move stops pull(Delta) short of the wall. The step-up that follows
    // (3.2.1) tries the rest of the move again from 20 uu higher, where the
    // wall is the same: that shorter trace is pulled back less, so the pawn
    // ends pull(rest) from the wall.
    let gap = f64::from(wall_x - move_radius(&params) - x0);
    let first = gap - pull(delta);
    let rest = delta - first;
    assert!(first > 0.0 && rest >= 1.0);
    let travel = gap - pull(rest);
    let carry = h * (1.0 - travel / delta);
    assert!(carry > 0.003, "a real fraction of the sub-step: {carry}");
    close(f64::from(stats.returned_time), carry, 1e-6, "returned");
    assert_modes(
        &stats,
        &[
            (PhysicsMode::Walking, DT),
            (PhysicsMode::Falling, carry as f32),
        ],
    );
    assert_accounting(&stats, DT);
    assert!(events.left_ground && !s.grounded);
}

/// 1.4: `remaining = 0 if |Delta| = 0`. A pawn standing still whose floor is
/// gone starts falling with no time left this tick (nothing moves).
#[test]
fn standing_still_without_a_floor_falls_from_the_next_tick() {
    let params = PlayerParams::default();
    let world = BoxWorld::new();
    let p0 = Vec3::new(0.0, 0.0, 100.0);
    let mut s = walking_at(p0, Vec3::Z, false, true);
    let (events, stats) = tick(&mut s, no_input(), &params, &world, DT);
    assert!(events.left_ground && !s.grounded && !s.pawn.based);
    assert_eq!(s.position, p0);
    assert_eq!(s.velocity, Vec3::ZERO);
    assert_modes(&stats, &[(PhysicsMode::Walking, DT)]);
    assert_eq!(stats.returned_time, 0.0);
    assert_accounting(&stats, DT);
}

/// 3.1 + 1.4: a nearly-zero but non-zero move ends the walking loop with
/// `remaining = 0`, and the fall then gives back `step·(1 − 0)` = one
/// sub-step (not the whole remainder: with a 0.2 s frame only 0.05 s fall).
#[test]
fn nearly_zero_move_off_a_missing_floor_falls_for_one_substep() {
    let mut params = PlayerParams::default();
    // AccelRate·dt·step < 1e-4 for these dt: Delta is "nearly zero" but not 0.
    params.movement.ground_acceleration.value = 0.005;
    let g = gravity(&params);
    let world = BoxWorld::new();
    for (dt, step) in [(DT, DT), (0.2_f32, 0.05_f32)] {
        let p0 = Vec3::new(0.0, 0.0, 100.0);
        let mut s = walking_at(p0, Vec3::Z, false, true);
        let (_, stats) = tick(&mut s, towards(Vec3::X), &params, &world, dt);
        assert_modes(
            &stats,
            &[(PhysicsMode::Walking, dt), (PhysicsMode::Falling, step)],
        );
        close(
            f64::from(stats.returned_time),
            f64::from(step),
            1e-7,
            "carry",
        );
        assert_accounting(&stats, dt);
        // One falling sub-step from rest: Δz = g·h², Vz = 2·g·h.
        let h = f64::from(step);
        close(f64::from(s.position.z - p0.z), g * h * h, 1e-4, "dz");
        close(f64::from(s.velocity.z), 2.0 * g * h, 1e-3, "vz");
    }
}

/// 1.4: the 8-sub-step budget is shared across modes. A 0.5 s frame walks
/// three 0.05 s sub-steps, walks off the ledge with nothing given back
/// (free move), falls for the five sub-steps left and drops 0.1 s.
#[test]
fn walking_off_a_ledge_shares_the_iteration_budget_with_falling() {
    let params = PlayerParams::default();
    let speed = params.movement.max_ground_speed.value;
    let world = BoxWorld::new().with_box(
        Vec3::new(-1000.0, -500.0, -100.0),
        Vec3::new(100.0, 500.0, 0.0),
        false,
    );
    // 22.5 uu per sub-step; supported while x < 100 + r = 120.
    let x0 = 120.0 - 3.0 * speed * 0.05 + 10.0;
    let mut s = walking_at(
        Vec3::new(x0, 0.0, hh(&params) + HOVER),
        Vec3::Z,
        true,
        false,
    );
    s.velocity = Vec3::new(speed, 0.0, 0.0);
    let (_, stats) = tick(&mut s, towards(Vec3::X), &params, &world, 0.5);
    let w = (PhysicsMode::Walking, 0.05);
    let f = (PhysicsMode::Falling, 0.05);
    assert_modes(&stats, &[w, w, w, f, f, f, f, f]);
    assert!(stats.returned_time.abs() < 1e-7);
    close(f64::from(stats.dropped_time), 0.1, 1e-6, "dropped");
    assert_eq!(stats.mode_changes, 1);
    assert_accounting(&stats, 0.5);
}

/// 4.2 + 5.2 + 3.1: the air limiter's clamp is per tick, but a landing
/// keeps the *clamped* acceleration (only normalised). When the wall probe
/// zeroed air control (limit `AccelRate·0 = 0` at 10 ≤ speed < GroundSpeed),
/// the carried walking time brakes even though input is held; without the
/// wall the same landing accelerates.
#[test]
fn landing_keeps_the_air_clamped_acceleration_for_the_rest_of_the_tick() {
    let params = PlayerParams::default();
    let friction = f64::from(params.movement.ground_friction.value);
    let accel = f64::from(params.movement.ground_acceleration.value);
    let air = f64::from(params.movement.air_control.value);
    let run = |with_wall: bool| {
        let mut world = BoxWorld::new().with_ground(0.0, false);
        if with_wall {
            // Inside the probe reach (85 uu) but out of reach of the moves.
            world = world.with_box(
                Vec3::new(60.0, -500.0, -10.0),
                Vec3::new(260.0, 500.0, 500.0),
                false,
            );
        }
        let mut s = airborne(
            Vec3::new(0.0, 0.0, hh(&params) + 1.0),
            Vec3::new(100.0, 0.0, -100.0),
        );
        let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, 0.1);
        assert!(events.landed.is_some() && s.grounded, "wall {with_wall}");
        assert_eq!(stats.count(PhysicsMode::Falling), 1);
        assert_accounting(&stats, 0.1);
        let carry = 0.05 + f64::from(stats.returned_time);
        (s, carry)
    };
    // Wall: TickAirControl = 0 → acceleration 0 in the air → SafeNormal(0) = 0
    // after landing → walking brakes from the landing speed (100 uu/s).
    let (s, carry) = run(true);
    let expected = braking_reference(100.0, carry, friction);
    assert!(expected < 80.0);
    close(f64::from(s.velocity.x), expected, 0.05, "braked");
    // No wall: air acceleration AccelRate·AirControl in +X (landing speed
    // 100 + 2·… no: the landing velocity is the semi-implicit one,
    // 100 + AccelRate·AirControl·0.05), then unit acceleration → full
    // AccelRate on the ground.
    let (s, carry) = run(false);
    let landing_vx = 100.0 + accel * air * 0.05;
    close(
        f64::from(s.velocity.x),
        landing_vx + accel * carry,
        0.05,
        "accelerated",
    );
}

// ---------------------------------------------------------------------------
// 2.1 / 3.3 / 3.5 walking details
// ---------------------------------------------------------------------------

/// 2.1: `MaxSpeedModifier` scales the speed cap only, never the
/// acceleration (UDK path).
#[test]
fn movement_speed_modifier_scales_the_cap_not_the_acceleration() {
    let mut params = PlayerParams::default();
    params.movement.movement_speed_modifier.value = 0.5;
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = walking_at(
        Vec3::new(0.0, 0.0, hh(&params) + HOVER),
        Vec3::Z,
        true,
        false,
    );
    let per_tick = params.movement.ground_acceleration.value * DT; // 50
    let cap = params.movement.max_ground_speed.value * 0.5; // 225
    for n in 1..=8 {
        tick(&mut s, towards(Vec3::X), &params, &world, DT);
        let expected = (per_tick * n as f32).min(cap);
        assert!(
            (s.velocity.x - expected).abs() < 0.02,
            "tick {n}: {} vs {expected}",
            s.velocity.x
        );
    }
}

/// 3.3: a floor trace that starts penetrating is never pushed up
/// ("only if FloorDist < 1.9 **and not penetrating**"), while a pawn just
/// above the floor (not penetrating) is pushed up by 2.15 less the reported
/// distance.
#[test]
fn penetrating_floor_trace_is_not_pushed_up() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false);
    let inside = Vec3::new(0.0, 0.0, hh(&params) - 1.0);
    let mut s = walking_at(inside, Vec3::Z, true, true);
    tick(&mut s, no_input(), &params, &world, DT);
    assert_eq!(s.position, inside);
    assert!(s.grounded);
    // 0.05 uu above the floor: the check reports it pull(probe) = 1.0 uu
    // nearer, which is clamped to a distance of 0, so the pawn is moved up
    // by the whole 2.15 and ends 2.20 above the floor.
    let near = Vec3::new(0.0, 0.0, hh(&params) + 0.05);
    let mut s = walking_at(near, Vec3::Z, true, true);
    tick(&mut s, no_input(), &params, &world, DT);
    let probe = f64::from(params.movement.step_height.value) + 2.0;
    assert!(0.05 - pull(probe) < 0.0);
    close(
        f64::from(s.position.z - hh(&params)),
        0.05 + REPORTED_HOVER,
        1e-4,
        "pushed up",
    );
}

/// 3.3 steep branch: walking into an unwalkable slope that is under the
/// pawn slides it down the slope by `0.1·N + (D − N·(D·N))`,
/// `D = (0, 0, −MaxStepHeight)`; nothing else is hit, so the floor stays
/// unwalkable and the pawn starts falling (3.4) with nothing given back
/// (the slide moved it more than |Delta| horizontally, frac = 1).
#[test]
fn steep_floor_pushed_into_slides_down_the_slope_and_falls() {
    let params = PlayerParams::default();
    let nz = 0.5_f32;
    assert!(nz < params.movement.walkable_floor_z.value);
    let ramp = HalfSpace::ramp_x(nz, Vec3::ZERO, false).expect("valid ramp");
    let n = ramp.normal;
    let world = SlopeWorld::new(BoxWorld::new()).with_half_space(ramp);
    let contact = world
        .sweep_box(
            Vec3::new(0.0, 0.0, 500.0),
            Vec3::new(0.0, 0.0, -500.0),
            trace_extent(&params),
        )
        .expect("ramp below")
        .hit
        .position;
    let p0 = contact + Vec3::Z * HOVER;
    let mut s = walking_at(p0, n, true, true);
    let (events, stats) = tick(&mut s, towards(Vec3::X), &params, &world, DT);

    // Uphill on a floor with Z < 0.98 → stepUp with Hit = floor: nothing
    // moves horizontally (head-on slide is zero), then the planned step
    // down (MaxStepHeight + 2) with the MoveActor sweep stops at the slope.
    let step_down = f64::from(params.movement.step_height.value) + 2.0;
    let t_down = world
        .sweep_box(p0, p0 - Vec3::Z * step_down as f32, move_extent(&params))
        .expect("slope below")
        .hit
        .time;
    let z_after_step =
        f64::from(p0.z) - step_down * reported(f64::from(t_down), DVec3::new(0.0, 0.0, -step_down));
    // Steep slide.
    let nd = d(n);
    let drop = DVec3::new(0.0, 0.0, -f64::from(params.movement.step_height.value));
    let slide = nd * 0.1 + (drop - nd * drop.dot(nd));
    close(f64::from(s.position.x - p0.x), slide.x, 1e-4, "x");
    close(f64::from(s.position.z), z_after_step + slide.z, 1e-3, "z");
    assert_eq!(s.position.y, p0.y);
    assert!(events.left_ground && !s.grounded);
    assert_modes(&stats, &[(PhysicsMode::Walking, DT)]);
    assert_eq!(stats.returned_time, 0.0, "frac = min(1, …) = 1");
    // StartFalling keeps the CalcVelocity result with Z = 0.
    close(
        f64::from(s.velocity.x),
        f64::from(params.movement.ground_acceleration.value * DT),
        1e-3,
        "vx",
    );
    assert_eq!(s.velocity.z, 0.0);
}

/// 3.5: the slope gravity slide uses the **whole** call's `dt` on every
/// sub-step (`g·dt/(2·max(F, 0.5))·dt` per sub-step). A 0.1 s frame walking
/// along the contour of a slippery ramp takes two sub-steps and slides
/// `2·g'(0.1)` (with the sub-step length it would be four times less).
#[test]
fn slope_slide_uses_the_whole_call_dt_on_every_substep() {
    let mut params = PlayerParams::default();
    params.movement.ground_friction.value = 2.0;
    let speed = params.movement.max_ground_speed.value;
    let nz = 0.8_f32;
    assert!(nz * 2.0 < 3.3 && nz < 0.99 && nz >= params.movement.walkable_floor_z.value);
    let ramp = HalfSpace::ramp_x(nz, Vec3::ZERO, false).expect("valid ramp");
    let n = d(ramp.normal);
    let world = SlopeWorld::new(BoxWorld::new()).with_half_space(ramp);
    let contact = world
        .sweep_box(
            Vec3::new(0.0, 0.0, 500.0),
            Vec3::new(0.0, 0.0, -500.0),
            trace_extent(&params),
        )
        .expect("ramp below")
        .hit
        .position;
    let p0 = contact + Vec3::Z * HOVER;
    let mut s = walking_at(p0, ramp.normal, true, false);
    s.velocity = Vec3::new(0.0, speed, 0.0);
    let dt = 0.1_f32;
    let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, dt);
    assert_modes(
        &stats,
        &[(PhysicsMode::Walking, 0.05), (PhysicsMode::Walking, 0.05)],
    );
    let g = gravity(&params);
    let dtd = f64::from(dt);
    let gp = g * dtd / (2.0 * 2.0_f64.max(0.5)) * dtd;
    let gv = DVec3::new(0.0, 0.0, gp);
    let slide = gv - n * n.dot(gv);
    let moved = d(s.position - p0);
    close(
        moved.x,
        2.0 * slide.x,
        1e-3,
        "x: two slides of g'(whole dt)",
    );
    close(moved.y, f64::from(speed) * dtd, 1e-3, "y");
    close(moved.z, 2.0 * slide.z, 1e-3, "z");
    close(f64::from(s.velocity.x), 2.0 * slide.x / dtd, 1e-2, "vx");
    assert_eq!(s.velocity.z, 0.0);
    assert!(s.grounded);
}

// ---------------------------------------------------------------------------
// Determinism and robustness
// ---------------------------------------------------------------------------

fn course() -> SlopeWorld {
    SlopeWorld::new(
        BoxWorld::new()
            .with_ground(0.0, false)
            .with_box(
                Vec3::new(200.0, -300.0, 0.0),
                Vec3::new(400.0, 300.0, 12.0),
                false,
            )
            .with_box(
                Vec3::new(600.0, -300.0, 0.0),
                Vec3::new(700.0, 300.0, 400.0),
                true,
            ),
    )
    .with_half_space(HalfSpace::ramp_x(0.75, Vec3::new(-400.0, 0.0, 0.0), false).expect("ramp"))
}

/// The hidden native state travels with `PlayerState`: snapshotting the
/// state through serde mid-run and continuing gives a bit-identical run.
#[test]
fn snapshot_and_resume_is_bit_identical() {
    let params = PlayerParams::default();
    let world = course();
    let mut rng = SplitMix64(0xC0FF_EE00);
    let inputs: Vec<_> = (0..600)
        .map(|i| asamu_player::InputFrame {
            move_forward: if i % 200 < 150 { 1.0 } else { -1.0 },
            move_right: if i % 70 < 20 {
                rng.range(-1.0, 1.0)
            } else {
                0.0
            },
            look_yaw_delta: if i % 45 == 0 {
                rng.range(-0.6, 0.6)
            } else {
                0.0
            },
            jump_pressed: i % 53 == 0,
            ..asamu_player::InputFrame::default()
        })
        .collect();
    let mut a = PlayerState::new(Vec3::new(0.0, 0.0, 120.0), 0.0);
    let mut resumed = None;
    for (i, input) in inputs.iter().enumerate() {
        if i == 300 {
            let json = serde_json::to_string(&a).expect("serialize");
            resumed = Some(serde_json::from_str::<PlayerState>(&json).expect("deserialize"));
        }
        step_with(
            &MovementModelKind::Ue3Pawn,
            &mut a,
            input,
            &params,
            &world,
            DT,
        );
    }
    let mut b = resumed.expect("snapshot taken");
    for input in &inputs[300..] {
        step_with(
            &MovementModelKind::Ue3Pawn,
            &mut b,
            input,
            &params,
            &world,
            DT,
        );
    }
    assert_eq!(a, b);
    assert_eq!(
        a.position.to_array().map(f32::to_bits),
        b.position.to_array().map(f32::to_bits)
    );
}

/// A contract-violating collision world: deterministic garbage hits (NaN or
/// out-of-range times, zero / non-unit / non-finite normals, random
/// "start penetrating"). The port must stay finite and keep its time
/// accounting; it may of course move nonsensically.
struct GarbageWorld;

fn mix(bits: &[u32]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325_u64;
    for b in bits {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let mut rng = SplitMix64(h);
    rng.next_u64()
}

impl CollisionWorld for GarbageWorld {
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        let key = [
            start.x.to_bits(),
            start.y.to_bits(),
            start.z.to_bits(),
            end.x.to_bits(),
            end.y.to_bits(),
            end.z.to_bits(),
            shape.radius.to_bits(),
        ];
        let r = mix(&key);
        let time = match r % 8 {
            0 => return None,
            1 => f32::NAN,
            2 => -3.0,
            3 => 7.5,
            4 => f32::INFINITY,
            _ => ((r >> 8) % 1000) as f32 / 1000.0,
        };
        let normal = match (r >> 20) % 7 {
            0 => Vec3::ZERO,
            1 => Vec3::new(f32::NAN, 0.0, 1.0),
            2 => Vec3::new(1.0e30, -1.0e30, 1.0e30),
            3 => Vec3::new(0.0, f32::INFINITY, 0.0),
            4 => Vec3::new(0.3, 0.4, 5.0),
            5 => Vec3::Z,
            _ => Vec3::new(-0.6, 0.0, 0.8),
        };
        Some(Hit {
            time,
            distance: time,
            position: start,
            normal,
            grapple_able: false,
            start_penetrating: (r >> 40).is_multiple_of(3),
            surface: asamu_player::world::Surface::default(),
        })
    }

    fn raycast(&self, _origin: Vec3, _direction: Vec3, _max_distance: f32) -> Option<Hit> {
        None
    }
}

#[test]
fn garbage_collision_results_never_produce_non_finite_state() {
    let params = PlayerParams::default();
    let mut rng = SplitMix64(7);
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
    let mut both_modes = (false, false);
    for i in 0..4000 {
        let dt = if i % 13 == 0 {
            rng.range(0.05, 1.0)
        } else {
            rng.range(0.001, 0.05)
        };
        let intent = LocomotionIntent {
            wish_dir: Vec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), 0.0)
                .normalize_or_zero(),
            wish_scale: 1.0,
            jump: rng.chance(0.05),
        };
        if i % 50 == 0 {
            s.grounded = rng.chance(0.5);
            s.velocity = Vec3::new(
                rng.range(-900.0, 900.0),
                rng.range(-900.0, 900.0),
                rng.range(-900.0, 900.0),
            );
        }
        let mut events = StepEvents::default();
        let stats = Ue3PawnMovement.advance_with_stats(
            &mut s,
            &intent,
            Vec3::ZERO,
            &params.movement,
            &GarbageWorld,
            dt,
            &mut events,
        );
        assert!(s.is_finite(), "tick {i}: {s:?}");
        assert!(stats.returned_time.is_finite() && stats.dropped_time.is_finite());
        assert_accounting(&stats, dt);
        if s.grounded {
            both_modes.0 = true;
        } else {
            both_modes.1 = true;
        }
        if s.position.length() > 1.0e6 {
            s.position = Vec3::new(0.0, 0.0, 100.0);
        }
    }
    assert!(both_modes.0 && both_modes.1);
}

// ---------------------------------------------------------------------------
// More velocity-update, stepUp and slide rules
// ---------------------------------------------------------------------------

/// 2.1 + 3.1: `CalcVelocity` runs once per walking call with the whole
/// `dt` (turning friction `V −= (V − dir·|V|)·F·dt`, `+ A·dt`, 3-D cap),
/// not once per movement sub-step. A 0.1 s frame has two sub-steps.
#[test]
fn calc_velocity_runs_once_per_walking_call_with_the_whole_dt() {
    let params = PlayerParams::default();
    let f = f64::from(params.movement.ground_friction.value);
    let a = f64::from(params.movement.ground_acceleration.value);
    let cap = f64::from(params.movement.max_ground_speed.value);
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut s = walking_at(
        Vec3::new(0.0, 0.0, hh(&params) + HOVER),
        Vec3::Z,
        true,
        false,
    );
    s.velocity = Vec3::new(450.0, 0.0, 0.0);
    let dt = 0.1_f64;
    let (_, stats) = tick(&mut s, towards(Vec3::Y), &params, &world, dt as f32);
    assert_eq!(stats.count(PhysicsMode::Walking), 2);
    let v0 = DVec3::new(450.0, 0.0, 0.0);
    let dir = DVec3::Y;
    let mut v = v0 - (v0 - dir * v0.length()) * f * dt + dir * a * dt;
    if v.length() > cap {
        v = v.normalize() * cap;
    }
    // Free moves: walking velocity (displacement / dt) equals the update.
    close(f64::from(s.velocity.x), v.x, 1e-2, "vx");
    close(f64::from(s.velocity.y), v.y, 1e-2, "vy");
}

/// 3.2 → 3.2.1 step 5: walking diagonally into a wall too tall to step on:
/// step up, the move across is blocked again (a little nearer the wall: a
/// shorter trace is pulled back less), the rest slides along the
/// horizontalised wall normal, then the step down. The wall-parallel part
/// of the move is kept in full; the walking velocity is the displacement.
/// Into an inner corner the slide hits the second wall, `TwoWallAdjust`
/// gives the vertical crease (zero horizontal move) and the pawn stops.
#[test]
fn walking_into_a_tall_wall_slides_along_it_and_stops_in_a_corner() {
    let params = PlayerParams::default();
    let speed = f64::from(params.movement.max_ground_speed.value);
    let wall = |world: BoxWorld| {
        world.with_box(
            Vec3::new(100.0, -1000.0, -10.0),
            Vec3::new(300.0, 1000.0, 500.0),
            false,
        )
    };
    let world = wall(BoxWorld::new().with_ground(0.0, false));
    let rm = f64::from(move_radius(&params));
    let gap = 3.0_f64;
    let x0 = (100.0 - rm - gap) as f32;
    let z0 = hh(&params) + HOVER;
    let dir = Vec3::new(1.0, 1.0, 0.0).normalize();
    let mut s = walking_at(Vec3::new(x0, 0.0, z0), Vec3::Z, true, false);
    s.velocity = dir * speed as f32;
    tick(&mut s, towards(dir), &params, &world, DT);
    let h = f64::from(DT);
    let delta = d(dir) * speed * h;
    let t = reported(gap / delta.x, delta);
    // Up; across with the rest of the move, blocked by the same wall at
    // t2; slide (0, rest.y·(1 − t2), 0); down.
    let rest_of_move = delta * (1.0 - t);
    let t2 = reported((gap - delta.x * t) / rest_of_move.x, rest_of_move);
    assert!(t > 0.0 && t2 > 0.0);
    let x = delta.x * t + rest_of_move.x * t2;
    close(f64::from(s.position.x - x0), x, 1e-3, "x");
    close(f64::from(s.position.y), delta.y, 1e-3, "y kept in full");
    close(f64::from(s.position.z), f64::from(z0), 1e-4, "z");
    close(f64::from(s.velocity.y), delta.y / h, 0.05, "vy");
    close(f64::from(s.velocity.x), x / h, 0.05, "vx");
    assert!(s.grounded);

    // Inner corner: second wall at y = 100.
    let corner = world.clone().with_box(
        Vec3::new(-1000.0, 100.0, -10.0),
        Vec3::new(1000.0, 300.0, 500.0),
        false,
    );
    let p0 = Vec3::new(x0, (100.0 - rm - 4.0) as f32, z0);
    let mut s = walking_at(p0, Vec3::Z, true, false);
    s.velocity = dir * speed as f32;
    for _ in 0..30 {
        tick(&mut s, towards(dir), &params, &corner, DT);
        assert!(s.grounded);
    }
    // Where it can stop. Standing still, each tick's attempted move is
    // AccelRate·dt² = 0.83 uu along the diagonal: shorter than 1 uu, so a
    // hit is reported 0.1 uu short along the trace. The pawn stays put only
    // if neither the move nor the slide gets anywhere: the nearer wall is no
    // more than 0.1·cos 45° away along its normal (the diagonal move is
    // reported at time 0), and the other no more than 0.1 (so is the slide,
    // which runs square into it). Which wall ends up the nearer, and where
    // inside that range the other one is, depends on the approach. It never
    // touches either.
    assert!(f64::from(params.movement.ground_acceleration.value) * h * h < 1.0);
    let rest = 100.0 - rm;
    let gaps = [
        rest - f64::from(s.position.x),
        rest - f64::from(s.position.y),
    ];
    let (near, far) = (gaps[0].min(gaps[1]), gaps[0].max(gaps[1]));
    assert!(near > 0.0, "never touches: {gaps:?}");
    assert!(
        near <= 0.1 * std::f64::consts::FRAC_1_SQRT_2 + 1e-4,
        "the diagonal move is held: {gaps:?}"
    );
    assert!(far <= 0.1 + 1e-4, "the slide is held: {gaps:?}");
    assert!(s.velocity.length() < 1.0, "{}", s.velocity);
    let before = s.position;
    tick(&mut s, towards(dir), &params, &corner, DT);
    assert_eq!(s.position, before, "at rest");
    assert_eq!(s.velocity, Vec3::ZERO);
}

/// 4.7: with `SlopeBoostFriction = 0` a falling slide up an unwalkable slope
/// is not height-clamped and launches the pawn upwards ("slope boosting");
/// with a non-zero value (no physical material) the slide may not climb, so
/// the pawn gains much less height (only what `TwoWallAdjust` gives back).
#[test]
fn slope_boost_friction_zero_lets_a_falling_slide_gain_height() {
    let nz = 0.5_f32;
    let ramp = HalfSpace::ramp_x(nz, Vec3::new(100.0, 0.0, 0.0), false).expect("ramp");
    let n = d(ramp.normal);
    let world = SlopeWorld::new(BoxWorld::new()).with_half_space(ramp);
    let run = |sbf: f32| {
        let mut params = PlayerParams::default();
        params.movement.slope_boost_friction.value = sbf;
        let extent = move_extent(&params);
        // Start 3 uu (horizontally) before touching the slope.
        let probe = world
            .sweep_box(
                Vec3::new(-500.0, 0.0, 100.0),
                Vec3::new(500.0, 0.0, 100.0),
                extent,
            )
            .expect("slope ahead")
            .hit
            .position;
        let p0 = probe - Vec3::X * 3.0;
        let mut s = airborne(p0, Vec3::new(600.0, 0.0, 0.0));
        tick(&mut s, no_input(), &params, &world, DT);
        let h = f64::from(DT);
        let adj = DVec3::new(600.0, 0.0, gravity(&params) * h) * h;
        let t_hit = world
            .sweep_box(p0, p0 + adj.as_vec3(), extent)
            .expect("hits the slope")
            .hit
            .time;
        let t = reported(f64::from(t_hit), adj);
        (s, p0, adj, t)
    };
    let (free, p0, adj, t) = run(0.0);
    // Unclamped slide along the plane: no further hit.
    let slide = (adj - n * adj.dot(n)) * (1.0 - t);
    assert!(slide.z > 0.0);
    let dz = adj.z * t + slide.z;
    close(f64::from(free.position.z - p0.z), dz, 1e-4, "free dz");
    // Refinement from rest (V_old.z = 0): Vz = 2·dz/h > 0, launched upwards.
    close(
        f64::from(free.velocity.z),
        2.0 * dz / f64::from(DT),
        1e-2,
        "vz",
    );
    assert!(free.velocity.z > 100.0, "{}", free.velocity);
    let (clamped, p0c, _, _) = run(0.5);
    let dzc = f64::from(clamped.position.z - p0c.z);
    assert!(dzc < 0.5 * dz, "clamped {dzc} vs free {dz}");
}

/// 4.1: pawn gravity = world gravity × `CustomGravityScaling`, then doubled
/// in effect by the refinement.
#[test]
fn custom_gravity_scaling_multiplies_world_gravity() {
    let world = BoxWorld::new();
    for scale in [0.5_f32, 1.0, 1.5] {
        let mut params = PlayerParams::default();
        params.movement.custom_gravity_scaling.value = scale;
        let g = -520.0 * f64::from(scale);
        // At the origin: the refinement re-derives the velocity from the
        // f32 displacement, so far from the origin a 0.07 uu step carries
        // the position's rounding (as it would in the original).
        let mut s = airborne(Vec3::ZERO, Vec3::ZERO);
        tick(&mut s, no_input(), &params, &world, DT);
        let h = f64::from(DT);
        close(f64::from(s.velocity.z), 2.0 * g * h, 1e-4, "vz");
        close(f64::from(s.position.z), g * h * h, 1e-6, "dz");
    }
}

/// 2.3: no guard against `2·F·h > 1`. With a huge friction the pieces
/// oscillate in sign; reversed pieces are left out of the average but a
/// later piece that points forward again counts, so braking can keep most of
/// the speed (F = 40, dt = 0.06: pieces ×(−1.4), ×(−1.4) → 0.98·V0).
#[test]
fn braking_with_overshooting_friction_follows_the_piecewise_rule() {
    use asamu_player::ue3_movement::apply_velocity_braking;
    for &(v0, dt, f) in &[
        (400.0_f32, 0.06_f32, 40.0_f32),
        (400.0, 0.045, 12.0),
        (400.0, 0.04, 20.0),
        (-250.0, 0.09, 35.0),
    ] {
        let got = apply_velocity_braking(Vec3::new(v0, 0.0, 0.0), dt, f).x;
        let want = braking_reference(f64::from(v0), f64::from(dt), f64::from(f));
        close(f64::from(got), want, 1e-3 * want.abs().max(1.0), "braking");
    }
    let kept = apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), 0.06, 40.0).x;
    close(f64::from(kept), 0.98 * 400.0, 1e-2, "oscillating pieces");
}

/// 3.2.1 step 5 retry: the slide along a first wall hits a second wall at an
/// obtuse angle (normals' dot > 0), so `TwoWallAdjust` projects the rest of
/// the slide onto the second wall and the pawn follows it, moving away from
/// the first wall (at 90° the crease is vertical and nothing would move).
#[test]
fn walking_into_an_obtuse_corner_follows_the_second_wall() {
    let params = PlayerParams::default();
    let speed = params.movement.max_ground_speed.value;
    let rm = move_radius(&params);
    let n_a = Vec3::NEG_X;
    let n_b = Vec3::new(-0.8, -0.6, 0.0);
    assert!(n_a.dot(n_b) > 0.0);
    let world = SlopeWorld::new(BoxWorld::new().with_ground(0.0, false))
        .with_half_space(HalfSpace::through_point(n_a, Vec3::new(100.0, 0.0, 0.0), false).unwrap())
        .with_half_space(
            HalfSpace::through_point(n_b, Vec3::new(100.0, 60.0, 0.0), false).unwrap(),
        );
    // Where the centre of the move's box (half width rm) may be: a box rests
    // against a plane with its nearest corner, rm·(|n.x| + |n.y|) from it
    // (spec 10.1). Wall A: x ≤ 100 − rm. Wall B: 0.8·x + 0.6·y ≤ 116 − 1.4·rm.
    let rm = f64::from(rm);
    let a_limit = 100.0 - rm;
    let b_limit = 116.0 - 1.4 * rm;
    let dir = Vec3::new(0.6, 0.8, 0.0);
    // Start 1 uu from wall A and 2.5 uu (along its normal) from wall B.
    let x0 = a_limit - 1.0;
    let y0 = (b_limit - 2.5 - 0.8 * x0) / 0.6;
    let p0 = Vec3::new(x0 as f32, y0 as f32, hh(&params) + HOVER);
    let mut s = walking_at(p0, Vec3::Z, true, false);
    s.velocity = dir * speed;
    tick(&mut s, towards(dir), &params, &world, DT);
    let p = s.position;

    // The same tick from the rules (3.2, 3.2.1, 4.8), in f64. `to_b` is the
    // distance left to wall B along its normal.
    let nb = d(n_b);
    let delta = d(dir) * f64::from(speed) * f64::from(DT);
    let mut at = DVec3::new(x0, y0, 0.0);
    let to_b = |at: DVec3| b_limit - (0.8 * at.x + 0.6 * at.y);
    // 1. The move: wall A first.
    let t1 = reported((a_limit - at.x) / delta.x, delta);
    assert!((a_limit - at.x) / delta.x < to_b(at) / -delta.dot(nb));
    at += delta * t1;
    // 2. Step up and across with the rest: wall A again.
    let rest = delta * (1.0 - t1);
    let t2 = reported((a_limit - at.x) / rest.x, rest);
    assert!((a_limit - at.x) / rest.x < to_b(at) / -rest.dot(nb));
    at += rest * t2;
    // 3. Slide along A (its normal is already horizontal): this meets B.
    let slide = DVec3::new(0.0, rest.y, 0.0) * (1.0 - t2);
    let raw3 = to_b(at) / -slide.dot(nb);
    assert!(raw3 < 1.0, "the slide reaches wall B");
    let t3 = reported(raw3, slide);
    at += slide * t3;
    // 4. TwoWallAdjust, walls at an obtuse angle: the rest of the slide
    // projected onto B. It runs along B, away from A, and meets nothing.
    let along_b = (slide - nb * slide.dot(nb)) * (1.0 - t3);
    assert!(along_b.x < 0.0 && along_b.y > 0.0 && along_b.dot(d(dir)) > 0.0);
    at += along_b;

    close(f64::from(p.x), at.x, 2e-3, "x");
    close(f64::from(p.y), at.y, 2e-3, "y");
    assert!(
        f64::from(p.x) < a_limit - 0.3,
        "moved away from wall A along B: {p}"
    );
    // Held off wall B by the pull-back of the slide, which met it at an
    // angle: pull(|slide|) along the slide is 0.6 of that along B's normal.
    close(
        b_limit - f64::from(0.8 * p.x + 0.6 * p.y),
        pull(slide.length()) * 0.6,
        2e-3,
        "distance to wall B",
    );
    assert!(s.grounded && (p.z - p0.z).abs() < 1e-3);
}

// ---------------------------------------------------------------------------
// 2.4 Velocity derived from a displacement: the float form.
//
// One recorded pair of the original (docs/PARITY_FINDINGS.md P8 / V17): in a
// tick of 0.01631380245089531 s a displacement of −14.2109375 uu gave the
// velocity −871.0989990234375 uu/s, which is the displacement times the f32
// reciprocal of the tick (61.297786712646484); dividing gives the next float,
// −871.0990600585938. The three tests below put that displacement through
// each movement mode. They work at a coordinate of 20000, where neighbouring
// f32 positions are 1/512 uu apart, and at a speed of 871.1 uu/s (f32
// 871.0999755859375): the move 871.0999755859375 × 0.01631380245089531 =
// 14.21095… uu then lands on the position 19985.7890625, and
// 19985.7890625 − 20000 = −14.2109375 exactly (0.2109375 = 108/512). The
// speeds, the cap and the drag set here are test inputs, not game values.
// ---------------------------------------------------------------------------

/// The tick length of the recorded pair (f32 bits 0x3C85A486).
fn recorded_dt() -> f32 {
    let dt = 0.01631380245089531_f64 as f32;
    assert_eq!(dt.to_bits(), 0x3C85_A486);
    dt
}

/// 871.1 uu/s: the f32 nearest to it is 871.0999755859375.
const RECORDED_PAIR_SPEED: f32 = 871.1;
/// The displacement of the recorded pair, uu.
const RECORDED_PAIR_MOVE: f32 = -14.2109375;
/// The velocity of the recorded pair, uu/s (f32 bits 0xC459C656).
const RECORDED_PAIR_VELOCITY: f64 = -871.0989990234375;
/// What dividing the displacement by the tick gives instead (0xC459C657).
const RECORDED_PAIR_DIVIDED: f64 = -871.0990600585938;

#[test]
fn walking_velocity_is_the_displacement_times_the_f32_reciprocal_of_dt() {
    // 3.1 with 2.4. Walking at the speed cap with the input along the
    // velocity: the turning-friction term is exactly zero, the acceleration
    // pushes the speed over the cap and the cap brings it back to
    // GroundSpeed (to within the rounding of the normalisation, far inside
    // the 1/1024 uu that would change the position reached).
    let dt = recorded_dt();
    let mut params = PlayerParams::default();
    params.movement.max_ground_speed.value = RECORDED_PAIR_SPEED;
    assert_eq!(params.movement.movement_speed_modifier.value, 1.0);
    let world = BoxWorld::new().with_ground(0.0, false);
    let start = Vec3::new(20000.0, 0.0, hh(&params) + 2.15);
    let mut s = walking_at(start, Vec3::Z, true, false);
    s.velocity = Vec3::new(-RECORDED_PAIR_SPEED, 0.0, 0.0);
    let (_, stats) = tick(&mut s, towards(Vec3::NEG_X), &params, &world, dt);
    assert_modes(&stats, &[(PhysicsMode::Walking, dt)]);
    assert!(s.grounded);
    assert_eq!(s.position.x - start.x, RECORDED_PAIR_MOVE);
    assert_eq!(f64::from(s.velocity.x), RECORDED_PAIR_VELOCITY);
    assert_ne!(f64::from(s.velocity.x), RECORDED_PAIR_DIVIDED);
    assert_eq!(s.velocity.y, 0.0);
    assert_eq!(s.velocity.z, 0.0);
}

#[test]
fn falling_refinement_uses_the_f32_reciprocal_of_the_substep() {
    // 4.3 and 4.6 with 2.4, the analogous case for falling. Free fall
    // straight down with no input, one sub-step (dt ≤ 0.05):
    //   V1 = V0 + g·dt, the move is V1·dt, the average velocity is the
    //   displacement × (1/dt), and since V0.Z < 0 and the average is below
    //   it the result is 2·average − V0.
    // V0.Z is chosen so that V1.Z is the speed of the recorded pair:
    //   g·dt = −520 × 0.01631380245089531 = −8.4831772744…, in f32
    //   −8.483177185058594 (0xC107BB18); V0.Z = −862.6168 (f32
    //   −862.6168212890625, 0xC457A77A); their sum −871.0999984741… is in
    //   f32 −871.0999755859375, the f32 nearest to −871.1.
    // The displacement is therefore −14.2109375 and the average
    // −871.0989990234375, so
    //   V.Z = 2 × (−871.0989990234375) − (−862.6168212890625)
    //       = −879.5811767578125                  (exact, f32 0xC45BE532).
    // With the division form the average would be −871.09906005859375 and
    // the result −879.581298828125 (0xC45BE534), two floats away.
    let dt = recorded_dt();
    let params = PlayerParams::default();
    assert_eq!(gravity(&params), -520.0);
    assert!(params.movement.terminal_velocity.value > 880.0);
    let world = BoxWorld::new();
    let v0 = Vec3::new(0.0, 0.0, -862.6168);
    assert_eq!(v0.z.to_bits(), 0xC457_A77A);
    let start = Vec3::new(0.0, 0.0, 20000.0);
    let mut s = airborne(start, v0);
    let (events, stats) = tick(&mut s, no_input(), &params, &world, dt);
    assert_modes(&stats, &[(PhysicsMode::Falling, dt)]);
    assert!(!s.grounded && events.landed.is_none());
    assert_eq!(s.position.z - start.z, RECORDED_PAIR_MOVE);
    assert_eq!(f64::from(s.velocity.z), -879.5811767578125);
    assert_eq!(s.velocity.z.to_bits(), 0xC45B_E532);
    assert_ne!(f64::from(s.velocity.z), -879.581298828125);
    assert_eq!(s.velocity.x, 0.0);
    assert_eq!(s.velocity.y, 0.0);
    assert_eq!(s.position.x, 0.0);
    assert_eq!(s.position.y, 0.0);
}

#[test]
fn falling_average_after_wall_contact_uses_the_f32_reciprocal_of_the_substep() {
    // 4.5 step 4 with 2.4: after a wall contact the old horizontal velocity
    // is replaced by the average actually travelled, displacement × (1/dt),
    // before the refinement 2·average − old. Sideways (Y) nothing stops the
    // pawn, so its Y average is the reciprocal form of the recorded pair and
    // the refinement gives 2·a − a = a exactly: the Y velocity after the
    // tick is that average itself. (Without the replacement the result would
    // be 2·a − V0.Y, about −871.098; with the division form it would be
    // −871.0990600585938.) The wall only has to be hit: it faces −X, far
    // from the pawn's Y range, and the pawn moves into it at 100 uu/s.
    let dt = recorded_dt();
    let mut params = PlayerParams::default();
    // No air steering and no gravity: the only acceleration is zero.
    params.movement.custom_gravity_scaling.value = 0.0;
    let wall_x = 100.0;
    let world = BoxWorld::new().with_box(
        Vec3::new(wall_x, 19000.0, 0.0),
        Vec3::new(wall_x + 50.0, 21000.0, 2000.0),
        false,
    );
    // Half a unit from the wall face (the sweep radius is 1.001 × the
    // collision radius), so the 1.63 uu move into it is cut short.
    let start = Vec3::new(wall_x - move_radius(&params) - 0.5, 20000.0, 1000.0);
    let v0 = Vec3::new(100.0, -RECORDED_PAIR_SPEED, 0.0);
    let mut s = airborne(start, v0);
    let (_, stats) = tick(&mut s, no_input(), &params, &world, dt);
    assert_modes(&stats, &[(PhysicsMode::Falling, dt)]);
    assert!(s.position.x - start.x < 0.6, "stopped by the wall");
    assert_eq!(s.position.y - start.y, RECORDED_PAIR_MOVE);
    assert_eq!(f64::from(s.velocity.y), RECORDED_PAIR_VELOCITY);
    assert_eq!(s.velocity.z, 0.0);
}

#[test]
fn flying_velocity_is_the_displacement_times_the_f32_reciprocal_of_dt() {
    // G-PH-3 step 6 with 2.4. No drag (FluidFriction 0 makes both friction
    // factors exactly 1), no acceleration and a cap above the speed: the
    // velocity update leaves V unchanged, the move is V·dt and the velocity
    // is re-derived from the displacement.
    let dt = recorded_dt();
    let mut params = PlayerParams::default();
    params.movement.fluid_friction.value = 0.0;
    params.movement.air_speed.value = 2000.0;
    let world = BoxWorld::new();
    let start = Vec3::new(20000.0, 0.0, 500.0);
    let mut s = flying(start, Vec3::new(-RECORDED_PAIR_SPEED, 0.0, 0.0));
    let (_, stats) = tick(&mut s, no_input(), &params, &world, dt);
    assert_modes(&stats, &[(PhysicsMode::Flying, dt)]);
    assert!(s.pawn.flying && !s.grounded);
    assert_eq!(s.position.x - start.x, RECORDED_PAIR_MOVE);
    assert_eq!(f64::from(s.velocity.x), RECORDED_PAIR_VELOCITY);
    assert_ne!(f64::from(s.velocity.x), RECORDED_PAIR_DIVIDED);
    assert_eq!(s.velocity.y, 0.0);
    assert_eq!(s.velocity.z, 0.0);
    assert_eq!(s.position.z, start.z);
}

/// [`PawnHooks`] with the class defaults that keeps what the pawn's `Landed`
/// event is told (the velocity and location at the landing, before the rest
/// of the tick is walked).
struct LandingRecorder {
    defaults: ClassDefaults,
    landings: Vec<Landing>,
}

impl PawnHooks for LandingRecorder {
    fn ground_speed(&self) -> f32 {
        self.defaults.ground_speed
    }

    fn air_control(&self) -> f32 {
        self.defaults.air_control
    }

    fn air_speed(&self) -> f32 {
        self.defaults.air_speed
    }

    fn landed(&mut self, landing: &Landing) {
        self.landings.push(*landing);
    }
}

#[test]
fn landing_velocity_is_the_displacement_times_the_f32_reciprocal_of_the_contact_time() {
    // 4.4 with 2.4: a landing after `step·Hit.Time` > 0.003 s with
    // `Hit.Time` > 0.1 replaces the velocity by the displacement of the
    // sub-step × (1 / (step·Hit.Time)): one f32 reciprocal, three products
    // (Mac 0x100AE0747, Win32 0xB2899B). No recording shows this velocity
    // (the rest of the landing frame is walked), so the rule rests on the
    // native code; this test keeps the port on it.
    //
    // Nothing here is taken from the code under test except what it reports
    // about the landing: where the pawn was (`Landing::location`) and how
    // much time it handed back (`returned_time`, spec 1.4/5.2:
    // step·(1 − Hit.Time)). With a tick of 1/32 s (one sub-step, a power of
    // two) and `Hit.Time` ≥ 0.5 every operation that leads from the returned
    // time back to the contact time is exact in f32:
    //   returned · 32 = 1 − Hit.Time   (scaling by a power of two; the
    //                                   difference of 1 and a float in
    //                                   [0.5, 1] is exact)
    //   Hit.Time = 1 − returned · 32,  contact = Hit.Time / 32.
    // The expected velocity is then written out from the native rule. The
    // cases differ in height and speed so that the contact time and the
    // displacement change; the division form gives another float in some of
    // them, and the test requires that at least one case tells the two
    // forms apart. If a change of the contact rule (the hit pull-back) ever
    // leaves none, add cases; the expected value does not change.
    let step = 1.0_f32 / 32.0;
    let params = PlayerParams::default();
    let g = gravity(&params);
    let world = BoxWorld::new().with_ground(0.0, false);
    let mut decided = 0;
    for case in 0..12_u8 {
        let k = f32::from(case);
        let start = Vec3::new(0.0, 0.0, hh(&params) + 14.0 + 0.5 * k);
        let v0 = Vec3::new(150.0 + 10.0 * k, -37.5, -700.0 - 6.25 * k);
        let mut s = airborne(start, v0);
        let mut hooks = LandingRecorder {
            defaults: ClassDefaults::new(&params.movement),
            landings: Vec::new(),
        };
        let mut events = StepEvents::default();
        let stats = Ue3PawnMovement.advance_scripted_with_stats(
            &mut s,
            &no_input(),
            Vec3::ZERO,
            &params.movement,
            &mut hooks,
            &world,
            step,
            &mut events,
        );
        assert_eq!(hooks.landings.len(), 1, "case {case}: one landing");
        let landing = hooks.landings[0];
        let first = stats.substeps()[0];
        assert_eq!(
            (first.mode, first.time),
            (PhysicsMode::Falling, step),
            "case {case}"
        );

        // The contact time, from the time handed back.
        let untravelled = stats.returned_time * 32.0;
        assert!(
            untravelled > 0.0 && untravelled <= 0.5,
            "case {case}: Hit.Time must be in [0.5, 1) for the exact derivation, 1 - t = {untravelled}"
        );
        let hit_time = 1.0 - untravelled;
        let contact = step * hit_time;
        assert!(contact > 0.003 && hit_time > 0.1, "case {case}");
        // The landing location is the start plus that fraction of the
        // sub-step's move (4.3: V1 = V0 + g·step, move V1·step).
        let moved = landing.location - start;
        let full_move_z = (f64::from(v0.z) + g * f64::from(step)) * f64::from(step);
        close(
            f64::from(moved.z),
            full_move_z * f64::from(hit_time),
            1e-4,
            "fraction of the move",
        );

        let reciprocal = 1.0 / contact;
        let expected = [
            moved.x * reciprocal,
            moved.y * reciprocal,
            moved.z * reciprocal,
        ];
        let divided = [moved.x / contact, moved.y / contact, moved.z / contact];
        assert_eq!(
            landing.velocity.to_array().map(f32::to_bits),
            expected.map(f32::to_bits),
            "case {case}: landing velocity {:?}, reciprocal form {expected:?}, division form {divided:?}",
            landing.velocity
        );
        assert_eq!(events.landed, Some(landing.velocity.z), "case {case}");
        if expected.map(f32::to_bits) != divided.map(f32::to_bits) {
            decided += 1;
        }
    }
    assert!(
        decided >= 1,
        "no case tells the reciprocal form from the division form: add cases"
    );
}

// ---------------------------------------------------------------------------
// 2.1 and 2.3: the order of the multiplications inside the velocity update.
//
// Both executables multiply in the same order (NATIVE_PHYSICS.md 2.1, 2.3):
// the turning friction is (difference · dt) · friction, and a braking piece
// adds (v · h) · (1 / dt) to the average. No recording can tell these from
// the other orders (the difference never reaches a recorded position), so
// the expected values are written out from the native rule in f32, one
// operation at a time, with the value another order would give beside them.
// The tick is the one of the recorded pair above (f32 bits 0x3C85A486).
// ---------------------------------------------------------------------------

#[test]
fn turning_friction_multiplies_by_the_time_before_the_friction() {
    use asamu_player::ue3_movement::udk_calc_velocity_with;
    let dt = recorded_dt();
    let params = PlayerParams::asamu_original();

    // The flying call (GRAPPLE.md G-PH-3): no input, fluid friction, no
    // braking, friction 0.5 × FluidFriction = 0.15000000596046448
    // (0x3E19999A), cap 2000. With no input the difference is the velocity:
    //   V ← V − (V·dt)·F, then V ← V·(1 − F·dt) with 1 − F·dt = 0x3F7F5FA1.
    // The speed, 1981.6 uu/s, is below the cap.
    //   component   V − (V·dt)·F   × (1 − F·dt)   with (V·F)·dt instead
    //   792         0x444583F7     0x4445083B     0x4445083A
    //   −889        0xC45DB4C5     0xC45D29E2     0xC45D29E3
    //   1584        0x44C583F7     0x44C5083B     0x44C5083A
    assert_eq!(params.movement.fluid_friction.value, 0.3);
    let friction = 0.5 * params.movement.fluid_friction.value;
    assert_eq!(friction.to_bits(), 0x3E19_999A);
    let r = udk_calc_velocity_with(
        Vec3::new(792.0, -889.0, 1584.0),
        Vec3::ZERO,
        2048.0,
        dt,
        2000.0,
        friction,
        true,
        false,
    );
    assert_eq!(
        r.velocity.to_array().map(f32::to_bits),
        [0x4445_083B, 0xC45D_29E2, 0x44C5_083B],
        "{:?}",
        r.velocity
    );
    assert_eq!(r.acceleration, Vec3::ZERO);

    // The walking call with an input: V = (110, 0, 0), input +Y, acceleration
    // 2048, cap 1000, friction 6. The friction is a test input: the game's
    // ground friction, 8, is a power of two, and multiplying by a power of
    // two is exact, so there the order cannot matter.
    //   |V| = 110, difference = V − input·|V| = (110, −110, 0)
    //   X: 110 − (110·dt)·6 = 0x42C6773E (99.23289489746094);
    //      110 − (110·6)·dt would be 0x42C6773D
    //   Y: 0 − (−110·dt)·6 = 0x412C4614 (0x412C4615 in the other order),
    //      + 2048·dt = 0x4230B60B (44.17777633666992)
    assert_eq!(params.movement.ground_friction.value, 8.0);
    let r = udk_calc_velocity_with(
        Vec3::new(110.0, 0.0, 0.0),
        Vec3::Y,
        2048.0,
        dt,
        1000.0,
        6.0,
        false,
        true,
    );
    assert_eq!(
        r.velocity.to_array().map(f32::to_bits),
        [0x42C6_773E, 0x4230_B60B, 0],
        "{:?}",
        r.velocity
    );
    assert_eq!(r.acceleration, Vec3::new(0.0, 2048.0, 0.0));
}

#[test]
fn braking_average_adds_each_piece_times_the_f32_reciprocal_of_dt() {
    use asamu_player::ue3_movement::apply_velocity_braking;
    let params = PlayerParams::asamu_original();
    let friction = params.movement.ground_friction.value;
    assert_eq!(friction, 8.0);
    let start = Vec3::new(250.0, -500.0, 0.0);

    // One piece (dt ≤ 0.03):
    //   V1 = V0 − ((V0 + V0)·dt)·8 = (0x4338BEAA, 0xC3B8BEAA)
    //      = (184.74478149414062, −369.48956298828125)
    //   average = (V1·dt)·(1/dt), 1/dt = 0x427530EF:
    //      (0x4338BEA9, 0xC3B8BEA9) = (184.74476623535156, −369.4895324707031)
    // V1·(dt/dt) would be V1 itself, the next float in both components.
    let v = apply_velocity_braking(start, recorded_dt(), friction);
    assert_eq!(
        v.to_array().map(f32::to_bits),
        [0x4338_BEA9, 0xC3B8_BEA9, 0],
        "{v:?}"
    );

    // Two pieces: dt = 1/30 s (0x3D088889) is 0.03 s and then
    // 0.003333335742354393 s (0x3B5A7418); 1/dt = 0x41EFFFFF.
    //   V1 = V0 − ((V0 + V0)·0.03)·8 = (130, −260)
    //   V2 = V1 − ((V1 + V1)·h2)·8   = (0x42F62222, 0xC3762222)
    //   average = (V1·0.03)·(1/dt) + (V2·h2)·(1/dt)
    //      = (0x43014E81, 0xC3814E81) = (129.30665588378906, −258.6133117675781)
    // V1·(0.03/dt) + V2·(h2/dt) would be (0x43014E82, 0xC3814E82).
    let dt = 1.0_f32 / 30.0;
    assert_eq!(dt.to_bits(), 0x3D08_8889);
    let v = apply_velocity_braking(start, dt, friction);
    assert_eq!(
        v.to_array().map(f32::to_bits),
        [0x4301_4E81, 0xC381_4E81, 0],
        "{v:?}"
    );
}

// ---------------------------------------------------------------------------
// physFlying (GRAPPLE.md G-PH-3): the mode the grapple uses.
// ---------------------------------------------------------------------------

fn flying(position: Vec3, velocity: Vec3) -> PlayerState {
    let mut s = airborne(position, velocity);
    s.pawn.flying = true;
    s
}

#[test]
fn flying_is_one_update_with_fluid_drag_the_air_speed_cap_and_no_gravity() {
    // G-PH-3 steps 1–4, 6: one update per tick even for long frames (no
    // sub-steps), V·(1 − F·dt)² with F = 0.5 × FluidFriction, the 3-D cap at
    // AirSpeed (here the class default 440 through `ClassDefaults`), no
    // gravity, velocity re-derived from the displacement.
    let params = PlayerParams::asamu_original();
    let world = BoxWorld::new();
    let f = 0.5 * f64::from(params.movement.fluid_friction.value);
    for dt in [DT, 0.2] {
        let v0 = Vec3::new(300.0, -100.0, 50.0);
        let mut s = flying(Vec3::new(0.0, 0.0, 500.0), v0);
        let (_, stats) = tick(&mut s, no_input(), &params, &world, dt);
        assert_modes(&stats, &[(PhysicsMode::Flying, dt)]);
        assert_accounting(&stats, dt);
        let k = (1.0 - f * f64::from(dt)).powi(2);
        let expected = d(v0) * k;
        assert!(
            (d(s.velocity) - expected).length() < 1e-3,
            "{dt}: {} vs {expected}",
            s.velocity
        );
        assert!(s.pawn.flying && !s.grounded);
        let moved = d(s.position) - DVec3::new(0.0, 0.0, 500.0);
        assert!(
            (moved - expected * f64::from(dt)).length() < 1e-2,
            "{moved}"
        );
    }
    // The cap: 3000 uu/s is cut to AirSpeed.
    let mut s = flying(Vec3::new(0.0, 0.0, 500.0), Vec3::new(0.0, 3000.0, 0.0));
    tick(&mut s, no_input(), &params, &world, DT);
    close(
        f64::from(s.speed()),
        f64::from(params.movement.air_speed.value),
        1e-2,
        "cap",
    );
}

#[test]
fn flying_never_lands_and_slides_along_floors_and_walls() {
    // G-PH-3 step 5: a blocking hit slides along the surface; velocity into
    // it is lost; there is no landing while flying.
    let params = PlayerParams::asamu_original();
    let world = BoxWorld::new().with_ground(0.0, false);
    let z = hh(&params) + 1.0;
    let v0 = DVec3::new(300.0, 0.0, -300.0);
    let mut s = flying(Vec3::new(0.0, 0.0, z), v0.as_vec3());
    for _ in 0..10 {
        let (e, _) = tick(&mut s, no_input(), &params, &world, DT);
        assert!(e.landed.is_none());
        assert!(s.pawn.flying && !s.grounded);
    }
    // The same ten ticks from the rules, in f64: drag (1 − F·dt)² with
    // F = 0.5 × FluidFriction (the speed stays under the AirSpeed cap), one
    // move, and where the floor is met the move stops at the reported hit
    // and the rest slides level, so the velocity re-derived from the
    // displacement keeps only that fraction of the speed into the floor.
    // The pawn never touches the floor: each contact leaves it the
    // pull-back short, and what speed is left closes the rest ever slower.
    let h = f64::from(DT);
    let f = 0.5 * f64::from(params.movement.fluid_friction.value);
    assert!(v0.length() < f64::from(params.movement.air_speed.value));
    let (mut gap, mut v, mut contacts) = (1.0_f64, v0, 0);
    for _ in 0..10 {
        v *= (1.0 - f * h).powi(2);
        let delta = v * h;
        if -delta.z >= gap {
            let t = reported(gap / -delta.z, delta);
            gap += delta.z * t;
            v.z = delta.z * t / h;
            contacts += 1;
        } else {
            gap += delta.z;
        }
    }
    assert_eq!(contacts, 3);
    assert!(gap > 0.0 && gap < 0.05, "{gap}");
    assert!(v.z < 0.0 && v.z > -1.0, "{}", v.z);
    close(f64::from(s.position.z - hh(&params)), gap, 1e-4, "gap");
    close(f64::from(s.velocity.z), v.z, 2e-3, "vz");
    close(f64::from(s.velocity.x), v.x, 1e-2, "slides along");
}

#[test]
fn flying_steps_up_only_near_vertical_walls_while_moving_roughly_horizontally() {
    // G-PH-3 step 5: |N.z| < 0.2 and −0.2 < (0,0,−1)·unit(V) < 0.5 → stepUp
    // with the rest of the move; the gained height is left out of the
    // re-derived velocity. Moving steeply down (u ≥ 0.5) → slide instead.
    let params = PlayerParams::asamu_original();
    let world = BoxWorld::new().with_box(
        Vec3::new(50.0, -500.0, -1000.0),
        Vec3::new(150.0, 500.0, 0.0),
        false,
    );
    // Centre 10 uu below the ledge top (within MaxStepHeight + 2), moving
    // horizontally at the class-default AirSpeed cap into the wall.
    let z0 = hh(&params) - 10.0;
    let mut s = flying(Vec3::new(25.0, 0.0, z0), Vec3::new(440.0, 0.0, 0.0));
    tick(&mut s, no_input(), &params, &world, DT);
    assert!(
        s.position.z > z0 + 5.0,
        "raised onto the ledge: {}",
        s.position
    );
    assert!(
        s.position.x > 50.0 - radius(&params),
        "moved on over it: {}",
        s.position
    );
    assert!(
        s.velocity.z.abs() < 1.0,
        "step height excluded from V: {}",
        s.velocity
    );
    // Steep downward motion into the same wall (u ≥ 0.5): no step-up, a
    // slide down the wall.
    let mut steep = flying(Vec3::new(25.0, 0.0, z0), Vec3::new(200.0, 0.0, -380.0));
    tick(&mut steep, no_input(), &params, &world, DT);
    assert!(steep.position.z < z0, "no step up: {}", steep.position);
    assert!(
        steep.position.x < 50.0 - radius(&params) + 0.01,
        "{}",
        steep.position
    );
}

#[test]
fn flying_walking_falling_mode_comes_from_the_flying_flag() {
    // A flying pawn with `grounded` set (inconsistent input) still flies.
    let params = PlayerParams::asamu_original();
    let world = BoxWorld::new();
    let mut s = flying(Vec3::new(0.0, 0.0, 500.0), Vec3::new(0.0, 0.0, 0.0));
    s.grounded = true;
    let (_, stats) = tick(&mut s, no_input(), &params, &world, DT);
    assert_modes(&stats, &[(PhysicsMode::Flying, DT)]);
    assert!(!s.grounded && s.pawn.flying);
}
