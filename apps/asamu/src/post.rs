//! Post-processing, fog and sky approximations from the original level data.
//!
//! For a converted level the plugin loads the scene's atmosphere data
//! (`asamu_assets::scene::AtmosphereInfo`: `WorldInfo` post-process
//! defaults, post-process volumes, height fog and fog volume components,
//! the default post-process chain) and drives, on the player camera and the
//! offscreen screenshot camera:
//!
//! - an **exponential height fog** pass with the original's maths (density
//!   at the camera height, line integral along each view ray, start
//!   distance, max opacity, two-colour directional inscattering), replacing
//!   the placeholder distance fog;
//! - the original's **post-process volume rules**: world defaults,
//!   overridden by the highest-priority enabled volume containing the
//!   camera, blended over time with the settings' interpolation durations
//!   (UE3 has no distance blend radius);
//! - the **uber post-process effect** of the default chain: its customizable
//!   tonemapper and the colour-grading LUT (the level's LUT texture with the
//!   scene shadows / highlights / midtones / desaturation / colorize
//!   transform baked in, as the original's LUT blender does), replacing
//!   Bevy's tonemapping;
//! - **bloom** through Bevy's bloom, mapped from the settings (an
//!   approximation: Bevy's bloom has no tint and a different kernel).
//!
//! Not rendered yet: fog volumes, depth of field (off in every shipped
//! level), motion blur, ambient occlusion, film grain, the hidden material
//! effects of the chain. `docs/reverse-engineering/POST_FOG_SKY.md` has the
//! evidence and the mapping; `post/ue3.rs` the UE3 rules with tests.
//!
//! `ASAMU_POST=off` disables the plugin (Bevy's default look and the
//! placeholder fog), for before/after comparisons; `ASAMU_POST=no-fog,
//! no-bloom,no-lut,no-tonemap` leaves out single parts ([`PostSwitches`]).
//! `--no-fog` disables the fog pass. The graybox level is not affected.

mod dds;
mod render;
pub(crate) mod ue3;

use std::collections::BTreeMap;

use asamu_assets::ConvertedDir;
use asamu_assets::scene::{PostProcessVolumeInfo, UeLightKind};
use asamu_core::coords::{bevy_pos_to_ue, ue_dir_to_bevy};
use asamu_core::glam as sim_glam;
use bevy::asset::RenderAssetUsages;
use bevy::camera::{CameraOutputMode, Exposure, Hdr};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::image::ImageSampler;
use bevy::pbr::DistanceFog;
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::{
    BlendState, Extent3d, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::PlayerCamera;
use crate::converted::ConvertedLevel;
use render::{PostFx, PostFxRenderPlugin, PostFxUniform};
use ue3::{
    ColorTransform, HeightFog, Lut, PostBlender, PostSettings, TonemapperType, UberEffect,
    UberParams,
};

/// Ray length (UU) used for pixels where nothing was drawn: effectively
/// infinite (OUR CHOICE; UE3's fog pass sees the far plane there). Rays
/// that rise converge to a finite amount of fog, the others fill up.
const EMPTY_PIXEL_DISTANCE_UU: f32 = 1.0e9;

/// Post-processing, fog and sky approximations from the original level data.
pub struct PostPlugin;

impl Plugin for PostPlugin {
    fn build(&self, app: &mut App) {
        let switches = std::env::var("ASAMU_POST")
            .map(|v| PostSwitches::parse(&v))
            .unwrap_or_default();
        if !switches.enabled {
            return;
        }
        app.insert_resource(PostRuntime {
            switches,
            ..PostRuntime::default()
        });
        app.add_plugins(PostFxRenderPlugin)
            .add_systems(Update, (start_loading, poll_loading).chain())
            .add_systems(
                PostUpdate,
                (apply_atmosphere, match_overlay_cameras)
                    .chain()
                    .after(TransformSystems::Propagate),
            );
    }
}

/// A level's atmosphere, ready to apply.
#[derive(Debug, Clone)]
struct LevelAtmosphere {
    level: String,
    world: PostSettings,
    persist: Option<bool>,
    /// Volumes in priority order with their parsed settings.
    volumes: Vec<(PostProcessVolumeInfo, PostSettings)>,
    fog: Option<HeightFog>,
    /// Direction the fog light shines along (UE3, unit), when a
    /// directional light exists.
    fog_light_ue: Option<sim_glam::Vec3>,
    /// The chain's uber effect (`None`: the scene predates the atmosphere
    /// export, or the chain has none shown in game).
    uber: Option<UberEffect>,
    /// Source LUT textures by path (lower case).
    luts: BTreeMap<String, Lut>,
    notes: Vec<String>,
}

/// `ASAMU_POST`: `off` (or `0`) disables the plugin; otherwise a
/// comma-separated list of parts to leave out (`no-fog`, `no-bloom`,
/// `no-lut`, `no-tonemap`), for comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PostSwitches {
    pub enabled: bool,
    pub fog: bool,
    pub bloom: bool,
    pub lut: bool,
    pub tonemap: bool,
}

