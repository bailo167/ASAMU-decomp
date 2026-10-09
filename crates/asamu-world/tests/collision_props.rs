//! Property tests of the triangle collision (synthetic data only).
//!
//! - The exact cylinder sweep against single triangles is checked against an
//!   independent oracle: the static overlap test (triangle clipped to the
//!   cylinder's height slab, 2-D distance to the axis) sampled along the
//!   motion. A reported contact must be real (a slightly larger cylinder
//!   overlaps there) and the first one (a slightly smaller cylinder never
//!   overlaps before it); a miss must never overlap.
//! - The two-level BVH queries are compared with the brute-force reference
//!   on random instanced scenes (rotations, non-uniform and mirroring
//!   scales): results must be bit-identical.
//! - Degenerate triangles, coplanar starts, extreme coordinates and hostile
//!   inputs.

use asamu_world::SurfaceTag;
use asamu_world::collision::cylinder::{overlaps, ray, sweep};
use asamu_world::collision::{
    Affine, CollisionClass, CollisionSceneBuilder, InstanceInfo, QueryFilter, contact_tolerance,
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
    let c = rng.vec(100.0);
    match rng.pick(6) {
        // Horizontal.
        0 => {
            let z = c.z;
            [0, 1, 2].map(|_| {
                f32ify(DVec3::new(
                    c.x + rng.range(-150.0, 150.0),
                    c.y + rng.range(-150.0, 150.0),
                    z,
                ))
            })
        }
        // Vertical.
        1 => {
            let dir =
                DVec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), 0.0).normalize_or_zero();
            [0, 1, 2].map(|_| {
                let s = rng.range(-150.0, 150.0);
                f32ify(c + dir * s + DVec3::Z * rng.range(-150.0, 150.0))
            })
        }
        // Axis-aligned edge (one horizontal edge).
        2 => {
            let a = c;
            let b = c + DVec3::new(rng.range(-150.0, 150.0), rng.range(-150.0, 150.0), 0.0);
            let cc = c + rng.vec(150.0);
            [f32ify(a), f32ify(b), f32ify(cc)]
        }
        // Sliver.
        3 => {
            let a = c;
            let b = c + rng.vec(150.0);
            let cc = a + (b - a) * rng.unit() + rng.vec(0.5);
            [f32ify(a), f32ify(b), f32ify(cc)]
        }
        // Vertical edge.
        4 => {
            let a = c;
            let b = c + DVec3::Z * rng.range(-150.0, 150.0);
            let cc = c + rng.vec(150.0);
            [f32ify(a), f32ify(b), f32ify(cc)]
        }
        _ => [0, 1, 2].map(|_| f32ify(c + rng.vec(150.0))),
    }
}

fn random_motion(rng: &mut Rng) -> (DVec3, DVec3) {
    let s = f32ify(rng.vec(300.0));
    let d = match rng.pick(5) {
        0 => DVec3::new(0.0, 0.0, rng.range(-500.0, 500.0)),
        1 => DVec3::new(rng.range(-500.0, 500.0), rng.range(-500.0, 500.0), 0.0),
        2 => DVec3::new(rng.range(-500.0, 500.0), 0.0, 0.0),
        _ => rng.vec(500.0),
    };
    (s, f32ify(d))
}

const R: f64 = 21.0;
const H: f64 = 44.0;

