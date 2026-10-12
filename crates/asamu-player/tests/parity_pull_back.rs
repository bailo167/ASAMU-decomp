//! The original reports every hit short of the contact, and sweeps the pawn
//! as a box: `docs/PARITY_FINDINGS.md` P3 and P4, on the simulation's side.
//!
//! Every expected value is derived here from the native rule (read in the
//! user's executable: NATIVE_PHYSICS.md 10.2, V2 of the findings) and the
//! walking and falling rules of NATIVE_PHYSICS.md 3 and 4, with the
//! derivation in a comment. None is the output of the code under test, and
//! none is a recorded number: where a recording has the same quantity it is
//! named beside the derived value.
//!
//! The rule, for a trace with the pawn's extent: a hit is reported 0.1 uu
//! short of the contact when the trace is shorter than 1 uu, else a tenth of
//! the trace's length short, but at most K uu: K = 1.0 for BSP, brushes and a
//! mesh's simple-collision body, K = 4.0 for a mesh's triangles.

use asamu_player::movement::LocomotionIntent;
use asamu_player::ue3_movement::{MOVE_RADIUS_INFLATION, PawnPhysicsState, Ue3PawnMovement};
use asamu_player::world::{KindWorld, PrimitiveKind};
use asamu_player::{
    BoxWorld, CollisionWorld, HalfSpace, PlayerParams, PlayerState, SlopeWorld, StepEvents,
};
use glam::Vec3;

/// The player's collision cylinder (DEFAULTS.md; `PlayerParams::asamu_original`).
const RADIUS: f32 = 21.0;
const HALF_HEIGHT: f32 = 44.0;
/// The walking floor check is `MaxStepHeight + 2` long: 26 + 2.
const FLOOR_CHECK: f32 = 28.0;
/// What the walking floor check aims at, as a reported distance (spec 3.3).
const REPORTED_HOVER: f32 = 2.15;

fn original() -> PlayerParams {
    let params = PlayerParams::asamu_original();
    assert_eq!(params.movement.capsule_radius.value, RADIUS);
    assert_eq!(params.movement.capsule_half_height.value, HALF_HEIGHT);
    assert_eq!(params.movement.step_height.value + 2.0, FLOOR_CHECK);
    params
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
) -> StepEvents {
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
    assert!(stats.substeps().len() <= 8);
    events
}

/// A walking pawn at `position` on a base, floor normal +Z; `force` makes
/// the next walking sub-step check the floor even without a move.
fn walking_at(position: Vec3, force: bool) -> PlayerState {
    let mut s = PlayerState::new(position, 0.0);
    s.grounded = true;
    s.pawn = PawnPhysicsState {
        floor: Vec3::Z,
        based: true,
        force_floor_check: force,
        ..PawnPhysicsState::default()
    };
    s
}

fn close(got: f32, want: f32, tol: f32, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: {got} vs {want}");
}

/// A flat floor at `z`, every hit of it answering as `kind`.
fn floor(z: f32, kind: PrimitiveKind) -> KindWorld<BoxWorld> {
    KindWorld::new(BoxWorld::new().with_ground(z, false), kind)
}

// ---------------------------------------------------------------------------
// The floor check: how high the pawn stands.
// ---------------------------------------------------------------------------

/// The Workshop's BSP floor is at z 72 and the original's pawn stands with
/// its centre at 119.150 on 1,817 of 1,817 records there (V18).
///
/// Derivation: the 28 uu floor check is longer than 10 uu, so a BSP floor is
/// reported min(0.1 × 28, 1.0) = 1.0 uu nearer than it is. The pawn is moved
/// until the reported distance is 2.15, so its bottom is a true 3.15 above
/// the floor and its centre at 72 + 44 + 2.15 + 1.0 = 119.15. A brush and a
/// mesh's simple-collision body have the same K.
#[test]
fn a_bsp_floor_at_72_holds_the_centre_at_119_15() {
    let params = original();
    for kind in [
        PrimitiveKind::Bsp,
        PrimitiveKind::BrushHull,
        PrimitiveKind::SimpleHull,
    ] {
        let world = floor(72.0, kind);
        // From above (reported 10 − 1 = 9, beyond 2.4: moved down by 6.85)
        // and from below (reported 2 − 1 = 1, under 1.9: moved up by 1.15).
        for gap in [10.0_f32, 2.0] {
            let mut s = walking_at(Vec3::new(131.0, -339.0, 72.0 + HALF_HEIGHT + gap), true);
            tick(&mut s, no_input(), &params, &world, 1.0 / 60.0);
            assert!(s.grounded && s.pawn.based);
            close(s.position.z, 119.15, 1e-4, "centre");
            // And it stays there while walking.
            for _ in 0..30 {
                tick(&mut s, towards(Vec3::X), &params, &world, 1.0 / 60.0);
                close(s.position.z, 119.15, 1e-4, "centre, walking");
            }
        }
    }
}

