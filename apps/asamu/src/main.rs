//! A Story About My Uncle — Rust/Bevy engine recreation (executable).
//!
//! **Graybox prototype.** It does not load original game data. It renders the
//! hand-made graybox level from `asamu-world` and drives the deterministic
//! simulation in `asamu-game` / `asamu-player` from Bevy's fixed-update
//! schedule. All gameplay logic lives in those pure crates; this file only
//! translates devices → `InputFrame`, and simulation state → camera, meshes,
//! gizmos and HUD.
//!
//! **Movement and abilities use the original's values and rules**: by
//! default the player runs the port of the original's native pawn physics
//! with `PlayerParams::asamu_original()` (class defaults and config values of
//! the original, each with provenance) and the ASAMU script layer: jump
//! release damping, sprint, story mode and zoom, landing, eye height and
//! bob, power jump, the original **grapple gun** (`asamu.GrappleGun`: pull
//! of 10⁷/d uu/s² in flying physics capped at 2000 uu/s, release below
//! 200 uu, grapple budget refilled by landings) and the **rocket boots**.
//! The graybox level enables every ability with 3 grapples (a test
//! configuration; the original's Kismet sets them per level). The on-screen
//! banner says which parameters are original and which are placeholders
//! (see `docs/PARITY.md`).
//!
//! Command line (debugging): `--placeholder` runs the old placeholder model
//! with the placeholder parameters (and the placeholder rope grapple);
//! `--placeholder-movement` keeps the original parameters on the
//! placeholder model. `ASAMU_MOVEMENT=placeholder` is the same as
//! `--placeholder`.
//!
//! Controls follow the original's `DefaultInput.ini` keyboard bindings: WASD
//! move, mouse look, Space jump (release damps the jump; in the air it also
//! fires the rocket boots), left shift sprint, left mouse fire (grapple;
//! releasing the button releases the grapple), hold right mouse to power
//! jump (zoom in story mode), E or Enter use, F7 quick load (respawn at the
//! checkpoint). Not original (debug/app): click to capture the mouse, arrow
//! keys move, R respawn, F2 toggle story mode, F3 cycle the grapple capacity
//! (0/1/2/3/unlimited), F4 toggle the rocket boots, F6 activate the attractor
//! pad (stand-ins for the original's Kismet actions), F9 start/stop trace
//! recording, Esc release the mouse (pauses). Mouse sensitivity is an app
//! setting: the original's `PlayerInput` look scaling (`MouseSensitivity`
//! 30, `LookRightScale` 300, `LookUpScale` −250) is not ported because its
//! exact formula is not specified yet.

use std::path::PathBuf;

use asamu_core::coords::{
    WorldScale, ue_extents_to_bevy, ue_pos_to_bevy, ue_right_flat, ue_view_to_bevy_rotation,
};
use asamu_core::glam as sim_glam;
use asamu_core::units::uu_per_s_to_presentation_m_per_s;
use asamu_game::{Game, GameState};
use asamu_player::{
    Aim, BootsStateName, GrappleState, InputFrame, MovementModelKind, PawnStateName,
};
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

/// The banner's first lines: which model and which parameter set runs.
fn banner(game: &Game) -> String {
    let report = game.params().provenance_report();
    let placeholders: Vec<&str> = report
        .iter()
        .filter(|e| e.provenance.is_placeholder())
        .map(|e| e.name.as_str())
        .collect();
    let original = report.len() - placeholders.len();
    let movement = match game.movement_model() {
        MovementModelKind::Ue3Pawn => "native pawn physics port",
        MovementModelKind::Placeholder => "PLACEHOLDER movement model",
    };
    if game.uses_original_params() {
        let p = game.params();
        let m = &p.movement;
        let gun = p
            .gun
            .as_ref()
            .map(|g| {
                format!(
                    "GrappleGun: range {}, release {}, AirSpeed {}, pull 10^7/d",
                    g.max_distance.value, g.release_distance.value, g.grapple_accel.value
                )
            })
            .unwrap_or_else(|| "no grapple gun".to_owned());
        format!(
            "ASAMU-decomp pre-alpha \u{b7} {movement} + ASAMU script layer (original grapple gun, rocket boots)\n\
             ORIGINAL values ({original} params): GroundSpeed {}, AccelRate {}, JumpZ {}, AirControl {}, \
             cylinder {}/{}, eye {}, FOV {} \u{b7} {gun}\n\
             PLACEHOLDER: {} params read only by the debug models (placeholder movement, rope grapple) \u{b7} \
             graybox abilities: everything on, 3 grapples (test configuration)",
            m.max_ground_speed.value,
            m.ground_acceleration.value,
            m.jump_velocity.value,
            m.air_control.value,
            m.capsule_radius.value,
            m.capsule_half_height.value,
            p.camera.eye_height.value,
            p.camera.fov_degrees.value,
            placeholders.len()
        )
    } else {
        format!(
            "ASAMU-decomp pre-alpha \u{b7} {movement} \u{b7} PLACEHOLDER parameters \
             ({} of {}; debug configuration, no ASAMU script layer)",
            placeholders.len(),
            report.len()
        )
    }
}

