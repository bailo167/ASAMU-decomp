//! Swept axis-aligned box against triangles and convex hulls.
//!
//! The original sweeps the player's pawn as an axis-aligned box whose half
//! extents are the collision cylinder's radius and half height, `(21, 21,
//! 44)`, not as a cylinder (`docs/PARITY_FINDINGS.md` P4: CONFIRMED on
//! floors, STRONG on walls). This module is that shape's exact test against
//! one triangle and against one convex hull, in `f64`.
//!
//! # Method: separating axes over the motion
//!
//! Two convex bodies are disjoint exactly when their projections on some
//! axis are disjoint, and for a box against a triangle or a convex
//! polyhedron a finite list of axes decides it: the face directions of both
//! and the cross products of one edge direction of each. Along a straight
//! motion each axis yields the time the projections start to overlap (its
//! *entry*) and the time they stop (its *exit*). The bodies first touch at
//! the latest entry, provided that is not later than the earliest exit, and
//! the axis that entered last is the direction of the contact plane. No
//! contact time is approximated: the reported time is the exact first time
//! of contact (the callers apply the original's pull-back, P3).
//!
//! # The original's rule for the contact normal
//!
//! Read from the user's own macOS executable (function addresses are that
//! file's; our reading of each routine was then checked by running the
//! routine's instructions in a local interpreter on random inputs and
//! comparing every result with the rule as written here). That interpreter
//! was not kept, so the case counts below cannot be regenerated. A second,
//! independent pass read the same routines statically (the order of the
//! axes from the operands of each test, the comparisons that update the
//! latest entry and the earliest exit, the selection of the result, the
//! constants from the file's bytes) and agrees with every rule stated here
//! for a sweep that starts clear; it found the scale dependence described
//! under "Convex hulls". Regenerate with `objdump -d` over 0x1007889D0 to
//! 0x10078C2A0, 0x100AA4840 to 0x100AA6710, 0x100AAD350 to 0x100AAD4B0 and
//! 0x100AB3D60 to 0x100AB3F80.
//!
//! **Triangles** (the per-triangle path of static meshes: the box check of
//! the kDOP tree at 0x100C5B650 calls the triangle routine at 0x1007889D0).
//! CONFIRMED (6,000 emulated cases, 0 differences):
//!
//! - 13 axes are tested in this order: the box's three face axes (Z, X, Y
//!   for a world-aligned box), the triangle's normal, then the box's X edge
//!   crossed with the triangle's three edges, its Y edge with the three, its
//!   Z edge with the three. Edges are `V1 − V0`, `V2 − V1`, `V0 − V2`,
//!   normalised first. An axis whose squared length is below 1e-8 (the
//!   constant at 0x101645800: parallel edges) is skipped.
//! - Each axis updates a running latest entry only when its entry is
//!   strictly later, so on equal entries the earlier axis in the list keeps
//!   the normal. The normal is that axis, signed against the motion.
//! - Consequences. Box face on a triangle vertex or edge: a world axis. Box
//!   vertex or edge lying flat on the triangle's face: the triangle's
//!   normal. Box edge against triangle edge: the unit vector perpendicular
//!   to both edges. Corner cases (a box vertex against a triangle edge or
//!   vertex) report whichever of the 13 axes entered last; there is no
//!   "radial" normal from a vertex as a cylinder or sphere would give.
//! - The routine returns the axis unnormalised; the static-mesh component
//!   (0x100C58790) maps it to world space with the inverse transpose of the
//!   mesh's transform, flips it for mirrored meshes and normalises it. The
//!   test itself runs in the mesh's space with the box's axes carried into
//!   that space, which gives the same axes as testing in world space.
//! - Those carried axes are not normalised (the check's constructor at
//!   0x100C5B2A0 stores them as the inverse matrix gives them and the box
//!   check hands them over as stored: CONFIRMED, read statically, second
//!   pass), so they are as long as one over the mesh's scale and the 1e-8
//!   test above is applied to vectors of that length. For a mesh scaled
//!   uniformly by `s` the box's own face axes have the squared length
//!   `1 / s⁴` and are skipped for `s` above 100, and an edge axis is
//!   skipped when the edge is within `asin(1e-4·s)` of the box's edge
//!   (0.57° at scale 100). The consequence is the one described for convex
//!   hulls below, at scales ten times larger. For a scale that is not
//!   uniform, a face axis goes when the scales on the other two axes
//!   multiply to more than 10,000. In the user's data 20 pawn-blocking
//!   placements of a mesh that is met at its triangles are that large (see
//!   "Ours").
//!
//! **Convex hulls** (the simple-collision elements of a mesh: the convex
//! element's check at 0x100AA5A70 with the per-axis test at 0x100AAD350 and
//! 0x100AB3D60, called per element by the aggregate check at 0x100AA4840).
//! CONFIRMED (1,500 emulated cases, 0 differences):
//!
//! - The same rule with this axis list: the box's three face axes (Z, X, Y),
//!   every face-normal direction the element stores, then for every edge
//!   direction the element stores its cross products with the box's X, Y and
//!   Z edges. The hull's projection on an axis is the range of its vertices.
//! - An axis is skipped below a squared length of 1e-4 (0x1016393A0), and
//!   that length is not a unit vector's. The test runs in the element's own
//!   space: the aggregate check multiplies the body's scale into its matrix
//!   and inverts it, and the box's three edge directions are the rows of
//!   that inverse, as long as one over the scale. The box's face axes are
//!   cross products of two rows and the edge axes are a stored (unit) edge
//!   direction crossed with one row, each handed to the per-axis test as
//!   computed. CONFIRMED (read statically, second pass). So for a body
//!   scaled uniformly by `s` the box's own three face axes have the squared
//!   length `1 / s⁴` and are **all skipped for `s` above 10**, and an edge
//!   axis is skipped when the edge is within `asin(0.01·s)` of the world
//!   axis (0.57° at scale 1; every edge axis above scale 100) instead of
//!   only when parallel. A skipped axis cannot separate, so the original's
//!   solid is then larger than the hull: it reports edge and corner
//!   contacts earlier. STRONG for that consequence (it follows from the
//!   code; no recording has been compared with it yet). For a scale that is
//!   not uniform, a face axis goes when the scales on the other two axes
//!   multiply to more than 100. In the user's data 2,162 of the 12,846
//!   pawn-blocking placements of a mesh that is met at its simple body
//!   lose at least one face axis that way (31,944 of the 89,412 shapes
//!   placed for them; counted by `scene_simple_real_data`), and no volume
//!   does.
//! - The aggregate check takes an element only when its time is strictly
//!   earlier than the best so far (convex elements first, then spheres,
//!   boxes, sphyls), and maps the normal to world space as above.
//!
//! **Box elements** (0x100AA69A0, with a 15-axis routine at 0x100AAC780):
//! the same family of test (three axes of one box, then of the other, then
//! nine cross products). STRONG for the family; the order of its axes and
//! its start rules were not read: TENTATIVE. The user's data has no box
//! element (see [`super`]), so nothing depends on it yet.
//!
//! **BSP** (0x10092C440): per convex leaf hull, the line is clipped against
//! the hull's planes pushed out by the box, then against the six
//! axis-aligned planes of the hull's bounds, then against bevel planes
//! built from pairs of hull planes and the world axes; the normal is the
//! plane that was entered last. That is the same construction with the
//! hull's faces tested before the world axes. STRONG (structure read, not
//! emulated). Our BSP is a triangle soup, so it goes through the triangle
//! test here: contact times agree for a box that starts outside the solid;
//! on an exact tie between a face and a world axis the two orders would
//! differ only when the directions differ, which needs an edge or corner
//! contact. TENTATIVE that no visible difference follows; a recording of a
//! walk into the outer corner of a BSP wall at an angle would settle it.
//!
//! # Contacts at the start: the original's rules and ours
//!
//! The original has no tolerance: it relies on every reported hit having
//! been pulled back (P3), so a pawn is normally 0.1 uu or more from
//! everything. For the record, both routines above treat a sweep that
//! starts in contact as follows (CONFIRMED, same evidence):
//!
//! - Overlapping on every axis at the start (latest entry negative): a hit
//!   at time 0 whose normal is the reversed direction of motion, except
//!   that there is no hit at all when the time already spent inside exceeds
//!   the time left until the earliest exit and the motion follows that
//!   exit's axis. The convex routine also raises the result's
//!   "started penetrating" flag.
//! - "Close and parallel": when the projected motion on an axis is below a
//!   threshold and the start lies outside the obstacle's range on that axis
//!   by less than the same threshold (1e-4 for triangles, the first such
//!   axis counting; 0.01 for convex elements, the last such axis counting),
//!   the result is a hit at time 0 with the normal `axis − 0.05 × direction`,
//!   normalised, if the sweep would otherwise have hit within its length or
//!   its latest-entry axis is that same axis. A per-body switch turns this
//!   off for convex elements.
//!
//! # The test before a convex element is swept
//!
//! The aggregate check (0x100AA4840) does not hand every trace to every
//! convex element. For a trace with an extent it first compares bounds, in
//! the element's own space (0x100AA4B22 to 0x100AA4E64, CONFIRMED by reading
//! it): the box `[-extent, extent]` is carried through the inverse matrix and
//! half the size of its bounds there is the box's half size in that space;
//! the smaller and the larger of the sweep's start and end on each axis,
//! widened by that half size plus 0.1 (the float at 0x1016457B8), are the
//! sweep's bounds; an element whose stored bounds (its vertices' box) lie
//! entirely beyond them on any axis is skipped, and touching counts. A
//! zero-extent trace goes to every element without this test.
//!
//! With every axis tested this changes nothing (a contact implies that the
//! bounds meet). With axes left out it is what confines the contact: the
//! box is stopped by an element's extended face planes only where the
//! sweep's bounds reach the element's bounds. [`convex_precheck`] is that
//! test.
//!
//! The matrix the check inverts is the placement's own: the component hands
//! the aggregate check a matrix with its rows divided by the component's
//! total scale, and that scale as a vector, and the aggregate check
//! multiplies the two again (`UPrimitiveComponent::GetTransformAndScale` at
//! 0x100533640, called by the brush component's check at 0x100759CFF;
//! CONFIRMED for brushes, STRONG for static meshes, whose call is through
//! the component's table). A component whose scale is below 1e-4 on all
//! three axes gets the identity instead.
//!
//! # Ours
//!
//! [`sweep_triangle`] and [`sweep_hull`] are the exact geometry: the
//! original's axes in its order with its tie rule, in world space with unit
//! vectors, where only an edge axis below 1e-8 (a parallel pair) is skipped.
//! For triangles that is the original's test at scale 1; for a convex body,
//! at scale 1 with every edge parallel to a world axis or more than 0.57°
//! from it.
//!
//! [`sweep_triangle_skipping`] and [`sweep_hull_skipping`] take an
//! [`AxisSkip`], the original's rule for leaving axes out, and
//! [`convex_precheck`] is the test above. The scene applies them to every
//! instance under `NativeRule` ([`super`]), with the box's edges carried
//! into the element's space by the instance's own inverse transform (for a
//! volume, whose hulls are stored in world space, by the inverse of its
//! component's transform). What is reproduced and what is not:
//!
//! - **Convex elements: the rule as read.** Face axes of the box, the
//!   stored face directions, the stored edge directions crossed with the
//!   box's edges, each left out by the 1e-4 threshold on the unnormalised
//!   vector, and the bounds test before. Evaluated in `f64` in world space
//!   (an axis in the element's space and its image in world space separate
//!   the same pairs), where the original works in `f32` in the element's
//!   space: a vector whose squared length is within rounding of 1e-4 can
//!   fall on the other side. One more test is ours: a solid that loses a
//!   face axis of the box is swept only when the sweep itself (not only its
//!   bounds) passes the solid's widened bounds, which is what lets the
//!   search for candidates stay a search in a hierarchy. The two differ for
//!   a long sweep that runs past such a solid at an angle in the solid's
//!   space, far enough for its bounds to reach where the sweep does not: the
//!   original could then be stopped by the solid's extended face planes away
//!   from the solid, and ours is not. A pawn's moves are a few units long.
//! - **Triangles: the edge axes as read, the face axes kept.** An edge axis
//!   is left out by the 1e-8 threshold on the unnormalised vector (within
//!   `asin(0.0001 × scale)` of a box edge). The box's own face axes are
//!   always tested, although the original leaves one out when the mesh's
//!   scales on the other two axes multiply to more than 10,000: what then
//!   confines a contact in the original is its collision tree, whose node
//!   boxes are not in the converted data (MESHES.md, "Collision": the node
//!   bytes are not decoded). In the user's data 20 pawn-blocking placements
//!   of a mesh that is met at its triangles are that large (13 in
//!   AG-ParadiseCave, 7 in AG-BeautifulCity; counted by
//!   `scene_simple_real_data`). For those ours is the exact contact where
//!   the original's can come earlier at corners. UNKNOWN by how much.
//!   Under the rule a triangle is tested when the sweep passes the
//!   triangle's own bounds in the mesh's space; with the exact test that
//!   is implied.
//! - **Box elements:** exact (their own routine's axis order was not read),
//!   never under the rule.
//! - **BSP:** exact, through the triangle test.
//!
//! The consequence of the rule for a convex solid placed at a scale above
//! 10 (two scales multiplying to more than 100): the box's own face axes do
//! not separate, so where an edge or a corner of the solid would have met a
//! face of the box, the box is stopped earlier, by the planes of the
//! solid's faces pushed out by the box, as far as the bounds test reaches
//! (the solid's bounds in its own space widened by the box and by 0.1 of
//! its own units, that is 0.1 × scale in the world). Above scale 100 no edge
//! axis is left either. STRONG: it follows from the code; no recording has
//! been compared with it yet. A walk off the edge of a large platform whose
//! sides lean inwards would show it: the pawn should still be carried a
//! short way past the rim.
//!
//! For a sweep that starts in contact ours keeps the convention of the rest
//! of this collision code instead ([`super::cylinder`]): an axis on which
//! the start is separated, touching
//! or overlapping by no more than the tolerance `tol` counts as separated
//! at the start. If the motion does not close that axis there is no
//! contact (a resting box can always slide or leave); if it does, the
//! contact is at its exact entry, clamped to 0. A start that overlaps by
//! more than `tol` on every axis is a **penetrating** start: reported at
//! time 0 with the axis of least overlap as the normal, and only when the
//! motion goes into that normal. The two conventions agree whenever the
//! start is clear of the obstacle by more than the tolerances, which the
//! pull-back guarantees in normal movement; the original's start rules
//! depend on single-precision rounding in each mesh's own space and are not
//! reproduced. TENTATIVE that nothing observable depends on the difference:
//! a recording of a pawn pushed into geometry (a mover closing on it) would
//! show it.
//!
//! An exact graze (latest entry equal to earliest exit) counts as a contact
//! in the original and here.

