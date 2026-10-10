//! Original levels from user-local converted data (`--converted DIR --level
//! NAME`).
//!
//! Loading never blocks the main thread: the scene JSON and manifests are
//! parsed into an [`asamu_assets::LevelPlan`] on Bevy's async compute pool;
//! once the plan is ready the level's entities are spawned at once while the
//! glTF primitives and DDS textures stream in through Bevy's asset server
//! (from a dedicated `converted://` asset source rooted at the converted
//! directory). Each distinct mesh primitive and material becomes **one**
//! handle shared by every draw that uses it, so repeated props instance and
//! batch.
//!
//! Approximations (render side only; see `asamu_assets::lighting` and
//! `asamu_assets::materials`): lights use our UE3 → physical mapping, the
//! original's baked lightmaps are not rendered (a constant ambient term
//! stands in), fog is a placeholder distance fog enabled only when the level
//! has a height-fog actor, and materials without a converted description use
//! untextured neutral colours.

use std::collections::HashMap;
use std::path::PathBuf;

use asamu_assets::manifest::AddressMode;
use asamu_assets::materials::UvTransform;
use asamu_assets::{
    BlendMode, ConvertedDir, LevelPlan, PlanOptions, RenderLightKind, RenderMaterial,
    TextureBinding,
};
use asamu_core::coords::ue_dir_to_bevy;
use bevy::asset::io::AssetSourceBuilder;
use bevy::asset::io::file::FileAssetReader;
use bevy::asset::{AssetId, AssetPath, RenderAssetUsages, UntypedAssetId};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::gltf::convert_coordinates::GltfConvertCoordinates;
use bevy::gltf::{GltfAssetLabel, GltfLoaderSettings};
use bevy::image::{
    ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor,
};
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster};
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology, UvChannel};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::render_resource::Face;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::{bevy_vec, to_render};

/// Name of the asset source that serves the converted directory.
pub const SOURCE: &str = "converted";

/// Registers the `converted://` asset source. Must run before
/// `DefaultPlugins` (the asset plugin builds its sources when added).
pub fn register_source(app: &mut App, root: PathBuf) {
    app.register_asset_source(
        SOURCE,
        AssetSourceBuilder::new(move || Box::new(FileAssetReader::new(root.clone()))),
    );
}

/// Renderer settings for converted levels (command line).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderSettings {
    /// Directional light shadows.
    pub shadows: bool,
    /// How many point/spot lights (the most powerful) get shadow maps.
    pub light_shadows: usize,
    /// Placeholder distance fog when the level has a height-fog actor.
    pub fog: bool,
    /// Use converted normal maps (off by default: the tangent-space
    /// convention of UE3 normal maps under the importer's axis mirror is not
    /// verified yet).
    pub normal_maps: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            shadows: true,
            light_shadows: 4,
            fog: true,
            normal_maps: false,
        }
    }
}

/// Where the camera should start once the level is known (UE3 eye
/// position, yaw, pitch). Consumed by the camera controller.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Default)]
pub struct LevelStart(pub Option<(asamu_core::glam::Vec3, f32, f32)>);

/// Summary of the spawned level for the HUD.
#[derive(Debug, Clone, Default)]
pub struct LevelSummary {
    /// Level title (`WorldInfo.Title`) or package name.
    pub title: String,
    /// One line of counts.
    pub counts: String,
    /// Asset handles to track (mesh primitives and images).
    pub tracked: Vec<UntypedAssetId>,
}

/// Loading state of the converted level.
pub enum LevelPhase {
    /// The plan is being built on the async pool.
    Planning(Task<Result<LevelPlan, String>>),
    /// Entities spawned; assets streaming.
    Spawned(LevelSummary),
    /// Loading failed.
    Failed(String),
}

