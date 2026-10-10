//! Triangle-mesh collision for converted levels.
//!
//! A [`CollisionScene`] holds unique collision meshes in their local space
//! (static-mesh kDOP triangles, BSP triangles, convex hulls of blocking
//! volumes), placed by [`Instance`]s with their `LocalToWorld` transform.
//! Static instances sit in a top-level BVH; each mesh has its own BVH over
//! its triangles. Instances whose transform changes during play (falling
//! rocks) are passed separately to every query as a small "dynamic" list and
//! tested by brute force.
//!
//! Queries ([`CollisionScene::sweep_cylinder`], [`CollisionScene::raycast`],
//! [`CollisionScene::overlaps_cylinder`]) transform candidate triangles to
//! world space in `f64` and run the exact tests of [`cylinder`].
//!
//! # Determinism
//!
//! The BVHs depend only on their input (see [`bvh`]); a query's result is
//! the minimum of `(time, instance, triangle)` in lexicographic order over
//! every candidate, so the traversal order cannot change it, and the brute
//! force reference [`CollisionScene::sweep_cylinder_brute_force`] returns
//! bit-identical results.
//!
//! # Collision flags
//!
//! Every instance carries [`InstanceInfo::blocks_pawn`] (non-zero-extent
//! checks: the player's moves) and [`InstanceInfo::blocks_traces`]
//! (zero-extent checks: grapple and aim traces); [`QueryFilter`] selects one,
//! plus a mask of loaded sub-levels.

pub mod affine;
pub mod bvh;
pub mod cylinder;

use std::cell::Cell;

use glam::{DVec2, DVec3, Vec3};
use serde::{Deserialize, Serialize};

pub use affine::Affine;
pub use bvh::{Aabb3, Bvh};

use crate::SurfaceTag;

/// What a collision instance stands for, as far as the gameplay script
/// distinguishes actors (GRAPPLE.md §5, §12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionClass {
    /// BSP (the hit actor is the level's `WorldInfo`).
    WorldGeometry,
    /// A static-mesh actor (`StaticMeshActor` and subclasses).
    StaticMesh,
    /// A blocking volume (brush collision, no static-mesh component).
    BlockingVolume,
    /// An `InterpActor` (mover).
    InterpActor,
    /// `ASAMURechargeCrystal`.
    RechargeCrystal,
    /// `ASAMUGlowFlower`.
    GlowFlower,
    /// `ASAMUFallingRock`.
    FallingRock,
    /// `ASAMUFallingWhenGrappledRock`.
    FallingWhenGrappledRock,
    /// `ASAMUInteractable_Actor`.
    Interactable,
}

/// What a query sees of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Actor id (see `crate::scene::actor_id`); `None` for BSP.
    pub actor: Option<u32>,
    /// Actor class.
    pub class: CollisionClass,
    /// The actor's tag (besides `NotGrappleAble`).
    pub tag: SurfaceTag,
    /// `false` = the tag `NotGrappleAble`.
    pub grapple_able: bool,
    /// Blocks the player's (non-zero-extent) moves.
    pub blocks_pawn: bool,
    /// Blocks zero-extent traces (grapple fire, crosshair).
    pub blocks_traces: bool,
    /// Sub-level index (0 = persistent level).
    pub sublevel: u8,
}

/// The kind of query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// Pawn moves (instances with `blocks_pawn`).
    Pawn,
    /// Zero-extent traces (instances with `blocks_traces`).
    Trace,
}

/// Which instances a query considers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryFilter {
    /// Pawn moves or traces.
    pub kind: QueryKind,
    /// Bit `i` set = sub-level `i` is loaded (bit 0 = persistent level).
    pub sublevels: u64,
}

impl QueryFilter {
    /// Pawn moves against every sub-level.
    pub const PAWN: Self = Self {
        kind: QueryKind::Pawn,
        sublevels: u64::MAX,
    };
    /// Traces against every sub-level.
    pub const TRACE: Self = Self {
        kind: QueryKind::Trace,
        sublevels: u64::MAX,
    };

    /// `true` when the filter accepts the instance.
    #[must_use]
    pub fn accepts(&self, info: &InstanceInfo) -> bool {
        let loaded = info.sublevel < 64 && self.sublevels & (1u64 << info.sublevel) != 0;
        loaded
            && match self.kind {
                QueryKind::Pawn => info.blocks_pawn,
                QueryKind::Trace => info.blocks_traces,
            }
    }
}

/// Convex-hull data of a one-sided mesh.
#[derive(Clone, Debug, PartialEq)]
struct Convex {
    centroid: DVec3,
    /// Outward unit normal and offset (`n · p = w` on the plane).
    planes: Vec<(DVec3, f64)>,
}

/// A collision mesh in its local space.
#[derive(Clone, Debug, PartialEq)]
pub struct CollisionMesh {
    vertices: Vec<Vec3>,
    triangles: Vec<[u32; 3]>,
    bvh: Bvh,
    convex: Option<Convex>,
    /// An upright cylinder `(radius, half height)` instead of triangles (a
    /// pawn's `CylinderComponent`; see [`CollisionScene::add_cylinder_mesh`]).
    cylinder: Option<(f64, f64)>,
}

impl CollisionMesh {
    /// Local-space vertices.
    #[must_use]
    pub fn vertices(&self) -> &[Vec3] {
        &self.vertices
    }

    /// Triangles (indices into [`Self::vertices`]).
    #[must_use]
    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    /// Local bounds.
    #[must_use]
    pub fn bounds(&self) -> Option<Aabb3> {
        self.bvh.bounds()
    }

    /// `true` for a closed convex hull (one-sided triangles).
    #[must_use]
    pub fn is_convex(&self) -> bool {
        self.convex.is_some()
    }

    fn triangle_local(&self, i: u32) -> Option<[Vec3; 3]> {
        let t = self.triangles.get(i as usize)?;
        Some([
            *self.vertices.get(t[0] as usize)?,
            *self.vertices.get(t[1] as usize)?,
            *self.vertices.get(t[2] as usize)?,
        ])
    }
}

/// A placed mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    /// Mesh index in the scene.
    pub mesh: u32,
    /// Local → world.
    pub to_world: Affine,
    /// World → local.
    pub to_local: Affine,
    /// World bounds.
    pub bounds: Aabb3,
    /// What queries see.
    pub info: InstanceInfo,
}

/// Which instance a hit belongs to (statics order before dynamics).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InstanceRef {
    /// Index into the scene's static instances.
    Static(u32),
    /// Index into the dynamic list passed to the query.
    Dynamic(u32),
}

/// A query result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneHit {
    /// Fraction of the motion / ray before contact, `[0, 1]`.
    pub t: f64,
    /// Unit normal pointing towards the mover / ray origin.
    pub normal: DVec3,
    /// The sweep started overlapping (always `false` for rays).
    pub penetrating: bool,
    /// The instance hit.
    pub instance: InstanceRef,
    /// Triangle index in the instance's mesh.
    pub triangle: u32,
}

impl SceneHit {
    fn key(&self) -> (f64, InstanceRef, u32) {
        (self.t, self.instance, self.triangle)
    }

    fn better_than(&self, other: &Self) -> bool {
        let (a, ia, ta) = self.key();
        let (b, ib, tb) = other.key();
        a < b || (a == b && (ia, ta) < (ib, tb))
    }
}

/// Counts of a scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneStats {
    /// Unique meshes.
    pub meshes: usize,
    /// Triangles over all unique meshes.
    pub mesh_triangles: usize,
    /// Static instances.
    pub instances: usize,
    /// Triangles over all static instances (world triangles).
    pub instance_triangles: usize,
}

