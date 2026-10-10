//! The Sandbox (experimental): an in-app lab for the movement and game
//! systems. Live parameter tuning, cheats, time control, save states,
//! read-outs and hand-made arenas, on top of the unchanged simulation.
//!
//! **Not the original's behaviour and never evidence of parity.** Classic
//! stays the faithful target; this module is compiled only with the
//! `sandbox` Cargo feature (on by default) and can be removed by deleting
//! `src/sandbox.rs`, `src/sandbox/` and the hooks marked
//! `cfg(feature = "sandbox")` in `main.rs`, `ui.rs` and `ui/menus.rs`.
//!
//! The model (overlays, sessions, commands, snapshots, arenas) is the
//! render-free crate `asamu-sandbox`; this module is its Bevy host.
//!
//! # How Classic is kept apart
//!
//! - A session exists only while the save session is in memory
//!   (`ui::Saves.0.store().is_none()`): `--sandbox` never opens the saves on
//!   disk, and the main-menu button is disabled on a disk-backed launch.
//! - Nothing here hooks into `fixed_tick`, `gather_input` or
//!   `cursor_and_pause`, and nothing here calls `Game::tick`: the app ticks
//!   the game exactly as in Classic, and the session acts between ticks.
//! - **Single writer:** only `sandbox/control.rs` takes the simulation
//!   (`Sim`) or the virtual and fixed clocks mutably. Everything else reads,
//!   and asks for changes with a [`LabRequest`]; `lifecycle` decides what a
//!   lifecycle request means and hands the writing to `control` as well. (A
//!   boundary test in `asamu-sandbox` scans this module for the mutable
//!   parameter types, which is why they are not spelled out here.)
//! - Outside a session only three systems run: `lifecycle::startup` (once),
//!   and `lifecycle::guard` and `lifecycle::requests` (which read, and queue
//!   nothing unless the Sandbox is asked for). Everything else runs only
//!   while a session exists (most of it while it is [`Phase::Active`], the
//!   launcher's while [`Phase::Launcher`]), so in a Classic process the
//!   plugin is registered but idle: it never writes the simulation or a
//!   clock there. What still executes in such a process besides those three
//!   is read-only: the run conditions of the other systems, and the click
//!   observer of `widgets`, which Bevy also calls for the Classic menu's
//!   buttons and which returns at once for any button that is not the
//!   Sandbox's.
//!
//! # Schedule placement (the contract between the submodules)
//!
//! | Schedule | Set / order | Systems |
//! |---|---|---|
//! | `PostStartup` | none | `lifecycle::startup` |
//! | `RunFixedMainLoop`, before the fixed loop | [`LabSet::Input`], before `cursor_and_pause` | `input::hotkeys` (keys to [`LabRequest`]) |
//! | same | [`LabSet::Apply`], after `Input`, before `cursor_and_pause` | `control::apply` (adopts a loaded level, reconcile, commands, inspector, fly placement, time control) |
//! | `FixedFirst` | none | `control::before_tick` |
//! | `FixedLast` | [`LabSet::Observe`] | `control::after_tick` |
//! | `RunFixedMainLoop`, after the fixed loop | none | `control::snap` |
//! | `Update` | before `ui::UiFlowSet`, in this order | `lifecycle::guard`, `lifecycle::requests`, `control::carry_out` |
//! | `Update` | [`LabSet::Draw`], after `sync_camera` | HUD, visualisers, graphs, panel, launcher, arena view |
//!
//! A session can be [`Phase::Active`] for a frame or two without a
//! simulation: a Kismet map change drops the old level's game before the
//! session notices and goes back to [`Phase::Loading`]. Systems that run
//! under [`lab_active`] therefore take the simulation as an `Option`.
//!
//! Who handles which [`LabRequest`]: `lifecycle` takes `OpenLauncher`,
//! `Start`, `End`, `Quit` and `SaveProfile` (it decides;
//! `control::carry_out` writes, in the same frame); `control::apply` takes
//! `Do`, `OpenInspector` and `CloseInspector`.
//!
//! Time as the view sees it: the session's model (`Session::time`) holds
//! what the player asked for; the inspector's "freeze while open" is on top
//! of it and is not in the model. Whether the fixed loop is actually held is
//! `Time<Virtual>::is_paused`.
//!
//! Submodules: `cli`, `lifecycle`, `control`, `storage` (the runtime) and
//! `input`, `panel`, `widgets`, `launcher`, `hud`, `viz`, `graphs`,
//! `arena_view` (the view). Each one except `cli` (a pure check, called by
//! `parse_cli`) registers its own systems from a
//! `pub(super) fn build(app: &mut App)`; that call is the only coupling
//! between this file and a submodule.
//!
//! The types, sets and registrations below are the contract between the
//! runtime and the view.