use glam::DVec3;

pub use super::cylinder::Contact;
use super::cylinder::triangle_normal;

/// Squared length below which the cross product of two unit directions is
/// no axis (the directions are parallel). The original's value for
/// triangles (the double at 0x101645800 in the user's macOS executable).
/// Used for hulls as well: the original's convex test compares with 1e-4,
/// but vectors that are not unit (see the module docs, "Convex hulls").
const PARALLEL_EPS2: f64 = 1.0e-8;

/// Squared length below which the original's convex test leaves an axis out
/// (the double at 0x1016393A0 in the user's macOS executable).
pub const CONVEX_MIN_AXIS_LENGTH2: f64 = 1.0e-4;
/// The same for the original's triangle test (the double at 0x101645800).
pub const TRIANGLE_MIN_AXIS_LENGTH2: f64 = 1.0e-8;
/// What the original's aggregate check adds to the swept box, in the units
/// of the element's own space, before it compares that box with a convex
/// element's bounds (the float at 0x1016457B8; see [`convex_precheck`]).
pub const CONVEX_PRECHECK_PAD: f64 = 0.1;

/// The original's rule for leaving separating axes out (module docs,
/// "Triangles" and "Convex hulls"): it tests in the space of what is hit,
/// with the box's edge directions carried into that space and **not**
/// normalised, and skips an axis whose squared length is under a fixed
/// threshold. So which axes take part depends on the placement's scale.
///
/// [`sweep_triangle`] and [`sweep_hull`] know nothing of this (they test
/// every axis that is not degenerate, which is the exact geometry);
/// [`sweep_triangle_skipping`] and [`sweep_hull_skipping`] apply it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSkip {
    /// The box's three edge directions in the space the original tests in:
    /// the images of the world X, Y and Z axes under the world → element
    /// transform (the rows of the inverse of the placement's matrix), as
    /// long as one over the scale.
    pub box_edges: [DVec3; 3],
    /// Squared length under which an axis is left out.
    pub min_length2: f64,
    /// Whether the box's own three face axes are subject to the rule
    /// (`false`: they are always tested).
    pub face_axes: bool,
}

