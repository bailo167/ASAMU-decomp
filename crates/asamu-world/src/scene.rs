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
//!   sections — the same set, CONFIRMED in `MESHES.md`);
//! - the same manifest's `simple_collision` records (mesh manifest version
//!   2) and the `<Name>.collision.json` shapes they name, through
//!   `asamu_assets` ([`MeshManifest`], [`SimpleCollision`]): each mesh's
//!   switches and its simple body.
//!
//! and builds a [`LoadedMap`]: a [`CollisionScene`] with every blocking
//! static-mesh component placed by its `LocalToWorld` (as its triangles, as
//! the shapes of its simple body, or both: see below), the BSP and the
//! convex hulls of blocking volumes; the moving actors' collision as dynamic
//! instances; the gameplay actors ([`crate::gameplay::SceneActors`]); and
//! the actors that are attached to another, carry one, or are turned by the
//! engine itself ([`PlacedActor`]: `Base`, `bHardAttach`, `Physics`,
//! `RotationRate`), for the game layer that moves them.
//!
//! The JSON structures here are our own, independent of `asamu-ue3`; unknown
//! fields are ignored. Everything read is treated as untrusted: file sizes,
//! counts, indices and glTF accessors are bounds-checked, malformed meshes
//! are skipped with a warning, and nothing panics.
//!
//! # What a static mesh is collided at
//!
//! The original does not collide everything with a mesh's triangles
//! (`docs/PARITY_FINDINGS.md` P1, P2, V3; MESHES.md, "Simple collision").
//! Each mesh has two switches, both on unless the mesh stores `false`, and
//! possibly a simple body (convex elements, boxes, spheres, capsules):
//!
//! | Trace | Switch | Switch on, body | Switch on, no body | Switch `false` |
//! |---|---|---|---|---|
//! | swept (the pawn) | `UseSimpleBoxCollision` | the body | **nothing** | triangles |
//! | line, not a bullet | `UseSimpleLineCollision` | the body | nothing | triangles |
//! | bullet (the grapple) | none | triangles | triangles | triangles |
//!
//! ([`mesh_routes`]). A blocking component therefore becomes up to two kinds
//! of instance of the same actor:
//!
//! - its **triangles** ([`PrimitiveKind::TriangleMesh`]), blocking bullet
//!   traces whenever the component blocks traces, the pawn when the box
//!   switch is `false`, and line traces when the line switch is `false`;
//! - one instance per **convex element and box of its body**
//!   ([`PrimitiveKind::SimpleHull`]; [`crate::simple_body`]), blocking the
//!   pawn when the box switch is on and line traces when the line switch
//!   is. Spheres and capsules never stop a swept trace in the original and
//!   are not placed.
//!
//! A mesh whose box switch is on and that has no body (515 of the shipped
//! game's 882 meshes: rugs, props, light beams) does not stop the pawn at
//! all. [`LoadStats`] counts the pawn-blocking components of each kind
//! (`pawn_simple`, `pawn_triangles`, `pawn_none`).
//!
//! **A placement that is very small has no simple collision at all.** Before
//! it hands a trace to the body, the original's static-mesh check takes the
//! lengths of the three rows of the component's matrix as the placement's
//! scale, and skips the body when their product is not above 1e-4
//! ([`simple_body_checked`]; in the user's macOS executable at 0x100C58AD4
//! to 0x100C58C95 of `UStaticMeshComponent::LineCheck`, the threshold is the
//! double at 0x1016393A0, CONFIRMED by reading it). Uniformly that is a scale
//! of 0.0464 or less. The pawn and line traces then meet nothing of such a
//! placement; bullet traces still go to its triangles. 7 pawn-blocking
//! placements of the shipped game are that small (4 in AG-ParadiseCave: two
//! lanterns and two poles at 0.027 to 0.042; 3 in AG-IceCave: a rock at
//! 0.0116), and the next smallest product is 2.9e-4.
//! `LoadStats::pawn_scale_skipped` counts them (they are in `pawn_none`).
//!
//! Every instance placed from converted data (triangles, body shapes,
//! volume hulls) is under [`crate::collision::NativeRule`]: the box is swept
//! with the original's own rule for leaving separating axes out, which
//! depends on the placement's scale (NATIVE_PHYSICS.md 10.1). The BSP and
//! box elements are swept as exact geometry.
//!
//! **Converted data without simple collision** (a version 1 mesh manifest)
//! still loads: every blocking mesh then stops the pawn at its triangles, as
//! this loader did before, `LoadStats::pawn_approximate` counts them and one
//! warning says that pawn collision is approximate and that the meshes
//! should be converted again. A mesh whose shapes file is missing or
//! malformed falls back the same way, with a warning naming it.
//!
//! # Volumes that can be switched
//!
//! A blocking volume with a `bEnabled` property (`Engine.DynamicBlockingVolume`)
//! collides or not by that property from the start of play, and level script
//! switches it (P5; [`switchable_volume`]). Its hulls are always placed and
//! start switched off ([`Instance::enabled`]) when `bEnabled` is false: 15 of
//! the shipped game's 24. [`set_actor_collision`] (or
//! [`LoadedMap::set_actor_collision`]) switches every instance of an actor
//! by actor id, [`actor_collision`] reads the switch, and
//! `LoadStats::switchable_volumes` / `disabled_volumes` count them.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use asamu_assets::manifest::{MESH_MANIFEST_VERSION, MeshManifest, SimpleCollisionEntry};
use asamu_assets::simple_collision::{MAX_SIMPLE_COLLISION_BYTES, SimpleCollision};
use glam::Vec3;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::SurfaceTag;
use crate::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, Instance, InstanceInfo,
    InstanceOptions, PrimitiveKind,
};
use crate::gameplay::{self, SceneActors};
use crate::simple_body::{self, Body};

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
    /// `RotationRate` as stored (`{"Pitch": .., "Yaw": .., "Roll": ..}`,
    /// rotator units per second); read it with [`Self::rotation_rate`].
    #[serde(rename = "RotationRate", default)]
    pub rotation_rate_value: Option<Value>,
    /// `bIgnoreBaseRotation` as stored; read it with
    /// [`Self::ignore_base_rotation`].
    #[serde(rename = "bIgnoreBaseRotation", default)]
    pub ignore_base_rotation_value: Option<Value>,
}

