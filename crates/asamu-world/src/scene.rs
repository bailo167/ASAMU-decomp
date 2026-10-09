//! Loader for the levels converted by `asamu-import` (user-local data).
//!
//! Reads, from a converted-data directory (or any [`DataSource`]):
//!
//! - `levels/<map>.scene.json` (`asamu-scene` v1, see
//!   `docs/reverse-engineering/LEVEL_FORMAT.md` and
//!   `tools/asamu-import/src/levels.rs`) for the map and every streamed
//!   sub-level its `WorldInfo` lists (AG-BeautifulCity → Freds_place,
//!   AG-IceCave → TheCore);
//! - `levels/<map>.bsp.json` + `.bsp.bin` (`asamu-bsp` v1): the BSP's
//!   blocking triangles;
//! - `meshes/manifest.json` and the per-mesh glTF files
//!   (`tools/asamu-import/src/meshes.rs`): the kDOP collision triangles of
//!   every static mesh a level places (the `UCX_<Name>` node written with
//!   `--collision`, or else the LOD 0 triangles of the collision-enabled
//!   sections — the same set, CONFIRMED in `MESHES.md`).
//!
//! and builds a [`LoadedMap`]: a [`CollisionScene`] with every blocking
//! static-mesh component placed by its `LocalToWorld`, the BSP and the convex
//! hulls of blocking volumes; the moving actors' collision as dynamic
//! instances; and the gameplay actors ([`crate::gameplay::SceneActors`]).
//!
//! The JSON structures here are our own, independent of `asamu-ue3`; unknown
//! fields are ignored. Everything read is treated as untrusted: file sizes,
//! counts, indices and glTF accessors are bounds-checked, malformed meshes
//! are skipped with a warning, and nothing panics.
//!
//! # Actor ids
//!
//! Every actor gets the id `(sub-level index << 16) | slot` ([`actor_id`]),
//! where the slot is its index in `ULevel::Actors` (< 7 600 in every shipped
//! map) and sub-level 0 is the persistent level.
//!
//! # Collision flags (what blocks what)
//!
//! An actor's effective `bCollideActors`/`bBlockActors` come from its stored
//! flags, unless its `CollisionType` is not `COLLIDE_CustomDefault`, in which
//! case the flags (and those of its collision component) follow the type, as
//! UE3's `SetCollisionType` sets them (TENTATIVE: when the engine applies the
//! stored type is not established; the crystals and glow flowers store
//! `COLLIDE_BlockAll` with cleared actor flags but are grapple targets).
//! Both falling-rock classes call `SetCollisionType(COLLIDE_BlockAll)` at
//! begin play, and checkpoints `COLLIDE_TouchAllButWeapons` (or none when
//! triggered from Kismet) (CONFIRMED (src)). A component then blocks the
//! player's moves when the actor collides and blocks and the component has
//! `CollideActors`, `BlockActors` and `BlockNonZeroExtent`; it blocks
//! zero-extent traces (grapple) with `BlockZeroExtent` (TENTATIVE: stock
//! trace rules).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use glam::Vec3;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::SurfaceTag;
use crate::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, Instance, InstanceInfo,
};
use crate::gameplay::{self, SceneActors};

/// `format` of a scene file.
pub const SCENE_FORMAT: &str = "asamu-scene";
/// Supported scene version.
pub const SCENE_VERSION: u32 = 1;
/// `format` of a BSP index.
pub const BSP_FORMAT: &str = "asamu-bsp";

/// Errors that stop loading a map.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SceneError {
    /// A file could not be read.
    #[error("{path}: {message}")]
    Io {
        /// File.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// A file is not valid JSON of the expected shape.
    #[error("{path}: invalid JSON: {message}")]
    Json {
        /// File.
        path: String,
        /// Parser message.
        message: String,
    },
    /// A file has the wrong format or version, or impossible contents.
    #[error("{path}: {message}")]
    Format {
        /// File.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// No converted scene for the map.
    #[error("no converted scene for map {0} (run `asamu-import levels`)")]
    MissingMap(String),
    /// A file is larger than [`LoadOptions::max_file_bytes`].
    #[error("{path}: {size} bytes exceeds the limit")]
    TooLarge {
        /// File.
        path: String,
        /// Its size.
        size: u64,
    },
    /// A path from the data tries to leave the data directory.
    #[error("unsafe relative path {0:?}")]
    UnsafePath(String),
}

/// Where converted files come from (paths are relative, `/`-separated).
pub trait DataSource {
    /// The bytes of `path`, refusing files above `max_bytes`.
    ///
    /// # Errors
    /// Missing or unreadable files, oversize files, unsafe paths.
    fn read(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, SceneError>;

    /// File names in the directory `dir`.
    ///
    /// # Errors
    /// Unreadable directory or unsafe path.
    fn list(&self, dir: &str) -> Result<Vec<String>, SceneError>;
}

/// Splits a relative data path into safe components (no root, no `..`, no
/// empty or `.` parts; `/` and `\` both separate).
fn safe_components(path: &str) -> Result<Vec<&str>, SceneError> {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let ok = !parts.is_empty()
        && parts.iter().all(|p| {
            !p.is_empty() && *p != "." && *p != ".." && !p.contains(':') && !p.contains('\0')
        });
    if ok {
        Ok(parts)
    } else {
        Err(SceneError::UnsafePath(path.to_owned()))
    }
}

/// A converted-data directory on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirSource {
    root: PathBuf,
}

impl DirSource {
    /// Reads below `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, SceneError> {
        let mut p = self.root.clone();
        for c in safe_components(path)? {
            p.push(c);
        }
        // Belt and braces: the joined path must not contain parent or root
        // components beyond the root's own.
        if p.components()
            .skip(self.root.components().count())
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(SceneError::UnsafePath(path.to_owned()));
        }
        Ok(p)
    }
}

impl DataSource for DirSource {
    fn read(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, SceneError> {
        let full = self.resolve(path)?;
        let io = |e: std::io::Error| SceneError::Io {
            path: path.to_owned(),
            message: e.to_string(),
        };
        let meta = std::fs::metadata(&full).map_err(io)?;
        if !meta.is_file() {
            return Err(SceneError::Io {
                path: path.to_owned(),
                message: "not a file".to_owned(),
            });
        }
        if meta.len() > max_bytes {
            return Err(SceneError::TooLarge {
                path: path.to_owned(),
                size: meta.len(),
            });
        }
        std::fs::read(&full).map_err(io)
    }

    fn list(&self, dir: &str) -> Result<Vec<String>, SceneError> {
        let full = if dir.is_empty() {
            self.root.clone()
        } else {
            self.resolve(dir)?
        };
        let io = |e: std::io::Error| SceneError::Io {
            path: dir.to_owned(),
            message: e.to_string(),
        };
        let mut out: Vec<String> = std::fs::read_dir(&full)
            .map_err(io)?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        out.sort();
        Ok(out)
    }
}

/// In-memory files (tests, embedded fixtures).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemorySource {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemorySource {
    /// No files.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a file.
    pub fn insert(&mut self, path: impl Into<String>, data: impl Into<Vec<u8>>) {
        self.files.insert(path.into(), data.into());
    }
}

