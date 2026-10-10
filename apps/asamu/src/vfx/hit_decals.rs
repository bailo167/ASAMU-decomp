//! Decals spawned at run time through the engine's decal manager: the
//! grapple's hit decal and the hard-landing decal. Pure model: the
//! manager's pool and lifetime rules, the spawn parameters, and the decal
//! frame, projected onto the world by rays (`crate::vfx` meshes it).
//!
//! From local reading of `GrappleGun.ProcessInstantHit`,
//! `ASAMUPawn.HardLanding` and the stock `Engine.DecalManager`
//! (VFX_DECALS.md §6).

use asamu_core::glam::Vec3 as UeVec3;

/// `DecalManager.MaxActiveDecals`, set by the gun to `fHitDecalLimit` (5).
/// CONFIRMED (cdo, src).
pub const MAX_ACTIVE_DECALS: usize = 5;
/// `DecalManager.DecalLifeSpan`: `[Engine.DecalManager] DecalLifeSpan=30.0`
/// in `DefaultGame.ini` (over `BaseGame.ini`'s 10.0); neither spawn passes
/// its own lifetime. CONFIRMED (config, src).
pub const DECAL_LIFESPAN: f32 = 30.0;
/// `GrappleGun.DecalWidth` / `DecalHeight`, UU. CONFIRMED (cdo).
pub const GRAPPLE_DECAL_SIZE: f32 = 80.0;
/// Thickness passed by the gun (literal 50; the manager makes it
/// `FarPlane = 25`, `NearPlane = −25`). CONFIRMED (src).
pub const GRAPPLE_DECAL_THICKNESS: f32 = 50.0;
/// `ASAMUPawn.hardLandDecalSize`, UU. CONFIRMED (cdo).
pub const HARD_LAND_DECAL_SIZE: f32 = 200.0;
/// `ASAMUPawn.hardLandDecalDepth` (thickness), UU. CONFIRMED (cdo).
pub const HARD_LAND_DECAL_DEPTH: f32 = 100.0;
/// The gun's decal material instance parent (`decalInstanceParent`).
/// CONFIRMED (cdo).
pub const GRAPPLE_DECAL_MATERIAL: &str = "Decals.GrappleDecal_inst";
/// `ASAMUPawn.hardLandDecal`. CONFIRMED (cdo).
pub const HARD_LAND_DECAL_MATERIAL: &str = "Shared_Materials.HardLandDecalMaterial";

/// The decal manager's active list (`ActiveDecals`) with the pool rule:
/// a spawn reuses a pooled component, else creates one; when nothing is
/// pooled and `MaxActiveDecals` are active the oldest is expired first (its
/// component is reset and dropped). Components return to the pool when
/// their lifetime runs out. A pooled component is reused before anything is
/// evicted and every expiry frees an active slot, so active plus pooled
/// never exceeds the maximum: at most [`MAX_ACTIVE_DECALS`] are ever
/// active.
#[derive(Clone, Debug, PartialEq)]
pub struct DecalPool<T> {
    active: Vec<(T, f32)>,
    pooled: usize,
    max: usize,
    lifespan: f32,
}

impl<T> Default for DecalPool<T> {
    fn default() -> Self {
        Self::new(MAX_ACTIVE_DECALS, DECAL_LIFESPAN)
    }
}

impl<T> DecalPool<T> {
    /// A pool for `max` active decals living `lifespan` seconds.
    #[must_use]
    pub fn new(max: usize, lifespan: f32) -> Self {
        Self {
            active: Vec::new(),
            pooled: 0,
            max: max.max(1),
            lifespan,
        }
    }

    /// Adds `item`; returns the expired oldest item when one had to make
    /// room.
    pub fn spawn(&mut self, item: T) -> Option<T> {
        let mut evicted = None;
        if self.pooled > 0 {
            self.pooled -= 1;
        } else if self.active.len() >= self.max {
            evicted = Some(self.active.remove(0).0);
        }
        self.active.push((item, self.lifespan));
        evicted
    }

