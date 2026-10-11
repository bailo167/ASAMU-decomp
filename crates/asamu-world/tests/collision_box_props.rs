//! Property tests of the swept box (synthetic data only, fixed seeds).
//!
//! - The box sweep against single triangles is checked against an oracle
//!   that shares no code with it: the time at which points sampled densely
//!   on the triangle enter the moving box (three slab intervals per point).
//!   A reported contact must be real and must be the first; a miss must
//!   never let a sample in.
//! - A convex solid and the same triangles as a plain two-sided mesh must
//!   stop a box that starts outside at the same time with the same normal,
//!   under rotations, non-uniform scales and mirroring.
//! - The two-level BVH queries are compared with the brute-force reference
//!   on random instanced scenes: results must be bit-identical.

#![allow(clippy::unwrap_used)]

use asamu_world::SurfaceTag;
use asamu_world::collision::box_sweep::{overlaps_triangle, sweep_triangle};
use asamu_world::collision::{
    Affine, CollisionClass, CollisionSceneBuilder, InstanceInfo, InstanceOptions, PrimitiveKind,
    QueryFilter, QueryKind,
};
use glam::{DVec3, Vec3};

/// SplitMix64: a tiny deterministic generator for test data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
    fn vec(&mut self, e: f64) -> DVec3 {
        DVec3::new(self.range(-e, e), self.range(-e, e), self.range(-e, e))
    }
    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// `f32`-representable vector (the world stores `f32` vertices).
fn f32ify(v: DVec3) -> DVec3 {
    v.as_vec3().as_dvec3()
}

fn random_triangle(rng: &mut Rng) -> [DVec3; 3] {
    let c = rng.vec(40.0);
    match rng.pick(6) {
        // Horizontal.
        0 => [0, 1, 2].map(|_| {
            f32ify(DVec3::new(
                c.x + rng.range(-60.0, 60.0),
                c.y + rng.range(-60.0, 60.0),
                c.z,
            ))
        }),
        // In a plane x = const (axis-aligned wall).
        1 => [0, 1, 2].map(|_| {
            f32ify(DVec3::new(
                c.x,
                c.y + rng.range(-60.0, 60.0),
                c.z + rng.range(-60.0, 60.0),
            ))
        }),
        // One edge along a world axis.
        2 => {
            let b = c + DVec3::Y * rng.range(-60.0, 60.0);
            [f32ify(c), f32ify(b), f32ify(c + rng.vec(60.0))]
        }
        // Sliver.
        3 => {
            let b = c + rng.vec(60.0);
            let cc = c + (b - c) * rng.unit() + rng.vec(0.5);
            [f32ify(c), f32ify(b), f32ify(cc)]
        }
        // Small (the box meets it with a face, an edge or a corner).
        4 => [0, 1, 2].map(|_| f32ify(c + rng.vec(4.0))),
        _ => [0, 1, 2].map(|_| f32ify(c + rng.vec(60.0))),
    }
}

/// Earliest `t ∈ [0, 1]` at which the point `p` is inside the closed box
/// with half extents `e` centred at `s + t·d`.
fn point_entry(p: DVec3, s: DVec3, d: DVec3, e: DVec3) -> Option<f64> {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let q = p[k] - s[k];
        if d[k] == 0.0 {
            if q.abs() > e[k] {
                return None;
            }
        } else {
            let (a, b) = ((q - e[k]) / d[k], (q + e[k]) / d[k]);
            lo = lo.max(a.min(b));
            hi = hi.min(a.max(b));
        }
    }
    (lo <= hi).then_some(lo)
}

/// A barycentric grid over the triangle (vertices and edges included) and
/// its spacing: every point of the triangle is within `spacing` of a sample.
fn grid(t: &[DVec3; 3], target: f64) -> (Vec<DVec3>, f64) {
    let e = (t[1] - t[0])
        .length()
        .max((t[2] - t[0]).length())
        .max((t[2] - t[1]).length());
    let n = ((e / target).ceil() as usize).clamp(1, 1000);
    let mut out = Vec::new();
    for i in 0..=n {
        for j in 0..=(n - i) {
            let (u, v) = (i as f64 / n as f64, j as f64 / n as f64);
            out.push(t[0] + (t[1] - t[0]) * u + (t[2] - t[0]) * v);
        }
    }
    (out, e / n as f64)
}

