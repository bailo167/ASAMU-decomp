//! Baked lighting for converted levels: the original's light maps on the
//! level geometry (APPROXIMATION, see below and
//! `docs/reverse-engineering/LIGHTMAPS.md`).
//!
//! Once `converted.rs` has spawned a level, this plugin
//!
//! 1. rebuilds the same deterministic [`LevelPlan`] on the async pool, loads
//!    `lightmaps/<map>.lightmaps.json` for the level and its merged
//!    sub-levels ([`LevelLightmaps`]) and pairs every draw with its UE3
//!    component (actor, component name) and every render light with its UE3
//!    light;
//! 2. matches the spawned [`LevelEntity`] draws by actor slot, mesh
//!    primitive and exact translation, and gives each light-mapped draw a
//!    Bevy [`Lightmap`] (the importer's irradiance atlas plus the
//!    component's UV rectangle). Meshes whose light map lives in UV channel
//!    0, and vertex light maps (a constant per component), get a derived
//!    mesh with a second UV set, because Bevy samples light maps through
//!    `UV_1`;
//! 3. sets `lightmap_exposure` on the materials of light-mapped draws
//!    ([`lightmap_exposure`]), so baked and dynamic light share one scale;
//! 4. stops lights that the original baked into its light maps from also
//!    lighting light-mapped surfaces (`affects_lightmapped_mesh_diffuse =
//!    false`): in UE3 a static light baked into a light map does not render
//!    dynamically on those primitives. Shadow-mapped (dominant) and dynamic
//!    lights stay as they are. The constant ambient term, which only stood
//!    in for the missing baked light, no longer reaches light-mapped
//!    surfaces;
//! 5. replaces the flat level BSP with BSP meshes that carry light map
//!    coordinates (the original's own render vertex buffer), using the same
//!    materials. Only when the light map files cover the whole flat BSP of
//!    the level and its merged sub-levels (same triangle count): surfaces the
//!    original drew without a light map are part of the replacement, drawn
//!    without one; a light map file from an importer that left them out
//!    keeps the flat BSP.
//!
//! Approximation: the original renders directional light maps (two
//! coefficient textures combined with the per-pixel normal); the importer
//! reduces them to one irradiance value per texel, i.e. what an unperturbed
//! normal would receive, so normal-mapped detail in the baked lighting is
//! lost. Bevy adds the light map as indirect diffuse light (times the
//! material's diffuse colour), which is how UE3 applies it too.
//!
//! Disable with `ASAMU_LIGHTMAPS=0` (or [`LightmapSettings::enabled`]).

use std::collections::HashMap;

use asamu_assets::lightmaps::{
    BspLightmapMesh, LevelLightmaps, LightBaking, LightmapKind, lightmap_exposure,
};
use asamu_assets::{LevelPlan, LightMapping, RenderLight, RenderLightKind};
use asamu_core::coords::ue_dir_to_bevy;
use asamu_core::glam::Vec3 as UeVec3;
use bevy::asset::{AssetPath, RenderAssetUsages};
use bevy::image::{ImageLoaderSettings, ImageSampler};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::pbr::Lightmap;
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::converted::{ConvertedLevel, LevelBsp, LevelEntity, LevelLight, LevelPhase, SOURCE};
use crate::{bevy_vec, to_render};

/// Baked lighting settings.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct LightmapSettings {
    /// Apply light maps at all (`ASAMU_LIGHTMAPS=0` turns this off).
    pub enabled: bool,
    /// Replace the level BSP with light-mapped BSP meshes.
    pub bsp: bool,
    /// Bevy's bicubic light map filtering (smoother, slightly slower).
    pub bicubic: bool,
}

impl Default for LightmapSettings {
    fn default() -> Self {
        let enabled = std::env::var("ASAMU_LIGHTMAPS").map_or(true, |v| v != "0");
        Self {
            enabled,
            bsp: true,
            bicubic: false,
        }
    }
}

/// Status of the baked lighting for the HUD / log.
#[derive(Resource, Debug, Clone, Default)]
pub struct LightmapStatus {
    /// One line.
    pub line: String,
}

