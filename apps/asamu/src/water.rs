//! Water surfaces, instanced foliage and SpeedTree placements of converted
//! levels (`docs/reverse-engineering/WATER_FOLIAGE.md`).
//!
//! - **Instanced foliage** (`InstancedStaticMeshComponent` instances of the
//!   `InstancedFoliageActor`s) needs nothing here: the scene reader
//!   ([`asamu_assets::scene`]) expands every instanced component into one
//!   mesh instance per instance from the scene's foliage block, so the level
//!   plan and `converted.rs` draw them like any other static mesh (shared
//!   primitives and materials, so they batch). This plugin only reports the
//!   counts.
//! - **Water** (`FluidSurfaceActor` / `FluidSurfaceComponent`): a flat,
//!   subdivided rectangle of `FluidWidth` × `FluidHeight` centred on the
//!   component (the original's extent rule, CONFIRMED from the Mac
//!   executable), UVs 0..1 across the surface (STRONG: the original's fluid
//!   vertices carry normalized grid UVs), with an **approximate** material:
//!   the converted description of the fluid material when there is one
//!   (its textures and UV transform), clamped to a translucent, smooth
//!   water look. The original's height-field simulation (ripples from
//!   impacts, `EnableSimulation`) is not reproduced.
//! - **SpeedTree**: every shipped placement (four in AG-Darkcave) names no
//!   tree asset and the shipped packages contain no `SpeedTree` object, so
//!   the original draws nothing for them either (CONFIRMED map data); they
//!   are logged. A tree asset could not be shown anyway: the Mac build's
//!   loader skips the tree's binary blob without reading it.
//!
//! The water entities carry [`LevelEntity`], so the menu flow despawns them
//! with the rest of the level, the streaming gate hides them with their
//! sub-level, and Kismet's hide actions apply to their actor.

use asamu_assets::materials::UvTransform;
use asamu_assets::scene::{LevelScene, SpeedTreeInfo, WaterSurfaceInfo};
use asamu_assets::{BlendMode, ConvertedDir, RenderMaterial, TextureBinding};
use asamu_core::WorldScale;
use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};
use asamu_core::glam as sim_glam;
use bevy::asset::RenderAssetUsages;
use bevy::image::{
    ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor,
};
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::converted::{ConvertedLevel, LevelEntity, LevelPhase, SOURCE};

/// Quads along each side of a water mesh (render resolution only; the
/// surface is flat).
pub const WATER_SUBDIVISIONS: u32 = 32;
/// Largest opacity given to water (ours: the converted fluid material is
/// fully opaque in its approximation, and a sea you cannot see into reads
/// as a floor).
pub const WATER_MAX_ALPHA: f32 = 0.85;
/// Perceptual roughness of the water approximation (ours).
pub const WATER_ROUGHNESS: f32 = 0.08;
/// Colour of water without a converted material (ours, a placeholder).
pub const WATER_FALLBACK_COLOR: [f32; 4] = [0.05, 0.22, 0.32, 0.8];

/// Marker on spawned water surfaces (their `Name` carries the actor).
#[derive(Component, Debug, Clone, Copy)]
pub struct WaterSurface;

/// What the plugin loaded for a level (built on the async pool).
#[derive(Debug, Clone, Default)]
pub struct WaterLoad {
    /// Level name (as in [`ConvertedLevel::level`]).
    pub level: String,
    /// Water surfaces (persistent level and merged sub-levels).
    pub water: Vec<WaterSurfaceInfo>,
    /// The converted description of each surface's material (same order).
    pub materials: Vec<Option<RenderMaterial>>,
    /// SpeedTree placements.
    pub speedtrees: Vec<SpeedTreeInfo>,
    /// Instanced components, instances drawn, components without data.
    pub instanced: (usize, usize, usize),
    /// Why the foliage block was ignored, if it was.
    pub block_error: Option<String>,
    /// The scene has a foliage block.
    pub has_block: bool,
    /// Entries of the block the reader left out (more than its bounds;
    /// never on a scene the importer wrote).
    pub over_limit: usize,
}

/// Loading state.
#[derive(Resource, Default)]
pub struct WaterState {
    /// Level the current task / spawn belongs to.
    level: Option<String>,
    task: Option<Task<Result<WaterLoad, String>>>,
    /// Loaded data waiting for the level to be spawned.
    pending: Option<WaterLoad>,
    /// One status line for logs and checks.
    pub status: String,
}

