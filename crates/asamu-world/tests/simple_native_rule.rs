//! The original's scale-dependent rule for a swept box (NATIVE_PHYSICS.md
//! 10.1; `collision::box_sweep`, "Triangles" and "Convex hulls"), on
//! geometry written here.
//!
//! Every expected value is derived from the rule as read in the user's
//! executable, in the comment of its test; none was taken from a run of this
//! code. The rule, for a convex element:
//!
//! - the test runs in the element's own space with the box's three edges
//!   carried there unnormalised (one over the scale long);
//! - an axis whose squared length is below 1e-4 is left out: a face axis of
//!   the box is the cross product of the other two carried edges, an edge
//!   axis is a stored unit edge direction crossed with one carried edge;
//! - before any of it, the bounds of the whole sweep in that space, widened
//!   by the box's half sizes there plus 0.1, must reach the element's bounds;
//! - the contact is the latest entry over the axes that are tested, and its
//!   normal is that axis (the earlier one in the list on equal entries).
//!
//! For triangles the threshold is 1e-8.

use asamu_world::SurfaceTag;
use asamu_world::collision::box_sweep::{
    self, AxisSkip, CONVEX_MIN_AXIS_LENGTH2, CONVEX_PRECHECK_PAD, TRIANGLE_MIN_AXIS_LENGTH2,
};
use asamu_world::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, Instance, InstanceInfo,
    InstanceOptions, NATIVE_RULE_MAX_EXTENT, PrimitiveKind, QueryFilter,
};
use glam::{DVec3, Vec3};

const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);
const ED: DVec3 = DVec3::new(21.0, 21.0, 44.0);
const TOL: f64 = 1.0e-3;

fn info(actor: u32) -> InstanceInfo {
    InstanceInfo {
        actor: Some(actor),
        class: CollisionClass::StaticMesh,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: true,
        blocks_traces: true,
        sublevel: 0,
    }
}

/// The carried box edges of a placement scaled by `s` per axis and not
/// turned: world axis `i` is `1 / s[i]` long in the element's space.
fn edges_of_scale(s: DVec3) -> [DVec3; 3] {
    [DVec3::X / s.x, DVec3::Y / s.y, DVec3::Z / s.z]
}

/// The octahedron with its six corners one unit from the centre.
fn octahedron() -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let v = vec![
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ];
    let t = vec![
        [0, 2, 4],
        [2, 1, 4],
        [1, 3, 4],
        [3, 0, 4],
        [2, 0, 5],
        [1, 2, 5],
        [3, 1, 5],
        [0, 3, 5],
    ];
    (v, t)
}

/// The octahedron's four face directions and six edge directions.
fn octahedron_directions() -> (Vec<Vec3>, Vec<Vec3>) {
    let f = 1.0 / 3.0f32.sqrt();
    let e = 1.0 / 2.0f32.sqrt();
    (
        vec![
            Vec3::new(f, f, f),
            Vec3::new(-f, f, f),
            Vec3::new(-f, -f, f),
            Vec3::new(f, -f, f),
        ],
        vec![
            Vec3::new(e, e, 0.0),
            Vec3::new(e, -e, 0.0),
            Vec3::new(e, 0.0, e),
            Vec3::new(e, 0.0, -e),
            Vec3::new(0.0, e, e),
            Vec3::new(0.0, e, -e),
        ],
    )
}

/// One octahedron placed by `to_world` under the original's rule, with or
/// without its edge directions.
fn octahedron_scene(to_world: Affine, with_edges: bool) -> CollisionScene {
    let mut b = CollisionSceneBuilder::new();
    let (v, t) = octahedron();
    let (faces, edges) = octahedron_directions();
    let edges = if with_edges { edges } else { Vec::new() };
    let m = b
        .add_convex_hull(v, t, &faces, &edges)
        .expect("the octahedron is a mesh");
    let i = b
        .add_static_with(
            m,
            to_world,
            info(1),
            InstanceOptions::of_kind(PrimitiveKind::SimpleHull),
        )
        .expect("the placement is usable");
    assert!(b.set_static_native(i, None));
    b.build()
}

fn scaled(s: DVec3) -> Affine {
    Affine {
        rows: [DVec3::X * s.x, DVec3::Y * s.y, DVec3::Z * s.z],
        translation: DVec3::ZERO,
    }
}

