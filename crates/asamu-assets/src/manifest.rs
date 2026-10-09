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

/// Manifest format version both importers write today.
pub const MANIFEST_VERSION: u32 = 1;

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

#[derive(Debug, Deserialize)]
struct RawMeshManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    meshes: BTreeMap<String, MeshEntry>,
}

/// `meshes/manifest.json` with package-variant and case-insensitive lookup.
#[derive(Debug, Clone, Default)]
pub struct MeshManifest {
    /// Format version.
    pub version: u32,
    /// Entries by key (object path, or `Path@Package` for a differing copy).
    pub meshes: BTreeMap<String, MeshEntry>,
    folded: HashMap<String, String>,
}

impl MeshManifest {
    /// Parses the manifest JSON.
    ///
    /// # Errors
    /// Malformed JSON.
    pub fn from_json(path: &std::path::Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawMeshManifest = crate::files::parse_json(path, data)?;
        Ok(Self::from_entries(raw.version, raw.meshes))
    }

    /// Builds a manifest from entries (tests, tools).
    #[must_use]
    pub fn from_entries(version: u32, meshes: BTreeMap<String, MeshEntry>) -> Self {
        let folded = meshes.keys().map(|k| (fold(k), k.clone())).collect();
        Self {
            version,
            meshes,
            folded,
        }
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