/// On a mesh's triangles K is 4.0: the 28 uu check reports the floor
/// min(0.1 × 28, 4.0) = 2.8 uu nearer, the bottom is a true 2.15 + 2.8 = 4.95
/// above it (measured under the box's footprint: 4.954, sd 0.113, n 23, P4)
/// and the centre at 72 + 44 + 4.95 = 120.95.
#[test]
fn a_triangle_floor_at_72_holds_the_centre_at_120_95() {
    let params = original();
    let world = floor(72.0, PrimitiveKind::TriangleMesh);
    for gap in [10.0_f32, 4.0] {
        let mut s = walking_at(Vec3::new(131.0, -339.0, 72.0 + HALF_HEIGHT + gap), true);
        tick(&mut s, no_input(), &params, &world, 1.0 / 60.0);
        close(s.position.z, 120.95, 1e-4, "centre");
        for _ in 0..30 {
            tick(&mut s, towards(Vec3::X), &params, &world, 1.0 / 60.0);
            close(s.position.z, 120.95, 1e-4, "centre, walking");
        }
    }
    // The band 1.9 to 2.4 is a band of reported distances: on triangles a
    // pawn anywhere from 4.7 to 5.2 above the floor is left where it is (the
    // standing gaps measured on per-triangle floors: 4.72 to 5.17, V7).
    for gap in [4.72_f32, 5.17] {
        let z = 72.0 + HALF_HEIGHT + gap;
        let mut s = walking_at(Vec3::new(0.0, 0.0, z), true);
        tick(&mut s, towards(Vec3::X), &params, &world, 1.0 / 60.0);
        close(s.position.z, z, 1e-5, "inside the band");
    }
}

// ---------------------------------------------------------------------------
// A blocked move.
// ---------------------------------------------------------------------------

/// A pawn drifting level through the air (no gravity: a test input) into a
/// wall it meets square on. The move is one sweep; the slide that follows a
/// head-on hit is zero. Returns how far from the wall the move leaves the
/// pawn's box, for a move `length` uu long that starts `gap` uu from it.
fn gap_after_one_move(kind: PrimitiveKind, gap: f32, length: f32) -> f32 {
    let mut params = original();
    params.movement.custom_gravity_scaling.value = 0.0;
    let dt = 1.0 / 64.0;
    // The move's box is 1.001 × 21 wide (`UWorld::MoveActor`); with the wall
    // face at −21.021 the box touches it when the centre is at x = 0, so
    // the centre's x is the gap.
    let face = -RADIUS * MOVE_RADIUS_INFLATION;
    let world = KindWorld::new(
        BoxWorld::new().with_box(
            Vec3::new(face - 50.0, -500.0, -500.0),
            Vec3::new(face, 500.0, 500.0),
            false,
        ),
        kind,
    );
    let mut s = PlayerState::new(Vec3::new(gap, 0.0, 0.0), 0.0);
    s.velocity = Vec3::new(-length / dt, 0.0, 0.0);
    tick(&mut s, no_input(), &params, &world, dt);
    assert!(!s.grounded);
    assert_eq!(s.position.y, 0.0);
    assert_eq!(s.position.z, 0.0);
    s.position.x
}

