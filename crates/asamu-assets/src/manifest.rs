//! Runtime views of the manifests written by `asamu-import`.
//!
//! Only the fields the runtime needs are read; unknown fields are ignored, so
//! the importer can add information without breaking the runtime. The
//! writer side lives in `tools/asamu-import/src/{textures,meshes}.rs`:
//!
//! | Manifest | Written by | Keyed by |
//! |---|---|---|
//! | `textures/manifest.json` | `asamu-import textures` | texture object path (`Pkg.Group.Name`) |
//! | `meshes/manifest.json` | `asamu-import meshes` | static mesh object path; a copy that differs in another package is keyed `Path@Package` |
//!
//! Every file path inside a manifest is validated with
//! [`crate::files::safe_relative_path`] before use.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

use crate::error::AssetResult;
use crate::files::safe_relative_path;

/// Manifest format version the texture importer writes today (and the mesh
/// importer wrote before [`MESH_MANIFEST_VERSION`]).
pub const MANIFEST_VERSION: u32 = 1;

/// Mesh manifest format version the mesh importer writes today: since
/// version 2 every mesh entry carries `simple_collision`
/// ([`SimpleCollisionEntry`]). Older manifests still load; their meshes have
/// no simple-collision record ([`MeshManifest::simple_collision_for_package`]
/// returns `None`), which means "convert again", not "no simple collision".
pub const MESH_MANIFEST_VERSION: u32 = 2;

/// Converts a path to the lower-case key used for case-insensitive lookups
/// (UE3 object names compare without regard to case).
fn fold(path: &str) -> String {
    path.to_ascii_lowercase()
}

// ------------------------------------------------------------ textures

/// One converted texture (`textures/manifest.json`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TextureEntry {
    /// Package the data came from.
    #[serde(default)]
    pub package: String,
    /// UE3 class (`Texture2D`, `LightMapTexture2D`, ...).
    #[serde(default)]
    pub class: String,
    /// DDS file relative to `textures/`.
    pub file: String,
    /// Original pixel format name (`PF_DXT1`, ...).
    #[serde(default)]
    pub format: String,
    /// Texture size.
    #[serde(default)]
    pub size: [u32; 2],
    /// Mips written.
    #[serde(default)]
    pub mips: u32,
    /// Cube map.
    #[serde(default)]
    pub cube: bool,
    /// `SRGB` (absent when unknown).
    #[serde(default)]
    pub srgb: Option<bool>,
    /// `AddressX` / `AddressY` (`TA_Wrap`, `TA_Clamp`, `TA_Mirror`).
    #[serde(default)]
    pub address: Option<[String; 2]>,
    /// `LODGroup`.
    #[serde(default)]
    pub lod_group: Option<String>,
    /// `CompressionSettings`.
    #[serde(default)]
    pub compression_settings: Option<String>,
}

/// UE3 texture address mode, as needed by a sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressMode {
    /// `TA_Wrap` (also the default when unknown).
    Wrap,
    /// `TA_Clamp`.
    Clamp,
    /// `TA_Mirror`.
    Mirror,
}

impl AddressMode {
    /// Parses a UE3 `TextureAddress` enumerator name (unknown names wrap,
    /// the enum's first value and its default).
    #[must_use]
    pub fn from_ue(name: &str) -> Self {
        match name {
            "TA_Clamp" => Self::Clamp,
            "TA_Mirror" => Self::Mirror,
            _ => Self::Wrap,
        }
    }
}

impl TextureEntry {
    /// The validated DDS path relative to `textures/`.
    ///
    /// # Errors
    /// An unsafe path in the manifest.
    pub fn safe_file(&self) -> AssetResult<String> {
        safe_relative_path(&self.file)
    }

    /// Sampler address modes (U, V).
    #[must_use]
    pub fn address_modes(&self) -> [AddressMode; 2] {
        match &self.address {
            Some([u, v]) => [AddressMode::from_ue(u), AddressMode::from_ue(v)],
            None => [AddressMode::Wrap; 2],
        }
    }