/// The converted level being shown.
#[derive(Resource)]
pub struct ConvertedLevel {
    /// Converted directory.
    pub dir: ConvertedDir,
    /// Level name.
    pub level: String,
    /// Plan options.
    pub options: PlanOptions,
    /// Renderer settings.
    pub settings: RenderSettings,
    /// Eye height above a player start (UU), for the initial camera.
    pub eye_height: f32,
    /// Show every streamed sub-level regardless of the game
    /// (`--all-sublevels`); the plan always contains them.
    pub force_all_sublevels: bool,
    /// Phase.
    pub phase: Option<LevelPhase>,
    /// Asset progress: (loaded, failed, total).
    pub progress: (usize, usize, usize),
}

impl ConvertedLevel {
    /// True once every tracked asset finished loading (or failed).
    #[must_use]
    pub fn assets_settled(&self) -> bool {
        matches!(self.phase, Some(LevelPhase::Spawned(_)))
            && self.progress.0 + self.progress.1 >= self.progress.2
    }

    /// A short status line.
    #[must_use]
    pub fn status(&self) -> String {
        match &self.phase {
            None => "starting".to_owned(),
            Some(LevelPhase::Planning(_)) => format!("parsing {} ...", self.level),
            Some(LevelPhase::Failed(e)) => format!("FAILED: {e}"),
            Some(LevelPhase::Spawned(s)) => {
                let (loaded, failed, total) = self.progress;
                let assets = if loaded + failed >= total {
                    format!("assets {loaded}/{total} loaded")
                } else {
                    format!("streaming assets {}/{total}", loaded + failed)
                };
                let failed = if failed > 0 {
                    format!(" ({failed} failed, see log)")
                } else {
                    String::new()
                };
                format!("{} | {} | {assets}{failed}", s.title, s.counts)
            }
        }
    }
}

/// Marker on every spawned level mesh entity.
#[derive(Component, Debug, Clone, Copy)]
pub struct LevelEntity {
    /// Owning UE3 actor slot.
    pub actor_slot: usize,
    /// Plan level of the actor (0 = the persistent level, then the merged
    /// sub-levels; names in [`RenderLevels`]).
    pub level: usize,
}

/// The levels of the spawned plan, by plan level index: package names and
/// whether each is always loaded (the others are streamed by Kismet and
/// shown only while the game has them streamed in, unless `force_all`).
#[derive(Resource, Debug, Clone, Default)]
pub struct RenderLevels {
    /// Package name per plan level.
    pub names: Vec<String>,
    /// Always loaded (the persistent level and `LevelStreamingAlwaysLoaded`).
    pub always_loaded: Vec<bool>,
    /// Show every sub-level regardless of the game (`--all-sublevels`).
    pub force_all: bool,
}

impl RenderLevels {
    /// The levels of `plan`.
    #[must_use]
    pub fn of_plan(plan: &LevelPlan, force_all: bool) -> Self {
        let mut names = vec![plan.scene.package.clone()];
        let mut always_loaded = vec![true];
        for sub in &plan.scene.merged_levels {
            always_loaded.push(
                plan.scene
                    .streaming_levels
                    .iter()
                    .any(|s| s.package.eq_ignore_ascii_case(sub) && s.always_loaded()),
            );
            names.push(sub.clone());
        }
        Self {
            names,
            always_loaded,
            force_all,
        }
    }
}

/// Marker on the level BSP meshes.
#[derive(Component, Debug, Clone, Copy)]
pub struct LevelBsp;

/// Every spawned level light: its UE3 actor (plan level and actor slot, so
/// Kismet's moves, toggles, streaming and Matinee property tracks find it)
/// and the placed values the render values were mapped from.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct LevelLight {
    /// Plan level of the owning actor (see [`LevelEntity::level`]).
    pub level: usize,
    /// Owning actor slot.
    pub actor_slot: usize,
    /// Placed `Brightness`.
    pub brightness: f32,
    /// Placed `Radius` (UU), if any.
    pub radius: Option<f32>,
    /// Render intensity mapped from the placed values (lumens; lux for a
    /// directional light).
    pub intensity: f32,
    /// Render range (point and spot lights; 0 otherwise).
    pub range: f32,
}

/// Which texture of a material an image fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureSlot {
    BaseColor,
    Normal,
    Emissive,
}

/// A material using an image: its id, the slot, and its UE3 path (for the
/// fallback colour).
type TextureUser = (AssetId<StandardMaterial>, TextureSlot, Option<String>);

