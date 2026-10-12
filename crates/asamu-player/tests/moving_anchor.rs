//! The anchor helper on a base that moves and turns (GRAPPLE.md G-AT-7,
//! G-AT-8): an actor based on another rides it rigidly.
//!
//! Expected places come from the rule: the helper's offset from its base's
//! location keeps its coordinates in the base's frame, whose axes are the
//! rows of the original's rotation matrix of (pitch, yaw, roll),
//!
//! ```text
//! X = ( cp·cy,             cp·sy,            sp   )
//! Y = ( sr·sp·cy − cr·sy,  sr·sp·sy + cr·cy, −sr·cp )
//! Z = (−(cr·sp·cy + sr·sy), cy·sr − cr·sp·sy,  cr·cp )
//! ```
//!
//! (`UWorld::MoveActor @ 0x1008E94A0`, CONFIRMED (native)). So a yaw of a
//! quarter turn takes local +X to world +Y, a pitch of a quarter turn takes
//! local +X to world +Z, and a roll of a quarter turn takes local +Y to
//! world −Z and local +Z to world +Y. The worlds are hand-made.

mod common;

use asamu_player::grapple_gun::{self, BasePose, ReleaseReason, carried_by_base};
use asamu_player::pawn;
use asamu_player::world::{
    ActorClass, ActorTag, CollisionShape, CollisionWorld, Hit, Surface, TraceHit,
};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, step_with,
};
use common::DT;
use glam::Vec3;

const QUARTER: i32 = 16_384;

fn pose(location: Vec3, rotation: [i32; 3]) -> BasePose {
    BasePose { location, rotation }
}

fn close(a: Vec3, b: Vec3, tolerance: f32) -> bool {
    (a - b).length() <= tolerance
}

#[test]
fn a_base_that_only_translates_moves_the_point_by_its_translation() {
    let p = Vec3::new(101.5, -20.25, 7.0);
    for rotation in [[0, 0, 0], [1234, -5678, 91_011]] {
        let moved = carried_by_base(
            p,
            pose(Vec3::new(1.0, 2.0, 3.0), rotation),
            pose(Vec3::new(11.0, 22.0, 33.0), rotation),
        );
        // Exactly: these sums are whole multiples of 1/4.
        assert_eq!(moved, Vec3::new(111.5, -0.25, 37.0));
    }
}

#[test]
fn a_base_that_turns_takes_the_point_around_its_location() {
    let origin = Vec3::ZERO;
    let turn = |p: Vec3, to: [i32; 3]| carried_by_base(p, pose(origin, [0; 3]), pose(origin, to));
    // The table's entry for a half turn is sin(f32(π)) = −8.7e-8, not 0, so
    // a quarter turn's cosine is off by that: 1e-5 uu on these arms.
    let tolerance = 1e-4;
    // Yaw: +X to +Y, +Y to −X, Z stays.
    let yaw = [0, QUARTER, 0];
    assert!(close(
        turn(Vec3::new(100.0, 0.0, 7.0), yaw),
        Vec3::new(0.0, 100.0, 7.0),
        tolerance
    ));
    assert!(close(
        turn(Vec3::new(0.0, 100.0, 7.0), yaw),
        Vec3::new(-100.0, 0.0, 7.0),
        tolerance
    ));
    // Pitch: +X to +Z, +Z to −X, Y stays.
    let pitch = [QUARTER, 0, 0];
    assert!(close(
        turn(Vec3::new(100.0, 5.0, 0.0), pitch),
        Vec3::new(0.0, 5.0, 100.0),
        tolerance
    ));
    assert!(close(
        turn(Vec3::new(0.0, 5.0, 100.0), pitch),
        Vec3::new(-100.0, 5.0, 0.0),
        tolerance
    ));
    // Roll: +Y to −Z, +Z to +Y, X stays.
    let roll = [0, 0, QUARTER];
    assert!(close(
        turn(Vec3::new(3.0, 100.0, 0.0), roll),
        Vec3::new(3.0, 0.0, -100.0),
        tolerance
    ));
    assert!(close(
        turn(Vec3::new(3.0, 0.0, 100.0), roll),
        Vec3::new(3.0, 100.0, 0.0),
        tolerance
    ));
}

#[test]
fn a_base_that_moves_and_turns_does_both() {
    // The base goes from (1000, 2000, 300) to (1010, 1990, 350) and turns
    // half a turn of yaw: an offset (50, 20, 5) becomes (−50, −20, 5).
    let from = pose(Vec3::new(1000.0, 2000.0, 300.0), [0, 0, 0]);
    let to = pose(Vec3::new(1010.0, 1990.0, 350.0), [0, 2 * QUARTER, 0]);
    let moved = carried_by_base(Vec3::new(1050.0, 2020.0, 305.0), from, to);
    assert!(
        close(moved, Vec3::new(960.0, 1970.0, 355.0), 1e-3),
        "{moved}"
    );
    // The turn is the change of rotation, whatever the rotation was: from a
    // quarter turn of yaw to three quarters the same offset is reversed too.
    let from = pose(from.location, [0, QUARTER, 0]);
    let to = pose(to.location, [0, 3 * QUARTER, 0]);
    let moved = carried_by_base(Vec3::new(1050.0, 2020.0, 305.0), from, to);
    assert!(
        close(moved, Vec3::new(960.0, 1970.0, 355.0), 1e-3),
        "{moved}"
    );
    // Turns counted beyond a full turn are the same turn.
    let wound = pose(to.location, [0, 3 * QUARTER + 65_536 * 4, 0]);
    let again = carried_by_base(Vec3::new(1050.0, 2020.0, 305.0), from, wound);
    assert!(close(again, moved, 1e-6), "{again} vs {moved}");
}