    /// Whether the texels are sRGB-encoded colour. Unknown (`srgb` absent)
    /// defaults to `true`, the UE3 `Texture.SRGB` class default; normal maps
    /// are linear whatever the flag says.
    #[must_use]
    pub fn is_srgb(&self) -> bool {
        if self.is_normal_map() {
            return false;
        }
        self.srgb.unwrap_or(true)
    }

    /// Normal map by its LOD group or compression setting.
    #[must_use]
    pub fn is_normal_map(&self) -> bool {
        self.lod_group
            .as_deref()
            .is_some_and(|g| g.eq_ignore_ascii_case("TEXTUREGROUP_WorldNormalMap"))
            || self
                .compression_settings
                .as_deref()
                .is_some_and(|c| c.starts_with("TC_Normalmap"))
    }
}

#[derive(Debug, Deserialize)]
struct RawTextureManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    textures: BTreeMap<String, TextureEntry>,
}

/// `textures/manifest.json` with case-insensitive lookup.
#[derive(Debug, Clone, Default)]
pub struct TextureManifest {
    /// Format version.
    pub version: u32,
    /// Entries by object path.
    pub textures: BTreeMap<String, TextureEntry>,
    folded: HashMap<String, String>,
}

impl TextureManifest {
    /// Parses the manifest JSON.
    ///
    /// # Errors
    /// Malformed JSON.
    pub fn from_json(path: &std::path::Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawTextureManifest = crate::files::parse_json(path, data)?;
        Ok(Self::from_entries(raw.version, raw.textures))
    }

    /// Builds a manifest from entries (tests, tools).
    #[must_use]
    pub fn from_entries(version: u32, textures: BTreeMap<String, TextureEntry>) -> Self {
        let folded = textures.keys().map(|k| (fold(k), k.clone())).collect();
        Self {
            version,
            textures,
            folded,
        }
    }

    /// Looks a texture up by object path: exact first, then without regard
    /// to ASCII case.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<(&str, &TextureEntry)> {
        if let Some((k, v)) = self.textures.get_key_value(path) {
            return Some((k.as_str(), v));
        }
        let key = self.folded.get(&fold(path))?;
        self.textures
            .get_key_value(key)
            .map(|(k, v)| (k.as_str(), v))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    /// True when there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}

// ------------------------------------------------------------ meshes

/// One section of an exported LOD.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SectionEntry {
    /// UE3 material path of the section (`None` = unassigned slot).
    #[serde(default)]
    pub material: Option<String>,
    /// First index in the LOD's index buffer.
    #[serde(default)]
    pub first_index: u32,
    /// Triangles.
    #[serde(default)]
    pub triangles: u32,
    /// `EnableCollision`.
    #[serde(default)]
    pub collision: bool,
    /// `bEnableShadowCasting`.
    #[serde(default = "default_true")]
    pub cast_shadow: bool,
}

fn default_true() -> bool {
    true
}

/// One exported LOD of a mesh.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LodEntry {
    /// LOD index (0 = finest).
    #[serde(default)]
    pub lod: usize,
    /// `.gltf` file relative to `meshes/`.
    pub gltf: String,
    /// Sections; each non-empty one is one glTF primitive, in order.
    #[serde(default)]
    pub sections: Vec<SectionEntry>,
}

/// A glTF primitive of a LOD and the section it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimitiveRef {
    /// Primitive index inside glTF mesh 0.
    pub primitive: usize,
    /// Section (= UE3 material slot) index.
    pub section: usize,
}

impl LodEntry {
    /// The glTF primitives in order: the importer writes one primitive per
    /// section with at least one triangle and skips empty sections.
    #[must_use]
    pub fn primitives(&self) -> Vec<PrimitiveRef> {
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.triangles > 0)
            .enumerate()
            .map(|(primitive, (section, _))| PrimitiveRef { primitive, section })
            .collect()
    }

    /// The validated glTF path relative to `meshes/`.
    ///
    /// # Errors
    /// An unsafe path in the manifest.
    pub fn safe_gltf(&self) -> AssetResult<String> {
        safe_relative_path(&self.gltf)
    }
}

