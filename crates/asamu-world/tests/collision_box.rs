//! The swept axis-aligned box against hand-built geometry (synthetic data
//! only). Every expected value is derived from the geometry in the comment
//! next to it; none comes from running the code.
//!
//! The pawn's box has half extents (21, 21, 44) (`CollisionRadius` 21,
//! `CollisionHeight` 44: DEFAULTS.md); the radius-21 cylinder of the same
//! height is swept alongside to show where the two shapes differ
//! (`docs/PARITY_FINDINGS.md` P4).

#![allow(clippy::unwrap_used)]

use asamu_world::SurfaceTag;
use asamu_world::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, InstanceInfo, QueryFilter,
    SceneHit, contact_tolerance,
};
use glam::{DVec3, Vec3};

const HALF: Vec3 = Vec3::new(21.0, 21.0, 44.0);
const PAWN: QueryFilter = QueryFilter::PAWN;

fn info() -> InstanceInfo {
    InstanceInfo {
        actor: None,
        class: CollisionClass::StaticMesh,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: true,
        blocks_traces: true,
        sublevel: 0,
    }
}

/// One static two-sided triangle mesh.
fn scene_of(vertices: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> CollisionScene {
    let mut b = CollisionSceneBuilder::new();
    let m = b.add_mesh(vertices, triangles).unwrap();
    b.add_static(m, Affine::IDENTITY, info()).unwrap();
    b.build()
}

/// The plane `z = a·x + b·y` as one large triangle. With `a` and `b`
/// multiples of 1/16 and integer corner coordinates the corners lie exactly
/// on the plane in `f32`.
fn plane(a: f32, b: f32) -> CollisionScene {
    let p = |x: f32, y: f32| Vec3::new(x, y, a * x + b * y);
    scene_of(
        vec![p(-4096.0, -4096.0), p(4096.0, -4096.0), p(0.0, 4096.0)],
        vec![[0, 1, 2]],
    )
}

/// The box `[x0, x1] × [y0, y1] × [z0, z1]` as twelve triangles.
fn block(lo: Vec3, hi: Vec3) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let v = (0..8u32)
        .map(|i| {
            Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        })
        .collect();
    let t = vec![
        [0, 1, 3],
        [0, 3, 2],
        [4, 6, 7],
        [4, 7, 5],
        [0, 4, 5],
        [0, 5, 1],
        [2, 3, 7],
        [2, 7, 6],
        [0, 2, 6],
        [0, 6, 4],
        [1, 5, 7],
        [1, 7, 3],
    ];
    (v, t)
}

fn sweep_box(scene: &CollisionScene, start: Vec3, end: Vec3) -> Option<SceneHit> {
    scene.sweep_box(&[], start, end, HALF, PAWN)
}

fn sweep_cylinder(scene: &CollisionScene, start: Vec3, end: Vec3) -> Option<SceneHit> {
    scene.sweep_cylinder(&[], start, end, 21.0, 44.0, PAWN)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0e-9
}

/// On a plane of gradient `(a, b)` the box rests with its centre
/// `44 + 21·(|a| + |b|)` above the plane point under its centre: the box
/// touches with the bottom corner at `(±21, ±21, −44)` on the uphill side
/// (the signs of `a` and `b`), where the plane is `21·|a| + 21·|b|` above
/// its height under the centre. The radius-21 cylinder touches with the rim
/// point straight uphill, where the plane is `21·√(a² + b²)` higher, so it
/// rests at `44 + 21·√(a² + b²)`: lower than the box on every slope that is
/// not along an axis. Both normals are the plane's, `(−a, −b, 1)` normalised.
#[test]
fn a_box_rests_higher_on_a_slope_than_a_cylinder() {
    for (a, b) in [
        (0.0f32, 0.0f32),
        (0.25, 0.0),
        (0.0, -0.5),
        (0.25, -0.5),
        (-0.75, 0.375),
        (1.0, 1.0),
    ] {
        let scene = plane(a, b);
        let (af, bf) = (f64::from(a), f64::from(b));
        let normal = DVec3::new(-af, -bf, 1.0).normalize();
        for (x, y) in [(0.0f32, 0.0f32), (128.0, -64.0), (-300.0, 200.0)] {
            let under = af * f64::from(x) + bf * f64::from(y);
            // Dropped 1000 uu from 600 above the plane point.
            let top = (under + 600.0) as f32;
            let start = Vec3::new(x, y, top);
            let end = Vec3::new(x, y, top - 1000.0);
            let fall = |rest: f64| (f64::from(top) - (under + rest)) / 1000.0;

            let box_rest = 44.0 + 21.0 * (af.abs() + bf.abs());
            let hit = sweep_box(&scene, start, end).unwrap();
            assert!(close(hit.t, fall(box_rest)), "box on ({a}, {b}): {hit:?}");
            assert!((hit.normal - normal).length() < 1e-12, "{hit:?}");
            assert!(!hit.penetrating);
            // The contact position is free; a hair lower overlaps.
            let at = hit.point(start, end);
            assert!(close(at.z, under + box_rest));
            let rest = Vec3::new(x, y, (under + box_rest) as f32);
            assert!(!scene.overlaps_box(&[], rest, HALF, PAWN));
            assert!(scene.overlaps_box(&[], rest - Vec3::Z * 0.01, HALF, PAWN));

            let cyl_rest = 44.0 + 21.0 * (af * af + bf * bf).sqrt();
            let hit = sweep_cylinder(&scene, start, end).unwrap();
            assert!(
                close(hit.t, fall(cyl_rest)),
                "cylinder on ({a}, {b}): {hit:?}"
            );
            assert!((hit.normal - normal).length() < 1e-12, "{hit:?}");
            assert!(cyl_rest <= box_rest);
            if a != 0.0 && b != 0.0 {
                // (0.25, −0.5): 15.75 against 11.74; (1, 1): 42 against 29.70.
                assert!(box_rest - cyl_rest > 3.0);
            }
        }
    }
}