/// The materials that use each image, so a material whose image fails to
/// load (a missing or damaged DDS file) can drop that texture instead of
/// waiting for it forever: Bevy does not draw a mesh until every texture of
/// its material is available, so one bad file would otherwise make every
/// wall that uses it vanish.
#[derive(Resource, Debug, Default)]
pub struct TextureUsers {
    pending: HashMap<AssetId<Image>, Vec<TextureUser>>,
    /// Material textures dropped after a failed image load.
    pub repaired: usize,
}

impl TextureUsers {
    fn register(
        &mut self,
        id: AssetId<StandardMaterial>,
        m: &StandardMaterial,
        path: Option<&str>,
    ) {
        let slots = [
            (m.base_color_texture.as_ref(), TextureSlot::BaseColor),
            (m.normal_map_texture.as_ref(), TextureSlot::Normal),
            (m.emissive_texture.as_ref(), TextureSlot::Emissive),
        ];
        for (image, slot) in slots {
            if let Some(image) = image {
                self.pending.entry(image.id()).or_default().push((
                    id,
                    slot,
                    path.map(str::to_owned),
                ));
            }
        }
    }
}

/// The base colour of a material whose base colour texture is unavailable:
/// its multiplier times the material's neutral palette colour (the same rule
/// `asamu_assets` applies to a texture that was never converted).
fn untextured_base_color(multiplier: Color, path: Option<&str>) -> Color {
    let [pr, pg, pb, _] = asamu_assets::materials::fallback_material(path).base_color;
    let c = multiplier.to_linear();
    Color::linear_rgba(c.red * pr, c.green * pg, c.blue * pb, c.alpha)
}

/// Drops the textures whose image failed to load from the materials that
/// use them: the base colour falls back to the material's neutral palette
/// colour times its multiplier (as for a texture that was never converted),
/// a failed emissive texture turns the emissive term off, a failed normal
/// map is simply left out.
fn repair_failed_textures(
    server: Res<AssetServer>,
    mut users: ResMut<TextureUsers>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if users.pending.is_empty() {
        return;
    }
    let mut failed = Vec::new();
    users
        .pending
        .retain(|id, _| match server.get_load_state(*id) {
            Some(bevy::asset::LoadState::Failed(_)) => {
                failed.push(*id);
                true
            }
            Some(bevy::asset::LoadState::Loaded) => false,
            _ => true,
        });
    for image in failed {
        let Some(list) = users.pending.remove(&image) else {
            continue;
        };
        for (material, slot, path) in list {
            let Some(mut m) = materials.get_mut(material) else {
                continue;
            };
            match slot {
                TextureSlot::BaseColor => {
                    m.base_color_texture = None;
                    m.base_color = untextured_base_color(m.base_color, path.as_deref());
                }
                TextureSlot::Normal => m.normal_map_texture = None,
                TextureSlot::Emissive => {
                    m.emissive_texture = None;
                    m.emissive = LinearRgba::BLACK;
                }
            }
            users.repaired += 1;
        }
        warn!("an image failed to load: its materials are drawn without it ({image:?})");
    }
}

/// Loads and spawns a converted level.
pub struct ConvertedLevelPlugin;

impl Plugin for ConvertedLevelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LevelStart>()
            .init_resource::<TextureUsers>()
            .add_systems(Startup, start_planning)
            .add_systems(
                Update,
                (
                    poll_planning,
                    track_assets,
                    repair_failed_textures,
                    log_actor_count,
                )
                    .chain(),
            );
    }
}

fn start_planning(mut level: ResMut<ConvertedLevel>) {
    let dir = level.dir.clone();
    let name = level.level.clone();
    let options = level.options;
    let task = AsyncComputeTaskPool::get()
        .spawn(async move { LevelPlan::load(&dir, &name, &options).map_err(|e| e.to_string()) });
    level.phase = Some(LevelPhase::Planning(task));
}