/// Start-contact tolerance (UU) for a query near `p`: 1e-3 plus two `f32`
/// ULPs of the largest coordinate (positions are stored in `f32`, so
/// contacts closer than that are not resolvable anyway).
#[must_use]
pub fn contact_tolerance(p: Vec3) -> f64 {
    let m = p.abs().max_element();
    let ulp = if m.is_finite() && m > 0.0 {
        f32::from_bits(m.to_bits() & 0x7F80_0000) * f32::EPSILON
    } else {
        0.0
    };
    1.0e-3 + 2.0 * f64::from(ulp)
}

/// Builds a [`CollisionScene`].
#[derive(Clone, Debug, Default)]
pub struct CollisionSceneBuilder {
    meshes: Vec<CollisionMesh>,
    statics: Vec<Instance>,
}

/// Valid triangles of `triangles` (indices in range, finite vertices).
fn clean_triangles(vertices: &[Vec3], triangles: Vec<[u32; 3]>) -> Vec<[u32; 3]> {
    triangles
        .into_iter()
        .filter(|t| {
            t.iter().all(|&i| {
                vertices
                    .get(i as usize)
                    .is_some_and(|v: &Vec3| v.is_finite())
            })
        })
        .collect()
}

fn triangle_boxes(vertices: &[Vec3], triangles: &[[u32; 3]]) -> Vec<Aabb3> {
    triangles
        .iter()
        .map(|t| {
            let mut b = Aabb3::EMPTY;
            for &i in t {
                if let Some(v) = vertices.get(i as usize) {
                    b = b.union(Aabb3 { min: *v, max: *v });
                }
            }
            b
        })
        .collect()
}

impl CollisionSceneBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a two-sided triangle mesh; returns its index, or `None` when no
    /// valid triangle remains (out-of-range indices and non-finite vertices
    /// are dropped).
    pub fn add_mesh(&mut self, vertices: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> Option<u32> {
        let triangles = clean_triangles(&vertices, triangles);
        if triangles.is_empty() {
            return None;
        }
        let bvh = Bvh::build(&triangle_boxes(&vertices, &triangles));
        let index = u32::try_from(self.meshes.len()).ok()?;
        self.meshes.push(CollisionMesh {
            vertices,
            triangles,
            bvh,
            convex: None,
            cylinder: None,
        });
        Some(index)
    }

    /// Adds the closed convex hull `vertices`/`triangles` as a one-sided
    /// mesh (outward normals point away from the vertex centroid).
    pub fn add_convex_mesh(
        &mut self,
        vertices: Vec<Vec3>,
        triangles: Vec<[u32; 3]>,
    ) -> Option<u32> {
        let triangles = clean_triangles(&vertices, triangles);
        if triangles.is_empty() {
            return None;
        }
        // The centroid of the vertices the triangles use (all finite after
        // cleaning; an unused non-finite or far-away vertex must not decide
        // which way the faces point).
        let mut used = vec![false; vertices.len()];
        for t in &triangles {
            for &i in t {
                if let Some(u) = used.get_mut(i as usize) {
                    *u = true;
                }
            }
        }
        let (sum, n) = vertices
            .iter()
            .zip(&used)
            .filter(|(_, u)| **u)
            .fold((DVec3::ZERO, 0usize), |(s, n), (v, _)| {
                (s + v.as_dvec3(), n + 1)
            });
        let centroid = sum / n.max(1) as f64;
        let corner = |i: u32| vertices.get(i as usize).map(|v| v.as_dvec3());
        let mut planes = Vec::new();
        for t in &triangles {
            let (Some(a), Some(b), Some(c)) = (corner(t[0]), corner(t[1]), corner(t[2])) else {
                continue;
            };
            let tri = [a, b, c];
            if let Some(mut n) = cylinder::triangle_normal(&tri) {
                if n.dot(tri[0] - centroid) < 0.0 {
                    n = -n;
                }
                planes.push((n, n.dot(tri[0])));
            }
        }
        let bvh = Bvh::build(&triangle_boxes(&vertices, &triangles));
        let index = u32::try_from(self.meshes.len()).ok()?;
        self.meshes.push(CollisionMesh {
            vertices,
            triangles,
            bvh,
            convex: Some(Convex { centroid, planes }),
            cylinder: None,
        });
        Some(index)
    }

    /// The mesh `index`.
    #[must_use]
    pub fn mesh(&self, index: u32) -> Option<&CollisionMesh> {
        self.meshes.get(index as usize)
    }

    /// An instance of `mesh` placed by `to_world`. A transform without an
    /// inverse (zero scale on an axis) is baked: the mesh is copied in world
    /// space and placed with the identity. `None` for an unknown mesh or a
    /// non-finite transform.
    pub fn instance(
        &mut self,
        mesh: u32,
        to_world: Affine,
        info: InstanceInfo,
    ) -> Option<Instance> {
        if !to_world.is_finite() {
            return None;
        }
        let m = self.meshes.get(mesh as usize)?;
        let local = m.bvh.bounds()?;
        match to_world.inverse() {
            Some(to_local) => {
                let (lo, hi) = to_world.transform_aabb(local.min.as_dvec3(), local.max.as_dvec3());
                let (min, max) = affine::round_out(lo, hi);
                Some(Instance {
                    mesh,
                    to_world,
                    to_local,
                    bounds: Aabb3 { min, max },
                    info,
                })
            }
            None => {
                let vertices: Vec<Vec3> = m
                    .vertices
                    .iter()
                    .map(|v| to_world.point_f32(*v).as_vec3())
                    .collect();
                let triangles = m.triangles.clone();
                let convex = m.convex.is_some();
                let baked = if convex {
                    self.add_convex_mesh(vertices, triangles)?
                } else {
                    self.add_mesh(vertices, triangles)?
                };
                let b = self.meshes.get(baked as usize)?.bvh.bounds()?;
                Some(Instance {
                    mesh: baked,
                    to_world: Affine::IDENTITY,
                    to_local: Affine::IDENTITY,
                    bounds: b,
                    info,
                })
            }
        }
    }

    /// Adds a static instance (see [`Self::instance`]); returns its index.
    pub fn add_static(&mut self, mesh: u32, to_world: Affine, info: InstanceInfo) -> Option<u32> {
        let inst = self.instance(mesh, to_world, info)?;
        let index = u32::try_from(self.statics.len()).ok()?;
        self.statics.push(inst);
        Some(index)
    }

    /// Finishes the scene.
    #[must_use]
    pub fn build(self) -> CollisionScene {
        let boxes: Vec<Aabb3> = self.statics.iter().map(|i| i.bounds).collect();
        let tlas = Bvh::build(&boxes);
        CollisionScene {
            meshes: self.meshes,
            statics: self.statics,
            tlas,
        }
    }
}

/// Static collision geometry of a level (see the module docs).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CollisionScene {
    meshes: Vec<CollisionMesh>,
    statics: Vec<Instance>,
    tlas: Bvh,
}

/// Per-query constants of a sweep.
struct SweepQuery {
    s: DVec3,
    d: DVec3,
    r: f64,
    h: f64,
    tol: f64,
    pad: DVec3,
}

impl CollisionScene {
    /// An empty scene.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The meshes.
    #[must_use]
    pub fn meshes(&self) -> &[CollisionMesh] {
        &self.meshes
    }

    /// The static instances.
    #[must_use]
    pub fn statics(&self) -> &[Instance] {
        &self.statics
    }

    /// Counts.
    #[must_use]
    pub fn stats(&self) -> SceneStats {
        SceneStats {
            meshes: self.meshes.len(),
            mesh_triangles: self.meshes.iter().map(|m| m.triangles.len()).sum(),
            instances: self.statics.len(),
            instance_triangles: self
                .statics
                .iter()
                .filter_map(|i| self.meshes.get(i.mesh as usize))
                .map(|m| m.triangles.len())
                .sum(),
        }
    }

    /// World bounds of the static instances.
    #[must_use]
    pub fn bounds(&self) -> Option<Aabb3> {
        self.tlas.bounds()
    }

