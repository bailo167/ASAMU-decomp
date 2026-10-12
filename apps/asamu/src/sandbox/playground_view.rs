//! How the playground looks: its shapes, colours, light, sky and haze.
//!
//! Everything drawn here is ours and needs no game data: the meshes are the
//! playground's own generated solids (`asamu_sandbox::playground`), the
//! colours tell their roles apart, and the evening sky, the sun, the haze
//! and the lamps are a look we chose for it. None of it is the original's
//! look, and none of it stands in for an effect of the original.
//!
//! While a session plays the playground, [`sync`]:
//!
//! - draws one flat-shaded mesh per piece and moves the pieces the level
//!   script moves (the ferry, the carousel, the wheel) to where the
//!   simulation has them;
//! - dims a recharge crystal while it is used up;
//! - gives the scene its own sun, ambient light, sky dome and distance haze;
//! - keeps what the composition would otherwise show out of the picture: the
//!   stock graybox boxes and light, the developer read-out and the level
//!   gizmos in a graybox run, the converted level behind the menu (and its
//!   atmosphere) in a run on converted data.
//!
//! When the session leaves the playground everything spawned here is
//! removed and what was changed is put back: the clear colour, the ambient
//! light, the developer read-out, the level gizmos, the stock visuals. A
//! converted level that was unloaded is loaded again by the menu flow when
//! a map is started (it plans a level whenever none is shown).
//!
//! This module reads the simulation and never writes it.

use std::collections::HashMap;

use asamu_core::coords::ue_dir_to_bevy;
use asamu_core::glam as sim_glam;
use asamu_game::asamu_world::rotation::rotation_rows;
use asamu_sandbox::playground::{
    Look, PLAYGROUND_MAP, Piece, Role, Shape, is_playground, playground,
};
use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;

use super::{Lab, LabSet, Phase, Stage, lab_active};
use crate::converted::{ConvertedLevel, LevelBsp, LevelEntity, LevelLight};
use crate::{LevelGizmos, LevelVisual, PlayerCamera, Sim, to_render};

/// An entity this module spawned.
#[derive(Component)]
struct PlaygroundEntity;

/// A piece the level script moves: its actor id.
#[derive(Component)]
struct Moving(u32);

/// A recharge crystal: its actor id and whether it is drawn charged.
#[derive(Component)]
struct CrystalPiece {
    actor: u32,
    charged: bool,
}

/// The sky dome (it stays centred on the camera).
#[derive(Component)]
struct SkyDome;

/// The two materials a crystal switches between.
#[derive(Resource, Default)]
struct CrystalLooks {
    charged: Handle<StandardMaterial>,
    spent: Handle<StandardMaterial>,
}

/// What was changed for the playground, to be put back.
#[derive(Debug)]
struct Kept {
    clear: Color,
    ambient: (Color, f32),
    /// The developer read-out's switch (`None`: the resource did not exist).
    debug: Option<bool>,
    gizmos: bool,
}

/// What this module shows now.
#[derive(Resource, Debug, Default)]
struct Shown {
    kept: Option<Kept>,
}

fn shown(shown: Res<Shown>) -> bool {
    shown.kept.is_some()
}

// ---------------------------------------------------------------------------
// Colours (ours).
// ---------------------------------------------------------------------------

/// The colours of a look: its flat tops, its sides, and what it gives off.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Paint {
    top: [f32; 3],
    side: [f32; 3],
    emissive: [f32; 3],
    /// Drawn without lighting (a light source).
    unlit: bool,
    /// Opacity (below 1: blended).
    alpha: f32,
}

const fn paint(top: [f32; 3], side: [f32; 3]) -> Paint {
    Paint {
        top,
        side,
        emissive: [0.0; 3],
        unlit: false,
        alpha: 1.0,
    }
}

const fn glow(colour: [f32; 3], emissive: [f32; 3]) -> Paint {
    Paint {
        top: colour,
        side: colour,
        emissive,
        unlit: false,
        alpha: 1.0,
    }
}

const fn lamp(colour: [f32; 3]) -> Paint {
    Paint {
        top: colour,
        side: colour,
        emissive: [0.0; 3],
        unlit: true,
        alpha: 1.0,
    }
}

/// The colours of the landmark lights, one per direction from the hub.
const BEACONS: [[f32; 3]; 5] = [
    [1.0, 0.82, 0.30],
    [0.35, 0.90, 1.0],
    [0.82, 0.50, 1.0],
    [1.0, 0.45, 0.62],
    [0.62, 1.0, 0.50],
];

