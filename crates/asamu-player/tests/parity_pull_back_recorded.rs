//! Second check of the pull-back and the box in the movement port
//! (`docs/PARITY_FINDINGS.md` P3 and P4), written by the package's verifier
//! independently of `parity_pull_back.rs`.
//!
//! The inputs here are the moves the original attempted in the first
//! recordings (the per-tick move of the Workshop door contact, the last fall
//! moves of five landings), recomputed from the raw records by the verifier.
//! The expected values are derived from the native rule in the comments and
//! restated in [`pull`]; none is the output of the code under test. Where the
//! recording has the same quantity it is compared with the **derived** value,
//! at the resolution of the recorded position.
//!
//! The rule (NATIVE_PHYSICS.md 10.2; read a second time in the user's macOS
//! executable at two routines the first reading did not use,
//! `UFracturedStaticMeshComponent::LineCheck` 0x10086CA5A to 0x10086CB07 and
//! `UTerrainComponent::LineCheck` 0x1001C0783 to 0x1001C07D2, both K = 4.0):
//! with L the length of the trace, the hit time is reduced by `0.1 / L` when
//! that exceeds 0.1, else by the smaller of 0.1 and `K / L`, and clamped to
//! `[0, 1]`. In distance along the trace: 0.1 uu when L < 1, else
//! `min(0.1 · L, K)`.

use asamu_player::movement::LocomotionIntent;
use asamu_player::ue3_movement::{MOVE_RADIUS_INFLATION, PawnPhysicsState, Ue3PawnMovement};
use asamu_player::world::{
    CollisionShape, Hit, KindWorld, PrimitiveKind, TraceExtent, pull_back, reported_time,
};
use asamu_player::{BoxWorld, CollisionWorld, PlayerParams, PlayerState, StepEvents};
use glam::Vec3;

const RADIUS: f32 = 21.0;
const HALF_HEIGHT: f32 = 44.0;

/// The distance along a trace `len` uu long by which a hit is reported short
/// of the contact, for the cap `k` (1.0: BSP, brushes, simple bodies; 4.0:
/// mesh triangles). The rule above, written out.
fn pull(len: f64, k: f64) -> f64 {
    if len < 1.0 { 0.1 } else { (0.1 * len).min(k) }
}

fn original() -> PlayerParams {
    let params = PlayerParams::asamu_original();
    assert_eq!(params.movement.capsule_radius.value, RADIUS);
    assert_eq!(params.movement.capsule_half_height.value, HALF_HEIGHT);
    // MaxStepHeight 26: the floor check and the step are 28 uu.
    assert_eq!(params.movement.step_height.value, 26.0);
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
    Ue3PawnMovement.advance_with_stats(
        state,
        &intent,
        Vec3::ZERO,
        &params.movement,
        world,
        dt,
        &mut events,
    );
    assert!(state.position.is_finite() && state.velocity.is_finite());
    events
}

fn walking_at(position: Vec3) -> PlayerState {
    let mut s = PlayerState::new(position, 0.0);
    s.grounded = true;
    s.pawn = PawnPhysicsState {
        floor: Vec3::Z,
        based: true,
        ..PawnPhysicsState::default()
    };
    s
}

fn close(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: {got} vs {want}");
}

/// A wall whose face a move's box (half width 21 × 1.001) touches when the
/// pawn's centre is at x = 0, so that the centre's x is the gap; the floor is
/// BSP and holds the centre at z = 0 (bottom 2.15 + 1.0 above it).
fn wall_and_floor() -> BoxWorld {
    let face = -RADIUS * MOVE_RADIUS_INFLATION;
    BoxWorld::new()
        .with_ground(-HALF_HEIGHT - 3.15, false)
        .with_box(
            Vec3::new(face - 50.0, -500.0, -500.0),
            Vec3::new(face, 500.0, 500.0),
            false,
        )
}

// ---------------------------------------------------------------------------
// The Workshop door, seven ticks.
// ---------------------------------------------------------------------------

