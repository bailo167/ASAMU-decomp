//! Independent verification of the swept box (synthetic data only, fixed
//! seeds).
//!
//! The reference in this file shares neither code nor method with the
//! kernel in `collision::box_sweep` (separating axes over the motion). It
//! is built from the distance between a box and a triangle:
//!
//! - two disjoint convex bodies are closest at a vertex of one and a face
//!   of the other, or at an edge of each, so the distance is the smallest
//!   of: the triangle's corners to the solid box, the box's corners to the
//!   triangle, and the box's twelve edges to the triangle's three; it is 0
//!   when an edge of the triangle enters the box or an edge of the box
//!   pierces the triangle;
//! - along a straight motion that distance is a convex function of time
//!   (the distance to a convex set after a translation), so its first zero
//!   is found by narrowing on the minimum (golden section, stopped early
//!   by the bound "the distance cannot change faster than the box moves")
//!   and then bisecting on the way down.
//!
//! What is asserted of every reported contact: at its time the box is
//! within [`TOUCH_SLACK`] of the triangle (it is a contact), [`EARLY`] of
//! travel before it the box did not overlap (it is the first), and the
//! normal is one of the thirteen axes, points against the motion and is the
//! direction of a plane that separates the two bodies at the contact. A
//! miss is checked the other way round. Only sweeps that graze within
//! [`GRAZE`] may differ in hit or miss; they are counted.

#![allow(clippy::unwrap_used)]

use asamu_world::SurfaceTag;
use asamu_world::collision::box_sweep::{overlaps_hull, sweep_hull, sweep_triangle};
use asamu_world::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, ConvexHull, InstanceInfo,
    InstanceOptions, PrimitiveKind, QueryFilter,
};
use glam::{DVec3, Vec3};

/// The pawn's box.
const E: DVec3 = DVec3::new(21.0, 21.0, 44.0);
const HALF: Vec3 = Vec3::new(21.0, 21.0, 44.0);
/// Start-contact tolerance handed to the kernel.
const TOL: f64 = 1.0e-3;
/// The reference calls two bodies touching at this distance (uu).
const TOUCH: f64 = 1.0e-9;
/// A reported contact must be within this distance of the obstacle (uu).
const TOUCH_SLACK: f64 = 1.0e-7;
/// This much travel before a reported contact the bodies must be apart (uu).
const EARLY: f64 = 1.0e-6;
/// Hit and miss may differ only for sweeps that pass within this (uu).
const GRAZE: f64 = 1.0e-7;
/// Reported and reference times of contacts that are not grazing agree
/// within this much travel (uu).
const TIME_SLACK: f64 = 1.0e-6;

const PAWN: QueryFilter = QueryFilter::PAWN;

// ---------------------------------------------------------------------
// Test data generator (SplitMix64).

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
    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn int(&mut self, lo: i64, hi: i64) -> f64 {
        (lo + (self.next() % (hi - lo + 1) as u64) as i64) as f64
    }
    fn sign(&mut self) -> f64 {
        if self.pick(2) == 0 { 1.0 } else { -1.0 }
    }
    fn vec(&mut self, e: f64) -> DVec3 {
        DVec3::new(self.range(-e, e), self.range(-e, e), self.range(-e, e))
    }
    fn ints(&mut self, e: i64) -> DVec3 {
        DVec3::new(self.int(-e, e), self.int(-e, e), self.int(-e, e))
    }
    fn direction(&mut self) -> DVec3 {
        loop {
            let v = self.vec(1.0);
            let l = v.length();
            if l > 0.1 && l <= 1.0 {
                return v / l;
            }
        }
    }
}

/// The nearest vector whose components are `f32` values (the world stores
/// `f32` positions).
fn f32v(v: DVec3) -> DVec3 {
    v.as_vec3().as_dvec3()
}

// ---------------------------------------------------------------------
// The reference: distance between a box and a triangle.

fn point_segment_d2(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let u = if l2 > 0.0 {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + ab * u - p).length_squared()
}

fn point_triangle_d2(p: DVec3, t: &[DVec3; 3]) -> f64 {
    let mut best = point_segment_d2(p, t[0], t[1])
        .min(point_segment_d2(p, t[1], t[2]))
        .min(point_segment_d2(p, t[2], t[0]));
    let n = (t[1] - t[0]).cross(t[2] - t[0]);
    let n2 = n.length_squared();
    if n2 > 0.0 {
        let h = (p - t[0]).dot(n);
        let q = p - n * (h / n2);
        if (0..3).all(|k| (t[(k + 1) % 3] - t[k]).cross(q - t[k]).dot(n) >= 0.0) {
            best = best.min(h * h / n2);
        }
    }
    best
}