/// A blocked 7 uu move stops a tenth of its length, 0.7 uu, short of the
/// wall; a blocked 0.5 uu move, being shorter than 1 uu, stops 0.1 uu short.
/// Neither reaches a cap, so the kind makes no difference; a 30 uu move is
/// capped at K.
#[test]
fn a_blocked_move_stops_a_tenth_of_its_length_short() {
    for kind in [
        PrimitiveKind::Bsp,
        PrimitiveKind::BrushHull,
        PrimitiveKind::SimpleHull,
        PrimitiveKind::TriangleMesh,
    ] {
        // 7 uu from 3 uu away: 0.7 short.
        close(gap_after_one_move(kind, 3.0, 7.0), 0.7, 1e-5, "7 uu");
        // 0.5 uu from 0.3 uu away: 0.1 short.
        close(gap_after_one_move(kind, 0.3, 0.5), 0.1, 1e-5, "0.5 uu");
        // 2.266 uu from 0.4189 uu away (the first tick of the wall contact
        // below, as a single sweep): 0.2266 short.
        close(
            gap_after_one_move(kind, 0.4189, 2.266),
            0.2266,
            1e-5,
            "2.266 uu",
        );
        // Already nearer than the pull-back: reported at time 0, no move.
        close(gap_after_one_move(kind, 0.25, 7.0), 0.25, 1e-6, "held");
        // A move that does not reach the wall is not shortened.
        close(gap_after_one_move(kind, 8.0, 7.0), 1.0, 1e-5, "free");
    }
    // 30 uu from 20 uu away: a tenth would be 3.0; K caps it at 1.0, on a
    // mesh's triangles K = 4.0 does not.
    close(
        gap_after_one_move(PrimitiveKind::Bsp, 20.0, 30.0),
        1.0,
        1e-5,
        "capped at 1.0",
    );
    close(
        gap_after_one_move(PrimitiveKind::TriangleMesh, 20.0, 30.0),
        3.0,
        1e-5,
        "under the cap of 4.0",
    );
}

// ---------------------------------------------------------------------------
// Walking into a wall.
// ---------------------------------------------------------------------------

/// The Workshop door, WS1 ticks 1562 to 1565 (V11): the pawn walks square
/// into a wall from 0.4189 uu away with attempted moves of 2.266, 0.788 and
/// 0.662 uu. Recorded: it advances 0.2114 uu, then 0.1074 uu, then stops,
/// 0.0999 uu from the contact plane.
///
/// Derivation (3.1, 3.2, 3.2.1 with the pull-back; all lengths in uu):
///
/// - Tick 1, move 2.266. The wall is met after 0.4189; the hit is reported a
///   tenth of 2.266 short, so the pawn advances 0.4189 − 0.2266 = 0.1923 and
///   is 0.2266 away. Walking then steps up (28 uu), tries the rest of the
///   move, 2.266 − 0.1923 = 2.0737, against the same wall: reported a tenth
///   of 2.0737 short, it advances 0.2266 − 0.2074 = 0.0192 and is 0.2074
///   away. The slide along the wall is zero (head-on); step down.
///   Advance of the tick: 0.4189 − 0.2074 = 0.2115 (recorded 0.2114).
/// - Tick 2, move 0.788, shorter than 1: reported 0.1 short. It advances
///   0.2074 − 0.1 = 0.1074 (recorded 0.1074) and is 0.1 away. The step-up's
///   retry, 0.6806 long, is reported 0.1 short of a wall 0.1 away: no move.
/// - Tick 3, move 0.662: the same, no move. It rests 0.1000 away (recorded
///   0.0999).
#[test]
fn walking_into_a_wall_leaves_0_2074_then_0_1() {
    let mut params = original();
    let dt = 1.0 / 64.0;
    let face = -RADIUS * MOVE_RADIUS_INFLATION;
    // The floor is BSP, like the Workshop's, and so is the wall here: for
    // moves shorter than 10 uu every kind gives the same distances.
    let world = BoxWorld::new()
        .with_ground(-HALF_HEIGHT - 3.15, false)
        .with_box(
            Vec3::new(face - 50.0, -500.0, -500.0),
            Vec3::new(face, 500.0, 500.0),
            false,
        );
    // Standing at the hover (2.15 + 1.0 above the floor), centre at z = 0.
    let mut s = walking_at(Vec3::new(0.4189, 0.0, 0.0), false);
    let mut gaps = Vec::new();
    for attempted in [2.266_f32, 0.788, 0.662] {
        // A pawn already at its speed cap with the input along its velocity
        // attempts exactly cap × dt (2.1: the turning friction is zero and
        // the cap cuts the acceleration off).
        let speed = attempted / dt;
        params.movement.max_ground_speed.value = speed;
        s.velocity = Vec3::new(-speed, 0.0, 0.0);
        let before = s.position.x;
        tick(&mut s, towards(Vec3::NEG_X), &params, &world, dt);
        assert!(s.grounded);
        close(s.position.z, 0.0, 1e-4, "height kept through the step-up");
        // Walking velocity is the displacement over the tick (3.1).
        close(s.velocity.x, (s.position.x - before) / dt, 1e-3, "velocity");
        gaps.push(s.position.x);
    }
    close(gaps[0], 0.2074, 1e-4, "after tick 1");
    close(0.4189 - gaps[0], 0.2115, 1e-4, "advance of tick 1");
    close(gaps[1], 0.1, 1e-5, "after tick 2");
    close(gaps[0] - gaps[1], 0.1074, 1e-4, "advance of tick 2");
    close(gaps[2], 0.1, 1e-5, "after tick 3");
    assert_eq!(gaps[2], gaps[1], "no move in tick 3");
}