#[allow(clippy::too_many_arguments)]
fn poll_planning(
    mut commands: Commands,
    mut level: ResMut<ConvertedLevel>,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_assets: ResMut<Assets<Mesh>>,
    mut start: ResMut<LevelStart>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut users: ResMut<TextureUsers>,
    camera: Query<Entity, With<Camera3d>>,
    mut projection: Query<&mut Projection, With<Camera3d>>,
) {
    let Some(LevelPhase::Planning(task)) = level.phase.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    let plan = match result {
        Ok(plan) => plan,
        Err(e) => {
            error!("could not load the converted level: {e}");
            level.phase = Some(LevelPhase::Failed(e));
            return;
        }
    };
    log_plan(&plan);
    let settings = level.settings;
    let mapping = level.options.lights;
    let summary = spawn_plan(
        &mut commands,
        &asset_server,
        &mut materials,
        &mut mesh_assets,
        &mut users,
        &plan,
        &settings,
        &mapping,
    );
    commands.insert_resource(RenderLevels::of_plan(&plan, level.force_all_sublevels));
    ambient.brightness = plan.ambient;
    ambient.color = Color::WHITE;
    start.0 = plan.start().map(|(loc, yaw, pitch)| {
        (
            loc + asamu_core::glam::Vec3::Z * level.eye_height,
            yaw,
            pitch,
        )
    });
    // Far plane (Bevy's projection is infinite reverse-Z: `far` only bounds
    // culling and shadow cascades) from the level extent; fog from the
    // origins' radius.
    let radius = plan.render_radius();
    // Kept finite (a damaged plan's extent can be huge).
    let far = (plan.extent * 2.0 + 10.0)
        .max(radius * 4.0)
        .clamp(1000.0, 1.0e9);
    for mut p in &mut projection {
        if let Projection::Perspective(persp) = p.as_mut() {
            persp.far = far;
            persp.near = 0.05;
        }
    }
    let has_height_fog = plan
        .scene
        .stats
        .atmosphere_actors
        .keys()
        .any(|k| k.to_ascii_lowercase().contains("heightfog"));
    if settings.fog && has_height_fog {
        for cam in &camera {
            // PLACEHOLDER parameters: the fog component values are not in the
            // scene export yet.
            commands.entity(cam).insert(DistanceFog {
                color: Color::srgb(0.55, 0.6, 0.68),
                falloff: FogFalloff::Linear {
                    start: radius * 0.5,
                    end: radius * 3.0,
                },
                ..default()
            });
        }
    }
    level.phase = Some(LevelPhase::Spawned(summary));
}

fn log_plan(plan: &LevelPlan) {
    let s = &plan.stats;
    info!(
        "level {}: {} instances ({} drawn), {} draws, {} shared primitives, {} materials \
         ({} converted, {} fallback), {} textures ({} missing), {} lights mapped ({} skipped), \
         {} mirrored draws, max shear {:.1e}",
        plan.scene.package,
        s.instances,
        s.instances_drawn,
        s.draws,
        s.primitives,
        s.materials,
        s.materials_converted,
        s.materials_fallback,
        s.textures,
        s.textures_missing,
        s.lights_mapped,
        s.lights_skipped,
        s.mirrored_draws,
        s.max_shear,
    );
    if !s.missing_meshes.is_empty() {
        warn!(
            "{} mesh paths are not converted (run `asamu-import meshes --package {}`), e.g. {:?}",
            s.missing_meshes.len(),
            plan.scene.package,
            s.missing_meshes.keys().take(5).collect::<Vec<_>>()
        );
    }
    if !plan.scene.merged_levels.is_empty() {
        info!(
            "merged streaming sub-levels: {:?}",
            plan.scene.merged_levels
        );
    }
    if !s.missing_sublevels.is_empty() {
        warn!(
            "streaming sub-levels not converted (run `asamu-import levels --map <name>`): {:?}",
            s.missing_sublevels
        );
    }
    if s.materials_converted == 0 && s.materials > 0 {
        info!(
            "no converted material descriptions (materials/materials.json; run `asamu-import \
             materials`): untextured fallback colours are used"
        );
    }
}

