//! A Story About My Uncle — Rust/Bevy engine recreation (executable).
//!
//! **Graybox prototype.** It does not load original game data. It renders the
//! hand-made graybox level from `asamu-world` and drives the deterministic
//! simulation in `asamu-game` / `asamu-player` from Bevy's fixed-update
//! schedule. All gameplay logic lives in those pure crates; this file only
//! translates devices → `InputFrame`, and simulation state → camera, meshes,
//! gizmos and HUD.
//!
//! **Placeholder physics:** no gameplay constant has been recovered from the
//! original game yet; every parameter is a documented placeholder (see
//! `docs/PARITY.md`, "Placeholder parameters").
//!
//! Controls: click to capture the mouse, WASD move, mouse look, Space jump,
//! hold left mouse to grapple, R respawn, F9 start/stop trace recording,
//! Esc release the mouse (pauses).

use std::path::PathBuf;

use asamu_core::coords::{
    WorldScale, ue_extents_to_bevy, ue_pos_to_bevy, ue_right_flat, ue_view_to_bevy_rotation,
};
use asamu_core::glam as sim_glam;
use asamu_core::units::uu_per_s_to_presentation_m_per_s;
use asamu_game::{Game, GameState};
use asamu_player::{Aim, GrappleState, InputFrame};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

/// Render scale: UU → Bevy units, using the **presentation** convention
/// (50 UU per metre). Not a gameplay value; the simulation runs in UU.
const SCALE: WorldScale = WorldScale::PRESENTATION_METRES;

/// Mouse sensitivity in radians per mouse count. An app input setting, not a
/// gameplay constant.
const MOUSE_RADIANS_PER_COUNT: f32 = 0.0025;

/// Render-only offset (UU) of the rope's start point from the eye, so the rope
/// is visible in first person: down and to the right.
const ROPE_HAND_DOWN_UU: f32 = 14.0;
const ROPE_HAND_RIGHT_UU: f32 = 10.0;

const BANNER: &str =
    "ASAMU-decomp pre-alpha \u{b7} placeholder physics (no original constants yet)";

/// The simulation plus render-interpolation state.
#[derive(Resource)]
struct Sim {
    game: Game,
    /// Collision-centre position before / after the latest tick (UU).
    prev_position: sim_glam::Vec3,
    curr_position: sim_glam::Vec3,
}

impl Sim {
    fn snap_interpolation(&mut self) {
        let p = self.game.player().position;
        self.prev_position = p;
        self.curr_position = p;
    }
}

/// Input gathered between fixed ticks and consumed by the next tick.
#[derive(Resource, Default)]
struct PendingInput {
    look_yaw: f32,
    look_pitch: f32,
    jump: bool,
    /// The grapple only reads the mouse once the click that captured the
    /// cursor has been released.
    grapple_armed: bool,
}

#[derive(Component)]
struct PlayerCamera;

#[derive(Component)]
struct HudText;

#[derive(Component)]
struct Crosshair;

fn main() -> AppExit {
    let game = match Game::graybox() {
        Ok(game) => game,
        Err(err) => {
            eprintln!("failed to create the graybox game: {err}");
            return AppExit::error();
        }
    };
    let tick_rate_hz = game.clock().tick_rate_hz();
    let position = game.player().position;

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "ASAMU-decomp (pre-alpha graybox)".into(),
                ..default()
            }),
            ..default()
        }))
        // Fixed tick rate comes from the simulation clock (a runtime choice;
        // the original's tick model is UNKNOWN).
        .insert_resource(Time::<Fixed>::from_hz(tick_rate_hz))
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)))
        .insert_resource(GlobalAmbientLight {
            brightness: 400.0,
            ..default()
        })
        .insert_resource(Sim {
            game,
            prev_position: position,
            curr_position: position,
        })
        .init_resource::<PendingInput>()
        .add_systems(Startup, (spawn_level, spawn_camera_and_light, spawn_hud))
        .add_systems(
            RunFixedMainLoop,
            (cursor_and_pause, gather_input)
                .chain()
                .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
        )
        .add_systems(FixedUpdate, fixed_tick)
        .add_systems(Update, (sync_camera, draw_gizmos, update_hud))
        .run()
}