impl InstanceValues {
    /// The stored `RotationRate` as `(pitch, yaw, roll)` in rotator units
    /// per second. A member that is not stored, not a whole number or
    /// outside the 32-bit range is 0, and so is all of it when nothing is
    /// stored (the class default of every placed class is zero).
    #[must_use]
    pub fn rotation_rate(&self) -> [i32; 3] {
        let member = |name: &str| {
            self.rotation_rate_value
                .as_ref()
                .and_then(|v| v.get(name))
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0)
        };
        [member("Pitch"), member("Yaw"), member("Roll")]
    }

    /// The stored `bIgnoreBaseRotation` (`false` when it is not stored: the
    /// class default).
    #[must_use]
    pub fn ignore_base_rotation(&self) -> bool {
        self.ignore_base_rotation_value
            .as_ref()
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
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
    /// `Base` (object path of the actor this one is attached to).
    #[serde(default)]
    pub base: Option<String>,
    /// `bHardAttach` (`None` when the scene file does not say).
    #[serde(default)]
    pub hard_attach: Option<bool>,
    /// `Physics` (`PHYS_Interpolating`, `PHYS_Rotating`, ...).
    #[serde(default)]
    pub physics: Option<String>,
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

/// `Physics` of a placed actor, as far as the game layer moves actors by it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlacedPhysics {
    /// `PHYS_Interpolating`: a Matinee movement track moves the actor.
    Interpolating,
    /// `PHYS_Rotating`: the actor turns by its `RotationRate`.
    Rotating,
    /// Anything else (`PHYS_None`, ...).
    #[default]
    Other,
}

impl PlacedPhysics {
    /// From the scene file's `physics` value.
    #[must_use]
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some(n) if n.eq_ignore_ascii_case("PHYS_Interpolating") => Self::Interpolating,
            Some(n) if n.eq_ignore_ascii_case("PHYS_Rotating") => Self::Rotating,
            _ => Self::Other,
        }
    }
}

/// A placed actor that takes part in motion the level's script does not
/// spell out: it is attached to another actor (`Base`), it carries one, or
/// the engine turns it by itself (`PHYS_Rotating`, a `RotationRate`). The
/// values are the level's own (LEVEL_FORMAT.md, "Actor fields").
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedActor {
    /// Actor id.
    pub id: u32,
    /// Object name (`InterpActor_51`).
    pub name: String,
    /// Gameplay kind of the scene file (`interp_actor`, `light`, ...).
    pub kind: String,
    /// `Location`, as stored: the sub-level's offset is not applied.
    pub location: Vec3,
    /// `Rotation` (pitch, yaw, roll).
    pub rotation: [i32; 3],
    /// `DrawScale`.
    pub draw_scale: f32,
    /// `DrawScale3D`.
    pub draw_scale3d: Vec3,
    /// `PrePivot`.
    pub pre_pivot: Vec3,
    /// `Physics`.
    pub physics: PlacedPhysics,
    /// `RotationRate` (pitch, yaw, roll), rotator units per second.
    pub rotation_rate: [i32; 3],
    /// The actor it is attached to (`Base`), when that is an actor of the
    /// same level.
    pub base: Option<u32>,
    /// `bHardAttach`.
    pub hard_attach: bool,
    /// `bIgnoreBaseRotation`.
    pub ignore_base_rotation: bool,
    /// `bBlockActors`.
    pub block_actors: bool,
}

