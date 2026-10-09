//! A converted level resolved into a render plan.
//!
//! [`ConvertedDir`] locates the files `asamu-import` wrote under one output
//! directory; [`LevelPlan::build`] combines a [`LevelScene`] with the mesh,
//! texture and material manifests into a flat list of draws that reference
//! **shared** mesh primitives and materials, so the renderer creates each
//! mesh and material handle once and instances it (the same mesh and
//! material on many draws lets the renderer batch them).
//!
//! Everything here is render-agnostic and deterministic; the Bevy app turns
//! the plan into entities.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use asamu_core::glam::Vec3;
use asamu_core::{Rotator, WorldScale};

use crate::bsp::{BspGeometry, BspRenderMesh};
use crate::error::{AssetError, AssetResult};
use crate::files::{MAX_MANIFEST_BYTES, MAX_SCENE_BYTES, read_bounded};
use crate::lighting::{LightMapping, RenderLight, RenderLightKind, map_light};
use crate::manifest::{MeshManifest, TextureManifest};
use crate::material_manifest::MaterialManifest;
use crate::materials::{MaterialSource, RenderMaterial, fallback_material};
use crate::scene::{LevelScene, PlayerStartInfo, StreamingInfo};
use crate::transform::{RenderTransform, decompose, instance_matrix};

/// Most streaming sub-levels merged into one plan. The shipped maps stream
/// at most one each (`LEVEL_FORMAT.md`); the cap only bounds crafted input.
pub const MAX_SUBLEVELS: usize = 16;

/// The directory tree written by `asamu-import --out <dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedDir {
    root: PathBuf,
}

/// Manifests found in a converted directory (each optional).
#[derive(Debug, Clone, Default)]
pub struct Manifests {
    /// `meshes/manifest.json`.
    pub meshes: Option<MeshManifest>,
    /// `textures/manifest.json`.
    pub textures: Option<TextureManifest>,
    /// `materials/materials.json`.
    pub materials: Option<MaterialManifest>,
}

impl ConvertedDir {
    /// Wraps an output directory of `asamu-import`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The importer's default output directory (per-OS user data directory +
    /// `asamu-decomp/converted`; the same rule as `asamu-import`'s
    /// `default_output_dir`), or `None` when no home/data directory is set.
    #[must_use]
    pub fn default_location() -> Option<Self> {
        let base = if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        } else if cfg!(target_os = "windows") {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        }?;
        Some(Self::new(base.join("asamu-decomp").join("converted")))
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `levels/`.
    #[must_use]
    pub fn levels_dir(&self) -> PathBuf {
        self.root.join("levels")
    }