/// UE3 bounds of a mesh.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct BoundsEntry {
    /// Box centre (UE3 axes, UU).
    pub origin: [f32; 3],
    /// Box half size.
    pub box_extent: [f32; 3],
    /// Sphere radius.
    #[serde(default)]
    pub sphere_radius: f32,
}

/// One mesh of `meshes/manifest.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MeshEntry {
    /// Package the files were converted from.
    #[serde(default)]
    pub package: String,
    /// Exported LODs (LOD 0 first).
    #[serde(default)]
    pub lods: Vec<LodEntry>,
    /// UE3 bounds.
    #[serde(default)]
    pub bounds_ue: Option<BoundsEntry>,
    /// glTF units per Unreal unit of the files (1 = UU).
    #[serde(default = "default_scale")]
    pub scale: f32,
}

fn default_scale() -> f32 {
    1.0
}

impl MeshEntry {
    /// The finest exported LOD.
    #[must_use]
    pub fn lod0(&self) -> Option<&LodEntry> {
        self.lods.iter().min_by_key(|l| l.lod)
    }
}

/// Simple collision of one mesh (`simple_collision` of a mesh entry, written
/// since mesh manifest version 2).
///
/// The three switches are the mesh's stored values or, when not stored, the
/// native default `true` (`docs/reverse-engineering/MESHES.md`, "Simple
/// collision"). In the original a swept (non-zero-extent) trace against a
/// mesh whose box switch is on tests the simple shapes only, so a mesh
/// without a body setup is not hit at all; with the switch off it tests the
/// triangles. The line switch does the same for line traces that are not
/// forced to the triangles.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SimpleCollisionEntry {
    /// `UseSimpleBoxCollision` (swept traces, the pawn's among them).
    pub use_simple_box_collision: bool,
    /// `UseSimpleLineCollision` (line traces).
    pub use_simple_line_collision: bool,
    /// `UseSimpleRigidBodyCollision`.
    pub use_simple_rigid_body_collision: bool,
    /// The mesh has a body setup, and so simple shapes.
    pub has_body_setup: bool,
    /// The `.collision.json` file with the shapes, relative to `meshes/`
    /// (present exactly when the mesh has a body setup). The shapes are in
    /// the mesh's local space, UE3 axes and Unreal units: unlike the glTF
    /// files they are neither converted to glTF axes nor scaled.
    #[serde(default)]
    pub shapes: Option<String>,
    /// Convex elements in that file.
    #[serde(default)]
    pub convex: usize,
    /// Boxes.
    #[serde(default)]
    pub boxes: usize,
    /// Spheres.
    #[serde(default)]
    pub spheres: usize,
    /// Capsules.
    #[serde(default)]
    pub sphyls: usize,
}

impl SimpleCollisionEntry {
    /// The validated shapes path relative to `meshes/` (`None` without a
    /// body setup).
    ///
    /// # Errors
    /// An unsafe path in the manifest.
    pub fn safe_shapes(&self) -> AssetResult<Option<String>> {
        self.shapes.as_deref().map(safe_relative_path).transpose()
    }

    /// Whether a swept trace (the pawn's) can hit the mesh's simple shapes:
    /// the box switch is on and there is a body setup.
    #[must_use]
    pub fn swept_traces_use_shapes(&self) -> bool {
        self.use_simple_box_collision && self.has_body_setup
    }

    /// Whether a swept trace finds nothing to hit on this mesh: the box
    /// switch is on but there is no body setup.
    #[must_use]
    pub fn swept_traces_miss(&self) -> bool {
        self.use_simple_box_collision && !self.has_body_setup
    }
}

/// A mesh entry as stored: the fields of [`MeshEntry`] and the optional
/// records kept beside it in [`MeshManifest`].
#[derive(Debug, Deserialize)]
struct RawMeshEntry {
    #[serde(default)]
    package: String,
    #[serde(default)]
    lods: Vec<LodEntry>,
    #[serde(default)]
    bounds_ue: Option<BoundsEntry>,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    simple_collision: Option<SimpleCollisionEntry>,
}

#[derive(Debug, Deserialize)]
struct RawMeshManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    meshes: BTreeMap<String, RawMeshEntry>,
}