fn bevy_vec(v: sim_glam::Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

fn bevy_quat(q: sim_glam::Quat) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// UE position (UU) → Bevy render position.
fn to_render(v: sim_glam::Vec3) -> Vec3 {
    bevy_vec(ue_pos_to_bevy(v, SCALE))
}

fn spawn_level(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    sim: Res<Sim>,
) {
    let level = sim.game.level();
    let solid = materials.add(StandardMaterial {
        base_color: Color::srgb(0.72, 0.72, 0.70),
        perceptual_roughness: 0.95,
        ..default()
    });
    let grapple_surface = materials.add(StandardMaterial {
        base_color: Color::srgb(0.95, 0.55, 0.15),
        perceptual_roughness: 0.8,
        ..default()
    });
    let hook = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.75, 0.2),
        emissive: LinearRgba::rgb(0.8, 0.4, 0.05),
        ..default()
    });

    let mut spawn_box =
        |min: sim_glam::Vec3, max: sim_glam::Vec3, material: Handle<StandardMaterial>| {
            let size = bevy_vec(ue_extents_to_bevy(max - min, SCALE));
            commands.spawn((
                Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
                MeshMaterial3d(material),
                Transform::from_translation(to_render((min + max) * 0.5)),
            ));
        };
    for b in &level.static_boxes {
        let material = if b.grapple_able {
            grapple_surface.clone()
        } else {
            solid.clone()
        };
        spawn_box(b.min, b.max, material);
    }
    for g in &level.grapple_points {
        let h = sim_glam::Vec3::splat(g.half_extent);
        spawn_box(g.position - h, g.position + h, hook.clone());
    }
}

fn spawn_camera_and_light(mut commands: Commands) {
    commands.spawn((
        PlayerCamera,
        Camera3d::default(),
        Projection::from(PerspectiveProjection::default()),
        Transform::default(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::default().looking_to(Vec3::new(-0.4, -1.0, -0.3), Vec3::Y),
    ));
}

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            ..default()
        },
        children![(
            HudText,
            Text::new(BANNER),
            TextFont {
                font_size: FontSize::Px(15.0),
                ..default()
            },
            TextColor(Color::srgb(0.05, 0.05, 0.08)),
        )],
    ));
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            top: percent(50),
            ..default()
        },
        children![(
            Crosshair,
            Text::new("+"),
            TextFont {
                font_size: FontSize::Px(22.0),
                ..default()
            },
            TextColor(Color::WHITE),
        )],
    ));
}

/// Cursor capture, pause/resume, respawn and trace recording hotkeys.
fn cursor_and_pause(
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut sim: ResMut<Sim>,
    mut pending: ResMut<PendingInput>,
) {
    let grabbed = cursor.grab_mode != CursorGrabMode::None;
    if !grabbed && mouse.just_pressed(MouseButton::Left) {
        // Locked falls back to Confined on X11 (see bevy_window docs).
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
        match sim.game.state() {
            GameState::Boot => sim.game.start(),
            GameState::Paused => sim.game.resume(),
            GameState::Playing => {}
        }
        *pending = PendingInput::default();
        return;
    }
    if !grabbed {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
        sim.game.pause();
        *pending = PendingInput::default();
        return;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        sim.game.respawn();
        sim.snap_interpolation();
    }
    if keys.just_pressed(KeyCode::F9) {
        toggle_recording(&mut sim.game);
    }
}

fn toggle_recording(game: &mut Game) {
    let Some(trace) = game.stop_recording() else {
        game.start_recording();
        info!("trace recording started");
        return;
    };
    let dir = std::env::var_os("ASAMU_TRACE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = dir.join(format!(
        "asamu-runtime-trace-tick{}.jsonl",
        game.clock().tick()
    ));
    let result = std::fs::File::create(&path)
        .map_err(asamu_player::trace::TraceError::from)
        .and_then(|f| trace.write_jsonl(std::io::BufWriter::new(f)));
    match result {
        Ok(()) => info!(
            "wrote {} samples to {}",
            trace.samples.len(),
            path.display()
        ),
        Err(err) => warn!("could not write trace to {}: {err}", path.display()),
    }
}