/// WS1 records 1563 to 1569: the move each tick attempted (towards −x, with
/// the sideways part that grows as the stick turns), recomputed from the raw
/// records with the walking velocity rule (spec 2.1) from the previous
/// record's velocity, this record's acceleration and `DeltaSeconds`. The
/// sideways parts equal the recorded sideways displacements to 0.00002 uu
/// (0.01920, 0.03192, 0.04196, 0.05096, 0.05981, 0.07300, 0.07385), which is
/// what the rule says of a wall slide and what checks the recomputation.
const DOOR_MOVES: [(f64, f64, f64); 7] = [
    (0.017_167_002, 2.265_963, 0.019_220),
    (0.016_826_898, 0.787_001, 0.031_900),
    (0.016_459_700, 0.660_360, 0.041_957),
    (0.016_376_499, 0.554_634, 0.050_957),
    (0.016_523_901, 0.565_965, 0.059_817),
    (0.017_761_599, 0.655_097, 0.072_997),
    (0.016_366_303, 0.557_212, 0.073_845),
];
/// The gap between the pawn's box and the door before record 1563, solved
/// from that record alone (its advance of 0.21140 uu and its move):
/// `(0.21140 + 0.11 × 2.265963) / 1.1`, see the derivation below.
const DOOR_FIRST_GAP: f64 = 0.418_774;
/// The gap after each record: the recorded x less the contact plane
/// x = 302.984852, which follows from [`DOOR_FIRST_GAP`]. The first entry
/// is therefore not a measurement of its own; the other six are.
const DOOR_RECORDED_GAPS: [f64; 7] = [
    0.207_38, 0.099_93, 0.099_80, 0.099_59, 0.099_47, 0.099_41, 0.099_13,
];
/// The recorded x is an `f32` near 303, a multiple of 3.05e-5 uu; a gap is
/// the difference of two of them.
const DOOR_RESOLUTION: f64 = 2.0 * 3.05e-5;

/// One walking tick against a wall `gap` uu ahead, as spec 3.2 and 3.2.1
/// order it and with every hit reported by the rule: the move, then the
/// step-up's second try with what is left of it. `a` is the part of the move
/// towards the wall, `len` its whole length. Returns the gap afterwards.
fn door_tick(gap: f64, a: f64, len: f64) -> f64 {
    // The pull-back is a distance along the trace; towards the wall it is
    // that distance times a / len.
    let sweep = |gap: f64, a: f64, len: f64| -> f64 {
        if a <= gap {
            return a; // the wall is not reached: nothing is reported
        }
        (gap - pull(len, 1.0) * a / len).max(0.0)
    };
    let first = sweep(gap, a, len);
    let left = 1.0 - first / a;
    let second = sweep(gap - first, a * left, len * left);
    gap - first - second
}

/// The door contact of WS1 (V11), with the moves as they were attempted,
/// sideways part included.
///
/// Derivation. Record 1563, move 2.266 uu long (2.265963 towards the door):
/// the door is met after 0.418774; the hit is reported a tenth of the move
/// short, so the pawn advances 0.418774 − 0.2265963 = 0.1921777 and is
/// 0.2265963 away. The step-up's second try is the rest of the move,
/// 2.265963 − 0.1921777 = 2.0737853 towards the door, reported a tenth of
/// itself short: the pawn is 0.2073785 away. In general the gap after such a
/// tick is 0.1 × (1.1 × a − gap), which is where [`DOOR_FIRST_GAP`] comes
/// from. Records 1564 to 1569, moves shorter than 1 uu: a hit is reported
/// 0.1 uu short **along the move**, which is 0.1 × a / len towards the door,
/// and a pawn nearer than that is not moved: the gap becomes the smaller of
/// itself and 0.1 × a / len. As the move turns sideways the pawn creeps in:
/// 0.787001 / 0.787647 → 0.09992, 0.660360 / 0.661691 → 0.09980,
/// 0.554634 / 0.556970 → 0.09958, 0.565965 / 0.569118 → 0.09945,
/// 0.655097 / 0.659152 → 0.09938, 0.557212 / 0.562084 → 0.09913.
/// Recorded: 0.09993, 0.09980, 0.09959, 0.09947, 0.09941, 0.09913 (the
/// "rest gap of 0.0999" of the findings is 0.1 times that cosine).
///
/// Sideways the pawn makes the whole move every tick: what the two blocked
/// tries leave of it is slid along the door.
#[test]
fn the_workshop_door_tick_by_tick() {
    let derived = [
        0.207_38, 0.099_92, 0.099_80, 0.099_58, 0.099_45, 0.099_38, 0.099_13,
    ];
    let world = wall_and_floor();
    let mut params = original();
    let mut s = walking_at(Vec3::new(DOOR_FIRST_GAP as f32, 0.0, 0.0));
    let mut gap = DOOR_FIRST_GAP;
    for (i, &(dt, a, b)) in DOOR_MOVES.iter().enumerate() {
        let dt = dt as f32;
        let len = a.hypot(b);
        // The rule, restated, gives the hand-derived value …
        gap = door_tick(gap, a, len);
        close(gap, derived[i], 1e-5, "restated rule vs the derivation");
        // … which is what the original did, to the recording's resolution.
        close(
            derived[i],
            DOOR_RECORDED_GAPS[i],
            DOOR_RESOLUTION,
            "derivation vs recording",
        );

        // The port: a pawn at its speed cap with the input along its
        // velocity attempts exactly cap × dt along it (2.1).
        let speed = (len / f64::from(dt)) as f32;
        let dir = Vec3::new(-(a / len) as f32, (b / len) as f32, 0.0);
        params.movement.max_ground_speed.value = speed;
        s.velocity = dir * speed;
        let before = s.position;
        tick(&mut s, towards(dir), &params, &world, dt);
        assert!(s.grounded && s.pawn.based);
        close(f64::from(s.position.x), derived[i], 2e-5, "gap");
        close(
            f64::from(s.position.x),
            DOOR_RECORDED_GAPS[i],
            DOOR_RESOLUTION,
            "gap vs recording",
        );
        close(f64::from(s.position.y - before.y), b, 2e-5, "sideways");
        close(f64::from(s.position.z), 0.0, 1e-4, "height");
        // Walking velocity is the displacement of the tick (3.1).
        close(
            f64::from(s.velocity.x),
            f64::from(s.position.x - before.x) / f64::from(dt),
            1e-3,
            "vx",
        );
    }
    // Never nearer than it was allowed, never touching.
    assert!(s.position.x > 0.099);
}

