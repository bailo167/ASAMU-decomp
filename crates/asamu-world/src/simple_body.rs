//! The simple-collision body of a converted mesh as collision meshes.
//!
//! The original collides a pawn with a static mesh at the mesh's simple
//! body, not at its triangles, unless the mesh says otherwise
//! (`docs/PARITY_FINDINGS.md` P2; the switches and the shapes are in
//! `docs/reverse-engineering/MESHES.md`, "Simple collision"). A body is a
//! list of convex elements, boxes, spheres and capsules in the mesh's local
//! space. [`build_body`] turns the document the importer wrote
//! ([`asamu_assets::simple_collision::SimpleCollision`]) into meshes of a
//! [`CollisionSceneBuilder`], one per shape a swept box can hit; the scene
//! loader ([`crate::scene`]) places them with the component's transform.
//!
//! # What each shape becomes
//!
//! - **Convex element**: one convex mesh
//!   ([`CollisionSceneBuilder::add_convex_hull`]) made of the element's
//!   vertices, its surface triangles and its own two direction lists. The box
//!   sweep tests what the original's check tests: the range of **all** the
//!   element's vertices on the box's face axes, on every stored face-normal
//!   direction and on every stored edge direction crossed with the box's
//!   edges, in that order (NATIVE_PHYSICS.md 10.1). Nothing is derived from
//!   the triangles and nothing checks that the element is convex.
//! - **Box**: the box as a convex mesh, placed by the element's own matrix.
//!   The original has a separate routine for box elements whose axis order
//!   was not read (TENTATIVE); no static mesh of the shipped game has one.
//! - **Sphere, capsule**: nothing. The original's checks of both return "no
//!   hit" at once when the trace has an extent (`FKSphereElem::LineCheck` at
//!   0x100AA6710 and `FKSphylElem::LineCheck` at 0x100AA7510 in the user's
//!   macOS executable: each compares the three components of the extent with
//!   zero first and leaves with its "no hit" value when one differs;
//!   CONFIRMED, read twice). They answer zero-extent traces only, and no ray
//!   test against them exists here: [`Body::untraced`] counts them so the
//!   loader can say so when a mesh would need one. The shipped game has one
//!   such mesh, the engine's own sphere, and its line traces go to its
//!   triangles.
//!
//! # Elements that are not convex
//!
//! 584 of the shipped game's 3,446 convex elements have a vertex outside one
//! of their own planes (MESHES.md), and some reach far outside their mesh.
//! That is the original's data and the original collides with it as it is.
//! For the box sweep such an element is the solid that its stored directions
//! cut out around its vertices: every vertex is inside it, so it is never
//! smaller than the convex hull of the vertices along those directions and
//! it can be larger than the surface the triangles describe. The sweep is a
//! fixed number of projections per element and cannot loop or fail on any
//! input. Zero-extent traces and the legacy cylinder sweep see the surface
//! triangles one-sided instead (outward as seen from the vertices' centre),
//! which on a dented element is the dented surface; the original's
//! zero-extent check clips the line against the element's planes, which is
//! the smaller solid the planes enclose. No trace of the game is known to go
//! to a simple body (the grapple's is a bullet trace and goes to the
//! triangles), so that difference is recorded and nothing depends on it.

use asamu_assets::simple_collision::{ConvexShape, SimpleCollision};
use glam::Vec3;

use crate::collision::{Affine, CollisionSceneBuilder};

/// One shape of a body that a swept box can hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPart {
    /// The shape's mesh in the builder.
    pub mesh: u32,
    /// The shape's space → the mesh's local space (the identity for a convex
    /// element, the element's matrix for a box).
    pub local: Affine,
    /// A convex element, which the original sweeps with its own axis rule
    /// ([`crate::collision::NativeRule`]); `false` for a box.
    pub convex: bool,
}

/// The simple body of one mesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    /// The convex elements, then the boxes, each in the document's order
    /// (the original tests the convex elements first and keeps the earlier
    /// shape on equal times).
    pub parts: Vec<BodyPart>,
    /// Spheres and capsules: shapes that never stop a swept box and that
    /// zero-extent traces cannot be tested against here.
    pub untraced: usize,
    /// Shapes that could not be used (no vertex, no size).
    pub skipped: usize,
}

/// Triangles a part costs in the scene's budget.
#[must_use]
pub fn convex_cost(shape: &ConvexShape) -> usize {
    shape
        .face_triangles
        .len()
        .saturating_add(shape.vertices.len())
}