/// Accumulates mouse look and latches jump presses until the next tick.
fn gather_input(
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    motion: Res<AccumulatedMouseMotion>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    sim: Res<Sim>,
    mut pending: ResMut<PendingInput>,
) {
    if cursor.grab_mode == CursorGrabMode::None || sim.game.state() != GameState::Playing {
        return;
    }
    // Screen +x (right) turns right (+yaw in UE3 convention); screen +y (down)
    // looks down (−pitch).
    pending.look_yaw += motion.delta.x * MOUSE_RADIANS_PER_COUNT;
    pending.look_pitch -= motion.delta.y * MOUSE_RADIANS_PER_COUNT;
    if keys.just_pressed(KeyCode::Space) {
        pending.jump = true;
    }
    if !mouse.pressed(MouseButton::Left) {
        pending.grapple_armed = true;
    }
}

fn axis(keys: &ButtonInput<KeyCode>, positive: [KeyCode; 2], negative: [KeyCode; 2]) -> f32 {
    let pos = positive.iter().any(|k| keys.pressed(*k));
    let neg = negative.iter().any(|k| keys.pressed(*k));
    f32::from(u8::from(pos)) - f32::from(u8::from(neg))
}

/// One simulation tick per fixed step.
fn fixed_tick(
    mut sim: ResMut<Sim>,
    mut pending: ResMut<PendingInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    if sim.game.state() != GameState::Playing {
        return;
    }
    let input = InputFrame {
        move_forward: axis(
            &keys,
            [KeyCode::KeyW, KeyCode::ArrowUp],
            [KeyCode::KeyS, KeyCode::ArrowDown],
        ),
        move_right: axis(
            &keys,
            [KeyCode::KeyD, KeyCode::ArrowRight],
            [KeyCode::KeyA, KeyCode::ArrowLeft],
        ),
        look_yaw_delta: std::mem::take(&mut pending.look_yaw),
        look_pitch_delta: std::mem::take(&mut pending.look_pitch),
        jump_pressed: std::mem::take(&mut pending.jump),
        jump_held: keys.pressed(KeyCode::Space),
        grapple_held: pending.grapple_armed && mouse.pressed(MouseButton::Left),
    };
    let before = sim.game.player().position;
    if let Some(report) = sim.game.tick(&input) {
        sim.prev_position = before;
        sim.curr_position = sim.game.player().position;
        if report.respawned {
            sim.snap_interpolation();
        }
        if let Some(id) = report.checkpoint_activated {
            info!("checkpoint {id} activated");
        }
    }
}

/// Places the camera at the interpolated eye position with the simulation's
/// view rotation (plus look input not yet consumed by a tick).
fn sync_camera(
    sim: Res<Sim>,
    pending: Res<PendingInput>,
    fixed: Res<Time<Fixed>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&mut Transform, &mut Projection), With<PlayerCamera>>,
) {
    let (mut transform, mut projection) = camera.into_inner();
    let params = sim.game.params();
    let player = sim.game.player();
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);
    let centre = sim.prev_position.lerp(sim.curr_position, alpha);
    let eye = centre + sim_glam::Vec3::Z * params.camera.eye_height.value;
    let max_pitch = params.camera.max_pitch_degrees.value.to_radians();
    let yaw = player.yaw + pending.look_yaw;
    let pitch = (player.pitch + pending.look_pitch).clamp(-max_pitch, max_pitch);
    transform.translation = to_render(eye);
    transform.rotation = bevy_quat(ue_view_to_bevy_rotation(yaw, pitch));

    // The FOV parameter is horizontal (UE3 convention, TENTATIVE for ASAMU);
    // Bevy's perspective FOV is vertical.
    if let Projection::Perspective(perspective) = projection.as_mut() {
        let aspect = (window.width() / window.height().max(1.0)).max(0.1);
        let horizontal = params.camera.fov_degrees.value.to_radians();
        perspective.fov = 2.0 * ((horizontal * 0.5).tan() / aspect).atan();
    }
}