/// A wall block whose face is the plane x = 282 for y ≥ −342 (the door of
/// finding V9, simplified). Walking in −x, the box stops with its centre at
/// x = 282 + 21 = 303 whenever any part of its 42-wide side reaches past
/// the corner at y = −342, however little: the contact is face against
/// face and its normal is +X.
///
/// The cylinder walking along y = −351.1 has its axis 9.1 beside the
/// corner; it touches the corner (a vertical edge) when
/// `(x − 282)² + 9.1² = 21²`, at x = 282 + √(441 − 82.81) = 300.926, which is
/// 2.07 uu further than the box, with a normal that points from the corner
/// to the axis instead of along +X.
#[test]
fn a_box_stops_21_from_a_wall_face_whatever_the_overlap() {
    let (v, t) = block(
        Vec3::new(182.0, -342.0, 0.0),
        Vec3::new(282.0, 100.0, 200.0),
    );
    let scene = scene_of(v, t);
    let walk = |y: f32| (Vec3::new(330.0, y, 100.0), Vec3::new(230.0, y, 100.0));
    // Sideways overlap of the box with the face: 42, 21, 11.9, 0.1 uu.
    for y in [-200.0f32, -342.0, -351.1, -362.9] {
        let (s, e) = walk(y);
        let hit = sweep_box(&scene, s, e).unwrap();
        // 330 − 303 = 27 of 100.
        assert!(close(hit.t, 0.27), "y = {y}: {hit:?}");
        assert_eq!(hit.normal, DVec3::X);
        assert!(close(hit.point(s, e).x, 303.0));
    }
    // Half a unit clear of the corner (the box's side at y = −342.5): past.
    let (s, e) = walk(-363.5);
    assert!(sweep_box(&scene, s, e).is_none());

    let (s, e) = walk(-351.1);
    let beside = -342.0 - f64::from(-351.1f32);
    let x = 282.0 + (21.0f64 * 21.0 - beside * beside).sqrt();
    let hit = sweep_cylinder(&scene, s, e).unwrap();
    assert!(close(hit.t, (330.0 - x) / 100.0), "{hit:?}");
    assert!(303.0 - x > 2.07 && 303.0 - x < 2.08);
    assert!(hit.normal.x > 0.0 && hit.normal.y < 0.0 && hit.normal.z == 0.0);
    // Head on, the two shapes agree: 21 from the face.
    let (s, e) = walk(-200.0);
    assert!(close(sweep_cylinder(&scene, s, e).unwrap().t, 0.27));
}

