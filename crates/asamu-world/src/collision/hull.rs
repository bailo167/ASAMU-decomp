//! Convex hulls as solids: the data a swept box needs.
//!
//! The simple-collision bodies of meshes (convex elements, boxes) and the
//! brushes of blocking volumes are convex polyhedra. Zero-extent traces and
//! the cylinder queries treat them as one-sided triangles; the box sweep
//! ([`super::box_sweep`]) tests the solid itself and needs its vertices, the
//! directions of its face normals and the directions of its edges.
//!
//! [`ConvexHull::from_triangles`] derives the three lists from a closed
//! triangle mesh and refuses meshes that are not closed and convex, so a
//! hostile or broken "hull" falls back to its triangles instead of being
//! treated as the convex hull of its points. [`ConvexHull::with_directions`]
//! takes the direction lists as given: the original's convex elements store
//! their own (`FaceNormalDirections`, `EdgeDirections`), and the original
//! tests exactly those, in that order.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DVec3, Vec3};

use super::cylinder::triangle_normal;

/// More vertices than this and the mesh is not treated as one solid (the
/// cost of a box sweep grows with vertices × directions). The original's
/// convex elements are far smaller.
pub const MAX_HULL_VERTICES: usize = 256;
/// Likewise for triangles.
pub const MAX_HULL_TRIANGLES: usize = 2048;

/// Two unit directions closer than this (squared sine) are one direction.
const SAME_DIRECTION_EPS2: f64 = 1.0e-10;

/// A convex solid in its mesh's local space.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvexHull {
    vertices: Vec<DVec3>,
    face_dirs: Vec<DVec3>,
    edge_dirs: Vec<DVec3>,
}

/// Adds `d` (normalised) unless it is not a direction or the list already
/// has it up to sign.
fn push_direction(list: &mut Vec<DVec3>, d: DVec3) {
    let len = d.length();
    if !(len.is_finite() && len > 0.0) {
        return;
    }
    let u = d / len;
    if !u.is_finite() {
        return;
    }
    if list
        .iter()
        .all(|o| o.cross(u).length_squared() > SAME_DIRECTION_EPS2)
    {
        list.push(u);
    }
}

/// Bit pattern of a position with `-0.0` folded into `0.0`.
fn position_key(v: Vec3) -> [u32; 3] {
    let k = |x: f32| if x == 0.0 { 0 } else { x.to_bits() };
    [k(v.x), k(v.y), k(v.z)]
}

