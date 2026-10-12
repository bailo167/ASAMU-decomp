//! Synthetic converted-data fixtures for tests (no game data).
//!
//! Writes, into a [`MemorySource`], files in the layout and format that
//! `asamu-import levels` and `asamu-import meshes --collision` produce
//! (`levels/<map>.scene.json`, `.bsp.json`/`.bsp.bin`, `meshes/manifest.json`
//! and glTF files), built entirely from geometry chosen by the test. Used by
//! the loader and gameplay tests here and in `asamu-game`.
//!
//! # Two manifest versions
//!
//! [`MeshFixtures::write`] writes a mesh manifest of **version 1**, the
//! format before the simple collision was converted: the loader then
//! collides the pawn with every mesh's triangles and says in a warning that
//! this is approximate. [`MeshFixtures::write_v2`] writes **version 2**, as
//! the importer does today: every mesh has its switches, and its simple
//! body when one was given ([`MeshFixtures::add_convex`],
//! [`MeshFixtures::add_solid_box`], ...). As in the original, a version 2
//! mesh that was given no body does not stop the pawn at all
//! (`docs/PARITY_FINDINGS.md` P1), so a test world built for version 2 gives
//! its floors and walls a body or stores their box switch `false`
//! ([`MeshFixtures::set_switches`]).

use std::collections::BTreeMap;

use glam::Vec3;
pub use serde_json::{Value, json};

use crate::rotation::actor_local_to_world;
use crate::scene::MemorySource;

/// Vertices and triangles of a fixture mesh.
pub type MeshData = (Vec<[f32; 3]>, Vec<[u32; 3]>);

/// A converted mesh: package, vertices, triangles, packages with a variant.
type MeshEntry = (String, Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<String>);

/// The simple collision of a fixture mesh (manifest version 2).
#[derive(Clone, Debug)]
struct SimpleFixture {
    use_box: bool,
    use_line: bool,
    body: bool,
    convex: Vec<Value>,
    boxes: Vec<Value>,
    spheres: Vec<Value>,
    sphyls: Vec<Value>,
}

impl Default for SimpleFixture {
    /// The original's defaults: both switches on, no body.
    fn default() -> Self {
        Self {
            use_box: true,
            use_line: true,
            body: false,
            convex: Vec::new(),
            boxes: Vec::new(),
            spheres: Vec::new(),
            sphyls: Vec::new(),
        }
    }
}

/// A convex element of a shapes document (`<Name>.collision.json`) for the
/// closed convex polyhedron `vertices`/`triangles`, with the lists the
/// original stores per element written the way it writes them: one plane
/// per face (normal outward), one face-normal direction per direction
/// (parallel faces share one), one edge direction per direction of an edge
/// between two faces that are not coplanar, and the vertices' bounds.
#[must_use]
pub fn convex_element(vertices: &[[f32; 3]], triangles: &[[u32; 3]]) -> Value {
    use glam::DVec3;
    let p = |i: u32| {
        vertices
            .get(i as usize)
            .map_or(DVec3::ZERO, |v| Vec3::from_array(*v).as_dvec3())
    };
    let n = vertices.len().max(1) as f64;
    let centre = vertices
        .iter()
        .map(|v| Vec3::from_array(*v).as_dvec3())
        .sum::<DVec3>()
        / n;
    // Outward unit normal of every triangle (None for a collapsed one).
    let normals: Vec<Option<DVec3>> = triangles
        .iter()
        .map(|t| {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let n = (b - a).cross(c - a).try_normalize()?;
            Some(if n.dot(a - centre) < 0.0 { -n } else { n })
        })
        .collect();
    let same = |a: DVec3, b: DVec3| a.cross(b).length_squared() < 1.0e-10;
    let mut planes: Vec<[f32; 4]> = Vec::new();
    let mut seen_planes: Vec<(DVec3, f64)> = Vec::new();
    let mut face_dirs: Vec<DVec3> = Vec::new();
    for (t, n) in triangles.iter().zip(&normals) {
        let Some(n) = *n else { continue };
        let d = n.dot(p(t[0]));
        if !seen_planes
            .iter()
            .any(|(m, e)| same(*m, n) && m.dot(n) > 0.0 && (e - d).abs() < 1.0e-4)
        {
            seen_planes.push((n, d));
            planes.push([n.x as f32, n.y as f32, n.z as f32, d as f32]);
        }
        if !face_dirs.iter().any(|m| same(*m, n)) {
            face_dirs.push(n);
        }
    }
    // Edges by their end points; an edge is one of the solid when the faces
    // on its two sides are not coplanar (or it has one side only).
    let mut edges: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for (ti, t) in triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if a != b {
                edges.entry((a.min(b), a.max(b))).or_default().push(ti);
            }
        }
    }
    let mut edge_dirs: Vec<DVec3> = Vec::new();
    for ((a, b), faces) in &edges {
        let coplanar = match faces.as_slice() {
            [f, g] => match (normals.get(*f), normals.get(*g)) {
                (Some(Some(m)), Some(Some(n))) => same(*m, *n) && m.dot(*n) > 0.0,
                _ => false,
            },
            _ => false,
        };
        if coplanar {
            continue;
        }
        if let Some(d) = (p(*b) - p(*a)).try_normalize()
            && !edge_dirs.iter().any(|m| same(*m, d))
        {
            edge_dirs.push(d);
        }
    }
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for v in vertices {
        lo = lo.min(Vec3::from_array(*v));
        hi = hi.max(Vec3::from_array(*v));
    }
    let dirs = |list: &[DVec3]| -> Vec<[f32; 3]> {
        list.iter()
            .map(|d| [d.x as f32, d.y as f32, d.z as f32])
            .collect()
    };
    json!({
        "vertices": vertices,
        "planes": planes,
        "face_triangles": triangles,
        "edge_directions": dirs(&edge_dirs),
        "face_normal_directions": dirs(&face_dirs),
        "bounds_min": lo.to_array(),
        "bounds_max": hi.to_array(),
    })
}