/// Where the box's centre is when a drop along −Z from `(x, y, 400)` to
/// `(x, y, 0)` is stopped.
fn drop_z(scene: &CollisionScene, x: f32, y: f32) -> Option<(f64, DVec3)> {
    let start = Vec3::new(x, y, 400.0);
    let end = Vec3::new(x, y, 0.0);
    scene
        .sweep_box(&[], start, end, E, QueryFilter::PAWN)
        .map(|h| (h.point(start, end).z, h.normal))
}

/// A face axis of the box is the cross product of the other two carried
/// edges, `1 / (s_j · s_k)` long. It is left out when its squared length is
/// below 1e-4, that is when `s_j · s_k > 100`: for a uniform scale, above 10.
/// For triangles the threshold is 1e-8: `s_j · s_k > 10,000`.
#[test]
fn face_axes_are_left_out_by_the_product_of_the_other_two_scales() {
    assert_eq!(CONVEX_MIN_AXIS_LENGTH2, 1.0e-4);
    assert_eq!(TRIANGLE_MIN_AXIS_LENGTH2, 1.0e-8);
    let convex = |s: DVec3| {
        let r = AxisSkip::convex(edges_of_scale(s));
        [r.skips_face(0), r.skips_face(1), r.skips_face(2)]
    };
    // Uniform: 1 / 9.9⁴ = 1.04e-4 stays, 1 / 10.1⁴ = 9.6e-5 goes.
    assert_eq!(convex(DVec3::splat(1.0)), [false; 3]);
    assert_eq!(convex(DVec3::splat(9.9)), [false; 3]);
    assert_eq!(convex(DVec3::splat(10.1)), [true; 3]);
    assert_eq!(convex(DVec3::splat(128.0)), [true; 3]);
    // Mirroring changes no length.
    assert_eq!(convex(DVec3::new(-10.1, 10.1, -10.1)), [true; 3]);
    // (4, 30, 2): the X face has 30 · 2 = 60, the Y face 4 · 2 = 8, the Z
    // face 4 · 30 = 120. Only Z goes.
    assert_eq!(convex(DVec3::new(4.0, 30.0, 2.0)), [false, false, true]);
    // A flat, wide placement (20, 20, 1): X has 20, Y has 20, Z has 400.
    assert_eq!(convex(DVec3::new(20.0, 20.0, 1.0)), [false, false, true]);
    // The triangle rule's own threshold, were its face axes subject to it:
    // (50, 50, 250) gives 12,500 for X and Y and 2,500 for Z.
    let tri = AxisSkip {
        box_edges: edges_of_scale(DVec3::new(50.0, 50.0, 250.0)),
        min_length2: TRIANGLE_MIN_AXIS_LENGTH2,
        face_axes: true,
    };
    assert_eq!(
        [tri.skips_face(0), tri.skips_face(1), tri.skips_face(2)],
        [true, true, false]
    );
    // What the triangle sweep uses keeps them (see `AxisSkip::triangles`).
    let kept = AxisSkip::triangles(edges_of_scale(DVec3::splat(500.0)));
    assert!(!kept.face_axes);
    assert_eq!(
        [kept.skips_face(0), kept.skips_face(1), kept.skips_face(2)],
        [false; 3]
    );
}