    /// Advances lifetimes by `dt`; returns the items whose lifetime ran out
    /// (their components go back to the pool).
    pub fn tick(&mut self, dt: f32) -> Vec<T> {
        if !dt.is_finite() || dt <= 0.0 {
            return Vec::new();
        }
        let mut done = Vec::new();
        let mut keep = Vec::with_capacity(self.active.len());
        for (item, left) in self.active.drain(..) {
            let left = left - dt;
            if left <= 0.0 {
                done.push(item);
            } else {
                keep.push((item, left));
            }
        }
        self.active = keep;
        self.pooled += done.len();
        done
    }

    /// Active items, oldest first, with their remaining lifetime.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active(&self) -> impl Iterator<Item = (&T, f32)> {
        self.active.iter().map(|(t, l)| (t, *l))
    }

    /// Number of active decals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// No active decal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Removes everything (a new game).
    pub fn clear(&mut self) -> Vec<T> {
        self.pooled = 0;
        self.active.drain(..).map(|(t, _)| t).collect()
    }
}

/// A decal spawn (`DecalManager.SpawnDecal` arguments that matter here).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecalSpawn {
    /// `DecalLocation`.
    pub location: UeVec3,
    /// The surface normal (the orientation is `rotator(−HitNormal)`, so the
    /// projection direction is `−normal`).
    pub normal: UeVec3,
    /// `Width`.
    pub width: f32,
    /// `Height`.
    pub height: f32,
    /// `Thickness` (near/far planes at ∓thickness/2).
    pub thickness: f32,
    /// `DecalRotation`, degrees (both callers pass `FRand() · 360`).
    pub rotation_degrees: f32,
}

impl DecalSpawn {
    /// The grapple's hit decal (`ProcessInstantHit`), skipped by the caller
    /// for `ASAMUDoesNotAcceptGrappleDecal` targets.
    #[must_use]
    pub fn grapple(hit: UeVec3, normal: UeVec3, random01: f32) -> Self {
        Self {
            location: hit,
            normal,
            width: GRAPPLE_DECAL_SIZE,
            height: GRAPPLE_DECAL_SIZE,
            thickness: GRAPPLE_DECAL_THICKNESS,
            rotation_degrees: random01 * 360.0,
        }
    }

    /// The hard-landing decal (`HardLanding`): at the pawn's location minus
    /// half its collision half-height (the script divides
    /// `GetCollisionHeight()`, already a half-height, by two again: a
    /// quirk; the 100 UU thickness still reaches the floor).
    #[must_use]
    pub fn hard_landing(
        pawn: UeVec3,
        collision_half_height: f32,
        floor_normal: UeVec3,
        random01: f32,
    ) -> Self {
        Self {
            location: pawn - UeVec3::Z * (collision_half_height / 2.0),
            normal: floor_normal,
            width: HARD_LAND_DECAL_SIZE,
            height: HARD_LAND_DECAL_SIZE,
            thickness: HARD_LAND_DECAL_DEPTH,
            rotation_degrees: random01 * 360.0,
        }
    }

    /// The decal frame: origin, projection direction `D` (`−normal`), width
    /// axis `W` and height axis `H` of `rotator(D)` turned by the decal
    /// rotation (the same construction as `asamu_ue3::decal::DecalFrame`).
    #[must_use]
    pub fn frame(&self) -> Frame {
        let d = (-self.normal).normalize_or_zero();
        let d = if d == UeVec3::ZERO { -UeVec3::Z } else { d };
        // Axes of rotator(d) (roll 0): Y = (−sin yaw, cos yaw, 0),
        // Z = X × Y in UE3's left-handed rows (−sin p cos y, −sin p sin y, cos p).
        let yaw = d.y.atan2(d.x);
        let pitch = d.z.atan2((d.x * d.x + d.y * d.y).sqrt());
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let y = UeVec3::new(-sy, cy, 0.0);
        let z = UeVec3::new(-sp * cy, -sp * sy, cp);
        let (s, c) = self.rotation_degrees.to_radians().sin_cos();
        Frame {
            origin: self.location,
            direction: d,
            width_axis: (y * c + z * s).normalize_or_zero(),
            height_axis: (z * c - y * s).normalize_or_zero(),
        }
    }
}

/// A decal frame in UE space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Origin.
    pub origin: UeVec3,
    /// Projection direction.
    pub direction: UeVec3,
    /// Width axis.
    pub width_axis: UeVec3,
    /// Height axis.
    pub height_axis: UeVec3,
}

