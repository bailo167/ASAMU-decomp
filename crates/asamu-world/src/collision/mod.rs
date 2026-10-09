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

use glam::{DVec3, Vec3};
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