/// `meshes/manifest.json` with package-variant and case-insensitive lookup.
#[derive(Debug, Clone, Default)]
pub struct MeshManifest {
    /// Format version.
    pub version: u32,
    /// Entries by key (object path, or `Path@Package` for a differing copy).
    pub meshes: BTreeMap<String, MeshEntry>,
    /// Simple collision by the same keys (meshes of a manifest older than
    /// [`MESH_MANIFEST_VERSION`] have none).
    pub simple_collision: BTreeMap<String, SimpleCollisionEntry>,
    folded: HashMap<String, String>,
}

impl MeshManifest {
    /// Parses the manifest JSON.
    ///
    /// # Errors
    /// Malformed JSON.
    pub fn from_json(path: &std::path::Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawMeshManifest = crate::files::parse_json(path, data)?;
        let mut meshes = BTreeMap::new();
        let mut simple_collision = BTreeMap::new();
        for (key, e) in raw.meshes {
            if let Some(sc) = e.simple_collision {
                simple_collision.insert(key.clone(), sc);
            }
            meshes.insert(
                key,
                MeshEntry {
                    package: e.package,
                    lods: e.lods,
                    bounds_ue: e.bounds_ue,
                    scale: e.scale,
                },
            );
        }
        Ok(Self::from_entries(raw.version, meshes).with_simple_collision(simple_collision))
    }

    /// Builds a manifest from entries (tests, tools), without simple
    /// collision records.
    #[must_use]
    pub fn from_entries(version: u32, meshes: BTreeMap<String, MeshEntry>) -> Self {
        let folded = meshes.keys().map(|k| (fold(k), k.clone())).collect();
        Self {
            version,
            meshes,
            simple_collision: BTreeMap::new(),
            folded,
        }
    }

    /// Sets the simple collision records (by the keys of the entries;
    /// records of unknown keys are dropped).
    #[must_use]
    pub fn with_simple_collision(
        mut self,
        mut simple_collision: BTreeMap<String, SimpleCollisionEntry>,
    ) -> Self {
        simple_collision.retain(|k, _| self.meshes.contains_key(k));
        self.simple_collision = simple_collision;
        self
    }

    /// The simple collision of the mesh [`MeshManifest::get_for_package`]
    /// returns for `path` and `package`. `None` when the mesh is unknown or
    /// the manifest predates [`MESH_MANIFEST_VERSION`].
    #[must_use]
    pub fn simple_collision_for_package(
        &self,
        path: &str,
        package: &str,
    ) -> Option<&SimpleCollisionEntry> {
        let (key, _) = self.get_for_package(path, package)?;
        self.simple_collision.get(key)
    }

    fn lookup(&self, key: &str) -> Option<(&str, &MeshEntry)> {
        if let Some((k, v)) = self.meshes.get_key_value(key) {
            return Some((k.as_str(), v));
        }
        let k = self.folded.get(&fold(key))?;
        self.meshes.get_key_value(k).map(|(k, v)| (k.as_str(), v))
    }

    /// The mesh a map package uses for `path`: the package's own variant
    /// (`Path@Package`) when its copy differs from the first one converted,
    /// otherwise the shared entry. Case-insensitive fallback.
    #[must_use]
    pub fn get_for_package(&self, path: &str, package: &str) -> Option<(&str, &MeshEntry)> {
        self.lookup(&format!("{path}@{package}"))
            .or_else(|| self.lookup(path))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    /// True when there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const MESHES: &str = r#"{
        "version": 1, "notice": "n", "coordinates": "c", "scale": 1.0,
        "meshes": {
            "Pkg.Meshes.Box": {
                "package": "MapA", "export_index": 7, "lod_count": 1,
                "lods": [{"lod": 0, "gltf": "MapA/Pkg/Meshes/Box.gltf", "bin": "MapA/Pkg/Meshes/Box.bin",
                          "sections": [
                            {"material": "Pkg.M_A", "first_index": 0, "triangles": 2, "collision": true, "cast_shadow": true},
                            {"material": null, "first_index": 6, "triangles": 0, "collision": false, "cast_shadow": true},
                            {"material": "Pkg.M_B", "first_index": 6, "triangles": 4, "collision": true, "cast_shadow": false}
                          ], "stats": {}}],
                "bounds_ue": {"origin": [0, 0, 0], "box_extent": [1, 2, 3], "sphere_radius": 4},
                "scale": 1.0, "content_hash": "x", "future_field": [1, 2]
            },
            "Pkg.Meshes.Box@MapB": {
                "package": "MapB", "lods": [{"lod": 0, "gltf": "MapB/Pkg/Meshes/Box@MapB.gltf", "sections": []}]
            }
        }
    }"#;