/// Render layers of the level's lights: the world and the first-person
/// overlay, so the hands are lit by the lights around the player (in the
/// original the foreground mesh is lit by the scene too) and not by a light
/// of their own.
fn scene_light_layers() -> RenderLayers {
    RenderLayers::from_layers(&[0, crate::FIRST_PERSON_LAYER])
}

fn gltf_settings(s: &mut GltfLoaderSettings) {
    s.load_materials = RenderAssetUsages::empty();
    s.load_cameras = false;
    s.load_lights = false;
    s.load_animations = false;
    s.convert_coordinates = Some(GltfConvertCoordinates {
        rotate_scene_entity: false,
        rotate_meshes: false,
    });
}

fn address(a: AddressMode) -> ImageAddressMode {
    match a {
        AddressMode::Wrap => ImageAddressMode::Repeat,
        AddressMode::Clamp => ImageAddressMode::ClampToEdge,
        AddressMode::Mirror => ImageAddressMode::MirrorRepeat,
    }
}

/// Loads (once) the images of a plan.
struct ImageCache<'a> {
    server: &'a AssetServer,
    handles: HashMap<String, Handle<Image>>,
}

impl ImageCache<'_> {
    fn get(&mut self, t: &TextureBinding) -> Handle<Image> {
        if let Some(h) = self.handles.get(&t.file) {
            return h.clone();
        }
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
        let path = format!("{SOURCE}://textures/{}", t.file);
        let handle: Handle<Image> = self
            .server
            .load_builder()
            .with_settings(move |s: &mut ImageLoaderSettings| {
                s.is_srgb = srgb;
                s.sampler = ImageSampler::Descriptor(descriptor.clone());
                s.asset_usage = RenderAssetUsages::RENDER_WORLD;
            })
            .load(path);
        self.handles.insert(t.file.clone(), handle.clone());
        handle
    }
}

fn uv(t: &UvTransform) -> (UvChannel, Affine2) {
    let channel = if t.channel == 1 {
        UvChannel::Uv1
    } else {
        UvChannel::Uv0
    };
    let affine = Affine2::from_scale_angle_translation(
        Vec2::new(t.scale[0], t.scale[1]),
        0.0,
        Vec2::new(t.offset[0], t.offset[1]),
    );
    (channel, affine)
}

fn standard_material(
    m: &RenderMaterial,
    mirrored: bool,
    settings: &RenderSettings,
    images: &mut ImageCache<'_>,
) -> StandardMaterial {
    let (channel, uv_transform) = uv(&m.uv);
    let [r, g, b, a] = m.base_color;
    let [er, eg, eb] = m.emissive;
    // Mirrored instances wind the other way: cull the faces the rasterizer
    // calls "front" (they are the mesh's back faces).
    let (double_sided, cull_mode) = if m.two_sided {
        (true, None)
    } else if mirrored {
        (false, Some(Face::Front))
    } else {
        (false, Some(Face::Back))
    };
    StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_channel: channel.clone(),
        base_color_texture: m.base_color_texture.as_ref().map(|t| images.get(t)),
        normal_map_channel: channel.clone(),
        normal_map_texture: if settings.normal_maps {
            m.normal_texture.as_ref().map(|t| images.get(t))
        } else {
            None
        },
        emissive: LinearRgba::rgb(er, eg, eb),
        emissive_channel: channel,
        emissive_texture: m.emissive_texture.as_ref().map(|t| images.get(t)),
        perceptual_roughness: m.roughness,
        metallic: m.metallic,
        alpha_mode: match m.blend {
            BlendMode::Opaque => AlphaMode::Opaque,
            BlendMode::Masked { cutoff } => AlphaMode::Mask(cutoff),
            BlendMode::Translucent => AlphaMode::Blend,
            BlendMode::Additive => AlphaMode::Add,
            BlendMode::Modulate => AlphaMode::Multiply,
        },
        double_sided,
        cull_mode,
        unlit: m.unlit,
        uv_transform,
        ..default()
    }
}