/// Marker: this draw has been considered for a light map.
#[derive(Component, Debug, Clone, Copy)]
pub struct LightmapHandled;

/// Marker on the light-mapped BSP meshes spawned by this plugin.
#[derive(Component, Debug, Clone, Copy)]
pub struct LightmappedBsp;

/// Identity of a spawned draw: actor slot, glTF file (relative to
/// `meshes/`), primitive index and translation bits.
type DrawKey = (usize, String, usize, [u32; 3]);

/// What one draw gets.
#[derive(Debug, Clone)]
struct Assignment {
    image: String,
    uv_rect: [f32; 4],
    kind: LightmapKind,
    uv_channel: u32,
}

/// The light-map half of a BSP draw, to recognise the spawned flat BSP
/// meshes: material path, vertex count, first position.
type BspSignature = (Option<String>, usize, [u32; 3]);

#[derive(Debug, Default)]
struct Prepared {
    draws: HashMap<DrawKey, Vec<Assignment>>,
    /// Light identity ([`light_key`]) → baking.
    lights: HashMap<LightKey, LightBaking>,
    bsp: Vec<BspLightmapMesh>,
    /// Triangles of the flat BSP (all merged levels) and of `bsp`.
    bsp_triangles: (usize, usize),
    bsp_signatures: Vec<BspSignature>,
    exposure: f32,
    components: usize,
    draws_total: usize,
    packages: Vec<String>,
    missing: Vec<String>,
    rejected: usize,
    bsp_dropped: usize,
}

#[derive(Resource, Default)]
enum LightmapState {
    #[default]
    Idle,
    Planning(Task<Result<Prepared, String>>),
    Applying(Box<Applying>),
    Done,
    Off,
}

/// Assets of the current level held by this plugin, kept after it finishes
/// and released when another level loads: the atlas images (to drop light
/// maps whose image fails to load) and the source meshes whose draws now use
/// a derived copy with a second UV set. The level tracks its assets by id
/// (`converted.rs`), so a source mesh must stay loaded after its last draw
/// moved to the copy; otherwise its load state disappears and the level never
/// counts as settled.
#[derive(Resource, Default)]
struct LightmapImages {
    handles: HashMap<String, Handle<Image>>,
    failed: Vec<AssetId<Image>>,
    source_meshes: Vec<Handle<Mesh>>,
}

#[derive(Default)]
struct Applying {
    prep: Prepared,
    /// Draws waiting for their mesh asset.
    pending: Vec<(Entity, Handle<Mesh>, Assignment)>,
    /// Source mesh → mesh with a second UV set.
    derived: HashMap<AssetId<Mesh>, Handle<Mesh>>,
    materials_done: Vec<AssetId<StandardMaterial>>,
    lit: usize,
    constant: usize,
    unmatched: usize,
    no_uv: usize,
    lights_matched: bool,
    bsp_done: bool,
    frames_waiting: u32,
}

/// Baked light map rendering for converted levels.
pub struct LightmapPlugin;

impl Plugin for LightmapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightmapSettings>()
            .init_resource::<LightmapStatus>()
            .init_resource::<LightmapState>()
            .init_resource::<LightmapImages>()
            .add_systems(
                Update,
                (
                    reset_on_level_change,
                    start_planning,
                    poll_planning,
                    assign_draws,
                    resolve_pending,
                    adjust_lights,
                    swap_bsp,
                    drop_failed_images,
                )
                    .chain(),
            );
    }
}

/// A spawned light's identity: point and spot lights by their exact
/// translation, directional lights (spawned at the origin) by their exact
/// rotation; plus the kind.
type LightKey = ([u32; 4], u8);

fn light_key_point(translation: Vec3, tag: u8) -> LightKey {
    let [x, y, z] = translation_bits(translation);
    ([x, y, z, 0], tag)
}

fn light_key_dir(rotation: Quat) -> LightKey {
    let r = rotation.to_array();
    (
        [
            r[0].to_bits(),
            r[1].to_bits(),
            r[2].to_bits(),
            r[3].to_bits(),
        ],
        2,
    )
}