/// Checks one sweep against the sampling oracle; returns whether it hit.
fn check_against_oracle(s: DVec3, d: DVec3, tri: &[DVec3; 3], tol: f64) -> bool {
    let res = sweep(s, d, R, H, tri, None, tol);
    // Contacts shallower than the tolerance (grazes, boundary bands) are not
    // reported by design, so the oracle shrinks the shape by twice that.
    let small = 2.0 * tol;
    let samples = 400;
    match res {
        Some(c) => {
            assert!(!c.penetrating, "start was separated: {s} {d} {tri:?}");
            assert!((0.0..=1.0).contains(&c.t));
            let at = s + d * c.t;
            assert!(
                overlaps(at, R + 1.0e-3, H + 1.0e-3, tri),
                "reported contact is not real: t={} s={s} d={d} tri={tri:?} n={}",
                c.t,
                c.normal
            );
            let len = d.length().max(1.0);
            let t_end = c.t - 1.0e-6 * 1000.0 / len;
            for k in 0..=samples {
                let t = t_end * k as f64 / samples as f64;
                if t <= 0.0 {
                    continue;
                }
                assert!(
                    !overlaps(s + d * t, R - small, H - small, tri),
                    "earlier contact at t={t} missed (reported {}): s={s} d={d} tri={tri:?}",
                    c.t
                );
            }
            // Normal: unit, opposing the motion, and moving along it separates.
            assert!((c.normal.length() - 1.0).abs() < 1e-9);
            assert!(
                c.normal.dot(d) <= 1e-6 * d.length(),
                "normal {} vs motion {d}",
                c.normal
            );
            assert!(
                !overlaps(at + c.normal * 0.05, R - small, H - small, tri),
                "moving along the normal does not separate: n={} s={s} d={d} tri={tri:?}",
                c.normal
            );
            true
        }
        None => {
            for k in 0..=samples {
                let t = k as f64 / samples as f64;
                assert!(
                    !overlaps(s + d * t, R - small, H - small, tri),
                    "missed contact at t={t}: s={s} d={d} tri={tri:?}"
                );
            }
            false
        }
    }
}

#[test]
fn sweep_agrees_with_the_overlap_oracle() {
    let mut rng = Rng(0xA5A5_1234);
    let mut hits = 0;
    let mut cases = 0;
    while cases < 12_000 {
        let tri = random_triangle(&mut rng);
        let (s, d) = random_motion(&mut rng);
        // Separated starts only (a 0.05 margin keeps the "touching" rule out).
        if overlaps(s, R + 0.05, H + 0.05, &tri) {
            continue;
        }
        cases += 1;
        if check_against_oracle(s, d, &tri, 1.0e-3) {
            hits += 1;
        }
    }
    assert!(hits > 250, "too few hits to be meaningful: {hits}");
}

#[test]
fn sweeps_aimed_at_triangles_hit_features_of_every_kind() {
    // Aim the motion at a random point of the triangle (or near an edge or
    // vertex) from a random direction, so most cases hit.
    let mut rng = Rng(77);
    let mut hits = 0;
    for _ in 0..4000 {
        let tri = random_triangle(&mut rng);
        let (u, v) = (rng.unit(), rng.unit());
        let (u, v) = if u + v > 1.0 {
            (1.0 - u, 1.0 - v)
        } else {
            (u, v)
        };
        let target = match rng.pick(3) {
            0 => tri[0] + (tri[1] - tri[0]) * u + (tri[2] - tri[0]) * v,
            1 => tri[0] + (tri[1] - tri[0]) * u,
            _ => tri[rng.pick(3) as usize],
        };
        let dir = rng.vec(1.0).normalize_or_zero();
        if dir == DVec3::ZERO {
            continue;
        }
        let s = f32ify(target - dir * 300.0 + rng.vec(30.0));
        let d = f32ify(dir * 600.0);
        if overlaps(s, R + 0.05, H + 0.05, &tri) {
            continue;
        }
        if check_against_oracle(s, d, &tri, 1.0e-3) {
            hits += 1;
        }
    }
    assert!(hits > 2500, "{hits}");
}