/// An edge axis is a unit edge direction (in the element's space) crossed
/// with a carried box edge: `sin θ / s` long, θ the angle between the two.
/// It is left out when `sin²θ / s² < threshold`: for a convex element when
/// `sin θ < 0.01 · s` (0.573° at scale 1, 5.74° at scale 10, every angle
/// above scale 100), for a triangle when `sin θ < 0.0001 · s` (0.573° at
/// scale 100).
#[test]
fn edge_axes_are_left_out_by_angle_and_scale() {
    // A world direction at `deg` degrees from the world X axis, in the XY
    // plane. With a uniform scale the angle is the same in the element's
    // space.
    let at = |deg: f64| {
        let a = deg.to_radians();
        DVec3::new(a.cos(), a.sin(), 0.0)
    };
    let convex = |s: f64, deg: f64| {
        AxisSkip::convex(edges_of_scale(DVec3::splat(s))).skips_edge(at(deg) * 7.0, 0)
    };
    // asin(0.01) = 0.5730°.
    assert!(convex(1.0, 0.0));
    assert!(convex(1.0, 0.56));
    assert!(!convex(1.0, 0.59));
    assert!(!convex(1.0, 45.0));
    // asin(0.1) = 5.739°.
    assert!(convex(10.0, 5.7));
    assert!(!convex(10.0, 5.8));
    // asin(0.5) = 30°.
    assert!(convex(50.0, 29.9));
    assert!(!convex(50.0, 30.1));
    // Above 100 nothing is long enough: sin θ ≤ 1 < 0.01 · 101.
    assert!(convex(101.0, 90.0));
    assert!(!convex(99.0, 90.0));
    // The same edge against the box's Z edge is at 90°: kept up to scale 100.
    let against_z =
        |s: f64| AxisSkip::convex(edges_of_scale(DVec3::splat(s))).skips_edge(at(0.2), 2);
    assert!(!against_z(1.0));
    assert!(!against_z(99.0));
    assert!(against_z(101.0));
    let tri = |s: f64, deg: f64| {
        AxisSkip::triangles(edges_of_scale(DVec3::splat(s))).skips_edge(at(deg), 0)
    };
    // asin(0.0001) = 0.00573°; at scale 100, asin(0.01) = 0.573°.
    assert!(tri(1.0, 0.005));
    assert!(!tri(1.0, 0.007));
    assert!(tri(100.0, 0.56));
    assert!(!tri(100.0, 0.59));
    // A direction that is no direction gives no axis.
    assert!(AxisSkip::convex(edges_of_scale(DVec3::ONE)).skips_edge(DVec3::ZERO, 1));
    assert!(AxisSkip::convex([DVec3::splat(f64::NAN); 3]).skips_face(0));
}

/// Non-uniform scale: the angle that counts is the one in the element's
/// space. A world edge at 45° in the XY plane under the scale (1, 20, 1) is,
/// in the element's space, along (1, 1/20, 0): 2.862° from the carried X
/// edge, whose length is 1. sin²(2.862°) = 0.002494 ≥ 1e-4: kept. Against
/// the carried Y edge (1/20 long) the angle is 87.14°: sin² / 400 = 0.002494:
/// kept. Under (1, 200, 1) the edge is 0.2865° from X: sin² = 2.5e-5: left
/// out.
#[test]
fn the_edge_angle_is_measured_in_the_elements_space() {
    let edge = DVec3::new(1.0, 1.0, 0.0);
    let r = AxisSkip::convex(edges_of_scale(DVec3::new(1.0, 20.0, 1.0)));
    assert!(!r.skips_edge(edge, 0));
    assert!(!r.skips_edge(edge, 1));
    let r = AxisSkip::convex(edges_of_scale(DVec3::new(1.0, 200.0, 1.0)));
    assert!(r.skips_edge(edge, 0));
    // Against Y: sin²(89.71°) / 200² = 2.5e-5: left out as well.
    assert!(r.skips_edge(edge, 1));
}

/// The test before a convex element is swept: per axis of the element's
/// space, `[min(start, end) − (half + 0.1), max(start, end) + (half + 0.1)]`
/// must reach `[elem_min, elem_max]`; touching counts.
#[test]
fn the_precheck_compares_the_bounds_of_the_whole_sweep() {
    assert_eq!(CONVEX_PRECHECK_PAD, 0.1);
    let (lo, hi) = (DVec3::splat(-1.0), DVec3::splat(1.0));
    let half = DVec3::new(0.5, 0.25, 0.125);
    let check = |a: DVec3, b: DVec3| box_sweep::convex_precheck(a, b, half, lo, hi);
    // On X the pad is 0.5 + 0.1: a sweep that ends at 2 reaches down to
    // 1.4 > 1, one that ends at 1.59 to 0.99, one that ends at 1.61 to 1.01.
    assert!(!check(DVec3::new(3.0, 0.0, 0.0), DVec3::new(2.0, 0.0, 0.0)));
    assert!(check(DVec3::new(3.0, 0.0, 0.0), DVec3::new(1.59, 0.0, 0.0)));
    assert!(!check(
        DVec3::new(3.0, 0.0, 0.0),
        DVec3::new(1.61, 0.0, 0.0)
    ));
    // Touching counts: an element whose upper bound is exactly where the
    // widened sweep begins is reached, one a single step of f64 below is not.
    let reach = 2.0 - (0.5 + CONVEX_PRECHECK_PAD);
    let touching = |elem_max_x: f64| {
        box_sweep::convex_precheck(
            DVec3::new(3.0, 0.0, 0.0),
            DVec3::new(2.0, 0.0, 0.0),
            half,
            lo,
            DVec3::new(elem_max_x, 1.0, 1.0),
        )
    };
    assert!(touching(reach));
    assert!(!touching(reach.next_down()));
    // On Y the pad is 0.35 and on Z 0.225.
    assert!(check(DVec3::new(0.0, 1.34, 0.0), DVec3::new(0.0, 5.0, 0.0)));
    assert!(!check(
        DVec3::new(0.0, 1.36, 0.0),
        DVec3::new(0.0, 5.0, 0.0)
    ));
    assert!(check(
        DVec3::new(0.0, 0.0, -1.22),
        DVec3::new(0.0, 0.0, -9.0)
    ));
    assert!(!check(
        DVec3::new(0.0, 0.0, -1.23),
        DVec3::new(0.0, 0.0, -9.0)
    ));
    // It is the bounds of the sweep that count, not the sweep: a diagonal
    // that passes the element at a distance still qualifies.
    assert!(check(
        DVec3::new(-50.0, 30.0, 0.0),
        DVec3::new(30.0, -50.0, 0.0)
    ));
    // NaN never qualifies.
    assert!(!check(DVec3::splat(f64::NAN), DVec3::ZERO));
}