fn earliest(samples: &[DVec3], s: DVec3, d: DVec3, e: DVec3) -> Option<f64> {
    samples
        .iter()
        .filter_map(|p| point_entry(*p, s, d, e))
        .reduce(f64::min)
}

const E: DVec3 = DVec3::new(21.0, 21.0, 44.0);

/// A reported contact must have a sample inside the box grown by the sample
/// spacing no later than it (no phantom contacts: at the contact some point
/// of the triangle is on the box, and a sample lies within the spacing of
/// that point). No sample may be inside the box shrunk by twice the
/// tolerance before it (no earlier contact was missed). A miss must not let
/// any sample into that shrunk box.
#[test]
fn box_sweep_agrees_with_a_point_sampling_oracle() {
    let mut rng = Rng(0x0B0C_5EED);
    let tol = 1.0e-3;
    let (mut cases, mut hits, mut axis_normals, mut other_normals) = (0, 0, 0, 0);
    while cases < 2500 {
        let tri = random_triangle(&mut rng);
        let (u, v) = (rng.unit(), rng.unit());
        let (u, v) = if u + v > 1.0 {
            (1.0 - u, 1.0 - v)
        } else {
            (u, v)
        };
        let target = tri[0] + (tri[1] - tri[0]) * u + (tri[2] - tri[0]) * v;
        let dir = match rng.pick(4) {
            0 => DVec3::Z * if rng.pick(2) == 0 { 1.0 } else { -1.0 },
            1 => DVec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), 0.0).normalize_or_zero(),
            2 => DVec3::X * if rng.pick(2) == 0 { 1.0 } else { -1.0 },
            _ => rng.vec(1.0).normalize_or_zero(),
        };
        // Aim at the triangle from a distance, off-centre by up to the
        // box's own size so that faces, edges and corners all lead.
        let s = f32ify(target - dir * rng.range(80.0, 200.0) + rng.vec(30.0));
        let d = f32ify((target - s) * rng.range(0.3, 1.5));
        if d == DVec3::ZERO {
            continue;
        }
        let (samples, spacing) = grid(&tri, 0.25);
        let margin = spacing + 0.05;
        if earliest(&samples, s, DVec3::ZERO, E + DVec3::splat(margin)).is_some()
            || overlaps_triangle(s, E + DVec3::splat(0.05), &tri)
        {
            continue;
        }
        cases += 1;
        let shrunk = earliest(&samples, s, d, E - DVec3::splat(2.0 * tol));
        match sweep_triangle(s, d, E, &tri, None, tol, 1.0) {
            Some(c) => {
                hits += 1;
                assert!(!c.penetrating && c.t > 0.0 && c.t <= 1.0, "{c:?}");
                assert!((c.normal.length() - 1.0).abs() < 1e-12);
                assert!(c.normal.dot(d) < 0.0, "normal along the motion: {c:?}");
                let grown = earliest(&samples, s, d, E + DVec3::splat(spacing + 1e-6));
                assert!(
                    grown.is_some_and(|g| g <= c.t + 1e-9),
                    "phantom contact t={} (grown {grown:?}): s={s} d={d} tri={tri:?}",
                    c.t
                );
                assert!(
                    shrunk.is_none_or(|e| e >= c.t - 1e-9),
                    "earlier contact at {shrunk:?} missed (reported {}): s={s} d={d} tri={tri:?}",
                    c.t
                );
                if c.normal.abs().max_element() == 1.0 {
                    axis_normals += 1;
                } else {
                    other_normals += 1;
                }
            }
            None => assert!(
                shrunk.is_none(),
                "missed contact at {shrunk:?}: s={s} d={d} tri={tri:?}"
            ),
        }
    }
    assert!(hits > 800, "too few hits: {hits}");
    // Both families of contact normal occur: a world axis (a box face on a
    // vertex or an edge) and others (the triangle's face, edge against edge).
    assert!(
        axis_normals > 100 && other_normals > 100,
        "{axis_normals} {other_normals}"
    );
}

fn info(rng: &mut Rng) -> InstanceInfo {
    InstanceInfo {
        actor: Some(rng.pick(1000) as u32),
        class: if rng.pick(5) == 0 {
            CollisionClass::WorldGeometry
        } else {
            CollisionClass::StaticMesh
        },
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: rng.pick(8) != 0,
        blocks_traces: rng.pick(8) != 0,
        sublevel: rng.pick(2) as u8,
    }
}