#[test]
fn the_sine_table_s_four_units_are_the_smallest_turn() {
    // An arm of 100 uu along X on a base at the origin.
    let p = Vec3::new(100.0, 0.0, 0.0);
    let base = |yaw: i32| pose(Vec3::ZERO, [0, yaw, 0]);
    // 1000 to 1003 units: the same table entry, nothing moves.
    let same = carried_by_base(p, base(1000), base(1003));
    assert!(close(same, p, 1e-5), "{same}");
    // 1000 to 1004 units: one entry on, 4 units of angle: the arm's end
    // moves 100 × 4 × 2π / 65 536 = 0.03835 uu across the arm.
    let next = carried_by_base(p, base(1000), base(1004));
    let moved = next - p;
    let expected = 100.0 * 4.0 * std::f32::consts::TAU / 65_536.0;
    assert!((moved.length() - expected).abs() < 1e-4, "{moved}");
    assert!(
        (moved.y - expected).abs() < 1e-4 && moved.z == 0.0,
        "{moved}"
    );
    // A negative angle is floored towards −∞ like a positive one: −1 unit
    // is the entry of −4.
    let below = carried_by_base(p, base(0), base(-1));
    assert!((below.y + expected).abs() < 1e-4, "{below}");
}

#[test]
fn many_small_turns_are_one_big_turn() {
    // 64 turns of 256 units of roll are a quarter turn: +Y to −Z on an arm
    // of 1000 uu. Each carry rounds in single precision (the arm's
    // coordinates have steps of 6e-5 uu), 64 carries stay within 0.01 uu.
    let location = Vec3::new(10.0, 20.0, 30.0);
    let mut p = location + Vec3::new(0.0, 1000.0, 0.0);
    for i in 0..64 {
        p = carried_by_base(
            p,
            pose(location, [0, 0, 256 * i]),
            pose(location, [0, 0, 256 * (i + 1)]),
        );
    }
    assert!(
        close(p, location + Vec3::new(0.0, 0.0, -1000.0), 0.01),
        "{p}"
    );
}

// ---------------------------------------------------------------------------
// The gun on a turning target.
// ---------------------------------------------------------------------------

/// Boxes whose tracked actors also report a rotation.
#[derive(Clone, Debug, Default)]
struct Turntable {
    boxes: BoxWorld,
    rotations: Vec<(u32, [i32; 3])>,
}

impl Turntable {
    fn set_pose(&mut self, id: u32, location: Vec3, rotation: [i32; 3]) {
        self.boxes.set_actor_location(id, location);
        match self.rotations.iter_mut().find(|r| r.0 == id) {
            Some(r) => r.1 = rotation,
            None => self.rotations.push((id, rotation)),
        }
    }
}

impl CollisionWorld for Turntable {
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        self.boxes.sweep_capsule(start, end, shape)
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        self.boxes.raycast(origin, direction, max_distance)
    }

    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.boxes.actor_location(id)
    }

    fn actor_rotation(&self, id: u32) -> Option<[i32; 3]> {
        self.rotations.iter().find(|r| r.0 == id).map(|r| r.1)
    }

    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        self.boxes.sweep_box(start, end, half_extents)
    }

    fn trace_line(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<TraceHit> {
        self.boxes.trace_line(origin, direction, max_distance)
    }
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &Turntable,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, DT);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

fn fire() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

/// A rock (an `InterpActor`, id 9: a cube of half size 100) at `centre`,
/// on an arm from its actor's location `pivot`.
fn rock_on_an_arm(centre: Vec3, pivot: Vec3) -> Turntable {
    let surface = Surface {
        actor: Some(9),
        class: ActorClass::InterpActor,
        tag: ActorTag::None,
    };
    let mut world = Turntable {
        boxes: BoxWorld::new().with_surface_box(
            centre - Vec3::splat(100.0),
            centre + Vec3::splat(100.0),
            true,
            surface,
        ),
        rotations: Vec::new(),
    };
    world.set_pose(9, pivot, [0; 3]);
    world
}

/// A started pawn at rest in the air at (0, 0, 1000) looking along +X, its
/// eye (38 uu above its centre) level.
fn floating(params: &PlayerParams) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    pawn::start(&mut s, params);
    grapple_gun::set_max_grapples(&mut s, 3);
    s
}

