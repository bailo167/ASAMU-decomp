//! Baked lighting of converted levels (`lightmaps/<map>.lightmaps.json`,
//! written by `asamu-import lightmaps`).
//!
//! The importer combines the original's two shipped light map coefficients
//! into linear RGB irradiance atlases (an APPROXIMATION of UE3's directional
//! light maps; see `docs/reverse-engineering/LIGHTMAPS.md`). This module reads
//! that output and answers, render-agnostically:
//!
//! - which atlas image and UV rectangle a static mesh component samples, and
//!   through which mesh UV channel ([`ComponentLightmap`]);
//! - which lights are already baked into the light maps
//!   ([`LevelLightmaps::light_baked`]): a renderer must not light
//!   light-mapped surfaces with them again;
//! - the BSP surfaces with their light map coordinates, as render meshes
//!   ([`LevelLightmaps::bsp_meshes`]); surfaces the original drew without a
//!   light map come with [`BspLightmapMesh::lightmap`] `None`, so the meshes
//!   of a level cover its whole visible BSP.
//!
//! Light baking is keyed by actor ([`LevelLightmaps::light_baked`]): an
//! actor with several light components counts as baked when any of them is
//! (every shipped light actor has one light component).
//!
//! # Units
//!
//! Atlas texels are irradiance in UE3 light units: 1.0 is what a light of
//! `Brightness` 1 delivers at normal incidence, and UE3 multiplies it with the
//! diffuse colour (no 1/π). The dynamic-light mapping
//! ([`crate::lighting::LightMapping`]) turns brightness 1 into
//! `directional_lux_per_brightness` lux, which a Lambertian surface turns
//! into `albedo × lux / π`; [`lightmap_exposure`] is the factor that puts
//! baked and dynamic light on the same scale (our convention, not recovered).
//!
//! Like every converted file, the documents are untrusted input: sizes are
//! bounded, stored paths are validated, binary spans are range-checked, and
//! nothing here panics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_core::WorldScale;
use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};
use asamu_core::glam::Vec3;
use serde::Deserialize;

use crate::error::{AssetError, AssetResult};
use crate::files::{
    MAX_MANIFEST_BYTES, MAX_SCENE_BYTES, parse_json, read_bounded, safe_relative_path,
};
use crate::level::ConvertedDir;
use crate::lighting::LightMapping;

/// `format` of `<map>.lightmaps.json`.
pub const LIGHTMAPS_FORMAT: &str = "asamu-lightmaps";
/// Supported `version`.
pub const LIGHTMAPS_VERSION: u32 = 1;