fn segment_segment_d2(p1: DVec3, q1: DVec3, p2: DVec3, q2: DVec3) -> f64 {
    let (d1, d2) = (q1 - p1, q2 - p2);
    let (a, e) = (d1.length_squared(), d2.length_squared());
    // Parameter of the point of segment 2 nearest to `x`, and likewise.
    let on2 = |x: DVec3| {
        if e > 0.0 {
            ((x - p2).dot(d2) / e).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let on1 = |x: DVec3| {
        if a > 0.0 {
            ((x - p1).dot(d1) / a).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    // The closest pair of the two lines, when they are not parallel.
    let r = p1 - p2;
    let (b, c, f) = (d1.dot(d2), d1.dot(r), d2.dot(r));
    let denom = a * e - b * b;
    let mut s = if denom > 0.0 {
        ((b * f - c * e) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut best = f64::INFINITY;
    // Alternate between the segments: each step can only come closer, and
    // from the lines' closest pair it ends at the segments' closest pair.
    for _ in 0..4 {
        let t = on2(p1 + d1 * s);
        s = on1(p2 + d2 * t);
        best = best.min(((p1 + d1 * s) - (p2 + d2 * t)).length_squared());
    }
    // Starting from each end as well covers nearly parallel segments.
    for s0 in [0.0, 1.0] {
        let t = on2(p1 + d1 * s0);
        let s1 = on1(p2 + d2 * t);
        let t1 = on2(p1 + d1 * s1);
        best = best.min(((p1 + d1 * s1) - (p2 + d2 * t1)).length_squared());
    }
    best
}

fn point_box_d2(p: DVec3, c: DVec3, e: DVec3) -> f64 {
    ((p - c).abs() - e).max(DVec3::ZERO).length_squared()
}

/// The segment `a → b` meets the closed box.
fn segment_meets_box(a: DVec3, b: DVec3, c: DVec3, e: DVec3) -> bool {
    let d = b - a;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let o = a[k] - c[k];
        if d[k] == 0.0 {
            if o.abs() > e[k] {
                return false;
            }
        } else {
            let (u, v) = ((-e[k] - o) / d[k], (e[k] - o) / d[k]);
            lo = lo.max(u.min(v));
            hi = hi.min(u.max(v));
            if lo > hi {
                return false;
            }
        }
    }
    true
}

/// The segment `a → b` passes through the triangle (not within its plane).
fn segment_pierces_triangle(a: DVec3, b: DVec3, t: &[DVec3; 3]) -> bool {
    let d = b - a;
    let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
    let pv = d.cross(e2);
    let det = e1.dot(pv);
    if det == 0.0 {
        return false;
    }
    let tv = a - t[0];
    let u = tv.dot(pv) / det;
    if !(0.0..=1.0).contains(&u) {
        return false;
    }
    let qv = tv.cross(e1);
    let v = d.dot(qv) / det;
    if v < 0.0 || u + v > 1.0 {
        return false;
    }
    (0.0..=1.0).contains(&(e2.dot(qv) / det))
}

/// Distance between the solid box `(c, e)` and the triangle; 0 when they
/// meet.
fn box_triangle_distance(c: DVec3, e: DVec3, t: &[DVec3; 3]) -> f64 {
    for k in 0..3 {
        if segment_meets_box(t[k], t[(k + 1) % 3], c, e) {
            return 0.0;
        }
    }
    let corner = |i: usize| {
        c + DVec3::new(
            if i & 1 == 0 { -e.x } else { e.x },
            if i & 2 == 0 { -e.y } else { e.y },
            if i & 4 == 0 { -e.z } else { e.z },
        )
    };
    let mut d2 = f64::INFINITY;
    for v in t {
        d2 = d2.min(point_box_d2(*v, c, e));
    }
    for i in 0..8 {
        d2 = d2.min(point_triangle_d2(corner(i), t));
        for axis in 0..3 {
            let j = i | (1 << axis);
            if j == i {
                continue;
            }
            let (a, b) = (corner(i), corner(j));
            if segment_pierces_triangle(a, b, t) {
                return 0.0;
            }
            for k in 0..3 {
                d2 = d2.min(segment_segment_d2(a, b, t[k], t[(k + 1) % 3]));
            }
        }
    }
    d2.sqrt()
}

/// The triangle relative to its first corner, and the start relative to it
/// (differences of `f32` positions are exact in `f64`, so coordinates near
/// 2e5 cost the reference nothing).
fn relative(s: DVec3, t: &[DVec3; 3]) -> (DVec3, [DVec3; 3]) {
    (s - t[0], [DVec3::ZERO, t[1] - t[0], t[2] - t[0]])
}

/// Distance between the box at `s + d·x` and the triangle.
fn distance_at(s: DVec3, d: DVec3, e: DVec3, t: &[DVec3; 3], x: f64) -> f64 {
    let (s, t) = relative(s, t);
    box_triangle_distance(s + d * x, e, &t)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Reference {
    /// First time in `[0, 1]` at which the bodies are within [`TOUCH`].
    Hit(f64),
    Miss,
}

/// First contact of the box moving from `s` by `d` with the triangle.
fn reference(s: DVec3, d: DVec3, e: DVec3, t: &[DVec3; 3]) -> Reference {
    let (s, t) = relative(s, t);
    let f = |x: f64| box_triangle_distance(s + d * x, e, &t);
    let len = d.length();
    if f(0.0) <= TOUCH {
        return Reference::Hit(0.0);
    }
    let mut inside = None;
    if f(1.0) <= TOUCH {
        inside = Some(1.0);
    } else {
        let phi = 0.618_033_988_749_894_9_f64;
        let (mut a, mut b) = (0.0f64, 1.0f64);
        let mut x1 = b - phi * (b - a);
        let mut x2 = a + phi * (b - a);
        let (mut y1, mut y2) = (f(x1), f(x2));
        for _ in 0..400 {
            if y1 <= TOUCH {
                inside = Some(x1);
                break;
            }
            if y2 <= TOUCH {
                inside = Some(x2);
                break;
            }
            // Nowhere in [a, b] can the distance be below this.
            if y1.max(y2) - len * (b - a) > TOUCH {
                return Reference::Miss;
            }
            if y1 < y2 {
                b = x2;
                x2 = x1;
                y2 = y1;
                x1 = b - phi * (b - a);
                y1 = f(x1);
            } else {
                a = x1;
                x1 = x2;
                y1 = y2;
                x2 = a + phi * (b - a);
                y2 = f(x2);
            }
        }
    }
    let Some(mut hi) = inside else {
        return Reference::Miss;
    };
    let mut lo = 0.0f64;
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi {
            break;
        }
        if f(mid) <= TOUCH {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Reference::Hit(hi)
}

/// The thirteen axes of the documented rule, in its order: the box's face
/// axes Z, X, Y, the triangle's normal, then X, Y and Z each crossed with
/// the three edges. Written from the rule, not from the kernel.
fn candidate_axes(t: &[DVec3; 3]) -> Vec<DVec3> {
    let mut out = vec![DVec3::Z, DVec3::X, DVec3::Y];
    let n = (t[1] - t[0]).cross(t[2] - t[0]);
    if n.length() > 0.0 {
        out.push(n.normalize());
    }
    for b in [DVec3::X, DVec3::Y, DVec3::Z] {
        for k in 0..3 {
            let c = b.cross((t[(k + 1) % 3] - t[k]).normalize_or_zero());
            if c.length() > 0.0 {
                out.push(c.normalize());
            }
        }
    }
    out
}

/// Gap between the box centred at `c` and the triangle along the unit
/// direction `n`, the box on the side `n` points to (negative: they
/// overlap on that axis by so much).
fn gap(n: DVec3, c: DVec3, e: DVec3, t: &[DVec3; 3]) -> f64 {
    let far = t
        .iter()
        .map(|v| n.dot(*v))
        .fold(f64::NEG_INFINITY, f64::max);
    n.dot(c) - e.dot(n.abs()) - far
}

fn is_world_axis(n: DVec3) -> bool {
    n.abs().max_element() == 1.0
}

#[derive(Default, Debug)]
struct Tally {
    cases: usize,
    hits: usize,
    misses: usize,
    grazes: usize,
    level: usize,
    timed: usize,
    worst_time: f64,
    worst_touch: f64,
    ties: usize,
    ties_first: usize,
    world_ties: usize,
}

/// One sweep that starts clear: the kernel against the reference.
fn check(case: &Case, exact: bool, tally: &mut Tally, what: &str) {
    let (s, d, e, tri) = (case.0, case.1, case.2, &case.3);
    let got = sweep_triangle(s, d, e, tri, None, TOL, 1.0);
    let want = reference(s, d, e, tri);
    let len = d.length();
    let ctx = || format!("{what}: s={s:?} d={d:?} e={e:?} tri={tri:?}");
    tally.cases += 1;
    match (want, got) {
        (Reference::Hit(tr), Some(c)) => {
            tally.hits += 1;
            assert!(!c.penetrating && c.t > 0.0 && c.t <= 1.0, "{c:?} {}", ctx());
            // A contact, and the first.
            let touch = distance_at(s, d, e, tri, c.t);
            assert!(
                touch <= TOUCH_SLACK,
                "no contact at t={}: {touch} uu away (reference {tr}); {}",
                c.t,
                ctx()
            );
            tally.worst_touch = tally.worst_touch.max(touch);
            let before = c.t - EARLY / len;
            if before > 0.0 {
                assert!(
                    distance_at(s, d, e, tri, before) > 0.0,
                    "late: overlapping {EARLY} uu before t={} (reference {tr}); {}",
                    c.t,
                    ctx()
                );
            }
            // Times, where the approach is not a graze: 1e-3 uu of travel
            // before the contact the bodies are at least 1e-5 apart.
            let back = c.t - 1.0e-3 / len;
            if back > 0.0 && distance_at(s, d, e, tri, back) >= 1.0e-5 {
                let dt = (c.t - tr).abs() * len;
                assert!(
                    dt <= TIME_SLACK,
                    "time {} against reference {tr}: {dt} uu of travel; {}",
                    c.t,
                    ctx()
                );
                tally.timed += 1;
                tally.worst_time = tally.worst_time.max(dt);
            }
            // The normal.
            let n = c.normal;
            assert!((n.length() - 1.0).abs() < 1.0e-12, "{c:?} {}", ctx());
            assert!(n.dot(d) < 0.0, "normal along the motion: {c:?} {}", ctx());
            let (rs, rt) = relative(s, tri);
            let at = rs + d * c.t;
            let g = gap(n, at, e, &rt);
            assert!(
                g.abs() <= TOUCH_SLACK,
                "the normal's plane does not separate at the contact (gap {g}): {c:?} {}",
                ctx()
            );
            let axes = candidate_axes(&rt);
            assert!(
                axes.iter().any(|a| a.cross(n).length() < 1.0e-9),
                "the normal is none of the thirteen axes: {c:?} {}",
                ctx()
            );
            // Axes that enter at the contact time, in the rule's order and
            // one per direction.
            let mut active: Vec<DVec3> = Vec::new();
            for a in &axes {
                let v = a.dot(d);
                if v == 0.0 {
                    continue;
                }
                let a = if v > 0.0 { -*a } else { *a };
                if gap(a, at, e, &rt).abs() <= 1.0e-9
                    && active.iter().all(|o| o.cross(a).length() >= 1.0e-9)
                {
                    active.push(a);
                }
            }
            if active.len() > 1 {
                tally.ties += 1;
                let first = active[0].cross(n).length() < 1.0e-9;
                tally.ties_first += usize::from(first);
                // With whole numbers the entry times of the box's own face
                // axes are exact, so a tie between them is a real one and
                // the first in the order Z, X, Y must be the one reported.
                // (Elsewhere two entries within 1e-9 uu are not a tie.)
                if exact && active.iter().all(|a| is_world_axis(*a)) {
                    tally.world_ties += 1;
                    assert!(
                        first,
                        "tie between {active:?} but the normal is {n:?}; {}",
                        ctx()
                    );
                }
            }
        }
        (Reference::Hit(tr), None) => {
            // Only a graze may be missed.
            let shrunk = reference(s, d, e - DVec3::splat(GRAZE), tri);
            assert!(
                shrunk == Reference::Miss,
                "missed the contact at {tr} (a box {GRAZE} smaller still hits: {shrunk:?}); {}",
                ctx()
            );
            // The documented kind of graze: on some axis the start is
            // level with the obstacle (within the tolerance) and the
            // motion does not close that axis, so the box slides by.
            let (rs, rt) = relative(s, tri);
            let level = candidate_axes(&rt)
                .iter()
                .flat_map(|a| [*a, -*a])
                .any(|a| a.dot(d) >= 0.0 && (-TOL..=GRAZE).contains(&gap(a, rs, e, &rt)));
            if level {
                tally.level += 1;
            } else {
                tally.grazes += 1;
            }
        }
        (Reference::Miss, Some(c)) => {
            let grown = reference(s, d, e + DVec3::splat(GRAZE), tri);
            assert!(
                grown != Reference::Miss,
                "contact {c:?} where a box {GRAZE} larger still misses; {}",
                ctx()
            );
            tally.grazes += 1;
        }
        (Reference::Miss, None) => tally.misses += 1,
    }
}

fn clear(s: DVec3, d: DVec3, e: DVec3, tri: &[DVec3; 3]) -> bool {
    d.length() > 0.0 && distance_at(s, d, e, tri, 0.0) > 0.05
}

fn point_in(rng: &mut Rng, t: &[DVec3; 3]) -> DVec3 {
    let (u, v) = (rng.unit(), rng.unit());
    let (u, v) = if u + v > 1.0 {
        (1.0 - u, 1.0 - v)
    } else {
        (u, v)
    };
    t[0] + (t[1] - t[0]) * u + (t[2] - t[0]) * v
}

/// A triangle of one of several shapes, within some 100 uu of the origin.
fn triangle(rng: &mut Rng) -> [DVec3; 3] {
    let c = rng.vec(40.0);
    let t = match rng.pick(8) {
        // Horizontal.
        0 => [0, 1, 2].map(|_| {
            DVec3::new(
                c.x + rng.range(-70.0, 70.0),
                c.y + rng.range(-70.0, 70.0),
                c.z,
            )
        }),
        // A wall in a plane x = const or y = const.
        1 => {
            let swap = rng.pick(2) == 0;
            [0, 1, 2].map(|_| {
                let (u, w) = (rng.range(-70.0, 70.0), rng.range(-70.0, 70.0));
                if swap {
                    DVec3::new(c.x, c.y + u, c.z + w)
                } else {
                    DVec3::new(c.x + u, c.y, c.z + w)
                }
            })
        }
        // One edge along a world axis.
        2 => {
            let mut along = DVec3::ZERO;
            along[rng.pick(3) as usize] = rng.range(-80.0, 80.0);
            [c, c + along, c + rng.vec(60.0)]
        }
        // Small: the box meets it with a face, an edge or a corner.
        3 => [0, 1, 2].map(|_| c + rng.vec(3.0)),
        // Large.
        4 => [0, 1, 2].map(|_| c + rng.vec(1500.0)),
        _ => [0, 1, 2].map(|_| c + rng.vec(70.0)),
    };
    t.map(f32v)
}

/// A sweep aimed at the triangle from outside, off-centre by up to the
/// box's size so that faces, edges and corners all lead.
fn aimed(rng: &mut Rng, tri: &[DVec3; 3]) -> (DVec3, DVec3) {
    let target = point_in(rng, tri);
    let dir = match rng.pick(5) {
        0 => DVec3::Z * rng.sign(),
        1 => DVec3::X * rng.sign(),
        2 => {
            let a = rng.range(0.0, std::f64::consts::TAU);
            DVec3::new(a.cos(), a.sin(), 0.0)
        }
        _ => rng.direction(),
    };
    let s = f32v(target - dir * rng.range(70.0, 260.0) + rng.vec(35.0));
    let end = f32v(s + (target - s) * rng.range(0.3, 1.6));
    (s, end - s)
}

/// A sweep in which a point of the box's surface (a corner, a point of an
/// edge or of a face) passes a corner or an edge point of the triangle at
/// a distance between 1e-9 and 1 uu: the boundary between hit and miss.
fn grazing(rng: &mut Rng, tri: &[DVec3; 3]) -> (DVec3, DVec3) {
    let k = rng.pick(3) as usize;
    let q = if rng.pick(2) == 0 {
        tri[k]
    } else {
        tri[k] + (tri[(k + 1) % 3] - tri[k]) * rng.unit()
    };
    let mut on_box = DVec3::new(E.x * rng.sign(), E.y * rng.sign(), E.z * rng.sign());
    for axis in 0..3 {
        // Corner (most often), edge or face.
        if rng.pick(4) == 0 {
            on_box[axis] = rng.range(-E[axis], E[axis]);
        }
    }
    let dir = rng.direction();
    let side = dir.cross(rng.direction()).normalize_or_zero();
    let off = side * 10f64.powf(rng.range(-9.0, 0.0)) * rng.sign();
    let len = rng.range(40.0, 400.0);
    let s = q + off - on_box - dir * (len * rng.range(0.2, 0.8));
    (s, dir * len)
}

fn run(
    name: &str,
    seed: u64,
    wanted: usize,
    exact: bool,
    mut make: impl FnMut(&mut Rng) -> Case,
) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    let mut tried = 0usize;
    while tally.cases < wanted {
        tried += 1;
        assert!(tried < wanted * 200, "{name}: generator starved");
        let case = make(&mut rng);
        if clear(case.0, case.1, case.2, &case.3) {
            check(&case, exact, &mut tally, name);
        }
    }
    eprintln!("{name}: {tally:?}");
    tally
}

type Case = (DVec3, DVec3, DVec3, [DVec3; 3]);

/// Thousands of random sweeps that start clear, in seven families: the
/// kernel and the reference must agree on hit or miss (grazes within
/// 1e-7 uu aside) and on the time (within 1e-6 uu of travel).
#[test]
fn the_box_kernel_agrees_with_an_independent_reference() {
    let mut total = Tally::default();
    let mut add = |t: Tally, min_hits: usize, grazes_per_thousand: usize| {
        assert!(t.hits >= min_hits, "too few hits: {t:?}");
        assert!(
            t.grazes * 1000 <= t.cases * grazes_per_thousand,
            "too many grazes: {t:?}"
        );
        total.cases += t.cases;
        total.hits += t.hits;
        total.misses += t.misses;
        total.grazes += t.grazes;
        total.level += t.level;
        total.timed += t.timed;
        total.worst_time = total.worst_time.max(t.worst_time);
        total.worst_touch = total.worst_touch.max(t.worst_touch);
        total.ties += t.ties;
        total.ties_first += t.ties_first;
        total.world_ties += t.world_ties;
    };

    // 1. Triangles of every shape near the origin.
    add(
        run("generic", 0xA11C_E001, 4000, false, |rng| {
            let tri = triangle(rng);
            let (s, d) = aimed(rng, &tri);
            (s, d, E, tri)
        }),
        1500,
        1,
    );

    // 2. Corners and edges passing corners and edges at 1e-9 to 1 uu.
    add(
        run("grazing", 0xA11C_E002, 3000, false, |rng| {
            let tri = triangle(rng);
            let (s, d) = grazing(rng, &tri);
            (s, d, E, tri)
        }),
        800,
        10,
    );

    // 3. Whole numbers: a point of the box's surface (a corner, the middle
    //    of an edge or of a face) reaches a corner of the triangle exactly
    //    half way, so axes tie exactly. Many of these sweeps only touch:
    //    where the box slides level with the corner the kernel reports
    //    nothing by its documented rule (counted as `level`), and where the
    //    touch is a single point in passing, entry and exit of an edge
    //    axis are equal in exact arithmetic and rounding decides (counted
    //    as grazes, up to 5 % here).
    add(
        run("integers", 0xA11C_E003, 2500, true, |rng| {
            let c = rng.ints(40);
            let tri = match rng.pick(4) {
                0 => [0, 1, 2]
                    .map(|_| DVec3::new(c.x + rng.int(-60, 60), c.y + rng.int(-60, 60), c.z)),
                1 => [0, 1, 2]
                    .map(|_| DVec3::new(c.x, c.y + rng.int(-60, 60), c.z + rng.int(-60, 60))),
                2 => [c, c, c],
                _ => [0, 1, 2].map(|_| c + rng.ints(60)),
            };
            let on_box = DVec3::new(
                21.0 * rng.int(-1, 1),
                21.0 * rng.int(-1, 1),
                44.0 * rng.int(-1, 1),
            );
            let m = match rng.pick(4) {
                0 => DVec3::X * rng.int(20, 150) * rng.sign(),
                1 => {
                    let k = rng.int(20, 150);
                    DVec3::new(k * rng.sign(), k * rng.sign(), 0.0)
                }
                2 => {
                    let k = rng.int(20, 150);
                    DVec3::new(k * rng.sign(), k * rng.sign(), k * rng.sign())
                }
                _ => rng.ints(150),
            };
            let target = tri[rng.pick(3) as usize];
            (target - on_box - m, m * 2.0, E, tri)
        }),
        600,
        50,
    );

    // 4. The same three kinds 2e5 uu from the origin, where the levels
    //    are: positions are `f32` values there (steps of 1/64 uu).
    add(
        run("far", 0xA11C_E004, 2500, false, |rng| {
            let far = DVec3::new(
                rng.sign() * rng.range(1.5e5, 2.4e5),
                rng.sign() * rng.range(1.5e5, 2.4e5),
                rng.sign() * rng.range(2.0e4, 2.4e5),
            );
            let near = triangle(rng);
            let (s, d) = if rng.pick(2) == 0 {
                aimed(rng, &near)
            } else {
                grazing(rng, &near)
            };
            let tri = near.map(|v| f32v(v + far));
            let start = f32v(s + far);
            let end = f32v(s + d + far);
            (start, end - start, E, tri)
        }),
        700,
        10,
    );

    // 5. Very long sweeps (1e4 to 1e7 uu), near and far.
    add(
        run("long", 0xA11C_E005, 1000, false, |rng| {
            let far = if rng.pick(2) == 0 {
                DVec3::ZERO
            } else {
                DVec3::new(2.0e5, -1.9e5, 1.2e5)
            };
            let tri = triangle(rng).map(|v| f32v(v + far));
            let target = point_in(rng, &tri) + rng.vec(25.0);
            let dir = rng.direction();
            let len = 10f64.powf(rng.range(4.0, 7.0));
            let s = f32v(target - dir * (len * rng.range(0.05, 0.95)));
            let end = f32v(s + dir * len);
            (s, end - s, E, tri)
        }),
        300,
        5,
    );

    // 6. Short sweeps (1e-3 to 5 uu, a frame's move) that start 0.06 to
    //    1 uu from the triangle.
    add(
        run("short", 0xA11C_E006, 1200, false, |rng| {
            let tri = triangle(rng);
            let (s, d) = aimed(rng, &tri);
            let Some(c) = sweep_triangle(s, d, E, &tri, None, TOL, 1.0) else {
                return (s, DVec3::ZERO, E, tri);
            };
            // Back off from a contact position and move again, mostly
            // towards the triangle.
            let at = s + d * c.t;
            let start = at + c.normal * rng.range(0.06, 1.0) + rng.vec(0.1);
            let step = (rng.direction() * 0.7 - c.normal * rng.range(0.3, 2.0)).normalize_or_zero()
                * 10f64.powf(rng.range(-3.0, 0.7));
            (start, step, E, tri)
        }),
        200,
        5,
    );

    // 7. Degenerate triangles: points, segments, slivers; and boxes of
    //    other sizes.
    add(
        run("degenerate", 0xA11C_E007, 1800, false, |rng| {
            let p = f32v(rng.vec(40.0));
            let q = f32v(p + rng.vec(60.0));
            let tri = match rng.pick(6) {
                0 => [p, p, p],
                1 => [p, p, q],
                2 => [p, q, q],
                3 => [p, q, f32v(p + (q - p) * rng.range(-1.0, 2.0))],
                4 => [
                    p,
                    q,
                    f32v(
                        p + (q - p) * rng.unit() + rng.vec(1.0) * 10f64.powf(rng.range(-6.0, -1.0)),
                    ),
                ],
                _ => {
                    let step = rng.ints(3);
                    [p, p + step * rng.int(1, 9), p + step * rng.int(-9, 20)].map(f32v)
                }
            };
            let (s, d) = if rng.pick(2) == 0 {
                aimed(rng, &tri)
            } else {
                grazing(rng, &tri)
            };
            let e = if rng.pick(3) == 0 {
                DVec3::new(
                    rng.range(0.5, 60.0),
                    rng.range(0.5, 60.0),
                    rng.range(0.5, 90.0),
                )
            } else {
                E
            };
            (s, d, e, tri)
        }),
        500,
        10,
    );

    eprintln!("total: {total:?}");
    assert!(total.cases >= 16_000 && total.hits >= 5_000 && total.misses >= 2_000);
    assert!(
        total.world_ties >= 100,
        "exact ties were not exercised: {total:?}"
    );
    assert!(total.worst_time <= TIME_SLACK && total.worst_touch <= TOUCH_SLACK);
}

/// Exact ties between the box's own face axes, derived by hand. The
/// obstacle is a single point (a triangle whose three corners coincide, so
/// only the three face axes exist); the box starts at the origin.
///
/// - Point (71, 71, 0), motion (100, 100, 0): the +X face reaches x = 71
///   when the centre is at 50, and so does the +Y face: both enter at 0.5.
///   X is tested before Y, so the normal is −X.
/// - Point (71, 71, 94), motion (100, 100, 100): the top face reaches
///   z = 94 at 0.5 as well. Z is tested first: −Z.
/// - Point (71, −71, 0), motion (100, −100, 0): X and Y tie: −X.
/// - Point (0, 71, 94), motion (0, 100, 100): Y and Z tie: −Z.
/// - Point (−71, 0, −94), motion (−100, 0, −100): X and Z tie: +Z.
/// - Point (0, −71, 0), motion (30, −100, 0): Y alone: +Y at 0.5.
#[test]
fn ties_between_the_box_axes_keep_the_order_z_x_y() {
    let cases = [
        (
            DVec3::new(71.0, 71.0, 0.0),
            DVec3::new(100.0, 100.0, 0.0),
            DVec3::NEG_X,
        ),
        (
            DVec3::new(71.0, 71.0, 94.0),
            DVec3::new(100.0, 100.0, 100.0),
            DVec3::NEG_Z,
        ),
        (
            DVec3::new(71.0, -71.0, 0.0),
            DVec3::new(100.0, -100.0, 0.0),
            DVec3::NEG_X,
        ),
        (
            DVec3::new(0.0, 71.0, 94.0),
            DVec3::new(0.0, 100.0, 100.0),
            DVec3::NEG_Z,
        ),
        (
            DVec3::new(-71.0, 0.0, -94.0),
            DVec3::new(-100.0, 0.0, -100.0),
            DVec3::Z,
        ),
        (
            DVec3::new(0.0, -71.0, 0.0),
            DVec3::new(30.0, -100.0, 0.0),
            DVec3::Y,
        ),
    ];
    for (p, d, normal) in cases {
        let c = sweep_triangle(DVec3::ZERO, d, E, &[p, p, p], None, TOL, 1.0).unwrap();
        assert_eq!(
            (c.t, c.normal, c.penetrating),
            (0.5, normal, false),
            "{p} {d}"
        );
        // The same through a scene, near the origin and 2e5 uu away (all
        // the numbers are whole, so nothing is rounded there either).
        for far in [Vec3::ZERO, Vec3::new(200_000.0, -180_000.0, 150_000.0)] {
            let mut b = CollisionSceneBuilder::new();
            let m = b
                .add_mesh(vec![p.as_vec3() + far], vec![[0, 0, 0]])
                .unwrap();
            b.add_static(m, Affine::IDENTITY, info()).unwrap();
            let scene = b.build();
            let hit = scene
                .sweep_box(&[], far, far + d.as_vec3(), HALF, PAWN)
                .unwrap();
            assert_eq!((hit.t, hit.normal), (0.5, normal), "{p} {d} at {far}");
            assert_eq!(
                scene.sweep_box_brute_force(&[], far, far + d.as_vec3(), HALF, PAWN),
                Some(hit)
            );
        }
    }
}

/// Starts in contact, against the rule the module documents, with the
/// classification done here from the gaps along the thirteen axes.
///
/// From a contact position `P` with normal `n` (taken from a sweep that
/// started clear):
///
/// - starting at `P + n·δ` for δ of 0, +0.4 and −0.4 tolerances, a motion
///   that has any component along `n` (away) meets nothing, and the motion
///   `−n` is stopped within `max(δ, 0)` of travel, not penetrating, with
///   the normal `n` (when no other axis is within two tolerances of
///   touching, which would make the start a touching one on that axis too);
/// - starting 0.05 to 5 uu further in, where every axis overlaps by more
///   than two tolerances: a penetrating start at time 0 whose normal is an
///   axis of least overlap, reported only for a motion into that normal.
#[test]
fn starts_in_contact_follow_the_documented_rule() {
    let mut rng = Rng(0x5747_0001);
    let (mut touching, mut into, mut deep, mut deep_hits) = (0, 0, 0, 0);
    let mut tried = 0;
    while touching < 1500 || deep < 600 {
        tried += 1;
        assert!(tried < 200_000, "starved: {touching} {into} {deep}");
        let tri = triangle(&mut rng);
        let (s, d) = aimed(&mut rng, &tri);
        if !clear(s, d, E, &tri) {
            continue;
        }
        let Some(c) = sweep_triangle(s, d, E, &tri, None, TOL, 1.0) else {
            continue;
        };
        let n = c.normal;
        let at = s + d * c.t;
        let axes = candidate_axes(&relative(at, &tri).1);
        let gaps = |c: DVec3| -> Vec<(DVec3, f64)> {
            let (rc, rt) = relative(c, &tri);
            axes.iter()
                .flat_map(|a| [*a, -*a])
                .map(|a| (a, gap(a, rc, E, &rt)))
                .collect()
        };

        let delta = [0.0, 0.4 * TOL, -0.4 * TOL][rng.pick(3) as usize];
        let start = at + n * delta;
        // Away, at any angle.
        let side = n.cross(rng.direction()).normalize_or_zero();
        let away = (n * rng.range(0.01, 1.0) + side * rng.range(-1.0, 1.0)) * rng.range(0.5, 60.0);
        assert!(
            sweep_triangle(start, away, E, &tri, None, TOL, 1.0).is_none(),
            "leaving a contact is blocked: start={start:?} d={away:?} n={n:?} tri={tri:?}"
        );
        touching += 1;
        // Straight in, when `n` is the only axis that is not clearly
        // overlapping at the start.
        let alone = gaps(start)
            .iter()
            .all(|(a, g)| *g < -2.0 * TOL || (a.cross(n).length() < 1.0e-9 && a.dot(n) > 0.0));
        if alone {
            let len = rng.range(0.5, 60.0);
            let hit = sweep_triangle(start, -n * len, E, &tri, None, TOL, 1.0);
            let Some(h) = hit else {
                panic!("a move into a contact passes: start={start:?} n={n:?} tri={tri:?}");
            };
            assert!(!h.penetrating, "{h:?}");
            assert!(h.t * len <= delta.max(0.0) + 1.0e-9, "{h:?} delta {delta}");
            assert!(
                h.normal.cross(n).length() < 1.0e-9 && h.normal.dot(n) > 0.0,
                "{h:?} {n:?}"
            );
            into += 1;
        }

        // Further in.
        let inside = at - n * rng.range(0.05, 5.0);
        let g = gaps(inside);
        // Along an axis and against it the two gaps add up to minus the
        // widths, so a start is inside on an axis when both are negative:
        // the overlap on the axis is the smaller of the two depths.
        let mut least = f64::NEG_INFINITY;
        let mut all_deep = true;
        for pair in g.chunks(2) {
            let overlap = pair[0].1.max(pair[1].1);
            all_deep &= overlap < -2.0 * TOL;
            least = least.max(overlap);
        }
        if !all_deep {
            continue;
        }
        deep += 1;
        for (a, depth) in &g {
            let side = a.cross(rng.direction()).normalize_or_zero();
            let motion = (side * rng.range(-0.5, 0.5) - *a) * rng.range(0.5, 30.0);
            let hit = sweep_triangle(inside, motion, E, &tri, None, TOL, 1.0);
            if let Some(h) = hit {
                deep_hits += 1;
                assert!(h.penetrating && h.t == 0.0, "{h:?}");
                assert!(h.normal.dot(motion) < 0.0, "{h:?} {motion:?}");
                let (rc, rt) = relative(inside, &tri);
                let own = gap(h.normal, rc, E, &rt);
                assert!(
                    (own - least).abs() <= 1.0e-9,
                    "normal {:?} overlaps by {own}, the least is {least}",
                    h.normal
                );
            } else {
                // Not reported: then the motion does not go into an axis
                // of least overlap (this one is not it, or it is one of
                // several and the kernel kept another).
                assert!(
                    (*depth - least).abs() > 1.0e-9
                        || g.iter()
                            .filter(|(_, x)| (*x - least).abs() <= 1.0e-9)
                            .count()
                            > 1,
                    "a move into the least-overlap axis {a:?} passes: inside={inside:?} tri={tri:?}"
                );
            }
        }
    }
    eprintln!(
        "starts in contact: {touching} leaving, {into} entering, {deep} deep ({deep_hits} blocked)"
    );
    assert!(into > 1000 && deep_hits > 500, "{into} {deep_hits}");
}

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

/// A rotation about a random axis (Rodrigues), a scale of 0.4 to 2.5 per
/// axis and, one time in four, a mirror.
fn placement(rng: &mut Rng, at: DVec3) -> Affine {
    let axis = rng.direction();
    let angle = rng.range(0.0, std::f64::consts::TAU);
    let turn = |v: DVec3| {
        v * angle.cos() + axis.cross(v) * angle.sin() + axis * (axis.dot(v) * (1.0 - angle.cos()))
    };
    let mut scale = DVec3::new(
        rng.range(0.4, 2.5),
        rng.range(0.4, 2.5),
        rng.range(0.4, 2.5),
    );
    if rng.pick(4) == 0 {
        scale[rng.pick(3) as usize] *= -1.0;
    }
    Affine {
        rows: [
            turn(DVec3::X) * scale.x,
            turn(DVec3::Y) * scale.y,
            turn(DVec3::Z) * scale.z,
        ],
        translation: at,
    }
}

/// Every triangle of every static instance, in world space.
fn world_triangles(scene: &CollisionScene) -> Vec<[DVec3; 3]> {
    let mut out = Vec::new();
    for inst in scene.statics() {
        let count = scene.meshes()[inst.mesh as usize].triangles().len() as u32;
        for k in 0..count {
            out.push(scene.world_triangle(inst, k).unwrap());
        }
    }
    out
}

/// The centre's path `s → s + d` comes within `reach` of the triangle's
/// bounding box (a plain slab test: the cheap rejection of the reference).
fn near_path(t: &[DVec3; 3], s: DVec3, d: DVec3, reach: DVec3) -> bool {
    let lo = t[0].min(t[1]).min(t[2]) - reach;
    let hi = t[0].max(t[1]).max(t[2]) + reach;
    segment_meets_box(s, s + d, (lo + hi) * 0.5, (hi - lo) * 0.5)
}

/// First contact with any of the triangles.
fn reference_all(tris: &[[DVec3; 3]], s: DVec3, d: DVec3, e: DVec3) -> Reference {
    let mut best = Reference::Miss;
    for t in tris {
        if !near_path(t, s, d, e + DVec3::ONE) {
            continue;
        }
        if let Reference::Hit(x) = reference(s, d, e, t) {
            best = match best {
                Reference::Hit(y) if y <= x => best,
                _ => Reference::Hit(x),
            };
        }
    }
    best
}

/// Distance from the box at `c` to the nearest triangle.
fn distance_all(tris: &[[DVec3; 3]], c: DVec3, e: DVec3) -> f64 {
    let mut best = f64::INFINITY;
    for t in tris {
        if near_path(t, c, DVec3::ZERO, e + DVec3::splat(best.min(1.0e6))) {
            best = best.min(distance_at(c, DVec3::ZERO, e, t, 0.0));
        }
    }
    best
}

/// The spacing of `f32` values at `m`.
fn spacing(m: f32) -> f64 {
    f64::from(m.abs().next_up()) - f64::from(m.abs())
}

/// The scene's start-contact tolerance as documented: 1e-3 plus two `f32`
/// steps at the largest coordinate of the sweep's ends.
fn band(s: Vec3, e: Vec3) -> f64 {
    1.0e-3 + 2.0 * spacing(s.abs().max_element().max(e.abs().max_element()))
}

/// One scene sweep that starts clear of every triangle by 0.5 uu: the
/// accelerated query, the brute-force query and the reference over the
/// scene's triangles (for convex solids these are the solid's surface).
/// The scene treats an overlap of up to its tolerance on an axis as
/// touching, so "the first contact" is checked with a box smaller by that.
fn check_scene(scene: &CollisionScene, tris: &[[DVec3; 3]], s: Vec3, e: Vec3, hits: &mut usize) {
    let fast = scene.sweep_box(&[], s, e, HALF, PAWN);
    assert_eq!(
        fast,
        scene.sweep_box_brute_force(&[], s, e, HALF, PAWN),
        "{s} -> {e}"
    );
    let (sd, dd) = (s.as_dvec3(), e.as_dvec3() - s.as_dvec3());
    let len = dd.length();
    let smaller = E - DVec3::splat(band(s, e) + GRAZE);
    match fast {
        Some(hit) => {
            *hits += 1;
            assert!(!hit.penetrating && hit.t > 0.0 && hit.t <= 1.0, "{hit:?}");
            assert!(
                (hit.normal.length() - 1.0).abs() < 1.0e-9 && hit.normal.dot(dd) < 0.0,
                "{hit:?}"
            );
            let touch = distance_all(tris, sd + dd * hit.t, E);
            assert!(
                touch <= 1.0e-6,
                "no contact at {hit:?}: {touch} uu away ({s} -> {e})"
            );
            let before = hit.t - 1.0e-5 / len;
            if before > 0.0 {
                assert_eq!(
                    reference_all(tris, sd, dd * before, smaller),
                    Reference::Miss,
                    "an earlier contact was missed before {hit:?} ({s} -> {e})"
                );
            }
        }
        None => assert_eq!(
            reference_all(tris, sd, dd, smaller),
            Reference::Miss,
            "missed ({s} -> {e})"
        ),
    }
}

/// Closed convex polyhedra: a block, a tetrahedron, an octahedron and a
/// wedge (a triangular prism).
fn solids() -> Vec<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let block: Vec<Vec3> = (0..8)
        .map(|i| {
            Vec3::new(
                (i & 1) as f32 * 110.0,
                ((i >> 1) & 1) as f32 * 70.0,
                ((i >> 2) & 1) as f32 * 45.0,
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
        Vec3::new(95.0, 5.0, 0.0),
        Vec3::new(25.0, 90.0, 10.0),
        Vec3::new(35.0, 25.0, 100.0),
    ];
    let tetra_tris = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
    let octa = vec![
        Vec3::new(60.0, 0.0, 0.0),
        Vec3::new(-60.0, 0.0, 0.0),
        Vec3::new(0.0, 45.0, 0.0),
        Vec3::new(0.0, -45.0, 0.0),
        Vec3::new(0.0, 0.0, 80.0),
        Vec3::new(0.0, 0.0, -80.0),
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
    let wedge = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(120.0, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 60.0),
        Vec3::new(0.0, 80.0, 0.0),
        Vec3::new(120.0, 80.0, 0.0),
        Vec3::new(0.0, 80.0, 60.0),
    ];
    let wedge_tris = vec![
        [0, 1, 2],
        [3, 5, 4],
        [0, 3, 4],
        [0, 4, 1],
        [0, 2, 5],
        [0, 5, 3],
        [1, 4, 5],
        [1, 5, 2],
    ];
    vec![
        (block, block_tris),
        (tetra, tetra_tris),
        (octa, octa_tris),
        (wedge, wedge_tris),
    ]
}

/// Convex solids, turned, scaled, mirrored and placed near the origin or
/// 2e5 uu from it, against the reference over their surface triangles.
#[test]
fn convex_solids_agree_with_the_reference_over_their_surface() {
    let mut rng = Rng(0x501D_0001);
    let (mut hits, mut sweeps) = (0, 0);
    for round in 0..32 {
        let (v, t) = solids().swap_remove(round % 4);
        let middle = v.iter().map(|p| p.as_dvec3()).sum::<DVec3>() / v.len() as f64;
        let centre = if round % 2 == 0 {
            rng.vec(300.0)
        } else {
            DVec3::new(2.0e5 * rng.sign(), 1.9e5 * rng.sign(), 1.1e5 * rng.sign()) + rng.vec(300.0)
        };
        let to_world = if round < 4 {
            Affine::from_translation(centre)
        } else {
            placement(&mut rng, centre)
        };
        let mut b = CollisionSceneBuilder::new();
        let m = b.add_convex_mesh(v, t).unwrap();
        assert!(b.mesh(m).unwrap().hull().is_some(), "round {round}");
        b.add_static(m, to_world, info()).unwrap();
        let scene = b.build();
        let tris = world_triangles(&scene);
        let mut done = 0;
        while done < 50 {
            let aim = to_world.point(middle) + rng.vec(70.0);
            let from = aim + rng.direction() * rng.range(350.0, 800.0);
            let s = from.as_vec3();
            let e = (from + (aim - from) * rng.range(0.4, 1.5)).as_vec3();
            if s == e || distance_all(&tris, s.as_dvec3(), E) <= 0.5 {
                continue;
            }
            done += 1;
            sweeps += 1;
            check_scene(&scene, &tris, s, e, &mut hits);
        }
    }
    eprintln!("convex solids: {hits} hits in {sweeps} sweeps");
    assert!(hits > 500, "{hits} of {sweeps}");
}

/// Triangle soups and solids 2e5 uu from the origin, swept by short and by
/// very long moves (up to 1e6 uu): the accelerated query equals the
/// brute-force one bit for bit and both agree with the reference.
#[test]
fn far_away_and_very_long_sweeps_agree_with_the_reference() {
    let mut rng = Rng(0xFA12_0001);
    let centre = DVec3::new(201_337.0, -187_211.0, 143_009.0);
    let mut b = CollisionSceneBuilder::new();
    let mut meshes = Vec::new();
    for _ in 0..6 {
        let n = 12 + rng.pick(20) as usize;
        let vertices: Vec<Vec3> = (0..n).map(|_| rng.vec(120.0).as_vec3()).collect();
        let tris = (0..n)
            .map(|_| [0, 1, 2].map(|_| rng.pick(n as u64) as u32))
            .collect();
        meshes.push(b.add_mesh(vertices, tris).unwrap());
    }
    for (v, t) in solids() {
        meshes.push(b.add_convex_mesh(v, t).unwrap());
    }
    for _ in 0..40 {
        let m = meshes[rng.pick(meshes.len() as u64) as usize];
        let at = centre + rng.vec(900.0);
        b.add_static(m, placement(&mut rng, at), info()).unwrap();
    }
    let scene = b.build();
    let tris = world_triangles(&scene);
    assert!(tris.len() > 300);
    let (mut hits, mut sweeps, mut long_hits) = (0, 0, 0);
    while sweeps < 900 {
        let aim = centre + rng.vec(900.0);
        let long = sweeps % 3 == 0;
        let reach = if long {
            10f64.powf(rng.range(4.0, 6.0))
        } else {
            rng.range(30.0, 900.0)
        };
        let dir = rng.direction();
        let from = aim - dir * (reach * rng.range(0.1, 0.9));
        let s = from.as_vec3();
        let e = (from + dir * reach).as_vec3();
        if s == e || distance_all(&tris, s.as_dvec3(), E) <= 0.5 {
            continue;
        }
        sweeps += 1;
        let before = hits;
        check_scene(&scene, &tris, s, e, &mut hits);
        if long {
            long_hits += hits - before;
        }
    }
    eprintln!("far: {hits} hits in {sweeps} sweeps ({long_hits} on long sweeps)");
    assert!(hits > 200 && long_hits > 60, "{hits} {long_hits}");
}

/// An L-shaped prism: the union of x 0..100, y 0..40 and x 0..40,
/// y 0..100, for z 0..50. It is closed but not convex.
fn l_prism() -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let outline = [
        (0.0, 0.0),
        (100.0, 0.0),
        (100.0, 40.0),
        (40.0, 40.0),
        (40.0, 100.0),
        (0.0, 100.0),
    ];
    let mut v: Vec<Vec3> = outline.iter().map(|&(x, y)| Vec3::new(x, y, 0.0)).collect();
    v.extend(outline.iter().map(|&(x, y)| Vec3::new(x, y, 50.0)));
    let mut t = vec![[0, 2, 1], [0, 3, 2], [0, 4, 3], [0, 5, 4]];
    t.extend([[6, 7, 8], [6, 8, 9], [6, 9, 10], [6, 10, 11]]);
    for i in 0..6u32 {
        let j = (i + 1) % 6;
        t.push([i, j, j + 6]);
        t.push([i, j + 6, i + 6]);
    }
    (v, t)
}

/// The original's convex elements are not all convex (MESHES.md: on 584 of
/// 3,446 a vertex lies outside one of the element's own planes). The box
/// check uses an element's vertices with its face and edge directions, so
/// what it sweeps is the solid those directions cut around the vertices.
///
/// The L-shaped prism with its own directions (faces X, Y, Z; edges X, Y,
/// Z) is, for the box, the block x 0..100, y 0..100, z 0..50: no listed
/// direction tells the notch from the block. Walking in −x at y = 70,
/// z = 25 from x = 300 the box stops with its centre at 100 + 21 = 121,
/// after 179 of 300, although the L's own face there is at x = 40. Without
/// directions the same mesh has no solid and is swept triangle by
/// triangle.
#[test]
fn a_concave_element_is_swept_as_the_solid_its_directions_describe() {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let (v, t) = l_prism();
    assert!(ConvexHull::from_triangles(&v, &t).is_none());
    let mut b = CollisionSceneBuilder::new();
    let listed = b
        .add_convex_hull(v.clone(), t.clone(), &axes, &axes)
        .unwrap();
    assert!(b.mesh(listed).unwrap().hull().is_some());
    let plain = b.add_convex_mesh(v.clone(), t.clone()).unwrap();
    assert!(b.mesh(plain).unwrap().hull().is_none());
    b.add_static_with(
        listed,
        Affine::IDENTITY,
        info(),
        InstanceOptions::of_kind(PrimitiveKind::SimpleHull),
    )
    .unwrap();
    let scene = b.build();
    let s = Vec3::new(300.0, 70.0, 25.0);
    let hit = scene
        .sweep_box(&[], s, Vec3::new(0.0, 70.0, 25.0), HALF, PAWN)
        .unwrap();
    assert!((hit.t - 179.0 / 300.0).abs() < 1.0e-12, "{hit:?}");
    assert_eq!(
        (hit.normal, hit.kind),
        (DVec3::X, PrimitiveKind::SimpleHull)
    );
    // Inside the notch (centre at (80, 80)) the box overlaps the solid.
    assert!(scene.overlaps_box(&[], Vec3::new(80.0, 80.0, 25.0), HALF, PAWN));

    // Whatever the directions, the solid holds its vertices' hull, so it
    // is never smaller than the element's surface: wherever a sweep meets
    // a triangle of the L, the solid is met no later.
    let mut rng = Rng(0xC0DC_A7E1);
    let (mut met, mut earlier) = (0, 0);
    for round in 0..12 {
        let at = rng.vec(400.0);
        let to_world = placement(&mut rng, at);
        let mut b = CollisionSceneBuilder::new();
        let m = b
            .add_convex_hull(v.clone(), t.clone(), &axes, &axes)
            .unwrap();
        b.add_static(m, to_world, info()).unwrap();
        let scene = b.build();
        let tris = world_triangles(&scene);
        let mut done = 0;
        while done < 60 {
            let aim = to_world.point(DVec3::new(50.0, 50.0, 25.0)) + rng.vec(90.0);
            let from = aim + rng.direction() * rng.range(300.0, 600.0);
            let (s, e) = (from.as_vec3(), aim.as_vec3());
            if scene.overlaps_box(&[], s, HALF + Vec3::ONE, PAWN) {
                continue;
            }
            done += 1;
            let got = scene.sweep_box(&[], s, e, HALF, PAWN);
            assert_eq!(
                got,
                scene.sweep_box_brute_force(&[], s, e, HALF, PAWN),
                "round {round}"
            );
            let (sd, dd) = (s.as_dvec3(), e.as_dvec3() - s.as_dvec3());
            if let Reference::Hit(tr) = reference_all(&tris, sd, dd, E - DVec3::splat(GRAZE)) {
                let hit = got.unwrap_or_else(|| panic!("through the surface at {tr}: {s} -> {e}"));
                assert!(hit.t <= tr + 1.0e-9, "{hit:?} after the surface at {tr}");
                met += 1;
                earlier += usize::from(hit.t < tr - 1.0e-6);
            }
        }
    }
    eprintln!(
        "concave element: {met} sweeps met the surface, {earlier} stopped earlier at the solid"
    );
    assert!(met > 300 && earlier > 20, "{met} {earlier}");
}

fn junk(rng: &mut Rng) -> f32 {
    match rng.pick(16) {
        0 => f32::NAN,
        1 => f32::INFINITY,
        2 => f32::NEG_INFINITY,
        3 => 0.0,
        4 => -0.0,
        5 => 1.0e30,
        6 => -1.0e30,
        7 => 1.0e-30,
        8 => f32::MAX,
        9 => f32::MIN_POSITIVE,
        _ => rng.range(-100.0, 100.0) as f32,
    }
}

fn junk_vec(rng: &mut Rng, rate: u64) -> Vec3 {
    if rng.pick(rate) == 0 {
        Vec3::new(junk(rng), junk(rng), junk(rng))
    } else {
        rng.vec(100.0).as_vec3()
    }
}

/// Elements that are no convex solid, with direction lists that are
/// wrong, repeated, empty, enormous or not numbers, cannot make a query
/// panic or hang, and every answer is a finite contact inside the sweep.
/// With sane sizes the accelerated queries still equal the brute-force
/// ones.
#[test]
fn hostile_hulls_never_panic_or_hang() {
    let mut rng = Rng(0xBAD_4011);
    let (mut solids, mut hits, mut wild) = (0, 0, 0);
    for round in 0..400 {
        // One round in eight has absurd coordinates somewhere.
        let rate = if round % 8 == 0 { 6 } else { u64::MAX };
        let n = 1 + rng.pick(40) as usize;
        let vertices: Vec<Vec3> = (0..n).map(|_| junk_vec(&mut rng, rate)).collect();
        let triangles: Vec<[u32; 3]> = (0..1 + rng.pick(60))
            .map(|_| [0, 1, 2].map(|_| rng.pick(n as u64 + 2) as u32))
            .collect();
        let dirs = |rng: &mut Rng| -> Vec<Vec3> {
            (0..rng.pick(40))
                .map(|_| match rng.pick(6) {
                    0 => Vec3::new(junk(rng), junk(rng), junk(rng)),
                    1 => Vec3::X,
                    2 => Vec3::NEG_X,
                    _ => rng.vec(1.0).as_vec3(),
                })
                .collect()
        };
        let (faces, edges) = (dirs(&mut rng), dirs(&mut rng));

        // The pieces on their own.
        let _ = ConvexHull::from_triangles(&vertices, &triangles);
        if let Some(h) = ConvexHull::with_directions(&vertices, &faces, &edges) {
            assert!(h.vertices().iter().all(|p| p.is_finite()));
            for d in h.face_directions().iter().chain(h.edge_directions()) {
                assert!((d.length() - 1.0).abs() < 1.0e-9, "{d:?}");
            }
        }
        // The kernel with the lists as they are (nothing cleaned).
        let raw: Vec<DVec3> = vertices.iter().map(|v| v.as_dvec3()).collect();
        let raw_faces: Vec<DVec3> = faces.iter().map(|v| v.as_dvec3()).collect();
        let raw_edges: Vec<DVec3> = edges.iter().map(|v| v.as_dvec3()).collect();
        let (s, d) = (rng.vec(300.0), rng.vec(400.0));
        if let Some(c) = sweep_hull(s, d, E, &raw, &raw_faces, &raw_edges, TOL, 1.0) {
            assert!((0.0..=1.0).contains(&c.t) && c.normal.is_finite(), "{c:?}");
        }
        let _ = overlaps_hull(s, E, &raw, &raw_faces, &raw_edges);

        // In a scene, with and without the lists, placed (one time in six
        // with a scale of zero on an axis, which bakes the mesh).
        let mut b = CollisionSceneBuilder::new();
        let mut placed = 0;
        let listed = b.add_convex_hull(vertices.clone(), triangles.clone(), &faces, &edges);
        let plain = b.add_convex_mesh(vertices.clone(), triangles.clone());
        for m in [listed, plain].into_iter().flatten() {
            solids += usize::from(b.mesh(m).unwrap().hull().is_some());
            for _ in 0..2 {
                let at = rng.vec(200.0);
                let mut to_world = placement(&mut rng, at);
                if rng.pick(6) == 0 {
                    to_world.rows[rng.pick(3) as usize] = DVec3::ZERO;
                }
                placed += usize::from(b.add_static(m, to_world, info()).is_some());
            }
        }
        let scene = b.build();
        let sane = rate == u64::MAX;
        wild += usize::from(!sane && placed > 0);
        for _ in 0..12 {
            let s = rng.vec(500.0).as_vec3();
            let e = if rng.pick(10) == 0 {
                s
            } else {
                rng.vec(500.0).as_vec3()
            };
            let fast = scene.sweep_box(&[], s, e, HALF, PAWN);
            let slow = scene.sweep_box_brute_force(&[], s, e, HALF, PAWN);
            let cylinder = scene.sweep_cylinder(&[], s, e, 21.0, 44.0, PAWN);
            let ray = scene.raycast(&[], s, e, QueryFilter::TRACE);
            let _ = scene.overlaps_box(&[], s, HALF, PAWN);
            let _ = scene.overlaps_cylinder(&[], s, 21.0, 44.0, PAWN);
            for h in [fast, slow, cylinder, ray].into_iter().flatten() {
                hits += 1;
                assert!((0.0..=1.0).contains(&h.t), "{h:?}");
                assert!((h.normal.length() - 1.0).abs() < 1.0e-6, "{h:?}");
            }
            if sane {
                assert_eq!(fast, slow, "round {round}: {s} -> {e}");
                assert_eq!(
                    cylinder,
                    scene.sweep_cylinder_brute_force(&[], s, e, 21.0, 44.0, PAWN),
                    "round {round}"
                );
                assert_eq!(
                    ray,
                    scene.raycast_brute_force(&[], s, e, QueryFilter::TRACE)
                );
            }
        }
    }
    eprintln!("hostile hulls: {solids} solids, {hits} hits, {wild} scenes with absurd coordinates");
    assert!(
        solids > 100 && hits > 300 && wild > 10,
        "{solids} {hits} {wild}"
    );

    // The largest lists that are accepted: 256 vertices, 2,048 face
    // directions, 6,144 edge directions. One sweep is some five million
    // projections; it ends.
    let vertices: Vec<Vec3> = (0..256).map(|_| rng.vec(100.0).as_vec3()).collect();
    let faces: Vec<Vec3> = (0..2048).map(|_| rng.direction().as_vec3()).collect();
    let edges: Vec<Vec3> = (0..6144).map(|_| rng.direction().as_vec3()).collect();
    let big = ConvexHull::with_directions(&vertices, &faces, &edges).unwrap();
    assert!(big.face_directions().len() > 2000 && big.edge_directions().len() > 6000);
    let c = sweep_hull(
        DVec3::new(900.0, 10.0, 20.0),
        DVec3::new(-900.0, 0.0, 0.0),
        E,
        big.vertices(),
        big.face_directions(),
        big.edge_directions(),
        TOL,
        1.0,
    )
    .unwrap();
    assert!(c.t > 0.0 && c.t < 1.0 && !c.penetrating, "{c:?}");
    // One more of anything and the lists are refused.
    let mut more = vertices.clone();
    more.push(Vec3::ONE);
    assert!(ConvexHull::with_directions(&more, &faces, &edges).is_none());
    let mut more = faces.clone();
    more.push(Vec3::X);
    assert!(ConvexHull::with_directions(&vertices, &more, &edges).is_none());
}