impl AxisSkip {
    /// The rule of the original's convex-element check
    /// ([`CONVEX_MIN_AXIS_LENGTH2`], face axes included).
    #[must_use]
    pub fn convex(box_edges: [DVec3; 3]) -> Self {
        Self {
            box_edges,
            min_length2: CONVEX_MIN_AXIS_LENGTH2,
            face_axes: true,
        }
    }

    /// The edge-axis part of the rule of the original's triangle check
    /// ([`TRIANGLE_MIN_AXIS_LENGTH2`]). The box's face axes stay: the
    /// original skips them too (for a mesh whose scales on the other two
    /// axes multiply to more than 10,000), but what then bounds a contact is
    /// its collision tree's node boxes, which the converted data does not
    /// have (module docs, "Ours").
    #[must_use]
    pub fn triangles(box_edges: [DVec3; 3]) -> Self {
        Self {
            box_edges,
            min_length2: TRIANGLE_MIN_AXIS_LENGTH2,
            face_axes: false,
        }
    }

    /// `true` when the box's face axis `axis` (0 = X, 1 = Y, 2 = Z) is left
    /// out: the original forms it as the cross product of the other two
    /// carried edges.
    #[must_use]
    pub fn skips_face(&self, axis: usize) -> bool {
        if !self.face_axes {
            return false;
        }
        let j = (axis + 1) % 3;
        let k = (axis + 2) % 3;
        match (self.box_edges.get(j), self.box_edges.get(k)) {
            (Some(a), Some(b)) => self.too_short(a.cross(*b)),
            _ => true,
        }
    }