/// A row-vector matrix that only moves to `origin` (for the shapes of a
/// body).
#[must_use]
pub fn shape_origin(origin: Vec3) -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [origin.x, origin.y, origin.z, 1.0],
    ]
}

/// A box as 8 vertices and 12 triangles (outward winding irrelevant: the
/// loader orients hull normals itself).
#[must_use]
pub fn box_mesh(min: Vec3, max: Vec3) -> MeshData {
    let v: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            ]
        })
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

fn box_planes(min: Vec3, max: Vec3) -> Vec<[f32; 4]> {
    vec![
        [1.0, 0.0, 0.0, max.x],
        [-1.0, 0.0, 0.0, -min.x],
        [0.0, 1.0, 0.0, max.y],
        [0.0, -1.0, 0.0, -min.y],
        [0.0, 0.0, 1.0, max.z],
        [0.0, 0.0, -1.0, -min.z],
    ]
}

/// One synthetic scene (a map package or a sub-level).
#[derive(Clone, Debug)]
pub struct SceneFixture {
    package: String,
    kill_z: f32,
    title: Option<String>,
    actors: Vec<Value>,
    streaming: Vec<Value>,
    bsp: Option<MeshData>,
}

/// Placement of a fixture actor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// `Location`.
    pub location: Vec3,
    /// `Rotation` (pitch, yaw, roll).
    pub rotation: [i32; 3],
    /// `DrawScale3D` (`DrawScale` 1).
    pub scale: Vec3,
}

impl Place {
    /// At `location`, unrotated, unscaled.
    #[must_use]
    pub fn at(location: Vec3) -> Self {
        Self {
            location,
            rotation: [0; 3],
            scale: Vec3::ONE,
        }
    }
}

impl SceneFixture {
    /// An empty scene for `package` with `WorldInfo.KillZ = kill_z`.
    #[must_use]
    pub fn new(package: &str, kill_z: f32) -> Self {
        let mut s = Self {
            package: package.to_owned(),
            kill_z,
            title: None,
            actors: Vec::new(),
            streaming: Vec::new(),
            bsp: None,
        };
        // Slot 0 is the WorldInfo, as in every shipped map.
        s.push(
            "WorldInfo_0",
            "Engine.WorldInfo",
            "world_info",
            Place::at(Vec3::ZERO),
            json!({}),
            Vec::new(),
            None,
        );
        s
    }