    /// Level names (scene file stems) available, sorted.
    #[must_use]
    pub fn list_levels(&self) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(self.levels_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".scene.json").map(str::to_owned)
            })
            .collect();
        out.sort();
        out
    }

    /// The scene file of `level` (matched without regard to ASCII case).
    ///
    /// # Errors
    /// [`AssetError::LevelNotFound`].
    pub fn scene_path(&self, level: &str) -> AssetResult<PathBuf> {
        let not_found = || AssetError::LevelNotFound {
            level: level.to_owned(),
            dir: self.levels_dir(),
        };
        // The level name becomes part of a file name: refuse separators and
        // the like before touching the file system.
        if level.is_empty()
            || level.contains(['/', '\\', ':', '\0'])
            || level == "."
            || level == ".."
        {
            return Err(not_found());
        }
        self.list_levels()
            .into_iter()
            .find(|l| l.eq_ignore_ascii_case(level))
            .map(|l| self.levels_dir().join(format!("{l}.scene.json")))
            .ok_or_else(not_found)
    }

    fn manifest_bytes(&self, sub: &str, file: &str) -> AssetResult<Option<(PathBuf, Vec<u8>)>> {
        let path = self.root.join(sub).join(file);
        if !path.is_file() {
            return Ok(None);
        }
        let data = read_bounded(&path, MAX_MANIFEST_BYTES)?;
        Ok(Some((path, data)))
    }

    /// Reads whichever manifests exist.
    ///
    /// # Errors
    /// A manifest that exists but cannot be read or parsed.
    pub fn load_manifests(&self) -> AssetResult<Manifests> {
        let meshes = self
            .manifest_bytes("meshes", "manifest.json")?
            .map(|(p, d)| MeshManifest::from_json(&p, &d))
            .transpose()?;
        let textures = self
            .manifest_bytes("textures", "manifest.json")?
            .map(|(p, d)| TextureManifest::from_json(&p, &d))
            .transpose()?;
        let materials = self
            .manifest_bytes("materials", "materials.json")?
            .map(|(p, d)| MaterialManifest::from_json(&p, &d))
            .transpose()?;
        Ok(Manifests {
            meshes,
            textures,
            materials,
        })
    }

    /// The BSP index of `level`, when it was converted.
    #[must_use]
    pub fn bsp_path(&self, level: &str) -> Option<PathBuf> {
        let scene = self.scene_path(level).ok()?;
        let stem = scene.file_name()?.to_str()?.strip_suffix(".scene.json")?;
        let p = scene.with_file_name(format!("{stem}.bsp.json"));
        p.is_file().then_some(p)
    }

    /// Reads and parses the scene of `level`.
    ///
    /// # Errors
    /// Missing level, I/O or parse errors.
    pub fn load_scene(&self, level: &str) -> AssetResult<LevelScene> {
        let path = self.scene_path(level)?;
        let data = read_bounded(&path, MAX_SCENE_BYTES)?;
        LevelScene::from_json(&path, &data)
    }
}

/// A shared mesh primitive: one glTF file's primitive of mesh 0.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrimitiveAsset {
    /// glTF file relative to `meshes/` (validated).
    pub gltf: String,
    /// Primitive index in glTF mesh 0.
    pub primitive: usize,
}

/// One thing to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Draw {
    /// Index into [`LevelPlan::primitives`].
    pub primitive: usize,
    /// Index into [`LevelPlan::materials`].
    pub material: usize,
    /// Render transform.
    pub transform: RenderTransform,
    /// Index of the instance in [`LevelScene::meshes`].
    pub instance: usize,
    /// Owning actor slot.
    pub actor_slot: usize,
    /// The section casts shadows (`bEnableShadowCasting`).
    pub cast_shadow: bool,
}

/// A BSP mesh (already in render space) and its material.
#[derive(Debug, Clone, PartialEq)]
pub struct BspDraw {
    /// Index into [`LevelPlan::materials`].
    pub material: usize,
    /// The mesh.
    pub mesh: BspRenderMesh,
}

/// Options of [`LevelPlan::build`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanOptions {
    /// UE3 → render scale.
    pub scale: WorldScale,
    /// Light mapping constants.
    pub lights: LightMapping,
    /// Also merge the sub-levels Kismet streams in at run time (otherwise
    /// only `LevelStreamingAlwaysLoaded` ones).
    pub all_sublevels: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            scale: WorldScale::PRESENTATION_METRES,
            lights: LightMapping::default(),
            all_sublevels: false,
        }
    }
}

/// Counts and problems of a plan (for the HUD and the log).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanStats {
    /// Mesh instances in the scene.
    pub instances: usize,
    /// Instances drawn (mesh found, transform usable).
    pub instances_drawn: usize,
    /// Draws (instance × primitive).
    pub draws: usize,
    /// Distinct mesh primitives.
    pub primitives: usize,
    /// Distinct materials.
    pub materials: usize,
    /// Materials from the converted material manifest.
    pub materials_converted: usize,
    /// Materials from the fallback.
    pub materials_fallback: usize,
    /// Distinct textures referenced by the materials.
    pub textures: usize,
    /// Texture references that were not converted.
    pub textures_missing: usize,
    /// Mesh paths with no converted entry (and how many instances use them).
    pub missing_meshes: BTreeMap<String, usize>,
    /// Instances whose transform could not be decomposed.
    pub bad_transforms: usize,
    /// Mirrored draws.
    pub mirrored_draws: usize,
    /// Largest shear lost in decomposition.
    pub max_shear: f32,
    /// Streaming sub-levels that should be merged but were not converted.
    pub missing_sublevels: Vec<String>,
    /// Further distinct sub-levels beyond [`MAX_SUBLEVELS`] (not looked up).
    pub sublevels_over_cap: usize,
    /// BSP triangles drawn.
    pub bsp_triangles: usize,
    /// BSP triangles dropped (invalid indices or surfaces).
    pub bsp_dropped: usize,
    /// Lights mapped / skipped (disabled, no effect, unknown class).
    pub lights_mapped: usize,
    /// Lights skipped.
    pub lights_skipped: usize,
}