    /// Removes the static instances whose actor id is in `actors` (keeping
    /// the order of the rest) and rebuilds the top-level BVH; returns the
    /// removed instances in their former order. Used before play to turn
    /// actors that Kismet moves (Matinee) or removes into dynamic
    /// instances. Static instance indices change; no hit should be kept
    /// across this call.
    pub fn take_actor_statics(
        &mut self,
        actors: &std::collections::BTreeSet<u32>,
    ) -> Vec<Instance> {
        if actors.is_empty() {
            return Vec::new();
        }
        let (taken, kept): (Vec<Instance>, Vec<Instance>) = std::mem::take(&mut self.statics)
            .into_iter()
            .partition(|i| i.info.actor.is_some_and(|a| actors.contains(&a)));
        self.statics = kept;
        let boxes: Vec<Aabb3> = self.statics.iter().map(|i| i.bounds).collect();
        self.tlas = Bvh::build(&boxes);
        taken
    }

    /// A dynamic instance of `mesh` (for the list passed to the queries);
    /// `None` for an unknown mesh or a transform without an inverse.
    #[must_use]
    pub fn dynamic_instance(
        &self,
        mesh: u32,
        to_world: Affine,
        info: InstanceInfo,
    ) -> Option<Instance> {
        let m = self.meshes.get(mesh as usize)?;
        let local = m.bvh.bounds()?;
        let to_local = to_world.inverse()?;
        let (lo, hi) = to_world.transform_aabb(local.min.as_dvec3(), local.max.as_dvec3());
        let (min, max) = affine::round_out(lo, hi);
        Some(Instance {
            mesh,
            to_world,
            to_local,
            bounds: Aabb3 { min, max },
            info,
        })
    }

    /// Moves a dynamic instance to `to_world` (bounds updated); returns
    /// `false` (and leaves it unchanged) for a transform without an inverse.
    pub fn place_dynamic(&self, inst: &mut Instance, to_world: Affine) -> bool {
        match self.dynamic_instance(inst.mesh, to_world, inst.info) {
            Some(n) => {
                *inst = n;
                true
            }
            None => false,
        }
    }

    /// Adds an upright collision cylinder (`radius`, `half_height`, centred
    /// on its instance's origin) for dynamic instances: NPC pawns'
    /// `CylinderComponent`s, which block the player and move with the pawn.
    /// Instances of it use only their transform's translation (pawn
    /// cylinders stay upright). Queries are exact: a swept player cylinder
    /// against it is a swept point against their Minkowski sum (radius and
    /// half height added). That is the engine's model too (CONFIRMED,
    /// locally decompiled `UCylinderComponent::LineCheck`: the trace extent
    /// is added to the cylinder — `Extent.X` to the radius, `Extent.Z` to
    /// the half height — and the line is clipped against the two caps and
    /// the circle). The start-contact rules are ours, those of the triangle
    /// sweeps; the engine instead blocks a start inside only against radial
    /// inward motion and pulls every hit time back by 0.001 of the move
    /// (NPCS.md §12). `None` for a non-positive or non-finite size.
    pub fn add_cylinder_mesh(&mut self, radius: f32, half_height: f32) -> Option<u32> {
        if !(radius.is_finite() && half_height.is_finite() && radius > 0.0 && half_height > 0.0) {
            return None;
        }
        let index = u32::try_from(self.meshes.len()).ok()?;
        let local = Aabb3 {
            min: Vec3::new(-radius, -radius, -half_height),
            max: Vec3::new(radius, radius, half_height),
        };
        self.meshes.push(CollisionMesh {
            vertices: Vec::new(),
            triangles: Vec::new(),
            bvh: Bvh::build(&[local]),
            convex: None,
            cylinder: Some((f64::from(radius), f64::from(half_height))),
        });
        Some(index)
    }

    /// The instance a hit refers to.
    #[must_use]
    pub fn instance<'a>(&'a self, dynamic: &'a [Instance], r: InstanceRef) -> Option<&'a Instance> {
        match r {
            InstanceRef::Static(i) => self.statics.get(i as usize),
            InstanceRef::Dynamic(i) => dynamic.get(i as usize),
        }
    }

    /// World-space triangle `tri` of `inst`.
    #[must_use]
    pub fn world_triangle(&self, inst: &Instance, tri: u32) -> Option<[DVec3; 3]> {
        let m = self.meshes.get(inst.mesh as usize)?;
        let l = m.triangle_local(tri)?;
        Some([
            inst.to_world.point_f32(l[0]),
            inst.to_world.point_f32(l[1]),
            inst.to_world.point_f32(l[2]),
        ])
    }

    fn outward(&self, inst: &Instance, world: &[DVec3; 3]) -> Option<DVec3> {
        let m = self.meshes.get(inst.mesh as usize)?;
        let c = m.convex.as_ref()?;
        let centroid = inst.to_world.point(c.centroid);
        let n = cylinder::triangle_normal(world)?;
        Some(if n.dot(world[0] - centroid) < 0.0 {
            -n
        } else {
            n
        })
    }

    fn sweep_instance(
        &self,
        inst: &Instance,
        which: InstanceRef,
        q: &SweepQuery,
        best: &mut Option<SceneHit>,
    ) {
        let Some(mesh) = self.meshes.get(inst.mesh as usize) else {
            return;
        };
        if let Some((cr, ch)) = mesh.cylinder {
            let limit = best.map_or(1.0, |b| b.t);
            if let Some(c) = cylinder_sweep(
                q.s,
                q.d,
                q.r + cr,
                q.h + ch,
                inst.to_world.translation,
                q.tol,
                limit,
            ) {
                let hit = SceneHit {
                    t: c.t,
                    normal: c.normal,
                    penetrating: c.penetrating,
                    instance: which,
                    triangle: 0,
                };
                if best.is_none_or(|b| hit.better_than(&b)) {
                    *best = Some(hit);
                }
            }
            return;
        }
        let s_l = inst.to_local.point(q.s);
        let d_l = inst.to_local.vector(q.d);
        let pad_l = inst.to_local.transform_extent(q.pad);
        let inv = bvh::reciprocal(d_l);
        // The local box swept by the shape, for a per-triangle rejection
        // before the triangle is transformed.
        let e_l = s_l + d_l;
        let (lo, hi) = affine::round_out(s_l.min(e_l) - pad_l, s_l.max(e_l) + pad_l);
        let cell = Cell::new(*best);
        mesh.bvh.query_ordered(
            |b| bvh::segment_box_entry(s_l, inv, b, pad_l, 1.0),
            || cell.get().map_or(1.0, |b| b.t),
            |tri| {
                let Some(l) = mesh.triangle_local(tri) else {
                    return;
                };
                let tmin = l[0].min(l[1]).min(l[2]);
                let tmax = l[0].max(l[1]).max(l[2]);
                if tmax.cmplt(lo).any() || tmin.cmpgt(hi).any() {
                    return;
                }
                let w = l.map(|p| inst.to_world.point_f32(p));
                let outward = if mesh.convex.is_some() {
                    self.outward(inst, &w)
                } else {
                    None
                };
                let limit = cell.get().map_or(1.0, |b| b.t);
                if let Some(c) =
                    cylinder::sweep_within(q.s, q.d, q.r, q.h, &w, outward, q.tol, limit)
                {
                    let hit = SceneHit {
                        t: c.t,
                        normal: c.normal,
                        penetrating: c.penetrating,
                        instance: which,
                        triangle: tri,
                    };
                    if cell.get().is_none_or(|b| hit.better_than(&b)) {
                        cell.set(Some(hit));
                    }
                }
            },
        );
        *best = cell.get();
    }