/// An octahedron of radius `a` under a box dropped on its top corner.
///
/// The exact contact is the box's bottom face on the corner: centre at
/// `a + 44`. Under the rule:
///
/// - Scale 5 (`1 / 5⁴ = 1.6e-3`): every axis is tested, so `5 + 44 = 49`
///   with the normal +Z (the box's own Z axis is first in the list).
/// - Scale 20 (`1 / 20⁴ = 6.25e-6`): the box's three face axes are left
///   out. The octahedron's horizontal edges (1, ±1, 0)/√2 crossed with the
///   box's X edge give the vertical axis, `(1/√2) / 20` long, squared
///   1.25e-3: tested. So still `20 + 44 = 64`, normal +Z.
/// - Scale 128 (`1 / 128⁴ = 3.7e-9`; every edge axis at most `1 / 128²` =
///   6.1e-5): only the four face directions (±1, ±1, 1)/√3 are left. On each
///   the octahedron reaches `a / √3` and the box `(21 + 21 + 44) / √3`, so
///   the box is stopped with its centre at `a + 86 = 214`: 42 uu above the
///   exact contact. All four enter at the same time and the first in the
///   element's list gives the normal.
#[test]
fn a_box_dropped_on_the_corner_of_a_scaled_octahedron() {
    let s = octahedron_scene(scaled(DVec3::splat(5.0)), true);
    let (z, n) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 49.0).abs() < 1.0e-9, "{z}");
    assert_eq!(n, DVec3::Z);
    assert!(!s.statics()[0].native.unwrap().skips_face_axes());

    let s = octahedron_scene(scaled(DVec3::splat(20.0)), true);
    let (z, n) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 64.0).abs() < 1.0e-9, "{z}");
    assert!((n - DVec3::Z).length() < 1.0e-9, "{n}");
    assert!(s.statics()[0].native.unwrap().skips_face_axes());

    let s = octahedron_scene(scaled(DVec3::splat(128.0)), true);
    let (z, n) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 214.0).abs() < 1.0e-6, "{z}");
    let f = 1.0 / 3.0f64.sqrt();
    assert!((n - DVec3::new(f, f, f)).length() < 1.0e-6, "{n}");
    // The same solid without the rule: the exact 128 + 44.
    let mut b = CollisionSceneBuilder::new();
    let (v, t) = octahedron();
    let (faces, edges) = octahedron_directions();
    let m = b.add_convex_hull(v, t, &faces, &edges).unwrap();
    b.add_static(m, scaled(DVec3::splat(128.0)), info(1))
        .unwrap();
    let (z, n) = drop_z(&b.build(), 0.0, 0.0).unwrap();
    assert!((z - 172.0).abs() < 1.0e-9, "{z}");
    assert_eq!(n, DVec3::Z);
}