/// A Bevy material for a converted render material outside the level plan
/// (skinned meshes): the mapping level meshes use, registered for the
/// failed-texture repair.
pub(crate) fn add_render_material(
    m: &RenderMaterial,
    settings: &RenderSettings,
    server: &AssetServer,
    materials: &mut Assets<StandardMaterial>,
    users: &mut TextureUsers,
) -> Handle<StandardMaterial> {
    let mut images = ImageCache {
        server,
        handles: HashMap::new(),
    };
    let sm = standard_material(m, false, settings, &mut images);
    let handle = materials.add(sm.clone());
    users.register(handle.id(), &sm, m.path.as_deref());
    handle
}

/// The scene light of each render light of `plan` (same order: the plan maps
/// the scene's lights in order and drops the ones without effect and the
/// ambient ones, as here). Empty when the mapping disagrees with the plan.
fn plan_light_sources<'a>(
    plan: &'a LevelPlan,
    mapping: &asamu_assets::LightMapping,
) -> Vec<&'a asamu_assets::scene::SceneLight> {
    let sources: Vec<&asamu_assets::scene::SceneLight> = plan
        .scene
        .lights
        .iter()
        .filter(|l| {
            matches!(
                asamu_assets::lighting::map_light(l, mapping, plan.scale),
                Some(r) if !matches!(r.kind, RenderLightKind::Ambient { .. })
            )
        })
        .collect();
    if sources.len() == plan.lights.len() {
        sources
    } else {
        Vec::new()
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_plan(
    commands: &mut Commands,
    server: &AssetServer,
    materials: &mut Assets<StandardMaterial>,
    mesh_assets: &mut Assets<Mesh>,
    users: &mut TextureUsers,
    plan: &LevelPlan,
    settings: &RenderSettings,
    mapping: &asamu_assets::LightMapping,
) -> LevelSummary {
    let mut tracked: Vec<UntypedAssetId> = Vec::new();
    // One mesh handle per distinct primitive.
    let meshes: Vec<Handle<Mesh>> = plan
        .primitives
        .iter()
        .map(|p| {
            let path = AssetPath::from(format!("{SOURCE}://meshes/{}", p.gltf));
            let label = GltfAssetLabel::Primitive {
                mesh: 0,
                primitive: p.primitive,
            };
            let h: Handle<Mesh> = server
                .load_builder()
                .with_settings(gltf_settings)
                .load(label.from_asset(path));
            tracked.push(h.id().untyped());
            h
        })
        .collect();
    // One material handle per (material, mirrored).
    let mut images = ImageCache {
        server,
        handles: HashMap::new(),
    };
    let mut material_handles: HashMap<(usize, bool), Handle<StandardMaterial>> = HashMap::new();
    let mut spawned = 0usize;
    for d in &plan.draws {
        let (Some(mesh), Some(m)) = (meshes.get(d.primitive), plan.materials.get(d.material))
        else {
            continue;
        };
        let mirrored = d.transform.mirrored;
        let material = material_handles
            .entry((d.material, mirrored))
            .or_insert_with(|| {
                let sm = standard_material(m, mirrored, settings, &mut images);
                let handle = materials.add(sm.clone());
                users.register(handle.id(), &sm, m.path.as_deref());
                handle
            })
            .clone();
        let t = d.transform;
        let transform = Transform {
            translation: bevy_vec(t.translation),
            rotation: Quat::from_array(t.rotation.to_array()),
            scale: bevy_vec(t.scale),
        };
        let mut e = commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            transform,
            LevelEntity {
                actor_slot: d.actor_slot,
                level: plan.scene.meshes.get(d.instance).map_or(0, |m| m.level),
            },
        ));
        let translucent = !matches!(m.blend, BlendMode::Opaque | BlendMode::Masked { .. });
        if !d.cast_shadow || translucent || m.unlit {
            e.insert(NotShadowCaster);
        }
        if m.unlit
            && m.path
                .as_deref()
                .is_some_and(|p| p.to_ascii_lowercase().contains("sky"))
        {
            // Sky domes are huge and surround the camera.
            e.insert(NoFrustumCulling);
        }
        spawned += 1;
    }
    // Level BSP (walls, floors and ceilings built from CSG brushes): one
    // render-space mesh per material, identity transform.
    for b in &plan.bsp {
        let Some(m) = plan.materials.get(b.material) else {
            continue;
        };
        let material = material_handles
            .entry((b.material, false))
            .or_insert_with(|| {
                let sm = standard_material(m, false, settings, &mut images);
                let handle = materials.add(sm.clone());
                users.register(handle.id(), &sm, m.path.as_deref());
                handle
            })
            .clone();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, b.mesh.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, b.mesh.normals.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, b.mesh.uvs.clone())
        .with_inserted_indices(Indices::U32(b.mesh.indices.clone()));
        if settings.normal_maps
            && let Err(e) = mesh.generate_tangents()
        {
            warn!("BSP tangents for {:?}: {e}", m.path);
        }
        commands.spawn((
            Mesh3d(mesh_assets.add(mesh)),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            LevelBsp,
        ));
        spawned += 1;
    }
    tracked.extend(images.handles.values().map(|h| h.id().untyped()));

    // Lights. Shadow maps for the directional light(s) and the most
    // powerful point/spot lights only.
    let mut shadow_rank: Vec<(usize, f32)> = plan
        .lights
        .iter()
        .enumerate()
        .filter(|(_, l)| l.cast_shadows)
        .filter_map(|(i, l)| match l.kind {
            RenderLightKind::Point { lumens, .. } | RenderLightKind::Spot { lumens, .. } => {
                Some((i, lumens))
            }
            _ => None,
        })
        .collect();
    shadow_rank.sort_by(|a, b| b.1.total_cmp(&a.1));
    let shadowed: Vec<usize> = shadow_rank
        .iter()
        .take(settings.light_shadows)
        .map(|(i, _)| *i)
        .collect();
    let radius = plan.render_radius();
    let mut light_count = 0usize;
    let sources = plan_light_sources(plan, mapping);
    for (i, l) in plan.lights.iter().enumerate() {
        let pos = to_render(l.location_ue);
        let dir = bevy_vec(ue_dir_to_bevy(l.direction_ue));
        let color = Color::srgb_u8(l.color_srgb[0], l.color_srgb[1], l.color_srgb[2]);
        let shadows = shadowed.contains(&i);
        let (intensity, range) = match l.kind {
            RenderLightKind::Point { lumens, range }
            | RenderLightKind::Spot { lumens, range, .. } => (lumens, range),
            RenderLightKind::Directional { lux } => (lux, 0.0),
            RenderLightKind::Ambient { brightness } => (brightness, 0.0),
        };
        // Without its scene light (a mapping mismatch) the light keeps a
        // slot nothing names (`usize::MAX`), so Kismet never touches it.
        let slot = match sources.get(i) {
            Some(src) => LevelLight {
                level: src.level,
                actor_slot: src.actor_slot,
                brightness: src.brightness,
                radius: src.radius,
                intensity,
                range,
            },
            None => LevelLight {
                level: 0,
                actor_slot: usize::MAX,
                brightness: 0.0,
                radius: None,
                intensity,
                range,
            },
        };
        match l.kind {
            RenderLightKind::Point { lumens, range } => {
                commands.spawn((
                    PointLight {
                        color,
                        intensity: lumens,
                        range,
                        shadow_maps_enabled: shadows,
                        ..default()
                    },
                    Transform::from_translation(pos),
                    slot,
                    scene_light_layers(),
                ));
            }
            RenderLightKind::Spot {
                lumens,
                range,
                inner,
                outer,
            } => {
                commands.spawn((
                    SpotLight {
                        color,
                        intensity: lumens,
                        range,
                        inner_angle: inner,
                        outer_angle: outer,
                        shadow_maps_enabled: shadows,
                        ..default()
                    },
                    Transform::from_translation(pos).looking_to(dir, Vec3::Y),
                    slot,
                    scene_light_layers(),
                ));
            }
            RenderLightKind::Directional { lux } => {
                commands.spawn((
                    DirectionalLight {
                        color,
                        illuminance: lux,
                        shadow_maps_enabled: settings.shadows && l.cast_shadows,
                        ..default()
                    },
                    Transform::default().looking_to(dir, Vec3::Y),
                    scene_light_layers(),
                    // Shadows near the player only (cost grows with the
                    // distance and cascade count; a render setting).
                    CascadeShadowConfigBuilder {
                        num_cascades: 3,
                        maximum_distance: radius.clamp(30.0, 150.0),
                        first_cascade_far_bound: 6.0,
                        ..default()
                    }
                    .build(),
                    slot,
                ));
            }
            RenderLightKind::Ambient { .. } => continue,
        }
        light_count += 1;
    }

    let title = plan
        .scene
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .map_or_else(
            || plan.scene.package.clone(),
            |t| format!("{t} ({})", plan.scene.package),
        );
    let s = &plan.stats;
    LevelSummary {
        title,
        counts: format!(
            "{spawned} draws ({} mesh primitives, {} BSP triangles), {} materials ({} converted, \
             {} fallback), {} textures, {light_count} lights",
            s.primitives,
            s.bsp_triangles,
            s.materials,
            s.materials_converted,
            s.materials_fallback,
            s.textures
        ),
        tracked,
    }
}