fn translation_bits(v: Vec3) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

fn light_tag(k: &RenderLightKind) -> u8 {
    match k {
        RenderLightKind::Point { .. } => 0,
        RenderLightKind::Spot { .. } => 1,
        RenderLightKind::Directional { .. } => 2,
        RenderLightKind::Ambient { .. } => 3,
    }
}

/// Pairs the plan with the light map files (runs on the async pool).
fn prepare(plan: &LevelPlan, lm: &LevelLightmaps, mapping: &LightMapping) -> Prepared {
    let scene = &plan.scene;
    let package_of = |level: usize| -> &str {
        if level == 0 {
            &scene.package
        } else {
            scene
                .merged_levels
                .get(level - 1)
                .map_or("", String::as_str)
        }
    };
    let mut prep = Prepared {
        exposure: lightmap_exposure(mapping),
        components: lm.component_count(),
        draws_total: plan.draws.len(),
        packages: lm.packages.clone(),
        missing: lm.missing.clone(),
        rejected: lm.rejected,
        ..Prepared::default()
    };
    for d in &plan.draws {
        let (Some(inst), Some(prim)) = (
            scene.meshes.get(d.instance),
            plan.primitives.get(d.primitive),
        ) else {
            continue;
        };
        let Some(c) = lm.component(
            package_of(inst.level),
            &inst.actor_name,
            &inst.component_name,
        ) else {
            continue;
        };
        let key = (
            d.actor_slot,
            prim.gltf.clone(),
            prim.primitive,
            translation_bits(bevy_vec(d.transform.translation)),
        );
        prep.draws.entry(key).or_default().push(Assignment {
            image: c.image.clone(),
            uv_rect: c.uv_rect,
            kind: c.kind,
            uv_channel: c.uv_channel,
        });
    }
    // Lights in the plan's order: the same filter as `LevelPlan::build`.
    for l in &scene.lights {
        let Some(r): Option<RenderLight> =
            asamu_assets::lighting::map_light(l, mapping, plan.scale)
        else {
            continue;
        };
        if matches!(r.kind, RenderLightKind::Ambient { .. }) {
            continue;
        }
        if let Some(b) = lm.light_baked(package_of(l.level), &l.actor_name) {
            let key = if matches!(r.kind, RenderLightKind::Directional { .. }) {
                let dir = bevy_vec(ue_dir_to_bevy(r.direction_ue));
                light_key_dir(Transform::default().looking_to(dir, Vec3::Y).rotation)
            } else {
                light_key_point(to_render(r.location_ue), light_tag(&r.kind))
            };
            prep.lights.insert(key, b);
        }
    }
    // Light-mapped BSP of the persistent level and every merged sub-level
    // (moved by its streaming offset, as `LevelPlan::load` moves the flat
    // BSP).
    let levels = std::iter::once((scene.package.as_str(), UeVec3::ZERO)).chain(
        scene.merged_levels.iter().map(|name| {
            let offset = scene
                .streaming_levels
                .iter()
                .find(|s| s.package.eq_ignore_ascii_case(name))
                .map_or(UeVec3::ZERO, |s| s.offset);
            (name.as_str(), offset)
        }),
    );
    for (package, offset) in levels {
        if !lm.has_bsp(package) {
            continue;
        }
        match lm.bsp_meshes(package, offset, plan.scale) {
            Ok((meshes, dropped)) => {
                prep.bsp.extend(meshes);
                prep.bsp_dropped += dropped;
            }
            Err(e) => warn!("light-mapped BSP of {package}: {e}"),
        }
    }
    prep.bsp_triangles = (
        plan.bsp.iter().map(|b| b.mesh.indices.len() / 3).sum(),
        prep.bsp.iter().map(|m| m.indices.len() / 3).sum(),
    );
    prep.bsp_signatures = plan
        .bsp
        .iter()
        .map(|b| {
            let first = b.mesh.positions.first().copied().unwrap_or([0.0; 3]);
            (
                b.mesh.material.clone(),
                b.mesh.positions.len(),
                translation_bits(Vec3::from_array(first)),
            )
        })
        .collect();
    prep
}