#[test]
fn degenerate_triangles_and_coplanar_starts() {
    let tol = 1.0e-3;
    // Zero-area triangles: a point and a segment still collide as such.
    let point = [DVec3::new(50.0, 0.0, 0.0); 3];
    let c = sweep(
        DVec3::ZERO,
        DVec3::new(100.0, 0.0, 0.0),
        R,
        H,
        &point,
        None,
        tol,
    )
    .unwrap();
    assert!((c.t - 29.0 / 100.0).abs() < 1e-12);
    assert!((c.normal - DVec3::NEG_X).length() < 1e-12);
    let segment = [
        DVec3::new(50.0, -100.0, 0.0),
        DVec3::new(50.0, 100.0, 0.0),
        DVec3::new(50.0, 0.0, 0.0),
    ];
    let c = sweep(
        DVec3::ZERO,
        DVec3::new(100.0, 0.0, 0.0),
        R,
        H,
        &segment,
        None,
        tol,
    )
    .unwrap();
    assert!((c.t - 29.0 / 100.0).abs() < 1e-12);
    // NaN / infinite inputs never hit and never panic.
    let floor = [
        DVec3::new(-500.0, -500.0, 0.0),
        DVec3::new(500.0, -500.0, 0.0),
        DVec3::new(0.0, 500.0, 0.0),
    ];
    assert!(sweep(DVec3::splat(f64::NAN), DVec3::Z, R, H, &floor, None, tol).is_none());
    assert!(
        sweep(
            DVec3::ZERO,
            DVec3::splat(f64::INFINITY),
            R,
            H,
            &floor,
            None,
            tol
        )
        .is_none()
    );
    let nan_tri = [DVec3::splat(f64::NAN), DVec3::ZERO, DVec3::X];
    let _ = sweep(
        DVec3::new(0.0, 0.0, 100.0),
        DVec3::NEG_Z * 200.0,
        R,
        H,
        &nan_tri,
        None,
        tol,
    );
    assert!(sweep(DVec3::ZERO, DVec3::Z, 0.0, H, &floor, None, tol).is_none());
    // Standing exactly on the plane (cap coplanar with the triangle).
    let s = DVec3::new(0.0, 0.0, H);
    assert!(sweep(s, DVec3::new(300.0, 0.0, 0.0), R, H, &floor, None, tol).is_none());
    assert_eq!(
        sweep(s, DVec3::new(300.0, 0.0, -1.0), R, H, &floor, None, tol)
            .unwrap()
            .t,
        0.0
    );
    // Side exactly touching a vertical wall: sliding along it is free.
    let wall = [
        DVec3::new(R, -500.0, -500.0),
        DVec3::new(R, 500.0, -500.0),
        DVec3::new(R, 0.0, 500.0),
    ];
    assert!(
        sweep(
            DVec3::ZERO,
            DVec3::new(0.0, 300.0, 0.0),
            R,
            H,
            &wall,
            None,
            tol
        )
        .is_none()
    );
    assert_eq!(
        sweep(
            DVec3::ZERO,
            DVec3::new(1.0, 300.0, 0.0),
            R,
            H,
            &wall,
            None,
            tol
        )
        .unwrap()
        .t,
        0.0
    );
    // Rays.
    assert!(ray(DVec3::splat(f64::NAN), DVec3::Z, &floor, None).is_none());
}

#[test]
fn far_from_the_origin() {
    // IceCave coordinates reach ±250 000 uu; f32 vertices there are 1/64 uu apart.
    let base = DVec3::new(-248_385.72, -195_900.81, -33_106.867);
    let floor = [
        f32ify(base + DVec3::new(-500.0, -500.0, 0.0)),
        f32ify(base + DVec3::new(500.0, -500.0, 0.0)),
        f32ify(base + DVec3::new(0.0, 500.0, 0.0)),
    ];
    let s = f32ify(base + DVec3::new(0.0, 0.0, 200.0));
    let tol = contact_tolerance(s.as_vec3());
    let c = sweep(s, DVec3::new(0.0, 0.0, -400.0), R, H, &floor, None, tol).unwrap();
    let rest = s + DVec3::new(0.0, 0.0, -400.0) * c.t;
    assert!((rest.z - (floor[0].z + H)).abs() < 1e-6, "{rest}");
    let mut rng = Rng(5);
    for _ in 0..500 {
        let tri = random_triangle(&mut rng).map(|v| f32ify(v + base));
        let (s0, d) = random_motion(&mut rng);
        let s = f32ify(s0 + base);
        if overlaps(s, R + 0.1, H + 0.1, &tri) {
            continue;
        }
        check_against_oracle(s, d, &tri, contact_tolerance(s.as_vec3()));
    }
}

fn info(rng: &mut Rng) -> InstanceInfo {
    InstanceInfo {
        actor: Some(rng.pick(1000) as u32),
        class: CollisionClass::StaticMesh,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: rng.pick(8) != 0,
        blocks_traces: rng.pick(8) != 0,
        sublevel: rng.pick(2) as u8,
    }
}