impl Default for PostSwitches {
    fn default() -> Self {
        Self {
            enabled: true,
            fog: true,
            bloom: true,
            lut: true,
            tonemap: true,
        }
    }
}

impl PostSwitches {
    /// Parses the variable's value (unknown words are ignored).
    #[must_use]
    pub fn parse(v: &str) -> Self {
        let mut s = Self::default();
        for word in v.split(',').map(str::trim) {
            match word.to_ascii_lowercase().as_str() {
                "off" | "0" | "false" => s.enabled = false,
                "no-fog" => s.fog = false,
                "no-bloom" => s.bloom = false,
                "no-lut" => s.lut = false,
                "no-tonemap" => s.tonemap = false,
                _ => {}
            }
        }
        s
    }
}

/// Plugin state.
#[derive(Resource, Default)]
struct PostRuntime {
    switches: PostSwitches,
    requested: Option<String>,
    task: Option<Task<Result<LevelAtmosphere, String>>>,
    atmosphere: Option<LevelAtmosphere>,
    blender: PostBlender,
    lut_image: Option<Handle<Image>>,
    lut_key: Option<(ColorTransform, Option<String>)>,
    overlays_fixed: Vec<Entity>,
    /// Cameras that carry this plugin's components ([`PostFx`], bloom,
    /// tonemapping off), so that they can be handed back.
    applied: Vec<Entity>,
    /// Of those, the cameras whose Bevy tonemapping is switched off.
    tonemapped: Vec<Entity>,
}

fn start_loading(mut rt: ResMut<PostRuntime>, level: Option<Res<ConvertedLevel>>) {
    let Some(level) = level else {
        return;
    };
    if rt.requested.as_deref() == Some(level.level.as_str()) {
        return;
    }
    rt.requested = Some(level.level.clone());
    let dir = level.dir.clone();
    let name = level.level.clone();
    rt.task = Some(AsyncComputeTaskPool::get().spawn(async move { load(&dir, &name) }));
}

fn poll_loading(mut rt: ResMut<PostRuntime>) {
    let Some(task) = rt.task.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    rt.task = None;
    match result {
        Ok(a) => {
            info!(
                "atmosphere of {}: height fog {}, {} post-process volumes, uber effect {}, \
                 LUTs {:?}",
                a.level,
                if a.fog.is_some() { "on" } else { "none" },
                a.volumes.len(),
                if a.uber.is_some() { "on" } else { "none" },
                a.luts.keys().collect::<Vec<_>>()
            );
            for n in &a.notes {
                info!("atmosphere: {n}");
            }
            // A new level snaps to its settings unless the previous one
            // asked to persist its post-process settings (UE3 then blends
            // from the old settings with the new durations).
            let persist = rt.atmosphere.as_ref().and_then(|p| p.persist);
            if persist != Some(true) {
                rt.blender.reset();
            }
            rt.lut_key = None;
            rt.atmosphere = Some(a);
        }
        Err(e) => {
            warn!("atmosphere: {e}; the level renders with Bevy's default post-processing");
            rt.atmosphere = None;
        }
    }
}