/// A thin wall in the plane x = 150 that ends at the vertical edge
/// (150, 185). Moving along the diagonal (1, 1) from the origin, the
/// cylinder's axis passes the edge at distance |150 − 185| / √2 = 24.75,
/// more than its radius 21: it goes by. The box's silhouette across that
/// motion is 21·√2 = 29.70 wide on each side, so it is caught: its +X side
/// crosses x = 150 when the centre is at 129, and its +Y side reaches
/// y = 185 when the centre is at 164, while its X range [143, 185] still
/// holds x = 150. The contact is at 164 of 300, on the box's +Y face, so
/// the normal is −Y.
#[test]
fn a_cylinder_passes_a_corner_the_box_cannot() {
    let scene = scene_of(
        vec![
            Vec3::new(150.0, 185.0, 0.0),
            Vec3::new(150.0, 600.0, 0.0),
            Vec3::new(150.0, 600.0, 200.0),
            Vec3::new(150.0, 185.0, 200.0),
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    let s = Vec3::new(0.0, 0.0, 100.0);
    let e = Vec3::new(300.0, 300.0, 100.0);
    assert!(sweep_cylinder(&scene, s, e).is_none());
    let hit = sweep_box(&scene, s, e).unwrap();
    assert!(close(hit.t, 164.0 / 300.0), "{hit:?}");
    assert_eq!(hit.normal, DVec3::NEG_Y);
    // Shifted 8 uu away from the wall's end (edge at distance 30.4), the
    // box passes as well: 21·√2 = 29.70 < 30.41.
    let shift = Vec3::new(8.0, 0.0, 0.0);
    let (s2, e2) = (s + shift, e + shift);
    assert!(((150.0f64 - 8.0 - 185.0) / 2.0f64.sqrt()).abs() > 21.0 * 2.0f64.sqrt());
    assert!(sweep_box(&scene, s2, e2).is_none());
}

/// A vertical wall in the plane x + y = 400. The box touches it with its
/// vertical edge at (+21, +21): walking in +x from the origin, that edge is
/// at x + y = c + 42, so the centre stops at c = 358, which is
/// 42 / √2 = 21·(|nx| + |ny|) = 29.70 from the plane. The cylinder stops 21
/// from the plane, at c = 400 − 21·√2 = 370.30: 12.30 uu further. Both
/// report the wall's normal, −(1, 1, 0) / √2.
#[test]
fn an_oblique_wall_is_reached_earlier_by_the_box() {
    let scene = scene_of(
        vec![
            Vec3::new(900.0, -500.0, -500.0),
            Vec3::new(-500.0, 900.0, -500.0),
            Vec3::new(-500.0, 900.0, 500.0),
            Vec3::new(900.0, -500.0, 500.0),
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    let s = Vec3::ZERO;
    let e = Vec3::new(400.0, 0.0, 0.0);
    let n = DVec3::new(-1.0, -1.0, 0.0).normalize();
    let hit = sweep_box(&scene, s, e).unwrap();
    assert!(close(hit.t, 358.0 / 400.0), "{hit:?}");
    assert!((hit.normal - n).length() < 1e-12);
    let hit = sweep_cylinder(&scene, s, e).unwrap();
    assert!(
        close(hit.t, (400.0 - 21.0 * 2.0f64.sqrt()) / 400.0),
        "{hit:?}"
    );
    assert!((hit.normal - n).length() < 1e-12);
}

fn floor() -> CollisionScene {
    plane(0.0, 0.0)
}

/// A box standing exactly on a floor (centre 44 above it), or within the
/// contact tolerance of that, is stopped by a downward move where the floor
/// is (at once, or after the hair's breadth `gap / 5` of a 5 uu move that
/// starts a gap above it) and is free to slide and to rise. Deeper than the
/// tolerance it is a penetrating start, which blocks only motion into the
/// floor.
#[test]
fn sweeps_that_start_touching() {
    let scene = floor();
    let tol = contact_tolerance(Vec3::new(0.0, 0.0, 44.0)) as f32;
    for dz in [0.0, 0.5 * tol, -0.5 * tol] {
        let s = Vec3::new(10.0, -20.0, 44.0 + dz);
        let gap = (f64::from(s.z) - 44.0).max(0.0);
        let hit = sweep_box(&scene, s, s - Vec3::Z * 5.0).unwrap();
        assert!(
            close(hit.t, gap / 5.0) && hit.t < 2.0e-4,
            "dz {dz}: {hit:?}"
        );
        assert_eq!((hit.normal, hit.penetrating), (DVec3::Z, false));
        assert_eq!(hit.t == 0.0, dz <= 0.0);
        // Down and sideways: the floor all the same.
        let hit = sweep_box(&scene, s, s + Vec3::new(30.0, 0.0, -5.0)).unwrap();
        assert!(close(hit.t, gap / 5.0));
        assert_eq!(hit.normal, DVec3::Z);
        for free in [
            Vec3::new(50.0, 0.0, 0.0),
            Vec3::new(-7.0, 9.0, 0.0),
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(20.0, 20.0, 1.0),
        ] {
            assert!(sweep_box(&scene, s, s + free).is_none(), "dz {dz} {free}");
        }
        assert!(!scene.overlaps_box(&[], s, HALF, PAWN));
    }
    // 1 uu inside.
    let s = Vec3::new(10.0, -20.0, 43.0);
    let hit = sweep_box(&scene, s, s + Vec3::new(10.0, 0.0, -5.0)).unwrap();
    assert_eq!((hit.t, hit.normal, hit.penetrating), (0.0, DVec3::Z, true));
    assert!(sweep_box(&scene, s, s + Vec3::X * 10.0).is_none());
    assert!(sweep_box(&scene, s, s + Vec3::Z * 3.0).is_none());
    assert!(scene.overlaps_box(&[], s, HALF, PAWN));
    // Touching a wall face sideways: into it is blocked, along it is free.
    let (v, t) = block(
        Vec3::new(100.0, -500.0, 0.0),
        Vec3::new(200.0, 500.0, 300.0),
    );
    let wall = scene_of(v, t);
    let s = Vec3::new(79.0, 0.0, 100.0);
    let hit = sweep_box(&wall, s, s + Vec3::X * 3.0).unwrap();
    assert_eq!(
        (hit.t, hit.normal, hit.penetrating),
        (0.0, DVec3::NEG_X, false)
    );
    assert!(sweep_box(&wall, s, s + Vec3::Y * 40.0).is_none());
    assert!(sweep_box(&wall, s, s + Vec3::new(-1.0, 40.0, 10.0)).is_none());
}

/// Passing the end of a wall. The wall is the triangle in the plane x = 100
/// with the vertical edge y = 100 and the rest at larger y.
///
/// - Moving in +x with the box's +Y side exactly at y = 100 (centre at 79):
///   the side slides along the edge; no contact.
/// - With the side 0.01 uu past the edge it is the wall face that is met,
///   when the +X side reaches x = 100: centre at 79, 79 of 200.
/// - Moving along (1, −1) so that the box's corner (+21, +21) passes exactly
///   through the edge: the corner is at x = 100 when the centre's x is 79,
///   after half of the motion (158, −158) from (0, 158), and its y is then
///   158 − 79 + 21 = 100. X is entered at the instant Y is left; an exact
///   graze counts as a contact, as in the original, on the axis entered: −X.
/// - The same motion started 0.5 uu lower in y misses the edge by 0.5.
#[test]
fn sweeps_that_graze_an_edge() {
    let scene = scene_of(
        vec![
            Vec3::new(100.0, 100.0, -1000.0),
            Vec3::new(100.0, 100.0, 1000.0),
            Vec3::new(100.0, 2100.0, 0.0),
        ],
        vec![[0, 1, 2]],
    );
    let s = Vec3::new(0.0, 79.0, 0.0);
    assert!(sweep_box(&scene, s, s + Vec3::X * 200.0).is_none());
    let s = Vec3::new(0.0, 79.01, 0.0);
    let hit = sweep_box(&scene, s, s + Vec3::X * 200.0).unwrap();
    assert!(close(hit.t, 79.0 / 200.0), "{hit:?}");
    assert_eq!(hit.normal, DVec3::NEG_X);

    let s = Vec3::new(0.0, 158.0, 0.0);
    let d = Vec3::new(158.0, -158.0, 0.0);
    let hit = sweep_box(&scene, s, s + d).unwrap();
    assert_eq!(
        (hit.t, hit.normal, hit.penetrating),
        (0.5, DVec3::NEG_X, false)
    );
    let s = Vec3::new(0.0, 157.5, 0.0);
    assert!(sweep_box(&scene, s, s + d).is_none());
}

/// Contacts with vertices and between edges.
///
/// - A spike that leans towards +x, so that its apex (0, 0, 10) is both its
///   highest point and its point of least x. Under the falling box the
///   box's bottom face lands on the vertex with the centre at z = 54; the
///   normal is the box face's axis, +Z, not a direction of the spike.
/// - The same apex met sideways by the box's +X face (centre z = 30, so the
///   apex is inside the face's height range): centre at x = −21, normal −X.
/// - A horizontal triangle at z = 10 whose edge runs along x + y = 100, met
///   by the box's vertical edge at (+21, +21) while walking in +x: that edge
///   is on the line when c + 42 = 100, centre at x = 58, and the box touches
///   the triangle's edge at (79, 21, 10), between its ends (200, −100) and
///   (0, 100). Edge against edge: the normal is perpendicular to both, the
///   horizontal direction −(1, 1, 0) / √2.
#[test]
fn sweeps_that_hit_a_vertex_or_cross_an_edge() {
    let spike = scene_of(
        vec![
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::new(60.0, 0.0, -200.0),
            Vec3::new(50.0, 3.0, -200.0),
            Vec3::new(50.0, -3.0, -200.0),
        ],
        vec![[0, 1, 2], [0, 2, 3], [0, 3, 1]],
    );
    let s = Vec3::new(7.0, -4.0, 154.0);
    let hit = sweep_box(&spike, s, s - Vec3::Z * 200.0).unwrap();
    assert_eq!((hit.t, hit.normal), (0.5, DVec3::Z));
    let s = Vec3::new(-121.0, 6.0, 30.0);
    let hit = sweep_box(&spike, s, s + Vec3::X * 200.0).unwrap();
    assert_eq!((hit.t, hit.normal), (0.5, DVec3::NEG_X));

    let ledge = scene_of(
        vec![
            Vec3::new(200.0, -100.0, 10.0),
            Vec3::new(0.0, 100.0, 10.0),
            Vec3::new(300.0, 300.0, 10.0),
        ],
        vec![[0, 1, 2]],
    );
    let s = Vec3::new(-100.0, 0.0, 0.0);
    let hit = sweep_box(&ledge, s, s + Vec3::X * 300.0).unwrap();
    assert!(close(hit.t, 158.0 / 300.0), "{hit:?}");
    assert!((hit.normal - DVec3::new(-1.0, -1.0, 0.0).normalize()).length() < 1e-12);
    // The cylinder reaches the same edge later: its axis must be 21 from
    // the line, at x + y = 100 − 21·√2, so x = 70.30.
    let hit = sweep_cylinder(&ledge, s, s + Vec3::X * 300.0).unwrap();
    assert!(
        close(hit.t, (200.0 - 21.0 * 2.0f64.sqrt()) / 300.0),
        "{hit:?}"
    );
}

/// Zero-length sweeps and hostile values give no hit and never panic.
#[test]
fn zero_length_and_non_finite_inputs_are_rejected() {
    let scene = floor();
    let s = Vec3::new(0.0, 0.0, 100.0);
    let e = Vec3::new(0.0, 0.0, -100.0);
    assert!(sweep_box(&scene, s, e).is_some());
    // Zero length, even where the box overlaps the floor.
    assert!(sweep_box(&scene, s, s).is_none());
    let inside = Vec3::new(0.0, 0.0, 10.0);
    assert!(sweep_box(&scene, inside, inside).is_none());
    assert!(
        scene
            .sweep_box_brute_force(&[], inside, inside, HALF, PAWN)
            .is_none()
    );
    assert!(scene.overlaps_box(&[], inside, HALF, PAWN));
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
        for k in 0..3 {
            let mut p = s;
            p[k] = bad;
            // `f32::MAX` is a finite position: the sweep runs (the motion
            // overflows to a non-finite length or simply misses).
            let _ = sweep_box(&scene, p, e);
            let _ = sweep_box(&scene, s, p);
            let _ = scene.sweep_box_brute_force(&[], p, e, HALF, PAWN);
            let _ = scene.overlaps_box(&[], p, HALF, PAWN);
            if !bad.is_finite() {
                assert!(sweep_box(&scene, p, e).is_none());
                assert!(sweep_box(&scene, s, p).is_none());
                assert!(!scene.overlaps_box(&[], p, HALF, PAWN));
                let mut h = HALF;
                h[k] = bad;
                assert!(scene.sweep_box(&[], s, e, h, PAWN).is_none());
                assert!(!scene.overlaps_box(&[], inside, h, PAWN));
            }
        }
    }
    for h in [
        Vec3::ZERO,
        Vec3::new(21.0, 21.0, 0.0),
        Vec3::new(0.0, 21.0, 44.0),
        Vec3::new(21.0, -21.0, 44.0),
        -HALF,
    ] {
        assert!(scene.sweep_box(&[], s, e, h, PAWN).is_none(), "{h}");
        assert!(scene.sweep_box_brute_force(&[], s, e, h, PAWN).is_none());
        assert!(!scene.overlaps_box(&[], inside, h, PAWN));
    }
    // An empty scene.
    let empty = CollisionScene::empty();
    assert!(empty.sweep_box(&[], s, e, HALF, PAWN).is_none());
    assert!(!empty.overlaps_box(&[], s, HALF, PAWN));
}