fn random_affine(rng: &mut Rng) -> Affine {
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
        translation: rng.vec(3000.0),
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

#[test]
fn bvh_queries_equal_brute_force() {
    let mut rng = Rng(0xBEEF);
    let mut b = CollisionSceneBuilder::new();
    let meshes: Vec<u32> = (0..12)
        .filter_map(|_| {
            let (v, t) = random_mesh(&mut rng);
            b.add_mesh(v, t)
        })
        .collect();
    for _ in 0..400 {
        let m = meshes[rng.pick(meshes.len() as u64) as usize];
        let a = random_affine(&mut rng);
        let i = info(&mut rng);
        b.add_static(m, a, i);
    }
    // A convex hull too.
    let cube: Vec<Vec3> = (0..8)
        .map(|i| Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32) * 300.0)
        .collect();
    let hull = b
        .add_convex_mesh(
            cube,
            vec![
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
            ],
        )
        .unwrap();
    b.add_static(
        hull,
        Affine::from_translation(DVec3::new(100.0, 100.0, 100.0)),
        info(&mut rng),
    );
    let scene = b.build();
    let dynamic: Vec<_> = (0..10)
        .filter_map(|_| {
            let m = meshes[rng.pick(meshes.len() as u64) as usize];
            let a = random_affine(&mut rng);
            let i = info(&mut rng);
            scene.dynamic_instance(m, a, i)
        })
        .collect();
    let filters = [
        QueryFilter::PAWN,
        QueryFilter::TRACE,
        QueryFilter {
            kind: asamu_world::collision::QueryKind::Pawn,
            sublevels: 1,
        },
    ];
    let mut hits = 0;
    for k in 0..3000 {
        let s = rng.vec(3500.0).as_vec3();
        let e = (s.as_dvec3() + rng.vec(1500.0)).as_vec3();
        let f = filters[k % 3];
        let fast = scene.sweep_cylinder(&dynamic, s, e, 21.0, 44.0, f);
        let slow = scene.sweep_cylinder_brute_force(&dynamic, s, e, 21.0, 44.0, f);
        assert_eq!(fast, slow, "sweep {s} -> {e}");
        if fast.is_some() {
            hits += 1;
        }
        let rf = scene.raycast(&dynamic, s, e, f);
        let rs = scene.raycast_brute_force(&dynamic, s, e, f);
        assert_eq!(rf, rs, "ray {s} -> {e}");
    }
    assert!(hits > 100, "{hits}");
    // Same queries on a rebuilt scene: identical (deterministic build).
    let s = Vec3::new(10.0, 20.0, 3000.0);
    let e = Vec3::new(10.0, 20.0, -3000.0);
    let again = scene.clone();
    assert_eq!(
        scene.sweep_cylinder(&dynamic, s, e, 21.0, 44.0, QueryFilter::PAWN),
        again.sweep_cylinder(&dynamic, s, e, 21.0, 44.0, QueryFilter::PAWN)
    );
}

#[test]
fn performance_smoke_test() {
    // A 256 × 256-cell terrain (131 072 triangles) as 64 instances of one
    // 32 × 32-cell patch, plus a few thousand small boxes; 2 000 short
    // sweeps and 200 long rays. Timing is only asserted in release builds.
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
    let base = InstanceInfo {
        actor: None,
        class: CollisionClass::StaticMesh,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: true,
        blocks_traces: true,
        sublevel: 0,
    };
    for py in 0..8 {
        for px in 0..8 {
            let t = DVec3::new(f64::from(px) * 3200.0, f64::from(py) * 3200.0, 0.0);
            b.add_static(patch, Affine::from_translation(t), base);
        }
    }
    let (cv, ct) = random_mesh(&mut rng);
    let small = b.add_mesh(cv, ct).unwrap();
    for _ in 0..3000 {
        let mut a = random_affine(&mut rng);
        a.translation = DVec3::new(
            rng.range(0.0, 25600.0),
            rng.range(0.0, 25600.0),
            rng.range(0.0, 300.0),
        );
        b.add_static(small, a, base);
    }
    let scene = b.build();
    assert!(scene.stats().instance_triangles > 131_072);
    let start = std::time::Instant::now();
    let mut hits = 0;
    for _ in 0..2000 {
        let p = Vec3::new(
            rng.range(500.0, 25000.0) as f32,
            rng.range(500.0, 25000.0) as f32,
            200.0,
        );
        let q = p + rng.vec(20.0).as_vec3();
        if scene
            .sweep_cylinder(&[], p, q - Vec3::Z * 300.0, 21.0, 44.0, QueryFilter::PAWN)
            .is_some()
        {
            hits += 1;
        }
    }
    for _ in 0..200 {
        let p = Vec3::new(
            rng.range(0.0, 25600.0) as f32,
            rng.range(0.0, 25600.0) as f32,
            150.0,
        );
        let dir = rng.vec(1.0).as_vec3().normalize_or_zero();
        let _ = scene.raycast(&[], p, p + dir * 16_384.0, QueryFilter::TRACE);
    }
    let elapsed = start.elapsed();
    let per_query_us = elapsed.as_secs_f64() * 1e6 / 2200.0;
    eprintln!(
        "{hits} hits; {per_query_us:.1} us per query ({:?} total)",
        elapsed
    );
    assert!(hits > 1000);
    if !cfg!(debug_assertions) {
        assert!(per_query_us < 100.0, "{per_query_us} us per query");
    }
}

