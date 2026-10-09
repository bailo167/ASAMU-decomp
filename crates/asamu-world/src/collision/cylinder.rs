//! Exact swept upright cylinder and ray tests against single triangles.
//!
//! # Shape
//!
//! The pawn is an upright, flat-ended cylinder of radius `r` and half-height
//! `h` (UE3 `CylinderComponent`: `CollisionRadius`, `CollisionHeight`; the
//! player uses 21 × 44, `docs/reverse-engineering/DEFAULTS.md`). The same
//! shape is used by `asamu_player::world::BoxWorld`, so box worlds and
//! triangle worlds agree on contact geometry. (Stock UE3 performs extent
//! traces against BSP and static-mesh kDOP trees with an axis-aligned *box*
//! of the cylinder's extent; whether ASAMU's pawn collides as a box or as a
//! cylinder is not settled by the evidence — TENTATIVE. The cylinder is the
//! shape the movement spec and the rest of this code base use.)
//!
//! # Method
//!
//! The first time of contact between the moving cylinder and a triangle is
//! the first time a feature of one touches a feature of the other. Every
//! such pairing is solved in closed form (all arithmetic in `f64`):
//!
//! | cylinder feature | triangle feature | equation |
//! |---|---|---|
//! | support point (lowest/highest rim point along the face normal) | face | linear (plane), point in triangle |
//! | lateral surface | vertex | quadratic (2-D distance = r), height range |
//! | cap disks | vertex | linear (height), 2-D distance ≤ r |
//! | lateral surface | edge | linear (2-D line distance = r), segment and height range; vertical edges: quadratic |
//! | rim circles | non-horizontal edge | quadratic (the edge's crossing of the rim plane at distance r) |
//! | cap disks | horizontal edge | linear (height), 2-D segment–disk distance |
//!
//! Each solution is a genuine contact configuration, and together they cover
//! every way two convex bodies can first touch, so the earliest valid root
//! is the exact time of impact. Degenerate support sets (horizontal faces
//! touched by a whole cap, vertical faces touched along a whole side line)
//! are handled by testing one representative point and relying on the edge
//! and vertex pairings for partial overlaps.
//!
//! # Contacts at the start
//!
//! A configuration that already touches or overlaps by less than the
//! tolerance `tol` counts as a contact at `t = 0` when the motion goes into
//! the contact normal, and is ignored otherwise (so a resting shape can
//! always move away or slide). An overlap deeper than `tol`
//! ([`overlaps`] with the shape shrunk by `tol`) is a **penetrating** start:
//! it blocks only motion into the triangle's face normal (oriented towards
//! the shape centre), mirroring the box world's least-penetration rule.
//!
//! # One-sided triangles
//!
//! Triangles of closed convex hulls (blocking volumes) are one-sided: they
//! are ignored when the shape centre starts behind their plane, so a shape
//! inside a hull can always leave it while hulls stay solid from outside.

use glam::{DVec2, DVec3};

/// Faces whose horizontal normal part is below this are treated as
/// horizontal (support set = whole cap disk); likewise for vertical faces.
const FLAT_EPS: f64 = 1.0e-9;
/// Relative slack for "inside the triangle / on the segment" decisions at
/// feature boundaries (neighbouring features also report these contacts).
const BARY_EPS: f64 = 1.0e-9;

/// A contact found by [`sweep`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// Fraction of the motion before contact, `[0, 1]`.
    pub t: f64,
    /// Unit normal pointing from the triangle towards the shape.
    pub normal: DVec3,
    /// The shape started overlapping the triangle by more than the
    /// tolerance.
    pub penetrating: bool,
}

#[derive(Clone, Copy)]
struct Best {
    t: f64,
    normal: DVec3,
}

#[derive(Default)]
struct Acc {
    best: Option<Best>,
}

impl Acc {
    fn consider(&mut self, t: f64, normal: DVec3) {
        if !(t.is_finite() && normal.is_finite()) {
            return;
        }
        let t = t.clamp(0.0, 1.0);
        if self.best.is_none_or(|b| t < b.t) {
            self.best = Some(Best { t, normal });
        }
    }
}