    fn sweep_query(start: Vec3, end: Vec3, radius: f32, half_height: f32) -> Option<SweepQuery> {
        let s = start.as_dvec3();
        let d = end.as_dvec3() - s;
        let r = f64::from(radius);
        let h = f64::from(half_height);
        if !(s.is_finite() && d.is_finite() && r > 0.0 && h > 0.0 && r.is_finite() && h.is_finite())
        {
            return None;
        }
        let tol = contact_tolerance(start).max(contact_tolerance(end));
        let margin = tol + 1.0e-3;
        Some(SweepQuery {
            s,
            d,
            r,
            h,
            tol,
            pad: DVec3::new(r + margin, r + margin, h + margin),
        })
    }

    /// Sweeps an upright cylinder from `start` to `end` and returns the
    /// first contact with an instance accepted by `filter` (static or in
    /// `dynamic`).
    #[must_use]
    pub fn sweep_cylinder(
        &self,
        dynamic: &[Instance],
        start: Vec3,
        end: Vec3,
        radius: f32,
        half_height: f32,
        filter: QueryFilter,
    ) -> Option<SceneHit> {
        let q = Self::sweep_query(start, end, radius, half_height)?;
        let inv = bvh::reciprocal(q.d);
        let mut best: Option<SceneHit> = None;
        let mut candidates: Vec<u32> = Vec::new();
        self.tlas.query_ordered(
            |b| bvh::segment_box_entry(q.s, inv, b, q.pad, 1.0),
            || 1.0,
            |i| candidates.push(i),
        );
        // Instances in order of their entry parameter, so the pruning limit
        // tightens early.
        let mut ordered: Vec<(f64, u32)> = candidates
            .into_iter()
            .filter_map(|i| {
                let inst = self.statics.get(i as usize)?;
                if !filter.accepts(&inst.info) {
                    return None;
                }
                bvh::segment_box_entry(q.s, inv, &inst.bounds, q.pad, 1.0).map(|t| (t, i))
            })
            .collect();
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (t, i) in ordered {
            if best.is_some_and(|b| t > b.t) {
                break;
            }
            if let Some(inst) = self.statics.get(i as usize) {
                self.sweep_instance(inst, InstanceRef::Static(i), &q, &mut best);
            }
        }
        for (i, inst) in dynamic.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            if !filter.accepts(&inst.info) {
                continue;
            }
            if bvh::segment_box_entry(q.s, inv, &inst.bounds, q.pad, 1.0).is_some() {
                self.sweep_instance(inst, InstanceRef::Dynamic(i), &q, &mut best);
            }
        }
        best
    }

    /// Reference implementation of [`Self::sweep_cylinder`] without any
    /// acceleration structure (every triangle of every accepted instance).
    /// Returns bit-identical results; used by tests.
    #[must_use]
    pub fn sweep_cylinder_brute_force(
        &self,
        dynamic: &[Instance],
        start: Vec3,
        end: Vec3,
        radius: f32,
        half_height: f32,
        filter: QueryFilter,
    ) -> Option<SceneHit> {
        let q = Self::sweep_query(start, end, radius, half_height)?;
        let mut best: Option<SceneHit> = None;
        let all = self
            .statics
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstanceRef::Static(i as u32), inst))
            .chain(
                dynamic
                    .iter()
                    .enumerate()
                    .map(|(i, inst)| (InstanceRef::Dynamic(i as u32), inst)),
            );
        for (which, inst) in all {
            if !filter.accepts(&inst.info) {
                continue;
            }
            let Some(mesh) = self.meshes.get(inst.mesh as usize) else {
                continue;
            };
            if mesh.cylinder.is_some() {
                self.sweep_instance(inst, which, &q, &mut best);
                continue;
            }
            for tri in 0..mesh.triangles.len() as u32 {
                let Some(w) = self.world_triangle(inst, tri) else {
                    continue;
                };
                let outward = if mesh.convex.is_some() {
                    self.outward(inst, &w)
                } else {
                    None
                };
                if let Some(c) = cylinder::sweep(q.s, q.d, q.r, q.h, &w, outward, q.tol) {
                    let hit = SceneHit {
                        t: c.t,
                        normal: c.normal,
                        penetrating: c.penetrating,
                        instance: which,
                        triangle: tri,
                    };
                    if best.is_none_or(|b| hit.better_than(&b)) {
                        best = Some(hit);
                    }
                }
            }
        }
        best
    }

    fn ray_instance(
        &self,
        inst: &Instance,
        which: InstanceRef,
        o: DVec3,
        dv: DVec3,
        best: &mut Option<SceneHit>,
    ) {
        let Some(mesh) = self.meshes.get(inst.mesh as usize) else {
            return;
        };
        if let Some((cr, ch)) = mesh.cylinder {
            if let Some((t, normal)) = cylinder_ray(o, dv, cr, ch, inst.to_world.translation) {
                let hit = SceneHit {
                    t,
                    normal,
                    penetrating: false,
                    instance: which,
                    triangle: 0,
                };
                if best.is_none_or(|b| hit.better_than(&b)) {
                    *best = Some(hit);
                }
            }
            return;
        }
        let o_l = inst.to_local.point(o);
        let d_l = inst.to_local.vector(dv);
        let pad_l = inst.to_local.transform_extent(DVec3::splat(1.0e-3));
        let inv = bvh::reciprocal(d_l);
        let cell = Cell::new(*best);
        mesh.bvh.query_ordered(
            |b| bvh::segment_box_entry(o_l, inv, b, pad_l, 1.0),
            || cell.get().map_or(1.0, |b| b.t),
            |tri| {
                let Some(w) = self.world_triangle(inst, tri) else {
                    return;
                };
                let outward = if mesh.convex.is_some() {
                    self.outward(inst, &w)
                } else {
                    None
                };
                if let Some((t, n)) = cylinder::ray(o, dv, &w, outward) {
                    let hit = SceneHit {
                        t,
                        normal: n,
                        penetrating: false,
                        instance: which,
                        triangle: tri,
                    };
                    if cell.get().is_none_or(|b| hit.better_than(&b)) {
                        cell.set(Some(hit));
                    }
                }
            },
        );
        *best = cell.get();
    }

    /// First triangle hit by the segment `origin → end` among the instances
    /// accepted by `filter`.
    #[must_use]
    pub fn raycast(
        &self,
        dynamic: &[Instance],
        origin: Vec3,
        end: Vec3,
        filter: QueryFilter,
    ) -> Option<SceneHit> {
        let o = origin.as_dvec3();
        let dv = end.as_dvec3() - o;
        if !(o.is_finite() && dv.is_finite()) || dv == DVec3::ZERO {
            return None;
        }
        let inv = bvh::reciprocal(dv);
        let pad = DVec3::splat(1.0e-3);
        let mut best: Option<SceneHit> = None;
        let mut candidates: Vec<u32> = Vec::new();
        self.tlas.query_ordered(
            |b| bvh::segment_box_entry(o, inv, b, pad, 1.0),
            || 1.0,
            |i| candidates.push(i),
        );
        let mut ordered: Vec<(f64, u32)> = candidates
            .into_iter()
            .filter_map(|i| {
                let inst = self.statics.get(i as usize)?;
                if !filter.accepts(&inst.info) {
                    return None;
                }
                bvh::segment_box_entry(o, inv, &inst.bounds, pad, 1.0).map(|t| (t, i))
            })
            .collect();
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (t, i) in ordered {
            if best.is_some_and(|b| t > b.t) {
                break;
            }
            if let Some(inst) = self.statics.get(i as usize) {
                self.ray_instance(inst, InstanceRef::Static(i), o, dv, &mut best);
            }
        }
        for (i, inst) in dynamic.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            if filter.accepts(&inst.info)
                && bvh::segment_box_entry(o, inv, &inst.bounds, pad, 1.0).is_some()
            {
                self.ray_instance(inst, InstanceRef::Dynamic(i), o, dv, &mut best);
            }
        }
        best
    }

    /// Reference implementation of [`Self::raycast`] (every triangle).
    #[must_use]
    pub fn raycast_brute_force(
        &self,
        dynamic: &[Instance],
        origin: Vec3,
        end: Vec3,
        filter: QueryFilter,
    ) -> Option<SceneHit> {
        let o = origin.as_dvec3();
        let dv = end.as_dvec3() - o;
        if !(o.is_finite() && dv.is_finite()) || dv == DVec3::ZERO {
            return None;
        }
        let mut best: Option<SceneHit> = None;
        let all = self
            .statics
            .iter()
            .enumerate()
            .map(|(i, inst)| (InstanceRef::Static(i as u32), inst))
            .chain(
                dynamic
                    .iter()
                    .enumerate()
                    .map(|(i, inst)| (InstanceRef::Dynamic(i as u32), inst)),
            );
        for (which, inst) in all {
            if !filter.accepts(&inst.info) {
                continue;
            }
            let Some(mesh) = self.meshes.get(inst.mesh as usize) else {
                continue;
            };
            if mesh.cylinder.is_some() {
                self.ray_instance(inst, which, o, dv, &mut best);
                continue;
            }
            for tri in 0..mesh.triangles.len() as u32 {
                let Some(w) = self.world_triangle(inst, tri) else {
                    continue;
                };
                let outward = if mesh.convex.is_some() {
                    self.outward(inst, &w)
                } else {
                    None
                };
                if let Some((t, n)) = cylinder::ray(o, dv, &w, outward) {
                    let hit = SceneHit {
                        t,
                        normal: n,
                        penetrating: false,
                        instance: which,
                        triangle: tri,
                    };
                    if best.is_none_or(|b| hit.better_than(&b)) {
                        best = Some(hit);
                    }
                }
            }
        }
        best
    }

    fn overlaps_instance(
        &self,
        inst: &Instance,
        c: DVec3,
        r: f64,
        h: f64,
        world_box: &Aabb3,
    ) -> bool {
        let Some(mesh) = self.meshes.get(inst.mesh as usize) else {
            return false;
        };
        if let Some((cr, ch)) = mesh.cylinder {
            let p = c - inst.to_world.translation;
            return DVec2::new(p.x, p.y).length() < r + cr && p.z.abs() < h + ch;
        }
        if let Some(cv) = &mesh.convex {
            let lc = inst.to_local.point(c);
            if !cv.planes.is_empty() && cv.planes.iter().all(|(n, w)| n.dot(lc) < *w) {
                return true;
            }
        }
        let (lo, hi) = inst
            .to_local
            .transform_aabb(world_box.min.as_dvec3(), world_box.max.as_dvec3());
        let (min, max) = affine::round_out(lo, hi);
        let local_box = Aabb3 { min, max };
        let mut hit = false;
        mesh.bvh.query(
            |b| b.overlaps(&local_box),
            |tri| {
                if let Some(w) = self.world_triangle(inst, tri)
                    && cylinder::overlaps(c, r, h, &w)
                {
                    hit = true;
                    return false;
                }
                true
            },
        );
        hit
    }

    /// `true` when the upright cylinder at `center` overlaps (by more than
    /// the contact tolerance) a triangle of an accepted instance, or has its
    /// centre inside an accepted convex hull.
    #[must_use]
    pub fn overlaps_cylinder(
        &self,
        dynamic: &[Instance],
        center: Vec3,
        radius: f32,
        half_height: f32,
        filter: QueryFilter,
    ) -> bool {
        let c = center.as_dvec3();
        let tol = contact_tolerance(center);
        let r = f64::from(radius) - tol;
        let h = f64::from(half_height) - tol;
        if !(c.is_finite() && r > 0.0 && h > 0.0) {
            return false;
        }
        let e = DVec3::new(r, r, h);
        let (min, max) = affine::round_out(c - e, c + e);
        let world_box = Aabb3 { min, max };
        let mut hit = false;
        self.tlas.query(
            |b| b.overlaps(&world_box),
            |i| {
                if let Some(inst) = self.statics.get(i as usize)
                    && filter.accepts(&inst.info)
                    && inst.bounds.overlaps(&world_box)
                    && self.overlaps_instance(inst, c, r, h, &world_box)
                {
                    hit = true;
                    return false;
                }
                true
            },
        );
        hit || dynamic.iter().any(|inst| {
            filter.accepts(&inst.info)
                && inst.bounds.overlaps(&world_box)
                && self.overlaps_instance(inst, c, r, h, &world_box)
        })
    }
}