/// The `StandardMaterial::lightmap_exposure`-style factor that converts
/// atlas irradiance (UE3 light units) into the render units the dynamic
/// lights use: `directional_lux_per_brightness / π` (see the module docs).
#[must_use]
pub fn lightmap_exposure(mapping: &LightMapping) -> f32 {
    mapping.directional_lux_per_brightness / std::f32::consts::PI
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RawSpan {
    offset: usize,
    count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct RawAtlas {
    file: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct RawComponent {
    actor: String,
    #[serde(default)]
    actor_slot: usize,
    component: String,
    #[serde(default)]
    mesh: Option<String>,
    kind: String,
    #[serde(default)]
    atlas: Option<usize>,
    uv_rect: [f32; 4],
    #[serde(default)]
    uv_channel: Option<i32>,
    #[serde(default)]
    baked_lights: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct RawBspElement {
    #[serde(default)]
    material: Option<String>,
    /// Absent for an element without a texture light map.
    #[serde(default)]
    atlas: Option<usize>,
    #[serde(default)]
    uv_rect: Option<[f32; 4]>,
    positions: RawSpan,
    normals: RawSpan,
    uv0: RawSpan,
    uv1: RawSpan,
    triangles: RawSpan,
}

#[derive(Debug, Clone, Deserialize)]
struct RawBsp {
    bin: String,
    #[serde(default)]
    elements: Vec<RawBspElement>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawLight {
    actor: String,
    #[serde(default)]
    actor_slot: usize,
    #[serde(default)]
    component: String,
    #[serde(default)]
    baked: bool,
    #[serde(default)]
    shadow_mapped: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMapFile {
    format: String,
    version: u32,
    map: String,
    #[serde(default)]
    atlases: Vec<RawAtlas>,
    #[serde(default)]
    vertex_atlas: Option<RawAtlas>,
    #[serde(default)]
    components: Vec<RawComponent>,
    #[serde(default)]
    bsp: Option<RawBsp>,
    #[serde(default)]
    lights: Vec<RawLight>,
}

/// How a component's light map is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightmapKind {
    /// A region of a texture atlas, sampled through a mesh UV channel.
    Texture,
    /// A vertex light map reduced to one constant: the rectangle is a point,
    /// so any UV samples the same texel.
    Constant,
}

/// The light map of one static mesh component.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentLightmap {
    /// Atlas image, relative to the converted root (`lightmaps/...`).
    pub image: String,
    /// `[min_u, min_v, max_u, max_v]` in the atlas.
    pub uv_rect: [f32; 4],
    /// Storage kind.
    pub kind: LightmapKind,
    /// Mesh UV channel holding the light map coordinates (texture kind; 0
    /// when the mesh did not tag one).
    pub uv_channel: u32,
    /// Static mesh path recorded by the importer.
    pub mesh: Option<String>,
    /// Lights baked into it.
    pub baked_lights: usize,
}

/// One light component's baking state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightBaking {
    /// Baked into at least one light map.
    pub baked: bool,
    /// Named by a static shadow map (rendered dynamically in UE3, with
    /// precomputed shadows).
    pub shadow_mapped: bool,
}

/// The light map of a BSP render mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct BspLightmap {
    /// Atlas image, relative to the converted root.
    pub image: String,
    /// Atlas rectangle.
    pub uv_rect: [f32; 4],
}

/// A BSP render mesh with light map coordinates, already in render space.
#[derive(Debug, Clone, PartialEq)]
pub struct BspLightmapMesh {
    /// Material path.
    pub material: Option<String>,
    /// The light map; `None` for surfaces the original drew without one
    /// (they must still be drawn: together the meshes of a level cover its
    /// whole visible BSP).
    pub lightmap: Option<BspLightmap>,
    /// Positions (render units).
    pub positions: Vec<[f32; 3]>,
    /// Unit normals (render axes).
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uv0: Vec<[f32; 2]>,
    /// Light map coordinates (`[0, 1]` inside the rectangle).
    pub uv1: Vec<[f32; 2]>,
    /// Triangle list, counter-clockwise for the render handedness.
    pub indices: Vec<u32>,
}

/// Key of a component: (level package, actor name, component name), names
/// compared without regard to ASCII case.
type ComponentKey = (String, String, String);

/// Light map data of a level and its merged sub-levels.
#[derive(Debug, Clone, Default)]
pub struct LevelLightmaps {
    components: BTreeMap<ComponentKey, ComponentLightmap>,
    lights: BTreeMap<(String, String), LightBaking>,
    bsp: BTreeMap<String, (PathBuf, Vec<RawBspElement>, Vec<String>)>,
    /// Packages whose light map file was found.
    pub packages: Vec<String>,
    /// Packages without a light map file.
    pub missing: Vec<String>,
    /// Entries dropped as invalid (bad path, atlas index or rectangle).
    pub rejected: usize,
}

fn key(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn rect_ok(r: &[f32; 4]) -> bool {
    r.iter()
        .all(|v| v.is_finite() && (-1e-3..=1.0 + 1e-3).contains(v))
        && r[0] <= r[2]
        && r[1] <= r[3]
}

impl LevelLightmaps {
    /// Path of `<package>.lightmaps.json` under `dir`.
    #[must_use]
    pub fn file_for(dir: &ConvertedDir, package: &str) -> PathBuf {
        dir.root()
            .join("lightmaps")
            .join(format!("{package}.lightmaps.json"))
    }

    /// Loads the light map files of `packages` (the persistent level first,
    /// then merged sub-levels). Missing files are recorded in
    /// [`Self::missing`], not errors.
    ///
    /// # Errors
    /// A file that exists but cannot be read or parsed, or declares another
    /// format or version.
    pub fn load(dir: &ConvertedDir, packages: &[String]) -> AssetResult<Self> {
        let mut out = Self::default();
        for p in packages {
            if p.is_empty() || p.contains(['/', '\\', ':', '\0']) || p == "." || p == ".." {
                out.missing.push(p.clone());
                continue;
            }
            let path = Self::file_for(dir, p);
            if !path.is_file() {
                out.missing.push(p.clone());
                continue;
            }
            let data = read_bounded(&path, MAX_MANIFEST_BYTES)?;
            out.add_json(dir.root(), &path, &data, p)?;
        }
        Ok(out)
    }

    /// Adds one parsed document (`package` is the level it belongs to).
    ///
    /// # Errors
    /// Malformed JSON, or a wrong format or version.
    pub fn add_json(
        &mut self,
        root: &Path,
        path: &Path,
        data: &[u8],
        package: &str,
    ) -> AssetResult<()> {
        let raw: RawMapFile = parse_json(path, data)?;
        if raw.format != LIGHTMAPS_FORMAT {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("format {LIGHTMAPS_FORMAT:?}"),
                found: format!("format {:?}", raw.format),
            });
        }
        if raw.version != LIGHTMAPS_VERSION {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("version {LIGHTMAPS_VERSION}"),
                found: format!("version {}", raw.version),
            });
        }
        let _ = raw.map;
        let image = |rel: &str| {
            safe_relative_path(rel)
                .ok()
                .map(|r| format!("lightmaps/{r}"))
        };
        let atlases: Vec<Option<String>> = raw
            .atlases
            .iter()
            .map(|a| image(&a.file).filter(|_| a.width > 0 && a.height > 0))
            .collect();
        let vertex = raw.vertex_atlas.as_ref().and_then(|a| image(&a.file));
        let pk = key(package);
        for c in raw.components {
            let (img, kind) = match c.kind.as_str() {
                "texture" => (
                    c.atlas.and_then(|i| atlases.get(i).cloned().flatten()),
                    LightmapKind::Texture,
                ),
                "vertex" => (vertex.clone(), LightmapKind::Constant),
                _ => (None, LightmapKind::Texture),
            };
            let channel = match kind {
                LightmapKind::Texture => c.uv_channel.and_then(|v| u32::try_from(v).ok()),
                LightmapKind::Constant => Some(0),
            };
            let (Some(img), Some(uv_channel)) = (img, channel) else {
                self.rejected += 1;
                continue;
            };
            if !rect_ok(&c.uv_rect) {
                self.rejected += 1;
                continue;
            }
            let _ = c.actor_slot;
            self.components.insert(
                (pk.clone(), key(&c.actor), key(&c.component)),
                ComponentLightmap {
                    image: img,
                    uv_rect: c.uv_rect,
                    kind,
                    uv_channel,
                    mesh: c.mesh,
                    baked_lights: c.baked_lights,
                },
            );
        }
        for l in raw.lights {
            let _ = (l.actor_slot, &l.component);
            let e = self
                .lights
                .entry((pk.clone(), key(&l.actor)))
                .or_insert(LightBaking {
                    baked: false,
                    shadow_mapped: false,
                });
            e.baked |= l.baked;
            e.shadow_mapped |= l.shadow_mapped;
        }
        if let Some(b) = raw.bsp {
            match safe_relative_path(&b.bin) {
                Ok(rel) => {
                    let names: Vec<String> = atlases
                        .iter()
                        .map(|a| a.clone().unwrap_or_default())
                        .collect();
                    self.bsp.insert(
                        pk.clone(),
                        (root.join("lightmaps").join(rel), b.elements, names),
                    );
                }
                Err(_) => self.rejected += 1,
            }
        }
        self.packages.push(package.to_owned());
        Ok(())
    }

    /// Number of components with a light map.
    #[must_use]
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    /// The light map of component `component` of actor `actor` in level
    /// `package`.
    #[must_use]
    pub fn component(
        &self,
        package: &str,
        actor: &str,
        component: &str,
    ) -> Option<&ComponentLightmap> {
        self.components
            .get(&(key(package), key(actor), key(component)))
    }

    /// Baking state of the light(s) of actor `actor` in level `package`
    /// (`None` when the level has no light map file or no such light).
    #[must_use]
    pub fn light_baked(&self, package: &str, actor: &str) -> Option<LightBaking> {
        self.lights.get(&(key(package), key(actor))).copied()
    }

    /// Every distinct atlas image referenced.
    #[must_use]
    pub fn images(&self) -> Vec<String> {
        let mut v: Vec<String> = self.components.values().map(|c| c.image.clone()).collect();
        for (_, elements, names) in self.bsp.values() {
            for e in elements {
                if let Some(n) = e.atlas.and_then(|a| names.get(a)).filter(|n| !n.is_empty()) {
                    v.push(n.clone());
                }
            }
        }
        v.sort();
        v.dedup();
        v
    }

    /// True when the light map file of level `package` carries BSP
    /// geometry.
    #[must_use]
    pub fn has_bsp(&self, package: &str) -> bool {
        self.bsp.contains_key(&key(package))
    }

    /// BSP render meshes of level `package`, moved by `offset` (UE3 units; a
    /// streaming sub-level's `Offset`, zero for the persistent level) and
    /// converted to render space at `scale`. Elements whose spans do not fit
    /// the binary file, or whose light map reference is invalid, are skipped
    /// and counted in the second value.
    ///
    /// # Errors
    /// The binary file cannot be read.
    pub fn bsp_meshes(
        &self,
        package: &str,
        offset: Vec3,
        scale: WorldScale,
    ) -> AssetResult<(Vec<BspLightmapMesh>, usize)> {
        let Some((path, elements, names)) = self.bsp.get(&key(package)) else {
            return Ok((Vec::new(), 0));
        };
        let bin = read_bounded(path, MAX_SCENE_BYTES)?;
        Ok(bsp_meshes_from(&bin, elements, names, offset, scale))
    }
}

