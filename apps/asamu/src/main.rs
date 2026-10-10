//! A Story About My Uncle — Rust/Bevy engine recreation (executable).
//!
//! Two modes:
//!
//! - **Graybox** (default, no game data needed): renders the hand-made
//!   graybox level from `asamu-world` and drives the deterministic
//!   simulation in `asamu-game` / `asamu-player` from Bevy's fixed-update
//!   schedule. All gameplay logic lives in those pure crates; this file only
//!   translates devices → `InputFrame`, and simulation state → camera,
//!   meshes, gizmos and HUD.
//! - **Converted original level** (`--converted DIR --level NAME`, see
//!   [`converted`]): renders a level from the user-local output of
//!   `asamu-import` (scene JSON, glTF meshes, DDS textures, materials when
//!   converted). The level's game with its Kismet and NPCs
//!   (`asamu_game::load_level_with_kismet_options`: triangle collision, the
//!   gameplay actors, the level script, Matinee movers, NPCs) loads on a
//!   background task; once it is ready the simulation drives the
//!   first-person camera exactly as in the graybox, and every fixed tick runs
//!   `LevelScript::tick` (Kismet, then the game frame) whose outputs
//!   [`kismet`] turns into sound, UI, camera and rendering changes
//!   (`docs/INTEGRATION.md`). Until then, when it fails, or with `--fly`, a
//!   render-only fly camera ([`fly`]) is used.
//!
//! The render scale is the presentation convention (50 UU per render unit);
//! the simulation stays in UU.
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
//! Command line: see [`USAGE`]. Debugging: `--placeholder` runs the old
//! placeholder model with the placeholder parameters (and the placeholder
//! rope grapple); `--placeholder-movement` keeps the original parameters on
//! the placeholder model. `ASAMU_MOVEMENT=placeholder` is the same as
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
//! recording, Esc pauses (pause menu). Mouse sensitivity is an app
//! setting (settings menu): the original's `PlayerInput` look scaling
//! (`MouseSensitivity` 30, `LookRightScale` 300, `LookUpScale` −250) is not
//! ported because its exact formula is not specified yet.
//!
//! Menus and saves ([`ui`], `docs/UI_AND_SAVES.md`): the app starts in the
//! main menu (graybox: New Game plays the test level; converted data without
//! `--level`: New Game / Continue / chapter select load the chapters, with
//! saves in the user data directory); `--level`, `--walk`, `--screenshot`
//! and `--no-menu` skip it.

mod audio;
mod capture;
mod converted;
mod fly;
mod gamepad;
mod hud;
mod kismet;
mod lightmaps;
mod npc;
mod particles;
mod post;
mod timetrial;
mod ui;
mod vfx;
mod water;

use std::path::PathBuf;

use asamu_assets::{ConvertedDir, LightMapping, PlanOptions};
use asamu_core::coords::{
    WorldScale, ue_extents_to_bevy, ue_pos_to_bevy, ue_right_flat, ue_view_to_bevy_rotation,
};
use asamu_core::glam as sim_glam;
use asamu_core::units::uu_per_s_to_presentation_m_per_s;
use asamu_game::{Game, GameState, LevelScript};
use asamu_player::{
    Aim, BootsStateName, GrappleState, InputFrame, MovementModelKind, PawnStateName,
};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

/// Render scale: UU → Bevy units, using the **presentation** convention
/// (50 UU per metre). Not a gameplay value; the simulation runs in UU.
pub(crate) const SCALE: WorldScale = WorldScale::PRESENTATION_METRES;

/// Render layer of the first-person overlay (the hands): drawn by its own
/// camera over the world, lit by the level's lights (ours, presentation).
pub(crate) const FIRST_PERSON_LAYER: usize = 1;

/// Mouse sensitivity in radians per mouse count. An app input setting, not a
/// gameplay constant.
pub(crate) const MOUSE_RADIANS_PER_COUNT: f32 = 0.0025;

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
        let abilities = if game.scene_map().is_some() {
            "converted level: abilities, story mode and movers from the map's Kismet, NPCs, triangle collision"
        } else {
            "graybox abilities: everything on, 3 grapples (test configuration)"
        };
        format!(
            "ASAMU-decomp pre-alpha | {movement} + ASAMU script layer (original grapple gun, rocket boots)\n\
             ORIGINAL values ({original} params): GroundSpeed {}, AccelRate {}, JumpZ {}, AirControl {}, \
             cylinder {}/{}, eye {}, FOV {} | {gun}\n\
             PLACEHOLDER: {} params read only by the debug models (placeholder movement, rope grapple) | \
             {abilities}",
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
            "ASAMU-decomp pre-alpha | {movement} | PLACEHOLDER parameters \
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

const USAGE: &str = "usage: asamu [options]\n\
    \n\
    graybox (default, no game data needed):\n  \
    --original              original parameters on the native pawn physics port (default)\n  \
    --placeholder           placeholder parameters and model (debugging)\n  \
    --placeholder-movement  original parameters on the placeholder model (debugging)\n\
    \n\
    converted original level (output of `asamu-import textures/meshes/levels [materials]`):\n  \
    --converted DIR         converted data directory (default: the importer's default output\n                          \
    directory, or ASAMU_CONVERTED_DIR); without --level: main menu (New Game, Continue,\n                          \
    chapter select; saves in the user data directory, ASAMU_SAVE_DIR overrides it)\n  \
    --level NAME            start straight in this level, e.g. AG-Workshop (saves stay in memory)\n  \
    --fly                   render-only fly camera (no collision)\n  \
    --all-sublevels         always show sub-levels Kismet streams in later (e.g. TheCore in AG-IceCave;\n                          \
    by default they show while the level script has them streamed in)\n  \
    --camera X,Y,Z[,YAW,PITCH]  start the fly camera at a UE3 position (UU), yaw/pitch in degrees;\n                          \
    with gameplay, a debug teleport of the player's eye there at the start\n  \
    --light-scale F         multiply converted light intensities (approximation, default 1)\n  \
    --light-shadows N       point/spot lights with shadow maps (default 4)\n  \
    --no-shadows            no directional light shadows\n  \
    --no-fog                no placeholder fog\n  \
    --normal-maps           use converted normal maps (unverified convention)\n  \
    --debug-info            start with the developer read-out shown (F1 toggles it; the graybox\n                          \
    always starts with it)\n\
    \n\
    unattended runs:\n  \
    --no-menu               graybox: skip the main menu (click to play)\n  \
    --screenshot PATH       save a screenshot once the level has loaded (keep it local)\n  \
    --screenshot-delay S    seconds after the assets settled before the screenshot (default 1)\n  \
    --screenshot-window     read the screenshot back from the window (with HUD and hands; the\n                          \
    window must be visible) instead of rendering it offscreen\n  \
    --hidden-window         do not show the window (unattended runs with an offscreen screenshot)\n  \
    --watch-window DIR      while playing, save the window every 2 s into DIR/window-NN.png\n                          \
    (a ring of 12 files; keep them local)\n  \
    --exit-after SECONDS    quit after this many seconds\n  \
    --walk SECONDS          start playing at once and hold forward for this simulated time";

/// A camera placement from the command line (UE3 position in UU, angles in
/// degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CameraOverride {
    position: [f32; 3],
    yaw_degrees: f32,
    pitch_degrees: f32,
}