/// Swept point `s + t·d` (`t ∈ [0, limit]`) against the upright cylinder
/// `(r, h)` centred at `c` (the Minkowski sum of a swept player cylinder and
/// a pawn cylinder). Start contacts follow the triangle sweeps: a start
/// within `tol` of the surface is a contact at `t = 0` when the motion goes
/// into the surface normal; a start deeper than `tol` inside is a
/// penetrating contact along the least-penetration normal (side or cap),
/// blocking only motion into it.
fn cylinder_sweep(
    s: DVec3,
    d: DVec3,
    r: f64,
    h: f64,
    c: DVec3,
    tol: f64,
    limit: f64,
) -> Option<cylinder::Contact> {
    if !(s.is_finite() && d.is_finite() && c.is_finite() && r > 0.0 && h > 0.0) {
        return None;
    }
    let p = s - c;
    let pxy = DVec2::new(p.x, p.y);
    let dxy = DVec2::new(d.x, d.y);
    let dist = pxy.length();
    let radial = |q: DVec2| -> DVec3 {
        let l = q.length();
        if l > 1.0e-12 {
            DVec3::new(q.x / l, q.y / l, 0.0)
        } else {
            let m = dxy.length();
            if m > 1.0e-12 {
                DVec3::new(-dxy.x / m, -dxy.y / m, 0.0)
            } else {
                DVec3::X
            }
        }
    };
    let contact = |t: f64, normal: DVec3, penetrating: bool| cylinder::Contact {
        t,
        normal,
        penetrating,
    };
    // Starts inside (deeper than the tolerance).
    if dist < r - tol && p.z.abs() < h - tol {
        let side_depth = r - dist;
        let cap_depth = h - p.z.abs();
        let n = if side_depth < cap_depth {
            radial(pxy)
        } else if p.z > 0.0 || (p.z == 0.0 && d.z < 0.0) {
            DVec3::Z
        } else {
            DVec3::NEG_Z
        };
        return (d.dot(n) < 0.0).then(|| contact(0.0, n, true));
    }
    // Starts touching (within the tolerance).
    if dist <= r + tol && p.z.abs() <= h + tol {
        let n = if p.z.abs() >= h - tol && dist <= r - tol {
            if p.z > 0.0 { DVec3::Z } else { DVec3::NEG_Z }
        } else {
            radial(pxy)
        };
        if d.dot(n) < 0.0 {
            return Some(contact(0.0, n, false));
        }
        // Moving along or away: later entries only through the other
        // feature, handled below.
    }
    let mut best: Option<cylinder::Contact> = None;
    let mut consider = |t: f64, n: DVec3| {
        if (0.0..=limit).contains(&t) && best.is_none_or(|b| t < b.t) {
            best = Some(contact(t, n, false));
        }
    };
    // Side: |p_xy + t d_xy| = r, entering.
    let a = dxy.dot(dxy);
    let b = pxy.dot(dxy);
    if a > 0.0 && b < 0.0 && dist > r {
        let cq = pxy.dot(pxy) - r * r;
        let disc = b * b - a * cq;
        if disc >= 0.0 {
            let t = (-b - disc.sqrt()) / a;
            let z = p.z + d.z * t;
            if z.abs() <= h {
                consider(t, radial(pxy + dxy * t));
            }
        }
    }
    // Caps.
    if d.z < 0.0 && p.z >= h {
        let t = (h - p.z) / d.z;
        if (pxy + dxy * t).length() <= r {
            consider(t, DVec3::Z);
        }
    } else if d.z > 0.0 && p.z <= -h {
        let t = (-h - p.z) / d.z;
        if (pxy + dxy * t).length() <= r {
            consider(t, DVec3::NEG_Z);
        }
    }
    best
}