fn floats<const N: usize>(bin: &[u8], span: RawSpan) -> Option<Vec<[f32; N]>> {
    let bytes = span.count.checked_mul(N)?.checked_mul(4)?;
    let data = bin.get(span.offset..span.offset.checked_add(bytes)?)?;
    let (words, _) = data.as_chunks::<4>();
    let values: Vec<f32> = words.iter().map(|w| f32::from_le_bytes(*w)).collect();
    let (groups, _) = values.as_chunks::<N>();
    Some(groups.to_vec())
}

fn triangles(bin: &[u8], span: RawSpan) -> Option<Vec<[u32; 3]>> {
    let bytes = span.count.checked_mul(12)?;
    let data = bin.get(span.offset..span.offset.checked_add(bytes)?)?;
    let (words, _) = data.as_chunks::<4>();
    let values: Vec<u32> = words.iter().map(|w| u32::from_le_bytes(*w)).collect();
    let (groups, _) = values.as_chunks::<3>();
    Some(groups.to_vec())
}

/// The light map of a BSP element: `Some(None)` for an element without one,
/// `None` for an invalid reference (unknown or rejected atlas, missing or bad
/// rectangle).
fn bsp_lightmap(e: &RawBspElement, names: &[String]) -> Option<Option<BspLightmap>> {
    let Some(atlas) = e.atlas else {
        return Some(None);
    };
    let image = names.get(atlas).filter(|n| !n.is_empty())?;
    let uv_rect = e.uv_rect.filter(rect_ok)?;
    Some(Some(BspLightmap {
        image: image.clone(),
        uv_rect,
    }))
}