fn track_assets(mut level: ResMut<ConvertedLevel>, server: Res<AssetServer>) {
    let Some(LevelPhase::Spawned(summary)) = &level.phase else {
        return;
    };
    let total = summary.tracked.len();
    if level.progress.2 == total && level.progress.0 + level.progress.1 >= total {
        return;
    }
    let mut loaded = 0;
    let mut failed = 0;
    for id in &summary.tracked {
        match server.get_load_state(*id) {
            Some(bevy::asset::LoadState::Loaded) => loaded += 1,
            Some(bevy::asset::LoadState::Failed(_)) => failed += 1,
            _ => {}
        }
    }
    let before = level.progress;
    level.progress = (loaded, failed, total);
    let was_settled = before.2 == total && before.0 + before.1 >= total;
    if !was_settled && loaded + failed >= total {
        info!("converted level assets settled: {loaded} loaded, {failed} failed of {total}");
    }
}

/// Logs, once, how many UE3 actors the spawned draws belong to.
fn log_actor_count(entities: Query<&LevelEntity>, mut done: Local<bool>) {
    if *done || entities.is_empty() {
        return;
    }
    let mut slots: Vec<usize> = entities.iter().map(|e| e.actor_slot).collect();
    let draws = slots.len();
    slots.sort_unstable();
    slots.dedup();
    info!(
        "spawned {draws} level mesh entities for {} actors",
        slots.len()
    );
    *done = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_users_record_every_slot() {
        let mut images: Assets<Image> = Assets::default();
        let (a, b) = (images.add(Image::default()), images.add(Image::default()));
        let mut materials: Assets<StandardMaterial> = Assets::default();
        let sm = StandardMaterial {
            base_color_texture: Some(a.clone()),
            normal_map_texture: Some(b.clone()),
            emissive_texture: Some(a.clone()),
            ..default()
        };
        let id = materials.add(sm.clone()).id();
        let mut users = TextureUsers::default();
        users.register(id, &sm, Some("Pkg.M_Wall"));
        users.register(id, &StandardMaterial::default(), None);
        let slots: Vec<TextureSlot> = users.pending[&a.id()].iter().map(|u| u.1).collect();
        assert_eq!(slots, vec![TextureSlot::BaseColor, TextureSlot::Emissive]);
        assert_eq!(users.pending[&b.id()][0].1, TextureSlot::Normal);
        assert_eq!(users.pending[&b.id()][0].2.as_deref(), Some("Pkg.M_Wall"));
        assert_eq!(users.pending.len(), 2);
    }

    #[test]
    fn untextured_base_color_tints_the_palette_colour() {
        let palette = asamu_assets::materials::fallback_material(Some("Pkg.M_Wall")).base_color;
        let c = untextured_base_color(Color::linear_rgba(0.5, 1.0, 0.25, 0.75), Some("Pkg.M_Wall"))
            .to_linear();
        let want = [0.5 * palette[0], palette[1], 0.25 * palette[2], 0.75];
        for (got, want) in [c.red, c.green, c.blue, c.alpha].into_iter().zip(want) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
    }
}