fn solid_info() -> InstanceInfo {
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

fn random_affine(rng: &mut Rng, spread: f64) -> Affine {
    // Rotation from three random angles, non-uniform scale, sometimes mirrored.
    let (a, b, c) = (
        rng.range(0.0, std::f64::consts::TAU),
        rng.range(0.0, std::f64::consts::TAU),
        rng.range(0.0, std::f64::consts::TAU),
    );
    let rot = |v: DVec3| {
        let v = DVec3::new(
            v.x * a.cos() - v.y * a.sin(),
            v.x * a.sin() + v.y * a.cos(),
            v.z,
        );
        let v = DVec3::new(
            v.x * b.cos() + v.z * b.sin(),
            v.y,
            -v.x * b.sin() + v.z * b.cos(),
        );
        DVec3::new(
            v.x,
            v.y * c.cos() - v.z * c.sin(),
            v.y * c.sin() + v.z * c.cos(),
        )
    };
    let mut scale = DVec3::new(
        rng.range(0.3, 3.0),
        rng.range(0.3, 3.0),
        rng.range(0.3, 3.0),
    );
    if rng.pick(4) == 0 {
        scale.x = -scale.x;
    }
    Affine {
        rows: [
            rot(DVec3::X) * scale.x,
            rot(DVec3::Y) * scale.y,
            rot(DVec3::Z) * scale.z,
        ],
        translation: rng.vec(spread),
    }
}

fn random_mesh(rng: &mut Rng) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let n = 3 + rng.pick(60) as usize;
    let vertices: Vec<Vec3> = (0..n).map(|_| rng.vec(200.0).as_vec3()).collect();
    let tris = (0..n * 2)
        .map(|_| {
            [
                rng.pick(n as u64) as u32,
                rng.pick(n as u64) as u32,
                rng.pick(n as u64) as u32,
            ]
        })
        .collect();
    (vertices, tris)
}

/// Closed convex polyhedra with outward triangles: a block, a tetrahedron
/// and an octahedron.
fn solids() -> Vec<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let block: Vec<Vec3> = (0..8)
        .map(|i| {
            Vec3::new(
                (i & 1) as f32 * 120.0,
                ((i >> 1) & 1) as f32 * 80.0,
                ((i >> 2) & 1) as f32 * 50.0,
            )
        })
        .collect();
    let block_tris = vec![
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
    let tetra = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(90.0, 10.0, 0.0),
        Vec3::new(20.0, 100.0, 5.0),
        Vec3::new(30.0, 30.0, 110.0),
    ];
    let tetra_tris = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
    let octa = vec![
        Vec3::new(70.0, 0.0, 0.0),
        Vec3::new(-70.0, 0.0, 0.0),
        Vec3::new(0.0, 50.0, 0.0),
        Vec3::new(0.0, -50.0, 0.0),
        Vec3::new(0.0, 0.0, 90.0),
        Vec3::new(0.0, 0.0, -90.0),
    ];
    let octa_tris = vec![
        [0, 2, 4],
        [2, 1, 4],
        [1, 3, 4],
        [3, 0, 4],
        [2, 0, 5],
        [1, 2, 5],
        [3, 1, 5],
        [0, 3, 5],
    ];
    vec![(block, block_tris), (tetra, tetra_tris), (octa, octa_tris)]
}