#[test]
fn g_at_8_the_anchor_turns_with_its_base() {
    let params = PlayerParams::asamu_original();
    // The rock's actor is 800 uu to the rock's −Y side: the rock is at the
    // end of an arm along +Y.
    let pivot = Vec3::new(3000.0, -800.0, 1038.0);
    let mut world = rock_on_an_arm(Vec3::new(3000.0, 0.0, 1038.0), pivot);
    let mut s = floating(&params);
    let e = tick(&mut s, &fire(), &params, &world);
    let anchor0 = e.gun.attached.expect("attached").anchor;
    // On the rock's near face (x = 2900), short of it by the trace's
    // pull-back, level with the eye.
    assert!((2899.0..2900.0).contains(&anchor0.x), "{anchor0}");
    assert!(anchor0.y.abs() < 1e-3 && (anchor0.z - 1038.0).abs() < 1e-3);
    let follow = s
        .script
        .gun
        .follow
        .expect("an InterpActor carries the anchor");
    assert_eq!(follow.actor, 9);
    assert_eq!(follow.base, Some(pose(pivot, [0; 3])));
    assert_eq!(follow.helper, anchor0);

    // The base turns a quarter turn of yaw about its location and does not
    // move: the anchor's offset (ax − 3000, 800, 0) becomes
    // (−800, ax − 3000, 0).
    world.set_pose(9, pivot, [0, QUARTER, 0]);
    tick(&mut s, &fire(), &params, &world);
    let expected = pivot + Vec3::new(-800.0, anchor0.x - 3000.0, 0.0);
    let anchor1 = s.grapple_anchor().expect("still attached");
    assert!(close(anchor1, expected, 1e-2), "{anchor1} vs {expected}");
    assert!(
        (anchor1 - anchor0).length() > 1000.0,
        "the anchor went with the rock: {anchor1}"
    );
    let follow = s.script.gun.follow.expect("follows");
    assert_eq!(follow.base, Some(pose(pivot, [0, QUARTER, 0])));
    // The pull's distance is measured to the anchor where it is now.
    let d = (s.position - anchor1).length();
    assert!((s.script.gun.distance - d).abs() < 1e-2, "{d}");

    // The base moves by (10, 20, 30) and turns on to half a turn: the
    // offset (−800, ax − 3000, 0) becomes (3000 − ax, −800, 0).
    let moved = pivot + Vec3::new(10.0, 20.0, 30.0);
    world.set_pose(9, moved, [0, 2 * QUARTER, 0]);
    tick(&mut s, &fire(), &params, &world);
    let expected = moved + Vec3::new(3000.0 - anchor0.x, -800.0, 0.0);
    let anchor2 = s.grapple_anchor().expect("still attached");
    assert!(close(anchor2, expected, 1e-2), "{anchor2} vs {expected}");

    // G-AT-8: the helper keeps riding after a release (cosmetic then). Back
    // at the start pose it is where the grapple hit.
    let e = tick(&mut s, &InputFrame::default(), &params, &world);
    assert_eq!(
        e.gun.released.map(|r| r.reason),
        Some(ReleaseReason::Button)
    );
    world.set_pose(9, pivot, [0; 3]);
    tick(&mut s, &InputFrame::default(), &params, &world);
    assert!(s.grapple_anchor().is_none());
    let helper = s.script.gun.follow.expect("the helper stays based").helper;
    assert!(close(helper, anchor0, 1e-2), "{helper} vs {anchor0}");
    assert_eq!(s.script.gun.grapple_location, helper);
}

#[test]
fn a_base_the_world_starts_to_track_later_does_not_move_the_helper() {
    let params = PlayerParams::asamu_original();
    let pivot = Vec3::new(3000.0, -800.0, 1038.0);
    let mut world = rock_on_an_arm(Vec3::new(3000.0, 0.0, 1038.0), pivot);
    // The world does not know where the rock's actor is.
    world.boxes.actors.clear();
    world.rotations.clear();
    let mut s = floating(&params);
    let e = tick(&mut s, &fire(), &params, &world);
    let anchor0 = e.gun.attached.expect("attached").anchor;
    assert_eq!(s.script.gun.follow.expect("follows").base, None);
    tick(&mut s, &fire(), &params, &world);
    assert_eq!(s.grapple_anchor(), Some(anchor0));
    // Once it does, the pose it reports first is where the helper rides
    // from: nothing is known of a move before it.
    world.set_pose(9, pivot, [0, QUARTER, 0]);
    tick(&mut s, &fire(), &params, &world);
    assert_eq!(s.grapple_anchor(), Some(anchor0));
    world.set_pose(9, pivot + Vec3::new(0.0, 0.0, 25.0), [0, QUARTER, 0]);
    tick(&mut s, &fire(), &params, &world);
    assert_eq!(
        s.grapple_anchor(),
        Some(anchor0 + Vec3::new(0.0, 0.0, 25.0))
    );
}