impl DataSource for MemorySource {
    fn read(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, SceneError> {
        safe_components(path)?;
        let data = self.files.get(path).ok_or_else(|| SceneError::Io {
            path: path.to_owned(),
            message: "not found".to_owned(),
        })?;
        if data.len() as u64 > max_bytes {
            return Err(SceneError::TooLarge {
                path: path.to_owned(),
                size: data.len() as u64,
            });
        }
        Ok(data.clone())
    }

    fn list(&self, dir: &str) -> Result<Vec<String>, SceneError> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        Ok(self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .filter(|rest| !rest.contains('/'))
            .map(str::to_owned)
            .collect())
    }
}

/// Limits for loading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadOptions {
    /// Largest file read (bytes). The default (256 MiB) is more than ten
    /// times the largest converted file of the shipped game (a scene of
    /// about 18 MB).
    pub max_file_bytes: u64,
    /// Largest triangle count of one mesh (also bounds its vertex and index
    /// counts: at most three per triangle).
    pub max_mesh_triangles: usize,
    /// Largest total of collision triangles held by the scene: unique mesh
    /// triangles, BSP and blocking-volume triangles, and the world-space
    /// copies made for placements without an inverse.
    pub max_total_triangles: usize,
    /// Largest actor count of one scene.
    pub max_actors: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 28,
            max_mesh_triangles: 1 << 22,
            max_total_triangles: 1 << 26,
            max_actors: 1 << 16,
        }
    }
}

impl LoadOptions {
    /// Largest vertex or index count of one mesh (three per triangle).
    #[must_use]
    pub fn max_mesh_elements(&self) -> usize {
        self.max_mesh_triangles.saturating_mul(3)
    }
}

// ---------------------------------------------------------------------------
// JSON shapes (subset of what the importer writes).
// ---------------------------------------------------------------------------

/// A row-vector 4×4 matrix.
pub type Mat4 = [[f32; 4]; 4];

const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn identity() -> Mat4 {
    IDENTITY
}

fn one() -> f32 {
    1.0
}

fn ones3() -> [f32; 3] {
    [1.0; 3]
}

/// `<map>.scene.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct SceneFile {
    /// [`SCENE_FORMAT`].
    pub format: String,
    /// [`SCENE_VERSION`].
    pub version: u32,
    /// Map package name.
    pub package: String,
    /// `WorldInfo` settings.
    #[serde(default)]
    pub world_info: Option<WorldInfoJson>,
    /// Streaming sub-levels.
    #[serde(default)]
    pub streaming_levels: Vec<StreamingLevelJson>,
    /// Actors in `ULevel::Actors` order.
    #[serde(default)]
    pub actors: Vec<ActorJson>,
}

/// `WorldInfo` summary.
#[derive(Clone, Debug, Deserialize)]
pub struct WorldInfoJson {
    /// `Title`.
    #[serde(default)]
    pub title: Option<String>,
    /// `KillZ`.
    pub kill_z: f32,
    /// `bSoftKillZ`.
    #[serde(default)]
    pub soft_kill_z: bool,
    /// `DefaultGravityZ`.
    #[serde(default)]
    pub default_gravity_z: f32,
    /// `GlobalGravityZ`.
    #[serde(default)]
    pub global_gravity_z: f32,
}

/// A streaming sub-level reference.
#[derive(Clone, Debug, Deserialize)]
pub struct StreamingLevelJson {
    /// The `LevelStreaming*` class.
    #[serde(default)]
    pub class: String,
    /// `PackageName`.
    #[serde(default)]
    pub package_name: Option<String>,
    /// `Offset`.
    #[serde(default)]
    pub offset: [f32; 3],
}

/// Stored instance values the loader needs.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InstanceValues {
    /// `CollisionComponent` (object path).
    #[serde(rename = "CollisionComponent", default)]
    pub collision_component: Option<String>,
}

/// One placed actor.
#[derive(Clone, Debug, Deserialize)]
pub struct ActorJson {
    /// Index in `ULevel::Actors`.
    pub slot: usize,
    /// Object name.
    pub name: String,
    /// Qualified class path.
    pub class: String,
    /// Gameplay kind (snake case, see the importer).
    pub kind: String,
    /// `Location`.
    #[serde(default)]
    pub location: [f32; 3],
    /// `Rotation` (pitch, yaw, roll).
    #[serde(default)]
    pub rotation: [i32; 3],
    /// `DrawScale`.
    #[serde(default = "one")]
    pub draw_scale: f32,
    /// `DrawScale3D`.
    #[serde(default = "ones3")]
    pub draw_scale3d: [f32; 3],
    /// `PrePivot`.
    #[serde(default)]
    pub pre_pivot: [f32; 3],
    /// `LocalToWorld`.
    #[serde(default = "identity")]
    pub local_to_world: Mat4,
    /// `bHidden`.
    #[serde(default)]
    pub hidden: bool,
    /// `bCollideActors`.
    #[serde(default)]
    pub collide_actors: bool,
    /// `bBlockActors`.
    #[serde(default)]
    pub block_actors: bool,
    /// `CollisionType`.
    #[serde(default)]
    pub collision_type: Option<String>,
    /// `Tag`.
    #[serde(default)]
    pub tag: Option<String>,
    /// Components.
    #[serde(default)]
    pub components: Vec<ComponentJson>,
    /// Effective class parameters.
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Stored instance values (subset).
    #[serde(default)]
    pub instance: InstanceValues,
    /// Brush / volume geometry.
    #[serde(default)]
    pub volume: Option<VolumeJson>,
    /// Matinee actions driving the actor.
    #[serde(default)]
    pub matinee: Vec<Value>,
}

/// A component.
#[derive(Clone, Debug, Deserialize)]
pub struct ComponentJson {
    /// Object name.
    pub name: String,
    /// Role (snake case).
    pub kind: String,
    /// World transform.
    #[serde(default = "identity")]
    pub local_to_world: Mat4,
    /// `CollideActors`.
    #[serde(default)]
    pub collide_actors: bool,
    /// `BlockActors`.
    #[serde(default)]
    pub block_actors: bool,
    /// `BlockZeroExtent`.
    #[serde(default)]
    pub block_zero_extent: bool,
    /// `BlockNonZeroExtent`.
    #[serde(default)]
    pub block_non_zero_extent: bool,
    /// Static mesh path.
    #[serde(default)]
    pub static_mesh: Option<String>,
    /// `[CollisionRadius, CollisionHeight]`.
    #[serde(default)]
    pub cylinder: Option<[f32; 2]>,
}

/// World-space brush/volume geometry.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct VolumeJson {
    /// Convex hulls.
    #[serde(default)]
    pub hulls: Vec<HullJson>,
}