fn xy(v: DVec3) -> DVec2 {
    DVec2::new(v.x, v.y)
}

/// Absolute length tolerance for boundary decisions at the scale of the
/// inputs.
fn slack(scale: f64) -> f64 {
    1.0e-7 + scale * 1.0e-12
}

/// The earliest entering root of `|p0 + t·v|² = r²` (2-D), given the signed
/// distance at the start. Touching or slightly inside (within `tol`) while
/// approaching gives `t = 0`. Grazing passes, whose closest approach does
/// not get deeper than `tol` inside the circle, are not contacts (a shape
/// sliding along a feature must not snag on it).
fn circle_entry(p0: DVec2, v: DVec2, r: f64, tol: f64) -> Option<f64> {
    let a = v.dot(v);
    let b = p0.dot(v);
    if a.is_nan() || a <= 0.0 || b >= 0.0 {
        return None; // not approaching the axis
    }
    let closest = p0.perp_dot(v).abs() / a.sqrt();
    if closest.is_nan() || closest >= r - tol {
        return None;
    }
    let dist0 = p0.length() - r;
    if dist0 < -tol {
        return None;
    }
    if dist0 <= 0.0 {
        return Some(0.0);
    }
    let c = p0.dot(p0) - r * r;
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    // Numerically stable smaller root: q = -(b + sign(b)·sqrt(disc)), b < 0.
    let q = -b + disc.sqrt();
    let t = c / q;
    t.is_finite().then_some(t)
}

/// Where a contact coordinate lies relative to a bound (`|value|` against
/// `bound`): well inside, in the boundary band `[bound − band, bound + eps]`,
/// or outside.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Interior,
    Boundary,
    Outside,
}

fn zone(value: f64, bound: f64, band: f64, eps: f64) -> Zone {
    if value < bound - band {
        Zone::Interior
    } else if value <= bound + eps {
        Zone::Boundary
    } else {
        Zone::Outside
    }
}

/// Linear gap `g0 + t·k` closing to zero (`k < 0`), with the start
/// tolerance.
fn linear_entry(g0: f64, k: f64, tol: f64) -> Option<f64> {
    if k.is_nan() || k >= 0.0 || g0 < -tol {
        return None;
    }
    if g0 <= 0.0 {
        return Some(0.0);
    }
    let t = -g0 / k;
    t.is_finite().then_some(t)
}

/// 2-D distance from `p` to the segment `a`–`b`.
fn point_segment_distance(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    if l2 <= 0.0 {
        return (p - a).length();
    }
    let u = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    (p - (a + ab * u)).length()
}

/// Horizontal unit normal from a feature towards the axis (`rel` = axis −
/// feature in 2-D); straight against the motion when the axis is on it.
fn radial_normal(rel: DVec2, d: DVec3) -> DVec3 {
    let len = rel.length();
    let n = if len > 0.0 {
        rel / len
    } else {
        (-xy(d)).normalize_or_zero()
    };
    DVec3::new(n.x, n.y, 0.0)
}

/// Squared 2-D distance between the segments `a`–`b` and `c`–`d` (zero
/// when they cross).
fn segment_segment_distance2(a: DVec2, b: DVec2, c: DVec2, d: DVec2) -> f64 {
    let ab = b - a;
    let cd = d - c;
    let denom = ab.perp_dot(cd);
    if denom != 0.0 {
        let u = (c - a).perp_dot(cd) / denom;
        let w = (c - a).perp_dot(ab) / denom;
        if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&w) {
            return 0.0;
        }
    }
    let p2 = |p: DVec2, s0: DVec2, s1: DVec2| {
        let seg = s1 - s0;
        let l2 = seg.dot(seg);
        let u = if l2 > 0.0 {
            ((p - s0).dot(seg) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p - (s0 + seg * u)).length_squared()
    };
    p2(a, c, d)
        .min(p2(b, c, d))
        .min(p2(c, a, b))
        .min(p2(d, a, b))
}

/// 2-D distance from `p` to the segment `a`–`b` and the closest point.
fn segment_closest(p: DVec2, a: DVec2, b: DVec2) -> (f64, DVec2) {
    let ab = b - a;
    let l2 = ab.dot(ab);
    let q = if l2 <= 0.0 {
        a
    } else {
        a + ab * ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    };
    ((p - q).length(), q)
}