/// The same with an element that stores no edge direction (two of the
/// shipped game's elements do not): above scale 10 only the face directions
/// are left, so the box stops at `a + 86`; at scale 5 the box's own axes are
/// tested and it stops at `a + 44`.
#[test]
fn an_element_without_edge_directions() {
    let s = octahedron_scene(scaled(DVec3::splat(20.0)), false);
    let (z, _) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 106.0).abs() < 1.0e-6, "{z}");
    let s = octahedron_scene(scaled(DVec3::splat(5.0)), false);
    let (z, n) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 49.0).abs() < 1.0e-9, "{z}");
    assert_eq!(n, DVec3::Z);
}

/// Scale (−20, 20, 1), mirrored in X, no edge directions: the corners are
/// at (±20, 0, 0), (0, ±20, 0) and (0, 0, ±1). The Z face axis has
/// `1 / (20 · 20)²` = 6.25e-6 and is left out; X and Y have `1 / 20²` and
/// stay but do not decide a drop. A face direction (±1, ±1, 1)/√3 of the
/// element is, in world space, along (±1/20, ±1/20, 1) (normals go with the
/// inverse transpose). Unnormalised, the octahedron reaches 1 on it (every
/// corner gives 20 · 1/20 or 1 · 1) and the box
/// `21/20 + 21/20 + 44 = 46.1`; the centre is at `z` on it. The box stops at
/// `z = 1 + 46.1 = 47.1`, where the exact contact is `1 + 44 = 45`.
#[test]
fn a_flat_wide_mirrored_placement_leaves_out_the_vertical_axis_only() {
    let s = octahedron_scene(scaled(DVec3::new(-20.0, 20.0, 1.0)), false);
    let rule = s.statics()[0].native.unwrap();
    assert!(rule.skips_face_axes());
    let (z, n) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 47.1).abs() < 1.0e-6, "{z}");
    // (±0.05, ±0.05, 1) normalised; the first of the list has both signs
    // positive in the element's space, and X is mirrored.
    let len = (1.0f64 + 2.0 * 0.05 * 0.05).sqrt();
    assert!(
        (n - DVec3::new(-0.05, 0.05, 1.0) / len).length() < 1.0e-6,
        "{n}"
    );
}

/// The test before the sweep, through a scene. Octahedron of radius 128
/// without edge directions; only its face directions are tested, so along
/// the X axis the box would be stopped with its centre at `128 + 86 = 214`
/// (the same sum as for the drop: on (1, 1, 1)/√3 the box reaches
/// `(21 + 21 + 44) / √3` whichever way it moves). But the element is swept
/// only when the sweep's bounds reach its bounds in its own space: there the
/// element spans ±1 and the box is `21 / 128` wide plus 0.1, so in world
/// units the sweep must come within `128 + 21 + 12.8 = 161.8` of the centre.
///
/// - 300 → 150 reaches (150 < 161.8): stopped at 214.
/// - 300 → 170 does not: no contact, although 214 lies on the way.
/// - 300 → 161 reaches: stopped at 214.
///
/// The exact contact would be at `128 + 21 = 149`, beyond all three.
#[test]
fn the_precheck_decides_whether_a_wide_solid_is_met_at_all() {
    let s = octahedron_scene(scaled(DVec3::splat(128.0)), false);
    let run = |to: f32| {
        let start = Vec3::new(300.0, 0.0, 0.0);
        let end = Vec3::new(to, 0.0, 0.0);
        let hit = s.sweep_box(&[], start, end, E, QueryFilter::PAWN);
        let brute = s.sweep_box_brute_force(&[], start, end, E, QueryFilter::PAWN);
        assert_eq!(hit, brute);
        hit.map(|h| h.point(start, end).x)
    };
    let x = run(150.0).unwrap();
    assert!((x - 214.0).abs() < 1.0e-6, "{x}");
    assert_eq!(run(170.0), None);
    let x = run(161.0).unwrap();
    assert!((x - 214.0).abs() < 1.0e-6, "{x}");
    assert_eq!(run(162.5), None);
    // The bounds of the instance hold that region: 128 + 12.8 on every axis
    // (the box's own 21 is added by every query).
    let b = s.statics()[0].bounds;
    for a in 0..3 {
        assert!((f64::from(b.max[a]) - 140.8).abs() < 1.0e-3, "{b:?}");
        assert!((f64::from(b.min[a]) + 140.8).abs() < 1.0e-3, "{b:?}");
    }
}