/// Parsed command line.
#[derive(Clone, Debug, PartialEq)]
struct Cli {
    config: Config,
    converted: Option<PathBuf>,
    level: Option<String>,
    fly: bool,
    all_sublevels: bool,
    camera: Option<CameraOverride>,
    light_scale: f32,
    light_shadows: usize,
    shadows: bool,
    fog: bool,
    normal_maps: bool,
    screenshot: Option<PathBuf>,
    screenshot_delay: f32,
    screenshot_window: bool,
    watch_window: Option<PathBuf>,
    hidden_window: bool,
    debug_info: bool,
    exit_after: Option<f32>,
    walk: f32,
    no_menu: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            config: Config::Original,
            converted: None,
            level: None,
            fly: false,
            all_sublevels: false,
            camera: None,
            light_scale: 1.0,
            light_shadows: converted::RenderSettings::default().light_shadows,
            shadows: true,
            fog: true,
            normal_maps: false,
            screenshot: None,
            screenshot_delay: 1.0,
            screenshot_window: false,
            watch_window: None,
            hidden_window: false,
            debug_info: false,
            exit_after: None,
            walk: 0.0,
            no_menu: false,
        }
    }
}

fn parse_f32(flag: &str, v: &str) -> Result<f32, String> {
    v.trim()
        .parse::<f32>()
        .ok()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("{flag}: {v:?} is not a finite number"))
}

fn parse_camera(v: &str) -> Result<CameraOverride, String> {
    let parts: Vec<f32> = v
        .split(',')
        .map(|p| parse_f32("--camera", p))
        .collect::<Result<_, _>>()?;
    match parts.as_slice() {
        [x, y, z] => Ok(CameraOverride {
            position: [*x, *y, *z],
            yaw_degrees: 0.0,
            pitch_degrees: 0.0,
        }),
        [x, y, z, yaw, pitch] => Ok(CameraOverride {
            position: [*x, *y, *z],
            yaw_degrees: *yaw,
            pitch_degrees: *pitch,
        }),
        _ => Err(format!(
            "--camera expects X,Y,Z or X,Y,Z,YAW,PITCH, got {v:?}"
        )),
    }
}

/// Parses the command line (`args` without the program name) and the
/// `ASAMU_MOVEMENT` value. `Ok(None)` means only the usage was requested.
fn parse_cli(
    args: impl IntoIterator<Item = String>,
    movement_env: Option<&str>,
) -> Result<Option<Cli>, String> {
    let mut cli = Cli {
        config: match movement_env {
            Some("placeholder") => Config::Placeholder,
            Some("ue3" | "original") | None => Config::Original,
            Some(other) => return Err(format!("unknown ASAMU_MOVEMENT value {other:?}")),
        },
        ..Cli::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| format!("{flag} needs a value (try --help)"))
        };
        match arg.as_str() {
            "--placeholder" => cli.config = Config::Placeholder,
            "--placeholder-movement" => cli.config = Config::PlaceholderMovement,
            "--original" => cli.config = Config::Original,
            "--converted" => cli.converted = Some(PathBuf::from(value("--converted")?)),
            "--level" => cli.level = Some(value("--level")?),
            "--fly" => cli.fly = true,
            "--all-sublevels" => cli.all_sublevels = true,
            "--camera" => cli.camera = Some(parse_camera(&value("--camera")?)?),
            "--light-scale" => {
                let v = parse_f32("--light-scale", &value("--light-scale")?)?;
                if v < 0.0 {
                    return Err("--light-scale must not be negative".to_owned());
                }
                cli.light_scale = v;
            }
            "--light-shadows" => {
                let v = value("--light-shadows")?;
                cli.light_shadows = v
                    .parse()
                    .map_err(|_| format!("--light-shadows: {v:?} is not a count"))?;
            }
            "--no-shadows" => cli.shadows = false,
            "--no-fog" => cli.fog = false,
            "--normal-maps" => cli.normal_maps = true,
            "--screenshot" => cli.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--screenshot-delay" => {
                let v = parse_f32("--screenshot-delay", &value("--screenshot-delay")?)?;
                if v < 0.0 {
                    return Err("--screenshot-delay must not be negative".to_owned());
                }
                cli.screenshot_delay = v;
            }
            "--screenshot-window" => cli.screenshot_window = true,
            "--debug-info" => cli.debug_info = true,
            "--hidden-window" => cli.hidden_window = true,
            "--watch-window" => {
                cli.watch_window = Some(PathBuf::from(value("--watch-window")?));
            }
            "--exit-after" => {
                let v = parse_f32("--exit-after", &value("--exit-after")?)?;
                if v <= 0.0 {
                    return Err("--exit-after must be positive".to_owned());
                }
                cli.exit_after = Some(v);
            }
            "--walk" => {
                let v = parse_f32("--walk", &value("--walk")?)?;
                if v < 0.0 {
                    return Err("--walk must not be negative".to_owned());
                }
                cli.walk = v;
            }
            "--no-menu" => cli.no_menu = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }
    Ok(Some(cli))
}