    /// Sets `WorldInfo.Title`.
    #[must_use]
    pub fn with_title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    fn matrix(p: Place) -> [[f32; 4]; 4] {
        actor_local_to_world(p.location, p.rotation, 1.0, p.scale, Vec3::ZERO).to_row_matrix()
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        name: &str,
        class: &str,
        kind: &str,
        p: Place,
        params: Value,
        components: Vec<Value>,
        volume: Option<Value>,
    ) -> usize {
        let slot = self.actors.len();
        self.actors.push(json!({
            "slot": slot,
            "export_index": slot + 1,
            "name": name,
            "class": class,
            "kind": kind,
            "archetype": null,
            "location": p.location.to_array(),
            "rotation": p.rotation,
            "draw_scale": 1.0,
            "draw_scale3d": p.scale.to_array(),
            "pre_pivot": [0.0, 0.0, 0.0],
            "local_to_world": Self::matrix(p),
            "hidden": false,
            "collide_actors": false,
            "block_actors": false,
            "collision_type": "COLLIDE_CustomDefault",
            "tag": class.rsplit('.').next().unwrap_or(class),
            "components": components,
            "params": params,
            "instance": {},
            "volume": volume,
            "matinee": [],
        }));
        slot
    }

    fn set(&mut self, slot: usize, key: &str, value: Value) {
        if let Some(a) = self.actors.get_mut(slot).and_then(Value::as_object_mut) {
            a.insert(key.to_owned(), value);
        }
    }

    /// Path of the actor in `slot` as other actors reference it.
    #[must_use]
    pub fn path(&self, slot: usize) -> String {
        let name = self
            .actors
            .get(slot)
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("None");
        format!("{}.TheWorld.PersistentLevel.{name}", self.package)
    }

    /// The BSP's blocking triangles (world space).
    pub fn set_bsp(&mut self, vertices: Vec<[f32; 3]>, triangles: Vec<[u32; 3]>) {
        self.bsp = Some((vertices, triangles));
    }

    /// A streamed sub-level (`LevelStreamingAlwaysLoaded` or `LevelStreamingKismet`).
    pub fn stream(&mut self, package: &str, class: &str) {
        self.streaming.push(json!({
            "object": format!("{}.TheWorld.{class}_0", self.package),
            "class": class,
            "package_name": package,
            "offset": [0.0, 0.0, 0.0],
            "params": {},
        }));
    }

    /// A `PlayerStart` (cylinder 40 × 80).
    pub fn player_start(&mut self, location: Vec3, yaw: i32) -> usize {
        let p = Place {
            location,
            rotation: [0, yaw, 0],
            scale: Vec3::ONE,
        };
        let m = Self::matrix(p);
        self.push(
            &format!("PlayerStart_{}", self.actors.len()),
            "Engine.PlayerStart",
            "player_start",
            p,
            json!({"bEnabled": true, "bPrimaryStart": true}),
            vec![json!({"name": "CylinderComponent_0", "kind": "cylinder", "local_to_world": m, "cylinder": [40.0, 80.0]})],
            None,
        )
    }