/// A triangle under the rule, kernel level. An edge axis is the unit edge
/// (in the mesh's space) crossed with a carried box edge, `sin θ / s` long,
/// and is left out below a squared length of 1e-8: at a scale of 20,000
/// every one of the nine goes (`1 / 20,000²` = 2.5e-9), at 5,000 only those
/// within 30° of a box edge (`sin θ < 0.5`).
///
/// The triangle lies flat at z = 0 with the corners (0, 0), (1000, 1000) and
/// (1000, 0): the part of the plane with 0 ≤ y ≤ x ≤ 1000. A box
/// (21, 21, 44) dropped at (0, 45) covers x ∈ [−21, 21], y ∈ [24, 66]; its
/// corner nearest the triangle, (21, 24), is 3 beyond the diagonal edge
/// y = x, so the exact test finds no contact: the separating axis is the
/// diagonal edge crossed with the box's Z edge, the horizontal direction
/// (−1, 1, 0)/√2 (90° between the two, so it is tested up to a scale of
/// 10,000). With every edge axis left out, what remains is the box's own
/// three axes and the triangle's normal: on X the triangle spans [0, 1000]
/// and the box [−21, 21], on Y [0, 1000] and [24, 66], so neither separates,
/// and the box is stopped by the plane with its centre at z = 44.
#[test]
fn a_triangle_loses_its_edge_axes_at_a_very_large_scale() {
    let tri = [
        DVec3::new(0.0, 0.0, 0.0),
        DVec3::new(1000.0, 1000.0, 0.0),
        DVec3::new(1000.0, 0.0, 0.0),
    ];
    let s = DVec3::new(0.0, 45.0, 400.0);
    let d = DVec3::new(0.0, 0.0, -400.0);
    assert_eq!(
        box_sweep::sweep_triangle(s, d, ED, &tri, None, TOL, 1.0),
        None
    );
    let rule = |scale: f64| AxisSkip::triangles(edges_of_scale(DVec3::splat(scale)));
    let c = box_sweep::sweep_triangle_skipping(s, d, ED, &tri, None, TOL, 1.0, &rule(20_000.0))
        .unwrap();
    assert_eq!(c.t, 356.0 / 400.0);
    assert_eq!(c.normal, DVec3::Z);
    assert!(!c.penetrating);
    // At 5,000 the deciding axis (90° from the box's Z edge) is tested.
    assert_eq!(
        box_sweep::sweep_triangle_skipping(s, d, ED, &tri, None, TOL, 1.0, &rule(5000.0)),
        None
    );
    // At scale 1 the rule is the exact test's own (parallel pairs only),
    // contact or not.
    for (s, d) in [
        (s, d),
        (DVec3::new(0.0, 40.0, 400.0), d),
        (
            DVec3::new(500.0, 80.0, 200.0),
            DVec3::new(-30.0, 160.0, -300.0),
        ),
        (
            DVec3::new(-200.0, 300.0, 10.0),
            DVec3::new(600.0, -50.0, 0.0),
        ),
    ] {
        assert_eq!(
            box_sweep::sweep_triangle_skipping(s, d, ED, &tri, None, TOL, 1.0, &rule(1.0)),
            box_sweep::sweep_triangle(s, d, ED, &tri, None, TOL, 1.0)
        );
    }
    // The drop at (0, 40) does land in both: the corner (21, 19) is over the
    // triangle.
    let over = DVec3::new(0.0, 40.0, 400.0);
    let c = box_sweep::sweep_triangle(over, d, ED, &tri, None, TOL, 1.0).unwrap();
    assert_eq!((c.t, c.normal), (356.0 / 400.0, DVec3::Z));
}

/// A deterministic generator (xorshift64*), so the random scenes below are
/// the same on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[lo, hi)`.
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random placement: any rotation (or a quarter turn, or none), a scale
/// from 0.5 to 200 per axis (uniform or not, sometimes mirrored).
fn random_placement(rng: &mut Rng, span: f64) -> Affine {
    let rot = match rng.pick(3) {
        0 => [DVec3::X, DVec3::Y, DVec3::Z],
        1 => [DVec3::Y, DVec3::NEG_X, DVec3::Z],
        _ => {
            let (a, b, c) = (
                rng.range(0.0, std::f64::consts::TAU),
                rng.range(-1.2, 1.2),
                rng.range(0.0, std::f64::consts::TAU),
            );
            let x = DVec3::new(a.cos() * b.cos(), a.sin() * b.cos(), b.sin());
            let up = DVec3::new(c.cos() * 0.3, c.sin() * 0.3, 1.0);
            let y = up.cross(x).normalize();
            let z = x.cross(y);
            [x, y, z]
        }
    };
    let log = |rng: &mut Rng| (rng.range((0.5f64).ln(), (200.0f64).ln())).exp();
    let mut s = if rng.pick(2) == 0 {
        DVec3::splat(log(rng))
    } else {
        DVec3::new(log(rng), log(rng), log(rng))
    };
    if rng.pick(5) == 0 {
        s.x = -s.x;
    }
    Affine {
        rows: [rot[0] * s.x, rot[1] * s.y, rot[2] * s.z],
        translation: DVec3::new(
            rng.range(-span, span),
            rng.range(-span, span),
            rng.range(-span, span),
        ),
    }
}