impl ConvexHull {
    /// The solid bounded by the closed convex triangle mesh, or `None` when
    /// the mesh is not one: too large, degenerate (no volume), not closed
    /// (an edge not shared by exactly two triangles) or not convex (a
    /// vertex in front of a face). Vertices are matched by position, so
    /// meshes with per-face vertices work.
    #[must_use]
    pub fn from_triangles(vertices: &[Vec3], triangles: &[[u32; 3]]) -> Option<Self> {
        if triangles.len() < 4 || triangles.len() > MAX_HULL_TRIANGLES {
            return None;
        }
        // Canonical vertex ids by position.
        let mut ids: BTreeMap<[u32; 3], u32> = BTreeMap::new();
        let mut points: Vec<DVec3> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(triangles.len());
        for t in triangles {
            let mut c = [0u32; 3];
            for (slot, &i) in c.iter_mut().zip(t) {
                let v = *vertices.get(i as usize)?;
                if !v.is_finite() {
                    return None;
                }
                let next = u32::try_from(points.len()).ok()?;
                let id = *ids.entry(position_key(v)).or_insert(next);
                if id == next {
                    points.push(v.as_dvec3());
                }
                *slot = id;
            }
            // Triangles collapsed to an edge or a point bound nothing.
            if c[0] != c[1] && c[1] != c[2] && c[2] != c[0] {
                tris.push(c);
            }
        }
        if points.len() < 4 || points.len() > MAX_HULL_VERTICES || tris.len() < 4 {
            return None;
        }
        let centroid = points.iter().copied().sum::<DVec3>() / points.len() as f64;
        let radius = points
            .iter()
            .map(|p| (*p - centroid).length())
            .fold(0.0f64, f64::max);
        if !(radius.is_finite() && radius > 0.0) {
            return None;
        }
        let eps = 1.0e-4 * (1.0 + radius);
        // Outward planes; every vertex must be behind every one of them and
        // the centroid strictly inside.
        let mut normals: Vec<DVec3> = Vec::with_capacity(tris.len());
        for t in &tris {
            let tri = [
                points[t[0] as usize],
                points[t[1] as usize],
                points[t[2] as usize],
            ];
            let mut n = triangle_normal(&tri)?;
            let inside = n.dot(tri[0] - centroid);
            if inside.abs() <= eps {
                return None;
            }
            if inside < 0.0 {
                n = -n;
            }
            let w = n.dot(tri[0]);
            if points.iter().any(|p| n.dot(*p) > w + eps) {
                return None;
            }
            normals.push(n);
        }
        // Closed: every edge is shared by exactly two triangles.
        let mut edges: BTreeMap<(u32, u32), (u32, [usize; 2])> = BTreeMap::new();
        for (ti, t) in tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                let entry = edges.entry(key).or_insert((0, [0, 0]));
                if entry.0 < 2 {
                    entry.1[entry.0 as usize] = ti;
                }
                entry.0 += 1;
            }
        }
        if edges.values().any(|(count, _)| *count != 2) {
            return None;
        }
        let mut face_dirs = Vec::new();
        for n in &normals {
            push_direction(&mut face_dirs, *n);
        }
        // Edges between coplanar triangles (the diagonal of a quad face) are
        // not edges of the solid.
        let mut edge_dirs = Vec::new();
        for ((a, b), (_, faces)) in &edges {
            let (n0, n1) = (normals[faces[0]], normals[faces[1]]);
            if n0.cross(n1).length_squared() <= SAME_DIRECTION_EPS2 && n0.dot(n1) > 0.0 {
                continue;
            }
            push_direction(&mut edge_dirs, points[*b as usize] - points[*a as usize]);
        }
        Some(Self {
            vertices: points,
            face_dirs,
            edge_dirs,
        })
    }

    /// The convex hull of `vertices` with the face-normal and edge
    /// directions as listed (order kept, duplicates and non-directions
    /// dropped). `None` without a finite vertex or a face direction, or
    /// with too many vertices or directions. Nothing checks the lists
    /// against the points: directions that are missing make the solid
    /// larger for the box sweep, never smaller.
    #[must_use]
    pub fn with_directions(
        vertices: &[Vec3],
        face_directions: &[Vec3],
        edge_directions: &[Vec3],
    ) -> Option<Self> {
        if vertices.len() > MAX_HULL_VERTICES
            || face_directions.len() > MAX_HULL_TRIANGLES
            || edge_directions.len() > 3 * MAX_HULL_TRIANGLES
        {
            return None;
        }
        let mut seen: BTreeSet<[u32; 3]> = BTreeSet::new();
        let mut points = Vec::new();
        for v in vertices {
            if v.is_finite() && seen.insert(position_key(*v)) {
                points.push(v.as_dvec3());
            }
        }
        let mut face_dirs = Vec::new();
        for d in face_directions {
            push_direction(&mut face_dirs, d.as_dvec3());
        }
        let mut edge_dirs = Vec::new();
        for d in edge_directions {
            push_direction(&mut edge_dirs, d.as_dvec3());
        }
        if points.is_empty() || face_dirs.is_empty() {
            return None;
        }
        Some(Self {
            vertices: points,
            face_dirs,
            edge_dirs,
        })
    }

    /// The solid's vertices (unique positions).
    #[must_use]
    pub fn vertices(&self) -> &[DVec3] {
        &self.vertices
    }

    /// Unit directions of the face normals (one per direction, either sign).
    #[must_use]
    pub fn face_directions(&self) -> &[DVec3] {
        &self.face_dirs
    }

    /// Unit directions of the edges (one per direction, either sign).
    #[must_use]
    pub fn edge_directions(&self) -> &[DVec3] {
        &self.edge_dirs
    }
}