/// `true` when `p` (on the triangle's plane) lies inside the triangle, with
/// a small relative slack. `n` must be the winding normal
/// (`(b − a) × (c − a)` direction), not a re-oriented one.
fn inside_triangle(p: DVec3, v: &[DVec3; 3], n: DVec3, eps: f64) -> bool {
    for i in 0..3 {
        let a = v[i];
        let b = v[(i + 1) % 3];
        let edge = b - a;
        let len = edge.length();
        if len <= 0.0 {
            continue;
        }
        // Signed distance of p from the edge line, inside positive.
        let inward = n.cross(edge) / len;
        if inward.dot(p - a) < -eps {
            return false;
        }
    }
    true
}

/// Unit normal of the triangle (`(b − a) × (c − a)` normalized), if it has
/// an area.
#[must_use]
pub fn triangle_normal(v: &[DVec3; 3]) -> Option<DVec3> {
    let n = (v[1] - v[0]).cross(v[2] - v[0]);
    let len = n.length();
    let scale = (v[1] - v[0]).length() * (v[2] - v[0]).length();
    (len.is_finite() && len > 1.0e-12 * scale && len > 0.0).then(|| n / len)
}

/// Static overlap of the solid cylinder `(center, r, h)` with the triangle
/// (touching does not count). Exact: the triangle is clipped to the
/// cylinder's height slab and the 2-D distance from the axis to the clipped
/// polygon is compared with `r`.
#[must_use]
pub fn overlaps(center: DVec3, r: f64, h: f64, v: &[DVec3; 3]) -> bool {
    if !(r > 0.0 && h > 0.0) {
        return false;
    }
    // The triangle's 2-D box must come within `r` of the axis, and its plane
    // must pass through the cylinder (exact pre-checks).
    let c2 = xy(center);
    let lo2 = xy(v[0]).min(xy(v[1])).min(xy(v[2]));
    let hi2 = xy(v[0]).max(xy(v[1])).max(xy(v[2]));
    if (c2.clamp(lo2, hi2) - c2).length_squared() >= r * r {
        return false;
    }
    if let Some(n) = triangle_normal(v) {
        let reach = r * xy(n).length() + h * n.z.abs();
        if n.dot(center - v[0]).abs() >= reach {
            return false;
        }
    }
    let lo = center.z - h;
    let hi = center.z + h;
    // Sutherland–Hodgman against z >= lo, then z <= hi.
    let mut poly: [DVec3; 8] = [DVec3::ZERO; 8];
    let mut n = 3usize;
    poly[..3].copy_from_slice(v);
    for (bound, keep_above) in [(lo, true), (hi, false)] {
        let mut out: [DVec3; 8] = [DVec3::ZERO; 8];
        let mut m = 0usize;
        let inside = |p: &DVec3| {
            if keep_above { p.z > bound } else { p.z < bound }
        };
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            let ia = inside(&a);
            let ib = inside(&b);
            if ia && m < 8 {
                out[m] = a;
                m += 1;
            }
            if ia != ib && m < 8 {
                let s = (bound - a.z) / (b.z - a.z);
                out[m] = a + (b - a) * s;
                m += 1;
            }
        }
        poly = out;
        n = m;
        if n == 0 {
            return false;
        }
    }
    let c = xy(center);
    let mut pts = [DVec2::ZERO; 8];
    for (dst, src) in pts.iter_mut().zip(&poly[..n]) {
        *dst = xy(*src);
    }
    // Inside test for a non-degenerate convex polygon.
    if n >= 3 {
        let mut area = 0.0;
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            area += a.perp_dot(b);
        }
        if area.abs() > 1.0e-12 {
            let sign = area.signum();
            let all_inside = (0..n).all(|i| {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                (b - a).perp_dot(c - a) * sign >= 0.0
            });
            if all_inside {
                return true;
            }
        }
    }
    let mut dist = f64::INFINITY;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        dist = dist.min(point_segment_distance(c, a, b));
    }
    dist < r
}