    /// `true` when the axis "edge direction × box edge `box_edge`" is left
    /// out. `edge` is the edge's direction in world space (any length): the
    /// original crosses the unit direction in the element's space with the
    /// carried box edge.
    #[must_use]
    pub fn skips_edge(&self, edge: DVec3, box_edge: usize) -> bool {
        let [x, y, z] = self.box_edges;
        let local = x * edge.x + y * edge.y + z * edge.z;
        let (Some(unit), Some(b)) = (local.try_normalize(), self.box_edges.get(box_edge)) else {
            return true;
        };
        self.too_short(unit.cross(*b))
    }

    /// `true` when the axis `v` is left out: its squared length is under the
    /// threshold (a vector that is not a number counts as left out).
    fn too_short(&self, v: DVec3) -> bool {
        let l2 = v.length_squared();
        l2.is_nan() || l2 < self.min_length2
    }
}

/// The original's test before a convex element is swept at all
/// (`FKAggregateGeom::LineCheck`, 0x100AA4CDC to 0x100AA4E64 in the user's
/// macOS executable, CONFIRMED by reading it): in the element's own space,
/// the bounds of the whole sweep (the smaller and the larger of the start and
/// the end on every axis, widened by the box's half sizes in that space plus
/// [`CONVEX_PRECHECK_PAD`]) must reach the element's bounds on all three
/// axes; touching counts. The box's half sizes in that space are half the
/// bounds of the box `[-extent, extent]` carried through the inverse matrix.
///
/// `start` and `end` are the sweep's ends, `half` those half sizes, and
/// `[elem_min, elem_max]` the element's bounds, all in the element's space.
/// A value that is not finite anywhere answers `false`.
#[must_use]
pub fn convex_precheck(
    start: DVec3,
    end: DVec3,
    half: DVec3,
    elem_min: DVec3,
    elem_max: DVec3,
) -> bool {
    if !(start.is_finite()
        && end.is_finite()
        && half.is_finite()
        && elem_min.is_finite()
        && elem_max.is_finite())
    {
        return false;
    }
    let pad = half + DVec3::splat(CONVEX_PRECHECK_PAD);
    let lo = start.min(end) - pad;
    let hi = start.max(end) + pad;
    elem_min.cmple(hi).all() && lo.cmple(elem_max).all()
}