fn draw_gizmos(sim: Res<Sim>, fixed: Res<Time<Fixed>>, mut gizmos: Gizmos) {
    let game = &sim.game;
    let params = game.params();
    let player = game.player();
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);
    let centre = sim.prev_position.lerp(sim.curr_position, alpha);
    let eye = centre + sim_glam::Vec3::Z * params.camera.eye_height.value;

    if let GrappleState::Attached {
        anchor,
        rope_length,
    } = player.grapple
    {
        let hand = eye - sim_glam::Vec3::Z * ROPE_HAND_DOWN_UU
            + ue_right_flat(player.yaw) * ROPE_HAND_RIGHT_UU;
        let taut = centre.distance(anchor) >= rope_length - 1.0;
        let color = if taut {
            Color::srgb(0.15, 0.1, 0.05)
        } else {
            Color::srgb(0.45, 0.35, 0.25)
        };
        gizmos.line(to_render(hand), to_render(anchor), color);
        gizmos.sphere(to_render(anchor), 0.12, Color::srgb(1.0, 0.3, 0.1));
    } else {
        match game.aim() {
            Aim::Grappleable { point, .. } => {
                gizmos.sphere(to_render(point), 0.15, Color::srgb(0.1, 0.9, 0.2));
            }
            Aim::Blocked { point, .. } => {
                gizmos.sphere(to_render(point), 0.08, Color::srgb(0.9, 0.1, 0.1));
            }
            Aim::OutOfRange => {}
        }
    }

    for c in &game.level().checkpoints {
        let size = bevy_vec(ue_extents_to_bevy(c.max - c.min, SCALE));
        let active = game.active_checkpoint() == Some(c.id);
        let color = if active {
            Color::srgb(0.1, 0.8, 0.3)
        } else {
            Color::srgb(0.2, 0.4, 0.9)
        };
        gizmos.cube(
            Transform::from_translation(to_render((c.min + c.max) * 0.5)).with_scale(size),
            color,
        );
    }
}

fn update_hud(
    sim: Res<Sim>,
    mut hud: Single<&mut Text, (With<HudText>, Without<Crosshair>)>,
    crosshair: Single<&mut TextColor, With<Crosshair>>,
) {
    let game = &sim.game;
    let player = game.player();
    let speed = player.speed();
    let grapple = match player.grapple {
        GrappleState::Idle => "idle".to_owned(),
        GrappleState::Attached { rope_length, .. } => {
            format!("attached (rope {rope_length:.0} uu)")
        }
    };
    let aim = game.aim();
    let aim_text = match aim {
        Aim::Grappleable { distance, .. } => format!("target {distance:.0} uu"),
        Aim::Blocked { distance, .. } => format!("not grapple-able ({distance:.0} uu)"),
        Aim::OutOfRange => "nothing in range".to_owned(),
    };
    let state = match game.state() {
        GameState::Boot => "click to play",
        GameState::Paused => "paused - click to resume",
        GameState::Playing => "playing",
    };
    let checkpoint = game
        .active_checkpoint()
        .map_or_else(|| "none".to_owned(), |id| id.to_string());
    hud.0 = format!(
        "{BANNER}\n\
         speed {speed:.0} uu/s ({:.1} m/s presentation) | horizontal {:.0} uu/s | grounded {}\n\
         grapple {grapple} | aim: {aim_text}\n\
         tick {} @ {:.0} Hz | checkpoint {checkpoint} | respawns {} | {state}{}\n\
         WASD move | mouse look | Space jump | hold LMB grapple | R respawn | F9 record trace | Esc release mouse",
        uu_per_s_to_presentation_m_per_s(speed),
        player.horizontal_speed(),
        if player.grounded { "yes" } else { "no" },
        game.clock().tick(),
        game.clock().tick_rate_hz(),
        game.respawn_count(),
        if game.is_recording() { " | REC" } else { "" },
    );
    let mut color = crosshair.into_inner();
    color.0 = match aim {
        Aim::Grappleable { .. } => Color::srgb(0.2, 1.0, 0.3),
        Aim::Blocked { .. } => Color::srgb(1.0, 0.4, 0.4),
        Aim::OutOfRange => Color::WHITE,
    };
}