mod arena_view;
pub(crate) mod cli;
mod control;
mod graphs;
mod hud;
mod input;
mod launcher;
mod lifecycle;
mod panel;
mod storage;
mod viz;
mod widgets;

use asamu_game::Game;
use asamu_sandbox::command::Command;
use asamu_sandbox::inspect::Inspection;
use asamu_sandbox::profile::SandboxDirs;
use asamu_sandbox::session::Session;
use bevy::prelude::*;

use crate::{Cli, SandboxArgs, Sim};

/// Where the Sandbox is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No Sandbox: Classic.
    Idle,
    /// The stage launcher is open (`ui::Screen::Sandbox`), no session yet.
    Launcher,
    /// A session exists and its converted level is loading.
    Loading,
    /// A session is running.
    Active,
}

/// What a session plays on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stage {
    /// The stock hand-made graybox test level.
    Graybox,
    /// A built-in hand-made arena (`asamu_sandbox::arena`), by id.
    Arena(String),
    /// A converted map of this launch (`ui::UiLaunch::levels`), by name.
    Map(String),
}

/// What `lifecycle` keeps to put Classic back when a session on a hand-made
/// stage ends: the graybox game the session replaced, and its banner.
struct ClassicKeep {
    game: Game,
    banner: String,
}

/// The Sandbox's state.
///
/// Written by `lifecycle` and `control` only; the view reads it.
#[derive(Resource)]
pub(crate) struct Lab {
    /// Lifecycle phase.
    pub phase: Phase,
    /// This is a `--sandbox` process: the Classic menus are unreachable and
    /// ending a session returns to the launcher.
    pub from_cli: bool,
    /// The running session (`Some` in [`Phase::Loading`] and
    /// [`Phase::Active`]).
    pub session: Option<Session>,
    /// What the session plays on.
    pub stage: Option<Stage>,
    /// The Sandbox's own directories (`None` without a user data directory:
    /// nothing can be saved then).
    pub dirs: Option<SandboxDirs>,
    /// The latest outcome or error line, for the HUD and the panel.
    pub notice: Option<String>,
    /// The Classic graybox game and banner to put back at session end.
    classic: Option<Box<ClassicKeep>>,
    /// A step, teleport or restore happened: `control::snap` snaps the
    /// render interpolation after the fixed loop.
    pending_snap: bool,
}

impl Lab {
    /// The idle state of a process (`from_cli`: started with `--sandbox`).
    pub(crate) fn new(from_cli: bool) -> Self {
        Self {
            phase: Phase::Idle,
            from_cli,
            session: None,
            stage: None,
            dirs: SandboxDirs::default_location(),
            notice: None,
            classic: None,
            pending_snap: false,
        }
    }
}

/// What the command line asked of the Sandbox (read by `lifecycle::startup`).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub(crate) struct LabLaunch {
    /// `--sandbox`, `--arena`, `--sandbox-profile`.
    pub args: SandboxArgs,
    /// `--level NAME`: with `--sandbox`, the session starts on that level.
    pub level: Option<String>,
    /// `--no-menu`: with `--sandbox`, skip the launcher (graybox stage).
    pub no_menu: bool,
}

/// A request to the Sandbox. The view writes them; `lifecycle` and `control`
/// carry them out (see the module docs for who takes which).
#[derive(Message, Clone, Debug)]
pub(crate) enum LabRequest {
    /// Open the stage launcher.
    OpenLauncher,
    /// Start a session on `stage` with the named profile (`None`: Classic).
    Start {
        /// What to play on.
        stage: Stage,
        /// Profile name (built-in or saved).
        profile: Option<String>,
    },
    /// End the session (or close the launcher). In a `--sandbox` process
    /// the launcher is what an ended session returns to, and closing the
    /// launcher itself does nothing: there is no Classic menu behind it.
    End,
    /// Quit the program, as the main menu's "Quit" does. The launcher of a
    /// `--sandbox` process offers it (there is no Classic menu behind that
    /// launcher). A running session is ended first, so its recording is
    /// written.
    Quit,
    /// Run a command through `Session::execute`.
    Do(Command),
    /// Open the inspector panel.
    OpenInspector,
    /// Close the inspector panel.
    CloseInspector,
    /// Save the session's current tuning as a user profile.
    SaveProfile {
        /// Profile name (`asamu_sandbox::profile::valid_profile_name`).
        name: String,
    },
}

/// The Sandbox's system sets (placement: see the module docs).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum LabSet {
    /// Keys to requests, before `cursor_and_pause` sees them.
    Input,
    /// Requests to the session and the clocks, before the fixed loop.
    Apply,
    /// After each tick (`FixedLast`): observation only.
    Observe,
    /// Presentation, after the camera was placed.
    Draw,
}

/// Run condition: a session is running.
pub(crate) fn lab_active(lab: Res<Lab>) -> bool {
    lab.phase == Phase::Active
}