/// sRGB colours per look. Each role has its own hue, so the level reads at
/// a glance: pale paving, green islands, grey rock, teal hooks, orange
/// ramps, magenta riders, dark machinery, cyan crystals, the red catch,
/// white start arches, gold finish frames.
fn paint_of(look: Look) -> Paint {
    match look {
        Look::Paving => paint([0.82, 0.79, 0.71], [0.50, 0.48, 0.47]),
        Look::Island => paint([0.38, 0.63, 0.30], [0.47, 0.38, 0.31]),
        Look::Rock => paint([0.62, 0.61, 0.64], [0.43, 0.43, 0.49]),
        Look::Hook => glow([0.08, 0.66, 0.62], [0.0, 0.42, 0.38]),
        Look::Ramp => paint([0.96, 0.56, 0.14], [0.70, 0.36, 0.10]),
        Look::Rider => paint([0.86, 0.32, 0.64], [0.56, 0.20, 0.46]),
        Look::Machine => paint([0.34, 0.34, 0.40], [0.24, 0.24, 0.30]),
        Look::Crystal => glow([0.40, 0.94, 1.0], [0.25, 1.3, 1.7]),
        Look::Hazard => Paint {
            alpha: 0.72,
            ..glow([0.95, 0.12, 0.10], [1.6, 0.12, 0.06])
        },
        Look::StartGate => glow([0.96, 0.96, 0.92], [0.30, 0.30, 0.27]),
        Look::FinishGate => glow([1.0, 0.78, 0.18], [0.75, 0.48, 0.05]),
        Look::Lamp => lamp([1.0, 0.86, 0.58]),
        Look::Beacon(n) => lamp(BEACONS[usize::from(n) % BEACONS.len()]),
        Look::Inlay(n) => {
            let c = BEACONS[usize::from(n) % BEACONS.len()];
            paint(c, [c[0] * 0.7, c[1] * 0.7, c[2] * 0.7])
        }
    }
}

/// A used-up crystal: the same shape, grey and dark.
const SPENT_CRYSTAL: Paint = paint([0.36, 0.42, 0.46], [0.30, 0.34, 0.38]);

/// An sRGB triple as the linear colour a vertex carries.
fn linear(srgb: [f32; 3], shade: f32) -> [f32; 4] {
    let c = Color::srgb(srgb[0], srgb[1], srgb[2]).to_linear();
    [c.red * shade, c.green * shade, c.blue * shade, 1.0]
}

/// The sky's colour in the direction whose height above the horizon is `up`
/// (−1 straight down, 0 the horizon, 1 straight up): an evening sky, warm at
/// the horizon, deep blue overhead, dusky haze below.
fn sky_colour(up: f32) -> [f32; 3] {
    const NADIR: [f32; 3] = [0.20, 0.23, 0.38];
    const HAZE: [f32; 3] = [0.62, 0.55, 0.62];
    const HORIZON: [f32; 3] = [0.99, 0.80, 0.62];
    const LOW: [f32; 3] = [0.62, 0.72, 0.90];
    const ZENITH: [f32; 3] = [0.15, 0.30, 0.64];
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    if !up.is_finite() {
        return HORIZON;
    }
    if up < -0.12 {
        mix(HAZE, NADIR, (-up - 0.12) / 0.88)
    } else if up < 0.0 {
        mix(HORIZON, HAZE, -up / 0.12)
    } else if up < 0.22 {
        mix(HORIZON, LOW, up / 0.22)
    } else {
        mix(LOW, ZENITH, (up - 0.22) / 0.78)
    }
}

/// Where the light of the playground's sun travels, UE axes (ours): from
/// behind the start, low, so the chains ahead are lit on the side the
/// player sees.
const SUN_TRAVELS: sim_glam::Vec3 = sim_glam::Vec3::new(0.55, 0.38, -0.74);

// ---------------------------------------------------------------------------
// Meshes.
// ---------------------------------------------------------------------------

/// The vertices of a flat-shaded mesh: positions, normals, colours.
#[derive(Clone, Debug, Default, PartialEq)]
struct Flat {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colours: Vec<[f32; 4]>,
}