/// Water/fluid surfaces, foliage and SpeedTree approximations.
pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WaterState>()
            .add_systems(Update, (start_loading, poll_loading, spawn_water).chain());
    }
}

/// The level's scene with its always-loaded sub-levels merged (the same
/// rule as `LevelPlan::load` without Kismet-streamed levels), plus the
/// converted descriptions of the water materials.
fn load(dir: &ConvertedDir, level: &str, all_sublevels: bool) -> Result<WaterLoad, String> {
    let mut scene = dir.load_scene(level).map_err(|e| e.to_string())?;
    let subs: Vec<_> = scene
        .streaming_levels
        .iter()
        .filter(|s| s.always_loaded() || all_sublevels)
        .filter(|s| !s.package.eq_ignore_ascii_case(level))
        .take(asamu_assets::level::MAX_SUBLEVELS)
        .cloned()
        .collect();
    for sub in subs {
        if let Ok(s) = dir.load_scene(&sub.package) {
            scene.merge_sublevel(s, sub.offset);
        }
    }
    let manifests = dir.load_manifests().unwrap_or_default();
    Ok(water_load(level, scene, &manifests))
}

/// Builds the plugin's view of a parsed scene.
#[must_use]
pub fn water_load(
    level: &str,
    scene: LevelScene,
    manifests: &asamu_assets::Manifests,
) -> WaterLoad {
    let materials = scene
        .foliage
        .water
        .iter()
        .map(|w| {
            let path = w.material.as_deref()?;
            manifests
                .materials
                .as_ref()?
                .render_material(path, manifests.textures.as_ref())
        })
        .collect();
    WaterLoad {
        level: level.to_owned(),
        materials,
        instanced: (
            scene.stats.instanced_components,
            scene.stats.instances,
            scene.stats.instanced_without_data,
        ),
        block_error: scene.foliage.block_error.clone(),
        has_block: scene.foliage.block_version.is_some(),
        over_limit: scene.foliage.over_limit,
        speedtrees: scene.foliage.speedtrees,
        water: scene.foliage.water,
    }
}

fn start_loading(mut state: ResMut<WaterState>, level: Option<Res<ConvertedLevel>>) {
    let Some(level) = level else {
        return;
    };
    if state
        .level
        .as_deref()
        .is_some_and(|l| l.eq_ignore_ascii_case(&level.level))
    {
        return;
    }
    let dir = level.dir.clone();
    let name = level.level.clone();
    let all = level.force_all_sublevels || level.options.all_sublevels;
    state.level = Some(name.clone());
    state.pending = None;
    state.status = format!("water: loading {name}");
    state.task = Some(AsyncComputeTaskPool::get().spawn(async move { load(&dir, &name, all) }));
}

fn poll_loading(mut state: ResMut<WaterState>) {
    let Some(task) = state.task.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    state.task = None;
    match result {
        Ok(load) => {
            state.status = summary(&load);
            info!("{}", state.status);
            if let Some(e) = &load.block_error {
                warn!("water/foliage: the scene's foliage block was ignored: {e}");
            }
            for t in &load.speedtrees {
                debug!(
                    "SpeedTree placement {} ({}): tree {:?} — nothing drawn",
                    t.actor_name, t.component_name, t.speedtree
                );
            }
            state.pending = Some(load);
        }
        Err(e) => {
            state.status = format!("water: could not read the scene: {e}");
            warn!("{}", state.status);
        }
    }
}