// ---------------------------------------------------------------------------
// Landing.
// ---------------------------------------------------------------------------

/// A landing whose last falling move is 4.66 uu long, on a mesh's triangles
/// (the Darkcave start: the rest after the landing is 2.62 uu above the
/// triangles, V7).
///
/// Derivation: the falling move meets the floor and is reported a tenth of
/// its length short: the pawn stops 0.466 uu above the floor and lands
/// (4.4). The rest of the tick is walked; landing forces a floor check
/// (5.2), which reports the floor min(2.8, 4.0) = 2.8 uu nearer, clamped to
/// a distance of 0; that is under 1.9, so the pawn is moved up by
/// 2.15 − 0 (3.3): 0.466 + 2.15 = 2.616. Standing still it stays (the check
/// is skipped without a move). The next moving tick checks again: reported
/// 2.616 − 2.8, clamped to 0, up by 2.15 again: 4.766. Now the floor is
/// reported at 1.966, inside 1.9 to 2.4, and the pawn stays at 4.766: it
/// reaches the band in stages and does not go on to 4.95.
#[test]
fn a_landing_on_triangles_rests_at_2_616_and_then_at_4_766() {
    let params = original();
    let world = floor(0.0, PrimitiveKind::TriangleMesh);
    let dt = 1.0_f32 / 64.0;
    let g = params.movement.world_gravity_z.value * params.movement.custom_gravity_scaling.value;
    // The falling move is (V0 + g·dt)·dt (4.3): choose V0 for 4.66 uu.
    let v0 = -4.66 / dt - g * dt;
    let start_gap = 3.0;
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, HALF_HEIGHT + start_gap), 0.0);
    s.velocity = Vec3::new(0.0, 0.0, v0);
    let events = tick(&mut s, no_input(), &params, &world, dt);
    assert!(events.landed.is_some() && s.grounded);
    close(
        s.position.z - HALF_HEIGHT,
        0.466 + REPORTED_HOVER,
        1e-4,
        "after landing",
    );
    close(s.position.z - HALF_HEIGHT, 2.616, 1e-4, "after landing");
    // Standing still: no floor check, no change.
    tick(&mut s, no_input(), &params, &world, dt);
    close(s.position.z - HALF_HEIGHT, 2.616, 1e-4, "standing");
    // The next moving tick.
    tick(&mut s, towards(Vec3::X), &params, &world, dt);
    close(
        s.position.z - HALF_HEIGHT,
        4.766,
        1e-4,
        "after the next moving tick",
    );
    // And it stays: 4.766 − 2.8 = 1.966 is inside the band.
    for _ in 0..20 {
        tick(&mut s, towards(Vec3::X), &params, &world, dt);
        close(s.position.z - HALF_HEIGHT, 4.766, 1e-4, "in the band");
    }

    // The same landing on BSP (K = 1.0): 2.616 after the landing as well
    // (0.466 is reported at 0 there too); the next moving tick sees
    // 2.616 − 1.0 = 1.616, under 1.9, and moves up to a reported 2.15:
    // 3.15.
    let world = floor(0.0, PrimitiveKind::Bsp);
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, HALF_HEIGHT + start_gap), 0.0);
    s.velocity = Vec3::new(0.0, 0.0, v0);
    let events = tick(&mut s, no_input(), &params, &world, dt);
    assert!(events.landed.is_some() && s.grounded);
    close(
        s.position.z - HALF_HEIGHT,
        2.616,
        1e-4,
        "after landing, BSP",
    );
    tick(&mut s, towards(Vec3::X), &params, &world, dt);
    close(
        s.position.z - HALF_HEIGHT,
        3.15,
        1e-4,
        "next moving tick, BSP",
    );
}