// ---------------------------------------------------------------------------
// Landings.
// ---------------------------------------------------------------------------

/// Falls straight down onto a flat floor of `kind` with a last fall move of
/// `fall` uu in a tick of `dt`, stands still for a few ticks and then starts
/// to walk. Returns the height of the pawn's bottom above the floor after
/// the landing tick, while standing, and after each of the first two moving
/// ticks.
fn land_and_walk(kind: PrimitiveKind, dt: f64, fall: f64) -> [f64; 4] {
    let dt = dt as f32;
    let params = original();
    let world = KindWorld::new(BoxWorld::new().with_ground(0.0, false), kind);
    let g = f64::from(
        params.movement.world_gravity_z.value * params.movement.custom_gravity_scaling.value,
    );
    // The fall move of a tick is (V + g·dt)·dt (4.3).
    let v0 = -fall / f64::from(dt) - g * f64::from(dt);
    // Start half a move above the floor: met in mid-move.
    let start = HALF_HEIGHT + (fall * 0.5) as f32;
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, start), 0.0);
    s.velocity = Vec3::new(0.0, 0.0, v0 as f32);
    let events = tick(&mut s, no_input(), &params, &world, dt);
    assert!(events.landed.is_some() && s.grounded);
    let bottom = |s: &PlayerState| f64::from(s.position.z - HALF_HEIGHT);
    let landed = bottom(&s);
    for _ in 0..5 {
        tick(&mut s, no_input(), &params, &world, dt);
    }
    let standing = bottom(&s);
    tick(&mut s, towards(Vec3::X), &params, &world, dt);
    let first = bottom(&s);
    tick(&mut s, towards(Vec3::X), &params, &world, dt);
    [landed, standing, first, bottom(&s)]
}