/// One line describing a load.
#[must_use]
pub fn summary(load: &WaterLoad) -> String {
    let (components, instances, without) = load.instanced;
    let mut s = format!(
        "water/foliage {}: {} water surfaces, {} instanced components ({} instances drawn",
        load.level,
        load.water.len(),
        components,
        instances
    );
    if without > 0 {
        s.push_str(&format!(
            ", {without} without instance data: re-run `asamu-import levels`"
        ));
    }
    s.push_str(&format!(
        "), {} SpeedTree placements ({} with a tree; none drawn)",
        load.speedtrees.len(),
        load.speedtrees
            .iter()
            .filter(|t| t.speedtree.is_some())
            .count()
    ));
    if !load.has_block {
        s.push_str("; the scene has no foliage block");
    }
    if load.over_limit > 0 {
        s.push_str(&format!(
            "; {} entries over the reader's bounds left out",
            load.over_limit
        ));
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn spawn_water(
    mut commands: Commands,
    mut state: ResMut<WaterState>,
    level: Option<Res<ConvertedLevel>>,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(level) = level else {
        return;
    };
    if !matches!(level.phase, Some(LevelPhase::Spawned(_))) {
        return;
    }
    let Some(load) = state.pending.take() else {
        return;
    };
    if !load.level.eq_ignore_ascii_case(&level.level) {
        return;
    }
    let scale = level.options.scale;
    for (w, m) in load.water.iter().zip(&load.materials) {
        if w.hidden {
            continue;
        }
        let Some(mesh) = water_mesh(w, scale, WATER_SUBDIVISIONS) else {
            warn!("water surface {}: unusable transform", w.actor_name);
            continue;
        };
        let look = water_look(m.as_ref());
        let material = materials.add(standard_water_material(&look, &server));
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            LevelEntity {
                actor_slot: w.actor_slot,
                level: w.level,
            },
            WaterSurface,
            Name::new(format!("Water {}", w.actor_name)),
        ));
    }
}