/// The triangles of a convex element as the mesh is built with: its surface
/// triangles, and a collapsed triangle for every vertex none of them uses,
/// so that the solid is made of all the element's vertices (the original's
/// check projects every vertex) and an element without a surface still has a
/// mesh.
fn element_triangles(shape: &ConvexShape) -> Vec<[u32; 3]> {
    let mut used = vec![false; shape.vertices.len()];
    for t in &shape.face_triangles {
        for &i in t {
            if let Some(u) = used.get_mut(i as usize) {
                *u = true;
            }
        }
    }
    let mut triangles = shape.face_triangles.clone();
    for (i, u) in used.iter().enumerate() {
        if !u && let Ok(i) = u32::try_from(i) {
            triangles.push([i, i, i]);
        }
    }
    triangles
}

/// Adds the shapes of `shapes` that a swept box can hit to `builder`.
/// `charge` takes triangles from the caller's budget and answers `false`
/// when they do not fit; the body then ends there and the error says so.
///
/// # Errors
/// The budget ran out (the parts built so far stay in the builder but the
/// body is not usable).
pub fn build_body(
    builder: &mut CollisionSceneBuilder,
    shapes: &SimpleCollision,
    charge: &mut dyn FnMut(usize) -> bool,
) -> Result<Body, String> {
    let mut body = Body {
        untraced: shapes.spheres.len().saturating_add(shapes.sphyls.len()),
        ..Body::default()
    };
    for shape in &shapes.convex {
        if !charge(convex_cost(shape)) {
            return Err("triangle budget exhausted".to_owned());
        }
        let vertices: Vec<Vec3> = shape
            .vertices
            .iter()
            .map(|v| Vec3::from_array(*v))
            .collect();
        let face: Vec<Vec3> = shape
            .face_normal_directions
            .iter()
            .map(|v| Vec3::from_array(*v))
            .collect();
        let edge: Vec<Vec3> = shape
            .edge_directions
            .iter()
            .map(|v| Vec3::from_array(*v))
            .collect();
        match builder.add_convex_hull(vertices, element_triangles(shape), &face, &edge) {
            Some(mesh) => body.parts.push(BodyPart {
                mesh,
                local: Affine::IDENTITY,
                convex: true,
            }),
            None => body.skipped += 1,
        }
    }
    for shape in &shapes.boxes {
        if !charge(12) {
            return Err("triangle budget exhausted".to_owned());
        }
        let half = Vec3::from_array(shape.size) * 0.5;
        match builder.add_box_mesh(half) {
            Some(mesh) => body.parts.push(BodyPart {
                mesh,
                local: Affine::from_row_matrix(&shape.transform),
                convex: false,
            }),
            None => body.skipped += 1,
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetra() -> ConvexShape {
        ConvexShape {
            vertices: vec![
                [0.0, 0.0, 0.0],
                [10.0, 0.0, 0.0],
                [0.0, 10.0, 0.0],
                [0.0, 0.0, 10.0],
                // Not on the surface: only the collapsed triangle uses it.
                [20.0, 20.0, 20.0],
            ],
            planes: Vec::new(),
            face_triangles: vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
            edge_directions: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            face_normal_directions: vec![[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
            bounds_min: [0.0; 3],
            bounds_max: [20.0; 3],
        }
    }

    /// Every vertex of an element is part of its solid, used by a triangle
    /// or not, and an element without triangles still becomes a mesh.
    #[test]
    fn a_body_keeps_every_vertex_of_an_element() {
        let mut b = CollisionSceneBuilder::new();
        let mut bare = tetra();
        bare.face_triangles.clear();
        let shapes = SimpleCollision {
            convex: vec![tetra(), bare],
            ..SimpleCollision::default()
        };
        let body = build_body(&mut b, &shapes, &mut |_| true).unwrap();
        assert_eq!((body.parts.len(), body.skipped, body.untraced), (2, 0, 0));
        for part in &body.parts {
            assert!(part.convex);
            assert_eq!(part.local, Affine::IDENTITY);
            let hull = b.mesh(part.mesh).unwrap().hull().unwrap();
            assert_eq!(hull.vertices().len(), 5);
            // The stored lists, in their order.
            assert_eq!(
                hull.face_directions(),
                [glam::DVec3::Z, glam::DVec3::Y, glam::DVec3::X]
            );
            assert_eq!(
                hull.edge_directions(),
                [glam::DVec3::X, glam::DVec3::Y, glam::DVec3::Z]
            );
        }
    }

    /// The budget is asked before every shape and ends the body.
    #[test]
    fn the_budget_ends_a_body() {
        let mut b = CollisionSceneBuilder::new();
        let shapes = SimpleCollision {
            convex: vec![tetra(), tetra()],
            ..SimpleCollision::default()
        };
        let mut asked = Vec::new();
        let r = build_body(&mut b, &shapes, &mut |n| {
            asked.push(n);
            asked.len() < 2
        });
        assert!(r.is_err());
        // 4 surface triangles and 5 vertices each.
        assert_eq!(asked, vec![9, 9]);
    }
}