/// Three respawn falls of the recordings that end on simple-collision floors
/// (DC1 record 5750, PLAY2 records 9088 and 20801; the pawn then stands for
/// 21 to 31 records before it walks). `DeltaSeconds` of the landing record
/// and the fall move `(Vz + g·dt)·dt` from the record before it.
///
/// Derivation (K = 1.0). The fall move, L uu long, is reported a tenth of
/// itself short: the pawn stops 0.1·L above the floor and lands (4.4). The
/// landing forces a floor check (5.2); the 28 uu check reports the floor
/// 1.0 uu nearer, so 0.1·L is reported as 0 and the pawn is moved up by the
/// whole 2.15 (3.3): 0.1·L + 2.15. Standing still nothing is checked. The
/// first moving tick reports 0.1·L + 1.15, under 1.9, and moves the pawn up
/// to a reported 2.15: a true 3.15, a rise of 1.0 − 0.1·L. Then it stays.
///
/// | record | L | rise derived | rise recorded |
/// |---|---|---|---|
/// | DC1 5750 | 1.32512 | 0.86749 | 0.8672 |
/// | PLAY2 9088 | 1.28335 | 0.87167 | 0.8789 |
/// | PLAY2 20801 | 1.28779 | 0.87122 | 0.8750 |
///
/// The recorded heights there are `f32` above 32,768, multiples of 1/256 uu
/// (0.0039); a rise is the difference of two of them.
#[test]
fn recorded_landings_on_simple_collision_floors_rise_to_3_15_with_the_first_step() {
    let cases = [
        (0.016_820_099, 1.325_12, 0.867_49, 0.8672),
        (0.016_375_601, 1.283_35, 0.871_67, 0.8789),
        (0.016_362_701, 1.287_79, 0.871_22, 0.8750),
    ];
    for (dt, fall, rise, recorded_rise) in cases {
        close(1.0 - pull(fall, 1.0), rise, 1e-5, "derivation");
        close(rise, recorded_rise, 2.0 / 256.0, "derivation vs recording");
        for kind in [
            PrimitiveKind::SimpleHull,
            PrimitiveKind::Bsp,
            PrimitiveKind::BrushHull,
        ] {
            let [landed, standing, first, second] = land_and_walk(kind, dt, fall);
            close(landed, 0.1 * fall + 2.15, 1e-4, "after the landing");
            close(standing, landed, 1e-6, "standing");
            close(first, 3.15, 1e-4, "first moving tick");
            close(first - standing, rise, 1e-4, "rise");
            close(second, 3.15, 1e-4, "and stays");
        }
    }
}

/// Two falls of the recordings that end on per-triangle floors: DC1 record
/// 1000 (a platform in AG-ParadiseCave) and PLAY2 record 25525 (the
/// AG-Darkcave start).
///
/// Derivation (K = 4.0; the 28 uu check reports the floor 2.8 uu nearer).
/// The landing leaves 0.1·L; the forced check reports 0 and moves the pawn up
/// by 2.15: 0.1·L + 2.15. The first moving tick reports that less 2.8, which
/// is below 0 and clamped to 0: up by 2.15 again, 0.1·L + 4.3.
///
/// - L = 1.05086: 2.25509 after the landing (measured under the pawn's
///   42 × 42 footprint on the converted triangles: 2.255), 4.40509 after
///   the first moving tick (recorded rise 2.1484, heights in multiples of
///   1/256). That is reported as 1.605, under 1.9, so the second moving tick
///   moves it up to a reported 2.15: 4.95.
/// - L = 4.65784: 2.61578 (measured: 2.615), then 4.76578 (recorded rise
///   2.1499), reported as 1.966, inside 1.9 to 2.4: it stays.
#[test]
fn recorded_landings_on_triangles_rise_by_2_15_twice() {
    let cases = [
        (0.016_878_597, 1.050_86, 2.255_09, 2.255, 2.1484, 4.95),
        (0.016_616_002, 4.657_84, 2.615_78, 2.615, 2.1499, 4.765_78),
    ];
    for (dt, fall, rest, measured_rest, recorded_rise, settled) in cases {
        close(pull(fall, 4.0) + 2.15, rest, 1e-5, "derivation");
        close(rest, measured_rest, 1e-3, "derivation vs measured gap");
        close(2.15, recorded_rise, 2.0 / 256.0, "derivation vs recording");
        let [landed, standing, first, second] =
            land_and_walk(PrimitiveKind::TriangleMesh, dt, fall);
        close(landed, rest, 1e-4, "after the landing");
        close(standing, landed, 1e-6, "standing");
        close(first, rest + 2.15, 1e-4, "first moving tick");
        close(second, settled, 1e-4, "second moving tick");
        // The band is one of reported distances: 4.7 to 5.2 on triangles.
        assert!((1.9..=2.4).contains(&(second - 2.8)));
    }
}

// ---------------------------------------------------------------------------
// Stepping onto a ledge: the step down and the floor check together.
// ---------------------------------------------------------------------------