/// Running state of one sweep against one convex obstacle.
struct Sat {
    /// Half extents of the box.
    e: DVec3,
    /// Motion.
    d: DVec3,
    tol: f64,
    limit: f64,
    /// Latest entry over the axes that are separated at the start.
    entry: f64,
    normal: DVec3,
    separated: bool,
    /// Earliest exit over all axes.
    exit: f64,
    /// Least overlap (largest negative separation) over the axes that
    /// overlap at the start, with its axis towards the box.
    depth: f64,
    depth_normal: DVec3,
}

impl Sat {
    fn new(e: DVec3, d: DVec3, tol: f64, limit: f64) -> Self {
        Self {
            e,
            d,
            tol,
            limit,
            entry: f64::NEG_INFINITY,
            normal: DVec3::ZERO,
            separated: false,
            exit: f64::INFINITY,
            depth: f64::NEG_INFINITY,
            depth_normal: DVec3::ZERO,
        }
    }

    /// Tests the unit axis `l`; `[lo, hi]` is the obstacle's projection on
    /// it relative to the box centre at the start. Returns `false` when no
    /// contact is possible any more.
    fn axis(&mut self, l: DVec3, lo: f64, hi: f64) -> bool {
        let r = self.e.dot(l.abs());
        let v = self.d.dot(l);
        // Signed gaps: the box below the obstacle's range, and above it.
        let below = lo - r;
        let above = -(hi + r);
        let (sep, n, closing) = if below >= above {
            (below, -l, v)
        } else {
            (above, l, -v)
        };
        // Written so that a NaN from hostile input ends the sweep.
        if !(sep.is_finite() && closing.is_finite()) {
            return false;
        }
        let exit = if sep >= -self.tol {
            // Separated (or touching) at the start: the motion must close
            // this axis for the bodies ever to overlap.
            if closing <= 0.0 {
                return false;
            }
            let entry = sep / closing;
            if entry > self.entry {
                self.entry = entry;
                self.normal = n;
            }
            self.separated = true;
            (sep + (hi - lo) + 2.0 * r) / closing
        } else {
            if sep > self.depth {
                self.depth = sep;
                self.depth_normal = n;
            }
            if v > 0.0 {
                (hi + r) / v
            } else if v < 0.0 {
                (lo - r) / v
            } else {
                f64::INFINITY
            }
        };
        if exit < self.exit {
            self.exit = exit;
        }
        self.entry <= self.exit && self.entry <= self.limit
    }

    fn finish(&self) -> Option<Contact> {
        if self.separated {
            let t = self.entry.max(0.0);
            (self.entry <= self.exit && t <= self.limit && self.normal != DVec3::ZERO).then_some(
                Contact {
                    t,
                    normal: self.normal,
                    penetrating: false,
                },
            )
        } else {
            // Deeper than the tolerance on every axis.
            (self.depth.is_finite() && self.d.dot(self.depth_normal) < 0.0).then_some(Contact {
                t: 0.0,
                normal: self.depth_normal,
                penetrating: true,
            })
        }
    }
}

fn valid_query(s: DVec3, d: DVec3, e: DVec3, tol: f64, limit: f64) -> bool {
    s.is_finite()
        && d.is_finite()
        && e.is_finite()
        && e.cmpgt(DVec3::ZERO).all()
        && d != DVec3::ZERO
        && tol.is_finite()
        && tol >= 0.0
        && limit >= 0.0
}

/// Range of the points on the unit axis `l`.
fn range(points: &[DVec3], l: DVec3) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in points {
        let x = p.dot(l);
        lo = lo.min(x);
        hi = hi.max(x);
    }
    (lo, hi)
}

/// Component range of the points on a world axis (exact).
fn range_axis(points: &[DVec3], axis: usize) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in points {
        lo = lo.min(p[axis]);
        hi = hi.max(p[axis]);
    }
    (lo, hi)
}

const AXES: [DVec3; 3] = [DVec3::X, DVec3::Y, DVec3::Z];
/// The original tests the box's face axes in the order Z, X, Y.
const FACE_ORDER: [usize; 3] = [2, 0, 1];

/// `a × b` normalised when the two unit directions are not parallel.
fn cross_axis(a: DVec3, b: DVec3) -> Option<DVec3> {
    let c = a.cross(b);
    let l2 = c.length_squared();
    (l2.is_finite() && l2 >= PARALLEL_EPS2).then(|| c / l2.sqrt())
}

/// The unit axis `a × b` under the original's rule `skip` (`edge` is the
/// obstacle's edge direction, `box_edge` the index of the box's edge), or
/// under ours without one: only a parallel pair gives no axis.
fn edge_axis(
    edge: DVec3,
    box_edge: usize,
    a: DVec3,
    b: DVec3,
    skip: Option<&AxisSkip>,
) -> Option<DVec3> {
    match skip {
        None => cross_axis(a, b),
        Some(rule) => {
            if rule.skips_edge(edge, box_edge) {
                return None;
            }
            let c = a.cross(b);
            let l2 = c.length_squared();
            (l2.is_finite() && l2 > 0.0)
                .then(|| c / l2.sqrt())
                .filter(|l| l.is_finite())
        }
    }
}