/// The hierarchy and the reference without one give the same answer, bit
/// for bit, for instances under the rule at every scale and turn, static and
/// dynamic: 4,000 sweeps of a box of a random size against 150 solids and
/// 30 triangle meshes. (For solids that leave a face axis out this includes
/// the widened bounds, which both paths apply.)
#[test]
fn the_hierarchy_agrees_with_the_reference_under_the_rule() {
    let mut rng = Rng(0x5EED_0B0C_0FFE_E123);
    let mut b = CollisionSceneBuilder::new();
    let (ov, ot) = octahedron();
    let (of, oe) = octahedron_directions();
    let with_edges = b.add_convex_hull(ov.clone(), ot.clone(), &of, &oe).unwrap();
    let without = b.add_convex_hull(ov.clone(), ot.clone(), &of, &[]).unwrap();
    let cube = b.add_box_mesh(Vec3::new(1.0, 0.6, 0.3)).unwrap();
    // A fan of triangles around the local Z axis.
    let mut fv = vec![Vec3::ZERO];
    let mut ft = Vec::new();
    for k in 0..12u32 {
        let a = f64::from(k) * std::f64::consts::TAU / 12.0;
        fv.push(Vec3::new(
            a.cos() as f32,
            a.sin() as f32,
            (0.2 * (3.0 * a).sin()) as f32,
        ));
        ft.push([0, k + 1, (k + 1) % 12 + 1]);
    }
    let fan = b.add_mesh(fv, ft).unwrap();
    let mut wide = 0usize;
    let mut dynamic: Vec<(u32, Affine, u32)> = Vec::new();
    for k in 0..180u32 {
        let mesh = match k % 6 {
            0 | 1 => with_edges,
            2 => without,
            3 | 4 => cube,
            _ => fan,
        };
        let to_world = random_placement(&mut rng, 1200.0);
        if k % 9 == 0 {
            dynamic.push((mesh, to_world, k));
            continue;
        }
        let kind = if mesh == fan {
            PrimitiveKind::TriangleMesh
        } else {
            PrimitiveKind::SimpleHull
        };
        let i = b
            .add_static_with(mesh, to_world, info(k), InstanceOptions::of_kind(kind))
            .unwrap();
        assert!(b.set_static_native(i, None));
    }
    let scene = b.build();
    let dynamic: Vec<Instance> = dynamic
        .into_iter()
        .map(|(mesh, to_world, k)| {
            let mut inst = scene.dynamic_instance(mesh, to_world, info(k)).unwrap();
            assert!(scene.set_native(&mut inst, None));
            inst
        })
        .collect();
    for inst in scene.statics().iter().chain(&dynamic) {
        if inst.native.is_some_and(|r| r.skips_face_axes()) {
            wide += 1;
        }
    }
    assert!(wide > 30, "{wide} solids leave a face axis out");
    let (mut hits, mut early) = (0usize, 0usize);
    for _ in 0..4000 {
        let e = Vec3::new(
            rng.range(5.0, 60.0) as f32,
            rng.range(5.0, 60.0) as f32,
            rng.range(5.0, 100.0) as f32,
        );
        assert!(f64::from(e.max_element()) <= NATIVE_RULE_MAX_EXTENT);
        let start = Vec3::new(
            rng.range(-1600.0, 1600.0) as f32,
            rng.range(-1600.0, 1600.0) as f32,
            rng.range(-1600.0, 1600.0) as f32,
        );
        let len = match rng.pick(3) {
            0 => rng.range(0.5, 40.0),
            1 => rng.range(40.0, 600.0),
            _ => rng.range(600.0, 5000.0),
        };
        let dir = DVec3::new(
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
        )
        .normalize();
        let end = start + (dir * len).as_vec3();
        let a = scene.sweep_box(&dynamic, start, end, e, QueryFilter::PAWN);
        let r = scene.sweep_box_brute_force(&dynamic, start, end, e, QueryFilter::PAWN);
        assert_eq!(a, r, "{start} -> {end} with {e}");
        if let Some(h) = a {
            hits += 1;
            assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t));
            assert!((h.normal.length() - 1.0).abs() < 1.0e-9);
            if scene
                .instance(&dynamic, h.instance)
                .is_some_and(|i| i.native.is_some_and(|r| r.skips_face_axes()))
            {
                early += 1;
            }
        }
    }
    eprintln!("{wide} wide solids, {hits} hits, {early} on wide solids");
    assert!(hits > 100, "{hits} hits");
    assert!(
        early > 20,
        "{early} hits on solids that leave a face axis out"
    );
}