/// Starts over when another level is planned (chapter select, story flow):
/// the prepared pairs, light keys and BSP belong to the previous level, and
/// the new one needs its own light maps.
fn reset_on_level_change(
    level: Option<Res<ConvertedLevel>>,
    mut state: ResMut<LightmapState>,
    mut images: ResMut<LightmapImages>,
    mut status: ResMut<LightmapStatus>,
    mut current: Local<Option<String>>,
) {
    let Some(level) = level else {
        return;
    };
    let replanning = matches!(level.phase, Some(LevelPhase::Planning(_)));
    let renamed = current
        .as_deref()
        .is_some_and(|c| !c.eq_ignore_ascii_case(&level.level));
    if needs_reset(&state, replanning || renamed) {
        info!("lightmaps: level changed to {}; starting over", level.level);
        *state = LightmapState::Idle;
        *images = LightmapImages::default();
        status.line.clear();
    }
    if current.as_deref() != Some(level.level.as_str()) {
        *current = Some(level.level.clone());
    }
}

/// Whether a level change must reset `state` (an idle or disabled plugin has
/// nothing to drop).
fn needs_reset(state: &LightmapState, level_changed: bool) -> bool {
    level_changed && !matches!(state, LightmapState::Idle | LightmapState::Off)
}

fn start_planning(
    settings: Res<LightmapSettings>,
    level: Option<Res<ConvertedLevel>>,
    mut state: ResMut<LightmapState>,
    mut status: ResMut<LightmapStatus>,
) {
    if !matches!(*state, LightmapState::Idle) {
        return;
    }
    let Some(level) = level else {
        return;
    };
    if !settings.enabled {
        status.line = "lightmaps off".to_owned();
        *state = LightmapState::Off;
        return;
    }
    if !matches!(level.phase, Some(LevelPhase::Spawned(_))) {
        return;
    }
    let dir = level.dir.clone();
    let name = level.level.clone();
    // The plan must match the spawned one exactly: same options.
    let options = level.options;
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let plan = LevelPlan::load(&dir, &name, &options).map_err(|e| e.to_string())?;
        let mut packages = vec![plan.scene.package.clone()];
        packages.extend(plan.scene.merged_levels.iter().cloned());
        let lm = LevelLightmaps::load(&dir, &packages).map_err(|e| e.to_string())?;
        Ok(prepare(&plan, &lm, &options.lights))
    });
    status.line = "lightmaps: preparing".to_owned();
    *state = LightmapState::Planning(task);
}

fn poll_planning(mut state: ResMut<LightmapState>, mut status: ResMut<LightmapStatus>) {
    let LightmapState::Planning(task) = &mut *state else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    match result {
        Ok(prep) => {
            if prep.packages.is_empty() {
                status.line = format!(
                    "lightmaps: none converted for {:?} (run `asamu-import lightmaps`)",
                    prep.missing
                );
                info!("{}", status.line);
                *state = LightmapState::Done;
                return;
            }
            info!(
                "lightmaps: {} components with light maps in {:?} (missing {:?}, rejected {}), \
                 {} of {} draws paired, {} light(s) known, {} BSP mesh(es) ({} dropped; {} of \
                 {} flat BSP triangles)",
                prep.components,
                prep.packages,
                prep.missing,
                prep.rejected,
                prep.draws.values().map(Vec::len).sum::<usize>(),
                prep.draws_total,
                prep.lights.len(),
                prep.bsp.len(),
                prep.bsp_dropped,
                prep.bsp_triangles.1,
                prep.bsp_triangles.0
            );
            *state = LightmapState::Applying(Box::new(Applying {
                prep,
                ..Applying::default()
            }));
        }
        Err(e) => {
            status.line = format!("lightmaps: FAILED: {e}");
            error!("{}", status.line);
            *state = LightmapState::Done;
        }
    }
}