// ---------------------------------------------------------------------------
// The shape.
// ---------------------------------------------------------------------------

/// On a slope the pawn rests where a box does, not where a cylinder would.
///
/// The floor rises 0.3 per uu along +X and 0.4 per uu along +Y (walkable:
/// its normal's Z is 1/sqrt(1.25) = 0.894). A box of half width 21 touches it
/// with the corner at (−21, −21) from its centre, where the floor is
/// 21·(0.3 + 0.4) = 14.7 uu lower than under the centre; a cylinder of
/// radius 21 would touch 21·sqrt(0.3² + 0.4²) = 10.5 uu lower. The floor
/// check moves straight down, so the pull-back (1.0 uu on BSP) and the
/// reported 2.15 are heights: the centre is 44 + 14.7 + 3.15 = 61.85 above
/// the floor under it (the cylinder: 57.65).
#[test]
fn on_a_slope_the_pawn_rests_on_the_corner_of_its_box() {
    let params = original();
    let plane = HalfSpace::through_point(Vec3::new(-0.3, -0.4, 1.0), Vec3::ZERO, false)
        .expect("valid plane");
    assert!(plane.normal.z >= params.movement.walkable_floor_z.value);
    // No slope slide on this floor (3.5 needs Floor.Z · GroundFriction < 3.3).
    assert!(plane.normal.z * params.movement.ground_friction.value >= 3.3);
    let world = SlopeWorld::new(BoxWorld::new()).with_half_space(plane);
    for (x, y) in [(0.0_f32, 0.0_f32), (40.0, -25.0)] {
        let under = 0.3 * x + 0.4 * y;
        for gap in [8.0_f32, 1.5] {
            let start = Vec3::new(x, y, under + HALF_HEIGHT + 14.7 + gap);
            let mut s = walking_at(start, true);
            s.pawn.floor = plane.normal;
            tick(&mut s, no_input(), &params, &world, 1.0 / 60.0);
            assert!(s.grounded && s.pawn.based);
            close(s.pawn.floor.z, plane.normal.z, 1e-6, "floor normal");
            close(s.position.z - under, 61.85, 1e-3, "centre above the floor");
            assert!(
                (s.position.z - under - 57.65).abs() > 4.0,
                "not the cylinder's"
            );
            assert_eq!((s.position.x, s.position.y), (x, y));
        }
    }
    // The box cannot be lower: 3.15 below that height its corner is on the
    // floor.
    let touching = Vec3::new(0.0, 0.0, HALF_HEIGHT + 14.7);
    let extent = Vec3::new(RADIUS, RADIUS, HALF_HEIGHT);
    assert!(!world.overlaps_box(touching + Vec3::Z * 0.01, extent));
    assert!(world.overlaps_box(touching - Vec3::Z * 0.01, extent));
}

/// The pawn's box stops at a wall's face with its centre one half width
/// (times 1.001 for a move) from it wherever the wall's end is sideways,
/// where a cylinder would slip past the corner (P4: the Workshop door,
/// stopped 21 uu from the face at a sideways offset of 9 uu).
#[test]
fn a_wall_end_9_uu_inside_the_footprint_stops_the_pawn_at_the_face() {
    let mut params = original();
    params.movement.custom_gravity_scaling.value = 0.0;
    let dt = 1.0 / 64.0;
    let face = -RADIUS * MOVE_RADIUS_INFLATION;
    // The wall covers y ≤ −9 only: its end is 9 uu to the side of the
    // pawn's centre line, 12 uu inside the box's footprint.
    let world = BoxWorld::new().with_box(
        Vec3::new(face - 50.0, -500.0, -500.0),
        Vec3::new(face, -9.0, 500.0),
        false,
    );
    // A 7 uu move from 3 uu away: stopped 0.7 uu from the face. A cylinder
    // of radius 21.021 would touch the corner only sqrt(21.021² − 9²) =
    // 19.0 uu from the face, 2 uu further on, and the move would be free.
    let mut s = PlayerState::new(Vec3::new(3.0, 0.0, 0.0), 0.0);
    s.velocity = Vec3::new(-7.0 / dt, 0.0, 0.0);
    tick(&mut s, no_input(), &params, &world, dt);
    close(s.position.x, 0.7, 1e-5, "stopped at the face");
    assert_eq!(s.position.y, 0.0);
}