/// Loads the scene (and its converted streaming sub-levels) and the LUT
/// textures it references.
fn load(dir: &ConvertedDir, level: &str) -> Result<LevelAtmosphere, String> {
    let mut scene = dir.load_scene(level).map_err(|e| e.to_string())?;
    let mut notes = Vec::new();
    for s in scene.streaming_levels.clone() {
        if let Ok(sub) = dir.load_scene(&s.package) {
            scene.merge_sublevel(sub, s.offset);
        }
    }
    let a = &scene.atmosphere;
    if let Some(e) = &a.block_error {
        notes.push(format!(
            "the scene's atmosphere block was ignored ({e}); re-run `asamu-import levels`"
        ));
    } else if a.block_version.is_none() {
        notes.push(
            "the scene has no atmosphere block (converted before it existed): no height fog \
             or tonemapper data; re-run `asamu-import levels`"
                .to_owned(),
        );
    }
    let world = a
        .world_post_process
        .as_ref()
        .map(PostSettings::from_props)
        .unwrap_or_default();
    let volumes: Vec<(PostProcessVolumeInfo, PostSettings)> = a
        .volumes_by_priority()
        .into_iter()
        .map(|v| (v.clone(), PostSettings::from_props(&v.settings)))
        .collect();
    // UE3 sets up the first exponential height fog of the scene.
    let fog = a.height_fogs.iter().find_map(HeightFog::from_component);
    // The light that colours the fog: a dominant directional light first,
    // else any directional light (TENTATIVE, see the doc).
    let directional = |dominant: bool| {
        scene.lights.iter().find(|l| {
            l.enabled
                && l.kind == UeLightKind::Directional
                && l.light_class.to_ascii_lowercase().contains("dominant") == dominant
        })
    };
    let fog_light_ue = directional(true)
        .or_else(|| directional(false))
        .map(|l| l.direction_ue)
        .filter(|d| d.is_finite() && d.length() > 0.5);
    let uber = a.post_process_chain.as_ref().and_then(|c| {
        c.effects
            .iter()
            .filter(|e| e.class.ends_with("UberPostProcessEffect"))
            .map(|e| UberEffect::from_props(&e.params))
            .find(|u| u.show_in_game)
    });
    // LUT textures referenced by the world settings and the volumes.
    let mut luts = BTreeMap::new();
    let wanted: Vec<String> = std::iter::once(world.color_grading_lut.clone())
        .chain(volumes.iter().map(|(_, s)| s.color_grading_lut.clone()))
        .flatten()
        .collect();
    if !wanted.is_empty() {
        match dir.load_manifests() {
            Ok(m) => {
                for path in wanted {
                    let key = path.to_ascii_lowercase();
                    if luts.contains_key(&key) {
                        continue;
                    }
                    match read_lut(dir, m.textures.as_ref(), &path) {
                        Ok(lut) => {
                            luts.insert(key, lut);
                        }
                        Err(e) => notes.push(format!("LUT {path}: {e} (graded without it)")),
                    }
                }
            }
            Err(e) => notes.push(format!("texture manifest: {e}")),
        }
    }
    Ok(LevelAtmosphere {
        level: scene.package.clone(),
        world,
        persist: a.persist_post_process,
        volumes,
        fog,
        fog_light_ue,
        uber,
        luts,
        notes,
    })
}

/// Largest LUT file read (a 256 × 16 32-bit DDS with every mip is about
/// 22 KiB; anything far larger is not a LUT and is not read into memory).
const MAX_LUT_FILE_BYTES: u64 = 1 << 20;

/// Reads at most `max` bytes of `path`; a larger file is refused.
fn read_capped(path: &std::path::Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut data = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() as u64 > max {
        return Err(format!("{}: larger than {max} bytes", path.display()));
    }
    Ok(data)
}

fn read_lut(
    dir: &ConvertedDir,
    textures: Option<&asamu_assets::manifest::TextureManifest>,
    path: &str,
) -> Result<Lut, String> {
    let manifest = textures.ok_or("textures are not converted (asamu-import textures)")?;
    let (_, entry) = manifest
        .get(path)
        .ok_or("not in the texture manifest (asamu-import textures)")?;
    let file = entry.safe_file().map_err(|e| e.to_string())?;
    let full = dir.root().join("textures").join(file);
    let data = read_capped(&full, MAX_LUT_FILE_BYTES)?;
    let (w, h, rgba) = dds::read_rgba8(&data).ok_or("not an uncompressed 32-bit DDS")?;
    Lut::from_rgba8(w, h, &rgba).ok_or_else(|| format!("{w}×{h} is not a 256×16 LUT"))
}

/// Mip levels of Bevy's bloom chain at its default maximum mip size (512:
/// `log2(512) − 1`).
const BLOOM_MIPS: u32 = 8;