fn image_handle(
    server: &AssetServer,
    cache: &mut HashMap<String, Handle<Image>>,
    image: &str,
) -> Handle<Image> {
    if let Some(h) = cache.get(image) {
        return h.clone();
    }
    let h: Handle<Image> = server
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.sampler = ImageSampler::linear();
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(format!("{SOURCE}://{image}"));
    cache.insert(image.to_owned(), h.clone());
    h
}

/// glTF file (relative to `meshes/`) and primitive index of a mesh handle
/// loaded by `converted.rs` (`converted://meshes/<file>#Mesh0/Primitive<p>`).
fn primitive_of(path: &AssetPath<'_>) -> Option<(String, usize)> {
    let file = path.path().to_str()?.replace('\\', "/");
    let rel = file.strip_prefix("meshes/")?.to_owned();
    let label = path.label()?;
    let p = label.rsplit("Primitive").next()?.parse().ok()?;
    Some((rel, p))
}

fn assign_draws(
    mut commands: Commands,
    mut state: ResMut<LightmapState>,
    draws: Query<(Entity, &LevelEntity, &Mesh3d, &Transform), Without<LightmapHandled>>,
) {
    let LightmapState::Applying(app) = &mut *state else {
        return;
    };
    for (entity, le, mesh, transform) in &draws {
        commands.entity(entity).insert(LightmapHandled);
        let Some((gltf, prim)) = mesh.0.path().and_then(primitive_of) else {
            app.unmatched += 1;
            continue;
        };
        let key = (
            le.actor_slot,
            gltf,
            prim,
            translation_bits(transform.translation),
        );
        let Some(a) = app.prep.draws.get_mut(&key).and_then(Vec::pop) else {
            continue;
        };
        app.pending.push((entity, mesh.0.clone(), a));
    }
}

/// A copy of `mesh` with `UV_1` taken from `UV_0` (or zeros).
fn with_second_uv(mesh: &Mesh) -> Mesh {
    let mut m = mesh.clone();
    let uv = match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
        Some(VertexAttributeValues::Float32x2(v)) => v.clone(),
        _ => vec![[0.0, 0.0]; mesh.count_vertices()],
    };
    m.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv);
    m
}

#[allow(clippy::too_many_arguments)]
fn resolve_pending(
    mut commands: Commands,
    mut state: ResMut<LightmapState>,
    mut status: ResMut<LightmapStatus>,
    settings: Res<LightmapSettings>,
    server: Res<AssetServer>,
    mut images: ResMut<LightmapImages>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mat_of: Query<&MeshMaterial3d<StandardMaterial>>,
) {
    let LightmapState::Applying(app) = &mut *state else {
        return;
    };
    let exposure = app.prep.exposure;
    let pending = std::mem::take(&mut app.pending);
    for (entity, mesh, a) in pending {
        let Some(m) = meshes.get(&mesh) else {
            if server.load_state(&mesh).is_failed() {
                continue;
            }
            app.pending.push((entity, mesh, a));
            continue;
        };
        let has_uv1 = m.attribute(Mesh::ATTRIBUTE_UV_1).is_some();
        let use_mesh = match (a.kind, a.uv_channel, has_uv1) {
            (LightmapKind::Texture, 1, true) | (LightmapKind::Constant, _, true) => None,
            (LightmapKind::Texture, 0, _) | (LightmapKind::Constant, _, false) => {
                let derived = match app.derived.get(&mesh.id()) {
                    Some(h) => h.clone(),
                    None => {
                        let d = with_second_uv(m);
                        let h = meshes.add(d);
                        app.derived.insert(mesh.id(), h.clone());
                        images.source_meshes.push(mesh.clone());
                        h
                    }
                };
                Some(derived)
            }
            _ => {
                app.no_uv += 1;
                continue;
            }
        };
        let image = image_handle(&server, &mut images.handles, &a.image);
        let [x0, y0, x1, y1] = a.uv_rect;
        let mut e = commands.entity(entity);
        if let Some(h) = use_mesh {
            e.insert(Mesh3d(h));
        }
        e.insert(Lightmap {
            image,
            uv_rect: Rect::new(x0, y0, x1, y1),
            bicubic_sampling: settings.bicubic,
        });
        if let Ok(mat) = mat_of.get(entity)
            && !app.materials_done.contains(&mat.0.id())
        {
            if let Some(mut sm) = materials.get_mut(&mat.0) {
                sm.lightmap_exposure = exposure;
            }
            app.materials_done.push(mat.0.id());
        }
        app.lit += 1;
        if a.kind == LightmapKind::Constant {
            app.constant += 1;
        }
    }
    app.frames_waiting = if app.pending.is_empty() {
        app.frames_waiting.saturating_add(1)
    } else {
        0
    };
    status.line = format!(
        "lightmaps: {} draws lit ({} constant), {} waiting, {} without a usable UV set",
        app.lit,
        app.constant,
        app.pending.len(),
        app.no_uv
    );
}