/// Materials interned by path, resolved through the material manifest or
/// the fallback.
struct MaterialTable<'a> {
    manifests: &'a Manifests,
    list: Vec<RenderMaterial>,
    index: HashMap<Option<String>, usize>,
}

impl<'a> MaterialTable<'a> {
    fn new(manifests: &'a Manifests) -> Self {
        Self {
            manifests,
            list: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn intern(&mut self, path: Option<String>) -> usize {
        if let Some(&i) = self.index.get(&path) {
            return i;
        }
        let m = path
            .as_deref()
            .and_then(|p| {
                self.manifests
                    .materials
                    .as_ref()
                    .and_then(|mm| mm.render_material(p, self.manifests.textures.as_ref()))
            })
            .unwrap_or_else(|| fallback_material(path.as_deref()));
        let i = self.list.len();
        self.list.push(m);
        self.index.insert(path, i);
        i
    }
}

/// A level ready for the renderer.
#[derive(Debug, Clone)]
pub struct LevelPlan {
    /// The parsed scene.
    pub scene: LevelScene,
    /// Distinct mesh primitives.
    pub primitives: Vec<PrimitiveAsset>,
    /// Distinct materials.
    pub materials: Vec<RenderMaterial>,
    /// Draws.
    pub draws: Vec<Draw>,
    /// BSP meshes (one per material).
    pub bsp: Vec<BspDraw>,
    /// Render lights (point, spot, directional).
    pub lights: Vec<RenderLight>,
    /// Ambient brightness (base + sky lights).
    pub ambient: f32,
    /// Counts.
    pub stats: PlanStats,
    /// The scale used.
    pub scale: WorldScale,
    /// Largest distance from the render origin reached by any drawn mesh
    /// (its bounding sphere) or BSP vertex, in render units. Sky domes make
    /// this large; a camera inside the level never sees anything farther
    /// than twice this.
    pub extent: f32,
}

impl LevelPlan {
    /// Reads the converted files of `level` and builds its plan.
    ///
    /// # Errors
    /// Missing level, unreadable scene or manifests.
    pub fn load(dir: &ConvertedDir, level: &str, options: &PlanOptions) -> AssetResult<Self> {
        let mut scene = dir.load_scene(level)?;
        let manifests = dir.load_manifests()?;
        let mut bsp = dir
            .bsp_path(level)
            .map(|p| BspGeometry::load(&p))
            .transpose()?;
        // Streaming sub-levels: the always-loaded ones (and, on request, the
        // Kismet-streamed ones) are merged when they were converted. Each is
        // merged once, never the persistent level itself, and at most
        // `MAX_SUBLEVELS` of them are looked up: a crafted scene must not make
        // the loader read the same (possibly large) files over and over.
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(scene.package.to_ascii_lowercase());
        seen.insert(level.to_ascii_lowercase());
        let candidates: Vec<StreamingInfo> = scene
            .streaming_levels
            .iter()
            .filter(|s| s.always_loaded() || options.all_sublevels)
            .filter(|s| seen.insert(s.package.to_ascii_lowercase()))
            .cloned()
            .collect();
        let over_cap = candidates.len().saturating_sub(MAX_SUBLEVELS);
        let mut missing_sublevels = Vec::new();
        for sub in candidates.into_iter().take(MAX_SUBLEVELS) {
            if dir.scene_path(&sub.package).is_err() {
                missing_sublevels.push(sub.package.clone());
                continue;
            }
            let sub_scene = dir.load_scene(&sub.package)?;
            scene.merge_sublevel(sub_scene, sub.offset);
            if let Some(p) = dir.bsp_path(&sub.package) {
                let mut g = BspGeometry::load(&p)?;
                g.translate(sub.offset);
                match bsp.as_mut() {
                    Some(b) => b.append(g),
                    None => bsp = Some(g),
                }
            }
        }
        let mut plan = Self::build_with_bsp(scene, bsp.as_ref(), &manifests, options);
        plan.stats.missing_sublevels = missing_sublevels;
        plan.stats.sublevels_over_cap = over_cap;
        Ok(plan)
    }

    /// Builds the plan from a parsed scene and manifests (no BSP).
    #[must_use]
    pub fn build(scene: LevelScene, manifests: &Manifests, options: &PlanOptions) -> Self {
        Self::build_with_bsp(scene, None, manifests, options)
    }

    /// Builds the plan from a parsed scene, the level BSP and manifests.
    #[must_use]
    pub fn build_with_bsp(
        scene: LevelScene,
        bsp: Option<&BspGeometry>,
        manifests: &Manifests,
        options: &PlanOptions,
    ) -> Self {
        let mut stats = PlanStats {
            instances: scene.meshes.len(),
            ..PlanStats::default()
        };
        let mut primitives: Vec<PrimitiveAsset> = Vec::new();
        let mut primitive_index: HashMap<PrimitiveAsset, usize> = HashMap::new();
        let mut materials = MaterialTable::new(manifests);
        let mut draws = Vec::new();
        let mut extent = 0.0_f32;

        for (instance_idx, inst) in scene.meshes.iter().enumerate() {
            let entry = manifests
                .meshes
                .as_ref()
                .and_then(|m| m.get_for_package(&inst.mesh, &scene.package))
                .map(|(_, e)| e);
            let Some((entry, lod)) = entry.and_then(|e| e.lod0().map(|l| (e, l))) else {
                *stats.missing_meshes.entry(inst.mesh.clone()).or_default() += 1;
                continue;
            };
            let Ok(gltf) = lod.safe_gltf() else {
                *stats.missing_meshes.entry(inst.mesh.clone()).or_default() += 1;
                continue;
            };
            let m = instance_matrix(&inst.ue_local_to_world, entry.scale, options.scale);
            let Some(transform) = decompose(&m) else {
                stats.bad_transforms += 1;
                continue;
            };
            stats.instances_drawn += 1;
            stats.max_shear = stats.max_shear.max(transform.shear);
            if let Some(b) = entry.bounds_ue {
                let origin = asamu_core::glam::Vec3::from_array(b.origin);
                let r = (origin.length() + b.sphere_radius.abs())
                    * transform.scale.abs().max_element()
                    / entry.scale.abs().max(f32::MIN_POSITIVE);
                let d = transform.translation.length() + r;
                if d.is_finite() {
                    extent = extent.max(d);
                }
            }
            for p in lod.primitives() {
                let Some(section) = lod.sections.get(p.section) else {
                    continue;
                };
                let key = PrimitiveAsset {
                    gltf: gltf.clone(),
                    primitive: p.primitive,
                };
                let pi = *primitive_index.entry(key.clone()).or_insert_with(|| {
                    primitives.push(key);
                    primitives.len() - 1
                });
                // UE3 convention (TENTATIVE): component override for the
                // section's slot index, else the section's own material.
                let material = inst
                    .material_overrides
                    .get(p.section)
                    .cloned()
                    .flatten()
                    .or_else(|| section.material.clone());
                let mi = materials.intern(material);
                if transform.mirrored {
                    stats.mirrored_draws += 1;
                }
                draws.push(Draw {
                    primitive: pi,
                    material: mi,
                    transform,
                    instance: instance_idx,
                    actor_slot: inst.actor_slot,
                    cast_shadow: section.cast_shadow,
                });
            }
        }

        let mut bsp_draws = Vec::new();
        if let Some(g) = bsp {
            stats.bsp_dropped = g.dropped;
            let meshes = g.render_meshes(options.scale);
            for mesh in meshes {
                stats.bsp_triangles += mesh.indices.len() / 3;
                for p in &mesh.positions {
                    let d = asamu_core::glam::Vec3::from_array(*p).length();
                    if d.is_finite() {
                        extent = extent.max(d);
                    }
                }
                let mi = materials.intern(mesh.material.clone());
                bsp_draws.push(BspDraw { material: mi, mesh });
            }
        }

        let mut ambient = options.lights.base_ambient;
        let mut lights = Vec::new();
        for l in &scene.lights {
            match map_light(l, &options.lights, options.scale) {
                Some(RenderLight {
                    kind: RenderLightKind::Ambient { brightness },
                    ..
                }) => {
                    ambient += brightness;
                    stats.lights_mapped += 1;
                }
                Some(r) => {
                    lights.push(r);
                    stats.lights_mapped += 1;
                }
                None => stats.lights_skipped += 1,
            }
        }

        let materials = materials.list;
        let mut textures: Vec<&str> = Vec::new();
        for m in &materials {
            match m.source {
                MaterialSource::Converted => stats.materials_converted += 1,
                MaterialSource::Fallback => stats.materials_fallback += 1,
            }
            for t in [
                &m.base_color_texture,
                &m.normal_texture,
                &m.emissive_texture,
            ]
            .into_iter()
            .flatten()
            {
                textures.push(t.file.as_str());
            }
            stats.textures_missing += m.missing_textures.len();
        }
        textures.sort_unstable();
        textures.dedup();
        stats.textures = textures.len();
        stats.draws = draws.len();
        stats.primitives = primitives.len();
        stats.materials = materials.len();

        Self {
            scene,
            primitives,
            materials,
            draws,
            bsp: bsp_draws,
            lights,
            ambient,
            stats,
            scale: options.scale,
            extent,
        }
    }

    /// Where a camera should start: the player start the simulation spawns
    /// at ([`LevelScene::player_start`]), as (UE3 location of the collision
    /// centre, yaw radians, pitch radians).
    #[must_use]
    pub fn start(&self) -> Option<(Vec3, f32, f32)> {
        let p: &PlayerStartInfo = self.scene.player_start()?;
        let (pitch, yaw, _) =
            Rotator::new(p.rotation[0], p.rotation[1], p.rotation[2]).to_radians();
        Some((p.location_ue, yaw, pitch))
    }

    /// A rough radius of the level in render units (from instance and light
    /// origins), at least 10 and always finite (extreme but finite origins
    /// in a damaged scene can overflow the span; then 10 is used).
    #[must_use]
    pub fn render_radius(&self) -> f32 {
        self.scene
            .origin_bounds()
            .map(|(lo, hi)| (hi - lo).length() * 0.5 * self.scale.bevy_units_per_uu)
            .filter(|r| r.is_finite())
            .map_or(10.0, |r| r.max(10.0))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::manifest::{LodEntry, MeshEntry, SectionEntry};

    fn scene() -> LevelScene {
        let json = crate::scene::tests::synthetic_scene_json();
        LevelScene::from_json(Path::new("TestMap.scene.json"), json.as_bytes()).unwrap()
    }

    fn mesh_manifest() -> MeshManifest {
        let mut m = BTreeMap::new();
        let section = |mat: Option<&str>, tris: u32| SectionEntry {
            material: mat.map(str::to_owned),
            first_index: 0,
            triangles: tris,
            collision: true,
            cast_shadow: true,
        };
        m.insert(
            "Pkg.Meshes.Box".to_owned(),
            MeshEntry {
                package: "TestMap".to_owned(),
                lods: vec![LodEntry {
                    lod: 0,
                    gltf: "TestMap/Pkg/Meshes/Box.gltf".to_owned(),
                    sections: vec![section(Some("Pkg.M_A"), 2), section(Some("Pkg.M_B"), 4)],
                }],
                bounds_ue: None,
                scale: 1.0,
            },
        );
        MeshManifest::from_entries(1, m)
    }

    #[test]
    fn plan_shares_primitives_and_materials_and_applies_overrides() {
        let manifests = Manifests {
            meshes: Some(mesh_manifest()),
            textures: None,
            materials: None,
        };
        let plan = LevelPlan::build(scene(), &manifests, &PlanOptions::default());
        // The box (2 primitives) is drawn; the door mesh is not converted.
        assert_eq!(plan.stats.instances, 2);
        assert_eq!(plan.stats.instances_drawn, 1);
        assert_eq!(plan.stats.missing_meshes.get("Pkg.Meshes.Door"), Some(&1));
        assert_eq!(plan.draws.len(), 2);
        assert_eq!(plan.primitives.len(), 2);
        // Slot 1 is overridden by the component.
        let mats: Vec<_> = plan
            .draws
            .iter()
            .map(|d| plan.materials[d.material].path.clone())
            .collect();
        assert_eq!(
            mats,
            vec![
                Some("Pkg.M_A".to_owned()),
                Some("Pkg.M_Override".to_owned())
            ]
        );
        assert_eq!(plan.stats.materials_fallback, 2);
        // Transform: UE (100, 200, 300) at 50 uu/m → Bevy (4, 6, -2), scale 2.
        let t = plan.draws[0].transform;
        assert!((t.translation - Vec3::new(4.0, 6.0, -2.0)).length() < 1e-4);
        assert!((t.scale - Vec3::splat(2.0 / 50.0)).length() < 1e-5);
        assert!(!t.mirrored);
        // Lights: point + directional; base ambient only.
        assert_eq!(plan.lights.len(), 2);
        assert_eq!(plan.ambient, LightMapping::default().base_ambient);
        let (loc, yaw, pitch) = plan.start().unwrap();
        assert_eq!(loc, Vec3::new(-50.0, 25.0, 90.0));
        assert!((yaw - core::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(pitch, 0.0);
        assert!(plan.render_radius() >= 10.0);
        // No bounds in the synthetic manifest: the extent stays 0.
        assert_eq!(plan.extent, 0.0);
    }

    #[test]
    fn repeated_meshes_reuse_the_same_primitive_and_material() {
        let mut s = scene();
        let first = s.meshes[0].clone();
        for _ in 0..10 {
            s.meshes.push(first.clone());
        }
        let manifests = Manifests {
            meshes: Some(mesh_manifest()),
            ..Manifests::default()
        };
        let plan = LevelPlan::build(s, &manifests, &PlanOptions::default());
        assert_eq!(plan.draws.len(), 22);
        assert_eq!(plan.primitives.len(), 2);
        assert_eq!(plan.materials.len(), 2);
    }

    #[test]
    fn without_manifests_nothing_is_drawn_but_lights_work() {
        let plan = LevelPlan::build(scene(), &Manifests::default(), &PlanOptions::default());
        assert!(plan.draws.is_empty());
        assert_eq!(plan.stats.missing_meshes.len(), 2);
        assert_eq!(plan.lights.len(), 2);
    }

    #[test]
    fn converted_dir_finds_levels_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ConvertedDir::new(tmp.path());
        assert!(dir.list_levels().is_empty());
        assert!(matches!(
            dir.scene_path("AG-Workshop"),
            Err(AssetError::LevelNotFound { .. })
        ));
        std::fs::create_dir_all(dir.levels_dir()).unwrap();
        std::fs::write(
            dir.levels_dir().join("TestMap.scene.json"),
            crate::scene::tests::synthetic_scene_json(),
        )
        .unwrap();
        let (bsp_json, bsp_bin) = crate::bsp::tests::synthetic_bsp();
        let bsp_json = bsp_json.replacen("Test.bsp.bin", "TestMap.bsp.bin", 1);
        std::fs::write(dir.levels_dir().join("TestMap.bsp.json"), &bsp_json).unwrap();
        std::fs::write(dir.levels_dir().join("TestMap.bsp.bin"), &bsp_bin).unwrap();
        assert_eq!(dir.list_levels(), vec!["TestMap".to_owned()]);
        assert!(dir.scene_path("testmap").is_ok());
        for bad in ["", "..", "../TestMap", "a/b", "C:x"] {
            assert!(dir.scene_path(bad).is_err(), "{bad}");
        }
        // No manifests: all None, plan still builds.
        let m = dir.load_manifests().unwrap();
        assert!(m.meshes.is_none() && m.textures.is_none() && m.materials.is_none());
        let plan = LevelPlan::load(&dir, "TESTMAP", &PlanOptions::default()).unwrap();
        assert_eq!(plan.scene.package, "TestMap");
        // The BSP floor is drawn with its (fallback) material.
        assert_eq!(plan.bsp.len(), 1);
        assert_eq!(plan.stats.bsp_triangles, 2);
        assert_eq!(plan.stats.bsp_dropped, 1);
        assert_eq!(
            plan.materials[plan.bsp[0].material].path.as_deref(),
            Some("Pkg.M_Floor")
        );
        // A corrupt BSP index is an error.
        std::fs::write(dir.levels_dir().join("TestMap.bsp.json"), "{}").unwrap();
        assert!(LevelPlan::load(&dir, "TestMap", &PlanOptions::default()).is_err());
        // A corrupt manifest is an error, not a panic.
        std::fs::create_dir_all(tmp.path().join("meshes")).unwrap();
        std::fs::write(tmp.path().join("meshes/manifest.json"), "{not json").unwrap();
        assert!(dir.load_manifests().is_err());
    }

    /// The synthetic scene with its streaming list replaced.
    fn scene_streaming(package: &str, streaming: &str) -> String {
        let json = crate::scene::tests::synthetic_scene_json();
        let start = json.find("\"streaming_levels\"").unwrap();
        let end = json.find("\"bsp_model\"").unwrap();
        let json = format!(
            "{}\"streaming_levels\": [{streaming}],\n  {}",
            &json[..start],
            &json[end..]
        );
        json.replacen(
            "\"package\": \"TestMap\"",
            &format!("\"package\": \"{package}\""),
            1,
        )
    }

    fn streaming_entry(package: &str) -> String {
        format!(
            r#"{{"class": "LevelStreamingAlwaysLoaded", "package_name": "{package}", "offset": [0, 0, 1000]}}"#
        )
    }

    #[test]
    fn sublevels_merge_once_and_the_lookup_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ConvertedDir::new(tmp.path());
        std::fs::create_dir_all(dir.levels_dir()).unwrap();
        // The map streams itself (twice, in two spellings), one converted
        // sub-level three times, then many sub-levels that do not exist.
        let mut list = vec![
            streaming_entry("TestMap"),
            streaming_entry("testmap"),
            streaming_entry("Sub"),
            streaming_entry("SUB"),
            streaming_entry("Sub"),
        ];
        list.extend((0..40).map(|i| streaming_entry(&format!("Missing{i}"))));
        std::fs::write(
            dir.levels_dir().join("TestMap.scene.json"),
            scene_streaming("TestMap", &list.join(",")),
        )
        .unwrap();
        // The sub-level streams the persistent level back: not followed.
        std::fs::write(
            dir.levels_dir().join("Sub.scene.json"),
            scene_streaming("Sub", &streaming_entry("TestMap")),
        )
        .unwrap();
        let plan = LevelPlan::load(&dir, "TestMap", &PlanOptions::default()).unwrap();
        assert_eq!(plan.scene.merged_levels, vec!["Sub".to_owned()]);
        assert_eq!(plan.scene.meshes.len(), 4, "2 own + 2 from Sub");
        assert_eq!(
            plan.scene.meshes[2].ue_local_to_world.w_axis.truncate(),
            Vec3::new(100.0, 200.0, 1300.0),
            "moved by the streaming offset"
        );
        assert_eq!(plan.stats.missing_sublevels.len(), MAX_SUBLEVELS - 1);
        assert_eq!(plan.stats.missing_sublevels[0], "Missing0");
        assert_eq!(plan.stats.sublevels_over_cap, 41 - MAX_SUBLEVELS);
        // Kismet-streamed sub-levels are skipped unless requested.
        let kismet = streaming_entry("Sub").replace("AlwaysLoaded", "Kismet");
        std::fs::write(
            dir.levels_dir().join("TestMap.scene.json"),
            scene_streaming("TestMap", &kismet),
        )
        .unwrap();
        let plan = LevelPlan::load(&dir, "TestMap", &PlanOptions::default()).unwrap();
        assert!(plan.scene.merged_levels.is_empty());
        let all = PlanOptions {
            all_sublevels: true,
            ..PlanOptions::default()
        };
        let plan = LevelPlan::load(&dir, "TestMap", &all).unwrap();
        assert_eq!(plan.scene.merged_levels, vec!["Sub".to_owned()]);
        // A converted sub-level whose scene is corrupt is an error.
        std::fs::write(dir.levels_dir().join("Sub.scene.json"), "{").unwrap();
        assert!(LevelPlan::load(&dir, "TestMap", &all).is_err());
    }

    #[test]
    fn hostile_mesh_entries_never_yield_non_finite_draws() {
        let mut m = BTreeMap::new();
        for (i, scale) in [0.0, f32::NAN, -1.0, 1e-30, 1e30, f32::INFINITY]
            .into_iter()
            .enumerate()
        {
            m.insert(
                "Pkg.Meshes.Box".to_owned() + &"@TestMap".repeat(usize::from(i == 0)),
                MeshEntry {
                    package: "TestMap".to_owned(),
                    lods: vec![LodEntry {
                        lod: usize::MAX,
                        gltf: "TestMap/Box.gltf".to_owned(),
                        sections: vec![SectionEntry {
                            material: None,
                            first_index: u32::MAX,
                            triangles: u32::MAX,
                            collision: false,
                            cast_shadow: false,
                        }],
                    }],
                    bounds_ue: Some(crate::manifest::BoundsEntry {
                        origin: [f32::NAN, f32::INFINITY, 1e38],
                        box_extent: [f32::NAN; 3],
                        sphere_radius: f32::INFINITY,
                    }),
                    scale,
                },
            );
            let manifests = Manifests {
                meshes: Some(MeshManifest::from_entries(1, m.clone())),
                ..Manifests::default()
            };
            let plan = LevelPlan::build(scene(), &manifests, &PlanOptions::default());
            for d in &plan.draws {
                let t = d.transform;
                assert!(t.translation.is_finite() && t.scale.is_finite() && t.rotation.is_finite());
                assert!(plan.materials[d.material].path.is_none());
            }
            assert!(plan.extent.is_finite());
            m.clear();
        }
    }

    #[test]
    fn plans_are_deterministic_and_radius_stays_finite() {
        let manifests = Manifests {
            meshes: Some(mesh_manifest()),
            ..Manifests::default()
        };
        let a = LevelPlan::build(scene(), &manifests, &PlanOptions::default());
        let b = LevelPlan::build(scene(), &manifests, &PlanOptions::default());
        assert_eq!(a.draws, b.draws);
        assert_eq!(a.primitives, b.primitives);
        assert_eq!(a.materials, b.materials);
        assert_eq!(a.lights, b.lights);
        assert_eq!(a.stats, b.stats);
        // Origins at opposite ends of the f32 range overflow the span.
        let mut s = scene();
        s.meshes[0].ue_local_to_world.w_axis.x = f32::MAX;
        s.meshes[1].ue_local_to_world.w_axis.x = -f32::MAX;
        let p = LevelPlan::build(s, &manifests, &PlanOptions::default());
        assert_eq!(p.render_radius(), 10.0);
        assert!(p.extent.is_finite());
    }
}