/// Sweeps the axis-aligned box with half extents `e` and centre `s + t·d`,
/// `t ∈ [0, limit]` (`limit ≤ 1`), against the triangle `v`. `outward`
/// makes the triangle one-sided: it is ignored when the box centre starts
/// behind its plane. `tol` is the start-contact tolerance (UU). `None` for
/// no contact, for a contact later than `limit`, for a zero motion and for
/// non-finite or non-positive input. See the module docs for the normal and
/// for contacts at the start.
#[must_use]
pub fn sweep_triangle(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    v: &[DVec3; 3],
    outward: Option<DVec3>,
    tol: f64,
    limit: f64,
) -> Option<Contact> {
    sweep_triangle_impl(s, d, e, v, outward, tol, limit, None)
}

/// [`sweep_triangle`] with the original's rule for leaving axes out (see
/// [`AxisSkip`]): an axis the rule skips cannot separate the two, so the
/// contact is never later than [`sweep_triangle`]'s and can be earlier.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn sweep_triangle_skipping(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    v: &[DVec3; 3],
    outward: Option<DVec3>,
    tol: f64,
    limit: f64,
    skip: &AxisSkip,
) -> Option<Contact> {
    sweep_triangle_impl(s, d, e, v, outward, tol, limit, Some(skip))
}

#[allow(clippy::too_many_arguments)]
fn sweep_triangle_impl(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    v: &[DVec3; 3],
    outward: Option<DVec3>,
    tol: f64,
    limit: f64,
    skip: Option<&AxisSkip>,
) -> Option<Contact> {
    if !valid_query(s, d, e, tol, limit) || !v.iter().all(|p| p.is_finite()) {
        return None;
    }
    // Relative to the start, so far-away coordinates cancel before the
    // projections.
    let w = [v[0] - s, v[1] - s, v[2] - s];
    let face = triangle_normal(&w);
    if let (Some(out), Some(n)) = (outward, face) {
        let n = if n.dot(out) < 0.0 { -n } else { n };
        // `n · (s − v0) < 0`: the centre starts behind the plane.
        if n.dot(w[0]) > 0.0 {
            return None;
        }
    }
    let mut sat = Sat::new(e, d, tol, limit.min(1.0));
    for a in FACE_ORDER {
        if skip.is_some_and(|rule| rule.skips_face(a)) {
            continue;
        }
        let (lo, hi) = range_axis(&w, a);
        if !sat.axis(AXES[a], lo, hi) {
            return None;
        }
    }
    if let Some(n) = face {
        let p = n.dot(w[0]);
        if !sat.axis(n, p, p) {
            return None;
        }
    }
    let edges = [
        (w[1] - w[0]).normalize_or_zero(),
        (w[2] - w[1]).normalize_or_zero(),
        (w[0] - w[2]).normalize_or_zero(),
    ];
    for (bi, b) in AXES.into_iter().enumerate() {
        for edge in edges {
            if let Some(l) = edge_axis(edge, bi, b, edge, skip) {
                let (lo, hi) = range(&w, l);
                if !sat.axis(l, lo, hi) {
                    return None;
                }
            }
        }
    }
    sat.finish()
}

/// Sweeps the box against the convex hull of `vertices` (world space),
/// whose face-normal directions are `face_dirs` and edge directions
/// `edge_dirs` (unit vectors, either sign, world space). Same conventions
/// as [`sweep_triangle`]; the hull is solid. An incomplete direction list
/// can only make the hull larger (extra contacts), never let the box
/// through a listed face.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn sweep_hull(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    vertices: &[DVec3],
    face_dirs: &[DVec3],
    edge_dirs: &[DVec3],
    tol: f64,
    limit: f64,
) -> Option<Contact> {
    sweep_hull_impl(s, d, e, vertices, face_dirs, edge_dirs, tol, limit, None)
}

/// [`sweep_hull`] with the original's rule for leaving axes out (see
/// [`AxisSkip`]; the caller also applies [`convex_precheck`]). An axis the
/// rule skips cannot separate the two, so the contact is never later than
/// [`sweep_hull`]'s and can be earlier: with the box's own face axes left
/// out, the box is stopped by the hull's face planes pushed out by the box
/// and by the edge axes that remain, with no bevel where the others were.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn sweep_hull_skipping(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    vertices: &[DVec3],
    face_dirs: &[DVec3],
    edge_dirs: &[DVec3],
    tol: f64,
    limit: f64,
    skip: &AxisSkip,
) -> Option<Contact> {
    sweep_hull_impl(
        s,
        d,
        e,
        vertices,
        face_dirs,
        edge_dirs,
        tol,
        limit,
        Some(skip),
    )
}