impl Frame {
    /// Decal texture coordinates of `p`: `u` along `+W / width`, `v` along
    /// `−H / height`, the box centre at 0.5 (the native decal matrix and
    /// the shipped decal vertex shaders, VFX_DECALS.md §8.4).
    #[must_use]
    pub fn uv(&self, p: UeVec3, width: f32, height: f32) -> [f32; 2] {
        let d = p - self.origin;
        let a = d.dot(self.width_axis);
        let b = d.dot(self.height_axis);
        let u = if width.abs() > 1e-6 { a / width } else { 0.0 };
        let v = if height.abs() > 1e-6 {
            -b / height
        } else {
            0.0
        };
        [u + 0.5, v + 0.5]
    }
}

/// Grid cells per side of a ray-projected decal (ours: a render choice;
/// the original clips the receiver's triangles, which the converted
/// collision cannot enumerate).
pub const GRID: usize = 8;
/// Distance the decal is lifted off the surface towards the viewer, UU
/// (ours: against z-fighting; the original uses a depth bias).
pub const LIFT_UU: f32 = 0.3;

/// A projected decal mesh in UE space.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DecalMesh {
    /// Positions, UU.
    pub positions: Vec<UeVec3>,
    /// Normals (towards the viewer).
    pub normals: Vec<UeVec3>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangles (indices into `positions`).
    pub indices: Vec<u32>,
}