impl Cli {
    /// The converted directory to use, if a converted level was requested
    /// (`--converted`, or `--level` with `ASAMU_CONVERTED_DIR` / the
    /// importer's default output directory).
    fn converted_dir(&self, env_dir: Option<PathBuf>) -> Option<ConvertedDir> {
        match (&self.converted, &self.level) {
            (Some(dir), _) => Some(ConvertedDir::new(dir.clone())),
            (None, Some(_)) => env_dir
                .map(ConvertedDir::new)
                .or_else(ConvertedDir::default_location),
            (None, None) => None,
        }
    }
}

/// The simulation plus render-interpolation state.
#[derive(Resource)]
pub(crate) struct Sim {
    game: Game,
    /// The converted level's Kismet (`None`: graybox, or a map without a
    /// Kismet export, which then keeps the level-start ability table).
    script: Option<LevelScript>,
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
    fn new(game: Game, banner: String) -> Self {
        let position = game.player().position;
        let eye = game.eye_position();
        Self {
            game,
            script: None,
            banner,
            prev_position: position,
            curr_position: position,
            prev_eye: eye,
            curr_eye: eye,
        }
    }

    /// The level script that drives this game (builder style).
    #[must_use]
    fn with_script(mut self, script: Option<LevelScript>) -> Self {
        self.script = script;
        self
    }

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
pub(crate) struct PlayerCamera;

/// A rendered mover (index into `Level::movers`).
#[derive(Component)]
struct MoverVisual(usize);

fn main() -> AppExit {
    let movement_env = std::env::var("ASAMU_MOVEMENT").ok();
    let mut cli = match parse_cli(std::env::args().skip(1), movement_env.as_deref()) {
        Ok(Some(cli)) => cli,
        Ok(None) => {
            println!("{USAGE}");
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return AppExit::error();
        }
    };
    // Screenshots of converted levels are game content: check the path
    // before a window opens (see `capture::check_screenshot_path`).
    if let Some(dir) = &cli.watch_window {
        let install = std::env::var_os("ASAMU_ORIGINAL_DIR").map(PathBuf::from);
        let probe = capture::WindowWatch::file(dir, 0);
        match capture::check_screenshot_path(&probe, install.as_deref()) {
            Ok(resolved) => cli.watch_window = resolved.parent().map(PathBuf::from),
            Err(message) => {
                eprintln!("{message}");
                return AppExit::error();
            }
        }
    }
    if let Some(path) = &cli.screenshot {
        let install = std::env::var_os("ASAMU_ORIGINAL_DIR").map(PathBuf::from);
        match capture::check_screenshot_path(path, install.as_deref()) {
            Ok(resolved) => cli.screenshot = Some(resolved),
            Err(message) => {
                eprintln!("{message}");
                return AppExit::error();
            }
        }
    }
    let env_dir = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from);
    match cli.converted_dir(env_dir) {
        Some(dir) => run_converted(&cli, dir),
        None => run_graybox(&cli),
    }
}

/// The window and default plugins (after any asset sources are registered).
fn add_default_plugins(app: &mut App, title: &str, cli: &Cli) {
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: title.into(),
            // `--hidden-window`: an unattended run that renders its
            // screenshot offscreen needs no window on screen.
            visible: !cli.hidden_window,
            ..default()
        }),
        ..default()
    }))
    .add_plugins((hud::HudPlugin, capture::CapturePlugin))
    .add_plugins((
        audio::AudioPlugin,
        ui::UiPlugin,
        lightmaps::LightmapPlugin,
        npc::NpcPlugin,
        post::PostPlugin,
        kismet::KismetPlugin,
    ))
    .add_plugins((
        particles::ParticlesPlugin,
        vfx::VfxPlugin,
        timetrial::TimeTrialPlugin,
        gamepad::GamepadPlugin,
        water::WaterPlugin,
    ));
}

fn run_graybox(cli: &Cli) -> AppExit {
    let game = match cli.config {
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
    let banner = banner(&game);
    println!("{banner}");

    let mut app = App::new();
    add_default_plugins(&mut app, "ASAMU-decomp (pre-alpha graybox)", cli);
    app.insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)))
        .insert_resource(GlobalAmbientLight {
            brightness: 400.0,
            ..default()
        })
        .insert_resource(
            capture::AutoCapture::new(cli.screenshot.clone(), cli.exit_after)
                .with_delay(cli.screenshot_delay)
                .with_window(cli.screenshot_window),
        )
        .add_systems(Startup, (spawn_level, spawn_camera_and_light))
        // The main menu first (graybox: New Game plays the test level),
        // except for unattended runs.
        .insert_resource(ui::UiLaunch {
            show_menu: !cli.no_menu && cli.walk == 0.0 && cli.screenshot.is_none(),
            ..ui::UiLaunch::default()
        });
    add_simulation(&mut app, game, banner);
    app.insert_resource(ScriptedWalk(cli.walk));
    if let Some(dir) = &cli.watch_window {
        app.insert_resource(capture::WindowWatch::new(dir.clone()));
    }
    app.run()
}

/// The fixed-step simulation, its input and the simulation-driven camera
/// and HUD.
fn add_simulation(app: &mut App, game: Game, banner: String) {
    // Fixed tick rate comes from the simulation clock (a runtime choice;
    // the original's tick model is UNKNOWN).
    app.insert_resource(Time::<Fixed>::from_hz(game.clock().tick_rate_hz()))
        .insert_resource(Sim::new(game, banner));
    add_simulation_systems(app);
}