/// The [`PlacedActor`]s of one level: every actor with a base in that
/// level, every such base, and every actor in `PHYS_Rotating` or with a
/// non-zero `RotationRate`; sorted by id.
#[must_use]
pub fn placed_actors(scene: &SceneFile, level: u8) -> Vec<PlacedActor> {
    let slots: BTreeMap<&str, usize> = scene
        .actors
        .iter()
        .map(|a| (a.name.as_str(), a.slot))
        .collect();
    // A base is named by its object path; it is an actor of this level when
    // the path's package is the level's and its last part is an actor's name.
    let base_of = |a: &ActorJson| -> Option<u32> {
        let path = a.base.as_deref()?;
        let package = path.split('.').next()?;
        if !package.eq_ignore_ascii_case(&scene.package) {
            return None;
        }
        let slot = *slots.get(object_name(path))?;
        (slot != a.slot).then(|| actor_id(level, slot)).flatten()
    };
    let mut wanted: BTreeSet<u32> = BTreeSet::new();
    for a in &scene.actors {
        let Some(id) = actor_id(level, a.slot) else {
            continue;
        };
        let base = base_of(a);
        let turns = PlacedPhysics::from_name(a.physics.as_deref()) == PlacedPhysics::Rotating
            || a.instance.rotation_rate() != [0; 3];
        if base.is_some() || turns {
            wanted.insert(id);
        }
        wanted.extend(base);
    }
    let mut out: Vec<PlacedActor> = scene
        .actors
        .iter()
        .filter_map(|a| {
            let id = actor_id(level, a.slot).filter(|id| wanted.contains(id))?;
            let location = v3(a.location);
            let draw_scale3d = v3(a.draw_scale3d);
            let pre_pivot = v3(a.pre_pivot);
            (location.is_finite()
                && draw_scale3d.is_finite()
                && pre_pivot.is_finite()
                && a.draw_scale.is_finite())
            .then(|| PlacedActor {
                id,
                name: a.name.clone(),
                kind: a.kind.clone(),
                location,
                rotation: a.rotation,
                draw_scale: a.draw_scale,
                draw_scale3d,
                pre_pivot,
                physics: PlacedPhysics::from_name(a.physics.as_deref()),
                rotation_rate: a.instance.rotation_rate(),
                base: base_of(a),
                hard_attach: a.hard_attach.unwrap_or(false),
                ignore_base_rotation: a.instance.ignore_base_rotation(),
                block_actors: a.block_actors,
            })
        })
        .collect();
    out.sort_by_key(|a| a.id);
    out.dedup_by_key(|a| a.id);
    out
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
    /// Static-mesh components that would block the pawn by their flags ...
    pub pawn_components: usize,
    /// ... which the pawn collides with at the simple body of their mesh
    /// (box switch on, a body with a convex element or a box).
    pub pawn_simple: usize,
    /// ... at the mesh's triangles, because the mesh stores the box switch
    /// `false`.
    pub pawn_triangles: usize,
    /// ... and not at all: the box switch is on and the mesh has no body, or
    /// a body without a shape a swept trace can hit, or the placement is too
    /// small for the original to look at its body
    /// ([`simple_body_checked`]).
    pub pawn_none: usize,
    /// ... of `pawn_none`: components whose mesh has a body the pawn would
    /// meet, placed at a scale whose product is not above
    /// [`SIMPLE_MIN_SCALE_PRODUCT`].
    pub pawn_scale_skipped: usize,
    /// ... at the mesh's triangles only because the simple-collision data is
    /// missing (a mesh manifest older than version 2, or an unreadable
    /// shapes file): approximate.
    pub pawn_approximate: usize,
    /// Simple-body shapes placed (instances).
    pub simple_parts: usize,
    /// Volumes with a `bEnabled` switch (`DynamicBlockingVolume`) that have
    /// blocking hulls ...
    pub switchable_volumes: usize,
    /// ... of which start with their collision switched off.
    pub disabled_volumes: usize,
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
    /// Attached actors, their bases and the actors the engine turns
    /// ([`placed_actors`] of every level), sorted by id.
    pub placed: Vec<PlacedActor>,
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

    /// The [`PlacedActor`] with the id `id`.
    #[must_use]
    pub fn placed_actor(&self, id: u32) -> Option<&PlacedActor> {
        let i = self.placed.binary_search_by_key(&id, |a| a.id).ok()?;
        self.placed.get(i)
    }

    /// The first [`PlacedActor`] named `name` (an object name is unique in
    /// its level; the persistent level comes first).
    #[must_use]
    pub fn placed_actor_named(&self, name: &str) -> Option<&PlacedActor> {
        self.placed.iter().find(|a| a.name == name)
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

/// What a trace against a static mesh is tested against (MESHES.md, "Simple
/// collision").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceRoute {
    /// The mesh's collision triangles.
    Triangles,
    /// The shapes of the mesh's simple body.
    Body,
    /// Nothing: the switch sends the trace to a body the mesh does not have.
    Nothing,
}

/// Where a mesh's switches send the original's traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRoutes {
    /// Swept (non-zero-extent) traces: the pawn's moves.
    pub pawn: TraceRoute,
    /// Zero-extent traces that are not bullet traces. (Bullet traces, the
    /// grapple's among them, always go to the triangles.)
    pub line: TraceRoute,
    /// The routes come from the mesh's record. `false`: the converted data
    /// has none (a mesh manifest older than version 2) and both routes are
    /// the triangles, which is what this loader did before the simple
    /// collision was known and is approximate for the pawn.
    pub recorded: bool,
}

/// The routes of a mesh with the simple-collision record `record`
/// (`docs/PARITY_FINDINGS.md` V3, P1, P2): a trace goes to the simple body
/// when its switch is on (`UseSimpleBoxCollision` for swept traces,
/// `UseSimpleLineCollision` for line traces), and then a mesh without a body
/// is not hit at all; with the switch stored `false` it goes to the
/// triangles.
#[must_use]
pub fn mesh_routes(record: Option<&SimpleCollisionEntry>) -> MeshRoutes {
    let Some(r) = record else {
        return MeshRoutes {
            pawn: TraceRoute::Triangles,
            line: TraceRoute::Triangles,
            recorded: false,
        };
    };
    let route = |switch: bool| {
        if !switch {
            TraceRoute::Triangles
        } else if r.has_body_setup {
            TraceRoute::Body
        } else {
            TraceRoute::Nothing
        }
    };
    MeshRoutes {
        pawn: route(r.use_simple_box_collision),
        line: route(r.use_simple_line_collision),
        recorded: true,
    }
}