/// The eight corners and twelve outward triangles of the box
/// `[-half, half]` (for simple-collision box elements).
#[must_use]
pub fn box_mesh(half: Vec3) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let vertices = (0..8u32)
        .map(|i| {
            let s = |bit: u32| if (i >> bit) & 1 == 1 { 1.0f32 } else { -1.0 };
            Vec3::new(s(0) * half.x, s(1) * half.y, s(2) * half.z)
        })
        .collect();
    let triangles = vec![
        [0, 3, 1],
        [0, 2, 3],
        [4, 5, 7],
        [4, 7, 6],
        [0, 1, 5],
        [0, 5, 4],
        [2, 7, 3],
        [2, 6, 7],
        [0, 6, 2],
        [0, 4, 6],
        [1, 3, 7],
        [1, 7, 5],
    ];
    (vertices, triangles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_has_three_face_and_three_edge_directions() {
        let (v, t) = box_mesh(Vec3::new(10.0, 20.0, 30.0));
        let h = ConvexHull::from_triangles(&v, &t).unwrap();
        assert_eq!(h.vertices().len(), 8);
        assert_eq!(h.face_directions().len(), 3);
        assert_eq!(h.edge_directions().len(), 3);
        for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
            assert!(h.face_directions().iter().any(|d| d.abs() == axis));
            assert!(h.edge_directions().iter().any(|d| d.abs() == axis));
        }
        // The triangles wind outwards.
        for tri in &t {
            let p = tri.map(|i| v[i as usize].as_dvec3());
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            assert!(n.dot(p[0]) > 0.0);
        }
    }

    #[test]
    fn a_tetrahedron_has_four_faces_and_six_edges() {
        let v = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(0.0, 0.0, 10.0),
        ];
        let t = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
        let h = ConvexHull::from_triangles(&v, &t).unwrap();
        assert_eq!(
            (
                h.vertices().len(),
                h.face_directions().len(),
                h.edge_directions().len()
            ),
            (4, 4, 6)
        );
    }

    #[test]
    fn meshes_that_are_no_convex_solid_are_refused() {
        let (v, t) = box_mesh(Vec3::splat(10.0));
        // Open: one triangle missing.
        assert!(ConvexHull::from_triangles(&v, &t[..11]).is_none());
        // Flat: all points in one plane.
        let flat: Vec<Vec3> = v.iter().map(|p| Vec3::new(p.x, p.y, 0.0)).collect();
        assert!(ConvexHull::from_triangles(&flat, &t).is_none());
        // Not convex: one corner pushed through the opposite side.
        let mut dent = v.clone();
        dent[7] = Vec3::new(-5.0, -5.0, -5.0);
        assert!(ConvexHull::from_triangles(&dent, &t).is_none());
        // Out-of-range index, non-finite vertex, too few triangles.
        let mut bad = t.clone();
        bad[0][0] = 99;
        assert!(ConvexHull::from_triangles(&v, &bad).is_none());
        let mut nan = v.clone();
        nan[3].y = f32::NAN;
        assert!(ConvexHull::from_triangles(&nan, &t).is_none());
        assert!(ConvexHull::from_triangles(&v, &t[..3]).is_none());
        assert!(ConvexHull::from_triangles(&[], &[]).is_none());
        // Per-face vertices (every triangle with its own three) still close.
        let mut soup_v = Vec::new();
        let mut soup_t = Vec::new();
        for tri in &t {
            let base = soup_v.len() as u32;
            soup_v.extend(tri.iter().map(|&i| v[i as usize]));
            soup_t.push([base, base + 1, base + 2]);
        }
        let h = ConvexHull::from_triangles(&soup_v, &soup_t).unwrap();
        assert_eq!(h.vertices().len(), 8);
    }

    #[test]
    fn listed_directions_are_kept_in_order_and_cleaned() {
        let (v, _) = box_mesh(Vec3::splat(10.0));
        let faces = [
            Vec3::new(0.0, 0.0, 2.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::X,
            Vec3::ZERO,
            Vec3::new(f32::NAN, 0.0, 0.0),
            Vec3::Y,
        ];
        let edges = [Vec3::Y, Vec3::NEG_Y, Vec3::X];
        let h = ConvexHull::with_directions(&v, &faces, &edges).unwrap();
        assert_eq!(h.face_directions(), [DVec3::Z, DVec3::X, DVec3::Y]);
        assert_eq!(h.edge_directions(), [DVec3::Y, DVec3::X]);
        assert!(ConvexHull::with_directions(&v, &[], &edges).is_none());
        assert!(ConvexHull::with_directions(&[], &faces, &edges).is_none());
        assert!(ConvexHull::with_directions(&[Vec3::splat(f32::NAN)], &faces, &edges).is_none());
    }
}