/// The simulation's systems; they run while a [`Sim`] resource exists (from
/// the start in the graybox, once the level's game has loaded for a
/// converted level).
fn add_simulation_systems(app: &mut App) {
    app.init_resource::<PendingInput>()
        .init_resource::<LevelGizmos>()
        .init_resource::<ScriptedWalk>()
        .add_systems(
            RunFixedMainLoop,
            (cursor_and_pause, gather_input)
                .chain()
                .run_if(resource_exists::<Sim>.and_then(ui::gameplay_input_enabled))
                .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
        )
        .add_systems(FixedUpdate, fixed_tick.run_if(resource_exists::<Sim>))
        .add_systems(
            Update,
            (sync_camera, sync_movers, draw_gizmos, update_hud).run_if(resource_exists::<Sim>),
        );
}

/// `--walk SECONDS`: simulated seconds of scripted forward input left
/// (unattended checks of the simulation; 0 = off).
#[derive(Resource, Clone, Copy, Debug, Default)]
struct ScriptedWalk(f32);

/// Whether the level-object gizmos (checkpoint volumes, crystal charge,
/// attractor pads) are drawn: on in the graybox, off (F10 toggles) on
/// converted levels, whose objects are rendered as meshes.
#[derive(Resource, Clone, Copy, Debug)]
struct LevelGizmos(bool);

impl Default for LevelGizmos {
    fn default() -> Self {
        Self(true)
    }
}

/// The converted level's [`Game`] and level script, loaded on the async
/// pool.
#[derive(Resource)]
struct GameLoad {
    /// Converted directory and level to load (`None` with `--fly`).
    request: Option<(PathBuf, String)>,
    task: Option<bevy::tasks::Task<Result<ui::LoadedLevel, String>>>,
}

fn start_game_load(mut load: ResMut<GameLoad>) {
    if let Some((root, name)) = load.request.take() {
        load.task =
            Some(bevy::tasks::AsyncComputeTaskPool::get().spawn(async move {
                ui::load_level(&root, &name, asamu_game::save::PlayMode::Story)
            }));
    }
}

/// Inserts the [`Sim`] once the converted level's game has loaded.
fn poll_game_load(
    mut commands: Commands,
    mut load: ResMut<GameLoad>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut saves: ResMut<ui::Saves>,
    mut play: ResMut<ui::Play>,
    camera_override: Res<CameraOverrideRes>,
) {
    let Some(task) = load.task.as_mut() else {
        return;
    };
    let Some(result) = bevy::tasks::futures::check_ready(task) else {
        return;
    };
    load.task = None;
    match result {
        Ok((game, mut script)) => {
            let banner = banner(&game);
            info!("gameplay collision loaded: the simulation drives the camera");
            // The loader tolerates missing pieces (e.g. meshes not converted:
            // no static-mesh collision); say so instead of failing silently.
            if let Some(map) = game.scene_map()
                && !map.warnings.is_empty()
            {
                warn!(
                    "the level's gameplay data loaded with {} warnings, e.g. {:?}",
                    map.warnings.len(),
                    map.warnings.iter().take(3).collect::<Vec<_>>()
                );
            }
            println!("{banner}");
            let mut game = game;
            // The level start of the (in-memory) save session, as a menu
            // load does: chapter, snapshot, "save loaded" for Kismet.
            ui::start_direct_level(&mut game, script.as_mut(), &mut saves, &mut play);
            if let Some(c) = camera_override.0 {
                // Debug teleport (`--camera` with gameplay): the eye at the
                // given point, falling from there.
                let eye = game.params().camera.eye_height.value;
                let player = game.player_mut();
                player.position = sim_glam::Vec3::from_array(c.position) - sim_glam::Vec3::Z * eye;
                player.yaw = c.yaw_degrees.to_radians();
                player.pitch = c.pitch_degrees.to_radians();
                player.velocity = sim_glam::Vec3::ZERO;
                player.grounded = false;
                player.pawn.force_floor_check = true;
                info!("debug teleport to {:?}", c.position);
            }
            if script.is_none() {
                warn!(
                    "no converted Kismet for this level (run `asamu-import kismet` and `matinee`)"
                );
            }
            // Already captured the mouse in fly mode: start playing at once.
            if cursor.grab_mode != CursorGrabMode::None {
                game.start();
            }
            commands.insert_resource(Time::<Fixed>::from_hz(game.clock().tick_rate_hz()));
            commands.insert_resource(Sim::new(game, banner).with_script(script));
        }
        Err(e) => {
            warn!("could not load the converted level for gameplay ({e}); staying in fly mode");
        }
    }
}