    /// An `ASAMUCheckpoint` with its cylinder; `extra` adds parameters
    /// (`spawnPointOffset`, `spawnPointActor`, `bTriggeredFromKismet`, ...).
    pub fn checkpoint(
        &mut self,
        p: Place,
        radius: f32,
        half_height: f32,
        index: i32,
        extra: Value,
    ) -> usize {
        let mut params = json!({
            "CylinderComponent": "CylinderComponent_0",
            "bEnabled": true,
            "bOffsetLocalSpace": true,
            "bRotatePlayerToSpawnPointRot": true,
            "checkpointIndex": index,
        });
        if let (Some(dst), Some(src)) = (params.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let m = Self::matrix(p);
        let slot = self.push(
            &format!("ASAMUCheckpoint_{}", self.actors.len()),
            "asamu.ASAMUCheckpoint",
            "checkpoint",
            p,
            params,
            vec![json!({
                "name": "CylinderComponent_0", "kind": "cylinder", "local_to_world": m,
                "collide_actors": true, "block_non_zero_extent": true, "cylinder": [radius, half_height]
            })],
            None,
        );
        self.set(slot, "collision_type", json!("COLLIDE_TouchAllButWeapons"));
        self.set(slot, "collide_actors", json!(true));
        slot
    }

    /// A spawn-point marker actor (like `ASAMUCheckpointVisuals`).
    pub fn marker(&mut self, p: Place) -> usize {
        self.push(
            &format!("ASAMUCheckpointVisuals_{}", self.actors.len()),
            "asamu.ASAMUCheckpointVisuals",
            "checkpoint_visuals",
            p,
            json!({}),
            Vec::new(),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn brush_actor(
        &mut self,
        name: &str,
        class: &str,
        kind: &str,
        min: Vec3,
        max: Vec3,
        collide: bool,
        block: bool,
    ) -> usize {
        let (v, t) = box_mesh(min, max);
        let slot = self.push(
            name,
            class,
            kind,
            Place::at(Vec3::ZERO),
            json!({}),
            vec![json!({
                "name": "BrushComponent_0", "kind": "brush",
                "local_to_world": Self::matrix(Place::at(Vec3::ZERO)),
                "collide_actors": collide, "block_actors": block,
                "block_zero_extent": false, "block_non_zero_extent": true
            })],
            Some(
                json!({"hulls": [{"vertices": v, "triangles": t, "planes": box_planes(min, max)}]}),
            ),
        );
        self.set(slot, "collide_actors", json!(collide));
        self.set(slot, "block_actors", json!(block));
        slot
    }

    /// An `ASAMUKillZone` box.
    pub fn kill_zone(&mut self, min: Vec3, max: Vec3) -> usize {
        let n = format!("ASAMUKillZone_{}", self.actors.len());
        self.brush_actor(
            &n,
            "asamu.ASAMUKillZone",
            "kill_zone",
            min,
            max,
            true,
            false,
        )
    }

    /// An `ASAMUDynamicKillZone` box.
    pub fn dynamic_kill_zone(&mut self, min: Vec3, max: Vec3) -> usize {
        let n = format!("ASAMUDynamicKillZone_{}", self.actors.len());
        self.brush_actor(
            &n,
            "asamu.ASAMUDynamicKillZone",
            "dynamic_kill_zone",
            min,
            max,
            true,
            false,
        )
    }

    /// A `TriggerVolume` box.
    pub fn trigger_volume(&mut self, min: Vec3, max: Vec3) -> usize {
        let n = format!("TriggerVolume_{}", self.actors.len());
        self.brush_actor(
            &n,
            "Engine.TriggerVolume",
            "trigger_volume",
            min,
            max,
            true,
            false,
        )
    }

    /// A `BlockingVolume` box (blocks the player, not traces).
    pub fn blocking_volume(&mut self, min: Vec3, max: Vec3) -> usize {
        let n = format!("BlockingVolume_{}", self.actors.len());
        self.brush_actor(
            &n,
            "Engine.BlockingVolume",
            "blocking_volume",
            min,
            max,
            true,
            true,
        )
    }

    /// A `DynamicBlockingVolume` box with its `bEnabled` switch (blocks the
    /// player, not traces, while it collides).
    pub fn dynamic_blocking_volume(&mut self, min: Vec3, max: Vec3, enabled: bool) -> usize {
        let n = format!("DynamicBlockingVolume_{}", self.actors.len());
        let slot = self.brush_actor(
            &n,
            "Engine.DynamicBlockingVolume",
            "blocking_volume",
            min,
            max,
            true,
            true,
        );
        self.set(slot, "params", json!({"bEnabled": enabled}));
        slot
    }

    /// Sets a top-level value of the actor in `slot` (for tests that need a
    /// stored flag the helpers do not offer).
    pub fn set_actor_value(&mut self, slot: usize, key: &str, value: Value) {
        self.set(slot, key, value);
    }

    /// A `Trigger` cylinder.
    pub fn trigger(&mut self, location: Vec3, radius: f32, half_height: f32) -> usize {
        let p = Place::at(location);
        let m = Self::matrix(p);
        let slot = self.push(
            &format!("Trigger_{}", self.actors.len()),
            "Engine.Trigger",
            "trigger",
            p,
            json!({"CylinderComponent": "CylinderComponent_0"}),
            vec![json!({
                "name": "CylinderComponent_0", "kind": "cylinder", "local_to_world": m,
                "collide_actors": true, "block_zero_extent": true, "block_non_zero_extent": true,
                "cylinder": [radius, half_height]
            })],
            None,
        );
        self.set(slot, "collide_actors", json!(true));
        slot
    }

    /// A static-mesh actor of `class`/`kind` placing `mesh` (blocking unless
    /// the collision type says otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn mesh_actor(
        &mut self,
        class: &str,
        kind: &str,
        mesh: &str,
        p: Place,
        collision_type: &str,
        tag: Option<&str>,
        params: Value,
    ) -> usize {
        let m = Self::matrix(p);
        let name = format!(
            "{}_{}",
            class.rsplit('.').next().unwrap_or("Actor"),
            self.actors.len()
        );
        let slot = self.push(
            &name,
            class,
            kind,
            p,
            params,
            vec![json!({
                "name": "StaticMeshComponent_0", "kind": "static_mesh", "local_to_world": m,
                "collide_actors": true, "block_actors": true, "block_zero_extent": true,
                "block_non_zero_extent": true, "static_mesh": mesh
            })],
            None,
        );
        self.set(
            slot,
            "collide_actors",
            json!(collision_type == "COLLIDE_CustomDefault"),
        );
        self.set(
            slot,
            "block_actors",
            json!(collision_type == "COLLIDE_CustomDefault"),
        );
        self.set(slot, "collision_type", json!(collision_type));
        self.set(
            slot,
            "instance",
            json!({"CollisionComponent": format!("{}.{name}.StaticMeshComponent_0", self.package)}),
        );
        if let Some(t) = tag {
            self.set(slot, "tag", json!(t));
        }
        slot
    }

    /// A blocking `StaticMeshActor`.
    pub fn static_mesh(&mut self, mesh: &str, p: Place) -> usize {
        self.mesh_actor(
            "Engine.StaticMeshActor",
            "static_mesh",
            mesh,
            p,
            "COLLIDE_CustomDefault",
            None,
            json!({}),
        )
    }

    /// The scene file contents.
    #[must_use]
    pub fn scene_json(&self) -> Value {
        json!({
            "format": "asamu-scene",
            "version": 1,
            "package": self.package,
            "level": format!("{}.TheWorld.PersistentLevel", self.package),
            "level_export": 1,
            "coordinates": "UE3 world space",
            "tail": {},
            "world_info": {
                "object": format!("{}.TheWorld.PersistentLevel.WorldInfo_0", self.package),
                "title": self.title,
                "kill_z": self.kill_z,
                "soft_kill_z": false,
                "default_gravity_z": -520.0,
                "global_gravity_z": 0.0,
                "default_game_type": "asamu.ASAMUGameInfo"
            },
            "streaming_levels": self.streaming,
            "bsp_model": null,
            "actors": self.actors,
            "stats": {},
            "warnings": [],
        })
    }

    /// Writes the scene (and its BSP) into `src`.
    pub fn write(&self, src: &mut MemorySource) {
        src.insert(
            format!("levels/{}.scene.json", self.package),
            self.scene_json().to_string().into_bytes(),
        );
        if let Some((v, t)) = &self.bsp {
            let mut bin = Vec::new();
            for p in v {
                for c in p {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            let tri_off = bin.len();
            for tri in t {
                for i in tri {
                    bin.extend_from_slice(&i.to_le_bytes());
                }
            }
            let tag_off = bin.len();
            for _ in t {
                bin.extend_from_slice(&0u32.to_le_bytes());
            }
            let spans = json!({
                "positions": {"offset": 0, "count": v.len()},
                "triangles": {"offset": tri_off, "count": t.len()},
                "surfaces": {"offset": tag_off, "count": t.len()},
            });
            let index = json!({
                "format": "asamu-bsp", "version": 1, "coordinates": "UE3 world space", "winding": "",
                "bin": format!("{}.bsp.bin", self.package),
                "model": format!("{}.TheWorld.PersistentLevel.Model_0", self.package),
                "meshes": {"collision": spans, "visible": spans},
            });
            src.insert(
                format!("levels/{}.bsp.json", self.package),
                index.to_string().into_bytes(),
            );
            src.insert(format!("levels/{}.bsp.bin", self.package), bin);
        }
    }
}

/// Synthetic converted static meshes.
#[derive(Clone, Debug, Default)]
pub struct MeshFixtures {
    meshes: BTreeMap<String, MeshEntry>,
    simple: BTreeMap<String, SimpleFixture>,
}

impl MeshFixtures {
    /// No meshes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the mesh `path` (UE3 local space) converted from `package`.
    pub fn add(
        &mut self,
        path: &str,
        package: &str,
        vertices: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
    ) {
        self.meshes.insert(
            path.to_owned(),
            (package.to_owned(), vertices, triangles, Vec::new()),
        );
    }

    /// Marks `path` as differing in `package` (the variant must be added as
    /// `<path>@<package>`).
    pub fn differs_in(&mut self, path: &str, package: &str) {
        if let Some(m) = self.meshes.get_mut(path) {
            m.3.push(package.to_owned());
        }
    }

    /// Stores the mesh's two simple-collision switches as the original's
    /// meshes do when they differ from the default `true`
    /// (`UseSimpleBoxCollision` for swept traces, `UseSimpleLineCollision`
    /// for line traces). Version 2 only.
    pub fn set_switches(&mut self, path: &str, use_box: bool, use_line: bool) {
        let s = self.simple.entry(path.to_owned()).or_default();
        s.use_box = use_box;
        s.use_line = use_line;
    }

    /// Gives the mesh a body setup without any shape yet (the other
    /// `add_*` shape methods do this themselves). Version 2 only.
    pub fn add_body(&mut self, path: &str) {
        self.simple.entry(path.to_owned()).or_default().body = true;
    }

    /// Adds the closed convex polyhedron `vertices`/`triangles` (mesh local
    /// space) to the mesh's simple body as a convex element
    /// ([`convex_element`]). Version 2 only.
    pub fn add_convex(&mut self, path: &str, vertices: &[[f32; 3]], triangles: &[[u32; 3]]) {
        self.add_convex_raw(path, convex_element(vertices, triangles));
    }

    /// Adds a convex element as written (`vertices`, `planes`,
    /// `face_triangles`, `edge_directions`, `face_normal_directions`,
    /// `bounds_min`, `bounds_max`), for elements whose lists the test wants
    /// to choose. Version 2 only.
    pub fn add_convex_raw(&mut self, path: &str, element: Value) {
        let s = self.simple.entry(path.to_owned()).or_default();
        s.body = true;
        s.convex.push(element);
    }

    /// Adds a box element of full edge lengths `size` centred on `origin`
    /// (mesh local space, unrotated). Version 2 only.
    pub fn add_box_shape(&mut self, path: &str, origin: Vec3, size: Vec3) {
        let s = self.simple.entry(path.to_owned()).or_default();
        s.body = true;
        s.boxes.push(json!({
            "transform": shape_origin(origin), "size": size.to_array(),
            "no_rb_collision": false, "per_poly_shape": false,
        }));
    }

    /// Adds a sphere element. Version 2 only.
    pub fn add_sphere_shape(&mut self, path: &str, origin: Vec3, radius: f32) {
        let s = self.simple.entry(path.to_owned()).or_default();
        s.body = true;
        s.spheres.push(json!({
            "transform": shape_origin(origin), "radius": radius,
            "no_rb_collision": false, "per_poly_shape": false,
        }));
    }

    /// Adds a capsule element (its segment of `length` along local Z).
    /// Version 2 only.
    pub fn add_sphyl_shape(&mut self, path: &str, origin: Vec3, radius: f32, length: f32) {
        let s = self.simple.entry(path.to_owned()).or_default();
        s.body = true;
        s.sphyls.push(json!({
            "transform": shape_origin(origin), "radius": radius, "length": length,
            "no_rb_collision": false, "per_poly_shape": false,
        }));
    }

    /// Adds the box `[min, max]` as a mesh whose simple body is that same
    /// box as one convex element: what a plain solid block is in the
    /// original (triangles for bullet traces, the body for the pawn).
    pub fn add_solid_box(&mut self, path: &str, package: &str, min: Vec3, max: Vec3) {
        let (v, t) = box_mesh(min, max);
        self.add_convex(path, &v, &t);
        self.add(path, package, v, t);
    }

    /// Writes `meshes/manifest.json` (version 1: no simple-collision data)
    /// and one glTF + bin per mesh, with the collision as a `UCX_` node
    /// (`ucx`) or only as render sections with `EnableCollision` (the
    /// loader's fallback).
    pub fn write(&self, src: &mut MemorySource, ucx: bool) {
        self.write_version(src, ucx, 1);
    }

    /// [`Self::write`] with a version 2 manifest: every mesh has its
    /// `simple_collision` record, and a mesh with a body its
    /// `.collision.json` file.
    pub fn write_v2(&self, src: &mut MemorySource, ucx: bool) {
        self.write_version(src, ucx, 2);
    }

    fn write_version(&self, src: &mut MemorySource, ucx: bool, version: u32) {
        let mut entries = serde_json::Map::new();
        for (path, (package, v, t, differs)) in &self.meshes {
            let stem = path.replace(['.', '@'], "_");
            let gltf = format!("{package}/{stem}.gltf");
            let bin_name = format!("{package}/{stem}.bin");
            let mut bin = Vec::new();
            for p in v {
                // glTF = (ue.y, ue.z, -ue.x).
                for c in [p[1], p[2], -p[0]] {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            let pos_len = bin.len();
            for tri in t {
                for i in tri {
                    bin.extend_from_slice(&u16::try_from(*i).unwrap_or(u16::MAX).to_le_bytes());
                }
            }
            while bin.len() % 4 != 0 {
                bin.push(0);
            }
            let views = json!([
                {"buffer": 0, "byteOffset": 0, "byteLength": pos_len, "target": 34962},
                {"buffer": 0, "byteOffset": pos_len, "byteLength": t.len() * 6, "target": 34963},
            ]);
            let accessors = json!([
                {"bufferView": 0, "componentType": 5126, "count": v.len(), "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": t.len() * 3, "type": "SCALAR"},
            ]);
            let mut nodes = vec![json!({"name": stem, "mesh": 0})];
            let mut meshes = vec![
                json!({"name": stem, "primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 4}]}),
            ];
            if ucx {
                nodes.push(json!({"name": format!("UCX_{stem}"), "mesh": 1, "extras": {"asamu_collision": true}}));
                meshes.push(json!({"name": format!("UCX_{stem}"), "primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 4}]}));
            }
            let doc = json!({
                "asset": {"version": "2.0"},
                "scene": 0,
                "nodes": nodes,
                "meshes": meshes,
                "accessors": accessors,
                "bufferViews": views,
                "buffers": [{"uri": format!("{stem}.bin"), "byteLength": bin.len()}],
            });
            src.insert(format!("meshes/{gltf}"), doc.to_string().into_bytes());
            src.insert(format!("meshes/{bin_name}"), bin);
            let simple = self.simple.get(path).cloned().unwrap_or_default();
            let shapes = format!("{package}/{stem}.collision.json");
            if version >= 2 && simple.body {
                let shapes_doc = json!({
                    "format": "asamu-simple-collision",
                    "version": 1,
                    "notice": "synthetic test data",
                    "space": "mesh local space, UE3 axes, Unreal units, unscaled",
                    "mesh": path,
                    "body_setup": format!("{path}.RB_BodySetup_0"),
                    "body_properties": {},
                    "convex": simple.convex,
                    "boxes": simple.boxes,
                    "spheres": simple.spheres,
                    "sphyls": simple.sphyls,
                });
                src.insert(
                    format!("meshes/{shapes}"),
                    shapes_doc.to_string().into_bytes(),
                );
            }
            let mut entry = json!({
                    "package": package,
                    "export_index": 1,
                    "also_in": [],
                    "differs_in": differs,
                    "lod_count": 1,
                    "lods": [{
                        "lod": 0, "gltf": gltf, "bin": bin_name,
                        "sections": [{"material": null, "first_index": 0, "triangles": t.len(), "collision": true, "cast_shadow": true}],
                        "stats": {}
                    }],
                    "light_map_coordinate_index": null,
                    "light_map_resolution": null,
                    "body_setup": null,
                    "collision_triangles": if ucx { t.len() } else { 0 },
                    "bounds_ue": {"origin": [0.0, 0.0, 0.0], "box_extent": [0.0, 0.0, 0.0], "sphere_radius": 0.0},
                    "scale": 1.0,
                    "content_hash": "0",
            });
            if version >= 2
                && let Some(e) = entry.as_object_mut()
            {
                e.insert(
                    "simple_collision".to_owned(),
                    json!({
                        "use_simple_box_collision": simple.use_box,
                        "use_simple_line_collision": simple.use_line,
                        "use_simple_rigid_body_collision": true,
                        "has_body_setup": simple.body,
                        "shapes": if simple.body { Value::from(shapes) } else { Value::Null },
                        "convex": simple.convex.len(),
                        "boxes": simple.boxes.len(),
                        "spheres": simple.spheres.len(),
                        "sphyls": simple.sphyls.len(),
                    }),
                );
                if simple.body {
                    e.insert(
                        "body_setup".to_owned(),
                        json!(format!("{path}.RB_BodySetup_0")),
                    );
                }
            }
            entries.insert(path.clone(), entry);
        }
        let manifest = json!({
            "version": version,
            "notice": "synthetic test data",
            "coordinates": "glTF",
            "scale": 1.0,
            "meshes": entries,
        });
        src.insert("meshes/manifest.json", manifest.to_string().into_bytes());
    }
}