#[allow(clippy::type_complexity)]
fn adjust_lights(
    mut state: ResMut<LightmapState>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut points: Query<(&Transform, &mut PointLight), With<LevelLight>>,
    mut spots: Query<(&Transform, &mut SpotLight), (With<LevelLight>, Without<PointLight>)>,
    mut dirs: Query<
        (&Transform, &mut DirectionalLight),
        (With<LevelLight>, Without<PointLight>, Without<SpotLight>),
    >,
) {
    let LightmapState::Applying(app) = &mut *state else {
        return;
    };
    if app.lights_matched || app.lit == 0 {
        return;
    }
    let baked = |key: LightKey| app.prep.lights.get(&key).is_some_and(|b| b.baked);
    let mut n = 0usize;
    for (t, mut l) in &mut points {
        if baked(light_key_point(t.translation, 0)) {
            l.affects_lightmapped_mesh_diffuse = false;
            n += 1;
        }
    }
    for (t, mut l) in &mut spots {
        if baked(light_key_point(t.translation, 1)) {
            l.affects_lightmapped_mesh_diffuse = false;
            n += 1;
        }
    }
    for (t, mut l) in &mut dirs {
        if baked(light_key_dir(t.rotation)) {
            l.affects_lightmapped_mesh_diffuse = false;
            n += 1;
        }
    }
    ambient.affects_lightmapped_meshes = false;
    app.lights_matched = true;
    info!("lightmaps: {n} baked light(s) no longer light light-mapped surfaces");
}

fn bsp_mesh(m: &BspLightmapMesh) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, m.uv0.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, m.uv1.clone())
    .with_inserted_indices(Indices::U32(m.indices.clone()))
}

/// Whether the light-mapped BSP may replace the flat BSP: it must cover
/// every flat triangle (`flat`, all merged levels), i.e. have the same
/// triangle count (`lightmapped`). Fewer means surfaces would vanish (a
/// sub-level without a light map file, or a file from an importer that
/// skipped the elements without a light map).
fn bsp_swap_allowed(flat: usize, lightmapped: usize) -> bool {
    flat > 0 && lightmapped == flat
}