/// A walking pawn steps onto a ledge 20 uu high and stands on it exactly as
/// high as it stood on the floor: 3.15 uu on BSP, 4.95 on triangles.
///
/// Derivation, for a 7 uu move that starts 3 uu from the ledge's face. The
/// move is blocked after 3 uu and reported 0.7 short: the pawn advances 2.3.
/// Step-up (3.2.1): up 28 uu, free; then the rest of the move, 4.7 uu, free
/// (the bottom is 28 + hover above the floor, over the 20 uu ledge), which
/// puts 4 uu of the box over the ledge; then down 28 uu. The ledge's top is
/// met after 8 + hover; the hit is reported min(2.8, K) short, so the bottom
/// stops K' = 1.0 (BSP) or 2.8 (triangles) above the ledge. The floor check
/// reports the ledge K' nearer again, at 0, and moves the pawn up by 2.15:
/// 3.15 or 4.95, the same hover as before the step. The whole move was
/// made: 2.3 + 4.7 = 7 uu, so the walking speed is kept.
#[test]
fn a_step_onto_a_ledge_ends_at_the_same_hover() {
    let dt = 1.0_f32 / 64.0;
    for (kind, hover) in [
        (PrimitiveKind::Bsp, 3.15_f64),
        (PrimitiveKind::SimpleHull, 3.15),
        (PrimitiveKind::TriangleMesh, 4.95),
    ] {
        let mut params = original();
        params.movement.max_ground_speed.value = 7.0 / dt;
        let face = 100.0_f32;
        let world = KindWorld::new(
            BoxWorld::new().with_ground(0.0, false).with_box(
                Vec3::new(face, -500.0, -50.0),
                Vec3::new(face + 500.0, 500.0, 20.0),
                false,
            ),
            kind,
        );
        let x0 = face - RADIUS * MOVE_RADIUS_INFLATION - 3.0;
        let mut s = walking_at(Vec3::new(x0, 0.0, HALF_HEIGHT + hover as f32));
        s.velocity = Vec3::new(7.0 / dt, 0.0, 0.0);
        tick(&mut s, towards(Vec3::X), &params, &world, dt);
        assert!(s.grounded && s.pawn.based);
        close(f64::from(s.position.x - x0), 7.0, 1e-4, "the whole move");
        close(
            f64::from(s.position.z - HALF_HEIGHT) - 20.0,
            hover,
            1e-4,
            "hover on the ledge",
        );
        close(f64::from(s.velocity.x), 7.0 / f64::from(dt), 0.05, "speed");
        for _ in 0..10 {
            tick(&mut s, towards(Vec3::X), &params, &world, dt);
            close(
                f64::from(s.position.z - HALF_HEIGHT) - 20.0,
                hover,
                1e-4,
                "walking on the ledge",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Very short moves, zero moves, and the rule applied exactly once.
// ---------------------------------------------------------------------------

/// A pawn drifting level (no gravity: a test input) towards the wall from
/// `gap` with a single move of `length`; returns the gap afterwards.
fn drift(kind: PrimitiveKind, gap: f32, length: f32) -> f32 {
    let mut params = original();
    params.movement.custom_gravity_scaling.value = 0.0;
    let dt = 1.0 / 64.0;
    let world = KindWorld::new(wall_and_floor(), kind);
    let mut s = PlayerState::new(Vec3::new(gap, 0.0, 50.0), 0.0);
    s.velocity = Vec3::new(-length / dt, 0.0, 0.0);
    tick(&mut s, no_input(), &params, &world, dt);
    assert!(!s.grounded);
    assert_eq!((s.position.y, s.position.z), (0.0, 50.0));
    s.position.x
}

/// Moves shorter than 1 uu, down to none at all.
///
/// - The rule moves a *reported hit* back; a move that ends before the wall
///   reports nothing and is made in full, however near the wall it ends. So
///   a pawn can come nearer than 0.1 uu with moves that small.
/// - A hit of a move shorter than 1 uu is reported 0.1 uu back, which for a
///   pawn nearer than 0.1 uu is before the start: time 0, no move, and no
///   push away from the wall either (the time is clamped, 10.2).
/// - The reduction is applied once: twice would leave a 7 uu move 1.4 uu
///   from the wall, and a 0.5 uu move from 0.3 uu where it started.
#[test]
fn short_and_zero_moves_follow_the_rule_and_nothing_else() {
    for kind in [PrimitiveKind::Bsp, PrimitiveKind::TriangleMesh] {
        // 0.001 uu from 0.05 uu away: not reached, made in full.
        close(f64::from(drift(kind, 0.05, 0.001)), 0.049, 1e-6, "free");
        // 0.001 uu from 0.0005 uu away: reached at half the move, reported
        // 0.1 uu back: held where it is.
        assert_eq!(drift(kind, 0.0005, 0.001), 0.0005);
        // 0.5 uu from 0.05 uu away: held, not pushed out to 0.1.
        assert_eq!(drift(kind, 0.05, 0.5), 0.05);
        // 0.5 uu from 0.3 uu away: 0.1 (twice the rule: 0.3, no move).
        close(f64::from(drift(kind, 0.3, 0.5)), 0.1, 1e-5, "0.5 uu");
        // The rule is continuous at 1 uu: 0.999 and 1.0 both leave 0.1.
        close(f64::from(drift(kind, 0.5, 0.999)), 0.1, 1e-5, "0.999 uu");
        close(f64::from(drift(kind, 0.5, 1.0)), 0.1, 1e-5, "1 uu");
        // 7 uu from 3 uu away: 0.7 (twice the rule: 1.4).
        close(f64::from(drift(kind, 3.0, 7.0)), 0.7, 1e-5, "7 uu");
        // A move of 1e-30 uu, whose length is 0 in f32: no hit is consumed
        // with a NaN time, the pawn does not move measurably.
        let x = drift(kind, 0.05, 1.0e-30);
        assert!((x - 0.05).abs() < 1e-6, "{x}");
    }
    // The caps differ from 10 uu on: 12 uu from 6 away leaves 1.0 on BSP and
    // 1.2 on triangles; 50 uu from 30 away leaves 1.0 and 4.0.
    close(
        f64::from(drift(PrimitiveKind::Bsp, 6.0, 12.0)),
        1.0,
        1e-5,
        "K 1",
    );
    close(
        f64::from(drift(PrimitiveKind::TriangleMesh, 6.0, 12.0)),
        1.2,
        1e-5,
        "a tenth",
    );
    close(
        f64::from(drift(PrimitiveKind::Bsp, 30.0, 50.0)),
        1.0,
        1e-4,
        "K 1",
    );
    close(
        f64::from(drift(PrimitiveKind::TriangleMesh, 30.0, 50.0)),
        4.0,
        1e-4,
        "K 4",
    );

    // No move at all: a pawn at rest in the air beside the wall, and a
    // walking pawn at rest 0.03 uu from it, stay where they are bit for bit.
    let mut params = original();
    params.movement.custom_gravity_scaling.value = 0.0;
    let world = wall_and_floor();
    let mut s = PlayerState::new(Vec3::new(0.03, 0.0, 50.0), 0.0);
    let before = s.position;
    tick(&mut s, no_input(), &params, &world, 1.0 / 64.0);
    assert_eq!(s.position, before);
    assert_eq!(s.velocity, Vec3::ZERO);
    let params = original();
    let mut s = walking_at(Vec3::new(0.03, 0.0, 0.0));
    let before = s.position;
    for _ in 0..3 {
        tick(&mut s, no_input(), &params, &world, 1.0 / 64.0);
        assert_eq!(s.position, before);
        assert!(s.grounded);
    }
    // Pushing into the wall from 0.03 uu: held in x (every try is reported
    // at time 0), and the pawn can still walk away from it.
    let dt = 1.0 / 64.0;
    for _ in 0..5 {
        tick(&mut s, towards(Vec3::NEG_X), &params, &world, dt);
        assert_eq!(s.position.x, 0.03);
        assert_eq!(s.velocity.x, 0.0);
    }
    tick(&mut s, towards(Vec3::X), &params, &world, dt);
    assert!(s.position.x > 0.03 && s.velocity.x > 0.0);
}

/// The rule itself at the ends of its domain: the result is a time in
/// `[0, 1]` for every length, including 0, subnormal and infinite ones, and
/// a hit at the very start stays at the start.
#[test]
fn the_rule_is_a_time_in_range_for_every_length() {
    let kinds = [
        PrimitiveKind::Bsp,
        PrimitiveKind::BrushHull,
        PrimitiveKind::SimpleHull,
        PrimitiveKind::TriangleMesh,
        PrimitiveKind::PawnCylinder,
    ];
    let lengths = [
        0.0_f32,
        f32::MIN_POSITIVE / 4.0,
        1.0e-30,
        1.0e-4,
        0.5,
        1.0,
        10.0,
        28.0,
        40.0,
        16384.0,
        f32::MAX,
        f32::INFINITY,
        f32::NAN,
    ];
    for kind in kinds {
        for extent in [TraceExtent::Box, TraceExtent::Zero] {
            for length in lengths {
                for time in [0.0_f32, 1.0e-6, 0.25, 1.0] {
                    let t = pull_back(kind, extent, length, time);
                    assert!(
                        (0.0..=1.0).contains(&t),
                        "{kind:?} {extent:?} {length} {time}"
                    );
                    assert!(t <= time, "never later than the contact");
                    for fraction in [0.0_f32, 1.0e-3, 0.5, 1.0, 2.0, f32::NAN] {
                        let r = reported_time(kind, extent, length, time, fraction);
                        assert!((0.0..=1.0).contains(&r));
                    }
                }
                assert_eq!(pull_back(kind, extent, length, 0.0), 0.0);
            }
        }
    }
    // In distance: the table of the module docs at one length each side of
    // every threshold (0.1 uu under 1 uu; a tenth; the cap).
    let back = |kind, extent, length: f32| {
        f64::from((0.9 - pull_back(kind, extent, length, 0.9)) * length)
    };
    for (length, k1, k4) in [
        (0.25_f32, 0.1, 0.1),
        (4.0, 0.4, 0.4),
        (10.0, 1.0, 1.0),
        (28.0, 1.0, 2.8),
        (40.0, 1.0, 4.0),
        (400.0, 1.0, 4.0),
    ] {
        close(pull(f64::from(length), 1.0), k1, 1e-12, "restated, K 1");
        close(pull(f64::from(length), 4.0), k4, 1e-12, "restated, K 4");
        let tol = 2e-6 * f64::from(length).max(1.0);
        for kind in [
            PrimitiveKind::Bsp,
            PrimitiveKind::BrushHull,
            PrimitiveKind::SimpleHull,
        ] {
            close(back(kind, TraceExtent::Box, length), k1, tol, "K 1");
        }
        close(
            back(PrimitiveKind::TriangleMesh, TraceExtent::Box, length),
            k4,
            tol,
            "K 4",
        );
        // A line trace of a mesh's triangles: the same computation (the
        // grapple anchor, 3.9967 uu short at ranges of 691 to 4,939 uu).
        close(
            back(PrimitiveKind::TriangleMesh, TraceExtent::Zero, length),
            k4,
            tol,
            "K 4, line",
        );
        // A line trace of BSP: a flat 0.5 uu.
        close(
            back(PrimitiveKind::Bsp, TraceExtent::Zero, length),
            0.5_f64.min(0.9 * f64::from(length)),
            tol,
            "BSP line",
        );
    }
}

// ---------------------------------------------------------------------------
// A world that has not implemented the box.
// ---------------------------------------------------------------------------

/// A world from before the box query: only the two older queries.
struct CylinderOnly(BoxWorld);

impl CollisionWorld for CylinderOnly {
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        self.0.sweep_capsule(start, end, shape)
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        self.0.raycast(origin, direction, max_distance)
    }
}

/// Through the trait's fallback the port still applies the rule, once, as
/// for BSP: the Workshop floor at 72 holds the centre at 119.15 (1,817
/// records, V18), and a blocked 7 uu move stops 0.7 uu short.
#[test]
fn a_world_without_the_box_gets_the_rule_once_as_bsp() {
    let params = original();
    let world = CylinderOnly(BoxWorld::new().with_ground(72.0, false));
    for gap in [10.0_f32, 2.0] {
        let mut s = walking_at(Vec3::new(131.0, -339.0, 72.0 + HALF_HEIGHT + gap));
        s.pawn.force_floor_check = true;
        tick(&mut s, no_input(), &params, &world, 1.0 / 60.0);
        close(f64::from(s.position.z), 119.15, 1e-4, "centre");
        for _ in 0..20 {
            tick(&mut s, towards(Vec3::X), &params, &world, 1.0 / 60.0);
            close(f64::from(s.position.z), 119.15, 1e-4, "centre, walking");
        }
    }
    let mut params = original();
    params.movement.custom_gravity_scaling.value = 0.0;
    let dt = 1.0 / 64.0;
    let world = CylinderOnly(wall_and_floor());
    let mut s = PlayerState::new(Vec3::new(3.0, 0.0, 50.0), 0.0);
    s.velocity = Vec3::new(-7.0 / dt, 0.0, 0.0);
    tick(&mut s, no_input(), &params, &world, dt);
    close(f64::from(s.position.x), 0.7, 1e-5, "7 uu");
}
