//! Synthetic converted-data fixtures for tests (no game data).
//!
//! Writes, into a [`MemorySource`], files in the layout and format that
//! `asamu-import levels` and `asamu-import meshes --collision` produce
//! (`levels/<map>.scene.json`, `.bsp.json`/`.bsp.bin`, `meshes/manifest.json`
//! and glTF files), built entirely from geometry chosen by the test. Used by
//! the loader and gameplay tests here and in `asamu-game`.

use std::collections::BTreeMap;

use glam::Vec3;
pub use serde_json::{Value, json};

use crate::rotation::actor_local_to_world;
use crate::scene::MemorySource;

/// Vertices and triangles of a fixture mesh.
pub type MeshData = (Vec<[f32; 3]>, Vec<[u32; 3]>);

/// A converted mesh: package, vertices, triangles, packages with a variant.
type MeshEntry = (String, Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<String>);

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

    /// Writes `meshes/manifest.json` and one glTF + bin per mesh, with the
    /// collision as a `UCX_` node (`ucx`) or only as render sections with
    /// `EnableCollision` (the loader's fallback).
    pub fn write(&self, src: &mut MemorySource, ucx: bool) {
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
            entries.insert(
                path.clone(),
                json!({
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
                }),
            );
        }
        let manifest = json!({
            "version": 1,
            "notice": "synthetic test data",
            "coordinates": "glTF",
            "scale": 1.0,
            "meshes": entries,
        });
        src.insert("meshes/manifest.json", manifest.to_string().into_bytes());
    }
}