/// The product of a placement's three scales at or below which the original
/// does not test a static mesh's simple body at all (the double at
/// 0x1016393A0 in the user's macOS executable, compared at 0x100C58C8D).
pub const SIMPLE_MIN_SCALE_PRODUCT: f64 = 1.0e-4;

/// Whether the original tests the simple body of a static mesh placed by
/// `to_world` (the component's `LocalToWorld`).
///
/// `UStaticMeshComponent::LineCheck` separates the component's matrix into
/// rows of length one and a scale before it calls the body's check
/// (0x100C58AD4 to 0x100C58C95 in the user's macOS executable, CONFIRMED by
/// reading it): the scale on an axis is the length of that row, or zero when
/// the row's squared length is not above 1e-8 (the float at 0x10163FED4);
/// and when the absolute product of the three is not above
/// [`SIMPLE_MIN_SCALE_PRODUCT`] the body is not tested: the trace, with or
/// without an extent, hits nothing of this component on the simple path.
/// Mirroring does not enter (the lengths are positive; the mirror stays in
/// the rows).
///
/// The lengths and the product are computed in `f32` in the original's
/// order, as it does.
#[must_use]
pub fn simple_body_checked(to_world: &Affine) -> bool {
    let scale = |row: glam::DVec3| {
        let (x, y, z) = (row.x as f32, row.y as f32, row.z as f32);
        let squared = z * z + (y * y + x * x);
        // 1e-8 as the original's float holds it.
        if squared > 1.0e-8_f32 {
            squared.sqrt()
        } else {
            0.0
        }
    };
    let [s0, s1, s2] = to_world.rows.map(scale);
    let product = (s1 * s0 * s2).abs();
    f64::from(product) > SIMPLE_MIN_SCALE_PRODUCT
}

/// `bEnabled` of a volume that has that switch (`Engine.DynamicBlockingVolume`
/// and anything derived from it: the only blocking volumes with the
/// property), else `None`.
///
/// At the start of play such a volume sets whether it collides at all from
/// `bEnabled` and keeps its stored `bBlockActors` (CONFIRMED: the two
/// arguments of the one call its start function makes, read locally from the
/// user's `Engine.u`), so one with `bEnabled = false` blocks nothing until
/// level script switches it (`docs/PARITY_FINDINGS.md` P5).
#[must_use]
pub fn switchable_volume(actor: &ActorJson) -> Option<bool> {
    (actor.kind == "blocking_volume")
        .then(|| param(&actor.params, "bEnabled").and_then(Value::as_bool))
        .flatten()
}

/// Switches the collision of the actor `actor` on or off: every one of its
/// instances, static or in `dynamic` (see [`Instance::enabled`]). Returns how
/// many instances the actor has; 0 means it has no collision here.
///
/// This is what level script does to a volume: a Kismet toggle action's
/// "turn on" input makes the actor collide, "turn off" stops it and "toggle"
/// flips it, each keeping the actor's block flag (CONFIRMED for the volume
/// classes: the handler of `Engine.Volume`, read locally from the user's
/// `Engine.u`). Use [`actor_collision`] for the present state when flipping.
pub fn set_actor_collision(
    collision: &mut CollisionScene,
    dynamic: &mut [Instance],
    actor: u32,
    enabled: bool,
) -> usize {
    let mut n = collision.set_actor_enabled(actor, enabled);
    for inst in dynamic.iter_mut().filter(|i| i.info.actor == Some(actor)) {
        inst.enabled = enabled;
        n += 1;
    }
    n
}

/// Whether the collision of the actor `actor` is switched on: the switch of
/// its first instance (they are switched together). `None` when the actor
/// has no instance.
#[must_use]
pub fn actor_collision(
    collision: &CollisionScene,
    dynamic: &[Instance],
    actor: u32,
) -> Option<bool> {
    collision
        .statics()
        .iter()
        .chain(dynamic)
        .find(|i| i.info.actor == Some(actor))
        .map(|i| i.enabled)
}

impl LoadedMap {
    /// [`set_actor_collision`] on this map's own static and dynamic
    /// instances. A game layer that keeps the dynamic instances elsewhere
    /// calls the free function with its list.
    pub fn set_actor_collision(&mut self, actor: u32, enabled: bool) -> usize {
        set_actor_collision(&mut self.collision, &mut self.dynamic, actor, enabled)
    }

    /// [`actor_collision`] on this map's own instances.
    #[must_use]
    pub fn actor_collision(&self, actor: u32) -> Option<bool> {
        actor_collision(&self.collision, &self.dynamic, actor)
    }

    /// The actors whose collision is switched off (volumes that start with
    /// `bEnabled = false`, and whatever was switched since).
    #[must_use]
    pub fn actors_without_collision(&self) -> BTreeSet<u32> {
        self.collision
            .statics()
            .iter()
            .chain(&self.dynamic)
            .filter(|i| !i.enabled)
            .filter_map(|i| i.info.actor)
            .collect()
    }
}

/// One instance a component is placed as.
#[derive(Clone, Copy, Debug)]
struct Placement {
    mesh: u32,
    to_world: Affine,
    info: InstanceInfo,
    kind: PrimitiveKind,
    /// [`Instance::blocks_lines`].
    lines: bool,
    /// Swept with the original's axis rule ([`crate::collision::NativeRule`]).
    native: bool,
}