/// The solid test (face directions, edge directions and the box's axes
/// against all the vertices) and the triangle-by-triangle test of the same
/// surface are two different computations of the same first contact for a
/// box that starts outside: equal times; equal normals except on ties
/// between axes, which random sweeps do not produce.
#[test]
fn a_convex_solid_agrees_with_its_triangles_from_outside() {
    let mut rng = Rng(0x50_11D5);
    let half = Vec3::new(21.0, 21.0, 44.0);
    let (mut hits, mut misses, mut deeper_overlaps) = (0, 0, 0);
    for round in 0..60 {
        let (v, t) = solids().swap_remove(round % 3);
        let to_world = if round < 3 {
            Affine::IDENTITY
        } else {
            random_affine(&mut rng, 200.0)
        };
        let mut a = CollisionSceneBuilder::new();
        let m = a.add_convex_mesh(v.clone(), t.clone()).unwrap();
        assert!(a.mesh(m).unwrap().hull().is_some(), "round {round}");
        a.add_static(m, to_world, solid_info()).unwrap();
        let a = a.build();
        let mut b = CollisionSceneBuilder::new();
        let m = b.add_mesh(v, t).unwrap();
        assert!(b.mesh(m).unwrap().hull().is_none());
        b.add_static(m, to_world, solid_info()).unwrap();
        let b = b.build();
        for _ in 0..150 {
            let s = (to_world.translation + rng.vec(700.0)).as_vec3();
            let e = (to_world.translation + rng.vec(300.0)).as_vec3();
            if b.overlaps_box(&[], s, half + Vec3::splat(1.0), QueryFilter::PAWN)
                || a.overlaps_box(&[], s, half + Vec3::splat(1.0), QueryFilter::PAWN)
            {
                continue;
            }
            let solid = a.sweep_box(&[], s, e, half, QueryFilter::PAWN);
            let soup = b.sweep_box(&[], s, e, half, QueryFilter::PAWN);
            assert_eq!(
                a.sweep_box_brute_force(&[], s, e, half, QueryFilter::PAWN),
                solid
            );
            match (solid, soup) {
                (Some(x), Some(y)) => {
                    hits += 1;
                    assert!((x.t - y.t).abs() < 1e-9, "{x:?} vs {y:?} ({s} -> {e})");
                    assert!((x.normal - y.normal).length() < 1e-6, "{x:?} vs {y:?}");
                    assert!(!x.penetrating && !y.penetrating);
                    assert_eq!(x.kind, PrimitiveKind::BrushHull);
                    assert_eq!(y.kind, PrimitiveKind::TriangleMesh);
                    // Free at the contact; a little further in along the
                    // normal it overlaps (unless the contact was a near
                    // graze, where a second axis barely overlaps).
                    let at = x.point(s, e);
                    assert!(!a.overlaps_box(&[], at.as_vec3(), half, QueryFilter::PAWN));
                    let deeper = (at - x.normal * 0.1).as_vec3();
                    if a.overlaps_box(&[], deeper, half, QueryFilter::PAWN)
                        && b.overlaps_box(&[], deeper, half, QueryFilter::PAWN)
                    {
                        deeper_overlaps += 1;
                    }
                }
                (None, None) => misses += 1,
                (x, y) => panic!("solid {x:?} but triangles {y:?} ({s} -> {e})"),
            }
        }
    }
    assert!(hits > 1000 && misses > 500, "{hits} hits, {misses} misses");
    assert!(
        deeper_overlaps * 100 >= hits * 98,
        "{deeper_overlaps} of {hits}"
    );
}