#[allow(clippy::too_many_arguments)]
fn swap_bsp(
    mut commands: Commands,
    mut state: ResMut<LightmapState>,
    mut status: ResMut<LightmapStatus>,
    settings: Res<LightmapSettings>,
    server: Res<AssetServer>,
    mut images: ResMut<LightmapImages>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    flat: Query<(Entity, &Mesh3d, &MeshMaterial3d<StandardMaterial>), With<LevelBsp>>,
) {
    let LightmapState::Applying(app) = &mut *state else {
        return;
    };
    if !app.bsp_done {
        app.bsp_done = true;
        let (flat_tris, lm_tris) = app.prep.bsp_triangles;
        if settings.bsp && !app.prep.bsp.is_empty() && !bsp_swap_allowed(flat_tris, lm_tris) {
            warn!(
                "lightmaps: keeping the flat BSP: the light map files cover {lm_tris} of its \
                 {flat_tris} triangles (re-run `asamu-import lightmaps --force`)"
            );
        } else if settings.bsp && !app.prep.bsp.is_empty() {
            // Material handle per BSP material path, from the flat meshes;
            // tangents when the flat meshes have them (normal maps).
            let mut by_path: HashMap<Option<String>, Handle<StandardMaterial>> = HashMap::new();
            let mut tangents = false;
            for (entity, mesh, mat) in &flat {
                let flat_mesh = meshes.get(&mesh.0);
                tangents |=
                    flat_mesh.is_some_and(|m| m.attribute(Mesh::ATTRIBUTE_TANGENT).is_some());
                let sig = flat_mesh.and_then(|m| match m.attribute(Mesh::ATTRIBUTE_POSITION) {
                    Some(VertexAttributeValues::Float32x3(p)) => Some((
                        p.len(),
                        p.first().map(|v| translation_bits(Vec3::from_array(*v))),
                    )),
                    _ => None,
                });
                if let Some((n, Some(first))) = sig
                    && let Some(s) = app
                        .prep
                        .bsp_signatures
                        .iter()
                        .find(|s| s.1 == n && s.2 == first)
                {
                    by_path.insert(s.0.clone(), mat.0.clone());
                }
                commands.entity(entity).insert(Visibility::Hidden);
            }
            let exposure = app.prep.exposure;
            let bsp = std::mem::take(&mut app.prep.bsp);
            let mut neutral: Option<Handle<StandardMaterial>> = None;
            let mut unmatched = 0usize;
            let mut unlit = 0usize;
            for m in &bsp {
                let material = match by_path.get(&m.material) {
                    Some(h) => h.clone(),
                    None => {
                        unmatched += 1;
                        neutral
                            .get_or_insert_with(|| {
                                materials.add(StandardMaterial {
                                    base_color: Color::srgb(0.6, 0.6, 0.6),
                                    perceptual_roughness: 0.9,
                                    ..default()
                                })
                            })
                            .clone()
                    }
                };
                if !app.materials_done.contains(&material.id()) {
                    if let Some(mut sm) = materials.get_mut(&material) {
                        sm.lightmap_exposure = exposure;
                    }
                    app.materials_done.push(material.id());
                }
                let mut mesh = bsp_mesh(m);
                if tangents && let Err(e) = mesh.generate_tangents() {
                    warn!("light-mapped BSP tangents for {:?}: {e}", m.material);
                }
                let mut e = commands.spawn((
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    LightmappedBsp,
                ));
                match &m.lightmap {
                    Some(lm) => {
                        let image = image_handle(&server, &mut images.handles, &lm.image);
                        let [x0, y0, x1, y1] = lm.uv_rect;
                        e.insert(Lightmap {
                            image,
                            uv_rect: Rect::new(x0, y0, x1, y1),
                            bicubic_sampling: settings.bicubic,
                        });
                    }
                    None => unlit += 1,
                }
            }
            info!(
                "lightmaps: {} BSP mesh(es) replace the flat BSP ({unlit} without a light map, \
                 as in the original; {} materials matched, {unmatched} mesh(es) with a neutral \
                 material)",
                bsp.len(),
                by_path.len()
            );
        }
    }
    // Finish once every draw was considered and nothing waits for a mesh.
    if app.pending.is_empty() && app.frames_waiting > 120 {
        status.line = format!(
            "lightmaps: {} draws lit ({} constant), {} without a usable UV set, {} unmatched",
            app.lit, app.constant, app.no_uv, app.unmatched
        );
        info!("{}", status.line);
        *state = LightmapState::Done;
    }
}