    #[test]
    fn mesh_manifest_parses_and_maps_primitives() {
        let m = MeshManifest::from_json(Path::new("m.json"), MESHES.as_bytes()).unwrap();
        assert_eq!(m.len(), 2);
        let (key, e) = m.get_for_package("Pkg.Meshes.Box", "MapA").unwrap();
        assert_eq!(key, "Pkg.Meshes.Box");
        let lod = e.lod0().unwrap();
        assert_eq!(
            lod.primitives(),
            vec![
                PrimitiveRef {
                    primitive: 0,
                    section: 0
                },
                PrimitiveRef {
                    primitive: 1,
                    section: 2
                }
            ]
        );
        assert_eq!(lod.safe_gltf().unwrap(), "MapA/Pkg/Meshes/Box.gltf");
        assert!(!lod.sections[2].cast_shadow);
        assert_eq!(e.bounds_ue.unwrap().box_extent, [1.0, 2.0, 3.0]);
        // A package with a differing copy gets its variant.
        let (key, _) = m.get_for_package("Pkg.Meshes.Box", "MapB").unwrap();
        assert_eq!(key, "Pkg.Meshes.Box@MapB");
        // Case-insensitive fallback.
        let (key, _) = m.get_for_package("pkg.meshes.BOX", "MapC").unwrap();
        assert_eq!(key, "Pkg.Meshes.Box");
        assert!(m.get_for_package("Pkg.Meshes.Nope", "MapA").is_none());
    }

    #[test]
    fn simple_collision_is_read_per_mesh_and_absent_in_old_manifests() {
        // Version 1 (the fixture above): entries load, no record.
        let old = MeshManifest::from_json(Path::new("m.json"), MESHES.as_bytes()).unwrap();
        assert_eq!(old.version, 1);
        assert!(old.simple_collision.is_empty());
        assert!(
            old.simple_collision_for_package("Pkg.Meshes.Box", "MapA")
                .is_none()
        );

        let json = r#"{
            "version": 2,
            "meshes": {
                "Pkg.Meshes.Door": {
                    "package": "MapA", "lods": [{"lod": 0, "gltf": "MapA/Pkg/Meshes/Door.gltf"}],
                    "body_setup": "Pkg.Meshes.Door.RB_BodySetup_0",
                    "simple_collision": {
                        "use_simple_box_collision": true, "use_simple_line_collision": false,
                        "use_simple_rigid_body_collision": true, "has_body_setup": true,
                        "shapes": "MapA/Pkg/Meshes/Door.collision.json",
                        "convex": 3, "boxes": 0, "spheres": 0, "sphyls": 0, "future": 1
                    }
                },
                "Pkg.Meshes.Rug": {
                    "package": "MapA", "lods": [{"lod": 0, "gltf": "MapA/Pkg/Meshes/Rug.gltf"}],
                    "simple_collision": {
                        "use_simple_box_collision": true, "use_simple_line_collision": true,
                        "use_simple_rigid_body_collision": true, "has_body_setup": false,
                        "shapes": null
                    }
                },
                "Pkg.Meshes.Rug@MapB": {
                    "package": "MapB", "lods": [{"lod": 0, "gltf": "MapB/Pkg/Meshes/Rug@MapB.gltf"}],
                    "simple_collision": {
                        "use_simple_box_collision": false, "use_simple_line_collision": false,
                        "use_simple_rigid_body_collision": false, "has_body_setup": false
                    }
                },
                "Pkg.Meshes.Bad": {
                    "package": "MapA", "lods": [],
                    "simple_collision": {
                        "use_simple_box_collision": true, "use_simple_line_collision": true,
                        "use_simple_rigid_body_collision": true, "has_body_setup": true,
                        "shapes": "../../outside.json"
                    }
                }
            }
        }"#;
        let m = MeshManifest::from_json(Path::new("m.json"), json.as_bytes()).unwrap();
        assert_eq!(m.version, MESH_MANIFEST_VERSION);
        assert_eq!((m.len(), m.simple_collision.len()), (4, 4));
        // The entries themselves are read as before.
        let (_, e) = m.get_for_package("Pkg.Meshes.Door", "MapA").unwrap();
        assert_eq!(e.lod0().unwrap().gltf, "MapA/Pkg/Meshes/Door.gltf");