/// A solid under the rule that is moved keeps the rule, and the rule follows
/// its new scale: an octahedron placed at scale 5 and moved to scale 128
/// leaves the box's face axes out afterwards (and its bounds widen), and the
/// other way round.
#[test]
fn a_moved_instance_keeps_the_rule_and_follows_its_new_scale() {
    let scene = octahedron_scene(scaled(DVec3::splat(5.0)), false);
    let mut inst = scene.statics()[0].clone();
    assert!(!inst.native.unwrap().skips_face_axes());
    assert!(scene.place_dynamic(&mut inst, scaled(DVec3::splat(128.0))));
    assert!(inst.native.unwrap().skips_face_axes());
    assert!((f64::from(inst.bounds.max.x) - 140.8).abs() < 1.0e-3);
    // Swept as a dynamic instance: the rule's 128 + 86.
    let start = Vec3::new(0.0, 0.0, 400.0);
    let end = Vec3::new(0.0, 0.0, 0.0);
    let mut off = scene.clone();
    off.set_static_enabled(0, false);
    let h = off
        .sweep_box(
            std::slice::from_ref(&inst),
            start,
            end,
            E,
            QueryFilter::PAWN,
        )
        .unwrap();
    assert!((h.point(start, end).z - 214.0).abs() < 1.0e-6);
    assert!(scene.place_dynamic(&mut inst, scaled(DVec3::splat(5.0))));
    assert!(!inst.native.unwrap().skips_face_axes());
    assert!((f64::from(inst.bounds.max.x) - 5.0).abs() < 1.0e-3);
}

/// A volume's hulls are stored in world space; the rule needs the space of
/// the volume's own component. An octahedron of radius 128 written out in
/// world coordinates, with the component's transform (scale 128) given
/// beside it, is met as the same solid placed by that transform.
#[test]
fn hulls_stored_in_world_space_use_their_components_space() {
    let to_world = scaled(DVec3::splat(128.0));
    let mut b = CollisionSceneBuilder::new();
    let (v, t) = octahedron();
    let (faces, _) = octahedron_directions();
    let world: Vec<Vec3> = v.iter().map(|p| *p * 128.0).collect();
    let m = b.add_convex_hull(world, t, &faces, &[]).unwrap();
    let i = b
        .add_static_with(
            m,
            Affine::IDENTITY,
            info(1),
            InstanceOptions::of_kind(PrimitiveKind::BrushHull),
        )
        .unwrap();
    // With the identity as its space the solid would be at scale 1.
    assert!(b.set_static_native(i, Some(to_world)));
    let s = b.build();
    assert!(s.statics()[0].native.unwrap().skips_face_axes());
    let (z, _) = drop_z(&s, 0.0, 0.0).unwrap();
    assert!((z - 214.0).abs() < 1.0e-6, "{z}");
    // A transform without an inverse is refused and changes nothing.
    let mut b = CollisionSceneBuilder::new();
    let (v, t) = octahedron();
    let m = b.add_convex_mesh(v, t).unwrap();
    let i = b.add_static(m, Affine::IDENTITY, info(1)).unwrap();
    let flat = scaled(DVec3::new(1.0, 1.0, 0.0));
    assert!(!b.set_static_native(i, Some(flat)));
    assert!(!b.set_static_native(99, None));
    assert!(b.build().statics()[0].native.is_none());
}