/// One convex hull.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HullJson {
    /// Vertices.
    #[serde(default)]
    pub vertices: Vec<[f32; 3]>,
    /// Triangles.
    #[serde(default)]
    pub triangles: Vec<[u32; 3]>,
    /// Face planes `(X, Y, Z, W)`.
    #[serde(default)]
    pub planes: Vec<[f32; 4]>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct SpanJson {
    offset: usize,
    count: usize,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct MeshSpansJson {
    positions: SpanJson,
    triangles: SpanJson,
}

#[derive(Clone, Debug, Deserialize)]
struct BspIndexJson {
    format: String,
    #[serde(default)]
    bin: String,
    #[serde(default)]
    meshes: BTreeMap<String, MeshSpansJson>,
}

/// `meshes/manifest.json`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MeshManifestJson {
    /// glTF units per UU of the last run.
    #[serde(default = "one")]
    pub scale: f32,
    /// Meshes by object path.
    #[serde(default)]
    pub meshes: BTreeMap<String, MeshEntryJson>,
}

/// One mesh.
#[derive(Clone, Debug, Deserialize)]
pub struct MeshEntryJson {
    /// Package the files were converted from.
    #[serde(default)]
    pub package: String,
    /// Packages with a differing copy (converted as `<path>@<package>`).
    #[serde(default)]
    pub differs_in: Vec<String>,
    /// Exported LODs.
    #[serde(default)]
    pub lods: Vec<LodJson>,
    /// kDOP collision triangles.
    #[serde(default)]
    pub collision_triangles: usize,
    /// glTF units per UU.
    #[serde(default)]
    pub scale: Option<f32>,
}

/// One exported LOD.
#[derive(Clone, Debug, Deserialize)]
pub struct LodJson {
    /// LOD index.
    #[serde(default)]
    pub lod: usize,
    /// `.gltf` path relative to `meshes/`.
    pub gltf: String,
    /// `.bin` path relative to `meshes/`.
    pub bin: String,
    /// Sections.
    #[serde(default)]
    pub sections: Vec<SectionJson>,
}