/// `shape` as flat-shaded triangles in render space, every face turned
/// outwards (the shape is convex, so "away from its centre" is outwards),
/// tops in the look's top colour and sides shaded darker towards the
/// bottom.
fn flat(shape: &Shape, paint: Paint) -> Flat {
    let points: Vec<Vec3> = shape
        .vertices
        .iter()
        .map(|p| to_render(sim_glam::Vec3::from_array(*p)))
        .collect();
    let mut out = Flat::default();
    if points.is_empty() {
        return out;
    }
    let centre = points.iter().copied().sum::<Vec3>() / points.len() as f32;
    let (low, high) = points
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), p| {
            (low.min(p.y), high.max(p.y))
        });
    let span = (high - low).max(1.0e-6);
    for triangle in &shape.triangles {
        let corner = |i: u32| points.get(i as usize).copied();
        let (Some(a), Some(mut b), Some(mut c)) = (
            corner(triangle[0]),
            corner(triangle[1]),
            corner(triangle[2]),
        ) else {
            continue;
        };
        let Some(mut normal) = (b - a).cross(c - a).try_normalize() else {
            continue;
        };
        if normal.dot(a - centre) < 0.0 {
            std::mem::swap(&mut b, &mut c);
            normal = -normal;
        }
        for p in [a, b, c] {
            let colour = if normal.y > 0.75 {
                linear(paint.top, 1.0)
            } else {
                // Darker towards the bottom, and under an overhang.
                let height = (p.y - low) / span;
                let under = if normal.y < -0.2 { 0.78 } else { 1.0 };
                linear(paint.side, (0.58 + 0.42 * height) * under)
            };
            out.positions.push(p.to_array());
            out.normals.push(normal.to_array());
            out.colours
                .push([colour[0], colour[1], colour[2], paint.alpha]);
        }
    }
    out
}

fn mesh_of(flat: Flat) -> Mesh {
    let count = u32::try_from(flat.positions.len()).unwrap_or(0);
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, flat.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, flat.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, flat.colours)
    .with_inserted_indices(Indices::U32((0..count).collect()))
}

/// Rings and segments of the sky dome.
const SKY_RINGS: u32 = 16;
const SKY_SEGMENTS: u32 = 24;
/// Radius of the sky dome, render units: inside every camera's far plane
/// (1,000 units or more in both compositions).
const SKY_RADIUS: f32 = 700.0;
const _: () = assert!(SKY_RADIUS < 1000.0);