#[allow(clippy::too_many_arguments)]
fn sweep_hull_impl(
    s: DVec3,
    d: DVec3,
    e: DVec3,
    vertices: &[DVec3],
    face_dirs: &[DVec3],
    edge_dirs: &[DVec3],
    tol: f64,
    limit: f64,
    skip: Option<&AxisSkip>,
) -> Option<Contact> {
    if !valid_query(s, d, e, tol, limit)
        || vertices.is_empty()
        || !vertices.iter().all(|p| p.is_finite())
    {
        return None;
    }
    let mut sat = Sat::new(e, d, tol, limit.min(1.0));
    // The world axes first: they are also the cheap rejection of far hulls.
    for a in FACE_ORDER {
        if skip.is_some_and(|rule| rule.skips_face(a)) {
            continue;
        }
        let (lo, hi) = range_axis(vertices, a);
        if !sat.axis(AXES[a], lo - s[a], hi - s[a]) {
            return None;
        }
    }
    for &n in face_dirs {
        let (lo, hi) = range(vertices, n);
        let p = s.dot(n);
        if !sat.axis(n, lo - p, hi - p) {
            return None;
        }
    }
    for &edge in edge_dirs {
        for (bi, b) in AXES.into_iter().enumerate() {
            if let Some(l) = edge_axis(edge, bi, edge, b, skip) {
                let (lo, hi) = range(vertices, l);
                let p = s.dot(l);
                if !sat.axis(l, lo - p, hi - p) {
                    return None;
                }
            }
        }
    }
    sat.finish()
}

/// Overlap of the projections on the unit axis `l` by more than nothing
/// (touching does not count); `[lo, hi]` relative to the box centre.
fn overlaps_axis(e: DVec3, l: DVec3, lo: f64, hi: f64) -> bool {
    let r = e.dot(l.abs());
    lo - r < 0.0 && hi + r > 0.0
}

/// Static overlap of the solid box `(center, e)` with the triangle
/// (touching does not count).
#[must_use]
pub fn overlaps_triangle(center: DVec3, e: DVec3, v: &[DVec3; 3]) -> bool {
    if !(center.is_finite() && e.is_finite() && e.cmpgt(DVec3::ZERO).all())
        || !v.iter().all(|p| p.is_finite())
    {
        return false;
    }
    let w = [v[0] - center, v[1] - center, v[2] - center];
    for a in FACE_ORDER {
        let (lo, hi) = range_axis(&w, a);
        if !overlaps_axis(e, AXES[a], lo, hi) {
            return false;
        }
    }
    if let Some(n) = triangle_normal(&w) {
        let p = n.dot(w[0]);
        if !overlaps_axis(e, n, p, p) {
            return false;
        }
    }
    let edges = [
        (w[1] - w[0]).normalize_or_zero(),
        (w[2] - w[1]).normalize_or_zero(),
        (w[0] - w[2]).normalize_or_zero(),
    ];
    for b in AXES {
        for edge in edges {
            if let Some(l) = cross_axis(b, edge) {
                let (lo, hi) = range(&w, l);
                if !overlaps_axis(e, l, lo, hi) {
                    return false;
                }
            }
        }
    }
    true
}

