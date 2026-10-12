//! Gameplay visual effects and level decals (VFX_DECALS.md):
//!
//! - **grapple beam** (`GrappleGun.MyBeam`): the two enabled beam emitters
//!   (4 and 2 UU wide, two strands each) as camera-facing noisy ribbons from
//!   the hand mesh's socket to the anchor while attached, drawn with the hand
//!   ([`emitter`]), with the beam material's three textures and the gun's
//!   colour ([`beam`], [`beam_material`]);
//! - the grapple's **flash at the hand** and **impact at the anchor**
//!   (`grappleBeginEffect`, `ImpactParticleSystem`): the two converted
//!   particle systems, run by the particle simulator and drawn as glowing
//!   sprites ([`bursts`]);
//! - **hit decal** at the anchor (`DecalManager.SpawnDecal` from
//!   `ProcessInstantHit`; none on `ASAMUDoesNotAcceptGrappleDecal` targets)
//!   and the **hard-landing decal**, sharing the manager's pool of 5 and its
//!   30 s lifetime ([`hit_decals`]);
//! - **speed lines** (`ASAMUVelocityCone`): the cone mesh at the view point
//!   plus `V·dt`, along `V`, its opacity from the velocity parameter; Kismet's
//!   `SeqAct_SetVelocityConeMaterial` swaps its material ([`cone`]);
//! - **hand lights** (`GrappleGunLightManager`): grapples left and the
//!   power-jump charge, drawn as HUD lamps because they live on the hand's
//!   plate material ([`lights`]);
//! - **crosshair** state machine (`ASAMUHUD.ChangeCrosshair`, Kismet
//!   `ToggleCrosshair`) drawn as a ring around the HUD crosshair
//!   ([`crosshair`]);
//! - **rocket-boots lens effect** (`RocketBootsCameraLensEffect`): a short
//!   screen overlay when a boost starts;
//! - **level decals** from `asamu-import decals`, as projected meshes
//!   ([`level_decals`]); a material that keeps its shape in an opacity
//!   texture only is drawn through the importer's baked mask
//!   ([`decal_shape`]).
//!
//! Everything here is presentation: it reads the simulation's state and the
//! tick reports (`crate::ui::GameTick`), never writes them (the debug
//! [`selftest`], off unless asked for by an environment variable, is the one
//! exception). Render-only
//! numbers that are ours (not recovered) say so where they are defined.
//! `DecalDynamicLight` exists in the original but is never spawned
//! ([`lights::DecalLight`]), so no light is added at the anchor.