#[test]
fn bvh_box_queries_equal_brute_force() {
    let mut rng = Rng(0xB0C5);
    let mut b = CollisionSceneBuilder::new();
    let mut meshes: Vec<u32> = (0..12)
        .filter_map(|_| {
            let (v, t) = random_mesh(&mut rng);
            b.add_mesh(v, t)
        })
        .collect();
    for (v, t) in solids() {
        meshes.push(b.add_convex_mesh(v, t).unwrap());
    }
    // A convex element with its own direction lists, and a box element.
    let (v, t) = solids().swap_remove(0);
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    meshes.push(b.add_convex_hull(v, t, &axes, &axes).unwrap());
    meshes.push(b.add_box_mesh(Vec3::new(40.0, 25.0, 60.0)).unwrap());
    // A random triangle soup declared convex: no solid, one-sided triangles.
    let (v, t) = random_mesh(&mut rng);
    let fake = b.add_convex_mesh(v, t).unwrap();
    assert!(b.mesh(fake).unwrap().hull().is_none());
    meshes.push(fake);
    let kinds = [
        None,
        Some(PrimitiveKind::Bsp),
        Some(PrimitiveKind::BrushHull),
        Some(PrimitiveKind::SimpleHull),
        Some(PrimitiveKind::TriangleMesh),
    ];
    let options = |rng: &mut Rng| InstanceOptions {
        kind: kinds[rng.pick(kinds.len() as u64) as usize],
        enabled: rng.pick(6) != 0,
    };
    for _ in 0..500 {
        let m = meshes[rng.pick(meshes.len() as u64) as usize];
        let a = random_affine(&mut rng, 3000.0);
        let i = info(&mut rng);
        let o = options(&mut rng);
        b.add_static_with(m, a, i, o);
    }
    let mut scene = b.build();
    let pawn = scene.add_cylinder_mesh(34.0, 78.0).unwrap();
    let mut dynamic: Vec<_> = (0..12)
        .filter_map(|_| {
            let m = meshes[rng.pick(meshes.len() as u64) as usize];
            let a = random_affine(&mut rng, 3000.0);
            let i = info(&mut rng);
            let o = options(&mut rng);
            scene.dynamic_instance_with(m, a, i, o)
        })
        .collect();
    for _ in 0..6 {
        let at = Affine::from_translation(rng.vec(2000.0));
        dynamic.push(scene.dynamic_instance(pawn, at, info(&mut rng)).unwrap());
    }
    let filters = [
        QueryFilter::PAWN,
        QueryFilter::TRACE,
        QueryFilter {
            kind: QueryKind::Pawn,
            sublevels: 1,
        },
    ];
    let halves = [
        Vec3::new(21.0, 21.0, 44.0),
        Vec3::new(21.0, 21.0, 44.0),
        Vec3::new(8.0, 30.0, 5.0),
        Vec3::new(60.0, 2.0, 90.0),
    ];
    // Random sweeps, then sweeps aimed at the middle of instances (static
    // and dynamic) from 600 uu away, so that every kind of shape is met.
    let mut aims: Vec<DVec3> = scene
        .statics()
        .iter()
        .take(200)
        .chain(dynamic.iter())
        .map(|i| (i.bounds.min.as_dvec3() + i.bounds.max.as_dvec3()) * 0.5)
        .collect();
    let (mut hits, mut starts, mut solids_hit, mut cylinders_hit) = (0, 0, 0, 0);
    for k in 0..4000 + aims.len() {
        let aim = if k >= 4000 { aims.pop() } else { None };
        let (s, e) = match aim {
            Some(c) => {
                let from = c + rng.vec(1.0).normalize_or(DVec3::Z) * 600.0;
                (from.as_vec3(), c.as_vec3())
            }
            None => {
                let s = rng.vec(3500.0).as_vec3();
                (s, (s.as_dvec3() + rng.vec(1500.0)).as_vec3())
            }
        };
        let f = filters[k % 3];
        let half = halves[k % 4];
        let fast = scene.sweep_box(&dynamic, s, e, half, f);
        let slow = scene.sweep_box_brute_force(&dynamic, s, e, half, f);
        assert_eq!(fast, slow, "box {s} -> {e}");
        if let Some(h) = fast {
            hits += 1;
            let inst = scene.instance(&dynamic, h.instance).unwrap();
            assert!(inst.enabled, "a disabled instance was hit");
            assert_eq!(inst.kind, h.kind);
            assert!((h.normal.length() - 1.0).abs() < 1e-9);
            assert!((0.0..=1.0).contains(&h.t));
            if h.t == 0.0 {
                starts += 1;
            }
            let mesh = &scene.meshes()[inst.mesh as usize];
            if mesh.hull().is_some() {
                solids_hit += 1;
            }
            if h.kind == PrimitiveKind::PawnCylinder {
                cylinders_hit += 1;
            }
        }
        // The cylinder and ray queries still agree with their references.
        assert_eq!(
            scene.sweep_cylinder(&dynamic, s, e, 21.0, 44.0, f),
            scene.sweep_cylinder_brute_force(&dynamic, s, e, 21.0, 44.0, f)
        );
        assert_eq!(
            scene.raycast(&dynamic, s, e, f),
            scene.raycast_brute_force(&dynamic, s, e, f)
        );
    }
    assert!(hits > 400, "{hits}");
    assert!(
        starts > 10 && solids_hit > 20 && cylinders_hit > 0,
        "{starts} {solids_hit} {cylinders_hit}"
    );
    // Switching instances changes both paths alike.
    for i in 0..scene.statics().len() as u32 {
        let on = scene.statics()[i as usize].enabled;
        assert!(scene.set_static_enabled(i, !on));
    }
    let mut hits = 0;
    for k in 0..1000 {
        let s = rng.vec(3500.0).as_vec3();
        let e = (s.as_dvec3() + rng.vec(1500.0)).as_vec3();
        let f = filters[k % 3];
        let fast = scene.sweep_box(&dynamic, s, e, halves[0], f);
        assert_eq!(
            fast,
            scene.sweep_box_brute_force(&dynamic, s, e, halves[0], f)
        );
        hits += usize::from(fast.is_some());
    }
    assert!(hits > 3, "{hits}");
    // Same query on a copy of the scene: identical (deterministic build).
    let s = Vec3::new(10.0, 20.0, 3000.0);
    let e = Vec3::new(10.0, 20.0, -3000.0);
    let again = scene.clone();
    assert_eq!(
        scene.sweep_box(&dynamic, s, e, halves[0], QueryFilter::PAWN),
        again.sweep_box(&dynamic, s, e, halves[0], QueryFilter::PAWN)
    );
}