        let door = m
            .simple_collision_for_package("pkg.meshes.DOOR", "MapZ")
            .unwrap();
        assert!(door.use_simple_box_collision && !door.use_simple_line_collision);
        assert!(door.has_body_setup && door.swept_traces_use_shapes() && !door.swept_traces_miss());
        assert_eq!(
            (door.convex, door.boxes, door.spheres, door.sphyls),
            (3, 0, 0, 0)
        );
        assert_eq!(
            door.safe_shapes().unwrap().as_deref(),
            Some("MapA/Pkg/Meshes/Door.collision.json")
        );
        // No body setup and the box switch on: swept traces find nothing.
        let rug = m
            .simple_collision_for_package("Pkg.Meshes.Rug", "MapA")
            .unwrap();
        assert!(rug.swept_traces_miss() && !rug.swept_traces_use_shapes());
        assert_eq!(rug.safe_shapes().unwrap(), None);
        // A package's differing copy has its own record: the switch is off,
        // so swept traces test the triangles.
        let rug_b = m
            .simple_collision_for_package("Pkg.Meshes.Rug", "MapB")
            .unwrap();
        assert!(!rug_b.use_simple_box_collision);
        assert!(!rug_b.swept_traces_miss() && !rug_b.swept_traces_use_shapes());
        // An unsafe shapes path parses but is refused on use.
        let bad = m
            .simple_collision_for_package("Pkg.Meshes.Bad", "MapA")
            .unwrap();
        assert!(bad.safe_shapes().is_err());
        assert!(
            m.simple_collision_for_package("Pkg.Meshes.Nope", "MapA")
                .is_none()
        );