fn run_converted(cli: &Cli, dir: ConvertedDir) -> AppExit {
    let levels = dir.list_levels();
    // Without --level the run starts in the main menu, with a converted
    // level (the original's front end when available) rendered behind it.
    let menu = cli.level.is_none();
    let Some(level) = cli.level.clone().or_else(|| ui::menu_backdrop(&levels)) else {
        eprintln!(
            "no converted levels in {} (run `asamu-import --out <dir> levels --map <name>` \
             plus `textures` and `meshes`)",
            dir.levels_dir().display()
        );
        return AppExit::error();
    };
    // Bevy resolves relative asset roots against the executable's directory:
    // use an absolute path.
    let dir = match dir.root().canonicalize() {
        Ok(abs) => ConvertedDir::new(abs),
        Err(e) => {
            eprintln!("converted directory {}: {e}", dir.root().display());
            return AppExit::error();
        }
    };
    // Fail early (before opening a window) when the level is missing.
    if let Err(e) = dir.scene_path(&level) {
        eprintln!("{e}");
        return AppExit::error();
    }
    let params = asamu_player::PlayerParams::asamu_original();
    let eye_height = params.camera.eye_height.value;
    let lights = LightMapping::default();
    // Every sub-level is planned; the ones Kismet streams show while the
    // game has them streamed in (`kismet::sync_actor_render`).
    let options = PlanOptions {
        scale: SCALE,
        all_sublevels: true,
        lights: LightMapping {
            point_lumens_per_brightness_m2: lights.point_lumens_per_brightness_m2 * cli.light_scale,
            directional_lux_per_brightness: lights.directional_lux_per_brightness * cli.light_scale,
            ..lights
        },
    };
    let settings = converted::RenderSettings {
        shadows: cli.shadows,
        light_shadows: cli.light_shadows,
        fog: cli.fog,
        normal_maps: cli.normal_maps,
    };
    // Gameplay: the level's game (triangle collision, gameplay actors) loads
    // on the async pool; until it is ready (or with --fly) the camera flies.
    let game_load = GameLoad {
        request: (!cli.fly && !menu).then(|| (dir.root().to_path_buf(), level.clone())),
        task: None,
    };
    let launch = ui::UiLaunch {
        converted: Some(dir.clone()),
        levels,
        show_menu: menu,
        saves: menu,
    };
    println!(
        "ASAMU-decomp: converted level {level} from {} (local data derived from your own install)",
        dir.root().display()
    );

    let mut app = App::new();
    converted::register_source(&mut app, dir.root().to_path_buf());
    add_default_plugins(
        &mut app,
        &format!("ASAMU-decomp - {level} (converted, pre-alpha)"),
        cli,
    );
    app.insert_resource(ClearColor(Color::srgb(0.04, 0.045, 0.06)))
        .insert_resource(GlobalAmbientLight {
            brightness: options.lights.base_ambient,
            ..default()
        })
        .insert_resource(hud::HudStyle {
            info_color: Color::srgb(0.92, 0.92, 0.88),
        })
        // The developer read-out would cover the game: F1 shows it.
        .insert_resource(hud::DebugInfo(cli.debug_info))
        .insert_resource(
            capture::AutoCapture::new(cli.screenshot.clone(), cli.exit_after)
                .with_delay(cli.screenshot_delay)
                .with_window(cli.screenshot_window),
        )
        .insert_resource(converted::ConvertedLevel {
            dir,
            level,
            options,
            settings,
            eye_height,
            force_all_sublevels: cli.all_sublevels,
            phase: None,
            progress: (0, 0, 0),
        })
        .insert_resource(CameraOverrideRes(cli.camera))
        .insert_resource(LevelGizmos(false))
        .insert_resource(game_load)
        .insert_resource(launch)
        .add_plugins((converted::ConvertedLevelPlugin, fly::FlyCameraPlugin))
        .add_systems(Startup, (spawn_converted_camera, start_game_load))
        .add_systems(
            Update,
            (
                poll_game_load,
                (apply_level_start, update_converted_hud).run_if(not(resource_exists::<Sim>)),
            ),
        );
    add_simulation_systems(&mut app);
    app.insert_resource(ScriptedWalk(cli.walk));
    if let Some(dir) = &cli.watch_window {
        app.insert_resource(capture::WindowWatch::new(dir.clone()));
    }
    app.run()
}

/// The `--camera` override, applied when the level start is known.
#[derive(Resource, Clone, Copy, Debug)]
struct CameraOverrideRes(Option<CameraOverride>);

fn spawn_converted_camera(mut commands: Commands) {
    commands.spawn((
        PlayerCamera,
        Camera3d::default(),
        Projection::from(PerspectiveProjection {
            // 90° horizontal at 16:9 (the original's default FOV is 90,
            // horizontal; refined per frame in `update_converted_hud`).
            fov: 2.0 * ((45.0_f32).to_radians().tan() * 9.0 / 16.0).atan(),
            near: 0.05,
            far: 1000.0,
            ..default()
        }),
        Transform::default(),
    ));
}

/// Moves the fly camera to the level's player start (or `--camera`) once.
fn apply_level_start(
    start: Res<converted::LevelStart>,
    override_: Res<CameraOverrideRes>,
    mut fly: ResMut<fly::FlyCam>,
    mut applied: Local<bool>,
) {
    if *applied {
        return;
    }
    if let Some(c) = override_.0 {
        fly.position = sim_glam::Vec3::from_array(c.position);
        fly.yaw = c.yaw_degrees.to_radians();
        fly.pitch = c.pitch_degrees.to_radians();
        *applied = true;
        return;
    }
    if !start.is_changed() {
        return;
    }
    if let Some((eye, yaw, pitch)) = start.0 {
        fly.position = eye;
        fly.yaw = yaw;
        fly.pitch = pitch;
        *applied = true;
    }
}

#[allow(clippy::type_complexity)]
fn update_converted_hud(
    level: Res<converted::ConvertedLevel>,
    fly: Res<fly::FlyCam>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut projection: Single<&mut Projection, With<PlayerCamera>>,
    mut info: Single<&mut Text, (With<hud::HudInfo>, Without<hud::HudAbilities>)>,
    mut abilities: Single<&mut Text, (With<hud::HudAbilities>, Without<hud::HudInfo>)>,
) {
    // Horizontal FOV 90 (UE3 convention, the original's default) mapped to
    // Bevy's vertical FOV for the window's aspect.
    if let Projection::Perspective(p) = projection.as_mut() {
        let aspect = (window.width() / window.height().max(1.0)).max(0.1);
        p.fov = 2.0 * ((90.0_f32.to_radians() * 0.5).tan() / aspect).atan();
    }
    let p = fly.position;
    info.0 = format!(
        "ASAMU-decomp pre-alpha | converted original level (render approximation: dynamic \
         lights, no lightmaps) | FLY CAMERA (render only, no collision)\n\
         {}\n\
         camera UE ({:.0}, {:.0}, {:.0}) uu | yaw {:.0} deg pitch {:.0} deg | speed {:.0} uu/s | {} fps\n\
         click to capture the mouse | WASD move | Space/E up | Ctrl/Q down | Shift fast | wheel speed | Esc release",
        level.status(),
        p.x,
        p.y,
        p.z,
        fly.yaw.to_degrees(),
        fly.pitch.to_degrees(),
        fly.speed,
        hud::fps(&diagnostics).map_or_else(|| "-".to_owned(), |f| format!("{f:.0}")),
    );
    abilities.0 = "GRAPPLES -/- | grapple gun: n/a | power jump: n/a | rocket boots: n/a\n\
                   (fly mode: no simulation running)"
        .to_owned();
}