mod beam;
mod beam_material;
mod bursts;
mod cone;
mod crosshair;
mod emitter;
mod hit_decals;
mod level_decals;
mod lights;
mod materials;
mod selftest;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use asamu_assets::manifest::AddressMode;
use asamu_assets::{BlendMode, Manifests, RenderMaterial, TextureBinding};
use asamu_core::coords::ue_dir_to_bevy;
use asamu_core::glam as sim_glam;
use asamu_game::asamu_kismet::Output;
use asamu_player::{BootsEvent, CollisionWorld, PowerJumpStateName};
use bevy::asset::{AssetEventSystems, AssetPath, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::gltf::convert_coordinates::GltfConvertCoordinates;
use bevy::gltf::{GltfAssetLabel, GltfLoaderSettings};
use bevy::image::{
    ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor,
};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::converted::{ConvertedLevel, LevelEntity, RenderLevels, SOURCE};
use crate::kismet::{KismetActor, KismetFrame, Presentation};
use crate::{PlayerCamera, SCALE, Sim, to_render};

pub(crate) use beam_material::BeamMaterial;
pub(crate) use emitter::{BeamSocket, BeamView, SOCKET_NODE};

/// PLACEHOLDER: where the beam starts when there is no hand to start it
/// from (no converted hand mesh, or its socket is missing): this far from
/// the eye, UU: forward, right, down, leaving straight ahead. The original
/// always has the hand; with it the beam starts at the hand's socket
/// ([`emitter`]).
const BEAM_HAND_OFFSET_UU: [f32; 3] = [24.0, 10.0, 12.0];
/// The narrowest a beam ribbon is drawn, pixels (ours: thinner strips break
/// up into dots; see [`beam::ribbon`]).
const BEAM_MIN_PIXELS: f32 = 1.5;
/// Viewport height assumed when the camera has none yet, pixels (ours).
const FALLBACK_VIEWPORT_HEIGHT: f32 = 720.0;
/// Bevy depth bias of decal materials (ours, with the geometric lift).
const DECAL_DEPTH_BIAS: f32 = 4.0;
/// Visible time of the rocket-boots lens effect, s: the upper bound of its
/// particles' lifetime (`P_RocketBootsEffect`: one burst of 15 particles
/// living 0.3–0.5 s; the actor destroys itself when the system finishes).
/// CONFIRMED (content).
const LENS_SECONDS: f32 = 0.5;
/// Peak opacity of the lens overlay (ours).
const LENS_OPACITY: f32 = 0.22;
/// Crosshair fade duration, s (ours: the movie's fade frames are not
/// ported).
const CROSSHAIR_FADE_SECONDS: f32 = 0.25;
/// Size of a converted file read here (hostile-input bound).
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Size of a converted decal file read here (hostile-input bound: the JSON
/// is parsed into a value tree several times its size; the largest shipped
/// map's file is about 11 MB, a few times that pretty-printed).
const MAX_DECAL_FILE_BYTES: u64 = 96 * 1024 * 1024;
/// Size of the procedural fallback cone (no converted mesh), UU: the
/// converted `VelocityCone`'s bounds (box half-extents 28.2, CONFIRMED
/// (content)).
const FALLBACK_CONE_RADIUS_UU: f32 = 28.2;

/// Presentation settings of the effects.
#[derive(Resource, Clone, Debug)]
pub(crate) struct VfxSettings {
    /// `SpeedlinesActive` (default true, `DefaultSettings.ini`).
    pub speedlines: bool,
    /// The extras that colour the beam (beam colour, goat, Midas) and the
    /// collectibles count that unlocks them.
    pub extras: beam::ExtrasSettings,
    /// Draw the level decals.
    pub level_decals: bool,
}

impl Default for VfxSettings {
    fn default() -> Self {
        Self {
            speedlines: cone::SPEEDLINES_DEFAULT,
            extras: beam::ExtrasSettings::default(),
            level_decals: true,
        }
    }
}

/// Converted data the effects need.
struct VfxData {
    root: PathBuf,
    manifests: Manifests,
    opacity: HashMap<String, materials::OpacitySpec>,
    /// The grapple's particle systems (taken by [`bursts::Bursts`]).
    bursts: Option<bursts::BurstData>,
}

/// Loaded handles and the converted-data load.
#[derive(Resource, Default)]
struct VfxAssets {
    data: Option<VfxData>,
    load: Option<Task<Result<VfxData, String>>>,
    images: HashMap<String, Handle<Image>>,
    /// Decal materials by lower-case material path (`None`: the material's
    /// shape is unavailable, its decals are not drawn).
    decal_materials: HashMap<String, Option<Handle<StandardMaterial>>>,
    beam_material: Handle<BeamMaterial>,
    grapple_decal: Handle<StandardMaterial>,
    hard_land_decal: Handle<StandardMaterial>,
    cone_material: Handle<StandardMaterial>,
    cone_max_opacity: f32,
    cone_scale: f32,
    /// Material path the cone currently uses.
    cone_material_path: Option<String>,
    visuals: Option<beam::BeamVisuals>,
}

/// Effect state carried between frames.
#[derive(Resource)]
struct VfxState {
    /// The noise of every beam strand, in the order of the ribbons
    /// ([`BeamRibbon::index`]).
    noise: Vec<beam::BeamNoise>,
    /// Seconds the beam's material has run (its pulse pans with it).
    beam_time: f32,
    /// The beam is drawn this frame.
    beam_on: bool,
    rng: beam::RenderRng,
    pool: hit_decals::DecalPool<Entity>,
    lights: lights::GrappleLights,
    crosshair: crosshair::CrosshairState,
    crosshair_fade: Option<(f32, bool)>,
    lens: f32,
    tick_dt: f32,
}

impl Default for VfxState {
    fn default() -> Self {
        Self {
            noise: strands()
                .map(|(i, e, _)| beam::BeamNoise::for_strand(e, i as u64))
                .collect(),
            beam_time: 0.0,
            beam_on: false,
            rng: beam::RenderRng::new(0xDECA1),
            pool: hit_decals::DecalPool::default(),
            lights: lights::GrappleLights::default(),
            crosshair: crosshair::CrosshairState::default(),
            crosshair_fade: None,
            lens: 0.0,
            tick_dt: 1.0 / 60.0,
        }
    }
}

/// Level-decal loading.
#[derive(Resource, Default)]
struct LevelDecals {
    task: Option<Task<Vec<(usize, String, level_decals::LevelDecalFile)>>>,
}

/// One of the beam's ribbons: a strand of one of the beam's emitters.
#[derive(Component)]
struct BeamRibbon {
    /// Which of [`beam::EMITTERS`].
    emitter: usize,
    /// Index of the strand among all ribbons (its noise in [`VfxState`]).
    index: usize,
}

/// Every beam strand: its index, its emitter and the emitter's index.
fn strands() -> impl Iterator<Item = (usize, &'static beam::BeamEmitter, usize)> {
    beam::EMITTERS
        .iter()
        .enumerate()
        .flat_map(|(e, emitter)| (0..beam::STRANDS_PER_EMITTER).map(move |_| (emitter, e)))
        .enumerate()
        .map(|(i, (emitter, e))| (i, emitter, e))
}

/// The speed-line cone.
#[derive(Component)]
struct SpeedCone;

/// A run-time decal (grapple hit, hard landing).
#[derive(Component)]
struct HitDecal;

/// A level decal entity.
#[derive(Component)]
struct LevelDecalEntity;

/// A level decal hidden at level start, waiting for the game's actor id.
#[derive(Component)]
struct PendingActorId {
    level: String,
    slot: usize,
}

/// The crosshair ring.
#[derive(Component)]
struct CrosshairRing;

/// The hand-lamp panel and its lamps (0–2 grapples, 3 power jump).
#[derive(Component)]
struct LampPanel;

/// One hand lamp.
#[derive(Component)]
struct Lamp(usize);

/// The lens-effect overlay.
#[derive(Component)]
struct LensOverlay;

/// Gameplay visual effects (registered in `add_default_plugins`).
pub struct VfxPlugin;

impl Plugin for VfxPlugin {
    fn build(&self, app: &mut App) {
        beam_material::register(app);
        bursts::register(app);
        app.init_resource::<VfxSettings>()
            .init_resource::<bursts::Bursts>()
            .init_resource::<VfxAssets>()
            .init_resource::<VfxState>()
            .init_resource::<LevelDecals>()
            .add_systems(Startup, (setup_effects, setup_overlays, start_data_load))
            .add_systems(
                Update,
                (
                    poll_data_load,
                    apply_visuals,
                    consume_ticks,
                    route_kismet,
                    update_beam,
                    update_bursts,
                    update_cone,
                    update_crosshair,
                    update_overlays,
                    level_decal_lifecycle,
                    resolve_pending_ids,
                )
                    .chain(),
            )
            // The beam's shape needs this frame's hand pose and camera, and
            // has to reach the renderer in the same frame.
            .add_systems(
                PostUpdate,
                (shape_beam, shape_bursts)
                    .after(bevy::transform::TransformSystems::Propagate)
                    .before(AssetEventSystems),
            );
        selftest::add(app);
    }
}

// ---------------------------------------------------------------------------
// Assets.
// ---------------------------------------------------------------------------

fn address(a: AddressMode) -> ImageAddressMode {
    match a {
        AddressMode::Wrap => ImageAddressMode::Repeat,
        AddressMode::Clamp => ImageAddressMode::ClampToEdge,
        AddressMode::Mirror => ImageAddressMode::MirrorRepeat,
    }
}

/// Loads (once) a converted texture through the `converted://` source.
fn load_image(
    server: &AssetServer,
    cache: &mut HashMap<String, Handle<Image>>,
    t: &TextureBinding,
) -> Handle<Image> {
    if let Some(h) = cache.get(&t.file) {
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
    let handle: Handle<Image> = server
        .load_builder()
        .with_settings(move |s: &mut ImageLoaderSettings| {
            s.is_srgb = srgb;
            s.sampler = ImageSampler::Descriptor(descriptor.clone());
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(format!("{SOURCE}://textures/{}", t.file));
    cache.insert(t.file.clone(), handle.clone());
    handle
}

fn uv_affine(m: &RenderMaterial) -> Affine2 {
    Affine2::from_scale_angle_translation(
        Vec2::new(m.uv.scale[0], m.uv.scale[1]),
        0.0,
        Vec2::new(m.uv.offset[0], m.uv.offset[1]),
    )
}

/// A decal's material from the shared render-material model: blended or
/// masked as converted (an opaque description becomes blended: a decal is
/// never a solid surface), both faces, a depth bias.
fn decal_material(
    m: &RenderMaterial,
    server: &AssetServer,
    images: &mut HashMap<String, Handle<Image>>,
) -> StandardMaterial {
    let [r, g, b, a] = m.base_color;
    let [er, eg, eb] = m.emissive;
    StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        base_color_texture: m
            .base_color_texture
            .as_ref()
            .map(|t| load_image(server, images, t)),
        emissive: LinearRgba::rgb(er, eg, eb),
        emissive_texture: m
            .emissive_texture
            .as_ref()
            .map(|t| load_image(server, images, t)),
        perceptual_roughness: m.roughness,
        metallic: m.metallic,
        alpha_mode: match m.blend {
            BlendMode::Opaque | BlendMode::Translucent => AlphaMode::Blend,
            BlendMode::Masked { cutoff } => AlphaMode::Mask(cutoff),
            BlendMode::Additive => AlphaMode::Add,
            BlendMode::Modulate => AlphaMode::Multiply,
        },
        unlit: m.unlit,
        double_sided: true,
        cull_mode: None,
        depth_bias: DECAL_DEPTH_BIAS,
        uv_transform: uv_affine(m),
        ..default()
    }
}

/// Where a decal material's shape (its opacity) comes from.
#[derive(Clone, Debug, PartialEq)]
enum DecalShape {
    /// The displayed texture's alpha or a constant: the shared
    /// render-material model has it.
    Own,
    /// A baked mask (`decals/masks/…`, white with the opacity in alpha).
    Mask(String),
    /// The opacity texture itself, whose alpha channel is the mask (a decal
    /// file without baked masks).
    OpacityTexture(TextureBinding),
    /// The shape is in a texture this renderer cannot use as alpha: the
    /// decal is left out (a solid patch would be worse).
    Unavailable,
}

/// Decides [`DecalShape`] for a material: when it displays no texture and
/// its opacity comes from one (the cave paintings, runes and symbols: a
/// constant glow or colour, the picture only in the mask), the shape needs
/// the baked mask, or that texture's own alpha channel.
fn decal_shape(
    m: &RenderMaterial,
    opacity: Option<&materials::OpacitySpec>,
    mask: Option<&String>,
    textures: Option<&asamu_assets::manifest::TextureManifest>,
) -> DecalShape {
    let Some(texture) = opacity.and_then(|o| o.texture.as_deref()) else {
        return DecalShape::Own;
    };
    if m.base_color_texture.is_some() || matches!(m.blend, BlendMode::Opaque) {
        return DecalShape::Own;
    }
    if let Some(file) = mask {
        return DecalShape::Mask(file.clone());
    }
    let alpha = opacity
        .and_then(|o| o.channels.as_deref())
        .is_some_and(|c| c.starts_with('a'));
    match asamu_assets::materials::bind_texture(textures, texture, true) {
        Some(t) if alpha => DecalShape::OpacityTexture(t),
        _ => DecalShape::Unavailable,
    }
}

/// Loads (once) a baked decal mask through the `converted://` source.
fn load_mask(
    server: &AssetServer,
    cache: &mut HashMap<String, Handle<Image>>,
    file: &str,
    address: [AddressMode; 2],
) -> Handle<Image> {
    // Texture files are relative paths: a key with a leading `/` is no
    // texture's.
    let key = format!("/decal-mask/{file}");
    if let Some(h) = cache.get(&key) {
        return h.clone();
    }
    let descriptor = ImageSamplerDescriptor {
        address_mode_u: self::address(address[0]),
        address_mode_v: self::address(address[1]),
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    };
    let handle: Handle<Image> = server
        .load_builder()
        .with_settings(move |s: &mut ImageLoaderSettings| {
            s.is_srgb = false;
            s.sampler = ImageSampler::Descriptor(descriptor.clone());
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(format!("{SOURCE}://decals/{file}"));
    cache.insert(key, handle.clone());
    handle
}

/// Writes a beam ribbon into `mesh`. The beam's shader reads texture
/// coordinates and vertex colours, so every beam mesh carries both; a ribbon
/// without triangles becomes one degenerate triangle (the renderer warns
/// about meshes without vertices).
fn fill_beam_mesh(mesh: &mut Mesh, ribbon: beam::Ribbon) {
    let beam::Ribbon {
        mut positions,
        mut uvs,
        brightness,
        mut indices,
    } = ribbon;
    let mut colors: Vec<[f32; 4]> = brightness.iter().map(|k| [*k, *k, *k, 1.0]).collect();
    if indices.is_empty() {
        positions = vec![[0.0; 3]; 3];
        uvs = vec![[0.0; 2]; 3];
        colors = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        indices = vec![0, 1, 2];
    }
    let n = positions.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

/// A beam mesh without a beam in it yet.
fn empty_beam_mesh() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    fill_beam_mesh(&mut mesh, beam::Ribbon::default());
    mesh
}

fn mesh_from(
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Spawns the beam ribbons and the cone with fallback materials (replaced
/// once converted data is in).
fn setup_effects(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    beam_mats: Option<ResMut<Assets<BeamMaterial>>>,
    mut assets: ResMut<VfxAssets>,
) {
    let v = beam::beam_visuals(beam::BeamModes::default());
    // Without a renderer the beam's material is not registered: the ribbons
    // exist (the systems run) but have nothing to be drawn with.
    if let Some(mut beam_mats) = beam_mats {
        assets.beam_material = beam_mats.add(BeamMaterial::new(v.beam_color));
    }
    for (index, _, emitter) in strands() {
        commands.spawn((
            BeamRibbon { emitter, index },
            Mesh3d(meshes.add(empty_beam_mesh())),
            MeshMaterial3d(assets.beam_material.clone()),
            RenderLayers::layer(0),
            Transform::IDENTITY,
            Visibility::Hidden,
            NotShadowCaster,
            NotShadowReceiver,
            NoFrustumCulling,
        ));
    }
    assets.grapple_decal = mats.add(StandardMaterial {
        base_color: decal_tint(v.decal_color),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        depth_bias: DECAL_DEPTH_BIAS,
        ..default()
    });
    assets.hard_land_decal = mats.add(StandardMaterial {
        base_color: Color::linear_rgba(0.0, 0.0, 0.0, 0.5),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        depth_bias: DECAL_DEPTH_BIAS,
        ..default()
    });
    assets.cone_material = mats.add(StandardMaterial {
        base_color: Color::linear_rgba(1.0, 1.0, 1.0, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    // The converted cone's default material (`VelocityCone_Mat`) has an
    // opacity constant of 0.1 (CONFIRMED (content)); used until the
    // converted description is read.
    assets.cone_max_opacity = 0.1;
    assets.cone_scale = 1.0;
    // Fallback cone: axis along glTF −Z (= UE +X), the converted mesh's size.
    let r = FALLBACK_CONE_RADIUS_UU;
    let cone = Mesh::from(Cone {
        radius: r,
        height: 2.0 * r,
    })
    .rotated_by(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
    commands.spawn((
        SpeedCone,
        Mesh3d(meshes.add(cone)),
        MeshMaterial3d(assets.cone_material.clone()),
        Transform::IDENTITY,
        Visibility::Hidden,
        NotShadowCaster,
        NotShadowReceiver,
        NoFrustumCulling,
    ));
}

/// The hit decal's colour: `DecalColor` scaled so its brightest channel is
/// 1 (the original multiplies it by `DecalEmissiveMultiplier`, 300 or 50, in
/// an unlit material: always saturated).
fn decal_tint(c: [f32; 4]) -> Color {
    let m = c[0].max(c[1]).max(c[2]).max(1e-3);
    Color::linear_rgba(c[0] / m, c[1] / m, c[2] / m, 1.0)
}

fn start_data_load(level: Option<Res<ConvertedLevel>>, mut assets: ResMut<VfxAssets>) {
    let Some(level) = level else {
        return;
    };
    let dir = level.dir.clone();
    assets.load = Some(AsyncComputeTaskPool::get().spawn(async move {
        let manifests = dir.load_manifests().map_err(|e| e.to_string())?;
        let path = dir.root().join("materials").join("materials.json");
        let opacity = if path.is_file() {
            let bytes = asamu_assets::files::read_bounded(&path, MAX_FILE_BYTES)
                .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&bytes);
            materials::parse_opacity(&text)?
        } else {
            HashMap::new()
        };
        // The grapple's two particle systems; without them the grapple
        // has no flash and no impact, and everything else is as it was.
        let bursts = bursts::BurstData::load(dir.root(), manifests.textures.as_ref())
            .unwrap_or_else(|e| {
                warn!("vfx: the grapple's particle systems are unavailable ({e})");
                None
            });
        Ok(VfxData {
            root: dir.root().to_path_buf(),
            manifests,
            opacity,
            bursts,
        })
    }));
}

/// Material of the cone for `path` (texture from its opacity channel).
fn cone_material_for(
    data: &VfxData,
    path: &str,
    server: &AssetServer,
    images: &mut HashMap<String, Handle<Image>>,
) -> Option<(StandardMaterial, f32)> {
    let spec = data.opacity.get(&path.to_ascii_lowercase())?;
    let texture = spec.texture.as_deref().and_then(|t| {
        asamu_assets::materials::bind_texture(data.manifests.textures.as_ref(), t, false)
    });
    let m = StandardMaterial {
        base_color: Color::linear_rgba(1.0, 1.0, 1.0, 0.0),
        base_color_texture: texture.as_ref().map(|t| load_image(server, images, t)),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        uv_transform: Affine2::from_scale(Vec2::new(spec.uv_scale[0], spec.uv_scale[1])),
        ..default()
    };
    Some((m, spec.value.clamp(0.0, 1.0)))
}

#[allow(clippy::too_many_arguments)]
fn poll_data_load(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut assets: ResMut<VfxAssets>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    beam_mats: Option<ResMut<Assets<BeamMaterial>>>,
    mut bursts: ResMut<bursts::Bursts>,
    cones: Query<Entity, With<SpeedCone>>,
) {
    let Some(task) = assets.load.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    assets.load = None;
    let mut data = match result {
        Ok(d) => d,
        Err(e) => {
            warn!("vfx: converted data unavailable ({e}); effects use fallback materials");
            return;
        }
    };
    info!(
        "vfx: the grapple's flash and impact systems are {}",
        if data.bursts.is_some() {
            "converted"
        } else {
            "not converted: neither is drawn"
        }
    );
    bursts.set_data(data.bursts.take());
    let VfxAssets {
        images,
        beam_material,
        hard_land_decal,
        cone_material,
        cone_max_opacity,
        cone_scale,
        cone_material_path,
        visuals,
        ..
    } = &mut *assets;
    let textures = data.manifests.textures.as_ref();
    // Beam: the three textures of the beam material (the shared material
    // model keeps one of them; the beam's own material takes all three).
    if let Some(mut beam_mats) = beam_mats
        && let Some(mut beam) = beam_mats.get_mut(&*beam_material)
    {
        let mut bind = |path: &str| {
            asamu_assets::materials::bind_texture(textures, path, false)
                .map(|t| load_image(&server, images, &t))
        };
        let (thin, core, pulse) = (
            bind(beam::TEXTURE_THIN),
            bind(beam::TEXTURE_MAIN),
            bind(beam::TEXTURE_PULSE),
        );
        info!(
            "vfx: beam textures: thin line {}, core {}, pulse {}",
            thin.is_some(),
            core.is_some(),
            pulse.is_some()
        );
        beam.set_textures(thin, core, pulse);
    }
    // Hard landing: black, alpha from the impact texture of its opacity
    // channel (the shared model cannot combine the two textures).
    if let Some(spec) = data
        .opacity
        .get(&hit_decals::HARD_LAND_DECAL_MATERIAL.to_ascii_lowercase())
        && let Some(t) = spec
            .texture
            .as_deref()
            .and_then(|t| asamu_assets::materials::bind_texture(textures, t, false))
        && let Some(mut m) = mats.get_mut(&*hard_land_decal)
    {
        m.base_color = Color::linear_rgba(0.0, 0.0, 0.0, spec.value.clamp(0.0, 1.0));
        m.base_color_texture = Some(load_image(&server, images, &t));
    }
    // Speed-line cone: the converted mesh and its section material.
    let cone_path = cone_material_path
        .clone()
        .unwrap_or_else(|| "ASAMUVelocityEffect.VelocityCone_Mat".to_owned());
    if let Some((m, max)) = cone_material_for(&data, &cone_path, &server, images)
        && let Some(mut slot) = mats.get_mut(&*cone_material)
    {
        *slot = m;
        *cone_max_opacity = max;
        *cone_material_path = Some(cone_path);
    }
    if let Some((_, entry)) = data
        .manifests
        .meshes
        .as_ref()
        .and_then(|m| m.get_for_package(cone::CONE_MESH, "Startup"))
        && let Some(lod) = entry.lod0()
        && let Ok(gltf) = lod.safe_gltf()
    {
        let path = AssetPath::from(format!("{SOURCE}://meshes/{gltf}"));
        let label = GltfAssetLabel::Primitive {
            mesh: 0,
            primitive: 0,
        };
        // The converted level meshes' loader settings (UE3 axes are baked
        // into the files; no glTF materials).
        let mesh: Handle<Mesh> = server
            .load_builder()
            .with_settings(|s: &mut GltfLoaderSettings| {
                s.load_materials = RenderAssetUsages::empty();
                s.load_cameras = false;
                s.load_lights = false;
                s.load_animations = false;
                s.convert_coordinates = Some(GltfConvertCoordinates {
                    rotate_scene_entity: false,
                    rotate_meshes: false,
                });
            })
            .load(label.from_asset(path));
        *cone_scale = entry.scale;
        for e in &cones {
            commands.entity(e).insert(Mesh3d(mesh.clone()));
        }
    }
    *visuals = None;
    assets.data = Some(data);
}

/// Keeps the beam and decal colours in line with the gun's modes.
fn apply_visuals(
    settings: Res<VfxSettings>,
    mut assets: ResMut<VfxAssets>,
    server: Res<AssetServer>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    beam_mats: Option<ResMut<Assets<BeamMaterial>>>,
) {
    let v = beam::beam_visuals(settings.extras.modes());
    if assets.visuals == Some(v) {
        return;
    }
    if let Some(mut beam_mats) = beam_mats
        && let Some(mut m) = beam_mats.get_mut(&assets.beam_material)
    {
        m.params.set_color(v.beam_color);
    }
    // The grapple decal: the converted decal texture, tinted.
    let converted = assets.data.as_ref().and_then(|d| {
        d.manifests.materials.as_ref().and_then(|m| {
            m.render_material(
                hit_decals::GRAPPLE_DECAL_MATERIAL,
                d.manifests.textures.as_ref(),
            )
        })
    });
    let VfxAssets {
        images,
        grapple_decal,
        ..
    } = &mut *assets;
    if let Some(mut m) = mats.get_mut(&*grapple_decal) {
        if let Some(c) = &converted {
            let mut sm = decal_material(c, &server, images);
            sm.base_color = decal_tint(v.decal_color);
            sm.unlit = true;
            *m = sm;
        } else {
            m.base_color = decal_tint(v.decal_color);
        }
    }
    assets.visuals = Some(v);
}

// ---------------------------------------------------------------------------
// Tick reports: decals, lights, lens.
// ---------------------------------------------------------------------------

/// UE → render direction as an array.
fn dir_to_render(v: sim_glam::Vec3) -> [f32; 3] {
    ue_dir_to_bevy(v).to_array()
}

fn decal_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    m: &hit_decals::DecalMesh,
) -> Option<Entity> {
    if m.indices.is_empty() {
        return None;
    }
    let mesh = mesh_from(
        m.positions
            .iter()
            .map(|p| to_render(*p).to_array())
            .collect(),
        m.normals.iter().map(|n| dir_to_render(*n)).collect(),
        m.uvs.clone(),
        m.indices.clone(),
    );
    Some(
        commands
            .spawn((
                HitDecal,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material),
                Transform::IDENTITY,
                NotShadowCaster,
                NotShadowReceiver,
            ))
            .id(),
    )
}

/// The surface normal at an attach's `anchor`, re-traced from the eye (the
/// tick report carries none; when the trace misses the anchor — a target
/// that moved — the direction back to the eye stands in).
///
/// The anchor is the fire trace's hit as the original reports it, which is
/// short of the surface along the trace: by 4.0 uu on a mesh's triangles,
/// less on anything else (GRAPPLE.md G-AT-4;
/// `asamu_player::world::PULL_BACK_MAX_TRIANGLES`). The surface is therefore
/// looked for up to that far beyond the anchor, plus the 2 uu of slack this
/// search always had.
fn anchor_normal(game: &asamu_game::Game, anchor: sim_glam::Vec3) -> sim_glam::Vec3 {
    let eye = game.eye_position();
    let to = anchor - eye;
    let reach = asamu_player::world::PULL_BACK_MAX_TRIANGLES + 2.0;
    game.world()
        .raycast(eye, to, to.length() + reach)
        .filter(|h| h.position.distance(anchor) < reach)
        .map_or_else(|| (-to).normalize_or_zero(), |h| h.normal)
}

/// The grapple's hit decal for an attach at `anchor`: the decal box is
/// projected onto the collision world along the surface normal there.
fn grapple_decal(
    game: &asamu_game::Game,
    anchor: sim_glam::Vec3,
    random01: f32,
) -> hit_decals::DecalMesh {
    // See `anchor_normal` for how the surface is found again.
    let spawn = hit_decals::DecalSpawn::grapple(anchor, anchor_normal(game, anchor), random01);
    hit_decals::project(&spawn, |o, d, len| {
        game.world()
            .raycast(o, d, len)
            .map(|h| (h.position, h.normal))
    })
}

/// The hard-landing decal under the player (floor normal from the pawn's
/// floor, straight up when unknown).
fn hard_landing_decal(game: &asamu_game::Game, random01: f32) -> hit_decals::DecalMesh {
    let p = game.player();
    let floor = if p.pawn.floor.length_squared() > 0.5 {
        p.pawn.floor
    } else {
        sim_glam::Vec3::Z
    };
    let half = game.params().movement.capsule_half_height.value;
    let spawn = hit_decals::DecalSpawn::hard_landing(p.position, half, floor, random01);
    hit_decals::project(&spawn, |o, d, len| {
        game.world()
            .raycast(o, d, len)
            .map(|h| (h.position, h.normal))
    })
}

#[allow(clippy::too_many_arguments)]
fn consume_ticks(
    mut commands: Commands,
    mut ticks: MessageReader<crate::ui::GameTick>,
    sim: Option<Res<Sim>>,
    assets: Res<VfxAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut state: ResMut<VfxState>,
    mut bursts: ResMut<bursts::Bursts>,
    decals: Query<Entity, With<HitDecal>>,
) {
    let Some(sim) = sim else {
        ticks.clear();
        return;
    };
    let state = &mut *state;
    if sim.is_added() {
        // A new game: the decal manager and the HUD start fresh.
        state.pool.clear();
        for e in &decals {
            commands.entity(e).despawn();
        }
        state.lights = lights::GrappleLights::default();
        state.crosshair = crosshair::CrosshairState::default();
        state.crosshair_fade = None;
        state.lens = 0.0;
        bursts.reset();
    }
    let game = &sim.game;
    let dt = (1.0 / game.clock().tick_rate_hz()) as f32;
    state.tick_dt = dt;
    for tick in ticks.read() {
        let ev = &tick.0.events;
        // The flash at the hand and the impact at the anchor.
        if let Some(attach) = ev.gun.attached {
            bursts.attached(attach.anchor, anchor_normal(game, attach.anchor));
        }
        // The grapple's hit decal.
        if let Some(attach) = ev.gun.attached
            && !attach.surface.class.rejects_decal()
        {
            let mesh = grapple_decal(game, attach.anchor, state.rng.next_f32());
            if let Some(e) = decal_entity(
                &mut commands,
                &mut meshes,
                assets.grapple_decal.clone(),
                &mesh,
            ) && let Some(old) = state.pool.spawn(e)
            {
                commands.entity(old).despawn();
            }
            debug!(
                "vfx: grapple decal at {} ({} active, {} triangles)",
                attach.anchor,
                state.pool.len(),
                mesh.indices.len() / 3
            );
        }
        // The hard-landing decal.
        if let Some(landing) = ev.landing
            && landing.hard
        {
            let mesh = hard_landing_decal(game, state.rng.next_f32());
            if let Some(e) = decal_entity(
                &mut commands,
                &mut meshes,
                assets.hard_land_decal.clone(),
                &mesh,
            ) && let Some(old) = state.pool.spawn(e)
            {
                commands.entity(old).despawn();
            }
        }
        if ev.boots == Some(BootsEvent::BoostBegan) {
            state.lens = LENS_SECONDS;
        }
        // Decal lifetimes run on game time.
        if !state.pool.is_empty() {
            for e in state.pool.tick(dt) {
                commands.entity(e).despawn();
            }
        }
        // The hand lights (`UpdateLights` once per gun tick).
        let gun = &game.player().script.gun;
        let pj = &game.player().script.power_jump;
        state.lights.update(
            dt,
            lights::LightInputs {
                max_grapples: gun.max_grapples,
                times_grappled: gun.times_grappled,
                can_grapple: gun.can_grapple,
                light_up: pj.state == PowerJumpStateName::Charging && pj.charged,
                workshop_mode: false,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Kismet.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn route_kismet(
    mut frames: MessageReader<KismetFrame>,
    mut state: ResMut<VfxState>,
    mut assets: ResMut<VfxAssets>,
    server: Res<AssetServer>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    for frame in frames.read() {
        for out in &frame.outputs {
            match out {
                Output::Crosshair { show, fade } => {
                    let frames = state.crosshair.toggle(*show, *fade);
                    if *fade && !frames.is_empty() {
                        state.crosshair_fade = Some((0.0, *show));
                    }
                }
                Output::ConsoleCommand { command } => {
                    let mut words = command.split_whitespace();
                    if words
                        .next()
                        .is_some_and(|c| c.eq_ignore_ascii_case("ToggleCrosshair"))
                    {
                        let show = match words.next() {
                            Some(w) => w.eq_ignore_ascii_case("true") || w == "1",
                            None => !state.crosshair.show,
                        };
                        state.crosshair.toggle(show, false);
                    }
                }
                Output::VelocityConeMaterial {
                    material: Some(path),
                } => {
                    let VfxAssets {
                        data,
                        images,
                        cone_material,
                        cone_max_opacity,
                        cone_material_path,
                        ..
                    } = &mut *assets;
                    *cone_material_path = Some(path.clone());
                    if let Some(d) = data.as_ref()
                        && let Some((m, max)) = cone_material_for(d, path, &server, images)
                        && let Some(mut slot) = mats.get_mut(&*cone_material)
                    {
                        *slot = m;
                        *cone_max_opacity = max;
                    }
                    debug!(
                        "vfx: velocity cone material {path} (opacity parameter {})",
                        cone::PARAMETER_NAME
                    );
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Beam and cone.
// ---------------------------------------------------------------------------

/// The interpolated eye (UU), as the camera uses it.
fn eye_now(sim: &Sim, fixed: &Time<Fixed>) -> sim_glam::Vec3 {
    let a = fixed.overstep_fraction().clamp(0.0, 1.0);
    sim.prev_eye.lerp(sim.curr_eye, a)
}

/// Where the beam ends now (UU), while the gun is attached.
fn beam_anchor(sim: &Sim) -> Option<sim_glam::Vec3> {
    let gun = &sim.game.player().script.gun;
    gun.attached?;
    Some(beam::beam_end(
        gun.grapple_location,
        gun.follow.map(|f| f.helper),
    ))
}

/// Whether the beam is drawn: the gun is attached, and the hand it leaves
/// from is on screen when there is a hand (`hand_shown`: `None` without a
/// hand camera). Ours: the original's gun cannot fire with the hand hidden
/// (GRAPPLE.md G-AC-3), and a beam whose hand is not drawn (a Matinee
/// director cut that starts during a grapple) would start at nothing.
fn beam_shown(attached: bool, hand_shown: Option<bool>) -> bool {
    attached && hand_shown.unwrap_or(true)
}

/// Switches the beam on and off with the gun, puts its ribbons on the layer
/// of the camera that draws them, and advances its noise and its pulse. The
/// shape follows in [`shape_beam`], once this frame's poses are known.
#[allow(clippy::too_many_arguments)]
fn update_beam(
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    assets: Res<VfxAssets>,
    mut state: ResMut<VfxState>,
    beam_mats: Option<ResMut<Assets<BeamMaterial>>>,
    hands: Query<&BeamView>,
    sockets: Query<(), With<BeamSocket>>,
    mut ribbons: Query<(&mut Visibility, &mut RenderLayers), With<BeamRibbon>>,
) {
    let hand = hands.iter().next();
    let attached = sim.as_ref().is_some_and(|s| beam_anchor(s).is_some());
    let on = beam_shown(attached, hand.map(|h| h.hand_shown));
    if state.beam_on != on {
        state.beam_on = on;
    }
    // With a hand and its socket the hand's camera draws the beam;
    // otherwise the world's does (the fallback start).
    let layer = if hand.is_some() && !sockets.is_empty() {
        RenderLayers::layer(crate::FIRST_PERSON_LAYER)
    } else {
        RenderLayers::layer(0)
    };
    let wanted = if on {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for (mut visibility, mut layers) in &mut ribbons {
        if *visibility != wanted {
            *visibility = wanted;
        }
        if *layers != layer {
            *layers = layer.clone();
        }
    }
    if !on {
        return;
    }
    let dt = time.delta_secs();
    for noise in &mut state.noise {
        noise.update(dt);
    }
    if dt.is_finite() && dt > 0.0 {
        // Kept small: only the pulse's phase matters.
        state.beam_time = (state.beam_time + dt) % 3600.0;
    }
    if let Some(mut beam_mats) = beam_mats
        && let Some(mut m) = beam_mats.get_mut(&assets.beam_material)
    {
        m.params.pulse.x = beam_material::pulse_offset(state.beam_time);
    }
}

/// Advances the grapple's flash and impact with the running game (they
/// stand still while it is paused), the impact riding the gun's anchor.
fn update_bursts(
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    fixed: Res<Time<Fixed>>,
    mut bursts: ResMut<bursts::Bursts>,
) {
    let Some(sim) = sim else {
        return;
    };
    if sim.game.state() != asamu_game::GameState::Playing {
        return;
    }
    bursts.advance(time.delta_secs(), beam_anchor(&sim), eye_now(&sim, &fixed));
}

/// What a frame's cameras and hand say about the beam: everything
/// [`strand_ribbon`] needs, in the world camera's space ("view space": the
/// eye at the origin, −z forward, render units).
#[derive(Clone, Copy, Debug, PartialEq)]
struct BeamFrame {
    /// Where the beam leaves, view space.
    source: Vec3,
    /// The direction it leaves in, view space, unit length.
    axis: Vec3,
    /// The anchor, view space.
    end: Vec3,
    /// From view space to the space of the camera that draws the beam.
    map: emitter::FovMap,
    /// World from the drawing camera's space.
    world_from_draw: bevy::math::Affine3A,
    /// The size of a pixel at unit depth in view space.
    pixel: f32,
}

impl BeamFrame {
    /// The frame of a beam that leaves the hand's socket: `socket` and
    /// `socket_axis` are in the hand camera's space, `end` in the world
    /// camera's; `view` and `hand` are the two cameras' half extents
    /// ([`emitter::half_extents`]).
    fn from_hand(
        socket: Vec3,
        socket_axis: Vec3,
        end: Vec3,
        view: Vec2,
        hand: Vec2,
        world_from_hand: bevy::math::Affine3A,
        viewport_height: f32,
    ) -> Self {
        let map = emitter::FovMap::new(view, hand);
        Self {
            source: map.hand_to_view(socket),
            axis: map.hand_dir_to_view(socket_axis),
            end,
            map,
            world_from_draw: world_from_hand,
            pixel: emitter::pixel_size(1.0, view.y, viewport_height),
        }
    }

    /// PLACEHOLDER: the frame of a beam without a hand, drawn by the world
    /// camera from [`BEAM_HAND_OFFSET_UU`], leaving straight ahead.
    fn without_hand(
        end: Vec3,
        view: Vec2,
        world_from_view: bevy::math::Affine3A,
        viewport_height: f32,
    ) -> Self {
        let [forward, right, down] = BEAM_HAND_OFFSET_UU;
        Self {
            source: Vec3::new(right, -down, -forward) * SCALE.bevy_units_per_uu,
            axis: Vec3::NEG_Z,
            end,
            map: emitter::FovMap::IDENTITY,
            world_from_draw: world_from_view,
            pixel: emitter::pixel_size(1.0, view.y, viewport_height),
        }
    }
}

/// One strand of the beam as a ribbon in world space: the emitter's curve
/// from the source to the anchor in view space, with the strand's noise, as
/// a strip facing the eye, never thinner than [`BEAM_MIN_PIXELS`]; then
/// every vertex into the drawing camera's space and out to the world.
fn strand_ribbon(
    frame: &BeamFrame,
    emitter: &beam::BeamEmitter,
    noise: &[[f32; 2]],
) -> beam::Ribbon {
    let unit = SCALE.bevy_units_per_uu;
    let path = beam::beam_path(
        frame.source,
        frame.end,
        frame.axis,
        emitter.source_strength,
        noise,
        unit,
    );
    let mut ribbon = beam::ribbon(&path, emitter.width_uu * unit, |depth| {
        BEAM_MIN_PIXELS * frame.pixel * depth
    });
    for p in &mut ribbon.positions {
        let drawn = frame.map.view_to_hand(Vec3::from_array(*p));
        *p = frame.world_from_draw.transform_point3(drawn).to_array();
    }
    ribbon
}

/// Half extents of a camera's perspective view; `None` for another kind of
/// projection.
fn view_extents(projection: &Projection) -> Option<Vec2> {
    match projection {
        Projection::Perspective(p) => Some(emitter::half_extents(p.fov, p.aspect_ratio)),
        _ => None,
    }
}

/// Shapes the beam's ribbons for this frame (after the animation and the
/// transforms of the frame are known, before the meshes go to the
/// renderer): from the hand's socket as the hand camera draws it, or
/// without a hand from the fallback start, to the anchor.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn shape_beam(
    sim: Option<Res<Sim>>,
    state: Res<VfxState>,
    mut meshes: ResMut<Assets<Mesh>>,
    view: Query<(&GlobalTransform, &Projection, &Camera), With<PlayerCamera>>,
    hands: Query<(&GlobalTransform, &Projection), With<BeamView>>,
    sockets: Query<&GlobalTransform, With<BeamSocket>>,
    ribbons: Query<(&BeamRibbon, &Mesh3d)>,
    mut was_on: Local<bool>,
) {
    let first = state.beam_on && !*was_on;
    *was_on = state.beam_on;
    if !state.beam_on {
        return;
    }
    let Some(end) = sim.as_ref().and_then(|s| beam_anchor(s)) else {
        return;
    };
    let Some((view_tf, view_projection, camera)) = view.iter().next() else {
        return;
    };
    let Some(view_half) = view_extents(view_projection) else {
        return;
    };
    let viewport_height = camera
        .physical_viewport_size()
        .map_or(FALLBACK_VIEWPORT_HEIGHT, |s| s.y as f32);
    let end = view_tf.affine().inverse().transform_point3(to_render(end));
    let hand = hands
        .iter()
        .next()
        .and_then(|(tf, projection)| Some((tf, view_extents(projection)?, sockets.iter().next()?)));
    let frame = match hand {
        Some((hand_tf, hand_half, socket)) => {
            let hand_from_world = hand_tf.affine().inverse();
            BeamFrame::from_hand(
                hand_from_world.transform_point3(socket.translation()),
                // The socket's X axis in the original's axes is the node's
                // forward axis in the converted mesh's (SKELETAL.md).
                hand_from_world.transform_vector3(socket.forward().as_vec3()),
                end,
                view_half,
                hand_half,
                hand_tf.affine(),
                viewport_height,
            )
        }
        None => BeamFrame::without_hand(end, view_half, view_tf.affine(), viewport_height),
    };
    if first {
        // One line per grapple: where the beam leaves, for whoever compares
        // a picture with the numbers.
        let on_screen = frame.source.truncate() / (-frame.source.z).max(1e-6) / view_half;
        info!(
            "vfx: beam from {} at view ({:.1}, {:.1}, {:.1}) uu, {:.1}% right and {:.1}% up of the \
             screen's half size, leaving along ({:.2}, {:.2}, {:.2}); anchor {:.0} uu away",
            if hand.is_some() {
                "the hand's socket"
            } else {
                "the fallback start (no hand)"
            },
            frame.source.x / SCALE.bevy_units_per_uu,
            frame.source.y / SCALE.bevy_units_per_uu,
            frame.source.z / SCALE.bevy_units_per_uu,
            on_screen.x * 100.0,
            on_screen.y * 100.0,
            frame.axis.x,
            frame.axis.y,
            frame.axis.z,
            frame.end.length() / SCALE.bevy_units_per_uu,
        );
    }
    for (ribbon, mesh) in &ribbons {
        let (Some(emitter), Some(noise)) = (
            beam::EMITTERS.get(ribbon.emitter),
            state.noise.get(ribbon.index),
        ) else {
            continue;
        };
        if let Some(mut mesh) = meshes.get_mut(&mesh.0) {
            fill_beam_mesh(&mut mesh, strand_ribbon(&frame, emitter, &noise.offsets()));
        }
    }
}

/// Draws the grapple's flash and impact for this frame (same place in the
/// frame as [`shape_beam`]): the impact by the world camera where it is,
/// the flash at the hand's socket, by the hand's camera when there is a hand
/// ([`bursts`] says why and how).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn shape_bursts(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut assets: ResMut<VfxAssets>,
    mut bursts: ResMut<bursts::Bursts>,
    mut meshes: ResMut<Assets<Mesh>>,
    materials: Option<ResMut<Assets<bursts::GlowMaterial>>>,
    view: Query<(&GlobalTransform, &Projection), With<PlayerCamera>>,
    hands: Query<(&GlobalTransform, &Projection, &BeamView)>,
    sockets: Query<&GlobalTransform, With<BeamSocket>>,
    mut existing: Query<(&Mesh3d, &mut Visibility, &mut RenderLayers), With<bursts::BurstMesh>>,
) {
    // Without a renderer the sprites' material is not registered.
    let Some(mut materials) = materials else {
        return;
    };
    let Some((view_tf, view_projection)) = view.iter().next() else {
        return;
    };
    let Some(view_half) = view_extents(view_projection) else {
        return;
    };
    let world = bursts::QuadView {
        eye: view_tf.translation(),
        right: view_tf.right().as_vec3(),
        up: view_tf.up().as_vec3(),
        pushed: None,
    };
    let hand = match hands.iter().next() {
        // No hand at all: the flash is where the beam's fallback start is,
        // drawn by the world camera (PLACEHOLDER, as that start).
        None => {
            let [forward, right, down] = BEAM_HAND_OFFSET_UU;
            let source = Vec3::new(right, -down, -forward) * SCALE.bevy_units_per_uu;
            Some((
                bursts::socket_transform(
                    view_tf.transform_point(source),
                    view_tf.forward().as_vec3(),
                    view_tf.right().as_vec3(),
                    view_tf.up().as_vec3(),
                ),
                world,
                0,
            ))
        }
        Some((hand_tf, hand_projection, beam_view)) => {
            match (view_extents(hand_projection), sockets.iter().next()) {
                (Some(hand_half), Some(socket)) if beam_view.hand_shown => Some((
                    bursts::socket_transform(
                        socket.translation(),
                        socket.forward().as_vec3(),
                        socket.right().as_vec3(),
                        socket.up().as_vec3(),
                    ),
                    bursts::QuadView {
                        pushed: Some((
                            view_tf.affine().inverse(),
                            emitter::FovMap::new(view_half, hand_half),
                            hand_tf.affine(),
                        )),
                        ..world
                    },
                    crate::FIRST_PERSON_LAYER,
                )),
                // The hand is not on screen (or not there yet): no flash.
                _ => None,
            }
        }
    };
    bursts::draw(
        &mut commands,
        &server,
        &mut assets.images,
        &mut bursts,
        &mut meshes,
        &mut materials,
        &mut existing,
        &bursts::DrawFrame { world, hand },
    );
}

#[allow(clippy::too_many_arguments)]
fn update_cone(
    sim: Option<Res<Sim>>,
    fixed: Res<Time<Fixed>>,
    settings: Res<VfxSettings>,
    assets: Res<VfxAssets>,
    state: Res<VfxState>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut cones: Query<(&mut Transform, &mut Visibility), With<SpeedCone>>,
) {
    let Some(sim) = sim else {
        for (_, mut v) in &mut cones {
            *v = Visibility::Hidden;
        }
        return;
    };
    let game = &sim.game;
    let v = cone::velocity_seen_by_cone(game.player(), state.tick_dt);
    let param = cone::cone_parameter(v);
    let show = settings.speedlines
        && param > 0.0
        && game.state() == asamu_game::GameState::Playing
        && game.uses_original_params();
    // Opacity = parameter × the material's maximum opacity; the material is
    // touched only when the value changes.
    let a = if show {
        param * assets.cone_max_opacity
    } else {
        0.0
    };
    let stale = mats
        .get(&assets.cone_material)
        .is_some_and(|m| (m.base_color.alpha() - a).abs() > 1e-3);
    if stale && let Some(mut m) = mats.get_mut(&assets.cone_material) {
        m.base_color.set_alpha(a);
    }
    let location = cone::cone_location(eye_now(&sim, &fixed), v, state.tick_dt);
    let (yaw, pitch) = cone::cone_rotation(v);
    // UE local-to-world of the cone actor (rows: X along V, Y, Z; roll 0).
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let ue = sim_glam::Mat4::from_cols(
        sim_glam::Vec4::new(cp * cy, cp * sy, sp, 0.0),
        sim_glam::Vec4::new(-sy, cy, 0.0, 0.0),
        sim_glam::Vec4::new(-sp * cy, -sp * sy, cp, 0.0),
        location.extend(1.0),
    );
    let m = asamu_assets::transform::instance_matrix(&ue, assets.cone_scale, SCALE);
    for (mut t, mut vis) in &mut cones {
        if let Some(rt) = asamu_assets::transform::decompose(&m) {
            *t = Transform {
                translation: Vec3::from_array(rt.translation.to_array()),
                rotation: Quat::from_array(rt.rotation.to_array()),
                scale: Vec3::from_array(rt.scale.to_array()),
            };
        }
        let want = if show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
    }
}

// ---------------------------------------------------------------------------
// HUD overlays: crosshair ring, hand lamps, lens effect.
// ---------------------------------------------------------------------------

fn setup_overlays(mut commands: Commands) {
    commands.spawn((
        CrosshairRing,
        Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            top: percent(50),
            width: px(20),
            height: px(20),
            margin: UiRect {
                left: px(-10),
                top: px(-10),
                ..default()
            },
            border: UiRect::all(px(2)),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BorderColor::all(Color::NONE),
        BackgroundColor(Color::NONE),
        Visibility::Hidden,
    ));
    let lamp = |i: usize, size: f32| {
        (
            Lamp(i),
            Node {
                width: px(size),
                height: px(size),
                margin: UiRect::all(px(3)),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.35)),
            BackgroundColor(Color::NONE),
        )
    };
    commands.spawn((
        LampPanel,
        Node {
            position_type: PositionType::Absolute,
            right: px(18),
            bottom: px(18),
            align_items: AlignItems::Center,
            ..default()
        },
        Visibility::Hidden,
        children![lamp(0, 12.0), lamp(1, 12.0), lamp(2, 12.0), lamp(3, 18.0)],
    ));
    commands.spawn((
        LensOverlay,
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            top: px(0),
            width: percent(100),
            height: percent(100),
            ..default()
        },
        BackgroundColor(Color::NONE),
        Visibility::Hidden,
    ));
}

/// Size (px) and colour of the ring for a crosshair look (ours: the
/// movie's art is not ported).
fn ring_style(
    kind: crosshair::CrosshairKind,
    tint: Option<[f32; 3]>,
) -> Option<(f32, Color, Color)> {
    use crosshair::CrosshairKind as K;
    let lit = tint.map_or(Color::srgb(0.35, 0.9, 1.0), |c| {
        Color::linear_rgb(c[0], c[1], c[2])
    });
    let dim = Color::srgba(1.0, 1.0, 1.0, 0.35);
    match kind {
        K::Hidden => None,
        K::Dot => Some((5.0, Color::NONE, Color::srgba(1.0, 1.0, 1.0, 0.8))),
        K::Disabled => Some((18.0, dim, Color::NONE)),
        K::Enabled => Some((24.0, lit, Color::NONE)),
        K::StoryDisabled => Some((10.0, dim, Color::NONE)),
        K::StoryEnabled => Some((14.0, Color::srgb(1.0, 0.9, 0.5), Color::NONE)),
    }
}

#[allow(clippy::type_complexity)]
fn update_crosshair(
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    settings: Res<VfxSettings>,
    pres: Option<Res<Presentation>>,
    mut state: ResMut<VfxState>,
    mut ring: Query<
        (
            &mut Node,
            &mut BorderColor,
            &mut BackgroundColor,
            &mut Visibility,
        ),
        With<CrosshairRing>,
    >,
) {
    let Some(sim) = sim else {
        for (_, _, _, mut v) in &mut ring {
            *v = Visibility::Hidden;
        }
        return;
    };
    let game = &sim.game;
    let state = &mut *state;
    let story = game.in_story_mode();
    if story != state.crosshair.story {
        state.crosshair.set_story_mode(story);
    }
    if game.params().gun.is_some() && game.player().script.gun.spawned {
        state.crosshair.change(game.crosshair(), true);
    }
    // Fade (ours: a linear ramp over `CROSSHAIR_FADE_SECONDS`).
    let mut alpha = 1.0;
    let mut fading_out = false;
    if let Some((t, show)) = state.crosshair_fade.as_mut() {
        *t += time.delta_secs();
        let k = (*t / CROSSHAIR_FADE_SECONDS).clamp(0.0, 1.0);
        alpha = if *show { k } else { 1.0 - k };
        fading_out = !*show && k < 1.0;
        if k >= 1.0 {
            state.crosshair_fade = None;
        }
    }
    let hud = pres.as_ref().is_none_or(|p| p.hud_shown());
    let tint = beam::beam_visuals(settings.extras.modes()).crosshair_tint;
    let style = ring_style(state.crosshair.current, tint);
    let shown = (state.crosshair.visible() || fading_out) && hud;
    for (mut node, mut border, mut bg, mut vis) in &mut ring {
        let Some((size, b, fill)) = style.filter(|_| shown) else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        };
        let want = px(size);
        if node.width != want {
            node.width = want;
            node.height = want;
            node.margin.left = px(-size * 0.5);
            node.margin.top = px(-size * 0.5);
        }
        *border = BorderColor::all(b.with_alpha(b.alpha() * alpha));
        bg.0 = fill.with_alpha(fill.alpha() * alpha);
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_overlays(
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    settings: Res<VfxSettings>,
    pres: Option<Res<Presentation>>,
    mut state: ResMut<VfxState>,
    mut panel: Query<&mut Visibility, (With<LampPanel>, Without<LensOverlay>)>,
    mut lamps: Query<(&Lamp, &mut BackgroundColor), Without<LensOverlay>>,
    mut lens: Query<(&mut BackgroundColor, &mut Visibility), (With<LensOverlay>, Without<Lamp>)>,
) {
    let playing = sim
        .as_ref()
        .is_some_and(|s| s.game.state() == asamu_game::GameState::Playing);
    let hud = pres.as_ref().is_none_or(|p| p.hud_shown());
    // Hand lamps: shown while the hand (and its gun) is visible.
    let lamps_shown = playing
        && hud
        && sim.as_ref().is_some_and(|s| {
            let gun = &s.game.player().script.gun;
            s.game.params().gun.is_some() && gun.spawned && !gun.hand_hidden
        });
    for mut v in &mut panel {
        let want = if lamps_shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *v != want {
            *v = want;
        }
    }
    let c = beam::beam_visuals(settings.extras.modes()).decal_color;
    for (lamp, mut bg) in &mut lamps {
        let value = state.lights.lamps.get(lamp.0).copied().unwrap_or(0.0);
        let a = value.clamp(0.0, 1.0);
        let col = if lamp.0 == 3 {
            Color::srgba(1.0, 0.85, 0.4, a)
        } else {
            decal_tint(c).with_alpha(a)
        };
        bg.0 = col;
    }
    // Lens effect.
    let dt = time.delta_secs();
    if state.lens > 0.0 && playing {
        state.lens = (state.lens - dt).max(0.0);
    }
    for (mut bg, mut vis) in &mut lens {
        let k = if playing {
            state.lens / LENS_SECONDS
        } else {
            0.0
        };
        if k <= 0.0 {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        }
        bg.0 = Color::srgba(0.85, 0.88, 0.92, LENS_OPACITY * k);
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
    }
}

// ---------------------------------------------------------------------------
// Level decals.
// ---------------------------------------------------------------------------

/// The decal file of the level package `package` under the converted root,
/// or `None` when the name is not a plain file name (level names come from
/// the converted scene: a separator, a `..`, a drive or a control character
/// in one must not lead the read out of `decals/`).
fn decals_file(root: &Path, package: &str) -> Option<PathBuf> {
    if package.is_empty() || package == "." || package == ".." {
        return None;
    }
    let name = asamu_assets::files::safe_relative_path(&format!("{package}.decals.json")).ok()?;
    (!name.contains('/')).then(|| root.join("decals").join(name))
}

#[allow(clippy::too_many_arguments)]
fn level_decal_lifecycle(
    mut commands: Commands,
    levels: Option<Res<RenderLevels>>,
    settings: Res<VfxSettings>,
    mut assets: ResMut<VfxAssets>,
    mut decals: ResMut<LevelDecals>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    server: Res<AssetServer>,
    existing: Query<Entity, With<LevelDecalEntity>>,
    mut waiting: Local<bool>,
) {
    let Some(levels) = levels else {
        return;
    };
    // A new plan: drop the previous level's decals and load this one's
    // (waits for the converted data so materials resolve).
    if levels.is_changed() {
        for e in &existing {
            commands.entity(e).despawn();
        }
        decals.task = None;
        // Decal materials are rebuilt per plan: whether a material has a
        // mask is the level's decal file's to say.
        assets.decal_materials.clear();
        *waiting = settings.level_decals;
    }
    if *waiting && assets.load.is_none() {
        *waiting = false;
        let root = assets.data.as_ref().map(|d| d.root.clone());
        if let Some(root) = root {
            let names: Vec<(usize, String)> = levels.names.iter().cloned().enumerate().collect();
            decals.task = Some(AsyncComputeTaskPool::get().spawn(async move {
                let mut out = Vec::new();
                for (index, name) in names {
                    let Some(path) = decals_file(&root, &name) else {
                        warn!("vfx: level name {name:?} is not a file name; no decals read for it");
                        continue;
                    };
                    if !path.is_file() {
                        continue;
                    }
                    let parsed = asamu_assets::files::read_bounded(&path, MAX_DECAL_FILE_BYTES)
                        .map_err(|e| e.to_string())
                        .and_then(|b| level_decals::parse(&String::from_utf8_lossy(&b)));
                    match parsed {
                        Ok((file, dropped)) => {
                            if dropped > 0 {
                                warn!(
                                    "vfx: {}: {dropped} malformed decal entries dropped",
                                    path.display()
                                );
                            }
                            out.push((index, name, file));
                        }
                        Err(e) => warn!("vfx: {}: {e}", path.display()),
                    }
                }
                out
            }));
        }
    }
    let Some(task) = decals.task.as_mut() else {
        return;
    };
    let Some(files) = check_ready(task) else {
        return;
    };
    decals.task = None;
    let VfxAssets {
        data,
        images,
        decal_materials,
        ..
    } = &mut *assets;
    let mut spawned = 0usize;
    let mut triangles = 0usize;
    let mut unavailable: Vec<String> = Vec::new();
    for (index, name, file) in files {
        triangles += file.triangles();
        for d in &file.decals {
            let material = d.material.as_deref().and_then(|path| {
                let key = path.to_ascii_lowercase();
                decal_materials
                    .entry(key.clone())
                    .or_insert_with(|| {
                        let textures = data.as_ref().and_then(|dd| dd.manifests.textures.as_ref());
                        let rm = data
                            .as_ref()
                            .and_then(|dd| {
                                dd.manifests
                                    .materials
                                    .as_ref()
                                    .and_then(|m| m.render_material(path, textures))
                            })
                            .unwrap_or_else(|| {
                                asamu_assets::materials::fallback_material(Some(path))
                            });
                        let opacity = data.as_ref().and_then(|dd| dd.opacity.get(&key));
                        let shape = decal_shape(&rm, opacity, file.masks.get(&key), textures);
                        let mut sm = decal_material(&rm, &server, images);
                        let mask = match shape {
                            DecalShape::Own => None,
                            DecalShape::Mask(mask_file) => {
                                // The mask repeats as its texture would.
                                let address = opacity
                                    .and_then(|o| o.texture.as_deref())
                                    .and_then(|t| {
                                        asamu_assets::materials::bind_texture(textures, t, true)
                                    })
                                    .map_or([AddressMode::Clamp; 2], |t| t.address);
                                Some(load_mask(&server, images, &mask_file, address))
                            }
                            DecalShape::OpacityTexture(t) => Some(load_image(&server, images, &t)),
                            DecalShape::Unavailable => {
                                unavailable.push(path.to_owned());
                                return None;
                            }
                        };
                        if let Some(mask) = mask {
                            // The material's own constant colour, the mask
                            // as the texture that carries the alpha.
                            let value = opacity.map_or(1.0, |o| o.value).clamp(0.0, 1.0);
                            sm.base_color_texture = Some(mask);
                            sm.base_color.set_alpha(value);
                            sm.uv_transform = opacity.map_or(Affine2::IDENTITY, |o| {
                                Affine2::from_scale(Vec2::new(o.uv_scale[0], o.uv_scale[1]))
                            });
                        }
                        Some(mats.add(sm))
                    })
                    .clone()
            });
            let Some(material) = material else {
                continue;
            };
            for g in &d.receivers {
                let rg =
                    level_decals::render_geometry(g, |p| to_render(p).to_array(), dir_to_render);
                let mesh = mesh_from(rg.positions, rg.normals, rg.uvs, rg.indices);
                let mut e = commands.spawn((
                    LevelDecalEntity,
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    NotShadowCaster,
                    NotShadowReceiver,
                ));
                if d.hidden {
                    // Shown only when Kismet unhides it (`KismetActor` once
                    // the game's actor id is known).
                    e.insert((
                        Visibility::Hidden,
                        PendingActorId {
                            level: name.clone(),
                            slot: d.slot,
                        },
                    ));
                } else {
                    // A level mesh of the actor: streamed, hidden and moved
                    // with it (`kismet::sync_actor_render`).
                    e.insert((
                        Visibility::Inherited,
                        LevelEntity {
                            actor_slot: d.slot,
                            level: index,
                        },
                    ));
                }
                spawned += 1;
            }
        }
    }
    info!("vfx: {spawned} level decal meshes ({triangles} triangles)");
    if !unavailable.is_empty() {
        warn!(
            "vfx: {} decal materials keep their shape in an opacity texture and have no baked mask \
             (run `asamu-import decals --force`); their decals are not drawn, e.g. {:?}",
            unavailable.len(),
            unavailable.first()
        );
    }
}

/// Gives hidden-at-start decals the game's actor id once the game exists.
fn resolve_pending_ids(
    mut commands: Commands,
    sim: Option<Res<Sim>>,
    pending: Query<(Entity, &PendingActorId)>,
) {
    let Some(map) = sim.as_ref().and_then(|s| s.game.scene_map()) else {
        return;
    };
    for (e, p) in &pending {
        let id = map
            .level_index(&p.level)
            .and_then(|i| asamu_game::asamu_world::scene::actor_id(u8::try_from(i).ok()?, p.slot));
        let mut ec = commands.entity(e);
        ec.remove::<PendingActorId>();
        if let Some(id) = id {
            ec.insert(KismetActor(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::ConvertedDir;
    use asamu_game::Game;
    use asamu_player::InputFrame;

    /// Bevy's vertical field of view for a horizontal one.
    fn vertical(horizontal_degrees: f32, aspect: f32) -> f32 {
        2.0 * ((horizontal_degrees.to_radians() * 0.5).tan() / aspect).atan()
    }

    fn perspective(horizontal_degrees: f32, aspect: f32) -> bevy::camera::PerspectiveProjection {
        bevy::camera::PerspectiveProjection {
            fov: vertical(horizontal_degrees, aspect),
            aspect_ratio: aspect,
            near: 0.01,
            far: 1000.0,
            ..default()
        }
    }

    /// The whole chain a frame goes through, with the renderer's own
    /// projection matrices: the first row of every strand's ribbon, drawn by
    /// the hand camera, straddles the pixel the hand camera draws the socket
    /// on, and its last row the pixel the world camera gives the anchor: for
    /// a turned and displaced camera, several windows, the world's field of
    /// view at its default, zoomed and widened, and several hand poses.
    #[test]
    fn the_beam_meets_the_socket_and_the_anchor_on_screen() {
        use bevy::camera::CameraProjection;
        let world_from_camera = bevy::math::Affine3A::from_rotation_translation(
            Quat::from_euler(EulerRot::YXZ, 2.1, -0.4, 0.0),
            Vec3::new(310.0, -42.0, 128.0),
        );
        let camera_from_world = world_from_camera.inverse();
        let anchors = [
            Vec3::new(0.0, 0.0, -4.0),
            Vec3::new(2.0, 3.0, -15.0),
            Vec3::new(-9.0, 4.0, -48.0),
        ];
        let sockets = [
            (Vec3::new(0.21, -0.19, -0.45), Vec3::new(-0.2, 0.3, -1.0)),
            (Vec3::new(0.10, -0.05, -0.33), Vec3::new(0.0, 0.0, -1.0)),
            (Vec3::new(0.30, -0.28, -0.60), Vec3::new(-0.5, 0.6, -0.4)),
        ];
        let mut worst = (0.0f32, 0.0f32);
        for (w, h) in [(1280.0f32, 720.0f32), (1920.0, 1200.0), (2560.0, 1080.0)] {
            let viewport = Vec2::new(w, h);
            let aspect = w / h;
            let hand = perspective(asamu_game::npc::HAND_MESH_FOV, aspect);
            for fov in [90.0f32, 30.0, 110.0] {
                let view = perspective(fov, aspect);
                for (socket, axis) in sockets {
                    for anchor in anchors {
                        let frame = BeamFrame::from_hand(
                            socket,
                            axis,
                            anchor,
                            emitter::half_extents(view.fov, aspect),
                            emitter::half_extents(hand.fov, aspect),
                            world_from_camera,
                            h,
                        );
                        let socket_pixel =
                            emitter::pixel_of(hand.get_clip_from_view(), socket, viewport)
                                .expect("the socket is in view");
                        let anchor_pixel =
                            emitter::pixel_of(view.get_clip_from_view(), anchor, viewport)
                                .expect("the anchor is in view");
                        for (i, e, _) in strands() {
                            let noise = beam::BeamNoise::for_strand(e, i as u64).offsets();
                            let rb = strand_ribbon(&frame, e, &noise);
                            assert_eq!(rb.positions.len(), 2 * (beam::INTERPOLATION_POINTS + 1));
                            // The ribbon is in world space; the hand camera
                            // (at the world camera's place) draws it.
                            let drawn = |k: usize| {
                                let local = camera_from_world
                                    .transform_point3(Vec3::from_array(rb.positions[k]));
                                emitter::pixel_of(hand.get_clip_from_view(), local, viewport)
                                    .expect("in view")
                            };
                            let first = (drawn(0) + drawn(1)) * 0.5;
                            let n = rb.positions.len();
                            let last = (drawn(n - 2) + drawn(n - 1)) * 0.5;
                            let (es, ea) =
                                (first.distance(socket_pixel), last.distance(anchor_pixel));
                            worst = (worst.0.max(es), worst.1.max(ea));
                            assert!(
                                es < 1.0 && ea < 1.0,
                                "{w}x{h} fov {fov}: start {first} for socket {socket_pixel}, end \
                                 {last} for anchor {anchor_pixel}"
                            );
                            // The strip is at least the minimum wide on
                            // screen everywhere.
                            for row in 0..n / 2 {
                                let wide = drawn(row * 2).distance(drawn(row * 2 + 1));
                                assert!(wide > BEAM_MIN_PIXELS * 0.8, "row {row}: {wide}");
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "worst error: {} pixels at the socket, {} at the anchor",
            worst.0, worst.1
        );
    }

    /// Without a hand the beam is drawn by the world camera from the
    /// fallback start at the lower right of the view, leaving straight
    /// ahead.
    #[test]
    fn a_beam_without_a_hand_starts_at_the_lower_right_of_the_view() {
        let half = emitter::half_extents(vertical(90.0, 16.0 / 9.0), 16.0 / 9.0);
        let frame = BeamFrame::without_hand(
            Vec3::new(0.0, 0.0, -10.0),
            half,
            bevy::math::Affine3A::IDENTITY,
            720.0,
        );
        let [f, r, d] = BEAM_HAND_OFFSET_UU;
        let unit = SCALE.bevy_units_per_uu;
        assert!(frame.source.distance(Vec3::new(r, -d, -f) * unit) < 1e-6);
        assert_eq!(frame.axis, Vec3::NEG_Z);
        assert_eq!(frame.map, emitter::FovMap::IDENTITY);
        let rb = strand_ribbon(&frame, &beam::EMITTERS[0], &[]);
        let first = (Vec3::from_array(rb.positions[0]) + Vec3::from_array(rb.positions[1])) * 0.5;
        assert!(first.distance(frame.source) < 1e-5);
        assert!(first.x > 0.0 && first.y < 0.0 && first.z < 0.0);
    }

    #[test]
    fn the_beam_is_shown_with_the_gun_and_the_hand() {
        assert!(!beam_shown(false, None));
        assert!(!beam_shown(false, Some(true)));
        assert!(beam_shown(true, None), "no hand camera: the fallback");
        assert!(beam_shown(true, Some(true)));
        assert!(!beam_shown(true, Some(false)), "the hand is not on screen");
        // Four strands: two of each emitter, the wide one first.
        let all: Vec<(usize, usize)> = strands().map(|(i, _, e)| (i, e)).collect();
        assert_eq!(all, [(0, 0), (1, 0), (2, 1), (3, 1)]);
        assert_eq!(VfxState::default().noise.len(), all.len());
    }

    /// A beam mesh always has the attributes the beam's shader reads, also
    /// before the first beam.
    #[test]
    fn beam_meshes_carry_what_the_shader_reads() {
        let mut mesh = empty_beam_mesh();
        assert_eq!(mesh.count_vertices(), 3);
        for _ in 0..2 {
            assert!(mesh.attribute(Mesh::ATTRIBUTE_POSITION).is_some());
            assert!(mesh.attribute(Mesh::ATTRIBUTE_NORMAL).is_some());
            assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_0).is_some());
            assert!(mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_some());
            let frame = BeamFrame::without_hand(
                Vec3::new(1.0, 2.0, -30.0),
                Vec2::new(1.0, 0.5625),
                bevy::math::Affine3A::IDENTITY,
                720.0,
            );
            fill_beam_mesh(&mut mesh, strand_ribbon(&frame, &beam::EMITTERS[1], &[]));
        }
        assert_eq!(mesh.count_vertices(), 2 * (beam::INTERPOLATION_POINTS + 1));
    }

    #[test]
    fn decal_files_stay_inside_the_decals_folder() {
        let root = Path::new("/data/converted");
        assert_eq!(
            decals_file(root, "AG-Darkcave"),
            Some(root.join("decals").join("AG-Darkcave.decals.json"))
        );
        for bad in [
            "",
            ".",
            "..",
            "../AG-Darkcave",
            "a/b",
            "a\\b",
            "/etc/passwd",
            "C:evil",
            "name#label",
            "new\nline",
            "nul\0",
        ] {
            assert_eq!(decals_file(root, bad), None, "{bad:?}");
        }
        // Dots inside a name are a file name like any other.
        assert!(decals_file(root, "Map.v2").is_some());
        let long = "x".repeat(5000);
        assert_eq!(decals_file(root, &long), None);
    }

    fn texture_manifest(paths: &[&str]) -> asamu_assets::manifest::TextureManifest {
        let entries = paths
            .iter()
            .map(|p| {
                (
                    (*p).to_owned(),
                    asamu_assets::manifest::TextureEntry {
                        package: "Map".to_owned(),
                        class: "Texture2D".to_owned(),
                        file: format!("Map/{p}.dds"),
                        format: "PF_DXT5".to_owned(),
                        size: [512, 512],
                        mips: 10,
                        cube: false,
                        srgb: Some(true),
                        address: None,
                        lod_group: None,
                        compression_settings: None,
                    },
                )
            })
            .collect();
        asamu_assets::manifest::TextureManifest::from_entries(1, entries)
    }

    #[test]
    fn decal_shapes_come_from_masks_when_the_material_shows_no_texture() {
        let textures = texture_manifest(&["Pkg.T_Mask", "Pkg.T_Colour"]);
        let spec = |texture: Option<&str>, channels: &str| materials::OpacitySpec {
            value: 1.0,
            texture: texture.map(str::to_owned),
            channels: Some(channels.to_owned()),
            uv_scale: [1.0, 1.0],
            alpha_mode: "mask".to_owned(),
        };
        // A cave painting: a constant glow, masked, the picture in the alpha
        // of an opacity texture.
        let mut glow = asamu_assets::materials::fallback_material(Some("Pkg.M_Painting"));
        glow.blend = BlendMode::Masked { cutoff: 0.33 };
        glow.emissive = [0.02, 8.0, 10.0];
        let alpha = spec(Some("Pkg.T_Mask"), "a");
        let rgb = spec(Some("Pkg.T_Mask"), "rgb");
        let file = "masks/m_Pkg_M_Painting-00000000.dds".to_owned();
        // The baked mask wins.
        assert_eq!(
            decal_shape(&glow, Some(&alpha), Some(&file), Some(&textures)),
            DecalShape::Mask(file.clone())
        );
        assert_eq!(
            decal_shape(&glow, Some(&rgb), Some(&file), None),
            DecalShape::Mask(file.clone())
        );
        // Without one, an alpha-channel mask can use the texture itself...
        match decal_shape(&glow, Some(&alpha), None, Some(&textures)) {
            DecalShape::OpacityTexture(t) => {
                assert_eq!(t.file, "Map/Pkg.T_Mask.dds");
                assert!(!t.srgb, "bound for its alpha: no colour conversion");
            }
            other => panic!("{other:?}"),
        }
        // ...a colour-channel mask cannot, nor an unconverted texture: the
        // decal is left out rather than drawn as a solid patch.
        assert_eq!(
            decal_shape(&glow, Some(&rgb), None, Some(&textures)),
            DecalShape::Unavailable
        );
        assert_eq!(
            decal_shape(&glow, Some(&alpha), None, None),
            DecalShape::Unavailable
        );
        assert_eq!(
            decal_shape(
                &glow,
                Some(&spec(Some("Pkg.Missing"), "a")),
                None,
                Some(&textures)
            ),
            DecalShape::Unavailable
        );
        // The shared model already has the shape: a displayed texture, a
        // constant opacity, no opacity at all, or an opaque material.
        let mut textured = glow.clone();
        textured.base_color_texture =
            asamu_assets::materials::bind_texture(Some(&textures), "Pkg.T_Colour", false);
        assert!(textured.base_color_texture.is_some());
        assert_eq!(
            decal_shape(&textured, Some(&alpha), Some(&file), Some(&textures)),
            DecalShape::Own
        );
        assert_eq!(
            decal_shape(&glow, Some(&spec(None, "a")), Some(&file), Some(&textures)),
            DecalShape::Own
        );
        assert_eq!(
            decal_shape(&glow, None, Some(&file), Some(&textures)),
            DecalShape::Own
        );
        let mut opaque = glow.clone();
        opaque.blend = BlendMode::Opaque;
        assert_eq!(
            decal_shape(&opaque, Some(&alpha), Some(&file), Some(&textures)),
            DecalShape::Own
        );
    }

    #[test]
    fn ring_styles_cover_every_look() {
        use crosshair::CrosshairKind as K;
        assert!(ring_style(K::Hidden, None).is_none());
        let sizes: Vec<f32> = [
            K::Dot,
            K::Disabled,
            K::Enabled,
            K::StoryDisabled,
            K::StoryEnabled,
        ]
        .into_iter()
        .map(|k| ring_style(k, None).map_or(0.0, |s| s.0))
        .collect();
        assert!(sizes.iter().all(|s| *s > 0.0));
        // The enabled look is the largest and takes the beam tint.
        assert!(sizes[2] > sizes[1]);
        let tinted = ring_style(K::Enabled, Some([1.0, 0.0, 0.0])).map(|s| s.1);
        assert_eq!(tinted, Some(Color::linear_rgb(1.0, 0.0, 0.0)));
    }

    #[test]
    fn tints_are_normalised() {
        let c = decal_tint(beam::DEFAULT_BEAM_COLOR).to_linear();
        assert!((c.blue - 1.0).abs() < 1e-6 && c.red < 0.1);
        // The beam is not normalised: its material keeps the colour times
        // the original's factor.
        let b = BeamMaterial::new(beam::DEFAULT_BEAM_COLOR).params.light;
        assert!((b.z - 0.79 * beam::EMISSIVE_SCALE).abs() < 1e-5);
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// user-local `asamu-import` output with `levels/`, `meshes/` and
    /// `decals/`; `ASAMU_VFX_LEVEL` picks the level, default the first one
    /// that loads): the grapple effects on a converted level.
    ///
    /// - every decal file parses with nothing dropped and has triangles;
    /// - the player grapples the first acceptable surface found by turning
    ///   on the spot; on the attach tick the hit decal projects onto the
    ///   level's collision as a non-empty mesh around the anchor with
    ///   texture coordinates in the unit square;
    /// - the beam runs from the hand to the anchor;
    /// - while pulled, the speed-line parameter is the capped speed's
    ///   (`2000 / 5000`), not the post-pull speed's;
    /// - the hand lamp of the used grapple goes dark.
    #[test]
    fn converted_level_grapple_effects() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let dir = ConvertedDir::new(root.clone());
        let mut files = 0usize;
        if let Ok(rd) = std::fs::read_dir(root.join("decals")) {
            for e in rd.flatten() {
                let p = e.path();
                if !p.to_string_lossy().ends_with(".decals.json") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).expect("decal file");
                let (file, dropped) = level_decals::parse(&text).expect("decal file parses");
                assert_eq!(dropped, 0, "{}", p.display());
                assert!(file.triangles() > 0, "{}", p.display());
                eprintln!(
                    "{}: {} decals, {} triangles",
                    file.package,
                    file.decals.len(),
                    file.triangles()
                );
                files += 1;
            }
        }
        eprintln!("{files} decal files");
        let wanted = std::env::var("ASAMU_VFX_LEVEL").ok();
        let mut loaded = None;
        for level in dir.list_levels() {
            if wanted
                .as_deref()
                .is_some_and(|w| !w.eq_ignore_ascii_case(&level))
            {
                continue;
            }
            if let Ok(game) = Game::load_level(&root, &level) {
                loaded = Some((level, game));
                break;
            }
        }
        let Some((level, mut game)) = loaded else {
            eprintln!("skipped: no converted level loads from {}", root.display());
            return;
        };
        game.start();
        game.set_max_grapples(3);
        let idle = InputFrame::default();
        for _ in 0..120 {
            game.tick(&idle);
        }
        // Turn on the spot until the gun's trace finds an acceptable target
        // some way off.
        let mut aimed = false;
        'scan: for pitch in [10.0f32, 30.0, 50.0, 0.0, -15.0, 70.0] {
            for step in 0..72 {
                let p = game.player_mut();
                p.yaw = (step as f32 * 5.0).to_radians() - std::f32::consts::PI;
                p.pitch = pitch.to_radians();
                if game
                    .gun_aim()
                    .is_some_and(|a| a.acceptable && (400.0..3500.0).contains(&a.distance))
                {
                    aimed = true;
                    break 'scan;
                }
            }
        }
        assert!(aimed, "{level}: nothing grapple-able around the start");
        let hold = InputFrame {
            grapple_held: true,
            ..InputFrame::default()
        };
        let mut attach = None;
        for _ in 0..30 {
            let report = game.tick(&hold).expect("tick");
            if let Some(a) = report.events.gun.attached {
                attach = Some(a);
                break;
            }
        }
        let attach = attach.unwrap_or_else(|| panic!("{level}: the grapple did not attach"));
        let anchor = attach.anchor;
        // Hit decal.
        let mesh = grapple_decal(&game, anchor, 0.25);
        assert!(!mesh.indices.is_empty(), "{level}: empty hit decal");
        let reach = hit_decals::GRAPPLE_DECAL_SIZE * 0.5 * 2f32.sqrt()
            + hit_decals::GRAPPLE_DECAL_THICKNESS;
        for (p, uv) in mesh.positions.iter().zip(&mesh.uvs) {
            assert!(p.distance(anchor) <= reach, "{p} vs {anchor}");
            assert!((-1e-3..=1.0 + 1e-3).contains(&uv[0]) && (-1e-3..=1.0 + 1e-3).contains(&uv[1]));
        }
        assert!(!attach.surface.class.rejects_decal());
        // Beam.
        let gun = game.player().script.gun;
        let end = beam::beam_end(gun.grapple_location, gun.follow.map(|f| f.helper));
        assert!((end - anchor).length() < 1.0);
        // The beam, as a frame without a hand shapes it: the camera at the
        // eye, looking along the view.
        let p = game.player();
        let world_from_view = bevy::math::Affine3A::from_rotation_translation(
            crate::bevy_quat(asamu_core::coords::ue_view_to_bevy_rotation(p.yaw, p.pitch)),
            to_render(game.eye_position()),
        );
        let frame = BeamFrame::without_hand(
            world_from_view.inverse().transform_point3(to_render(end)),
            emitter::half_extents(1.0, 16.0 / 9.0),
            world_from_view,
            720.0,
        );
        let start = world_from_view.transform_point3(frame.source);
        for (i, e, _) in strands() {
            let noise = beam::BeamNoise::for_strand(e, i as u64).offsets();
            let rb = strand_ribbon(&frame, e, &noise);
            assert_eq!(rb.positions.len(), 2 * (beam::INTERPOLATION_POINTS + 1));
            let mid = |k: usize| {
                (Vec3::from_array(rb.positions[k]) + Vec3::from_array(rb.positions[k + 1])) * 0.5
            };
            let n = rb.positions.len();
            let unit = SCALE.bevy_units_per_uu;
            assert!(mid(0).distance(start) < 1e-3, "{} {start}", mid(0));
            assert!(mid(n - 2).distance(to_render(end)) < 0.5 * unit);
        }
        let start =
            asamu_core::coords::bevy_pos_to_ue(sim_glam::Vec3::from_array(start.to_array()), SCALE);
        // Speed lines while pulled: the capped speed's parameter.
        let dt = (1.0 / game.clock().tick_rate_hz()) as f32;
        let mut capped = 0usize;
        let mut lamps = lights::GrappleLights::default();
        for _ in 0..10 {
            lamps.update(
                dt,
                lights::LightInputs {
                    max_grapples: 3,
                    times_grappled: 0,
                    can_grapple: true,
                    light_up: false,
                    workshop_mode: false,
                },
            );
        }
        for _ in 0..90 {
            if game.tick(&hold).is_none() || !game.player().script.gun.is_attached() {
                break;
            }
            let seen = cone::velocity_seen_by_cone(game.player(), dt);
            let param = cone::cone_parameter(seen);
            assert!(param <= 0.4 + 1e-3, "{param}");
            if (param - 0.4).abs() < 1e-3 {
                capped += 1;
                // The post-pull velocity alone would read higher.
                assert!(cone::cone_parameter(game.player().velocity) > 0.4);
            }
            let g = &game.player().script.gun;
            lamps.update(
                dt,
                lights::LightInputs {
                    max_grapples: g.max_grapples,
                    times_grappled: g.times_grappled,
                    can_grapple: g.can_grapple,
                    light_up: false,
                    workshop_mode: false,
                },
            );
        }
        eprintln!(
            "{level}: attached at {anchor} ({:.0} uu), hit decal {} triangles, {capped} ticks at the \
             capped speed, lamps {:?}",
            (anchor - start).length(),
            mesh.indices.len() / 3,
            lamps.lamps
        );
        assert!(capped > 0, "{level}: the pull never reached the cap");
        assert!(
            lamps.lamps[2] < 0.5 && lamps.lamps[0] >= 1.0,
            "{:?}",
            lamps.lamps
        );
    }
}