/// One section.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct SectionJson {
    /// Triangles.
    #[serde(default)]
    pub triangles: u32,
    /// `EnableCollision`.
    #[serde(default)]
    pub collision: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfJson {
    #[serde(default)]
    nodes: Vec<GltfNode>,
    #[serde(default)]
    meshes: Vec<GltfMesh>,
    #[serde(default)]
    accessors: Vec<GltfAccessor>,
    #[serde(rename = "bufferViews", default)]
    buffer_views: Vec<GltfView>,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfNode {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mesh: Option<usize>,
    #[serde(default)]
    extras: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfMesh {
    #[serde(default)]
    primitives: Vec<GltfPrimitive>,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfPrimitive {
    #[serde(default)]
    attributes: BTreeMap<String, usize>,
    #[serde(default)]
    indices: Option<usize>,
    #[serde(default)]
    mode: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfAccessor {
    #[serde(rename = "bufferView", default)]
    buffer_view: Option<usize>,
    #[serde(rename = "byteOffset", default)]
    byte_offset: usize,
    #[serde(rename = "componentType")]
    component_type: u32,
    count: usize,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GltfView {
    #[serde(default)]
    buffer: usize,
    #[serde(rename = "byteOffset", default)]
    byte_offset: usize,
    #[serde(rename = "byteLength")]
    byte_length: usize,
    #[serde(rename = "byteStride", default)]
    byte_stride: Option<usize>,
}

// ---------------------------------------------------------------------------
// Parameter helpers.
// ---------------------------------------------------------------------------

/// Case-insensitive parameter lookup (UE3 property names ignore case).
#[must_use]
pub fn param<'a>(params: &'a BTreeMap<String, Value>, name: &str) -> Option<&'a Value> {
    params.get(name).or_else(|| {
        params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

/// A float parameter (`default` when missing or not a number).
#[must_use]
pub fn param_f32(params: &BTreeMap<String, Value>, name: &str, default: f32) -> f32 {
    param(params, name)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// An integer parameter.
#[must_use]
pub fn param_i64(params: &BTreeMap<String, Value>, name: &str, default: i64) -> i64 {
    param(params, name)
        .and_then(Value::as_i64)
        .unwrap_or(default)
}

/// A bool parameter.
#[must_use]
pub fn param_bool(params: &BTreeMap<String, Value>, name: &str, default: bool) -> bool {
    param(params, name)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

/// A string parameter (names, enumerators, object paths).
#[must_use]
pub fn param_str<'a>(params: &'a BTreeMap<String, Value>, name: &str) -> Option<&'a str> {
    param(params, name).and_then(Value::as_str)
}

/// A vector parameter (`{X, Y, Z}`).
#[must_use]
pub fn param_vec3(params: &BTreeMap<String, Value>, name: &str) -> Option<Vec3> {
    let m = param(params, name)?.as_object()?;
    let c = |k: &str| {
        m.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .and_then(|(_, v)| v.as_f64())
            .map(|v| v as f32)
    };
    let v = Vec3::new(c("X")?, c("Y")?, c("Z")?);
    v.is_finite().then_some(v)
}

/// The object name at the end of an object path (`Pkg.TheWorld.PersistentLevel.Name` → `Name`).
#[must_use]
pub fn object_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Actor id of `slot` in sub-level `level` (see the module docs); `None`
/// when the slot does not fit in 16 bits.
#[must_use]
pub fn actor_id(level: u8, slot: usize) -> Option<u32> {
    let slot = u32::try_from(slot).ok().filter(|s| *s < 1 << 16)?;
    Some(u32::from(level) << 16 | slot)
}

fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::from_array(a)
}

// ---------------------------------------------------------------------------
// Loaded data.
// ---------------------------------------------------------------------------

/// `WorldInfo` settings of the persistent level.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldSettings {
    /// `Title`.
    pub title: Option<String>,
    /// `KillZ`.
    pub kill_z: f32,
    /// `bSoftKillZ`.
    pub soft_kill_z: bool,
    /// `DefaultGravityZ`.
    pub default_gravity_z: f32,
    /// `GlobalGravityZ` (0 = default).
    pub global_gravity_z: f32,
}

/// One level of the loaded set (index 0 = the persistent level).
#[derive(Clone, Debug, PartialEq)]
pub struct SubLevel {
    /// Package name (as in its scene file).
    pub name: String,
    /// `LevelStreaming*` class that streams it (`None` for the persistent level).
    pub streaming_class: Option<String>,
    /// Loaded and visible at level start (`LevelStreamingAlwaysLoaded`; a
    /// `LevelStreamingKismet` level waits for its Kismet action).
    pub initially_loaded: bool,
    /// `Offset` applied to everything in it.
    pub offset: Vec3,
    /// Actors extracted.
    pub actors: usize,
}

/// A moving actor's collision: dynamic instances placed relative to the
/// actor's transform.
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicBody {
    /// Actor id.
    pub actor: u32,
    /// `(index into LoadedMap::dynamic, component-relative transform)`.
    pub parts: Vec<(usize, Affine)>,
    /// `DrawScale` (for recomputing the actor transform).
    pub draw_scale: f32,
    /// `DrawScale3D`.
    pub draw_scale3d: Vec3,
    /// `PrePivot`.
    pub pre_pivot: Vec3,
}

/// Counts of a load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadStats {
    /// Actors over all levels.
    pub actors: usize,
    /// Static-mesh components that block something.
    pub blocking_components: usize,
    /// ... of which with collision geometry placed.
    pub placed_components: usize,
    /// Unique meshes loaded.
    pub meshes_loaded: usize,
    /// Mesh paths that could not be loaded (missing or malformed).
    pub meshes_failed: usize,
    /// Meshes without any collision triangle (no collision, by design).
    pub meshes_without_collision: usize,
    /// Blocking components without a static mesh.
    pub components_without_mesh: usize,
    /// BSP triangles.
    pub bsp_triangles: usize,
    /// Convex hulls of blocking volumes.
    pub blocking_hulls: usize,
}

/// A converted map ready for play.
#[derive(Clone, Debug)]
pub struct LoadedMap {
    /// Persistent level package name.
    pub map: String,
    /// The persistent level and its streamed sub-levels.
    pub levels: Vec<SubLevel>,
    /// `WorldInfo` settings.
    pub world: WorldSettings,
    /// Static collision.
    pub collision: CollisionScene,
    /// Initial dynamic instances (see [`DynamicBody`]).
    pub dynamic: Vec<Instance>,
    /// Moving actors.
    pub bodies: Vec<DynamicBody>,
    /// Gameplay actors.
    pub actors: SceneActors,
    /// Counts.
    pub stats: LoadStats,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

impl LoadedMap {
    /// Bit mask of the initially loaded levels (for `QueryFilter::sublevels`).
    #[must_use]
    pub fn initial_level_mask(&self) -> u64 {
        self.levels
            .iter()
            .enumerate()
            .filter(|(i, l)| *i < 64 && l.initially_loaded)
            .fold(0u64, |m, (i, _)| m | 1 << i)
    }

    /// Index of the level named `name` (case-insensitive).
    #[must_use]
    pub fn level_index(&self, name: &str) -> Option<usize> {
        self.levels
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(name))
    }
}

// ---------------------------------------------------------------------------
// Loading.
// ---------------------------------------------------------------------------

fn parse_json<T: for<'de> Deserialize<'de>>(path: &str, data: &[u8]) -> Result<T, SceneError> {
    serde_json::from_slice(data).map_err(|e| SceneError::Json {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

/// Case-insensitive lookup of `levels/<name>.scene.json`.
fn find_level_file(source: &dyn DataSource, name: &str, suffix: &str) -> Option<String> {
    let wanted = format!("{name}{suffix}");
    let files = source.list("levels").ok()?;
    files
        .iter()
        .find(|f| **f == wanted)
        .or_else(|| files.iter().find(|f| f.eq_ignore_ascii_case(&wanted)))
        .map(|f| format!("levels/{f}"))
}

/// Reads and checks one scene file.
fn read_scene(
    source: &dyn DataSource,
    name: &str,
    opts: &LoadOptions,
) -> Result<SceneFile, SceneError> {
    let path = find_level_file(source, name, ".scene.json")
        .ok_or_else(|| SceneError::MissingMap(name.to_owned()))?;
    let data = source.read(&path, opts.max_file_bytes)?;
    let scene: SceneFile = parse_json(&path, &data)?;
    if scene.format != SCENE_FORMAT || scene.version != SCENE_VERSION {
        return Err(SceneError::Format {
            path,
            message: format!(
                "format {} version {} (expected {SCENE_FORMAT} {SCENE_VERSION})",
                scene.format, scene.version
            ),
        });
    }
    if scene.actors.len() > opts.max_actors {
        return Err(SceneError::Format {
            path,
            message: format!("{} actors exceeds the limit", scene.actors.len()),
        });
    }
    Ok(scene)
}

/// Vertices and triangles.
type TriangleSoup = (Vec<Vec3>, Vec<[u32; 3]>);

/// Little-endian `f32`/`u32` readers over a byte slice.
fn read_f32(bin: &[u8], at: usize) -> Option<f32> {
    let b = bin.get(at..at.checked_add(4)?)?;
    Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u32(bin: &[u8], at: usize) -> Option<u32> {
    let b = bin.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Three consecutive 4-byte values at `at` (offsets checked: a hostile
/// offset near `usize::MAX` must not overflow).
fn read3<T>(bin: &[u8], at: usize, read: fn(&[u8], usize) -> Option<T>) -> Option<[T; 3]> {
    Some([
        read(bin, at)?,
        read(bin, at.checked_add(4)?)?,
        read(bin, at.checked_add(8)?)?,
    ])
}

fn read_u16(bin: &[u8], at: usize) -> Option<u16> {
    let b = bin.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// The BSP's blocking triangles (world space), if the map has a BSP file.
fn read_bsp(
    source: &dyn DataSource,
    name: &str,
    opts: &LoadOptions,
) -> Result<Option<TriangleSoup>, SceneError> {
    let Some(path) = find_level_file(source, name, ".bsp.json") else {
        return Ok(None);
    };
    let index: BspIndexJson = parse_json(&path, &source.read(&path, opts.max_file_bytes)?)?;
    let bad = |message: String| SceneError::Format {
        path: path.clone(),
        message,
    };
    if index.format != BSP_FORMAT {
        return Err(bad(format!("format {}", index.format)));
    }
    let Some(spans) = index.meshes.get("collision").copied() else {
        return Ok(None);
    };
    if spans.triangles.count > opts.max_mesh_triangles
        || spans.positions.count > opts.max_mesh_elements()
    {
        return Err(bad("too many BSP triangles or vertices".to_owned()));
    }
    let bin_path = format!("levels/{}", index.bin);
    let bin = source.read(&bin_path, opts.max_file_bytes)?;
    let mut positions = Vec::new();
    for k in 0..spans.positions.count {
        let at = k
            .checked_mul(12)
            .and_then(|o| o.checked_add(spans.positions.offset))
            .ok_or_else(|| bad("position offset overflow".to_owned()))?;
        let Some([x, y, z]) = read3(&bin, at, read_f32) else {
            return Err(bad("positions exceed the .bin".to_owned()));
        };
        positions.push(Vec3::new(x, y, z));
    }
    let mut tris = Vec::new();
    for k in 0..spans.triangles.count {
        let at = k
            .checked_mul(12)
            .and_then(|o| o.checked_add(spans.triangles.offset))
            .ok_or_else(|| bad("triangle offset overflow".to_owned()))?;
        let Some(t) = read3(&bin, at, read_u32) else {
            return Err(bad("triangles exceed the .bin".to_owned()));
        };
        tris.push(t);
    }
    Ok(Some((positions, tris)))
}

/// Element layout of a glTF accessor.
fn accessor_layout(acc: &GltfAccessor) -> Option<(usize, usize)> {
    let comp = match acc.component_type {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    };
    let n = match acc.kind.as_str() {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => return None,
    };
    Some((comp, n))
}

/// Byte offsets of every element of an accessor, bounds-checked against the
/// view and the buffer, for at most `max_count` elements (larger accessors
/// are refused before anything is allocated).
fn accessor_offsets(
    g: &GltfJson,
    index: usize,
    bin_len: usize,
    max_count: usize,
) -> Option<(Vec<usize>, &GltfAccessor)> {
    let acc = g.accessors.get(index)?;
    if acc.count > max_count {
        return None;
    }
    let (comp, n) = accessor_layout(acc)?;
    let elem = comp.checked_mul(n)?;
    let view = g.buffer_views.get(acc.buffer_view?)?;
    if view.buffer != 0 {
        return None;
    }
    let stride = view.byte_stride.unwrap_or(elem);
    if stride < elem {
        return None;
    }
    let view_end = view.byte_offset.checked_add(view.byte_length)?;
    if view_end > bin_len {
        return None;
    }
    let start = view.byte_offset.checked_add(acc.byte_offset)?;
    if acc.count == 0 {
        return Some((Vec::new(), acc));
    }
    let last = start
        .checked_add(stride.checked_mul(acc.count - 1)?)?
        .checked_add(elem)?;
    if last > view_end {
        return None;
    }
    Some(((0..acc.count).map(|k| start + k * stride).collect(), acc))
}

/// Positions of a `VEC3`/`FLOAT` accessor converted back to UE3 local axes.
fn read_positions(
    g: &GltfJson,
    index: usize,
    bin: &[u8],
    scale: f32,
    max_count: usize,
) -> Option<Vec<Vec3>> {
    let (offsets, acc) = accessor_offsets(g, index, bin.len(), max_count)?;
    if acc.component_type != 5126 || acc.kind != "VEC3" {
        return None;
    }
    let inv = if scale.is_finite() && scale > 0.0 {
        1.0 / scale
    } else {
        1.0
    };
    offsets
        .into_iter()
        .map(|at| {
            let [x, y, z] = read3(bin, at, read_f32)?;
            // glTF = (ue.y, ue.z, -ue.x) · scale.
            let v = Vec3::new(-z, x, y) * inv;
            v.is_finite().then_some(v)
        })
        .collect()
}

fn read_indices(g: &GltfJson, index: usize, bin: &[u8], max_count: usize) -> Option<Vec<u32>> {
    let (offsets, acc) = accessor_offsets(g, index, bin.len(), max_count)?;
    if acc.kind != "SCALAR" {
        return None;
    }
    offsets
        .into_iter()
        .map(|at| match acc.component_type {
            5121 => bin.get(at).map(|b| u32::from(*b)),
            5123 => read_u16(bin, at).map(u32::from),
            5125 => read_u32(bin, at),
            _ => None,
        })
        .collect()
}

/// Collision triangles of one converted mesh (UE3 local space).
fn load_mesh(
    source: &dyn DataSource,
    entry: &MeshEntryJson,
    default_scale: f32,
    opts: &LoadOptions,
) -> Result<TriangleSoup, String> {
    let lod = entry
        .lods
        .iter()
        .find(|l| l.lod == 0)
        .or_else(|| entry.lods.first())
        .ok_or("no LOD")?;
    let gltf_path = format!("meshes/{}", lod.gltf);
    let bin_path = format!("meshes/{}", lod.bin);
    let g: GltfJson = parse_json(
        &gltf_path,
        &source
            .read(&gltf_path, opts.max_file_bytes)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let bin = source
        .read(&bin_path, opts.max_file_bytes)
        .map_err(|e| e.to_string())?;
    let scale = entry.scale.unwrap_or(default_scale);

    // Prefer the UCX_ collision node; else the collision-enabled sections.
    let collision_node = g.nodes.iter().find(|n| {
        n.extras
            .as_ref()
            .and_then(|e| e.get("asamu_collision"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || n.name.as_deref().is_some_and(|s| s.starts_with("UCX_"))
    });
    let mut primitives: Vec<&GltfPrimitive> = Vec::new();
    if let Some(node) = collision_node {
        let mesh = node
            .mesh
            .and_then(|m| g.meshes.get(m))
            .ok_or("collision node without mesh")?;
        primitives.extend(mesh.primitives.iter());
    } else {
        let render = g
            .nodes
            .iter()
            .find_map(|n| n.mesh)
            .and_then(|m| g.meshes.get(m))
            .ok_or("no mesh node")?;
        // Primitives are the non-empty sections in order.
        let sections: Vec<&SectionJson> = lod.sections.iter().filter(|s| s.triangles > 0).collect();
        if sections.len() != render.primitives.len() {
            return Err("sections do not match the primitives".to_owned());
        }
        primitives.extend(
            render
                .primitives
                .iter()
                .zip(sections)
                .filter(|(_, s)| s.collision)
                .map(|(p, _)| p),
        );
    }
    // Every count is bounded before anything is allocated: a primitive's
    // vertices and indices by three per allowed triangle, and the vertices
    // of all primitives together likewise (distinct accessors may all point
    // at the same bytes, so the file size alone does not bound them).
    let max_elements = opts.max_mesh_elements();
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut accessor_base: BTreeMap<usize, u32> = BTreeMap::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for prim in primitives {
        if prim.mode.is_some_and(|m| m != 4) {
            continue;
        }
        let pos = *prim
            .attributes
            .get("POSITION")
            .ok_or("primitive without positions")?;
        let count = g.accessors.get(pos).map_or(0, |a| a.count);
        let base = match accessor_base.get(&pos) {
            Some(b) => *b,
            None => {
                let room = max_elements.saturating_sub(vertices.len());
                if count > room {
                    return Err("too many vertices".to_owned());
                }
                let p =
                    read_positions(&g, pos, &bin, scale, room).ok_or("bad POSITION accessor")?;
                let b = u32::try_from(vertices.len()).map_err(|_| "too many vertices")?;
                vertices.extend(p);
                accessor_base.insert(pos, b);
                b
            }
        };
        let room = max_elements.saturating_sub(triangles.len().saturating_mul(3));
        let idx = match prim.indices {
            Some(i) => {
                if g.accessors.get(i).is_some_and(|a| a.count > room) {
                    return Err("too many triangles".to_owned());
                }
                read_indices(&g, i, &bin, room).ok_or("bad index accessor")?
            }
            None if count <= room => {
                (0..u32::try_from(count).map_err(|_| "too many vertices")?).collect()
            }
            None => return Err("too many triangles".to_owned()),
        };
        if idx.len() % 3 != 0 {
            return Err("index count is not a multiple of 3".to_owned());
        }
        triangles.reserve(idx.len() / 3);
        for t in idx.as_chunks::<3>().0 {
            if t.iter().any(|&i| i as usize >= count) {
                return Err("index out of range".to_owned());
            }
            let shifted = |i: u32| i.checked_add(base).ok_or("too many vertices");
            triangles.push([shifted(t[0])?, shifted(t[1])?, shifted(t[2])?]);
        }
        if triangles.len() > opts.max_mesh_triangles {
            return Err("too many triangles".to_owned());
        }
    }
    // An empty result is legitimate: 31 shipped meshes have no collision
    // triangles at all (MESHES.md).
    Ok((vertices, triangles))
}

/// Effective actor flags and, when the collision type overrides them, the
/// collision component's `(BlockZeroExtent, BlockNonZeroExtent)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveCollision {
    /// `bCollideActors`.
    pub collide: bool,
    /// `bBlockActors`.
    pub block: bool,
    /// The type blocks weapons (zero-extent traces) without blocking actors.
    pub blocks_weapons_only: bool,
    /// Component flags forced by the type.
    pub component: Option<(bool, bool)>,
}

/// The collision type an actor ends up with (see the module docs).
#[must_use]
pub fn effective_collision(actor: &ActorJson) -> EffectiveCollision {
    let forced = match actor.kind.as_str() {
        "falling_rock" | "falling_when_grappled_rock" => Some("COLLIDE_BlockAll"),
        "checkpoint" => Some(
            if param_bool(&actor.params, "bTriggeredFromKismet", false) {
                "COLLIDE_NoCollision"
            } else {
                "COLLIDE_TouchAllButWeapons"
            },
        ),
        _ => None,
    };
    let ct = forced
        .or(actor.collision_type.as_deref())
        .unwrap_or("COLLIDE_CustomDefault");
    let (collide, block, comp, weapons) = match ct {
        "COLLIDE_NoCollision" => (false, false, Some((false, false)), false),
        "COLLIDE_BlockAll" => (true, true, Some((true, true)), false),
        "COLLIDE_BlockAllButWeapons" => (true, true, Some((false, true)), false),
        "COLLIDE_BlockWeapons" | "COLLIDE_BlockWeaponsKickable" => {
            (true, false, Some((true, false)), true)
        }
        "COLLIDE_TouchAll" => (true, false, Some((true, true)), false),
        "COLLIDE_TouchWeapons" => (true, false, Some((true, false)), false),
        "COLLIDE_TouchAllButWeapons" => (true, false, Some((false, true)), false),
        _ => (actor.collide_actors, actor.block_actors, None, false),
    };
    EffectiveCollision {
        collide,
        block,
        blocks_weapons_only: weapons,
        component: comp,
    }
}

/// `(blocks_pawn, blocks_traces)` of a component of `actor`.
#[must_use]
pub fn component_blocking(
    actor: &ActorJson,
    eff: &EffectiveCollision,
    comp: &ComponentJson,
) -> (bool, bool) {
    let is_collision_component = actor
        .instance
        .collision_component
        .as_deref()
        .is_some_and(|p| object_name(p).eq_ignore_ascii_case(&comp.name));
    let (zero, non_zero, collide, block) = match eff.component {
        Some((z, nz)) if is_collision_component || actor.instance.collision_component.is_none() => {
            (z, nz, eff.collide, eff.block)
        }
        _ => (
            comp.block_zero_extent,
            comp.block_non_zero_extent,
            comp.collide_actors,
            comp.block_actors,
        ),
    };
    let pawn = eff.collide && eff.block && collide && block && non_zero;
    let traces = eff.collide && collide && zero && (eff.block || eff.blocks_weapons_only);
    (pawn, traces)
}

/// The surface tag of an actor tag name (case-insensitive like UE3 names);
/// the second value is `false` for `NotGrappleAble`.
#[must_use]
pub fn surface_tag(tag: Option<&str>) -> (SurfaceTag, bool) {
    let Some(t) = tag else {
        return (SurfaceTag::None, true);
    };
    let is = |n: &str| t.eq_ignore_ascii_case(n);
    if is("NotGrappleAble") {
        (SurfaceTag::None, false)
    } else if is("TopOnlyGrappleAble") {
        (SurfaceTag::TopOnlyGrappleAble, true)
    } else if is("BottomOnlyGrappleAble") {
        (SurfaceTag::BottomOnlyGrappleAble, true)
    } else if is("grappleInteractable") {
        (SurfaceTag::GrappleInteractable, true)
    } else if is("NotLandable") {
        (SurfaceTag::NotLandable, true)
    } else {
        (SurfaceTag::None, true)
    }
}

/// The collision class of an actor.
#[must_use]
pub fn collision_class(actor: &ActorJson) -> CollisionClass {
    match actor.kind.as_str() {
        "interp_actor" => CollisionClass::InterpActor,
        "recharge_crystal" => CollisionClass::RechargeCrystal,
        "falling_rock" => CollisionClass::FallingRock,
        "falling_when_grappled_rock" => CollisionClass::FallingWhenGrappledRock,
        _ if actor.class.eq_ignore_ascii_case("asamu.ASAMUGlowFlower") => {
            CollisionClass::GlowFlower
        }
        _ if actor
            .class
            .eq_ignore_ascii_case("asamu.ASAMUInteractable_Actor") =>
        {
            CollisionClass::Interactable
        }
        "blocking_volume" | "volume" | "trigger_volume" | "kill_zone" | "dynamic_kill_zone"
        | "brush" => CollisionClass::BlockingVolume,
        _ => CollisionClass::StaticMesh,
    }
}

/// `true` for the actor kinds whose transform changes during play.
#[must_use]
pub fn is_dynamic_kind(kind: &str) -> bool {
    matches!(kind, "falling_rock" | "falling_when_grappled_rock")
}

/// Mesh cache: resolved manifest key → mesh index (or failure).
struct MeshCache<'a> {
    source: &'a dyn DataSource,
    manifest: Option<MeshManifestJson>,
    /// Lower-case manifest key → the first key (in key order) with that
    /// spelling (variant lookups ignore case without scanning the manifest).
    lower_keys: BTreeMap<String, String>,
    loaded: BTreeMap<String, Option<u32>>,
    total_triangles: usize,
}

impl MeshCache<'_> {
    /// Takes `n` triangles from the scene's budget
    /// ([`LoadOptions::max_total_triangles`]); `false` (and nothing taken)
    /// when they do not fit.
    fn charge(&mut self, n: usize, opts: &LoadOptions) -> bool {
        match self.total_triangles.checked_add(n) {
            Some(t) if t <= opts.max_total_triangles => {
                self.total_triangles = t;
                true
            }
            _ => false,
        }
    }

    /// Resolves a component's static mesh path for the package that placed it.
    fn key(&self, path: &str, package: &str) -> Option<String> {
        let m = self.manifest.as_ref()?;
        let entry = m.meshes.get(path)?;
        if entry
            .differs_in
            .iter()
            .any(|p| p.eq_ignore_ascii_case(package))
        {
            let variant = format!("{path}@{package}");
            if m.meshes.contains_key(&variant) {
                return Some(variant);
            }
            // The importer keys variants by the package's own spelling.
            if let Some(k) = self.lower_keys.get(&variant.to_ascii_lowercase()) {
                return Some(k.clone());
            }
        }
        Some(path.to_owned())
    }

    fn mesh(
        &mut self,
        builder: &mut CollisionSceneBuilder,
        path: &str,
        package: &str,
        opts: &LoadOptions,
        stats: &mut LoadStats,
        warnings: &mut Vec<String>,
    ) -> Option<u32> {
        let Some(key) = self.key(path, package) else {
            if self.manifest.is_some() && !self.loaded.contains_key(path) {
                stats.meshes_failed += 1;
                warnings.push(format!("static mesh {path} is not in the mesh manifest"));
                self.loaded.insert(path.to_owned(), None);
            }
            return None;
        };
        if let Some(r) = self.loaded.get(&key) {
            return *r;
        }
        let m = self.manifest.as_ref()?;
        let entry = m.meshes.get(&key)?;
        let result = match load_mesh(self.source, entry, m.scale, opts) {
            Ok((_, t)) if t.is_empty() => {
                stats.meshes_without_collision += 1;
                self.loaded.insert(key, None);
                return None;
            }
            Ok((v, t)) if self.charge(t.len(), opts) => builder.add_mesh(v, t),
            Ok(_) => {
                warnings.push(format!("{key}: triangle budget exhausted"));
                None
            }
            Err(e) => {
                warnings.push(format!("{key}: {e}"));
                None
            }
        };
        match result {
            Some(_) => stats.meshes_loaded += 1,
            None => stats.meshes_failed += 1,
        }
        self.loaded.insert(key, result);
        result
    }
}

/// Loads `map` (case-insensitive) and its streamed sub-levels from `source`.
///
/// # Errors
/// The map's scene is missing or malformed. Problems with single meshes,
/// sub-levels or the BSP are warnings.
pub fn load_map(
    source: &dyn DataSource,
    map: &str,
    opts: &LoadOptions,
) -> Result<LoadedMap, SceneError> {
    let main = read_scene(source, map, opts)?;
    let mut warnings: Vec<String> = Vec::new();
    let mut stats = LoadStats::default();

    // Levels: the persistent one, then every streamed package.
    let mut scenes: Vec<(SceneFile, SubLevel)> = Vec::new();
    let streaming = main.streaming_levels.clone();
    let main_name = main.package.clone();
    scenes.push((
        main,
        SubLevel {
            name: main_name.clone(),
            streaming_class: None,
            initially_loaded: true,
            offset: Vec3::ZERO,
            actors: 0,
        },
    ));
    for s in streaming {
        let Some(pkg) = s.package_name.clone() else {
            continue;
        };
        if scenes.len() >= 64 {
            warnings.push("more than 63 streaming levels; the rest are ignored".to_owned());
            break;
        }
        match read_scene(source, &pkg, opts) {
            Ok(scene) => {
                let offset = v3(s.offset);
                let initially_loaded = s.class.eq_ignore_ascii_case("LevelStreamingAlwaysLoaded");
                let name = scene.package.clone();
                scenes.push((
                    scene,
                    SubLevel {
                        name,
                        streaming_class: Some(s.class.clone()),
                        initially_loaded,
                        offset: if offset.is_finite() {
                            offset
                        } else {
                            Vec3::ZERO
                        },
                        actors: 0,
                    },
                ));
            }
            Err(e) => warnings.push(format!("streaming level {pkg}: {e}")),
        }
    }

    let world = match &scenes.first().and_then(|(s, _)| s.world_info.clone()) {
        Some(w) => WorldSettings {
            title: w.title.clone(),
            kill_z: if w.kill_z.is_finite() {
                w.kill_z
            } else {
                f32::MIN
            },
            soft_kill_z: w.soft_kill_z,
            default_gravity_z: w.default_gravity_z,
            global_gravity_z: w.global_gravity_z,
        },
        None => {
            warnings.push("no WorldInfo; KillZ disabled".to_owned());
            WorldSettings {
                title: None,
                kill_z: f32::MIN,
                soft_kill_z: false,
                default_gravity_z: 0.0,
                global_gravity_z: 0.0,
            }
        }
    };

    let manifest = match source.read("meshes/manifest.json", opts.max_file_bytes) {
        Ok(data) => match parse_json::<MeshManifestJson>("meshes/manifest.json", &data) {
            Ok(m) => Some(m),
            Err(e) => {
                warnings.push(e.to_string());
                None
            }
        },
        Err(e) => {
            warnings.push(format!(
                "no static-mesh collision: {e} (run `asamu-import meshes --collision`)"
            ));
            None
        }
    };
    let mut lower_keys: BTreeMap<String, String> = BTreeMap::new();
    for k in manifest.iter().flat_map(|m| m.meshes.keys()) {
        lower_keys
            .entry(k.to_ascii_lowercase())
            .or_insert_with(|| k.clone());
    }
    let mut cache = MeshCache {
        source,
        manifest,
        lower_keys,
        loaded: BTreeMap::new(),
        total_triangles: 0,
    };
    let mut builder = CollisionSceneBuilder::new();
    let mut pending_dynamic: Vec<(u32, u32, Affine, InstanceInfo)> = Vec::new();
    let mut bodies: Vec<DynamicBody> = Vec::new();
    let mut actors = SceneActors::default();

    for (level_index, (scene, info)) in scenes.iter_mut().enumerate() {
        let level = level_index as u8;
        let offset = Affine::from_translation(info.offset.as_dvec3());
        info.actors = scene.actors.len();
        stats.actors += scene.actors.len();

        // BSP.
        match read_bsp(source, &scene.package, opts) {
            Ok(Some((_, t))) if !cache.charge(t.len(), opts) => {
                warnings.push(format!(
                    "BSP of {}: triangle budget exhausted",
                    scene.package
                ));
            }
            Ok(Some((v, t))) => {
                let n = t.len();
                let v: Vec<Vec3> = v.into_iter().map(|p| p + info.offset).collect();
                if let Some(m) = builder.add_mesh(v, t) {
                    stats.bsp_triangles += n;
                    builder.add_static(
                        m,
                        Affine::IDENTITY,
                        InstanceInfo {
                            actor: None,
                            class: CollisionClass::WorldGeometry,
                            tag: SurfaceTag::None,
                            grapple_able: true,
                            blocks_pawn: true,
                            blocks_traces: true,
                            sublevel: level,
                        },
                    );
                }
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("BSP of {}: {e}", scene.package)),
        }

        for actor in &scene.actors {
            let Some(id) = actor_id(level, actor.slot) else {
                warnings.push(format!("{}: slot {} out of range", actor.name, actor.slot));
                continue;
            };
            let eff = effective_collision(actor);
            let (tag, grapple_able) = surface_tag(actor.tag.as_deref());
            let class = collision_class(actor);
            let dynamic = is_dynamic_kind(&actor.kind);
            let mut body = DynamicBody {
                actor: id,
                parts: Vec::new(),
                draw_scale: actor.draw_scale,
                draw_scale3d: v3(actor.draw_scale3d),
                pre_pivot: v3(actor.pre_pivot),
            };
            let actor_affine = Affine::from_row_matrix(&actor.local_to_world).then(&offset);
            for comp in &actor.components {
                if comp.kind != "static_mesh" {
                    continue;
                }
                let (pawn, traces) = component_blocking(actor, &eff, comp);
                if !(pawn || traces) {
                    continue;
                }
                stats.blocking_components += 1;
                let Some(path) = comp.static_mesh.as_deref() else {
                    stats.components_without_mesh += 1;
                    continue;
                };
                let Some(mesh) = cache.mesh(
                    &mut builder,
                    path,
                    &scene.package,
                    opts,
                    &mut stats,
                    &mut warnings,
                ) else {
                    continue;
                };
                let to_world = Affine::from_row_matrix(&comp.local_to_world).then(&offset);
                // A placement without an inverse is baked into a world-space
                // copy of the mesh (see `CollisionSceneBuilder::instance`),
                // which costs its triangles again.
                if !dynamic
                    && to_world.inverse().is_none()
                    && !cache.charge(builder.mesh(mesh).map_or(0, |m| m.triangles().len()), opts)
                {
                    warnings.push(format!(
                        "{}.{}: triangle budget exhausted",
                        actor.name, comp.name
                    ));
                    continue;
                }
                let inst_info = InstanceInfo {
                    actor: Some(id),
                    class,
                    tag,
                    grapple_able,
                    blocks_pawn: pawn,
                    blocks_traces: traces,
                    sublevel: level,
                };
                if dynamic {
                    match actor_affine.inverse() {
                        Some(inv) => {
                            let relative = to_world.then(&inv);
                            pending_dynamic.push((id, mesh, to_world, inst_info));
                            body.parts.push((usize::MAX, relative));
                        }
                        None => warnings.push(format!("{}: singular actor transform", actor.name)),
                    }
                } else if builder.add_static(mesh, to_world, inst_info).is_some() {
                    stats.placed_components += 1;
                } else {
                    warnings.push(format!("{}.{}: unusable transform", actor.name, comp.name));
                }
            }
            if dynamic && !body.parts.is_empty() {
                bodies.push(body);
            }

            // Blocking brush collision (blocking volumes).
            if let Some(vol) = &actor.volume {
                let brush = actor.components.iter().find(|c| c.kind == "brush");
                let blocking = brush.map(|c| component_blocking(actor, &eff, c));
                if let Some((pawn, traces)) = blocking
                    && (pawn || traces)
                {
                    for hull in &vol.hulls {
                        if !cache.charge(hull.triangles.len(), opts) {
                            warnings.push(format!("{}: triangle budget exhausted", actor.name));
                            break;
                        }
                        let verts: Vec<Vec3> =
                            hull.vertices.iter().map(|v| v3(*v) + info.offset).collect();
                        if let Some(m) = builder.add_convex_mesh(verts, hull.triangles.clone()) {
                            stats.blocking_hulls += 1;
                            builder.add_static(
                                m,
                                Affine::IDENTITY,
                                InstanceInfo {
                                    actor: Some(id),
                                    class: CollisionClass::BlockingVolume,
                                    tag,
                                    grapple_able,
                                    blocks_pawn: pawn,
                                    blocks_traces: traces,
                                    sublevel: level,
                                },
                            );
                        }
                    }
                }
            }
        }
        gameplay::collect_actors(&mut actors, scene, level, info.offset, &mut warnings);
    }

    let collision = builder.build();
    // Dynamic instances (need the finished scene for their bounds).
    let mut dynamic: Vec<Instance> = Vec::new();
    let mut next = pending_dynamic.into_iter();
    for body in &mut bodies {
        for part in &mut body.parts {
            let Some((_, mesh, to_world, info)) = next.next() else {
                break;
            };
            match collision.dynamic_instance(mesh, to_world, info) {
                Some(inst) => {
                    part.0 = dynamic.len();
                    dynamic.push(inst);
                }
                None => warnings.push(format!("actor {}: unusable dynamic transform", body.actor)),
            }
        }
        body.parts.retain(|p| p.0 != usize::MAX);
    }
    gameplay::finish_actors(&mut actors, &collision, &dynamic, &bodies);

    Ok(LoadedMap {
        map: main_name,
        levels: scenes.into_iter().map(|(_, l)| l).collect(),
        world,
        collision,
        dynamic,
        bodies,
        actors,
        stats,
        warnings,
    })
}

/// Loads from a converted-data directory on disk (`<dir>/levels`, `<dir>/meshes`).
///
/// # Errors
/// See [`load_map`].
pub fn load_map_from_dir(dir: impl AsRef<Path>, map: &str) -> Result<LoadedMap, SceneError> {
    let source = DirSource::new(dir.as_ref());
    load_map(&source, map, &LoadOptions::default())
}

/// Recomputes an actor's world transform with a new location and rotation
/// (for moving actors; see [`crate::rotation::actor_local_to_world`]).
#[must_use]
pub fn body_transform(body: &DynamicBody, location: Vec3, rotation: [i32; 3]) -> Affine {
    crate::rotation::actor_local_to_world(
        location,
        rotation,
        body.draw_scale,
        body.draw_scale3d,
        body.pre_pivot,
    )
}

/// World position of `p` under an affine (convenience for tests and tools).
#[must_use]
pub fn transform_point(a: &Affine, p: Vec3) -> Vec3 {
    a.point(p.as_dvec3()).as_vec3()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths_are_refused() {
        for p in [
            "../x",
            "a/../b",
            "/etc/passwd",
            "a//b",
            "",
            "C:/x",
            "a\\..\\b",
            "./a",
        ] {
            assert!(safe_components(p).is_err(), "{p}");
        }
        assert!(safe_components("levels/AG-Workshop.scene.json").is_ok());
        let src = DirSource::new("/nonexistent-root");
        assert!(matches!(
            src.read("../secret", 10),
            Err(SceneError::UnsafePath(_))
        ));
        let mut m = MemorySource::new();
        m.insert("levels/a.scene.json", b"{}".to_vec());
        m.insert("levels/sub/b.json", b"{}".to_vec());
        assert_eq!(m.list("levels").unwrap(), vec!["a.scene.json".to_owned()]);
        assert!(matches!(
            m.read("levels/a.scene.json", 1),
            Err(SceneError::TooLarge { .. })
        ));
    }

    #[test]
    fn params_and_tags() {
        let mut p = BTreeMap::new();
        p.insert("Range".to_owned(), serde_json::json!(1000.0));
        p.insert("bEnabled".to_owned(), serde_json::json!(false));
        p.insert("checkpointIndex".to_owned(), serde_json::json!(7));
        p.insert(
            "spawnPointOffset".to_owned(),
            serde_json::json!({"X": -300.0, "Y": 0.0, "Z": 1.5}),
        );
        assert_eq!(param_f32(&p, "range", 0.0), 1000.0);
        assert!(!param_bool(&p, "bEnabled", true));
        assert_eq!(param_i64(&p, "CheckpointIndex", 0), 7);
        assert_eq!(
            param_vec3(&p, "spawnPointOffset"),
            Some(Vec3::new(-300.0, 0.0, 1.5))
        );
        assert_eq!(param_f32(&p, "missing", 2.5), 2.5);
        assert_eq!(
            surface_tag(Some("notgrappleable")),
            (SurfaceTag::None, false)
        );
        assert_eq!(
            surface_tag(Some("NotLandable")),
            (SurfaceTag::NotLandable, true)
        );
        assert_eq!(
            surface_tag(Some("StaticMeshActor")),
            (SurfaceTag::None, true)
        );
        assert_eq!(
            object_name("Pkg.TheWorld.PersistentLevel.Thing_3"),
            "Thing_3"
        );
        assert_eq!(actor_id(1, 5), Some(65_541));
        assert_eq!(actor_id(0, 70_000), None);
    }
}