/// Mesh cache: resolved manifest key → mesh index (or failure).
struct MeshCache<'a> {
    source: &'a dyn DataSource,
    manifest: Option<MeshManifestJson>,
    /// The simple-collision records of the same manifest.
    records: Option<MeshManifest>,
    /// Lower-case manifest key → the first key (in key order) with that
    /// spelling (variant lookups ignore case without scanning the manifest).
    lower_keys: BTreeMap<String, String>,
    loaded: BTreeMap<String, Option<u32>>,
    /// Resolved key → index into `bodies` (or failure).
    body_index: BTreeMap<String, Option<usize>>,
    bodies: Vec<Body>,
    /// Meshes of a manifest with records that have none themselves (warned
    /// about once each).
    unrecorded: BTreeSet<String>,
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

    /// [`Self::key`], with a warning (once per path) for a mesh the manifest
    /// does not have.
    fn resolve(
        &mut self,
        path: &str,
        package: &str,
        stats: &mut LoadStats,
        warnings: &mut Vec<String>,
    ) -> Option<String> {
        let key = self.key(path, package);
        if key.is_none() && self.manifest.is_some() && !self.loaded.contains_key(path) {
            stats.meshes_failed += 1;
            warnings.push(format!("static mesh {path} is not in the mesh manifest"));
            self.loaded.insert(path.to_owned(), None);
        }
        key
    }

    /// The simple-collision record of the mesh `key`.
    fn record(&self, key: &str) -> Option<&SimpleCollisionEntry> {
        self.records.as_ref()?.simple_collision.get(key)
    }

    /// The collision triangles of the mesh `key` as a mesh of the builder.
    fn triangles(
        &mut self,
        builder: &mut CollisionSceneBuilder,
        key: &str,
        opts: &LoadOptions,
        stats: &mut LoadStats,
        warnings: &mut Vec<String>,
    ) -> Option<u32> {
        if let Some(r) = self.loaded.get(key) {
            return *r;
        }
        let m = self.manifest.as_ref()?;
        let entry = m.meshes.get(key)?;
        let result = match load_mesh(self.source, entry, m.scale, opts) {
            Ok((_, t)) if t.is_empty() => {
                stats.meshes_without_collision += 1;
                self.loaded.insert(key.to_owned(), None);
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
        self.loaded.insert(key.to_owned(), result);
        result
    }

    /// Reads and builds the simple body of the mesh `key`.
    fn load_body(
        &mut self,
        builder: &mut CollisionSceneBuilder,
        key: &str,
        opts: &LoadOptions,
    ) -> Result<Body, String> {
        let record = self.record(key).ok_or("no simple-collision record")?;
        let rel = record
            .safe_shapes()
            .map_err(|e| e.to_string())?
            .ok_or("the record names no shapes file")?;
        let path = format!("meshes/{rel}");
        let limit = opts.max_file_bytes.min(MAX_SIMPLE_COLLISION_BYTES);
        let data = self.source.read(&path, limit).map_err(|e| e.to_string())?;
        let shapes =
            SimpleCollision::from_json(Path::new(&path), &data).map_err(|e| e.to_string())?;
        let mut total = self.total_triangles;
        let max = opts.max_total_triangles;
        let body =
            simple_body::build_body(builder, &shapes, &mut |n| match total.checked_add(n) {
                Some(t) if t <= max => {
                    total = t;
                    true
                }
                _ => false,
            })?;
        self.total_triangles = total;
        Ok(body)
    }

    /// The simple body of the mesh `key` (an index into `self.bodies`), or
    /// `None` with a warning (once) when its shapes cannot be used.
    fn body(
        &mut self,
        builder: &mut CollisionSceneBuilder,
        key: &str,
        opts: &LoadOptions,
        warnings: &mut Vec<String>,
    ) -> Option<usize> {
        if let Some(r) = self.body_index.get(key) {
            return *r;
        }
        let result = match self.load_body(builder, key, opts) {
            Ok(body) => {
                if body.skipped > 0 {
                    warnings.push(format!(
                        "{key}: {} simple-collision shapes are unusable and were left out",
                        body.skipped
                    ));
                }
                self.bodies.push(body);
                Some(self.bodies.len() - 1)
            }
            Err(e) => {
                warnings.push(format!(
                    "{key}: simple collision unusable ({e}); its triangles are used instead, \
                     which is approximate"
                ));
                None
            }
        };
        self.body_index.insert(key.to_owned(), result);
        result
    }
}

/// The instances a blocking static-mesh component is placed as, by the
/// routes of its mesh (see [`mesh_routes`]): the triangles for whatever goes
/// to them, and the shapes of the simple body for the rest. Bullet traces
/// (`traces`) always go to the triangles.
#[allow(clippy::too_many_arguments)]
fn component_placements(
    cache: &mut MeshCache<'_>,
    builder: &mut CollisionSceneBuilder,
    key: &str,
    to_world: Affine,
    base: InstanceInfo,
    opts: &LoadOptions,
    stats: &mut LoadStats,
    warnings: &mut Vec<String>,
) -> Vec<Placement> {
    let (pawn, traces) = (base.blocks_pawn, base.blocks_traces);
    let mut routes = mesh_routes(cache.record(key));
    if !routes.recorded
        && cache
            .records
            .as_ref()
            .is_some_and(|r| !r.simple_collision.is_empty())
        && cache.unrecorded.insert(key.to_owned())
    {
        warnings.push(format!(
            "{key}: no simple-collision record in the mesh manifest; its triangles are used for \
             the pawn, which is approximate"
        ));
    }
    // A placement too small for the original to look at its body: both
    // routes to the body lead to nothing (bullet traces keep the triangles).
    let mut scale_skipped = false;
    if !simple_body_checked(&to_world) {
        if routes.pawn == TraceRoute::Body {
            routes.pawn = TraceRoute::Nothing;
            // Counted below only when the body has a shape the pawn would
            // have met.
            scale_skipped = pawn && cache.record(key).is_some_and(|r| r.convex + r.boxes > 0);
        }
        if routes.line == TraceRoute::Body {
            routes.line = TraceRoute::Nothing;
        }
    }
    let needs_body =
        (pawn && routes.pawn == TraceRoute::Body) || (traces && routes.line == TraceRoute::Body);
    let body = if needs_body {
        cache.body(builder, key, opts, warnings)
    } else {
        None
    };
    if needs_body && body.is_none() {
        // Unusable shapes: fall back to the triangles rather than to nothing.
        if routes.pawn == TraceRoute::Body {
            routes.pawn = TraceRoute::Triangles;
        }
        if routes.line == TraceRoute::Body {
            routes.line = TraceRoute::Triangles;
        }
        routes.recorded = false;
    }
    let parts: &[simple_body::BodyPart] = body
        .and_then(|b| cache.bodies.get(b))
        .map_or(&[], |b| b.parts.as_slice());
    let parts = parts.to_vec();
    if pawn {
        stats.pawn_components += 1;
        match routes.pawn {
            TraceRoute::Body if !parts.is_empty() => stats.pawn_simple += 1,
            TraceRoute::Body | TraceRoute::Nothing => {
                stats.pawn_none += 1;
                if scale_skipped {
                    stats.pawn_scale_skipped += 1;
                }
            }
            TraceRoute::Triangles if routes.recorded => stats.pawn_triangles += 1,
            TraceRoute::Triangles => stats.pawn_approximate += 1,
        }
    }
    if traces
        && routes.line == TraceRoute::Body
        && body
            .and_then(|b| cache.bodies.get(b))
            .is_some_and(|b| b.untraced > 0)
    {
        warnings.push(format!(
            "{key}: line traces go to a simple body with spheres or capsules, which are not traced"
        ));
    }
    let mut out = Vec::new();
    let tri_pawn = pawn && routes.pawn == TraceRoute::Triangles;
    let tri_lines = traces && routes.line == TraceRoute::Triangles;
    if (tri_pawn || traces)
        && let Some(mesh) = cache.triangles(builder, key, opts, stats, warnings)
    {
        out.push(Placement {
            mesh,
            to_world,
            info: InstanceInfo {
                blocks_pawn: tri_pawn,
                blocks_traces: traces,
                ..base
            },
            kind: PrimitiveKind::TriangleMesh,
            lines: tri_lines,
            native: true,
        });
    }
    let body_pawn = pawn && routes.pawn == TraceRoute::Body;
    let body_lines = traces && routes.line == TraceRoute::Body;
    if body_pawn || body_lines {
        for part in parts {
            out.push(Placement {
                mesh: part.mesh,
                to_world: part.local.then(&to_world),
                info: InstanceInfo {
                    blocks_pawn: body_pawn,
                    blocks_traces: false,
                    ..base
                },
                kind: PrimitiveKind::SimpleHull,
                lines: body_lines,
                native: part.convex,
            });
        }
    }
    out
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

    let mut records: Option<MeshManifest> = None;
    let manifest = match source.read("meshes/manifest.json", opts.max_file_bytes) {
        Ok(data) => match parse_json::<MeshManifestJson>("meshes/manifest.json", &data) {
            Ok(m) => {
                // The same file through the asset crate's reader, for the
                // simple-collision records (mesh manifest version 2).
                match MeshManifest::from_json(Path::new("meshes/manifest.json"), &data) {
                    Ok(r) => {
                        if r.version < MESH_MANIFEST_VERSION
                            || (r.simple_collision.is_empty() && !r.is_empty())
                        {
                            warnings.push(format!(
                                "meshes/manifest.json (version {}) has no simple-collision data: \
                                 the pawn collides with every mesh's triangles, which is \
                                 approximate (the original collides it with the meshes' simple \
                                 bodies, and not at all with a mesh that has none); convert the \
                                 meshes again with the current `asamu-import`",
                                r.version
                            ));
                        }
                        records = Some(r);
                    }
                    Err(e) => warnings.push(format!(
                        "no simple-collision data ({e}): pawn collision is approximate"
                    )),
                }
                Some(m)
            }
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
        records,
        lower_keys,
        loaded: BTreeMap::new(),
        body_index: BTreeMap::new(),
        bodies: Vec::new(),
        unrecorded: BTreeSet::new(),
        total_triangles: 0,
    };
    let mut builder = CollisionSceneBuilder::new();
    let mut pending_dynamic: Vec<Placement> = Vec::new();
    let mut bodies: Vec<DynamicBody> = Vec::new();
    let mut placed: Vec<PlacedActor> = Vec::new();
    let mut actors = SceneActors::default();

    for (level_index, (scene, info)) in scenes.iter_mut().enumerate() {
        let level = level_index as u8;
        placed.extend(placed_actors(scene, level));
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
                let Some(key) = cache.resolve(path, &scene.package, &mut stats, &mut warnings)
                else {
                    continue;
                };
                let to_world = Affine::from_row_matrix(&comp.local_to_world).then(&offset);
                let base = InstanceInfo {
                    actor: Some(id),
                    class,
                    tag,
                    grapple_able,
                    blocks_pawn: pawn,
                    blocks_traces: traces,
                    sublevel: level,
                };
                let placements = component_placements(
                    &mut cache,
                    &mut builder,
                    &key,
                    to_world,
                    base,
                    opts,
                    &mut stats,
                    &mut warnings,
                );
                if dynamic {
                    match actor_affine.inverse() {
                        Some(inv) => {
                            for p in placements {
                                body.parts.push((usize::MAX, p.to_world.then(&inv)));
                                pending_dynamic.push(p);
                            }
                        }
                        None => warnings.push(format!("{}: singular actor transform", actor.name)),
                    }
                    continue;
                }
                let mut placed = false;
                for p in placements {
                    // A placement without an inverse is baked into a
                    // world-space copy of the mesh (see
                    // `CollisionSceneBuilder::instance`), which costs its
                    // triangles again.
                    let invertible = p.to_world.inverse().is_some();
                    if !invertible
                        && !cache.charge(
                            builder.mesh(p.mesh).map_or(0, |m| m.triangles().len()),
                            opts,
                        )
                    {
                        warnings.push(format!(
                            "{}.{}: triangle budget exhausted",
                            actor.name, comp.name
                        ));
                        continue;
                    }
                    let options = InstanceOptions::of_kind(p.kind);
                    match builder.add_static_with(p.mesh, p.to_world, p.info, options) {
                        Some(index) => {
                            if let Some(inst) = builder.static_mut(index) {
                                inst.blocks_lines = p.lines;
                            }
                            if p.native && invertible {
                                builder.set_static_native(index, None);
                            }
                            if p.kind == PrimitiveKind::SimpleHull {
                                stats.simple_parts += 1;
                            }
                            placed = true;
                        }
                        None => warnings
                            .push(format!("{}.{}: unusable transform", actor.name, comp.name)),
                    }
                }
                if placed {
                    stats.placed_components += 1;
                }
            }
            if dynamic && !body.parts.is_empty() {
                bodies.push(body);
            }

            // Blocking brush collision (blocking volumes).
            if let Some(vol) = &actor.volume {
                let brush = actor.components.iter().find(|c| c.kind == "brush");
                // A volume with the `bEnabled` switch collides or not by that
                // switch, whatever its stored actor flag says; its hulls are
                // placed either way and start switched off when it is false.
                let switch = switchable_volume(actor);
                let flags = if switch.is_some() {
                    EffectiveCollision {
                        collide: true,
                        ..eff
                    }
                } else {
                    eff
                };
                let blocking = brush.map(|c| component_blocking(actor, &flags, c));
                if let Some((pawn, traces)) = blocking
                    && (pawn || traces)
                {
                    let options = InstanceOptions {
                        kind: Some(PrimitiveKind::BrushHull),
                        enabled: switch.unwrap_or(true),
                    };
                    // The space the original sweeps the hulls in: the brush
                    // component's own (the hulls are stored in world space).
                    let element_to_world =
                        brush.map(|c| Affine::from_row_matrix(&c.local_to_world).then(&offset));
                    let mut hulls = 0usize;
                    for hull in &vol.hulls {
                        if !cache.charge(hull.triangles.len(), opts) {
                            warnings.push(format!("{}: triangle budget exhausted", actor.name));
                            break;
                        }
                        let verts: Vec<Vec3> =
                            hull.vertices.iter().map(|v| v3(*v) + info.offset).collect();
                        if let Some(m) = builder.add_convex_mesh(verts, hull.triangles.clone()) {
                            let index = builder.add_static_with(
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
                                options,
                            );
                            if let Some(index) = index {
                                stats.blocking_hulls += 1;
                                hulls += 1;
                                if element_to_world.is_some_and(|t| t.inverse().is_some()) {
                                    builder.set_static_native(index, element_to_world);
                                }
                            }
                        }
                    }
                    if let Some(enabled) = switch
                        && hulls > 0
                    {
                        stats.switchable_volumes += 1;
                        if !enabled {
                            stats.disabled_volumes += 1;
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
            let Some(p) = next.next() else {
                break;
            };
            let options = InstanceOptions::of_kind(p.kind);
            match collision.dynamic_instance_with(p.mesh, p.to_world, p.info, options) {
                Some(mut inst) => {
                    inst.blocks_lines = p.lines;
                    if p.native {
                        collision.set_native(&mut inst, None);
                    }
                    if p.kind == PrimitiveKind::SimpleHull {
                        stats.simple_parts += 1;
                    }
                    part.0 = dynamic.len();
                    dynamic.push(inst);
                }
                None => warnings.push(format!("actor {}: unusable dynamic transform", body.actor)),
            }
        }
        body.parts.retain(|p| p.0 != usize::MAX);
    }
    // The sizes of gameplay actors come from the geometry that is drawn and
    // traced (triangles, hulls of volumes), as before simple bodies were
    // placed; an actor that has nothing but simple-body shapes is sized by
    // those.
    let sized: BTreeSet<u32> = collision
        .statics()
        .iter()
        .chain(&dynamic)
        .filter(|i| i.kind != PrimitiveKind::SimpleHull)
        .filter_map(|i| i.info.actor)
        .collect();
    let sizing: Vec<Instance> = collision
        .statics()
        .iter()
        .chain(&dynamic)
        .filter(|i| {
            i.kind != PrimitiveKind::SimpleHull || i.info.actor.is_some_and(|a| !sized.contains(&a))
        })
        .cloned()
        .collect();
    gameplay::finish_actors(&mut actors, &CollisionScene::empty(), &sizing, &bodies);

    Ok(LoadedMap {
        map: main_name,
        levels: scenes.into_iter().map(|(_, l)| l).collect(),
        world,
        collision,
        dynamic,
        bodies,
        placed,
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

    /// A hand-written level: a hub in `PHYS_Rotating` with a rate, a rock
    /// hard-attached to it, a lamp attached to the rock, a mover with a
    /// stored rate in `PHYS_Interpolating`, and actors that take no part.
    fn wheel_scene() -> SceneFile {
        let text = serde_json::json!({
            "format": SCENE_FORMAT, "version": SCENE_VERSION, "package": "Wheel",
            "actors": [
                {"slot": 4, "name": "Hub", "class": "Engine.InterpActor", "kind": "interp_actor",
                 "location": [100.0, 200.0, 300.0], "rotation": [0, 16384, 0],
                 "draw_scale": 2.0, "pre_pivot": [0.0, 50.0, 0.0],
                 "physics": "PHYS_Rotating",
                 "instance": {"RotationRate": {"Pitch": 0, "Roll": 0, "Yaw": -9102}}},
                {"slot": 9, "name": "Rock", "class": "Engine.InterpActor", "kind": "interp_actor",
                 "location": [100.0, 700.0, 300.0], "physics": "PHYS_Interpolating",
                 "base": "wheel.TheWorld.PersistentLevel.Hub", "hard_attach": true,
                 "block_actors": true},
                {"slot": 2, "name": "Lamp", "class": "Engine.PointLightMovable", "kind": "light",
                 "base": "Wheel.TheWorld.PersistentLevel.Rock", "hard_attach": null,
                 "instance": {"bIgnoreBaseRotation": true}},
                {"slot": 7, "name": "Mover", "class": "Engine.InterpActor", "kind": "interp_actor",
                 "physics": "PHYS_Interpolating",
                 "instance": {"RotationRate": {"Roll": 4551, "Pitch": 1.5, "Yaw": "x"}}},
                {"slot": 11, "name": "Still", "class": "Engine.InterpActor", "kind": "interp_actor",
                 "physics": "PHYS_Interpolating", "instance": {"RotationRate": {"Yaw": 0}}},
                {"slot": 12, "name": "Elsewhere", "class": "Engine.StaticMeshActor",
                 "kind": "static_mesh", "base": "Other.TheWorld.PersistentLevel.Hub"},
                {"slot": 13, "name": "Self", "class": "Engine.StaticMeshActor",
                 "kind": "static_mesh", "base": "Wheel.TheWorld.PersistentLevel.Self"},
                {"slot": 14, "name": "Lost", "class": "Engine.StaticMeshActor",
                 "kind": "static_mesh", "base": "Wheel.TheWorld.PersistentLevel.Gone"},
            ],
        });
        serde_json::from_value(text).expect("the scene parses")
    }

    #[test]
    fn placed_actors_are_the_attached_their_bases_and_the_turning() {
        let scene = wheel_scene();
        let placed = placed_actors(&scene, 3);
        let id = |slot: usize| actor_id(3, slot).unwrap();
        // Sorted by id (slot): the lamp (2), the hub (4), the mover (7), the
        // rock (9). The actor without a rate, the one whose base is in
        // another package, the one based on itself and the one whose base
        // does not exist take no part.
        assert_eq!(
            placed.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Lamp", "Hub", "Mover", "Rock"]
        );
        let hub = &placed[1];
        assert_eq!(hub.id, id(4));
        assert_eq!(hub.kind, "interp_actor");
        assert_eq!(hub.location, Vec3::new(100.0, 200.0, 300.0));
        assert_eq!(hub.rotation, [0, 16_384, 0]);
        assert_eq!(hub.draw_scale, 2.0);
        assert_eq!(hub.draw_scale3d, Vec3::ONE);
        assert_eq!(hub.pre_pivot, Vec3::new(0.0, 50.0, 0.0));
        assert_eq!(hub.physics, PlacedPhysics::Rotating);
        // (pitch, yaw, roll).
        assert_eq!(hub.rotation_rate, [0, -9102, 0]);
        assert_eq!(hub.base, None);
        let rock = &placed[3];
        // The base's package is matched without regard to case.
        assert_eq!(rock.base, Some(id(4)));
        assert!(rock.hard_attach);
        assert!(rock.block_actors && !hub.block_actors);
        assert_eq!(rock.physics, PlacedPhysics::Interpolating);
        assert_eq!(rock.rotation_rate, [0; 3]);
        let lamp = &placed[0];
        assert_eq!(lamp.base, Some(id(9)));
        assert!(!lamp.hard_attach, "a null flag is false");
        assert!(lamp.ignore_base_rotation);
        assert_eq!(lamp.physics, PlacedPhysics::Other);
        // A member that is not a whole number is 0; the others stay.
        let mover = &placed[2];
        assert_eq!(mover.rotation_rate, [0, 0, 4551]);
        assert_eq!(mover.base, None);
    }
}