/// Which configuration the command line / environment selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Config {
    /// Original parameters on the native-physics port (default).
    Original,
    /// Original parameters on the placeholder model.
    PlaceholderMovement,
    /// Placeholder parameters on the placeholder model (legacy graybox).
    Placeholder,
}

const USAGE: &str = "usage: asamu [--original | --placeholder | --placeholder-movement]\n  \
    --original              original parameters on the native pawn physics port (default)\n  \
    --placeholder           placeholder parameters and model (debugging)\n  \
    --placeholder-movement  original parameters on the placeholder model (debugging)";

/// The configuration, or `None` when only the usage was requested.
fn parse_config() -> Result<Option<Config>, String> {
    let mut config = match std::env::var("ASAMU_MOVEMENT").as_deref() {
        Ok("placeholder") => Config::Placeholder,
        Ok("ue3") | Ok("original") | Err(_) => Config::Original,
        Ok(other) => return Err(format!("unknown ASAMU_MOVEMENT value {other:?}")),
    };
    for arg in std::env::args().skip(1) {
        config = match arg.as_str() {
            "--placeholder" => Config::Placeholder,
            "--placeholder-movement" => Config::PlaceholderMovement,
            "--original" => Config::Original,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        };
    }
    Ok(Some(config))
}

/// The simulation plus render-interpolation state.
#[derive(Resource)]
struct Sim {
    game: Game,
    /// First banner line (configuration and parameter provenance).
    banner: String,
    /// Collision-centre position before / after the latest tick (UU).
    prev_position: sim_glam::Vec3,
    curr_position: sim_glam::Vec3,
    /// Eye (view) position before / after the latest tick (UU), including
    /// the script layer's eye height and walk bob.
    prev_eye: sim_glam::Vec3,
    curr_eye: sim_glam::Vec3,
}

impl Sim {
    fn snap_interpolation(&mut self) {
        let p = self.game.player().position;
        self.prev_position = p;
        self.curr_position = p;
        let e = self.game.eye_position();
        self.prev_eye = e;
        self.curr_eye = e;
    }
}

/// Input gathered between fixed ticks and consumed by the next tick.
#[derive(Resource, Default)]
struct PendingInput {
    look_yaw: f32,
    look_pitch: f32,
    jump: bool,
    /// `use` (E / Enter) pressed since the last tick.
    use_key: bool,
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

/// A rendered mover (index into `Level::movers`).
#[derive(Component)]
struct MoverVisual(usize);

fn main() -> AppExit {
    let config = match parse_config() {
        Ok(Some(config)) => config,
        Ok(None) => {
            println!("{USAGE}");
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return AppExit::error();
        }
    };
    let game = match config {
        Config::Original => Game::graybox(),
        Config::PlaceholderMovement => {
            Game::graybox().map(|g| g.with_movement_model(MovementModelKind::Placeholder))
        }
        Config::Placeholder => Game::graybox_placeholder(),
    };
    let game = match game {
        Ok(game) => game,
        Err(err) => {
            eprintln!("failed to create the graybox game: {err}");
            return AppExit::error();
        }
    };
    let tick_rate_hz = game.clock().tick_rate_hz();
    let position = game.player().position;
    let eye = game.eye_position();
    let banner = banner(&game);
    println!("{banner}");

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
            banner,
            prev_position: position,
            curr_position: position,
            prev_eye: eye,
            curr_eye: eye,
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
        .add_systems(Update, (sync_camera, sync_movers, draw_gizmos, update_hud))
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
    let crystal = materials.add(StandardMaterial {
        base_color: Color::srgb(0.3, 0.9, 1.0),
        emissive: LinearRgba::rgb(0.1, 0.5, 0.7),
        ..default()
    });
    let flower = materials.add(StandardMaterial {
        base_color: Color::srgb(0.8, 0.4, 1.0),
        emissive: LinearRgba::rgb(0.4, 0.1, 0.6),
        ..default()
    });
    let interactable = materials.add(StandardMaterial {
        base_color: Color::srgb(0.95, 0.9, 0.3),
        ..default()
    });
    let mover_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.45, 0.2),
        perceptual_roughness: 0.7,
        ..default()
    });
    for c in &level.crystals {
        let h = sim_glam::Vec3::splat(c.half_extent);
        spawn_box(c.center - h, c.center + h, crystal.clone());
    }
    for f in &level.flowers {
        let h = sim_glam::Vec3::splat(f.half_extent);
        spawn_box(f.center - h, f.center + h, flower.clone());
    }
    for i in &level.interactables {
        spawn_box(i.min, i.max, interactable.clone());
    }
    for (index, m) in level.movers.iter().enumerate() {
        let size = bevy_vec(ue_extents_to_bevy(m.max - m.min, SCALE));
        commands.spawn((
            MoverVisual(index),
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(mover_material.clone()),
            Transform::from_translation(to_render((m.min + m.max) * 0.5)),
        ));
    }
}