/// Segment `o + t·dv` (`t ∈ [0, 1]`) entering the upright cylinder `(r, h)`
/// centred at `c` from outside: `(t, normal)`.
fn cylinder_ray(o: DVec3, dv: DVec3, r: f64, h: f64, c: DVec3) -> Option<(f64, DVec3)> {
    let mut best: Option<(f64, DVec3)> = None;
    let mut consider = |t: f64, n: DVec3| {
        if (0.0..=1.0).contains(&t) && best.is_none_or(|b| t < b.0) {
            best = Some((t, n));
        }
    };
    let p = o - c;
    let pxy = DVec2::new(p.x, p.y);
    let dxy = DVec2::new(dv.x, dv.y);
    let a = dxy.dot(dxy);
    let b = pxy.dot(dxy);
    if a > 0.0 && b < 0.0 && pxy.length() > r {
        let disc = b * b - a * (pxy.dot(pxy) - r * r);
        if disc >= 0.0 {
            let t = (-b - disc.sqrt()) / a;
            let q = pxy + dxy * t;
            if (p.z + dv.z * t).abs() <= h && q.length() > 0.0 {
                let n = q / q.length();
                consider(t, DVec3::new(n.x, n.y, 0.0));
            }
        }
    }
    if dv.z < 0.0 && p.z >= h {
        let t = (h - p.z) / dv.z;
        if (pxy + dxy * t).length() <= r {
            consider(t, DVec3::Z);
        }
    } else if dv.z > 0.0 && p.z <= -h {
        let t = (-h - p.z) / dv.z;
        if (pxy + dxy * t).length() <= r {
            consider(t, DVec3::NEG_Z);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A unit cube `[0, 1]³` as 12 triangles.
    pub(crate) fn cube() -> (Vec<Vec3>, Vec<[u32; 3]>) {
        let v: Vec<Vec3> = (0..8)
            .map(|i| Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32))
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

    /// An NPC pawn's cylinder (radius 34, half height 78) blocks the player
    /// cylinder (21 × 44) exactly where their Minkowski sum begins, from the
    /// side and from above, moves with its instance, lets zero-extent traces
    /// through when it does not block them, and agrees with the brute-force
    /// queries.
    #[test]
    fn pawn_cylinders_block_the_player_exactly() {
        let mut scene = CollisionSceneBuilder::new().build();
        assert!(scene.add_cylinder_mesh(0.0, 10.0).is_none());
        assert!(scene.add_cylinder_mesh(f32::NAN, 10.0).is_none());
        let m = scene.add_cylinder_mesh(34.0, 78.0).unwrap();
        let mut pawn = info();
        pawn.actor = Some(9);
        pawn.blocks_traces = false;
        let mut inst = scene
            .dynamic_instance(
                m,
                Affine::from_translation(DVec3::new(500.0, 0.0, 78.0)),
                pawn,
            )
            .unwrap();
        let dynamic = vec![inst.clone()];
        let f = QueryFilter::PAWN;
        // From the side along +X at the pawn's height: contact at x = 500 − 55.
        let h = scene
            .sweep_cylinder(
                &dynamic,
                Vec3::new(0.0, 0.0, 78.0),
                Vec3::new(1000.0, 0.0, 78.0),
                21.0,
                44.0,
                f,
            )
            .unwrap();
        assert!((h.t - 0.445).abs() < 1e-9, "{}", h.t);
        assert!((h.normal - DVec3::NEG_X).length() < 1e-9);
        assert_eq!(h.instance, InstanceRef::Dynamic(0));
        assert!(!h.penetrating);
        let brute = scene
            .sweep_cylinder_brute_force(
                &dynamic,
                Vec3::new(0.0, 0.0, 78.0),
                Vec3::new(1000.0, 0.0, 78.0),
                21.0,
                44.0,
                f,
            )
            .unwrap();
        assert_eq!(brute, h);
        // Passing beside it (|y| > 55) touches nothing.
        assert!(
            scene
                .sweep_cylinder(
                    &dynamic,
                    Vec3::new(0.0, 60.0, 78.0),
                    Vec3::new(1000.0, 60.0, 78.0),
                    21.0,
                    44.0,
                    f
                )
                .is_none()
        );
        // Falling onto its top (z = 156 + 44).
        let h = scene
            .sweep_cylinder(
                &dynamic,
                Vec3::new(500.0, 10.0, 400.0),
                Vec3::new(500.0, 10.0, 0.0),
                21.0,
                44.0,
                f,
            )
            .unwrap();
        assert!((h.t - 0.5).abs() < 1e-9 && h.normal == DVec3::Z);
        // Resting on top: moving down is blocked at once, sideways is free
        // until the edge.
        let top = Vec3::new(500.0, 0.0, 200.0);
        let h = scene
            .sweep_cylinder(&dynamic, top, top - Vec3::Z * 10.0, 21.0, 44.0, f)
            .unwrap();
        assert_eq!(h.t, 0.0);
        assert!(
            scene
                .sweep_cylinder(&dynamic, top, top + Vec3::X * 20.0, 21.0, 44.0, f)
                .is_none()
        );
        // Starting inside: only motion deeper in is blocked.
        let inside = Vec3::new(470.0, 0.0, 78.0);
        let h = scene
            .sweep_cylinder(&dynamic, inside, inside + Vec3::X * 10.0, 21.0, 44.0, f)
            .unwrap();
        assert!(h.penetrating && h.t == 0.0);
        assert!(
            scene
                .sweep_cylinder(&dynamic, inside, inside - Vec3::X * 10.0, 21.0, 44.0, f)
                .is_none()
        );
        assert!(scene.overlaps_cylinder(&dynamic, inside, 21.0, 44.0, f));
        assert!(!scene.overlaps_cylinder(&dynamic, Vec3::new(400.0, 0.0, 78.0), 21.0, 44.0, f));
        // Traces pass (block-all-but-weapons) unless the instance blocks them.
        let o = Vec3::new(0.0, 0.0, 78.0);
        assert!(
            scene
                .raycast(&dynamic, o, o + Vec3::X * 1000.0, QueryFilter::TRACE)
                .is_none()
        );
        let mut tracing = dynamic.clone();
        tracing[0].info.blocks_traces = true;
        let r = scene
            .raycast(&tracing, o, o + Vec3::X * 1000.0, QueryFilter::TRACE)
            .unwrap();
        assert!((r.t - 0.466).abs() < 1e-9);
        assert_eq!(
            scene.raycast_brute_force(&tracing, o, o + Vec3::X * 1000.0, QueryFilter::TRACE),
            Some(r)
        );
        // Moving the instance moves the obstacle.
        assert!(scene.place_dynamic(
            &mut inst,
            Affine::from_translation(DVec3::new(800.0, 0.0, 78.0))
        ));
        let h = scene
            .sweep_cylinder(
                &[inst],
                Vec3::new(0.0, 0.0, 78.0),
                Vec3::new(1000.0, 0.0, 78.0),
                21.0,
                44.0,
                f,
            )
            .unwrap();
        assert!((h.t - 0.745).abs() < 1e-9);
    }

    /// `cylinder_sweep` against an independent stepped march of the same
    /// path through the Minkowski cylinder: every path the march finds
    /// entering is hit at the entry (to the march's resolution), on the
    /// surface, with that surface's outward normal opposing the motion; a
    /// path the sweep misses never enters.
    #[test]
    fn cylinder_sweeps_agree_with_a_stepped_march() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let (r, h) = (55.0f64, 122.0f64);
        let c = DVec3::new(10.0, -20.0, 30.0);
        let tol = 1.0e-3;
        let inside = |p: DVec3, margin: f64| {
            let q = p - c;
            DVec2::new(q.x, q.y).length() < r - margin && q.z.abs() < h - margin
        };
        let steps = 4000usize;
        let (mut hits, mut misses, mut side, mut cap) = (0usize, 0usize, 0usize, 0usize);
        for _ in 0..6000 {
            let mut point = |sx: f64, sz: f64| {
                c + DVec3::new((rnd() - 0.5) * sx, (rnd() - 0.5) * sx, (rnd() - 0.5) * sz)
            };
            let s = point(400.0, 600.0);
            let e = point(400.0, 600.0);
            // Start contacts have their own rules (tested with the pawn
            // cylinder above and below).
            if inside(s, -2.0 * tol) {
                continue;
            }
            let d = e - s;
            let got = cylinder_sweep(s, d, r, h, c, tol, 1.0);
            let first = (1..=steps)
                .map(|i| i as f64 / steps as f64)
                .find(|t| inside(s + d * *t, 1.0e-9));
            let on_surface = |t: f64| {
                let q = s + d * t - c;
                let dist = DVec2::new(q.x, q.y).length();
                let on_side = (dist - r).abs() < 1.0e-6 && q.z.abs() <= h + 1.0e-6;
                let on_cap = (q.z.abs() - h).abs() < 1.0e-6 && dist <= r + 1.0e-6;
                (on_side, on_cap, q)
            };
            match (got, first) {
                (Some(hit), found) => {
                    assert!(!hit.penetrating && (0.0..=1.0).contains(&hit.t));
                    if let Some(t) = found {
                        // The entry lies in the march step before `t`.
                        assert!(
                            hit.t <= t + 1.0e-12 && hit.t >= t - 1.0 / steps as f64 - 1.0e-9,
                            "sweep {} march {t}",
                            hit.t
                        );
                    }
                    let (on_side, on_cap, q) = on_surface(hit.t);
                    assert!(on_side || on_cap, "contact off the surface: {q:?}");
                    assert!(d.dot(hit.normal) < 0.0, "normal along the motion");
                    assert!((hit.normal.length() - 1.0).abs() < 1.0e-9);
                    if on_cap && !on_side {
                        assert_eq!(hit.normal, DVec3::Z * q.z.signum());
                        cap += 1;
                    } else if on_side && !on_cap {
                        let n = DVec3::new(q.x, q.y, 0.0).normalize();
                        assert!((hit.normal - n).length() < 1.0e-6);
                        side += 1;
                    }
                    hits += 1;
                }
                (None, None) => misses += 1,
                (None, Some(t)) => panic!("the sweep missed an entry at {t}: {s:?} + {d:?}"),
            }
        }
        assert!(hits > 500 && misses > 500, "{hits} hits, {misses} misses");
        assert!(side > 100 && cap > 100, "{side} side, {cap} cap");
    }

    /// Start contacts of the cylinder sweep at the rim, on the cap and with
    /// hostile values: resting anywhere on the top blocks downward motion,
    /// sliding off the edge is free, a start inside blocks only motion
    /// deeper in, and non-finite input hits nothing.
    #[test]
    fn cylinder_sweep_start_contacts() {
        let (r, h, tol) = (55.0f64, 122.0f64, 1.0e-3f64);
        let c = DVec3::ZERO;
        let down = DVec3::new(0.0, 0.0, -10.0);
        // On the cap, from the axis out to the rim (inside the radius by
        // less than the tolerance included).
        for x in [0.0, 20.0, r - 1.0, r - tol * 0.5, r] {
            let hit = cylinder_sweep(DVec3::new(x, 0.0, h), down, r, h, c, tol, 1.0)
                .unwrap_or_else(|| panic!("falls through the cap at x = {x}"));
            assert_eq!(hit.t, 0.0, "x = {x}");
            assert_eq!(hit.normal, DVec3::Z, "x = {x}");
            assert!(!hit.penetrating);
        }
        // Just past the rim there is nothing below.
        assert!(cylinder_sweep(DVec3::new(r + 0.01, 0.0, h), down, r, h, c, tol, 1.0).is_none());
        // Under the bottom cap, moving up.
        let hit = cylinder_sweep(DVec3::new(3.0, 4.0, -h), -down, r, h, c, tol, 1.0).unwrap();
        assert_eq!((hit.t, hit.normal), (0.0, DVec3::NEG_Z));
        // Resting on the cap, sideways and upward motion is free.
        for d in [DVec3::X * 30.0, DVec3::Z * 5.0, DVec3::new(-7.0, 9.0, 0.0)] {
            assert!(cylinder_sweep(DVec3::new(5.0, 5.0, h), d, r, h, c, tol, 1.0).is_none());
        }
        // Touching the side: inward is blocked at once, along and away free.
        let at_side = DVec3::new(r, 0.0, 10.0);
        let hit = cylinder_sweep(at_side, DVec3::NEG_X * 3.0, r, h, c, tol, 1.0).unwrap();
        assert_eq!((hit.t, hit.normal), (0.0, DVec3::X));
        assert!(cylinder_sweep(at_side, DVec3::Y * 3.0, r, h, c, tol, 1.0).is_none());
        assert!(cylinder_sweep(at_side, DVec3::X * 3.0, r, h, c, tol, 1.0).is_none());
        assert!(cylinder_sweep(at_side, down, r, h, c, tol, 1.0).is_none());
        // Inside: the least-penetration face blocks motion into it only.
        let near_side = DVec3::new(r - 2.0, 0.0, 0.0);
        let hit = cylinder_sweep(near_side, DVec3::NEG_X, r, h, c, tol, 1.0).unwrap();
        assert!(hit.penetrating && hit.t == 0.0 && hit.normal == DVec3::X);
        assert!(cylinder_sweep(near_side, DVec3::X, r, h, c, tol, 1.0).is_none());
        let near_top = DVec3::new(0.0, 0.0, h - 2.0);
        let hit = cylinder_sweep(near_top, down, r, h, c, tol, 1.0).unwrap();
        assert!(hit.penetrating && hit.normal == DVec3::Z);
        assert!(cylinder_sweep(near_top, -down, r, h, c, tol, 1.0).is_none());
        // On the axis the radial normal falls back to the motion.
        let hit =
            cylinder_sweep(DVec3::new(0.0, 0.0, 0.0), DVec3::Y, 1.0, 100.0, c, tol, 1.0).unwrap();
        assert!(hit.penetrating && hit.normal == DVec3::NEG_Y);
        // A hit beyond the limit is not reported.
        let far = DVec3::new(-500.0, 0.0, 0.0);
        let hit = cylinder_sweep(far, DVec3::X * 1000.0, r, h, c, tol, 1.0).unwrap();
        assert!((hit.t - 0.445).abs() < 1.0e-12);
        assert!(cylinder_sweep(far, DVec3::X * 1000.0, r, h, c, tol, 0.4).is_none());
        // Hostile values.
        for bad in [f64::NAN, f64::INFINITY] {
            assert!(cylinder_sweep(DVec3::splat(bad), down, r, h, c, tol, 1.0).is_none());
            assert!(cylinder_sweep(far, DVec3::splat(bad), r, h, c, tol, 1.0).is_none());
            assert!(cylinder_sweep(far, down, r, h, DVec3::splat(bad), tol, 1.0).is_none());
            // A non-finite size never panics (`add_cylinder_mesh` refuses
            // one before it gets here).
            let _ = cylinder_sweep(far, DVec3::X * 1000.0, bad, h, c, tol, 1.0);
            assert!(cylinder_ray(DVec3::splat(bad), down, r, h, c).is_none());
        }
        assert!(cylinder_sweep(far, DVec3::X * 1000.0, -1.0, h, c, tol, 1.0).is_none());
        assert!(cylinder_sweep(far, DVec3::ZERO, r, h, c, tol, 1.0).is_none());
    }

    #[test]
    fn taking_actor_statics_turns_them_into_dynamic_instances() {
        let mut b = CollisionSceneBuilder::new();
        let (v, t) = cube();
        let m = b.add_mesh(v, t).unwrap();
        let floor = Affine {
            rows: [DVec3::X * 200.0, DVec3::Y * 200.0, DVec3::Z * 200.0],
            translation: DVec3::new(-100.0, -100.0, -200.0),
        };
        b.add_static(m, floor, info()).unwrap();
        let mut mover = info();
        mover.actor = Some(7);
        mover.class = CollisionClass::InterpActor;
        b.add_static(
            m,
            floor.then(&Affine::from_translation(DVec3::new(0.0, 0.0, 1000.0))),
            mover,
        )
        .unwrap();
        let mut s = b.build();
        let none = s.take_actor_statics(&std::collections::BTreeSet::new());
        assert!(none.is_empty());
        let taken = s.take_actor_statics(&std::collections::BTreeSet::from([7]));
        assert_eq!(taken.len(), 1);
        assert_eq!(s.statics().len(), 1);
        let ray = |s: &CollisionScene, dynamic: &[Instance]| {
            s.raycast(
                dynamic,
                Vec3::new(0.0, 0.0, 2000.0),
                Vec3::new(0.0, 0.0, 500.0),
                QueryFilter::TRACE,
            )
        };
        // Gone from the static set; found again as a dynamic instance.
        assert!(ray(&s, &[]).is_none());
        let mut dynamic = vec![
            s.dynamic_instance(taken[0].mesh, taken[0].to_world, taken[0].info)
                .unwrap(),
        ];
        assert_eq!(ray(&s, &dynamic).unwrap().instance, InstanceRef::Dynamic(0));
        // Moving it below the ray's end clears the hit.
        let low = taken[0]
            .to_world
            .then(&Affine::from_translation(DVec3::new(0.0, 0.0, -900.0)));
        assert!(s.place_dynamic(&mut dynamic[0], low));
        assert!(ray(&s, &dynamic).is_none());
    }

    #[test]
    fn instanced_cube_blocks_and_flags_filter() {
        let mut b = CollisionSceneBuilder::new();
        let (v, t) = cube();
        let m = b.add_mesh(v, t).unwrap();
        // Scale the cube to 200³ and move it so its top is at z = 0.
        let to_world = Affine {
            rows: [DVec3::X * 200.0, DVec3::Y * 200.0, DVec3::Z * 200.0],
            translation: DVec3::new(-100.0, -100.0, -200.0),
        };
        b.add_static(m, to_world, info()).unwrap();
        let mut ghost = info();
        ghost.blocks_pawn = false;
        ghost.sublevel = 1;
        b.add_static(
            m,
            Affine::from_translation(DVec3::new(0.0, 0.0, 500.0)),
            ghost,
        )
        .unwrap();
        let s = b.build();
        assert_eq!(s.stats().instances, 2);
        assert_eq!(s.stats().instance_triangles, 24);
        let hit = s
            .sweep_cylinder(
                &[],
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, -100.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((hit.t - 56.0 / 200.0).abs() < 1e-9, "{hit:?}");
        assert_eq!(hit.normal, DVec3::Z);
        assert_eq!(hit.instance, InstanceRef::Static(0));
        // The second cube does not block pawns but blocks traces of loaded sub-levels.
        let up = s.raycast(
            &[],
            Vec3::new(0.5, 0.5, 400.0),
            Vec3::new(0.5, 0.5, 600.0),
            QueryFilter::TRACE,
        );
        assert_eq!(up.unwrap().instance, InstanceRef::Static(1));
        let unloaded = QueryFilter {
            kind: QueryKind::Trace,
            sublevels: 1,
        };
        assert!(
            s.raycast(
                &[],
                Vec3::new(0.5, 0.5, 400.0),
                Vec3::new(0.5, 0.5, 600.0),
                unloaded
            )
            .is_none()
        );
        assert!(s.overlaps_cylinder(
            &[],
            Vec3::new(0.0, 0.0, 40.0),
            21.0,
            44.0,
            QueryFilter::PAWN
        ));
        assert!(!s.overlaps_cylinder(
            &[],
            Vec3::new(0.0, 0.0, 44.5),
            21.0,
            44.0,
            QueryFilter::PAWN
        ));
    }

    #[test]
    fn convex_hull_is_solid_from_outside_and_escapable_from_inside() {
        let mut b = CollisionSceneBuilder::new();
        let (v, t) = cube();
        let v: Vec<Vec3> = v
            .into_iter()
            .map(|p| p * 400.0 - Vec3::splat(200.0))
            .collect();
        let m = b.add_convex_mesh(v, t).unwrap();
        b.add_static(m, Affine::IDENTITY, info()).unwrap();
        let s = b.build();
        // From outside: blocked by the +X face.
        let hit = s
            .sweep_cylinder(
                &[],
                Vec3::new(500.0, 0.0, 0.0),
                Vec3::new(0.0, 0.0, 0.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((hit.normal - DVec3::X).length() < 1e-12);
        // From inside: free to leave; the centre counts as overlapping.
        assert!(
            s.sweep_cylinder(
                &[],
                Vec3::ZERO,
                Vec3::new(500.0, 0.0, 0.0),
                21.0,
                44.0,
                QueryFilter::PAWN
            )
            .is_none()
        );
        assert!(s.overlaps_cylinder(&[], Vec3::ZERO, 21.0, 44.0, QueryFilter::PAWN));
    }

    #[test]
    fn dynamic_instances_and_zero_scale_baking() {
        let mut b = CollisionSceneBuilder::new();
        let (v, t) = cube();
        let m = b.add_mesh(v, t).unwrap();
        // Zero Z scale: baked into a flat world-space mesh.
        let flat = Affine {
            rows: [DVec3::X * 100.0, DVec3::Y * 100.0, DVec3::ZERO],
            translation: DVec3::new(-50.0, -50.0, 0.0),
        };
        let i = b.add_static(m, flat, info()).unwrap();
        let s = b.build();
        assert_eq!(s.statics()[i as usize].to_world, Affine::IDENTITY);
        assert_eq!(s.meshes().len(), 2);
        let hit = s
            .sweep_cylinder(
                &[],
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, 0.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((hit.t - 0.56).abs() < 1e-9);
        // A dynamic copy of the cube floating above.
        let mut dynamic = vec![
            s.dynamic_instance(
                m,
                Affine {
                    rows: [DVec3::X * 50.0, DVec3::Y * 50.0, DVec3::Z * 50.0],
                    translation: DVec3::new(-25.0, -25.0, 200.0),
                },
                info(),
            )
            .unwrap(),
        ];
        let hit = s
            .sweep_cylinder(
                &dynamic,
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, 400.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert_eq!(hit.instance, InstanceRef::Dynamic(0));
        assert!((hit.t - 56.0 / 300.0).abs() < 1e-9);
        // Move it away.
        let moved = Affine::from_translation(DVec3::new(1000.0, 0.0, 200.0));
        let mut inst = dynamic[0].clone();
        assert!(s.place_dynamic(&mut inst, moved));
        dynamic[0] = inst;
        assert!(
            s.sweep_cylinder(
                &dynamic,
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, 400.0),
                21.0,
                44.0,
                QueryFilter::PAWN
            )
            .is_none()
        );
        assert!(!s.place_dynamic(&mut dynamic[0], flat));
    }

    #[test]
    fn contact_tolerance_grows_with_coordinates() {
        assert!((contact_tolerance(Vec3::ZERO) - 1.0e-3).abs() < 1e-12);
        let far = contact_tolerance(Vec3::new(250_000.0, 0.0, 0.0));
        assert!(far > 0.03 && far < 0.04, "{far}");
    }
}