pub(crate) fn bevy_vec(v: sim_glam::Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

pub(crate) fn bevy_quat(q: sim_glam::Quat) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// UE position (UU) → Bevy render position.
pub(crate) fn to_render(v: sim_glam::Vec3) -> Vec3 {
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

/// Cursor capture, pause/resume, respawn and trace recording hotkeys.
fn cursor_and_pause(
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut sim: ResMut<Sim>,
    mut pending: ResMut<PendingInput>,
    mut level_gizmos: ResMut<LevelGizmos>,
    presentation: Option<Res<kismet::Presentation>>,
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
    // `SeqAct_DisablePauseMenu` (TheCore, during the credits): Esc does
    // nothing.
    let pause_allowed = presentation.as_ref().is_none_or(|p| p.pause_menu);
    if keys.just_pressed(KeyCode::Escape) && pause_allowed {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
        sim.game.pause();
        *pending = PendingInput::default();
        return;
    }
    // R (debug) and F7 (the original's QuickLoad: restart from the
    // checkpoint) respawn.
    if keys.just_pressed(KeyCode::KeyR) || keys.just_pressed(KeyCode::F7) {
        if sim.game.scene_map().is_some() {
            // The original's quick load: the death sequence, then the latest
            // checkpoint (Kismet sees the `PlayerDied` event).
            sim.game.kill_player();
        } else {
            sim.game.respawn();
            sim.snap_interpolation();
        }
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
    if keys.just_pressed(KeyCode::F10) {
        level_gizmos.0 = !level_gizmos.0;
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
    settings: Res<ui::UserSettings>,
    mut pending: ResMut<PendingInput>,
) {
    if cursor.grab_mode == CursorGrabMode::None || sim.game.state() != GameState::Playing {
        return;
    }
    // Screen +x (right) turns right (+yaw in UE3 convention); screen +y (down)
    // looks down (−pitch); the user's sensitivity and invert apply.
    let (yaw_scale, pitch_scale) = settings.look_scale();
    pending.look_yaw += motion.delta.x * yaw_scale;
    pending.look_pitch -= motion.delta.y * pitch_scale;
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
#[allow(clippy::too_many_arguments)]
fn fixed_tick(
    mut sim: ResMut<Sim>,
    mut pending: ResMut<PendingInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut walk: ResMut<ScriptedWalk>,
    mut reports: MessageWriter<ui::GameTick>,
    mut frames: MessageWriter<kismet::KismetFrame>,
    presentation: Option<Res<kismet::Presentation>>,
) {
    // `--walk`: start at once and hold forward for the given simulated time.
    let scripted = walk.0 > 0.0;
    if scripted && sim.game.state() == GameState::Boot {
        sim.game.start();
    }
    if sim.game.state() != GameState::Playing {
        return;
    }
    if scripted {
        walk.0 -= 1.0 / sim.game.clock().tick_rate_hz() as f32;
    }
    let mut input = InputFrame {
        move_forward: if scripted {
            1.0
        } else {
            axis(
                &keys,
                [KeyCode::KeyW, KeyCode::ArrowUp],
                [KeyCode::KeyS, KeyCode::ArrowDown],
            )
        },
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
    // Cinematic mode (`SeqAct_ToggleCinematicMode`): the flags it sets block
    // movement, turning and buttons while active.
    if let Some(p) = &presentation {
        let (movement, turning, buttons) = p.input_allowed();
        if !movement {
            input.move_forward = 0.0;
            input.move_right = 0.0;
        }
        if !turning {
            input.look_yaw_delta = 0.0;
            input.look_pitch_delta = 0.0;
        }
        if !buttons {
            input.jump_pressed = false;
            input.jump_held = false;
            input.grapple_held = false;
            input.sprint_held = false;
            input.power_jump_held = false;
            input.use_pressed = false;
        }
    }
    let before = sim.game.player().position;
    let eye_before = sim.game.eye_position();
    let Sim { game, script, .. } = &mut *sim;
    let report = match script.as_mut() {
        Some(script) => script.tick(game, &input).map(|t| {
            frames.write(kismet::KismetFrame {
                outputs: t.outputs,
                npc_events: t.npc_events,
            });
            t.report
        }),
        None => game.tick(&input),
    };
    if let Some(report) = report {
        // Checkpoint saves and the time-trial rules (`ui::flow`).
        reports.write(ui::GameTick(report));
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
            // `ASAMUPlayerController.Use`: the stock use search only finds
            // actors with a `SeqEvent_Used` (none in the shipped maps);
            // story interactables use the fire button (GRAPPLE.md G-AC-0).
            if let Some(npcs) = sim.game.npcs() {
                debug!("use: {:?}", npcs.use_action(sim.game.player()));
            }
        }
        for event in report.world.iter() {
            debug!("world: {event:?}");
        }
        if let Some(fire) = report.events.gun.fire {
            info!("grapple fire: {fire:?}");
        }
    }
}

/// Places the camera at the interpolated eye position with the simulation's
/// view rotation (plus look input not yet consumed by a tick).
#[allow(clippy::too_many_arguments)]
fn sync_camera(
    sim: Res<Sim>,
    pending: Res<PendingInput>,
    fixed: Res<Time<Fixed>>,
    settings: Res<ui::UserSettings>,
    time: Res<Time>,
    presentation: Option<Res<kismet::Presentation>>,
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
    if let Some(p) = &presentation {
        // Kismet's view target (a Matinee director cut, `SetCameraTarget`):
        // the camera sits at that actor (its FOV is not exported; the
        // player's is kept).
        if let Some((location, rotation)) = kismet::view_override(&sim, p) {
            transform.translation = to_render(location);
            let yaw = asamu_core::rotator::rotator_units_to_radians(rotation[1]);
            let pitch = asamu_core::rotator::rotator_units_to_radians(
                asamu_core::rotator::normalize_rotator_axis(rotation[0]),
            );
            transform.rotation = bevy_quat(ue_view_to_bevy_rotation(yaw, pitch));
        }
        transform.translation += kismet::shake_offset(p, time.elapsed_secs());
    }

    // The FOV is horizontal (UE3 convention); Bevy's perspective FOV is
    // vertical. The story-mode zoom changes the run-time FOV; the user's FOV
    // setting shifts it.
    if let Projection::Perspective(perspective) = projection.as_mut() {
        let aspect = (window.width() / window.height().max(1.0)).max(0.1);
        let horizontal = settings
            .view_fov_degrees(sim.game.fov(), params.camera.fov_degrees.value)
            .to_radians();
        perspective.fov = 2.0 * ((horizontal * 0.5).tan() / aspect).atan();
    }
}

fn draw_gizmos(
    sim: Res<Sim>,
    fixed: Res<Time<Fixed>>,
    level_gizmos: Res<LevelGizmos>,
    mut gizmos: Gizmos,
) {
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

    if !level_gizmos.0 {
        return;
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

#[allow(clippy::type_complexity)]
fn update_hud(
    sim: Res<Sim>,
    level: Option<Res<converted::ConvertedLevel>>,
    mut hud: Single<&mut Text, (With<hud::HudInfo>, Without<hud::HudAbilities>)>,
    mut abilities: Single<&mut Text, (With<hud::HudAbilities>, Without<hud::HudInfo>)>,
    crosshair: Single<&mut TextColor, With<hud::Crosshair>>,
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
    let banner = match &level {
        Some(l) => format!("{}\n{}", sim.banner, l.status()),
        None => sim.banner.clone(),
    };
    hud.0 = format!(
        "{banner}\n\
         speed {speed:.0} uu/s ({:.1} m/s presentation) | horizontal {:.0} uu/s | {}\n\
         {pawn_line}\n\
         grapple {grapple} | aim: {aim_text} | rocket boots {boots_text}\n\
         tick {} @ {:.0} Hz | checkpoint {checkpoint} | respawns {} | {state}{}\n\
         WASD move | mouse look | Space jump (air: rocket boost) | LShift sprint | LMB grapple | hold RMB power jump / zoom | \
         E use | F7/R respawn | debug: F2 story mode, F3 grapple capacity, F4 boots, F6 attractor, F9 record trace, \
         F10 level gizmos, F1 this read-out | Esc release mouse",
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
    abilities.0 = ability_panel(game);
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

/// The ability panel: grapple count and the state of each ability.
fn ability_panel(game: &Game) -> String {
    let player = game.player();
    let script = &player.script;
    let gun = &script.gun;
    let grapples = if game.params().gun.is_none() {
        "GRAPPLES - (placeholder rope)".to_owned()
    } else if gun.max_grapples >= 32_767 || gun.max_grapples < 0 {
        "GRAPPLES unlimited".to_owned()
    } else {
        let left = (gun.max_grapples - gun.times_grappled).max(0);
        format!("GRAPPLES {left}/{}", gun.max_grapples)
    };
    let gun_state = if game.params().gun.is_none() {
        "n/a".to_owned()
    } else if gun.anchor().is_some() {
        "attached".to_owned()
    } else if gun.can_grapple {
        "ready".to_owned()
    } else {
        "closed".to_owned()
    };
    let power_jump = if !script.started {
        "n/a".to_owned()
    } else if script.power_jump.charged {
        "charged".to_owned()
    } else {
        format!("{:?}", script.power_jump.state)
    };
    let boots = &script.boots;
    let boots_state = if !boots.spawned {
        "none"
    } else if !boots.enabled {
        "disabled"
    } else {
        match boots.state {
            BootsStateName::Ready => "ready",
            BootsStateName::Boosting => "boosting",
            BootsStateName::Unavailable | BootsStateName::UnavailableAndPlayedSound => "used",
        }
    };
    format!(
        "{grapples} | grapple gun: {gun_state} | power jump: {power_jump} | rocket boots: {boots_state}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn default_is_the_graybox_with_original_parameters() {
        let cli = parse_cli(args(&[]), None).unwrap().unwrap();
        assert_eq!(cli, Cli::default());
        assert!(cli.converted_dir(None).is_none());
        assert_eq!(
            parse_cli(args(&["--placeholder"]), None)
                .unwrap()
                .unwrap()
                .config,
            Config::Placeholder
        );
        assert_eq!(
            parse_cli(args(&[]), Some("placeholder"))
                .unwrap()
                .unwrap()
                .config,
            Config::Placeholder
        );
        assert!(parse_cli(args(&[]), Some("bogus")).is_err());
        assert!(parse_cli(args(&["--help"]), None).unwrap().is_none());
        assert!(!cli.no_menu);
        assert!(
            parse_cli(args(&["--no-menu"]), None)
                .unwrap()
                .unwrap()
                .no_menu
        );
    }

    #[test]
    fn converted_level_options() {
        let cli = parse_cli(
            args(&[
                "--converted",
                "/tmp/conv",
                "--level",
                "AG-Workshop",
                "--fly",
                "--all-sublevels",
                "--camera",
                "1,2,3,90,-10",
                "--light-scale",
                "2",
                "--light-shadows",
                "0",
                "--no-shadows",
                "--no-fog",
                "--normal-maps",
                "--screenshot",
                "/tmp/shot.png",
                "--screenshot-window",
                "--exit-after",
                "5",
                "--walk",
                "1.5",
            ]),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cli.level.as_deref(), Some("AG-Workshop"));
        assert!(cli.fly && cli.all_sublevels && !cli.shadows && !cli.fog && cli.normal_maps);
        assert_eq!(cli.light_scale, 2.0);
        assert_eq!(cli.light_shadows, 0);
        assert_eq!(
            cli.camera,
            Some(CameraOverride {
                position: [1.0, 2.0, 3.0],
                yaw_degrees: 90.0,
                pitch_degrees: -10.0
            })
        );
        assert_eq!(cli.exit_after, Some(5.0));
        assert!(cli.screenshot_window);
        assert_eq!(cli.walk, 1.5);
        assert_eq!(
            cli.converted_dir(Some(PathBuf::from("/elsewhere")))
                .unwrap()
                .root(),
            std::path::Path::new("/tmp/conv")
        );
        // --level alone uses ASAMU_CONVERTED_DIR (or the importer default).
        let cli = parse_cli(args(&["--level", "X"]), None).unwrap().unwrap();
        assert_eq!(
            cli.converted_dir(Some(PathBuf::from("/env")))
                .unwrap()
                .root(),
            std::path::Path::new("/env")
        );
    }

    #[test]
    fn bad_arguments_are_errors() {
        for bad in [
            &["--level"][..],
            &["--camera", "1,2"],
            &["--camera", "1,2,x"],
            &["--light-scale", "-1"],
            &["--light-scale", "nan"],
            &["--light-shadows", "-3"],
            &["--exit-after", "0"],
            &["--walk", "-1"],
            &["--bogus"],
        ] {
            assert!(parse_cli(args(bad), None).is_err(), "{bad:?}");
        }
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// user-local `asamu-import` output with `levels/` and `meshes/`): the
    /// simulation and the renderer agree on where the player starts and
    /// where the floor is.
    ///
    /// - both pick the same `PlayerStart` and yaw;
    /// - the simulation spawns there (its spot search only moves upwards),
    ///   and its render-space eye is the fly camera's start up to that lift;
    /// - after settling (no input, 2 s), when a level BSP triangle lies under
    ///   the grounded pawn, its feet hover over that rendered triangle by the
    ///   native walking physics' 1.9-2.4 uu (the floor the player sees is the
    ///   floor the simulation collides with).
    #[test]
    fn converted_levels_spawn_where_the_render_plan_starts() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let dir = ConvertedDir::new(root.clone());
        if !root.join("meshes").is_dir() {
            eprintln!("skipped: no converted meshes in {}", root.display());
            return;
        }
        let eye_height = asamu_player::PlayerParams::asamu_original()
            .camera
            .eye_height
            .value;
        let mut bsp_floors = 0usize;
        for level in dir.list_levels() {
            let plan =
                asamu_assets::LevelPlan::load(&dir, &level, &PlanOptions::default()).expect("plan");
            let Some((start, yaw, _)) = plan.start() else {
                continue;
            };
            let mut game = match Game::load_level(&root, &level) {
                Ok(game) => game,
                Err(e) => {
                    eprintln!("{level}: no game ({e})");
                    continue;
                }
            };
            let sim_start = game
                .scene_map()
                .and_then(|m| m.actors.player_start())
                .expect("player start");
            assert_eq!(sim_start.location.to_array(), start.to_array(), "{level}");
            let p = game.player().position;
            // Same direction (the two conversions may wrap differently).
            let turn = (game.player().yaw - yaw).rem_euclid(std::f32::consts::TAU);
            assert!(
                turn.min(std::f32::consts::TAU - turn) < 1e-4,
                "{level}: yaw {} vs {yaw}",
                game.player().yaw
            );
            assert!(
                (p.x - start.x).abs() < 1e-3 && (p.y - start.y).abs() < 1e-3,
                "{level}: spawn {p} vs start {start}"
            );
            let lift = p.z - start.z;
            assert!((-1e-3..=260.0).contains(&lift), "{level}: lift {lift}");
            let fly_eye = sim_glam::Vec3::new(start.x, start.y, start.z + eye_height);
            let gap = to_render(game.eye_position()) - to_render(fly_eye);
            assert!(
                (gap.x.abs() + gap.z.abs()) < 1e-4 && (gap.y - lift / 50.0).abs() < 0.2,
                "{level}: eye gap {gap}"
            );

            game.start();
            let ticks = (2.0 * game.clock().tick_rate_hz()) as usize;
            for _ in 0..ticks {
                game.tick(&InputFrame::default());
            }
            let half = game.params().movement.capsule_half_height.value;
            let feet = game.player().position - sim_glam::Vec3::Z * half;
            let f = to_render(feet);
            // The highest BSP triangle under the feet (render space, y up).
            let mut floor: Option<f32> = None;
            for b in &plan.bsp {
                for t in b.mesh.indices.as_chunks::<3>().0 {
                    let v = t.map(|i| Vec3::from_array(b.mesh.positions[i as usize]));
                    let side =
                        |a: Vec3, c: Vec3| (c.x - a.x) * (f.z - a.z) - (c.z - a.z) * (f.x - a.x);
                    let (d1, d2, d3) = (side(v[0], v[1]), side(v[1], v[2]), side(v[2], v[0]));
                    let inside = (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0)
                        || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0);
                    let n = (v[1] - v[0]).cross(v[2] - v[0]);
                    if !inside || n.y.abs() < 1e-6 {
                        continue;
                    }
                    let y = v[0].y - (n.x * (f.x - v[0].x) + n.z * (f.z - v[0].z)) / n.y;
                    if y <= f.y + 0.1 && floor.is_none_or(|h| y > h) {
                        floor = Some(y);
                    }
                }
            }
            eprintln!(
                "{level}: start {start}, spawn lift {lift:.2} uu, after 2 s grounded {} at {}, \
                 feet above the BSP floor {:?} uu",
                game.player().grounded,
                game.player().position,
                floor.map(|h| (f.y - h) * 50.0),
            );
            // A walking pawn hovers 1.9-2.4 uu above its floor
            // (NATIVE_PHYSICS.md, CONFIRMED); 0.01 uu of slack for the
            // render-space rounding. A BSP triangle far below means the pawn
            // stands on a static mesh (not checked here).
            if let Some(h) = floor
                && game.player().grounded
                && (f.y - h) * 50.0 < 10.0
            {
                let hover = (f.y - h) * 50.0;
                assert!(
                    (1.89..=2.41).contains(&hover),
                    "{level}: feet {hover} uu above the BSP floor"
                );
                bsp_floors += 1;
            }
        }
        eprintln!("{bsp_floors} levels start on a BSP floor (hover checked)");
    }

    #[test]
    fn graybox_ability_panel_reports_grapples() {
        let game = Game::graybox().unwrap();
        let panel = ability_panel(&game);
        assert!(panel.starts_with("GRAPPLES "), "{panel}");
        assert!(panel.contains("rocket boots"), "{panel}");
    }
}