#[test]
fn box_performance_smoke_test() {
    // A 256 × 256-cell terrain (131 072 triangles) as 64 instances of one
    // 32 × 32-cell patch, 3 000 small triangle meshes and 3 000 convex
    // solids; 2 000 short box sweeps and as many cylinder sweeps for
    // comparison. Timing is only asserted in release builds.
    let mut b = CollisionSceneBuilder::new();
    let n = 32;
    let mut vertices = Vec::new();
    let mut tris = Vec::new();
    for y in 0..=n {
        for x in 0..=n {
            let h = ((x as f32 * 0.7).sin() + (y as f32 * 0.4).cos()) * 30.0;
            vertices.push(Vec3::new(x as f32 * 100.0, y as f32 * 100.0, h));
        }
    }
    for y in 0..n {
        for x in 0..n {
            let i = (y * (n + 1) + x) as u32;
            let j = i + (n + 1) as u32;
            tris.push([i, i + 1, j + 1]);
            tris.push([i, j + 1, j]);
        }
    }
    let patch = b.add_mesh(vertices, tris).unwrap();
    let mut rng = Rng(1);
    for py in 0..8 {
        for px in 0..8 {
            let t = DVec3::new(f64::from(px) * 3200.0, f64::from(py) * 3200.0, 0.0);
            b.add_static(patch, Affine::from_translation(t), solid_info());
        }
    }
    let (cv, ct) = random_mesh(&mut rng);
    let small = b.add_mesh(cv, ct).unwrap();
    let hulls: Vec<u32> = solids()
        .into_iter()
        .map(|(v, t)| b.add_convex_mesh(v, t).unwrap())
        .collect();
    for k in 0..6000 {
        let mut a = random_affine(&mut rng, 0.0);
        a.translation = DVec3::new(
            rng.range(0.0, 25600.0),
            rng.range(0.0, 25600.0),
            rng.range(0.0, 300.0),
        );
        let mesh = if k % 2 == 0 { small } else { hulls[k % 3] };
        b.add_static(mesh, a, solid_info());
    }
    let scene = b.build();
    assert!(scene.stats().instance_triangles > 131_072);
    let queries: Vec<(Vec3, Vec3)> = (0..2000)
        .map(|_| {
            let p = Vec3::new(
                rng.range(500.0, 25000.0) as f32,
                rng.range(500.0, 25000.0) as f32,
                200.0,
            );
            (p, p + rng.vec(20.0).as_vec3() - Vec3::Z * 300.0)
        })
        .collect();
    let half = Vec3::new(21.0, 21.0, 44.0);
    let start = std::time::Instant::now();
    let box_hits = queries
        .iter()
        .filter(|(p, q)| {
            scene
                .sweep_box(&[], *p, *q, half, QueryFilter::PAWN)
                .is_some()
        })
        .count();
    let box_time = start.elapsed();
    let start = std::time::Instant::now();
    let cylinder_hits = queries
        .iter()
        .filter(|(p, q)| {
            scene
                .sweep_cylinder(&[], *p, *q, 21.0, 44.0, QueryFilter::PAWN)
                .is_some()
        })
        .count();
    let cylinder_time = start.elapsed();
    let per_query_us = box_time.as_secs_f64() * 1e6 / queries.len() as f64;
    eprintln!(
        "box: {box_hits} hits, {per_query_us:.1} us per query ({box_time:?}); \
         cylinder: {cylinder_hits} hits ({cylinder_time:?})"
    );
    // Every sweep ends below the terrain, and a box reaches whatever the
    // cylinder inside it reaches.
    assert_eq!(box_hits, queries.len());
    assert_eq!(cylinder_hits, queries.len());
    if !cfg!(debug_assertions) {
        assert!(per_query_us < 150.0, "{per_query_us} us per query");
    }
}