/// Projects `spawn` onto the world: a `(GRID + 1)²` lattice of rays along
/// the projection direction, each from the near plane to the far plane
/// (`raycast(origin, direction, length) -> (point, normal)`). A sample is
/// kept when it hits a surface facing the decal (the original skips
/// back faces); a cell becomes two triangles when its four corners were
/// kept and lie within a thickness of each other.
pub fn project(
    spawn: &DecalSpawn,
    mut raycast: impl FnMut(UeVec3, UeVec3, f32) -> Option<(UeVec3, UeVec3)>,
) -> DecalMesh {
    let f = spawn.frame();
    let half_t = spawn.thickness * 0.5;
    let n = GRID + 1;
    let mut samples: Vec<Option<(UeVec3, UeVec3)>> = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let a = (i as f32 / GRID as f32 - 0.5) * spawn.width;
            let b = (j as f32 / GRID as f32 - 0.5) * spawn.height;
            let start = f.origin + f.width_axis * a + f.height_axis * b - f.direction * half_t;
            let hit = raycast(start, f.direction, spawn.thickness)
                .filter(|(p, nrm)| p.is_finite() && nrm.dot(f.direction) < 0.0);
            samples.push(hit.map(|(p, nrm)| (p - f.direction * LIFT_UU, nrm)));
        }
    }
    let mut mesh = DecalMesh::default();
    let mut index_of = vec![None; n * n];
    for (k, s) in samples.iter().enumerate() {
        if let Some((p, nrm)) = s
            && let Ok(idx) = u32::try_from(mesh.positions.len())
        {
            index_of[k] = Some(idx);
            mesh.positions.push(*p);
            mesh.normals.push(*nrm);
            mesh.uvs.push(f.uv(*p, spawn.width, spawn.height));
        }
    }
    let max_gap = spawn.thickness.max(1.0);
    for j in 0..GRID {
        for i in 0..GRID {
            let k = [
                j * n + i,
                j * n + i + 1,
                (j + 1) * n + i,
                (j + 1) * n + i + 1,
            ];
            let (Some(a), Some(b), Some(c), Some(d)) = (
                index_of[k[0]],
                index_of[k[1]],
                index_of[k[2]],
                index_of[k[3]],
            ) else {
                continue;
            };
            let p = |x: u32| mesh.positions[x as usize];
            let close = [b, c, d]
                .iter()
                .all(|&x| p(x).distance(p(a)) <= max_gap * 2.0);
            if close {
                mesh.indices.extend_from_slice(&[a, b, d, a, d, c]);
            }
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_evicts_the_oldest_beyond_the_limit() {
        let mut p: DecalPool<u32> = DecalPool::default();
        for i in 0..5 {
            assert_eq!(p.spawn(i), None);
        }
        assert_eq!(p.spawn(5), Some(0));
        assert_eq!(p.len(), 5);
        assert_eq!(p.active().next().map(|(t, _)| *t), Some(1));
    }

    #[test]
    fn pool_lifetimes_and_reuse() {
        let mut p: DecalPool<u32> = DecalPool::default();
        p.spawn(1);
        assert!(p.tick(29.9).is_empty());
        p.spawn(2);
        assert_eq!(p.tick(0.2), vec![1]);
        // A pooled component is reused without evicting.
        for i in 3..7 {
            assert_eq!(p.spawn(i), None);
        }
        assert_eq!(p.len(), 5);
        assert_eq!(p.spawn(7), Some(2));
        assert_eq!(p.clear().len(), 5);
        assert!(p.is_empty());
        assert!(p.tick(f32::NAN).is_empty());
    }

    #[test]
    fn frame_projects_into_the_surface() {
        let s = DecalSpawn::grapple(UeVec3::ZERO, UeVec3::Z, 0.0);
        let f = s.frame();
        assert!((f.direction - (-UeVec3::Z)).length() < 1e-6);
        // Orthonormal basis.
        assert!(f.width_axis.dot(f.height_axis).abs() < 1e-6);
        assert!(f.width_axis.dot(f.direction).abs() < 1e-6);
        assert!((f.width_axis.length() - 1.0).abs() < 1e-6);
        // The decal rotation turns W towards H.
        let r = DecalSpawn::grapple(UeVec3::ZERO, UeVec3::Z, 0.25).frame();
        assert!((r.width_axis - f.height_axis).length() < 1e-5);
        // UV: centre 0.5, the width edge 0 or 1.
        assert_eq!(f.uv(UeVec3::ZERO, 80.0, 80.0), [0.5, 0.5]);
        let e = f.uv(f.width_axis * 40.0, 80.0, 80.0);
        assert!((e[0] - 1.0).abs() < 1e-6 && (e[1] - 0.5).abs() < 1e-6);
        let t = f.uv(f.height_axis * 40.0, 80.0, 80.0);
        assert!((t[0] - 0.5).abs() < 1e-6 && t[1].abs() < 1e-6);
        // Degenerate normal: straight down.
        let z = DecalSpawn::grapple(UeVec3::ZERO, UeVec3::ZERO, 0.0).frame();
        assert_eq!(z.direction, -UeVec3::Z);
    }

    #[test]
    fn hard_landing_spawn_values() {
        let s = DecalSpawn::hard_landing(UeVec3::new(0.0, 0.0, 100.0), 44.0, UeVec3::Z, 0.5);
        assert_eq!(s.location, UeVec3::new(0.0, 0.0, 78.0));
        assert_eq!((s.width, s.height, s.thickness), (200.0, 200.0, 100.0));
        assert_eq!(s.rotation_degrees, 180.0);
    }

    #[test]
    fn projection_onto_a_floor_and_an_edge() {
        let s = DecalSpawn::grapple(UeVec3::new(0.0, 0.0, 10.0), UeVec3::Z, 0.0);
        // A floor at z = 0 within reach (the decal sits 10 above it, the
        // thickness reaches 25 below the origin).
        let floor = |o: UeVec3, d: UeVec3, len: f32| {
            let t = -o.z / d.z;
            (t >= 0.0 && t <= len).then(|| (o + d * t, UeVec3::Z))
        };
        let m = project(&s, floor);
        assert_eq!(m.positions.len(), (GRID + 1) * (GRID + 1));
        assert_eq!(m.indices.len(), GRID * GRID * 6);
        assert!(m.positions.iter().all(|p| (p.z - LIFT_UU).abs() < 1e-4));
        assert!(
            m.uvs
                .iter()
                .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]))
        );
        // Only half the floor exists (x > 0): about half the cells.
        let half = |o: UeVec3, d: UeVec3, len: f32| floor(o, d, len).filter(|(p, _)| p.x > 0.0);
        let m2 = project(&s, half);
        assert!(m2.indices.len() < m.indices.len() && !m2.indices.is_empty());
        // A back face is not decorated.
        let back = |o: UeVec3, d: UeVec3, len: f32| floor(o, d, len).map(|(p, _)| (p, -UeVec3::Z));
        assert!(project(&s, back).positions.is_empty());
        // Out of reach (floor 30 below the origin): nothing.
        let s_far = DecalSpawn::grapple(UeVec3::new(0.0, 0.0, 30.0), UeVec3::Z, 0.0);
        assert!(project(&s_far, floor).positions.is_empty());
    }
}