/// The sky: a sphere seen from inside, coloured by height.
fn sky_mesh() -> Mesh {
    let mut positions = Vec::new();
    let mut colours = Vec::new();
    for ring in 0..=SKY_RINGS {
        let latitude = std::f32::consts::PI * (ring as f32 / SKY_RINGS as f32 - 0.5);
        for segment in 0..=SKY_SEGMENTS {
            let longitude = std::f32::consts::TAU * segment as f32 / SKY_SEGMENTS as f32;
            let direction = Vec3::new(
                latitude.cos() * longitude.cos(),
                latitude.sin(),
                latitude.cos() * longitude.sin(),
            );
            positions.push((direction * SKY_RADIUS).to_array());
            colours.push(linear(sky_colour(direction.y), 1.0));
        }
    }
    let mut indices = Vec::new();
    let row = SKY_SEGMENTS + 1;
    for ring in 0..SKY_RINGS {
        for segment in 0..SKY_SEGMENTS {
            let a = ring * row + segment;
            let b = a + row;
            indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
    .with_inserted_indices(Indices::U32(indices))
}

fn material_of(paint: Paint) -> StandardMaterial {
    StandardMaterial {
        // The vertices carry the colour.
        base_color: Color::WHITE,
        emissive: LinearRgba::rgb(paint.emissive[0], paint.emissive[1], paint.emissive[2]),
        perceptual_roughness: 0.92,
        unlit: paint.unlit,
        alpha_mode: if paint.alpha < 1.0 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        // A blended sheet is seen from both sides.
        cull_mode: if paint.alpha < 1.0 {
            None
        } else {
            StandardMaterial::default().cull_mode
        },
        ..default()
    }
}

/// The render transform of a piece at `location` (UU) turned by `rotation`
/// (pitch, yaw, roll in rotator units): the simulation's own rotation
/// matrix, carried into render axes.
fn placement(location: sim_glam::Vec3, rotation: [i32; 3]) -> Transform {
    let rows = rotation_rows(rotation);
    let axis = |v: sim_glam::DVec3| {
        let d = ue_dir_to_bevy(v.as_vec3());
        Vec3::new(d.x, d.y, d.z)
    };
    // Render X is UE Y, render Y is UE Z, render Z is UE −X: the images of
    // the render axes are the images of those UE axes.
    let basis = Mat3::from_cols(axis(rows[1]), axis(rows[2]), -axis(rows[0]));
    let rotation = if basis.is_finite() && basis.determinant() > 0.5 {
        Quat::from_mat3(&basis).normalize()
    } else {
        Quat::IDENTITY
    };
    Transform {
        translation: to_render(location),
        rotation,
        scale: Vec3::ONE,
    }
}

// ---------------------------------------------------------------------------
// The scene.
// ---------------------------------------------------------------------------

/// Every entity of a spawned converted level (meshes, BSP, lights, and the
/// light-mapped BSP meshes), as the menu flow removes them.
type LevelEntities = Or<(
    With<LevelEntity>,
    With<LevelBsp>,
    With<LevelLight>,
    With<crate::lightmaps::LightmappedBsp>,
)>;

/// What entering and leaving the playground touches.
#[derive(SystemParam)]
struct Scene<'w, 's> {
    commands: Commands<'w, 's>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    crystals: ResMut<'w, CrystalLooks>,
    clear: ResMut<'w, ClearColor>,
    ambient: ResMut<'w, GlobalAmbientLight>,
    debug: Option<ResMut<'w, crate::hud::DebugInfo>>,
    gizmos: Option<ResMut<'w, LevelGizmos>>,
    level: Option<ResMut<'w, ConvertedLevel>>,
    own: Query<'w, 's, Entity, With<PlaygroundEntity>>,
    cameras: Query<'w, 's, Entity, With<PlayerCamera>>,
    level_entities: Query<'w, 's, Entity, LevelEntities>,
}

fn haze() -> DistanceFog {
    let horizon = sky_colour(0.02);
    DistanceFog {
        color: Color::srgb(horizon[0] * 0.86, horizon[1] * 0.86, horizon[2] * 0.92),
        falloff: FogFalloff::Linear {
            start: 120.0,
            end: 2400.0,
        },
        ..default()
    }
}

impl Scene<'_, '_> {
    fn enter(&mut self) -> Kept {
        let kept = Kept {
            clear: self.clear.0,
            ambient: (self.ambient.color, self.ambient.brightness),
            debug: self.debug.as_ref().map(|d| d.0),
            gizmos: self.gizmos.as_ref().is_none_or(|g| g.0),
        };
        let horizon = sky_colour(0.0);
        self.clear.0 = Color::srgb(horizon[0], horizon[1], horizon[2]);
        self.ambient.color = Color::srgb(0.80, 0.84, 1.0);
        self.ambient.brightness = 1500.0;
        // A clean screen: no developer read-out, no level gizmos.
        if let Some(debug) = self.debug.as_mut()
            && debug.0
        {
            debug.0 = false;
        }
        if let Some(gizmos) = self.gizmos.as_mut()
            && gizmos.0
        {
            gizmos.0 = false;
        }

        let level = playground();
        let mut looks: HashMap<Look, Handle<StandardMaterial>> = HashMap::new();
        let mut shapes: HashMap<(usize, Look), Handle<Mesh>> = HashMap::new();
        for piece in &level.pieces {
            self.spawn_piece(piece, &level.shapes, &mut looks, &mut shapes);
        }
        self.crystals.charged = looks
            .get(&Look::Crystal)
            .cloned()
            .unwrap_or_else(|| self.materials.add(material_of(paint_of(Look::Crystal))));
        self.crystals.spent = self.materials.add(material_of(SPENT_CRYSTAL));

        // The sun, with shadows near the player.
        let travels = ue_dir_to_bevy(SUN_TRAVELS);
        self.commands.spawn((
            PlaygroundEntity,
            DirectionalLight {
                illuminance: 8500.0,
                color: Color::srgb(1.0, 0.90, 0.76),
                shadow_maps_enabled: true,
                ..default()
            },
            CascadeShadowConfigBuilder {
                num_cascades: 4,
                maximum_distance: 260.0,
                first_cascade_far_bound: 12.0,
                ..default()
            }
            .build(),
            Transform::default().looking_to(Vec3::new(travels.x, travels.y, travels.z), Vec3::Y),
        ));
        // The sky.
        let sky = self.meshes.add(sky_mesh());
        let sky_material = self.materials.add(StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            cull_mode: None,
            fog_enabled: false,
            ..default()
        });
        self.commands.spawn((
            PlaygroundEntity,
            SkyDome,
            Mesh3d(sky),
            MeshMaterial3d(sky_material),
            Transform::default(),
            NotShadowCaster,
            NotShadowReceiver,
        ));
        // Haze, for depth.
        for camera in &self.cameras {
            self.commands.entity(camera).insert(haze());
        }
        kept
    }

    fn spawn_piece(
        &mut self,
        piece: &Piece,
        all: &[Shape],
        looks: &mut HashMap<Look, Handle<StandardMaterial>>,
        shapes: &mut HashMap<(usize, Look), Handle<Mesh>>,
    ) {
        let Some(shape) = all.get(piece.shape) else {
            return;
        };
        let paint = paint_of(piece.look);
        let material = looks
            .entry(piece.look)
            .or_insert_with(|| self.materials.add(material_of(paint)))
            .clone();
        let mesh = shapes
            .entry((piece.shape, piece.look))
            .or_insert_with(|| self.meshes.add(mesh_of(flat(shape, paint))))
            .clone();
        let mut entity = self.commands.spawn((
            PlaygroundEntity,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            placement(piece.location, piece.rotation),
        ));
        if paint.unlit || paint.alpha < 1.0 {
            entity.insert((NotShadowCaster, NotShadowReceiver));
        }
        match (piece.role, piece.actor) {
            (Role::Turning | Role::Carried | Role::Gondola | Role::Ferry, Some(actor)) => {
                entity.insert(Moving(actor));
            }
            (Role::Crystal, Some(actor)) => {
                entity.insert(CrystalPiece {
                    actor,
                    charged: true,
                });
            }
            _ => {}
        }
        // A lamp also lights what stands around it.
        if piece.look == Look::Lamp {
            self.commands.spawn((
                PlaygroundEntity,
                PointLight {
                    intensity: 900_000.0,
                    range: 22.0,
                    color: Color::srgb(1.0, 0.82, 0.55),
                    ..default()
                },
                Transform::from_translation(to_render(piece.location)),
            ));
        }
    }

    fn leave(&mut self, kept: Kept) {
        for entity in &self.own {
            self.commands.entity(entity).despawn();
        }
        for camera in &self.cameras {
            self.commands.entity(camera).remove::<DistanceFog>();
        }
        self.clear.0 = kept.clear;
        self.ambient.color = kept.ambient.0;
        self.ambient.brightness = kept.ambient.1;
        if let (Some(debug), Some(was)) = (self.debug.as_mut(), kept.debug)
            && debug.0 != was
        {
            debug.0 = was;
        }
        if let Some(gizmos) = self.gizmos.as_mut()
            && gizmos.0 != kept.gizmos
        {
            gizmos.0 = kept.gizmos;
        }
    }

    /// A run on converted data shows a converted level behind its menu (and
    /// plans it at start-up): it is taken out of the picture, with its
    /// lights and its atmosphere, for as long as the playground is played.
    /// The menu flow plans a level again when a map is started.
    fn unload_converted_level(&mut self) {
        let Some(level) = self.level.as_mut() else {
            return;
        };
        // Whatever of a level is in the world goes, in every frame: a plan
        // that finished in the frame it was cancelled in still spawns.
        for entity in &self.level_entities {
            self.commands.entity(entity).despawn();
        }
        if level.phase.is_none() && level.level == PLAYGROUND_MAP {
            return;
        }
        // Dropping a planning task cancels it.
        level.phase = None;
        level.progress = (0, 0, 0);
        // No converted level has this name: nothing of one (its atmosphere,
        // its light maps) applies.
        PLAYGROUND_MAP.clone_into(&mut level.level);
    }
}

/// Keeps the picture in step with the session's stage.
#[allow(clippy::type_complexity)]
fn sync(
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    mut shown: ResMut<Shown>,
    mut scene: Scene,
    mut stock: Query<
        &mut Visibility,
        (
            Or<(With<LevelVisual>, With<DirectionalLight>)>,
            Without<PlaygroundEntity>,
        ),
    >,
) {
    let wanted = lab.phase == Phase::Active
        && lab.stage == Some(Stage::Playground)
        && sim.as_deref().is_some_and(|sim| is_playground(&sim.game));
    match (wanted, shown.kept.take()) {
        (true, None) => shown.kept = Some(scene.enter()),
        (true, kept) => shown.kept = kept,
        (false, Some(kept)) => {
            scene.leave(kept);
            for mut visibility in &mut stock {
                if *visibility != Visibility::Inherited {
                    *visibility = Visibility::Inherited;
                }
            }
            return;
        }
        (false, None) => return,
    }
    // What the composition shows by itself stays out of the picture.
    for mut visibility in &mut stock {
        if *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
    }
    scene.unload_converted_level();
}

/// Moves what the level script moves, shows which crystals are used up, and
/// keeps the sky around the camera.
#[allow(clippy::type_complexity)]
fn animate(
    sim: Option<Res<Sim>>,
    looks: Res<CrystalLooks>,
    camera: Query<&Transform, (With<PlayerCamera>, Without<PlaygroundEntity>)>,
    mut moving: Query<(&Moving, &mut Transform), (With<PlaygroundEntity>, Without<SkyDome>)>,
    mut crystals: Query<(&mut CrystalPiece, &mut MeshMaterial3d<StandardMaterial>)>,
    mut sky: Query<&mut Transform, (With<SkyDome>, With<PlaygroundEntity>, Without<Moving>)>,
) {
    if let Some(camera) = camera.iter().next() {
        for mut dome in &mut sky {
            if dome.translation != camera.translation {
                dome.translation = camera.translation;
            }
        }
    }
    let Some(sim) = sim.as_deref().filter(|sim| is_playground(&sim.game)) else {
        return;
    };
    if let Some(script) = sim.script.as_ref() {
        let moved: HashMap<u32, (sim_glam::Vec3, [i32; 3])> = script
            .moved_actors()
            .into_iter()
            .map(|(id, location, rotation)| (id, (location, rotation)))
            .collect();
        for (piece, mut transform) in &mut moving {
            if let Some((location, rotation)) = moved.get(&piece.0)
                && location.is_finite()
            {
                let now = placement(*location, *rotation);
                if *transform != now {
                    *transform = now;
                }
            }
        }
    }
    let level = sim.game.level();
    let objects = sim.game.objects();
    for (mut crystal, mut material) in &mut crystals {
        let charged = objects.crystal_charged(level, crystal.actor);
        if crystal.charged != charged {
            crystal.charged = charged;
            material.0 = if charged {
                looks.charged.clone()
            } else {
                looks.spent.clone()
            };
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<Shown>()
        .init_resource::<CrystalLooks>()
        .add_systems(
            Update,
            (
                sync.run_if(lab_active.or_else(shown)),
                animate.run_if(shown),
            )
                .chain()
                .in_set(LabSet::Draw),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    use asamu_sandbox::playground::load_playground;
    use asamu_sandbox::session::Session;

    /// A headless app in a session on the playground, with what a graybox
    /// run has for this module to change.
    fn app() -> App {
        let (mut game, script) = load_playground().unwrap();
        game.start();
        let mut lab = Lab::new(true);
        lab.phase = Phase::Active;
        lab.session = Some(Session::classic());
        lab.stage = Some(Stage::Playground);
        lab.dirs = None;
        let mut app = App::new();
        app.insert_resource(lab)
            .insert_resource(Sim::new(game, String::new()).with_script(Some(script)))
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .insert_resource(ClearColor(Color::BLACK))
            .insert_resource(GlobalAmbientLight {
                brightness: 123.0,
                ..default()
            })
            .insert_resource(crate::hud::DebugInfo(true))
            .insert_resource(LevelGizmos(true));
        build(&mut app);
        app
    }

    fn own(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<PlaygroundEntity>>()
            .iter(world)
            .count()
    }

    #[test]
    fn the_playground_is_drawn_while_it_is_played_and_everything_is_put_back() {
        let mut app = app();
        let stock = app
            .world_mut()
            .spawn((LevelVisual, Visibility::Inherited))
            .id();
        let camera = app
            .world_mut()
            .spawn((PlayerCamera, Transform::from_xyz(1.0, 2.0, 3.0)))
            .id();
        app.update();
        app.update();
        // One mesh per piece, a light per lamp, the sun and the sky.
        let level = playground();
        let lamps = level.pieces.iter().filter(|p| p.look == Look::Lamp).count();
        assert!(lamps > 0);
        assert_eq!(own(&mut app), level.pieces.len() + lamps + 2);
        // A clean picture: nothing of the graybox, no developer drawings.
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Hidden)
        );
        assert!(!app.world().resource::<crate::hud::DebugInfo>().0);
        assert!(!app.world().resource::<LevelGizmos>().0);
        assert_ne!(app.world().resource::<ClearColor>().0, Color::BLACK);
        assert_ne!(
            app.world().resource::<GlobalAmbientLight>().brightness,
            123.0
        );
        assert!(app.world().get::<DistanceFog>(camera).is_some());
        // The sky stays around the camera.
        let world = app.world_mut();
        let sky: Vec<Vec3> = world
            .query_filtered::<&Transform, With<SkyDome>>()
            .iter(world)
            .map(|t| t.translation)
            .collect();
        assert_eq!(sky, [Vec3::new(1.0, 2.0, 3.0)]);

        // What the level script moves is drawn where the script has it.
        {
            let mut sim = app.world_mut().resource_mut::<Sim>();
            let Sim { game, script, .. } = &mut *sim;
            for _ in 0..240 {
                script
                    .as_mut()
                    .unwrap()
                    .tick(game, &asamu_player::InputFrame::default());
            }
        }
        app.update();
        let moved: HashMap<u32, (sim_glam::Vec3, [i32; 3])> = app
            .world()
            .resource::<Sim>()
            .script
            .as_ref()
            .unwrap()
            .moved_actors()
            .into_iter()
            .map(|(id, location, rotation)| (id, (location, rotation)))
            .collect();
        let world = app.world_mut();
        let drawn: Vec<(u32, Transform)> = world
            .query::<(&Moving, &Transform)>()
            .iter(world)
            .map(|(piece, transform)| (piece.0, *transform))
            .collect();
        let movers = level
            .pieces
            .iter()
            .filter(|p| !matches!(p.role, Role::Fixed | Role::Decor | Role::Crystal))
            .count();
        assert_eq!(drawn.len(), movers);
        let mut carried = 0;
        for (actor, transform) in drawn {
            let piece = level
                .pieces
                .iter()
                .find(|p| p.actor == Some(actor))
                .unwrap();
            let Some((location, rotation)) = moved.get(&actor).copied() else {
                continue;
            };
            assert_eq!(transform, placement(location, rotation), "{}", piece.name);
            if matches!(piece.role, Role::Carried | Role::Gondola) {
                assert_ne!(
                    transform.translation,
                    placement(piece.location, piece.rotation).translation,
                    "{} did not move",
                    piece.name
                );
                carried += 1;
            }
        }
        assert!(carried >= 12);

        // The session leaves the playground: nothing of it stays.
        app.world_mut().resource_mut::<Lab>().phase = Phase::Idle;
        app.update();
        app.update();
        assert_eq!(own(&mut app), 0);
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Inherited)
        );
        assert!(app.world().resource::<crate::hud::DebugInfo>().0);
        assert!(app.world().resource::<LevelGizmos>().0);
        assert_eq!(app.world().resource::<ClearColor>().0, Color::BLACK);
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().brightness,
            123.0
        );
        assert!(app.world().get::<DistanceFog>(camera).is_none());
        assert!(app.world().resource::<Shown>().kept.is_none());
    }

    #[test]
    fn a_converted_level_behind_the_menu_is_taken_out_of_the_picture() {
        use asamu_assets::{ConvertedDir, PlanOptions};

        let mut app = app();
        app.insert_resource(ConvertedLevel {
            dir: ConvertedDir::new(std::path::PathBuf::from("conv")),
            level: "AG-Somewhere".to_owned(),
            options: PlanOptions::default(),
            settings: crate::converted::RenderSettings::default(),
            eye_height: 38.0,
            force_all_sublevels: false,
            phase: None,
            progress: (3, 0, 5),
        });
        let mesh = app
            .world_mut()
            .spawn(LevelEntity {
                actor_slot: 0,
                level: 0,
            })
            .id();
        let bsp = app.world_mut().spawn(LevelBsp).id();
        let other = app.world_mut().spawn(Transform::default()).id();
        app.update();
        app.update();
        assert!(app.world().get_entity(mesh).is_err());
        assert!(app.world().get_entity(bsp).is_err());
        assert!(
            app.world().get_entity(other).is_ok(),
            "only the level's own"
        );
        {
            let level = app.world().resource::<ConvertedLevel>();
            assert_eq!(level.level, PLAYGROUND_MAP);
            assert!(level.phase.is_none());
            assert_eq!(level.progress, (0, 0, 0));
        }
        // A plan that still spawned after it was cancelled goes as well.
        let late = app
            .world_mut()
            .spawn(LevelEntity {
                actor_slot: 1,
                level: 0,
            })
            .id();
        app.update();
        app.update();
        assert!(app.world().get_entity(late).is_err());
        // Another stage: the level's entities are the flow's again.
        app.world_mut().resource_mut::<Lab>().stage = Some(Stage::Map("AG-Somewhere".to_owned()));
        app.update();
        let kept = app
            .world_mut()
            .spawn(LevelEntity {
                actor_slot: 2,
                level: 0,
            })
            .id();
        app.update();
        app.update();
        assert!(app.world().get_entity(kept).is_ok());
    }

    #[test]
    fn every_look_has_its_own_colour() {
        let looks = [
            Look::Paving,
            Look::Island,
            Look::Rock,
            Look::Hook,
            Look::Ramp,
            Look::Rider,
            Look::Machine,
            Look::Crystal,
            Look::Hazard,
            Look::StartGate,
            Look::FinishGate,
            Look::Lamp,
        ];
        for (i, a) in looks.iter().enumerate() {
            for b in &looks[i + 1..] {
                let (pa, pb) = (paint_of(*a), paint_of(*b));
                let apart: f32 = (0..3).map(|k| (pa.top[k] - pb.top[k]).abs()).sum();
                assert!(apart > 0.12, "{a:?} and {b:?} read alike ({apart})");
            }
        }
        // The landmark lights differ from each other, and a number past the
        // list still has a colour.
        for (i, a) in BEACONS.iter().enumerate() {
            for b in &BEACONS[i + 1..] {
                let apart: f32 = (0..3).map(|k| (a[k] - b[k]).abs()).sum();
                assert!(apart > 0.3, "{a:?} and {b:?}");
            }
        }
        assert_eq!(paint_of(Look::Beacon(200)), paint_of(Look::Beacon(0)));
        // Only light sources are unlit; only the catch is see-through.
        for look in looks {
            let p = paint_of(look);
            assert_eq!(p.unlit, look == Look::Lamp, "{look:?}");
            assert_eq!(p.alpha < 1.0, look == Look::Hazard, "{look:?}");
        }
    }

    #[test]
    fn every_piece_becomes_a_closed_outward_facing_mesh() {
        let level = playground();
        for (index, shape) in level.shapes.iter().enumerate() {
            let flat = flat(shape, paint_of(Look::Island));
            assert_eq!(flat.positions.len(), shape.triangles.len() * 3, "{index}");
            assert_eq!(flat.normals.len(), flat.positions.len());
            assert_eq!(flat.colours.len(), flat.positions.len());
            let centre = flat
                .positions
                .iter()
                .map(|p| Vec3::from_array(*p))
                .sum::<Vec3>()
                / flat.positions.len() as f32;
            for (corners, normals) in flat.positions.chunks(3).zip(flat.normals.chunks(3)) {
                let [a, b, c] = [corners[0], corners[1], corners[2]].map(Vec3::from_array);
                let normal = Vec3::from_array(normals[0]);
                // The winding agrees with the normal, and the normal points
                // away from the middle of the solid.
                assert!((b - a).cross(c - a).dot(normal) > 0.0, "shape {index}");
                assert!(normal.dot(a - centre) > -1.0e-3, "shape {index}");
                assert!((normal.length() - 1.0).abs() < 1.0e-3);
            }
            assert!(
                flat.colours
                    .iter()
                    .flatten()
                    .all(|c| c.is_finite() && (0.0..=1.0).contains(c)),
                "shape {index}"
            );
        }
        // A flat top takes the top colour; a wall does not.
        let island = level.piece("hub").unwrap();
        let hub = flat(&level.shapes[island.shape], paint_of(Look::Paving));
        let top = linear(paint_of(Look::Paving).top, 1.0);
        let tops = hub
            .normals
            .iter()
            .zip(&hub.colours)
            .filter(|(n, _)| n[1] > 0.99)
            .count();
        assert!(tops > 0);
        assert!(
            hub.normals
                .iter()
                .zip(&hub.colours)
                .all(|(n, c)| (n[1] > 0.99) == (*c == top))
        );
        // Nothing to draw is nothing, not a panic.
        let nothing = Shape {
            vertices: Vec::new(),
            triangles: vec![[0, 1, 2]],
        };
        assert_eq!(flat(&nothing, paint_of(Look::Rock)), Flat::default());
    }

    #[test]
    fn a_piece_is_placed_where_the_simulation_has_it() {
        // Unturned: the render axes of a UE frame.
        let at = sim_glam::Vec3::new(100.0, 200.0, 300.0);
        let still = placement(at, [0; 3]);
        assert_eq!(still.translation, to_render(at));
        assert!(still.rotation.angle_between(Quat::IDENTITY) < 1.0e-4);
        // A quarter turn about the vertical (yaw): a point ahead of the
        // piece (UE +X) ends up to its right (UE +Y), as the simulation's
        // own rotation says.
        let turned = placement(at, [0, 16_384, 0]);
        let ahead = to_render(sim_glam::Vec3::X * 100.0);
        let right = to_render(sim_glam::Vec3::Y * 100.0);
        assert!((turned.rotation * ahead - right).length() < 1.0e-3);
        // A quarter roll: what was on its right is now below or above it,
        // never ahead.
        let rolled = placement(at, [0, 0, 16_384]);
        let moved = rolled.rotation * right;
        assert!(moved.dot(ahead).abs() < 1.0e-3 && moved.dot(right).abs() < 1.0e-3);
        // Every rotation the level script can report is a proper rotation.
        for rotation in [[1234, 0, 0], [0, -40_000, 77], [65_535, 65_535, 65_535]] {
            let t = placement(at, rotation);
            assert!((t.rotation.length() - 1.0).abs() < 1.0e-4, "{rotation:?}");
        }
    }

    #[test]
    fn the_sky_is_warm_at_the_horizon_and_blue_overhead() {
        let horizon = sky_colour(0.0);
        let zenith = sky_colour(1.0);
        assert!(horizon[0] > horizon[2], "{horizon:?}");
        assert!(zenith[2] > zenith[0], "{zenith:?}");
        // Continuous: no band in the sky.
        let mut last = sky_colour(-1.0);
        for step in 1..=400 {
            let now = sky_colour(-1.0 + step as f32 / 200.0);
            let jump: f32 = (0..3).map(|k| (now[k] - last[k]).abs()).sum();
            assert!(jump < 0.03, "at step {step}: {jump}");
            last = now;
        }
        assert_eq!(sky_colour(f32::NAN), sky_colour(0.0));
        // The dome is a whole sphere inside the far plane.
        let mesh = sky_mesh();
        assert_eq!(
            mesh.count_vertices(),
            ((SKY_RINGS + 1) * (SKY_SEGMENTS + 1)) as usize
        );
    }
}