/// A water rectangle as a render mesh: `subdivisions`² quads with positions
/// already in render space (the transform stays identity), normals along
/// the surface's local +Z, UVs 0..1 (U along local X, V along local Y). Both
/// faces render (the material is double-sided). `None` for an unusable
/// transform or size.
#[must_use]
pub fn water_mesh(w: &WaterSurfaceInfo, scale: WorldScale, subdivisions: u32) -> Option<Mesh> {
    let n = subdivisions.clamp(1, 256);
    let m = w.ue_local_to_world;
    let usable = |x: f32| x.is_finite() && x > 0.0;
    if !m.is_finite() || !usable(w.width) || !usable(w.height) {
        return None;
    }
    let normal_ue = m.transform_vector3(sim_glam::Vec3::Z).normalize_or_zero();
    if normal_ue == sim_glam::Vec3::ZERO {
        return None;
    }
    let normal = ue_dir_to_bevy(normal_ue).to_array();
    let verts = (n + 1) * (n + 1);
    let mut positions = Vec::with_capacity(verts as usize);
    let mut normals = Vec::with_capacity(verts as usize);
    let mut uvs = Vec::with_capacity(verts as usize);
    #[allow(clippy::cast_precision_loss)]
    let nf = n as f32;
    for j in 0..=n {
        for i in 0..=n {
            #[allow(clippy::cast_precision_loss)]
            let (u, v) = (i as f32 / nf, j as f32 / nf);
            let local = sim_glam::Vec3::new((u - 0.5) * w.width, (v - 0.5) * w.height, 0.0);
            let p = ue_pos_to_bevy(m.transform_point3(local), scale);
            if !p.is_finite() {
                return None;
            }
            positions.push(p.to_array());
            normals.push(normal);
            uvs.push([u, v]);
        }
    }
    let mut indices = Vec::with_capacity((n * n * 6) as usize);
    let row = n + 1;
    for j in 0..n {
        for i in 0..n {
            let a = j * row + i;
            let (b, c, d) = (a + 1, a + row, a + row + 1);
            // The UE3 → render basis flips handedness: (a, c, b) winds
            // counter-clockwise seen from the render-space normal side.
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

/// The water approximation, before any asset handle exists.
#[derive(Debug, Clone, PartialEq)]
pub struct WaterLook {
    /// Linear RGBA (alpha = opacity).
    pub base_color: [f32; 4],
    /// Base colour texture.
    pub base_color_texture: Option<TextureBinding>,
    /// Linear emissive colour.
    pub emissive: [f32; 3],
    /// Emissive texture.
    pub emissive_texture: Option<TextureBinding>,
    /// UV transform of the textures.
    pub uv: UvTransform,
    /// Perceptual roughness.
    pub roughness: f32,
}

/// The water look for a converted fluid material (`None`: placeholder).
/// The converted description keeps its textures and UV transform; its
/// colours are clamped to 0..1 (the original multiplies the macro texture
/// by 4 into an HDR range our approximation does not reproduce), the
/// opacity to [`WATER_MAX_ALPHA`] and the surface is smooth
/// ([`WATER_ROUGHNESS`]). All of it is ours (render approximation).
#[must_use]
pub fn water_look(m: Option<&RenderMaterial>) -> WaterLook {
    let Some(m) = m else {
        return WaterLook {
            base_color: WATER_FALLBACK_COLOR,
            base_color_texture: None,
            emissive: [0.0; 3],
            emissive_texture: None,
            uv: UvTransform::default(),
            roughness: WATER_ROUGHNESS,
        };
    };
    let c = |x: f32| {
        if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let [r, g, b, a] = m.base_color;
    let alpha = match m.blend {
        BlendMode::Translucent | BlendMode::Additive | BlendMode::Modulate => {
            c(a).min(WATER_MAX_ALPHA)
        }
        _ => WATER_MAX_ALPHA,
    };
    WaterLook {
        base_color: [c(r), c(g), c(b), alpha],
        base_color_texture: m.base_color_texture.clone(),
        emissive: m.emissive.map(c),
        emissive_texture: m.emissive_texture.clone(),
        uv: m.uv,
        roughness: WATER_ROUGHNESS,
    }
}

fn address(a: asamu_assets::manifest::AddressMode) -> ImageAddressMode {
    match a {
        asamu_assets::manifest::AddressMode::Wrap => ImageAddressMode::Repeat,
        asamu_assets::manifest::AddressMode::Clamp => ImageAddressMode::ClampToEdge,
        asamu_assets::manifest::AddressMode::Mirror => ImageAddressMode::MirrorRepeat,
    }
}

fn image(server: &AssetServer, t: &TextureBinding) -> Handle<Image> {
    let srgb = t.srgb;
    let descriptor = ImageSamplerDescriptor {
        address_mode_u: address(t.address[0]),
        address_mode_v: address(t.address[1]),
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    };
    server
        .load_builder()
        .with_settings(move |s: &mut ImageLoaderSettings| {
            s.is_srgb = srgb;
            s.sampler = ImageSampler::Descriptor(descriptor.clone());
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(format!("{SOURCE}://textures/{}", t.file))
}

fn standard_water_material(look: &WaterLook, server: &AssetServer) -> StandardMaterial {
    let [r, g, b, a] = look.base_color;
    let [er, eg, eb] = look.emissive;
    StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: look.base_color_texture.as_ref().map(|t| image(server, t)),
        emissive: LinearRgba::rgb(er, eg, eb),
        emissive_texture: look.emissive_texture.as_ref().map(|t| image(server, t)),
        perceptual_roughness: look.roughness,
        metallic: 0.0,
        reflectance: 0.6,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        uv_transform: Affine2::from_scale_angle_translation(
            Vec2::new(look.uv.scale[0], look.uv.scale[1]),
            0.0,
            Vec2::new(look.uv.offset[0], look.uv.offset[1]),
        ),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use asamu_assets::MaterialSource;
    use asamu_assets::scene::UeProps;
    use bevy::mesh::VertexAttributeValues;

    use super::*;

    fn surface(m: sim_glam::Mat4) -> WaterSurfaceInfo {
        WaterSurfaceInfo {
            level: 0,
            actor_slot: 3,
            actor_name: "FluidSurfaceActor_0".to_owned(),
            component_name: "FluidSurfaceComponent_1".to_owned(),
            ue_local_to_world: m,
            width: 2000.0,
            height: 1000.0,
            grid_spacing: Some(10.0),
            material: None,
            material_vectors: BTreeMap::new(),
            material_scalars: BTreeMap::new(),
            params: UeProps::default(),
            hidden: false,
        }
    }

    fn positions(mesh: &Mesh) -> Vec<[f32; 3]> {
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
            _ => panic!("positions"),
        }
    }

    #[test]
    fn water_mesh_spans_width_by_height_around_the_component() {
        let m = sim_glam::Mat4::from_translation(sim_glam::Vec3::new(500.0, -200.0, 50.0));
        let scale = WorldScale::PRESENTATION_METRES;
        let mesh = water_mesh(&surface(m), scale, 4).unwrap();
        let p = positions(&mesh);
        assert_eq!(p.len(), 25);
        let s = scale.bevy_units_per_uu;
        // UE (x, y, z) → render (y, z, −x) · s: the first vertex is the
        // (−X, −Y) corner.
        let first = p[0];
        assert!((first[0] - (-200.0 - 500.0) * s).abs() < 1e-4, "{first:?}");
        assert!((first[1] - 50.0 * s).abs() < 1e-4);
        assert!((first[2] - -(500.0 - 1000.0) * s).abs() < 1e-4);
        let last = p[24];
        assert!((last[0] - (-200.0 + 500.0) * s).abs() < 1e-4, "{last:?}");
        assert!((last[2] - -(500.0 + 1000.0) * s).abs() < 1e-4);
        // Flat at the component height; normals point up in render space.
        assert!(p.iter().all(|v| (v[1] - 50.0 * s).abs() < 1e-4));
        match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(n)) => {
                assert!(n.iter().all(|v| *v == [0.0, 1.0, 0.0]));
            }
            _ => panic!("normals"),
        }
        match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
            Some(VertexAttributeValues::Float32x2(uv)) => {
                assert_eq!(uv[0], [0.0, 0.0]);
                assert_eq!(uv[24], [1.0, 1.0]);
            }
            _ => panic!("uvs"),
        }
        assert_eq!(mesh.indices().map(Indices::len), Some(4 * 4 * 6));
    }

    #[test]
    fn rotated_surfaces_follow_their_component() {
        // A quarter turn about Z: local X (width) runs along world Y.
        let m = sim_glam::Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let scale = WorldScale::PRESENTATION_METRES;
        let p = positions(&water_mesh(&surface(m), scale, 1).unwrap());
        let s = scale.bevy_units_per_uu;
        // Corner (u=1, v=0): local (1000, −500) → world (500, 1000) →
        // render (1000 s, 0, −500 s).
        let c = p[1];
        assert!((c[0] - 1000.0 * s).abs() < 1e-3, "{c:?}");
        assert!((c[2] + 500.0 * s).abs() < 1e-3, "{c:?}");
    }

    #[test]
    fn unusable_surfaces_make_no_mesh() {
        let scale = WorldScale::PRESENTATION_METRES;
        let mut w = surface(sim_glam::Mat4::IDENTITY);
        w.width = 0.0;
        assert!(water_mesh(&w, scale, 8).is_none());
        let mut w = surface(sim_glam::Mat4::IDENTITY);
        w.height = f32::NAN;
        assert!(water_mesh(&w, scale, 8).is_none());
        let w = surface(sim_glam::Mat4::from_scale(sim_glam::Vec3::new(
            1.0, 1.0, 0.0,
        )));
        assert!(water_mesh(&w, scale, 8).is_none(), "degenerate normal");
        let mut m = sim_glam::Mat4::IDENTITY;
        m.w_axis.x = f32::INFINITY;
        assert!(water_mesh(&surface(m), scale, 8).is_none());
        // Subdivisions are clamped.
        let mesh = water_mesh(&surface(sim_glam::Mat4::IDENTITY), scale, 0).unwrap();
        assert_eq!(positions(&mesh).len(), 4);
    }

    #[test]
    fn water_look_clamps_the_converted_material() {
        let fallback = water_look(None);
        assert_eq!(fallback.base_color, WATER_FALLBACK_COLOR);
        let mut m = asamu_assets::materials::fallback_material(Some("Pkg.M_Water"));
        m.source = MaterialSource::Converted;
        m.base_color = [4.0, 4.0, 4.0, 1.0];
        m.emissive = [0.2, f32::NAN, 2.0];
        m.blend = BlendMode::Translucent;
        m.uv = UvTransform {
            channel: 0,
            scale: [0.01, 0.01],
            offset: [0.1, 0.02],
        };
        let look = water_look(Some(&m));
        assert_eq!(look.base_color, [1.0, 1.0, 1.0, WATER_MAX_ALPHA]);
        assert_eq!(look.emissive, [0.2, 0.0, 1.0]);
        assert_eq!(look.uv.scale, [0.01, 0.01]);
        assert_eq!(look.roughness, WATER_ROUGHNESS);
        m.blend = BlendMode::Opaque;
        m.base_color = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(water_look(Some(&m)).base_color[3], WATER_MAX_ALPHA);
    }

    #[test]
    fn summary_reports_counts_and_stale_conversions() {
        let mut load = WaterLoad {
            level: "TheCore".to_owned(),
            instanced: (101, 4531, 0),
            has_block: true,
            ..WaterLoad::default()
        };
        let s = summary(&load);
        assert!(
            s.contains("101 instanced components (4531 instances drawn)"),
            "{s}"
        );
        load.instanced = (2, 0, 2);
        load.has_block = false;
        let s = summary(&load);
        assert!(s.contains("2 without instance data"), "{s}");
        assert!(s.contains("no foliage block"), "{s}");
        assert!(!s.contains("left out"), "{s}");
        load.over_limit = 7;
        assert!(summary(&load).contains("7 entries over the reader's bounds left out"));
    }

    #[test]
    fn scenes_become_the_plugins_view() {
        // A hand-written scene in the importer's format (no game data): one
        // water surface, one hidden one, a SpeedTree placement without a
        // tree, one instanced component of two instances.
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        let json = format!(
            r#"{{"format": "asamu-scene", "version": 1, "package": "M",
              "actors": [{{"slot": 1, "name": "InstancedFoliageActor_0", "class": "Engine.InstancedFoliageActor",
                 "kind": "other", "is_static": true,
                 "components": [{{"name": "ISMC_0", "class": "Engine.InstancedStaticMeshComponent",
                    "kind": "static_mesh", "local_to_world": {ident}, "static_mesh": "Pkg.Rock"}}]}}],
              "foliage": {{"version": 1,
                "instanced_meshes": [{{"slot": 1, "component": "ISMC_0",
                   "instances": [{{"local_to_world": {ident}}}, {{"local_to_world": {ident}}}]}}],
                "fluid_surfaces": [
                  {{"slot": 2, "actor": "FluidSurfaceActor_0", "component": "F", "local_to_world": {ident},
                    "hidden": false, "params": {{"FluidWidth": 400.0, "FluidHeight": 200.0}}}},
                  {{"slot": 3, "actor": "FluidSurfaceActor_1", "component": "F", "local_to_world": {ident},
                    "hidden": true, "params": {{"FluidWidth": 400.0, "FluidHeight": 200.0}}}}],
                "speedtrees": [{{"slot": 4, "actor": "SpeedTreeActor_0", "component": "S",
                    "local_to_world": {ident}, "speedtree": null}}]}}}}"#
        );
        let scene = LevelScene::from_json(std::path::Path::new("x"), json.as_bytes()).unwrap();
        let load = water_load("M", scene, &asamu_assets::Manifests::default());
        assert!(load.has_block && load.block_error.is_none());
        assert_eq!(load.instanced, (1, 2, 0));
        assert_eq!(load.over_limit, 0);
        assert_eq!(load.water.len(), 2);
        assert_eq!(load.materials, vec![None, None], "no converted materials");
        assert_eq!(load.water.iter().filter(|w| w.hidden).count(), 1);
        assert_eq!(load.speedtrees.len(), 1);
        let s = summary(&load);
        assert!(s.contains("2 water surfaces"), "{s}");
        assert!(
            s.contains("1 instanced components (2 instances drawn)"),
            "{s}"
        );
        assert!(
            s.contains("1 SpeedTree placements (0 with a tree; none drawn)"),
            "{s}"
        );
        // Every surface, shown or hidden, has a usable mesh of the right
        // size: (n + 1)² vertices, 6 n² indices.
        for w in &load.water {
            let mesh = water_mesh(w, WorldScale::PRESENTATION_METRES, WATER_SUBDIVISIONS).unwrap();
            let n = WATER_SUBDIVISIONS as usize;
            assert_eq!(positions(&mesh).len(), (n + 1) * (n + 1));
            assert_eq!(mesh.indices().map(Indices::len), Some(6 * n * n));
        }
        // The largest request is clamped (no unbounded mesh).
        let big = water_mesh(&load.water[0], WorldScale::PRESENTATION_METRES, u32::MAX).unwrap();
        assert_eq!(positions(&big).len(), 257 * 257);
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// user-local `asamu-import levels` output with AG-ParadiseCave and
    /// AG-IceCave): ParadiseCave's two water surfaces make meshes, and
    /// IceCave's streamed TheCore (merged on request) brings 101 instanced
    /// components with 4,531 instances.
    #[test]
    fn converted_water_and_foliage_load() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let dir = ConvertedDir::new(root);
        let Ok(paradise) = load(&dir, "AG-ParadiseCave", false) else {
            eprintln!("skipped: AG-ParadiseCave is not converted");
            return;
        };
        if !paradise.has_block {
            eprintln!("skipped: converted before the foliage block existed");
            return;
        }
        assert_eq!(paradise.water.len(), 2);
        for w in &paradise.water {
            assert!(water_mesh(w, WorldScale::PRESENTATION_METRES, 8).is_some());
        }
        if let Ok(ice) = load(&dir, "AG-IceCave", true) {
            assert_eq!(ice.instanced.0, 101, "{}", summary(&ice));
            assert_eq!(ice.instanced.1, 4531);
            assert_eq!(ice.instanced.2, 0);
        }
    }
}