fn bsp_meshes_from(
    bin: &[u8],
    elements: &[RawBspElement],
    names: &[String],
    offset: Vec3,
    scale: WorldScale,
) -> (Vec<BspLightmapMesh>, usize) {
    let offset = if offset.is_finite() {
        offset
    } else {
        Vec3::ZERO
    };
    let mut out = Vec::new();
    let mut dropped = 0;
    for e in elements {
        let parts = (
            floats::<3>(bin, e.positions),
            floats::<3>(bin, e.normals),
            floats::<2>(bin, e.uv0),
            floats::<2>(bin, e.uv1),
            triangles(bin, e.triangles),
            bsp_lightmap(e, names),
        );
        let (Some(p), Some(n), Some(a), Some(b), Some(t), Some(lightmap)) = parts else {
            dropped += 1;
            continue;
        };
        let count = p.len();
        if n.len() != count || a.len() != count || b.len() != count {
            dropped += 1;
            continue;
        }
        let Ok(limit) = u32::try_from(count) else {
            dropped += 1;
            continue;
        };
        if t.iter().flatten().any(|i| *i >= limit) {
            dropped += 1;
            continue;
        }
        let finite2 = |v: [f32; 2]| {
            if v.iter().all(|c| c.is_finite()) {
                v
            } else {
                [0.0; 2]
            }
        };
        out.push(BspLightmapMesh {
            material: e.material.clone(),
            lightmap,
            positions: p
                .iter()
                .map(|v| ue_pos_to_bevy(Vec3::from_array(*v) + offset, scale).to_array())
                .collect(),
            normals: n
                .iter()
                .map(|v| {
                    ue_dir_to_bevy(Vec3::from_array(*v).try_normalize().unwrap_or(Vec3::Z))
                        .to_array()
                })
                .collect(),
            uv0: a.into_iter().map(finite2).collect(),
            uv1: b.into_iter().map(finite2).collect(),
            // (a, c, b): the UE3 → render basis change mirrors (as in
            // `crate::bsp`).
            indices: t.iter().flat_map(|[x, y, z]| [*x, *z, *y]).collect(),
        });
    }
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> String {
        r#"{
          "format": "asamu-lightmaps", "version": 1, "map": "TestMap",
          "atlases": [ {"file": "TestMap/atlas_0.dds", "width": 4, "height": 4},
                       {"file": "../escape.dds", "width": 4, "height": 4} ],
          "vertex_atlas": {"file": "TestMap/vertex.dds", "width": 4, "height": 4},
          "components": [
            {"actor": "StaticMeshActor_1", "actor_slot": 3, "component": "StaticMeshComponent_0",
             "mesh": "Pkg.Rock", "kind": "texture", "atlas": 0, "uv_rect": [0.25, 0.5, 0.5, 0.75],
             "uv_channel": 1, "baked_lights": 2},
            {"actor": "StaticMeshActor_2", "component": "StaticMeshComponent_0",
             "kind": "vertex", "atlas": 0, "uv_rect": [0.5, 0.5, 0.5, 0.5]},
            {"actor": "Bad", "component": "C", "kind": "texture", "atlas": 1, "uv_rect": [0,0,1,1], "uv_channel": 1},
            {"actor": "Bad2", "component": "C", "kind": "texture", "atlas": 0, "uv_rect": [0,0,7,1], "uv_channel": 1},
            {"actor": "Bad3", "component": "C", "kind": "texture", "atlas": 9, "uv_rect": [0,0,1,1], "uv_channel": 1}
          ],
          "bsp": {"bin": "TestMap.bsp.bin", "elements": [
            {"component": "ModelComponent_0", "element": 0, "material": "Pkg.Floor", "atlas": 0,
             "uv_rect": [0, 0, 0.5, 0.5],
             "positions": {"offset": 0, "count": 3}, "normals": {"offset": 36, "count": 3},
             "uv0": {"offset": 72, "count": 3}, "uv1": {"offset": 96, "count": 3},
             "triangles": {"offset": 120, "count": 1}},
            {"component": "ModelComponent_0", "element": 1, "material": null, "atlas": 0,
             "uv_rect": [0, 0, 0.5, 0.5],
             "positions": {"offset": 0, "count": 3}, "normals": {"offset": 36, "count": 3},
             "uv0": {"offset": 72, "count": 3}, "uv1": {"offset": 96, "count": 3},
             "triangles": {"offset": 130, "count": 1}},
            {"component": "ModelComponent_0", "element": 2, "material": "Pkg.Wall",
             "positions": {"offset": 0, "count": 3}, "normals": {"offset": 36, "count": 3},
             "uv0": {"offset": 72, "count": 3}, "uv1": {"offset": 96, "count": 3},
             "triangles": {"offset": 120, "count": 1}},
            {"component": "ModelComponent_0", "element": 3, "material": "Pkg.Bad", "atlas": 0,
             "uv_rect": [0, 0, 5, 0.5],
             "positions": {"offset": 0, "count": 3}, "normals": {"offset": 36, "count": 3},
             "uv0": {"offset": 72, "count": 3}, "uv1": {"offset": 96, "count": 3},
             "triangles": {"offset": 120, "count": 1}},
            {"component": "ModelComponent_0", "element": 4, "material": "Pkg.Bad", "atlas": 1,
             "uv_rect": [0, 0, 0.5, 0.5],
             "positions": {"offset": 0, "count": 3}, "normals": {"offset": 36, "count": 3},
             "uv0": {"offset": 72, "count": 3}, "uv1": {"offset": 96, "count": 3},
             "triangles": {"offset": 120, "count": 1}}
          ]},
          "lights": [
            {"actor": "PointLight_0", "component": "PointLightComponent_0", "baked": true, "shadow_mapped": false},
            {"actor": "DominantDirectionalLight_0", "component": "C", "baked": false, "shadow_mapped": true}
          ]
        }"#
        .to_owned()
    }

    fn bin() -> Vec<u8> {
        let mut b = Vec::new();
        let mut f = |v: &[f32]| {
            for x in v {
                b.extend_from_slice(&x.to_le_bytes());
            }
        };
        f(&[0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0, 0.0]); // positions
        f(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]); // normals
        f(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]); // uv0
        f(&[0.0, 0.0, 1.0, 0.0, 0.0, f32::NAN]); // uv1
        for i in [0u32, 1, 2] {
            b.extend_from_slice(&i.to_le_bytes());
        }
        b
    }

    fn loaded() -> LevelLightmaps {
        let mut l = LevelLightmaps::default();
        l.add_json(
            Path::new("/conv"),
            Path::new("TestMap.lightmaps.json"),
            doc().as_bytes(),
            "TestMap",
        )
        .unwrap();
        l
    }

    #[test]
    fn components_lights_and_rejections() {
        let l = loaded();
        let c = l
            .component("testmap", "staticmeshactor_1", "STATICMESHCOMPONENT_0")
            .unwrap();
        assert_eq!(c.image, "lightmaps/TestMap/atlas_0.dds");
        assert_eq!(c.uv_rect, [0.25, 0.5, 0.5, 0.75]);
        assert_eq!(c.kind, LightmapKind::Texture);
        assert_eq!(c.uv_channel, 1);
        assert_eq!(c.baked_lights, 2);
        let v = l
            .component("TestMap", "StaticMeshActor_2", "StaticMeshComponent_0")
            .unwrap();
        assert_eq!(v.kind, LightmapKind::Constant);
        assert_eq!(v.image, "lightmaps/TestMap/vertex.dds");
        assert_eq!(l.rejected, 3);
        assert_eq!(l.component_count(), 2);
        assert_eq!(
            l.light_baked("TestMap", "PointLight_0"),
            Some(LightBaking {
                baked: true,
                shadow_mapped: false
            })
        );
        assert!(
            !l.light_baked("TestMap", "DominantDirectionalLight_0")
                .unwrap()
                .baked
        );
        assert_eq!(l.light_baked("Other", "PointLight_0"), None);
        assert_eq!(
            l.images(),
            vec![
                "lightmaps/TestMap/atlas_0.dds".to_owned(),
                "lightmaps/TestMap/vertex.dds".to_owned()
            ]
        );
    }

    #[test]
    fn wrong_format_or_version_is_an_error() {
        let mut l = LevelLightmaps::default();
        let bad = doc().replace("\"version\": 1", "\"version\": 2");
        assert!(
            l.add_json(Path::new("/"), Path::new("x"), bad.as_bytes(), "M")
                .is_err()
        );
        let bad = doc().replace("asamu-lightmaps", "asamu-scene");
        assert!(
            l.add_json(Path::new("/"), Path::new("x"), bad.as_bytes(), "M")
                .is_err()
        );
        assert!(
            l.add_json(Path::new("/"), Path::new("x"), b"{", "M")
                .is_err()
        );
    }

    #[test]
    fn bsp_meshes_are_converted_and_checked() {
        let l = loaded();
        let (_, elements, names) = l.bsp.get("testmap").unwrap();
        let (meshes, dropped) = bsp_meshes_from(
            &bin(),
            elements,
            names,
            Vec3::ZERO,
            WorldScale::PRESENTATION_METRES,
        );
        // Dropped: the second element's triangle span runs past the end of
        // the file, the fourth has a rectangle outside the atlas and the
        // fifth names a rejected atlas (a path escaping the directory).
        assert_eq!(dropped, 3);
        assert_eq!(meshes.len(), 2);
        let m = &meshes[0];
        assert_eq!(m.material.as_deref(), Some("Pkg.Floor"));
        assert_eq!(
            m.lightmap,
            Some(BspLightmap {
                image: "lightmaps/TestMap/atlas_0.dds".to_owned(),
                uv_rect: [0.0, 0.0, 0.5, 0.5]
            })
        );
        // An element without a light map is still drawn, without one.
        assert_eq!(meshes[1].material.as_deref(), Some("Pkg.Wall"));
        assert_eq!(meshes[1].lightmap, None);
        assert_eq!(meshes[1].indices, m.indices);
        // (a, c, b) winding.
        assert_eq!(m.indices, vec![0, 2, 1]);
        // UE3 +Z normal is render +Y.
        assert!((m.normals[0][1] - 1.0).abs() < 1e-6);
        // UE3 X = 100 UU is two presentation metres along render -Z.
        assert!((m.positions[1][2] + 2.0).abs() < 1e-5);
        // A non-finite light map UV is zeroed.
        assert_eq!(m.uv1[2], [0.0, 0.0]);
        // A sub-level offset (UE3 units) moves the geometry before the
        // conversion: UE3 +X 100 UU is two presentation metres along -Z.
        let (moved, _) = bsp_meshes_from(
            &bin(),
            elements,
            names,
            Vec3::new(100.0, 0.0, 0.0),
            WorldScale::PRESENTATION_METRES,
        );
        assert!((moved[0].positions[0][2] + 2.0).abs() < 1e-5);
        assert!((moved[0].positions[1][2] + 4.0).abs() < 1e-5);
        // A non-finite offset is ignored.
        let (same, _) = bsp_meshes_from(
            &bin(),
            elements,
            names,
            Vec3::new(f32::NAN, 0.0, 0.0),
            WorldScale::PRESENTATION_METRES,
        );
        assert_eq!(same, meshes);
        // Missing level: nothing.
        assert!(!l.has_bsp("Nope") && l.has_bsp("testmap"));
        assert!(
            l.bsp_meshes("Nope", Vec3::ZERO, WorldScale::PRESENTATION_METRES)
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn hostile_spans_never_panic() {
        let l = loaded();
        let (_, elements, names) = l.bsp.get("testmap").unwrap();
        let full = bin();
        for cut in 0..full.len() {
            let _ = bsp_meshes_from(
                &full[..cut],
                elements,
                names,
                Vec3::ZERO,
                WorldScale::PRESENTATION_METRES,
            );
        }
        let mut huge = elements.clone();
        for e in &mut huge {
            e.positions.count = usize::MAX;
            e.triangles.offset = usize::MAX;
        }
        let (m, d) = bsp_meshes_from(
            &full,
            &huge,
            names,
            Vec3::ZERO,
            WorldScale::PRESENTATION_METRES,
        );
        assert!(m.is_empty());
        assert_eq!(d, huge.len());
    }

    #[test]
    fn missing_files_are_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConvertedDir::new(dir.path());
        let l = LevelLightmaps::load(&conv, &["Nope".to_owned(), "../x".to_owned()]).unwrap();
        assert_eq!(l.missing.len(), 2);
        assert!(l.packages.is_empty());
        std::fs::create_dir_all(dir.path().join("lightmaps")).unwrap();
        std::fs::write(dir.path().join("lightmaps/TestMap.lightmaps.json"), doc()).unwrap();
        std::fs::write(dir.path().join("lightmaps/TestMap.bsp.bin"), bin()).unwrap();
        let l = LevelLightmaps::load(&conv, &["TestMap".to_owned()]).unwrap();
        assert_eq!(l.packages, vec!["TestMap".to_owned()]);
        let (meshes, dropped) = l
            .bsp_meshes("TestMap", Vec3::ZERO, WorldScale::PRESENTATION_METRES)
            .unwrap();
        assert_eq!((meshes.len(), dropped), (2, 3));
    }

    #[test]
    fn exposure_follows_the_directional_mapping() {
        let m = LightMapping::default();
        assert!(
            (lightmap_exposure(&m) - m.directional_lux_per_brightness / std::f32::consts::PI).abs()
                < 1e-3
        );
    }
}