/// Static overlap of the solid box with the convex hull (see
/// [`sweep_hull`]); touching does not count.
#[must_use]
pub fn overlaps_hull(
    center: DVec3,
    e: DVec3,
    vertices: &[DVec3],
    face_dirs: &[DVec3],
    edge_dirs: &[DVec3],
) -> bool {
    if !(center.is_finite() && e.is_finite() && e.cmpgt(DVec3::ZERO).all())
        || vertices.is_empty()
        || !vertices.iter().all(|p| p.is_finite())
    {
        return false;
    }
    for a in FACE_ORDER {
        let (lo, hi) = range_axis(vertices, a);
        if !overlaps_axis(e, AXES[a], lo - center[a], hi - center[a]) {
            return false;
        }
    }
    for &n in face_dirs {
        let (lo, hi) = range(vertices, n);
        let p = center.dot(n);
        if !overlaps_axis(e, n, lo - p, hi - p) {
            return false;
        }
    }
    for &edge in edge_dirs {
        for b in AXES {
            if let Some(l) = cross_axis(edge, b) {
                let (lo, hi) = range(vertices, l);
                let p = center.dot(l);
                if !overlaps_axis(e, l, lo - p, hi - p) {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: DVec3 = DVec3::new(21.0, 21.0, 44.0);
    const TOL: f64 = 1.0e-3;

    fn floor() -> [DVec3; 3] {
        [
            DVec3::new(-1000.0, -1000.0, 0.0),
            DVec3::new(1000.0, -1000.0, 0.0),
            DVec3::new(0.0, 1000.0, 0.0),
        ]
    }

    /// A box falling 200 uu from z = 100 meets the floor z = 0 when its
    /// centre is at z = 44: after 56 of 200.
    #[test]
    fn falls_onto_a_floor_at_its_half_height() {
        let c = sweep_triangle(
            DVec3::new(0.0, 0.0, 100.0),
            DVec3::new(0.0, 0.0, -200.0),
            E,
            &floor(),
            None,
            TOL,
            1.0,
        )
        .unwrap();
        assert_eq!(c.t, 56.0 / 200.0);
        assert_eq!(c.normal, DVec3::Z);
        assert!(!c.penetrating);
        // A contact later than the limit is not reported; one at it is.
        let run = |limit| {
            sweep_triangle(
                DVec3::new(0.0, 0.0, 100.0),
                DVec3::new(0.0, 0.0, -200.0),
                E,
                &floor(),
                None,
                TOL,
                limit,
            )
        };
        assert!(run(0.27).is_none());
        assert_eq!(run(0.28), Some(c));
    }

    /// One-sided triangles are ignored from behind; two-sided ones are hit
    /// from below with the normal pointing down.
    #[test]
    fn one_sided_triangles_are_ignored_from_behind() {
        let s = DVec3::new(0.0, 0.0, -100.0);
        let d = DVec3::new(0.0, 0.0, 200.0);
        let c = sweep_triangle(s, d, E, &floor(), None, TOL, 1.0).unwrap();
        assert_eq!((c.t, c.normal), (56.0 / 200.0, DVec3::NEG_Z));
        assert!(sweep_triangle(s, d, E, &floor(), Some(DVec3::Z), TOL, 1.0).is_none());
        assert!(sweep_triangle(s, d, E, &floor(), Some(DVec3::NEG_Z), TOL, 1.0).is_some());
    }

    /// The hull of a cube and the cube's twelve triangles give the same
    /// contact for a box that starts outside.
    #[test]
    fn a_hull_agrees_with_its_triangles_from_outside() {
        let verts: Vec<DVec3> = (0..8)
            .map(|i| {
                DVec3::new(
                    f64::from(i & 1) * 100.0,
                    f64::from((i >> 1) & 1) * 60.0,
                    f64::from((i >> 2) & 1) * 30.0,
                )
            })
            .collect();
        let tris: [[usize; 3]; 12] = [
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
        let dirs = [DVec3::X, DVec3::Y, DVec3::Z];
        for (s, d) in [
            (DVec3::new(300.0, 30.0, 15.0), DVec3::new(-400.0, 0.0, 0.0)),
            (DVec3::new(50.0, 30.0, 300.0), DVec3::new(0.0, 0.0, -400.0)),
            (
                DVec3::new(300.0, 200.0, 150.0),
                DVec3::new(-400.0, -300.0, -200.0),
            ),
            (
                DVec3::new(-200.0, -90.0, 20.0),
                DVec3::new(300.0, 110.0, 5.0),
            ),
        ] {
            let hull = sweep_hull(s, d, E, &verts, &dirs, &dirs, TOL, 1.0).unwrap();
            let mut best: Option<Contact> = None;
            for t in tris {
                let tri = [verts[t[0]], verts[t[1]], verts[t[2]]];
                if let Some(c) = sweep_triangle(s, d, E, &tri, None, TOL, 1.0)
                    && best.is_none_or(|b| c.t < b.t)
                {
                    best = Some(c);
                }
            }
            let best = best.unwrap();
            assert!((hull.t - best.t).abs() < 1e-12, "{hull:?} vs {best:?}");
            assert!((hull.normal - best.normal).length() < 1e-12);
        }
    }

    #[test]
    fn hostile_input_is_rejected() {
        let f = floor();
        let s = DVec3::new(0.0, 0.0, 100.0);
        let d = DVec3::new(0.0, 0.0, -200.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(sweep_triangle(DVec3::splat(bad), d, E, &f, None, TOL, 1.0).is_none());
            assert!(sweep_triangle(s, DVec3::splat(bad), E, &f, None, TOL, 1.0).is_none());
            assert!(sweep_triangle(s, d, DVec3::splat(bad), &f, None, TOL, 1.0).is_none());
            assert!(sweep_triangle(s, d, E, &f, None, bad, 1.0).is_none());
            let mut g = f;
            g[1].y = bad;
            assert!(sweep_triangle(s, d, E, &g, None, TOL, 1.0).is_none());
            assert!(!overlaps_triangle(DVec3::splat(bad), E, &f));
            assert!(!overlaps_triangle(s, E, &g));
            assert!(sweep_hull(s, d, E, &g, &[DVec3::Z], &[], TOL, 1.0).is_none());
            assert!(!overlaps_hull(s, E, &g, &[DVec3::Z], &[]));
        }
        assert!(sweep_triangle(s, DVec3::ZERO, E, &f, None, TOL, 1.0).is_none());
        assert!(sweep_triangle(s, d, DVec3::new(21.0, 0.0, 44.0), &f, None, TOL, 1.0).is_none());
        assert!(sweep_triangle(s, d, -E, &f, None, TOL, 1.0).is_none());
        assert!(sweep_hull(s, d, E, &[], &[], &[], TOL, 1.0).is_none());
        assert!(!overlaps_hull(s, E, &[], &[], &[]));
        // A degenerate triangle is a segment or a point: still solid.
        let seg = [DVec3::ZERO, DVec3::X * 10.0, DVec3::X * 20.0];
        let c = sweep_triangle(s, d, E, &seg, None, TOL, 1.0).unwrap();
        assert_eq!((c.t, c.normal), (0.28, DVec3::Z));
        let point = [DVec3::ZERO; 3];
        let c = sweep_triangle(s, d, E, &point, None, TOL, 1.0).unwrap();
        assert_eq!((c.t, c.normal), (0.28, DVec3::Z));
    }
}