        // A record without its switches is an error, not a guess.
        for broken in [
            r#"{"meshes": {"a": {"simple_collision": {"has_body_setup": true}}}}"#,
            r#"{"meshes": {"a": {"simple_collision": 5}}}"#,
            r#"{"meshes": {"a": {"simple_collision": {"use_simple_box_collision": 1,
                "use_simple_line_collision": true, "use_simple_rigid_body_collision": true,
                "has_body_setup": true}}}}"#,
        ] {
            assert!(MeshManifest::from_json(Path::new("m"), broken.as_bytes()).is_err());
        }
        // `null` is "no record".
        let none = MeshManifest::from_json(
            Path::new("m"),
            br#"{"meshes": {"a": {"simple_collision": null}}}"#,
        )
        .unwrap();
        assert!(none.simple_collision.is_empty() && none.len() == 1);

        // Built from entries: records of unknown keys are dropped.
        let mut records = BTreeMap::new();
        records.insert("Pkg.Meshes.Door".to_owned(), door.clone());
        records.insert("Pkg.Meshes.Gone".to_owned(), door.clone());
        let built = MeshManifest::from_entries(2, m.meshes.clone()).with_simple_collision(records);
        assert_eq!(built.simple_collision.len(), 1);
        assert!(
            built
                .simple_collision_for_package("Pkg.Meshes.Door", "MapA")
                .is_some()
        );
    }

    /// Real converted data (skipped unless `ASAMU_CONVERTED_DIR` holds an
    /// `asamu-import` output with `meshes/manifest.json`): a version 1
    /// manifest still loads and has no simple-collision record; a version 2
    /// manifest has one per mesh, and a complete one (all 882 meshes) has the
    /// counts of the shipped game (`docs/reverse-engineering/MESHES.md`,
    /// "Simple collision").
    #[test]
    fn real_mesh_manifest_loads_with_or_without_simple_collision() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let path = std::path::PathBuf::from(dir).join("meshes/manifest.json");
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("skipping: no {}", path.display());
            return;
        };
        let m = MeshManifest::from_json(&path, &data).unwrap();
        eprintln!(
            "mesh manifest version {}: {} meshes, {} simple-collision records",
            m.version,
            m.len(),
            m.simple_collision.len()
        );
        if m.version < MESH_MANIFEST_VERSION {
            assert!(m.simple_collision.is_empty());
            return;
        }
        assert_eq!(m.simple_collision.len(), m.len());
        for e in m.simple_collision.values() {
            assert_eq!(e.safe_shapes().unwrap().is_some(), e.has_body_setup);
            assert_eq!(
                e.has_body_setup,
                e.convex + e.boxes + e.spheres + e.sphyls > 0
            );
        }
        if m.len() != 882 {
            eprintln!("partial conversion: the game's counts are not checked");
            return;
        }
        let count = |f: &dyn Fn(&SimpleCollisionEntry) -> bool| {
            m.simple_collision.values().filter(|e| f(e)).count()
        };
        assert_eq!(count(&|e| e.has_body_setup), 316);
        assert_eq!(count(&|e| e.swept_traces_use_shapes()), 297);
        assert_eq!(count(&|e| e.swept_traces_miss()), 515);
        assert_eq!(count(&|e| !e.use_simple_box_collision), 70);
        assert_eq!(count(&|e| !e.use_simple_line_collision), 65);
    }

    #[test]
    fn texture_manifest_parses_with_defaults() {
        let json = r#"{"version": 1, "notice": "n", "textures": {
            "Pkg.T_D": {"package": "MapA", "class": "Texture2D", "file": "MapA/Pkg/T_D.dds",
                        "format": "PF_DXT1", "size": [64, 64], "written_size": [64, 64], "mips": 7,
                        "srgb": true, "address": ["TA_Wrap", "TA_Clamp"], "lod_group": "TEXTUREGROUP_World"},
            "Pkg.T_N": {"file": "MapA/Pkg/T_N.dds", "srgb": true, "lod_group": "TEXTUREGROUP_WorldNormalMap"},
            "Pkg.T_L": {"file": "MapA/Pkg/T_L.dds"}
        }}"#;
        let t = TextureManifest::from_json(Path::new("t.json"), json.as_bytes()).unwrap();
        let (_, d) = t.get("pkg.t_d").unwrap();
        assert_eq!(d.address_modes(), [AddressMode::Wrap, AddressMode::Clamp]);
        assert!(d.is_srgb());
        let (_, n) = t.get("Pkg.T_N").unwrap();
        assert!(n.is_normal_map() && !n.is_srgb());
        let (_, l) = t.get("Pkg.T_L").unwrap();
        assert!(l.is_srgb());
        assert_eq!(l.address_modes(), [AddressMode::Wrap; 2]);
        assert_eq!(AddressMode::from_ue("TA_Mirror"), AddressMode::Mirror);
    }

    #[test]
    fn hostile_manifests_are_errors_not_panics() {
        for bad in [
            "",
            "null",
            "{\"meshes\": 5}",
            "{\"meshes\": {\"a\": {\"lods\": [{}]}}}",
            "{\"meshes\": {\"a\": {\"lods\": [{\"gltf\": 1}]}}}",
            &"[".repeat(100_000),
        ] {
            assert!(
                MeshManifest::from_json(Path::new("m"), bad.as_bytes()).is_err(),
                "{bad:.40}"
            );
        }
        // Unsafe file paths parse but are refused on use.
        let m = MeshManifest::from_json(
            Path::new("m"),
            br#"{"meshes": {"a": {"lods": [{"gltf": "../../etc/passwd"}]}}}"#,
        )
        .unwrap();
        assert!(m.meshes["a"].lods[0].safe_gltf().is_err());
        let t = TextureManifest::from_json(
            Path::new("t"),
            br#"{"textures": {"a": {"file": "/abs.dds"}}}"#,
        )
        .unwrap();
        assert!(t.textures["a"].safe_file().is_err());
    }
}