/// Removes light maps whose atlas image failed to load (a missing or damaged
/// file must not leave the surface waiting for it).
fn drop_failed_images(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut images: ResMut<LightmapImages>,
    lit: Query<(Entity, &Lightmap)>,
) {
    let newly: Vec<AssetId<Image>> = images
        .handles
        .values()
        .map(Handle::id)
        .filter(|id| !images.failed.contains(id) && server.load_state(*id).is_failed())
        .collect();
    if newly.is_empty() {
        return;
    }
    let mut n = 0usize;
    for (e, lm) in &lit {
        if newly.contains(&lm.image.id()) {
            commands.entity(e).remove::<Lightmap>();
            n += 1;
        }
    }
    warn!(
        "lightmaps: {} atlas image(s) failed to load; {n} surface(s) drawn without a light map",
        newly.len()
    );
    images.failed.extend(newly);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_of_parses_converted_mesh_paths() {
        let p = AssetPath::from("converted://meshes/Pkg/Group/Rock.gltf#Mesh0/Primitive3");
        assert_eq!(
            primitive_of(&p),
            Some(("Pkg/Group/Rock.gltf".to_owned(), 3))
        );
        let no_label = AssetPath::from("converted://meshes/Pkg/Rock.gltf");
        assert_eq!(primitive_of(&no_label), None);
        let other = AssetPath::from("converted://textures/x.dds#Mesh0/Primitive0");
        assert_eq!(primitive_of(&other), None);
        let bad = AssetPath::from("converted://meshes/x.gltf#Mesh0/PrimitiveX");
        assert_eq!(primitive_of(&bad), None);
    }

    #[test]
    fn second_uv_copies_the_first_or_zeros() {
        let base = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        );
        let m = with_second_uv(&base);
        assert!(matches!(
            m.attribute(Mesh::ATTRIBUTE_UV_1),
            Some(VertexAttributeValues::Float32x2(v)) if v == &vec![[0.0, 0.0]; 3]
        ));
        let uv = vec![[0.25f32, 0.5], [1.0, 0.0], [0.0, 1.0]];
        let m = with_second_uv(&base.with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone()));
        assert!(matches!(
            m.attribute(Mesh::ATTRIBUTE_UV_1),
            Some(VertexAttributeValues::Float32x2(v)) if v == &uv
        ));
    }

    #[test]
    fn light_keys_distinguish_kind_and_position() {
        let a = light_key_point(Vec3::new(1.0, 2.0, 3.0), 0);
        assert_eq!(a, light_key_point(Vec3::new(1.0, 2.0, 3.0), 0));
        assert_ne!(a, light_key_point(Vec3::new(1.0, 2.0, 3.0), 1));
        assert_ne!(a, light_key_point(Vec3::new(1.0, 2.0, 3.5), 0));
        let r = Transform::default()
            .looking_to(Vec3::new(0.3, -1.0, 0.2), Vec3::Y)
            .rotation;
        assert_eq!(light_key_dir(r), light_key_dir(r));
        assert_eq!(light_key_dir(r).1, 2);
    }

    #[test]
    fn a_level_change_resets_only_active_states() {
        assert!(needs_reset(&LightmapState::Done, true));
        assert!(needs_reset(&LightmapState::Applying(Box::default()), true));
        assert!(!needs_reset(&LightmapState::Done, false));
        assert!(!needs_reset(&LightmapState::Idle, true));
        assert!(!needs_reset(&LightmapState::Off, true));
    }

    #[test]
    fn bsp_swap_needs_full_coverage() {
        assert!(bsp_swap_allowed(877, 877));
        // An older light map file without the unlit elements, or a merged
        // sub-level without one: surfaces would vanish.
        assert!(!bsp_swap_allowed(877, 772));
        assert!(!bsp_swap_allowed(0, 0));
        assert!(!bsp_swap_allowed(10, 12));
    }

    #[test]
    fn bsp_mesh_has_two_uv_sets() {
        let m = BspLightmapMesh {
            material: None,
            lightmap: None,
            positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            normals: vec![[0.0, 1.0, 0.0]; 3],
            uv0: vec![[0.0; 2]; 3],
            uv1: vec![[0.5; 2]; 3],
            indices: vec![0, 2, 1],
        };
        let mesh = bsp_mesh(&m);
        assert!(mesh.attribute(Mesh::ATTRIBUTE_UV_1).is_some());
        assert_eq!(mesh.count_vertices(), 3);
        assert_eq!(mesh.indices().map(Indices::len), Some(3));
    }
}