/// Earliest `t ∈ [0, 1]` at which the point `p` is inside the closed
/// cylinder `(r, h)` centred at `s + t·d` (exact interval arithmetic; an
/// oracle that shares no code with the sweep or the overlap test).
fn point_entry(p: DVec3, s: DVec3, d: DVec3, r: f64, h: f64) -> Option<f64> {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let z0 = p.z - s.z;
    if d.z == 0.0 {
        if z0.abs() > h {
            return None;
        }
    } else {
        let (a, b) = ((z0 - h) / d.z, (z0 + h) / d.z);
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    let (qx, qy) = (p.x - s.x, p.y - s.y);
    let a = d.x * d.x + d.y * d.y;
    let b = qx * d.x + qy * d.y;
    let c = qx * qx + qy * qy - r * r;
    if a == 0.0 {
        if c > 0.0 {
            return None;
        }
    } else {
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        lo = lo.max((b - disc.sqrt()) / a);
        hi = hi.min((b + disc.sqrt()) / a);
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

fn earliest(samples: &[DVec3], s: DVec3, d: DVec3, r: f64, h: f64) -> Option<f64> {
    samples
        .iter()
        .filter_map(|p| point_entry(*p, s, d, r, h))
        .reduce(f64::min)
}

#[test]
fn sweep_agrees_with_a_point_sampling_oracle() {
    // Independent of `overlaps`: the time of impact against dense samples of
    // the triangle. A reported contact must have a sample entering the
    // cylinder grown by the sample spacing no later than it (no phantom
    // contacts); no sample may enter the cylinder shrunk by twice the
    // tolerance before it (no missed earlier contacts, deeper than the
    // tolerance); a miss must not let any sample in that shrunk cylinder.
    let mut rng = Rng(0x0BAD_5EED);
    let tol = 1.0e-3;
    let (mut cases, mut hits) = (0, 0);
    while cases < 1500 {
        let tri = random_triangle(&mut rng).map(|p| f32ify(p * 0.4));
        let (u, v) = (rng.unit(), rng.unit());
        let (u, v) = if u + v > 1.0 {
            (1.0 - u, 1.0 - v)
        } else {
            (u, v)
        };
        let target = tri[0] + (tri[1] - tri[0]) * u + (tri[2] - tri[0]) * v;
        let dir = match rng.pick(3) {
            0 => DVec3::Z * if rng.pick(2) == 0 { 1.0 } else { -1.0 },
            1 => DVec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), 0.0).normalize_or_zero(),
            _ => rng.vec(1.0).normalize_or_zero(),
        };
        let s = f32ify(target - dir * rng.range(30.0, 120.0) + rng.vec(15.0));
        let d = f32ify((target - s) * rng.range(0.3, 1.5));
        let (samples, spacing) = grid(&tri, 0.25);
        let margin = spacing + 0.05;
        if earliest(&samples, s, DVec3::ZERO, R + margin, H + margin).is_some()
            || overlaps(s, R + 0.05, H + 0.05, &tri)
        {
            continue;
        }
        cases += 1;
        let shrunk = earliest(&samples, s, d, R - 2.0 * tol, H - 2.0 * tol);
        match sweep(s, d, R, H, &tri, None, tol) {
            Some(c) => {
                hits += 1;
                let grown = earliest(&samples, s, d, R + spacing + 1e-6, H + spacing + 1e-6);
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
            }
            None => assert!(
                shrunk.is_none(),
                "missed contact at {shrunk:?}: s={s} d={d} tri={tri:?}"
            ),
        }
    }
    assert!(hits > 500, "too few hits: {hits}");
}