/// Moves the rendered movers to their simulated positions.
fn sync_movers(sim: Res<Sim>, mut movers: Query<(&MoverVisual, &mut Transform)>) {
    let level = sim.game.level();
    let objects = sim.game.objects();
    for (visual, mut transform) in &mut movers {
        if let (Some(m), Some(offset)) = (
            level.movers.get(visual.0),
            objects.mover_offsets.get(visual.0),
        ) {
            transform.translation = to_render((m.min + m.max) * 0.5 + *offset);
        }
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

fn spawn_hud(mut commands: Commands, sim: Res<Sim>) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            ..default()
        },
        children![(
            HudText,
            Text::new(sim.banner.clone()),
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
    // R (debug) and F7 (the original's QuickLoad: restart from the
    // checkpoint) respawn.
    if keys.just_pressed(KeyCode::KeyR) || keys.just_pressed(KeyCode::F7) {
        sim.game.respawn();
        sim.snap_interpolation();
    }
    // F2 (debug): story mode, which the original toggles from Kismet.
    if keys.just_pressed(KeyCode::F2) {
        let on = sim.game.toggle_story_mode();
        info!("story mode {}", if on { "on" } else { "off" });
    }
    // F3/F4/F6 (debug): stand-ins for the original's Kismet actions
    // (SeqAct_SetMaxGrapples, SeqAct_ToggleRocketBoots, SeqAct_ToggleAttractor).
    if keys.just_pressed(KeyCode::F3) {
        let next = match sim.game.player().script.gun.max_grapples {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => -1,
            _ => 0,
        };
        sim.game.set_max_grapples(next);
        info!(
            "grapple capacity {}",
            sim.game.player().script.gun.max_grapples
        );
    }
    if keys.just_pressed(KeyCode::F4) {
        let on = !sim.game.player().script.boots.enabled;
        sim.game.enable_rocket_boots(on);
        info!("rocket boots {}", if on { "enabled" } else { "disabled" });
    }
    if keys.just_pressed(KeyCode::F6) {
        let pads: Vec<u32> = sim.game.level().attractors.iter().map(|a| a.id).collect();
        for id in pads {
            sim.game.activate_attractor(id);
        }
        info!("attractor pads activated");
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
    if keys.just_pressed(KeyCode::KeyE) || keys.just_pressed(KeyCode::Enter) {
        pending.use_key = true;
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
        sprint_held: keys.pressed(KeyCode::ShiftLeft),
        power_jump_held: mouse.pressed(MouseButton::Right),
        use_pressed: std::mem::take(&mut pending.use_key),
    };
    let before = sim.game.player().position;
    let eye_before = sim.game.eye_position();
    if let Some(report) = sim.game.tick(&input) {
        sim.prev_position = before;
        sim.curr_position = sim.game.player().position;
        sim.prev_eye = eye_before;
        sim.curr_eye = sim.game.eye_position();
        if report.respawned {
            sim.snap_interpolation();
        }
        if let Some(id) = report.checkpoint_activated {
            info!("checkpoint {id} activated");
        }
        if let Some(landing) = report.events.landing
            && landing.hard
        {
            info!("hard landing at {:.0} uu/s", landing.velocity_z);
        }
        if report.events.use_requested {
            info!("use (story mode): the original interacts through the fire button");
        }
        for event in report.world.iter() {
            info!("world: {event:?}");
        }
        if let Some(fire) = report.events.gun.fire {
            info!("grapple fire: {fire:?}");
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
    let eye = sim.prev_eye.lerp(sim.curr_eye, alpha);
    let max_pitch = params.camera.max_pitch_degrees.value.to_radians();
    let yaw = player.yaw + pending.look_yaw;
    let pitch = (player.pitch + pending.look_pitch).clamp(-max_pitch, max_pitch);
    transform.translation = to_render(eye);
    transform.rotation = bevy_quat(ue_view_to_bevy_rotation(yaw, pitch));

    // The FOV is horizontal (UE3 convention); Bevy's perspective FOV is
    // vertical. The story-mode zoom changes the run-time FOV.
    if let Projection::Perspective(perspective) = projection.as_mut() {
        let aspect = (window.width() / window.height().max(1.0)).max(0.1);
        let horizontal = sim.game.fov().to_radians();
        perspective.fov = 2.0 * ((horizontal * 0.5).tan() / aspect).atan();
    }
}

fn draw_gizmos(sim: Res<Sim>, fixed: Res<Time<Fixed>>, mut gizmos: Gizmos) {
    let game = &sim.game;
    let player = game.player();
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);
    let centre = sim.prev_position.lerp(sim.curr_position, alpha);
    let eye = sim.prev_eye.lerp(sim.curr_eye, alpha);

    let hand = eye - sim_glam::Vec3::Z * ROPE_HAND_DOWN_UU
        + ue_right_flat(player.yaw) * ROPE_HAND_RIGHT_UU;
    if let Some(anchor) = player.script.gun.anchor() {
        // The original grapple: a beam, no rope.
        gizmos.line(
            to_render(hand),
            to_render(anchor),
            Color::srgb(0.2, 0.6, 1.0),
        );
        gizmos.sphere(to_render(anchor), 0.12, Color::srgb(0.3, 0.7, 1.0));
    } else if let GrappleState::Attached {
        anchor,
        rope_length,
    } = player.grapple
    {
        let taut = centre.distance(anchor) >= rope_length - 1.0;
        let color = if taut {
            Color::srgb(0.15, 0.1, 0.05)
        } else {
            Color::srgb(0.45, 0.35, 0.25)
        };
        gizmos.line(to_render(hand), to_render(anchor), color);
        gizmos.sphere(to_render(anchor), 0.12, Color::srgb(1.0, 0.3, 0.1));
    } else if let Some(aim) = game.gun_aim() {
        if aim.impact.hit.is_some() {
            let color = if aim.acceptable {
                Color::srgb(0.1, 0.9, 0.2)
            } else {
                Color::srgb(0.9, 0.1, 0.1)
            };
            gizmos.sphere(to_render(aim.impact.location), 0.1, color);
        }
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

    // Crystal charge (outline), attractor pads.
    let level = game.level();
    for c in &level.crystals {
        let charged = game.objects().crystal_charged(level, c.id);
        let size = bevy_vec(ue_extents_to_bevy(
            sim_glam::Vec3::splat(c.half_extent * 2.2),
            SCALE,
        ));
        let color = if charged {
            Color::srgb(0.2, 1.0, 0.4)
        } else {
            Color::srgb(0.4, 0.4, 0.4)
        };
        gizmos.cube(
            Transform::from_translation(to_render(c.center)).with_scale(size),
            color,
        );
    }
    for (a, state) in level.attractors.iter().zip(&game.objects().attractors) {
        let color = if state.active {
            Color::srgb(1.0, 0.2, 0.2)
        } else {
            Color::srgb(0.5, 0.2, 0.2)
        };
        gizmos.sphere(to_render(a.position), 0.4, color);
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
    let gun = &player.script.gun;
    let grapple = if game.params().gun.is_some() {
        let state = match gun.anchor() {
            Some(_) => format!("attached (d {:.0} uu)", gun.distance),
            None => "idle".to_owned(),
        };
        let capacity = if gun.max_grapples >= 32_767 {
            "unlimited".to_owned()
        } else {
            gun.max_grapples.to_string()
        };
        format!(
            "{state} | used {}/{capacity} | latch {} | weapon {:?}",
            gun.times_grappled,
            if gun.can_grapple { "open" } else { "closed" },
            gun.weapon
        )
    } else {
        match player.grapple {
            GrappleState::Idle => "placeholder rope: idle".to_owned(),
            GrappleState::Attached { rope_length, .. } => {
                format!("placeholder rope: attached (rope {rope_length:.0} uu)")
            }
        }
    };
    let aim_text = match game.gun_aim() {
        Some(aim) => match aim.impact.hit {
            Some(_) if aim.acceptable => format!("target {:.0} uu", aim.distance),
            Some(_) => format!("no grapple ({:.0} uu)", aim.distance),
            None => "nothing hit".to_owned(),
        },
        None => match game.aim() {
            Aim::Grappleable { distance, .. } => format!("target {distance:.0} uu"),
            Aim::Blocked { distance, .. } => format!("not grapple-able ({distance:.0} uu)"),
            Aim::OutOfRange => "nothing in range".to_owned(),
        },
    };
    let boots = &player.script.boots;
    let boots_text = if !boots.spawned {
        "none".to_owned()
    } else if !boots.enabled {
        "disabled".to_owned()
    } else {
        match boots.state {
            BootsStateName::Ready => "ready".to_owned(),
            BootsStateName::Boosting => format!("boosting ({:.2} s)", boots.tau),
            BootsStateName::Unavailable | BootsStateName::UnavailableAndPlayedSound => {
                "used (land to re-arm)".to_owned()
            }
        }
    };
    let state = match game.state() {
        GameState::Boot => "click to play",
        GameState::Paused => "paused - click to resume",
        GameState::Playing => "playing",
    };
    let checkpoint = game
        .active_checkpoint()
        .map_or_else(|| "none".to_owned(), |id| id.to_string());
    let script = &player.script;
    let pawn_line = if script.started {
        let mode = match script.code.state {
            PawnStateName::StoryState => "story",
            PawnStateName::Zooming => "story (zoom)",
            _ => {
                if script.sprint.active {
                    "sprint"
                } else {
                    "walk"
                }
            }
        };
        format!(
            "pawn {:?} | {mode} | GroundSpeed {:.0} | AirControl {:.2} | power jump {:?}{} | \
             move lock {} | eye {:.1} | FOV {:.1}",
            script.code.state,
            script.ground_speed,
            script.air_control,
            script.power_jump.state,
            if script.power_jump.charged {
                " (charged)"
            } else {
                ""
            },
            script.move_input_lock,
            script.eye_height,
            script.fov,
        )
    } else {
        "pawn script layer off (placeholder parameters)".to_owned()
    };
    hud.0 = format!(
        "{}\n\
         speed {speed:.0} uu/s ({:.1} m/s presentation) | horizontal {:.0} uu/s | {}\n\
         {pawn_line}\n\
         grapple {grapple} | aim: {aim_text} | rocket boots {boots_text}\n\
         tick {} @ {:.0} Hz | checkpoint {checkpoint} | respawns {} | {state}{}\n\
         WASD move | mouse look | Space jump (air: rocket boost) | LShift sprint | LMB grapple | hold RMB power jump / zoom | \
         E use | F7/R respawn | debug: F2 story mode, F3 grapple capacity, F4 boots, F6 attractor, F9 record trace | Esc release mouse",
        sim.banner,
        uu_per_s_to_presentation_m_per_s(speed),
        player.horizontal_speed(),
        if player.pawn.flying {
            "flying (grapple)"
        } else if player.grounded {
            "walking"
        } else {
            "falling"
        },
        game.clock().tick(),
        game.clock().tick_rate_hz(),
        game.respawn_count(),
        if game.is_recording() { " | REC" } else { "" },
    );
    let mut color = crosshair.into_inner();
    color.0 = if game.params().gun.is_some() {
        // The original HUD crosshair state (GRAPPLE.md G-TG-2).
        if game.crosshair() {
            Color::srgb(0.2, 1.0, 0.3)
        } else {
            Color::WHITE
        }
    } else {
        match game.aim() {
            Aim::Grappleable { .. } => Color::srgb(0.2, 1.0, 0.3),
            Aim::Blocked { .. } => Color::srgb(1.0, 0.4, 0.4),
            Aim::OutOfRange => Color::WHITE,
        }
    };
}