/// Total gain of Bevy's additive bloom: the main texture receives
/// `b₀·(m₀ + b₁·(m₁ + b₂·(…)))`, `bᵢ` being Bevy's per-mip blend factor
/// (`compute_blend_factor` in `bevy_post_process`, mirrored here) and `mᵢ`
/// the blurred mips, which carry about the same energy; the gain is the sum
/// of the products.
#[must_use]
pub fn bloom_total_gain(b: &Bloom, mips: u32) -> f32 {
    let max_mip = mips.saturating_sub(1).max(1) as f32;
    let factor = |mip: u32| {
        let t = mip as f32 / max_mip;
        let lf = (1.0 - (1.0 - t).powf(1.0 / (1.0 - b.low_frequency_boost_curvature)))
            * b.low_frequency_boost
            * match b.composite_mode {
                BloomCompositeMode::EnergyConserving => 1.0 - b.intensity,
                BloomCompositeMode::Additive => 1.0,
            };
        let high_pass = 1.0 - ((t - b.high_pass_frequency) / b.high_pass_frequency).clamp(0.0, 1.0);
        (b.intensity + lf) * high_pass
    };
    let mut total = 0.0;
    let mut product = 1.0;
    for mip in 0..mips {
        product *= factor(mip);
        total += product;
    }
    total
}