/// Run condition: the stage launcher is open.
pub(crate) fn lab_launcher(lab: Res<Lab>) -> bool {
    lab.phase == Phase::Launcher
}

/// Places the Sandbox's system sets (the plugin's own; the runtime's tests
/// build on it).
fn configure_sets(app: &mut App) {
    app.configure_sets(
        RunFixedMainLoop,
        (LabSet::Input, LabSet::Apply)
            .chain()
            .before(crate::cursor_and_pause)
            .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
    )
    .configure_sets(Update, LabSet::Draw.after(crate::sync_camera));
}

/// The read-only view of the running session (`None` without one). The view
/// reads the simulation through this, never through `Sim` mutably.
pub(crate) fn inspection<'a>(sim: &'a Sim, lab: &'a Lab) -> Option<Inspection<'a>> {
    lab.session
        .as_ref()
        .map(|session| Inspection::new(&sim.game, sim.script.as_ref(), session))
}

/// The inspector's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PanelTab {
    /// Parameters, one page per group.
    #[default]
    Tune,
    /// Rules, abilities, teleport targets, bookmarks.
    Rules,
    /// Freeze, step, speed, save-state slots, rewind.
    Time,
    /// Visualiser toggles.
    View,
    /// Built-in and user profiles.
    Profiles,
    /// The stage list.
    Stage,
}

/// The inspector panel and the quick tuner. View-owned: only the view writes
/// it, except [`PanelState::open`], which `control::apply` writes when it
/// opens or closes `ui::Screen::Sandbox` for the inspector.
#[derive(Resource, Clone, Debug, PartialEq)]
pub(crate) struct PanelState {
    /// The inspector is open.
    pub open: bool,
    /// The selected tab.
    pub tab: PanelTab,
    /// The selected parameter group (index into `Catalog::groups`).
    pub group: usize,
    /// The selected row of the group.
    pub row: usize,
    /// Freeze the simulation while the inspector is open.
    pub freeze_while_open: bool,
    /// The quick tuner's parameter keys (usable during play).
    pub pinned: Vec<String>,
    /// The quick tuner's selected entry of `pinned`.
    pub pinned_index: usize,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            open: false,
            tab: PanelTab::default(),
            group: 0,
            row: 0,
            freeze_while_open: true,
            // A starting selection for the quick tuner (ours). Air control
            // is the landed value: it is the one a pawn runs on from its
            // first landing; `movement.air_control` only reaches a pawn
            // that has not landed yet.
            pinned: [
                "movement.custom_gravity_scaling",
                "movement.jump_velocity",
                "pawn.landed_air_control",
                "pawn.move_speed",
                "gun.grapple_accel",
                "gun.max_distance",
                "boots.boost_strength",
            ]
            .map(str::to_owned)
            .to_vec(),
            pinned_index: 0,
        }
    }
}

/// Which visualisers are drawn. View-owned.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VizSettings {
    /// Velocity arrow (with its horizontal component).
    pub velocity: bool,
    /// The collision cylinder and the floor normal.
    pub cylinder: bool,
    /// The aim ray, coloured by whether the hit can be grappled.
    pub aim: bool,
    /// The trail of the latest ticks.
    pub trail: bool,
    /// The archived trails of earlier attempts.
    pub attempts: bool,
    /// The predicted arc (static world; opt-in).
    pub prediction: bool,
    /// Arena markers.
    pub markers: bool,
    /// Speed and height graphs.
    pub graphs: bool,
}

impl Default for VizSettings {
    fn default() -> Self {
        Self {
            velocity: true,
            cylinder: false,
            aim: true,
            trail: true,
            attempts: true,
            prediction: false,
            markers: true,
            graphs: true,
        }
    }
}

/// The gizmo group of the Sandbox's visualisers, so the level gizmos and the
/// default group are left alone.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub(crate) struct SandboxGizmos;

/// Registers the Sandbox. Idle until a session starts.
pub(crate) struct SandboxPlugin {
    launch: LabLaunch,
}

impl SandboxPlugin {
    /// The plugin for this process's command line.
    pub(crate) fn from_cli(cli: &Cli) -> Self {
        Self {
            launch: LabLaunch {
                args: cli.sandbox.clone(),
                level: cli.level.clone(),
                no_menu: cli.no_menu,
            },
        }
    }
}

impl Plugin for SandboxPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Lab::new(self.launch.args.enabled))
            .insert_resource(self.launch.clone())
            .init_resource::<PanelState>()
            .init_resource::<VizSettings>()
            .add_message::<LabRequest>()
            .init_gizmo_group::<SandboxGizmos>();
        configure_sets(app);
        // The runtime.
        lifecycle::build(app);
        control::build(app);
        storage::build(app);
        // The view.
        input::build(app);
        panel::build(app);
        widgets::build(app);
        launcher::build(app);
        hud::build(app);
        viz::build(app);
        graphs::build(app);
        arena_view::build(app);
    }
}