/// Sweeps the cylinder `(r, h)` with centre `s + t·d`, `t ∈ [0, 1]`, against
/// the triangle `v`. `outward` makes the triangle one-sided (see the module
/// docs). `tol` is the start-contact tolerance (UU).
#[must_use]
pub fn sweep(
    s: DVec3,
    d: DVec3,
    r: f64,
    h: f64,
    v: &[DVec3; 3],
    outward: Option<DVec3>,
    tol: f64,
) -> Option<Contact> {
    sweep_within(s, d, r, h, v, outward, tol, 1.0)
}

/// [`sweep`] that may skip triangles which cannot be touched before
/// `limit` (a contact found earlier elsewhere): `None` is then returned
/// for contacts later than `limit` too.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn sweep_within(
    s: DVec3,
    d: DVec3,
    r: f64,
    h: f64,
    v: &[DVec3; 3],
    outward: Option<DVec3>,
    tol: f64,
    limit: f64,
) -> Option<Contact> {
    if !(r > 0.0 && h > 0.0 && s.is_finite() && d.is_finite()) {
        return None;
    }
    // Exact early-outs (no contact is lost): the triangle's box against the
    // box swept by the shape, then the triangle's plane against the shape's
    // support along its normal at both ends of the motion.
    let ext = DVec3::new(r, r, h) + DVec3::splat(tol);
    let tmin = v[0].min(v[1]).min(v[2]);
    let tmax = v[0].max(v[1]).max(v[2]);
    let e = s + d;
    if tmax.cmplt(s.min(e) - ext).any() || tmin.cmpgt(s.max(e) + ext).any() {
        return None;
    }
    let face = triangle_normal(v);
    if let Some(n) = face {
        let reach = r * xy(n).length() + h * n.z.abs() + tol;
        let g_start = n.dot(s - v[0]);
        let g_end = n.dot(e - v[0]);
        if (g_start > reach && g_end > reach) || (g_start < -reach && g_end < -reach) {
            return None;
        }
        // Every contact needs the shape to reach the plane first: a lower
        // bound on the contact time.
        let rate = g_end - g_start;
        let t_plane = if g_start > reach {
            (g_start - reach) / -rate
        } else if g_start < -reach {
            (-reach - g_start) / rate
        } else {
            0.0
        };
        if t_plane > limit {
            return None;
        }
    }
    // One-sided triangles: ignore when the centre starts behind the plane.
    if let (Some(out), Some(n)) = (outward, face) {
        let n = if n.dot(out) < 0.0 { -n } else { n };
        if n.dot(s - v[0]) < 0.0 {
            return None;
        }
    }
    // Penetrating start (only possible when the triangle meets the start box).
    let shrink_r = r - tol;
    let shrink_h = h - tol;
    let near_start = !(tmax.cmplt(s - ext).any() || tmin.cmpgt(s + ext).any());
    if near_start && shrink_r > 0.0 && shrink_h > 0.0 && overlaps(s, shrink_r, shrink_h, v) {
        let n = face?;
        let n = match outward {
            Some(out) => {
                if n.dot(out) < 0.0 {
                    -n
                } else {
                    n
                }
            }
            None => {
                let side = n.dot(s - v[0]);
                if side < 0.0 || (side == 0.0 && n.dot(d) > 0.0) {
                    -n
                } else {
                    n
                }
            }
        };
        return (d.dot(n) < 0.0).then_some(Contact {
            t: 0.0,
            normal: n,
            penetrating: true,
        });
    }

    let scale = s.abs().max_element().max(v[0].abs().max_element()) + r + h + d.length();
    let eps = slack(scale);
    let mut acc = Acc::default();

    // Face against the cylinder's support point.
    if let Some(n0) = face {
        let n = match outward {
            Some(out) => {
                if n0.dot(out) < 0.0 {
                    -n0
                } else {
                    n0
                }
            }
            None => {
                let side = n0.dot(s - v[0]);
                if side < 0.0 || (side == 0.0 && n0.dot(d) > 0.0) {
                    -n0
                } else {
                    n0
                }
            }
        };
        let nxy = xy(n).length();
        let extent = r * nxy + h * n.z.abs();
        let g0 = n.dot(s - v[0]) - extent;
        if let Some(t) = linear_entry(g0, n.dot(d), tol)
            && t <= 1.0
        {
            let c = s + d * t;
            let mut p = c;
            if nxy > FLAT_EPS {
                p.x -= r * n.x / nxy;
                p.y -= r * n.y / nxy;
            }
            if n.z.abs() > FLAT_EPS {
                p.z -= h * n.z.signum();
            }
            let on_plane = p - n * n.dot(p - v[0]);
            // The edge test needs the winding normal (inward = n0 × edge).
            if inside_triangle(on_plane, v, n0, eps + BARY_EPS * scale) {
                acc.consider(t, n);
            }
        }
    }

    // Vertices against the lateral surface and the caps. A contact on the
    // boundary between the side and a cap only counts when the motion
    // carries the feature into the open interior (otherwise the shape slides
    // past it, e.g. along a floor's edge in the plane of its bottom cap).
    // Exact culls: a vertex or edge can only be touched when it comes within
    // `r` (2-D) of the axis path and within the swept box.
    let reach2 = (r + tol) * (r + tol);
    let (sx, ex) = (xy(s), xy(e));
    let box_lo = s.min(e) - ext;
    let box_hi = s.max(e) + ext;
    for &p in v {
        let near = {
            let seg = ex - sx;
            let l2 = seg.dot(seg);
            let u = if l2 > 0.0 {
                ((xy(p) - sx).dot(seg) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let q = sx + seg * u;
            (xy(p) - q).length_squared() <= reach2
        };
        if !near || p.cmplt(box_lo).any() || p.cmpgt(box_hi).any() {
            continue;
        }
        if let Some(t) = circle_entry(xy(s - p), xy(d), r, tol)
            && t <= 1.0
        {
            let c = s + d * t;
            let z_rel = p.z - c.z;
            let ok = match zone(z_rel.abs(), h, tol, eps) {
                Zone::Interior => true,
                Zone::Boundary => z_rel * d.z > 0.0,
                Zone::Outside => false,
            };
            if ok {
                acc.consider(t, radial_normal(xy(c - p), d));
            }
        }
        // Bottom cap descending onto the vertex / top cap rising into it.
        let caps = [
            ((s.z - h) - p.z, d.z, DVec3::Z),
            (p.z - (s.z + h), -d.z, DVec3::NEG_Z),
        ];
        for (g0, k, n) in caps {
            if let Some(t) = linear_entry(g0, k, tol)
                && t <= 1.0
            {
                let c = s + d * t;
                let rel = xy(p - c);
                let ok = match zone(rel.length(), r, tol, eps) {
                    Zone::Interior => true,
                    Zone::Boundary => rel.dot(xy(d)) > 0.0,
                    Zone::Outside => false,
                };
                if ok {
                    acc.consider(t, n);
                }
            }
        }
    }

    // Edges.
    for i in 0..3 {
        let a = v[i];
        let b = v[(i + 1) % 3];
        let ab = b - a;
        let ab_xy = xy(ab);
        let l2 = ab_xy.dot(ab_xy);
        let len3 = ab.length();
        if len3 <= 0.0
            || a.min(b).cmpgt(box_hi).any()
            || a.max(b).cmplt(box_lo).any()
            || segment_segment_distance2(xy(a), xy(b), sx, ex) > reach2
        {
            continue;
        }
        let horizontal = ab.z.abs() <= 1.0e-9 * (1.0 + len3);
        // Lateral surface against the edge.
        if l2 > (1.0e-9 * len3) * (1.0e-9 * len3) {
            let ul = l2.sqrt();
            let perp = DVec2::new(-ab_xy.y, ab_xy.x) / ul;
            let delta0 = (xy(s) - xy(a)).dot(perp);
            let delta_v = xy(d).dot(perp);
            let side = if delta0 != 0.0 {
                delta0.signum()
            } else {
                -delta_v.signum()
            };
            let g0 = delta0.abs() - r;
            if let Some(t) = linear_entry(g0, side * delta_v, tol)
                && t <= 1.0
            {
                let c = s + d * t;
                let lambda = (xy(c) - xy(a)).dot(ab_xy) / l2;
                let lam_eps = (eps + BARY_EPS * scale) / ul;
                if (-lam_eps..=1.0 + lam_eps).contains(&lambda) {
                    let q = a + ab * lambda.clamp(0.0, 1.0);
                    let z_rel = q.z - c.z;
                    // Boundary contacts of slanted edges are the rim test's.
                    let ok = match zone(z_rel.abs(), h, tol, eps) {
                        Zone::Interior => true,
                        Zone::Boundary => horizontal && z_rel * d.z > 0.0,
                        Zone::Outside => false,
                    };
                    if ok {
                        let n = perp * side;
                        acc.consider(t, DVec3::new(n.x, n.y, 0.0));
                    }
                }
            }
        } else if let Some(t) = circle_entry(xy(s - a), xy(d), r, tol)
            && t <= 1.0
        {
            // Vertical edge: a point in 2-D with a height interval.
            let c = s + d * t;
            let (z0, z1) = if a.z <= b.z { (a.z, b.z) } else { (b.z, a.z) };
            let lo = z0.max(c.z - h);
            let hi = z1.min(c.z + h);
            let ok = if hi - lo > tol {
                true
            } else if hi - lo >= -eps {
                // Touching the slab only at one end: count it when the
                // motion moves that end into the slab.
                if z1 - (c.z - h) <= tol {
                    d.z < 0.0
                } else {
                    d.z > 0.0
                }
            } else {
                false
            };
            if ok {
                acc.consider(t, radial_normal(xy(c - a), d));
            }
        }

        if !horizontal {
            // Rim circles against the edge.
            for sigma in [-1.0f64, 1.0] {
                let lambda0 = (s.z + sigma * h - a.z) / ab.z;
                let lambda_v = d.z / ab.z;
                let w0 = xy(a) + ab_xy * lambda0 - xy(s);
                let wv = ab_xy * lambda_v - xy(d);
                if let Some(t) = circle_entry(w0, wv, r, tol)
                    && t <= 1.0
                {
                    let lambda = lambda0 + lambda_v * t;
                    let lam_eps = (eps + BARY_EPS * scale) / len3;
                    if (-lam_eps..=1.0 + lam_eps).contains(&lambda) {
                        let w = w0 + wv * t;
                        let wl = w.length();
                        if wl > 0.0 {
                            let rho = w / wl;
                            let tangent = DVec3::new(-rho.y, rho.x, 0.0);
                            let mut m = ab.cross(tangent);
                            let ml = m.length();
                            let n = if ml > 1.0e-12 * len3 {
                                m /= ml;
                                let cone = DVec3::new(rho.x, rho.y, sigma);
                                if m.dot(cone) < 0.0 {
                                    m = -m;
                                }
                                -m
                            } else {
                                -DVec3::new(rho.x, rho.y, sigma).normalize()
                            };
                            acc.consider(t, n);
                        }
                    }
                }
            }
        } else {
            // Cap disks against a horizontal edge.
            let ze = (a.z + b.z) * 0.5;
            let caps = [
                ((s.z - h) - ze, d.z, DVec3::Z),
                (ze - (s.z + h), -d.z, DVec3::NEG_Z),
            ];
            for (g0, k, n) in caps {
                if let Some(t) = linear_entry(g0, k, tol)
                    && t <= 1.0
                {
                    let c = s + d * t;
                    let (dist, closest) = segment_closest(xy(c), xy(a), xy(b));
                    let ok = match zone(dist, r, tol, eps) {
                        Zone::Interior => true,
                        Zone::Boundary => (closest - xy(c)).dot(xy(d)) > 0.0,
                        Zone::Outside => false,
                    };
                    if ok {
                        acc.consider(t, n);
                    }
                }
            }
        }
    }

    acc.best.map(|b| Contact {
        t: b.t,
        normal: b.normal,
        penetrating: false,
    })
}

/// Ray (segment) `o + t·dv`, `t ∈ [0, 1]`, against the triangle
/// (Möller–Trumbore in `f64`, both faces unless `outward` is given, in which
/// case only rays entering through the outward face hit). Returns `t` and
/// the face normal oriented against the ray.
#[must_use]
pub fn ray(o: DVec3, dv: DVec3, v: &[DVec3; 3], outward: Option<DVec3>) -> Option<(f64, DVec3)> {
    let e1 = v[1] - v[0];
    let e2 = v[2] - v[0];
    let p = dv.cross(e2);
    let det = e1.dot(p);
    let scale = e1.length() * e2.length() * dv.length();
    if !det.is_finite() || det.abs() <= 1.0e-14 * scale || scale.is_nan() {
        return None;
    }
    let inv = 1.0 / det;
    let tv = o - v[0];
    let u = tv.dot(p) * inv;
    if !(-BARY_EPS..=1.0 + BARY_EPS).contains(&u) {
        return None;
    }
    let q = tv.cross(e1);
    let w = dv.dot(q) * inv;
    if w < -BARY_EPS || u + w > 1.0 + BARY_EPS {
        return None;
    }
    let t = e2.dot(q) * inv;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    let mut n = triangle_normal(v)?;
    if let Some(out) = outward {
        let n_out = if n.dot(out) < 0.0 { -n } else { n };
        if dv.dot(n_out) >= 0.0 {
            return None;
        }
    }
    if n.dot(dv) > 0.0 {
        n = -n;
    }
    Some((t, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f64 = 21.0;
    const H: f64 = 44.0;
    const TOL: f64 = 1.0e-3;

    fn floor() -> [DVec3; 3] {
        [
            DVec3::new(-1000.0, -1000.0, 0.0),
            DVec3::new(1000.0, -1000.0, 0.0),
            DVec3::new(0.0, 1000.0, 0.0),
        ]
    }

    #[test]
    fn falling_onto_a_floor_lands_on_the_cap() {
        let c = sweep(
            DVec3::new(0.0, 0.0, 100.0),
            DVec3::new(0.0, 0.0, -200.0),
            R,
            H,
            &floor(),
            None,
            TOL,
        )
        .unwrap();
        assert!((c.t - (56.0 / 200.0)).abs() < 1e-12, "{c:?}");
        assert_eq!(c.normal, DVec3::Z);
        assert!(!c.penetrating);
        // From below the floor (two-sided): hits the top cap from below.
        let c = sweep(
            DVec3::new(0.0, 0.0, -100.0),
            DVec3::new(0.0, 0.0, 200.0),
            R,
            H,
            &floor(),
            None,
            TOL,
        )
        .unwrap();
        assert!((c.t - (56.0 / 200.0)).abs() < 1e-12);
        assert_eq!(c.normal, DVec3::NEG_Z);
        // One-sided with the outward normal up: ignored from below.
        assert!(
            sweep(
                DVec3::new(0.0, 0.0, -100.0),
                DVec3::new(0.0, 0.0, 200.0),
                R,
                H,
                &floor(),
                Some(DVec3::Z),
                TOL
            )
            .is_none()
        );
    }

    #[test]
    fn walking_into_a_wall_touches_with_the_side() {
        // Vertical wall in the plane x = 100 facing -x.
        let wall = [
            DVec3::new(100.0, -500.0, -500.0),
            DVec3::new(100.0, 500.0, -500.0),
            DVec3::new(100.0, 0.0, 500.0),
        ];
        let c = sweep(
            DVec3::ZERO,
            DVec3::new(200.0, 0.0, 0.0),
            R,
            H,
            &wall,
            None,
            TOL,
        )
        .unwrap();
        assert!((c.t - (79.0 / 200.0)).abs() < 1e-12, "{c:?}");
        assert!((c.normal - DVec3::NEG_X).length() < 1e-12);
    }

    #[test]
    fn rim_contact_on_a_slanted_edge() {
        // A thin sliver whose top edge is a slanted line; the cylinder
        // descends next to it so the bottom rim touches the edge.
        let tri = [
            DVec3::new(-100.0, 30.0, 0.0),
            DVec3::new(100.0, 30.0, 40.0),
            DVec3::new(0.0, 30.0, -300.0),
        ];
        let c = sweep(
            DVec3::new(0.0, 20.0, 200.0),
            DVec3::new(0.0, 0.0, -400.0),
            R,
            H,
            &tri,
            None,
            TOL,
        );
        let c = c.expect("contact");
        // The bottom rim circle (radius 21 around (0, 20)) meets the plane
        // y = 30 at x = ±sqrt(21² − 10²); the edge z at x is 20 + 0.2·x, so the
        // highest crossing is at x = +18.47: z ≈ 23.69.
        let x = (21.0f64 * 21.0 - 100.0).sqrt();
        let z_edge = 20.0 + 0.2 * x;
        let expected_t = (200.0 - H - z_edge) / 400.0;
        assert!((c.t - expected_t).abs() < 1e-9, "{c:?} vs {expected_t}");
        assert!(c.normal.z > 0.0 && c.normal.y < 0.0, "{c:?}");
    }

    #[test]
    fn touching_and_penetrating_starts() {
        let f = floor();
        // Resting exactly on the floor: moving down is blocked at t = 0,
        // sideways and up are free.
        let s = DVec3::new(0.0, 0.0, H);
        let c = sweep(s, DVec3::new(0.0, 0.0, -5.0), R, H, &f, None, TOL).unwrap();
        assert_eq!(c.t, 0.0);
        assert!(!c.penetrating);
        assert!(sweep(s, DVec3::new(50.0, 0.0, 0.0), R, H, &f, None, TOL).is_none());
        assert!(sweep(s, DVec3::new(0.0, 0.0, 5.0), R, H, &f, None, TOL).is_none());
        // 1 UU inside: penetrating, blocks only inward motion.
        let s = DVec3::new(0.0, 0.0, H - 1.0);
        let c = sweep(s, DVec3::new(10.0, 0.0, -5.0), R, H, &f, None, TOL).unwrap();
        assert!(c.penetrating && c.t == 0.0 && c.normal == DVec3::Z);
        assert!(sweep(s, DVec3::new(10.0, 0.0, 0.0), R, H, &f, None, TOL).is_none());
        assert!(sweep(s, DVec3::new(0.0, 0.0, 3.0), R, H, &f, None, TOL).is_none());
    }

    #[test]
    fn static_overlap_matches_simple_cases() {
        let f = floor();
        assert!(overlaps(DVec3::new(0.0, 0.0, 43.0), R, H, &f));
        assert!(!overlaps(DVec3::new(0.0, 0.0, 44.0), R, H, &f));
        assert!(!overlaps(DVec3::new(0.0, 0.0, 45.0), R, H, &f));
        // Horizontally just outside the triangle's slanted edge.
        let small = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(100.0, 0.0, 0.0),
            DVec3::new(0.0, 100.0, 0.0),
        ];
        assert!(overlaps(DVec3::new(-20.0, 50.0, 0.0), R, H, &small));
        assert!(!overlaps(DVec3::new(-22.0, 50.0, 0.0), R, H, &small));
    }

    #[test]
    fn rays_hit_both_faces_unless_one_sided() {
        let f = floor();
        let (t, n) = ray(
            DVec3::new(0.0, 0.0, 10.0),
            DVec3::new(0.0, 0.0, -20.0),
            &f,
            None,
        )
        .unwrap();
        assert!((t - 0.5).abs() < 1e-12);
        assert_eq!(n, DVec3::Z);
        let (_, n) = ray(
            DVec3::new(0.0, 0.0, -10.0),
            DVec3::new(0.0, 0.0, 20.0),
            &f,
            None,
        )
        .unwrap();
        assert_eq!(n, DVec3::NEG_Z);
        assert!(
            ray(
                DVec3::new(0.0, 0.0, -10.0),
                DVec3::new(0.0, 0.0, 20.0),
                &f,
                Some(DVec3::Z)
            )
            .is_none()
        );
        assert!(
            ray(
                DVec3::new(0.0, 0.0, 10.0),
                DVec3::new(5.0, 0.0, 0.0),
                &f,
                None
            )
            .is_none()
        );
        // Degenerate triangle.
        let line = [DVec3::ZERO, DVec3::X, DVec3::X * 2.0];
        assert!(
            ray(
                DVec3::new(0.5, 0.0, 1.0),
                DVec3::new(0.0, 0.0, -2.0),
                &line,
                None
            )
            .is_none()
        );
        assert!(triangle_normal(&line).is_none());
    }
}