/// Bevy bloom settings that produce the original's "blurred bright parts"
/// (OUR APPROXIMATION, not an original constant): additive, Bevy's default
/// low-frequency boost (the original weighs its widest blur most), the
/// prefilter threshold at `Bloom_Threshold` (in render units), and the
/// intensity solved so that the total gain ([`bloom_total_gain`]) is
/// `gain` — 1 for the uber pass, which recovers the bloom as a difference
/// and applies tint, scale and the bright-pixel fade itself (the original
/// normalises its blur weights to 1). Not reproduced: the original's
/// three-size kernel and its soft threshold ramp.
#[must_use]
pub fn bevy_bloom(gain: f32, threshold_render: f32) -> Bloom {
    let target = gain.max(0.0);
    let mut b = Bloom {
        intensity: 0.0,
        composite_mode: BloomCompositeMode::Additive,
        prefilter: BloomPrefilter {
            threshold: threshold_render.max(0.0),
            threshold_softness: 0.0,
        },
        ..Bloom::NATURAL
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..32 {
        let mid = 0.5 * (lo + hi);
        b.intensity = mid;
        if bloom_total_gain(&b, BLOOM_MIPS) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    b.intensity = lo;
    b
}

/// Render radiance per UE3 scene-colour unit (OUR APPROXIMATION): a white
/// surface lit by a UE3 light of brightness 1 renders, with the app's light
/// mapping (`directional_lux_per_brightness` lux per unit brightness,
/// lightmaps scaled the same way) and the camera exposure, at
/// `lux · exposure / π`; UE3 shows it at about 1.
#[must_use]
pub fn render_per_ue(directional_lux_per_brightness: f32, exposure: f32) -> f32 {
    let v = directional_lux_per_brightness * exposure / std::f32::consts::PI;
    if v.is_finite() && v > 0.0 { v } else { 1.0 }
}

/// The per-camera uniform for this frame (camera matrix filled at
/// extraction).
fn uniform_for(
    a: &LevelAtmosphere,
    uber: Option<&UberParams>,
    camera_ue: sim_glam::Vec3,
    scale: f32,
    fog_on: bool,
    lut_on: bool,
) -> PostFxUniform {
    let uu_per_unit = 1.0 / crate::SCALE.bevy_units_per_uu;
    let mut u = PostFxUniform {
        camera: Vec4::new(0.0, 0.0, 0.0, uu_per_unit),
        grade: Vec4::new(1.0 / scale, 0.0, 0.0, 0.0),
        ..PostFxUniform::default()
    };
    if let Some(f) = a.fog.filter(|_| fog_on) {
        // Towards the light, in render axes; (0, 0, 1) in UE3 without a
        // light (the native default).
        let toward = a
            .fog_light_ue
            .map(|d| -ue_dir_to_bevy(d).normalize_or_zero())
            .filter(|d| d.length() > 0.5)
            .unwrap_or(ue_dir_to_bevy(sim_glam::Vec3::Z));
        let opp = f.opposite_color * scale;
        let ins = f.inscattering_color * scale;
        u.fog = Vec4::new(
            f.density_at(camera_ue.z),
            f.falloff,
            f.terminator_exponent,
            f.start_distance,
        );
        u.fog_opposite = Vec4::new(opp.x, opp.y, opp.z, f.min_transmittance);
        u.fog_inscatter = Vec4::new(ins.x, ins.y, ins.z, 1.0);
        u.fog_light = Vec4::new(toward.x, toward.y, toward.z, EMPTY_PIXEL_DISTANCE_UU);
    }
    if let Some(p) = uber {
        let t = &p.tonemapper;
        u.tonemap = Vec4::new(t.a, t.b, t.crossover, t.scale);
        let kind = match t.kind {
            TonemapperType::Off => 0.0,
            TonemapperType::Filmic => 1.0,
            TonemapperType::Customizable => 2.0,
        };
        u.tonemap2 = Vec4::new(t.toe, kind, if lut_on { 1.0 } else { 0.0 }, 0.0);
        let tint = p.bloom_tint * p.bloom_scale;
        u.bloom = Vec4::new(tint.x, tint.y, tint.z, p.bloom_screen_blend_threshold);
    }
    u
}

fn new_lut_image(lut: &Lut) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: 256,
            height: 16,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        lut.to_rgba8(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

/// Takes the atmosphere off the cameras it was applied to (no converted
/// level, or a level whose atmosphere is still loading or failed to load,
/// so that another level's fog and grade never linger): the passes stop and
/// Bevy's default tonemapping and dithering come back; the plugin's bloom
/// is removed (the cameras are spawned without bloom).
fn release_cameras(commands: &mut Commands, rt: &mut PostRuntime) {
    release_except(commands, rt, &[]);
}

/// [`release_cameras`] for every camera not in `keep` (a camera that no
/// longer qualifies, or was despawned: then nothing is queued).
fn release_except(commands: &mut Commands, rt: &mut PostRuntime, keep: &[Entity]) {
    for e in rt.applied.iter().filter(|e| !keep.contains(e)) {
        if let Ok(mut ec) = commands.get_entity(*e) {
            ec.try_remove::<(PostFx, Bloom)>();
        }
    }
    for e in rt.tonemapped.iter().filter(|e| !keep.contains(e)) {
        if let Ok(mut ec) = commands.get_entity(*e) {
            ec.try_insert((Tonemapping::default(), DebandDither::default()));
        }
    }
    rt.applied.retain(|e| keep.contains(e));
    rt.tonemapped.retain(|e| keep.contains(e));
}

/// Cameras that get the atmosphere: the player camera and its children
/// that render before it (the offscreen screenshot camera).
#[allow(clippy::type_complexity)]
fn atmosphere_cameras(
    player: &Query<(Entity, &GlobalTransform, Option<&Children>), With<PlayerCamera>>,
    cameras: &Query<&Camera, With<Camera3d>>,
) -> Vec<Entity> {
    let mut out = Vec::new();
    for (e, _, children) in player {
        out.push(e);
        for c in children.into_iter().flatten() {
            if cameras.get(*c).is_ok_and(|cam| cam.order <= 0) {
                out.push(*c);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn apply_atmosphere(
    mut commands: Commands,
    mut rt: ResMut<PostRuntime>,
    time: Res<Time<Real>>,
    level: Option<Res<ConvertedLevel>>,
    player: Query<(Entity, &GlobalTransform, Option<&Children>), With<PlayerCamera>>,
    cameras: Query<&Camera, With<Camera3d>>,
    exposures: Query<&Exposure>,
    mut camera3d: Query<&mut Camera3d>,
    has_fog: Query<(), With<DistanceFog>>,
    mut images: ResMut<Assets<Image>>,
) {
    let rt = &mut *rt;
    let Some(level) = level else {
        release_cameras(&mut commands, rt);
        return;
    };
    let Some(a) = rt
        .atmosphere
        .as_ref()
        .filter(|a| a.level.eq_ignore_ascii_case(&level.level))
    else {
        release_cameras(&mut commands, rt);
        return;
    };
    let Some((player_entity, transform, _)) = player.iter().next() else {
        return;
    };
    let camera_ue = bevy_pos_to_ue(
        sim_glam::Vec3::from_array(transform.translation().to_array()),
        crate::SCALE,
    );
    // Post-process settings: world defaults, the volume at the camera,
    // UE3's time blend.
    let pairs: Vec<(&PostProcessVolumeInfo, PostSettings)> =
        a.volumes.iter().map(|(v, s)| (v, s.clone())).collect();
    let (desired, volume) = ue3::desired_settings(&a.world, &pairs, camera_ue);
    let settings = rt
        .blender
        .update(&desired, volume, time.elapsed_secs())
        .clone();
    let sw = rt.switches;
    let uber = a
        .uber
        .as_ref()
        .filter(|_| sw.tonemap)
        .map(|e| ue3::uber_params(e, Some(&settings)));
    // The graded LUT, rebuilt when the transform or the texture changes.
    let lut_on = uber.is_some() && sw.lut;
    if let Some(p) = &uber {
        let key = (p.transform, p.color_grading_lut.clone());
        if rt.lut_key.as_ref() != Some(&key) {
            let source = p
                .color_grading_lut
                .as_ref()
                .and_then(|path| a.luts.get(&path.to_ascii_lowercase()));
            let lut = Lut::graded(source, &p.transform);
            let updated = match rt.lut_image.as_ref().and_then(|h| images.get_mut(h)) {
                Some(mut image) => {
                    image.data = Some(lut.to_rgba8());
                    true
                }
                None => false,
            };
            if !updated {
                rt.lut_image = Some(images.add(new_lut_image(&lut)));
            }
            rt.lut_key = Some(key);
        }
    }
    let exposure = exposures
        .get(player_entity)
        .copied()
        .unwrap_or_default()
        .exposure();
    let scale = render_per_ue(
        level.options.lights.directional_lux_per_brightness,
        exposure,
    );
    let fog_on = sw.fog && level.settings.fog && a.fog.is_some();
    let uniform = uniform_for(a, uber.as_ref(), camera_ue, scale, fog_on, lut_on);
    let lut_handle = rt.lut_image.clone().unwrap_or_default();
    let targets = atmosphere_cameras(&player, &cameras);
    release_except(&mut commands, rt, &targets);
    for entity in targets {
        if !rt.applied.contains(&entity) {
            rt.applied.push(entity);
        }
        let mut e = commands.entity(entity);
        e.insert((
            Hdr,
            PostFx {
                uniform,
                fog: fog_on,
                uber: uber.is_some(),
                lut: lut_handle.clone(),
            },
        ));
        if uber.is_some() {
            // The uber pass tonemaps; Bevy's dithering only runs inside its
            // own tonemapping pass.
            e.insert((Tonemapping::None, DebandDither::Disabled));
            if !rt.tonemapped.contains(&entity) {
                rt.tonemapped.push(entity);
            }
        } else if let Some(i) = rt.tonemapped.iter().position(|t| *t == entity) {
            // No uber effect for this level (or `no-tonemap`): Bevy's
            // tonemapping again.
            rt.tonemapped.swap_remove(i);
            e.insert((Tonemapping::default(), DebandDither::default()));
        }
        // The height fog pass (or the level's lack of height fog) replaces
        // the placeholder distance fog.
        if has_fog.contains(entity) {
            e.remove::<DistanceFog>();
        }
        match uber
            .as_ref()
            .filter(|p| sw.bloom && settings.enable_bloom && p.bloom_scale > 0.0)
        {
            Some(p) => {
                e.insert(bevy_bloom(1.0, p.bloom_threshold * scale));
            }
            None => {
                e.remove::<Bloom>();
            }
        }
        // The fog pass reads the depth buffer (it also runs, as a copy,
        // without fog).
        if let Ok(mut c3) = camera3d.get_mut(entity) {
            let usages = TextureUsages::from(c3.depth_texture_usages);
            if !usages.contains(TextureUsages::TEXTURE_BINDING) {
                c3.depth_texture_usages = (usages | TextureUsages::TEXTURE_BINDING).into();
            }
        }
    }
}

/// Overlay cameras (the first-person hands, drawn after the player camera
/// into the same window without clearing) rely on sharing the player
/// camera's intermediate texture, which stops once the player camera
/// renders in HDR. They are switched to clear to transparent and blend
/// their output over the window (premultiplied alpha) instead.
#[allow(clippy::type_complexity)]
fn match_overlay_cameras(
    mut rt: ResMut<PostRuntime>,
    player: Query<&Children, (With<PlayerCamera>, With<Hdr>)>,
    mut cameras: Query<&mut Camera, (With<Camera3d>, Without<Hdr>)>,
) {
    for children in &player {
        for child in children {
            if rt.overlays_fixed.contains(child) {
                continue;
            }
            let Ok(mut cam) = cameras.get_mut(*child) else {
                continue;
            };
            if cam.order <= 0 || !matches!(cam.clear_color, ClearColorConfig::None) {
                continue;
            }
            cam.clear_color = ClearColorConfig::Custom(Color::NONE);
            cam.output_mode = CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            };
            rt.overlays_fixed.push(*child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_gain_and_mapping() {
        // Without low-frequency boost the gain is the geometric series.
        let plain = Bloom {
            intensity: 0.5,
            low_frequency_boost: 0.0,
            composite_mode: BloomCompositeMode::Additive,
            ..Bloom::NATURAL
        };
        let expect: f32 = (1..=8).map(|k| 0.5f32.powi(k)).sum();
        assert!((bloom_total_gain(&plain, 8) - expect).abs() < 1e-6);
        // The mapping reaches the target gain, monotonically.
        for gain in [0.1, 0.3, 0.8, 1.0] {
            let b = bevy_bloom(gain, 2.0);
            assert!(
                (bloom_total_gain(&b, BLOOM_MIPS) - gain).abs() < 1e-3,
                "{gain}"
            );
            assert_eq!(b.composite_mode, BloomCompositeMode::Additive);
            assert_eq!(b.prefilter.threshold, 2.0);
        }
        assert!(bevy_bloom(0.8, 0.0).intensity > bevy_bloom(0.3, 0.0).intensity);
        assert_eq!(bevy_bloom(0.0, 0.0).intensity, 0.0);
        assert_eq!(bevy_bloom(-1.0, -1.0).prefilter.threshold, 0.0);
    }

    #[test]
    fn switches_parse() {
        assert_eq!(PostSwitches::parse(""), PostSwitches::default());
        assert!(!PostSwitches::parse("OFF").enabled);
        assert!(!PostSwitches::parse("0").enabled);
        let s = PostSwitches::parse("no-fog, no-LUT,unknown");
        assert!(s.enabled && !s.fog && !s.lut && s.bloom && s.tonemap);
        assert!(!PostSwitches::parse("no-bloom,no-tonemap").tonemap);
    }

    #[test]
    fn render_scale_follows_light_mapping_and_exposure() {
        let e = Exposure::default().exposure();
        let s = render_per_ue(10_000.0, e);
        assert!((s - 10_000.0 * e / std::f32::consts::PI).abs() < 1e-6);
        assert_eq!(render_per_ue(0.0, e), 1.0);
        assert_eq!(render_per_ue(f32::NAN, e), 1.0);
    }

    #[test]
    fn uniform_packs_fog_and_tonemapper() {
        let fog = HeightFog {
            height: 0.0,
            density: 2e-5,
            falloff: 2e-4,
            min_transmittance: 0.25,
            start_distance: 1024.0,
            terminator_exponent: 0.2,
            opposite_color: sim_glam::Vec3::ONE,
            inscattering_color: sim_glam::Vec3::new(2.0, 1.0, 0.0),
        };
        let a = LevelAtmosphere {
            level: "L".to_owned(),
            world: PostSettings::default(),
            persist: None,
            volumes: Vec::new(),
            fog: Some(fog),
            // Light shining straight down (UE3 −Z): towards it is render +Y.
            fog_light_ue: Some(sim_glam::Vec3::NEG_Z),
            uber: Some(UberEffect::default()),
            luts: BTreeMap::new(),
            notes: Vec::new(),
        };
        let p = ue3::uber_params(&UberEffect::default(), Some(&a.world));
        let u = uniform_for(
            &a,
            Some(&p),
            sim_glam::Vec3::new(0.0, 0.0, 5000.0),
            2.0,
            true,
            true,
        );
        assert!((u.fog.x - fog.density_at(5000.0)).abs() < 1e-12);
        assert_eq!(u.fog.w, 1024.0);
        assert_eq!(u.fog_opposite, Vec4::new(2.0, 2.0, 2.0, 0.25));
        assert_eq!(u.fog_inscatter, Vec4::new(4.0, 2.0, 0.0, 1.0));
        assert!((u.fog_light.truncate() - Vec3::Y).length() < 1e-6);
        assert_eq!(u.grade.x, 0.5);
        assert_eq!(u.camera.w, 1.0 / crate::SCALE.bevy_units_per_uu);
        assert_eq!(u.tonemap2.y, 0.0, "class default tonemapper: off");
        assert_eq!(u.tonemap2.z, 1.0);
        // Bloom: tint · scale and the screen-blend threshold (class
        // defaults of the effect: white, 1, 10).
        assert_eq!(u.bloom, Vec4::new(1.0, 1.0, 1.0, 10.0));
        // Fog off: the pass flag is 0 and no colours are set.
        let off = uniform_for(&a, None, sim_glam::Vec3::ZERO, 1.0, false, false);
        assert_eq!(off.fog_inscatter.w, 0.0);
        assert_eq!(off.tonemap2, Vec4::ZERO);
        // Without a light the fog colours toward UE3 +Z (render +Y).
        let no_light = LevelAtmosphere {
            fog_light_ue: None,
            ..a.clone()
        };
        let u = uniform_for(&no_light, None, sim_glam::Vec3::ZERO, 1.0, true, false);
        assert!((u.fog_light.truncate() - Vec3::Y).length() < 1e-6);
    }

    #[test]
    fn lut_reads_are_capped() {
        let dir = std::env::temp_dir().join(format!("asamu-post-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lut.dds");
        std::fs::write(&file, vec![7u8; 100]).unwrap();
        assert_eq!(read_capped(&file, 100).unwrap().len(), 100);
        assert!(
            read_capped(&file, 99)
                .unwrap_err()
                .contains("larger than 99")
        );
        assert!(read_capped(&dir.join("missing.dds"), 100).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A camera the atmosphere no longer applies to (other level, failed or
    /// pending load, or no longer a target) loses the passes and the
    /// plugin's bloom and gets Bevy's tonemapping back; kept cameras and
    /// despawned ones are left alone.
    #[test]
    fn released_cameras_get_bevys_defaults_back() {
        use bevy::ecs::world::CommandQueue;
        let mut world = World::new();
        let cam = world
            .spawn((
                PostFx::default(),
                Bloom::NATURAL,
                Tonemapping::None,
                DebandDither::Disabled,
            ))
            .id();
        let keep = world
            .spawn((PostFx::default(), Tonemapping::None, DebandDither::Disabled))
            .id();
        let gone = world.spawn_empty().id();
        assert!(world.despawn(gone));
        let mut rt = PostRuntime {
            applied: vec![cam, keep, gone],
            tonemapped: vec![cam, keep, gone],
            ..PostRuntime::default()
        };
        let mut queue = CommandQueue::default();
        release_except(&mut Commands::new(&mut queue, &world), &mut rt, &[keep]);
        queue.apply(&mut world);
        assert!(world.get::<PostFx>(cam).is_none());
        assert!(world.get::<Bloom>(cam).is_none());
        assert_eq!(world.get::<Tonemapping>(cam), Some(&Tonemapping::default()));
        assert_eq!(
            world.get::<DebandDither>(cam),
            Some(&DebandDither::default())
        );
        assert!(world.get::<PostFx>(keep).is_some());
        assert_eq!(world.get::<Tonemapping>(keep), Some(&Tonemapping::None));
        assert_eq!(
            (rt.applied.as_slice(), rt.tonemapped.as_slice()),
            (&[keep][..], &[keep][..])
        );
        // Everything goes when no level atmosphere applies.
        release_cameras(&mut Commands::new(&mut queue, &world), &mut rt);
        queue.apply(&mut world);
        assert!(world.get::<PostFx>(keep).is_none());
        assert_eq!(
            world.get::<Tonemapping>(keep),
            Some(&Tonemapping::default())
        );
        assert!(rt.applied.is_empty() && rt.tonemapped.is_empty());
    }

    #[test]
    fn lut_images_have_the_ue3_layout() {
        let image = new_lut_image(&Lut::neutral());
        assert_eq!(image.width(), 256);
        assert_eq!(image.height(), 16);
        assert_eq!(image.data.as_ref().map(Vec::len), Some(256 * 16 * 4));
    }
}
