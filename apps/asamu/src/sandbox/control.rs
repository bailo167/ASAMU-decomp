//! The single writer: the only Sandbox module that changes the simulation
//! (`Sim`) and the clocks (`Time<Virtual>`, `Time<Fixed>`). A boundary test
//! in `asamu-sandbox` scans the plugin's files to keep it that way.
//!
//! Three jobs:
//!
//! - **Lifecycle writes** ([`carry_out`]). `lifecycle` decides what should
//!   happen (open the launcher, start or end a session) and queues a
//!   [`SimOp`]; this module carries it out in one go, so the session, the
//!   simulation, the clocks and the open screen always change together.
//! - **The session at work** ([`apply`], before the fixed loop of every
//!   frame): adopts a freshly loaded level, makes the game run the session's
//!   parameter set before its next tick, executes the requested commands,
//!   opens and closes the inspector, moves the player in fly placement and
//!   brings the Bevy clocks to the session's time-control model.
//! - **Around each tick**: [`before_tick`] (the session's sticky rules) and
//!   [`after_tick`] (telemetry and rewind keyframes; read-only), and
//!   [`snap`] after the fixed loop.
//!
//! What is *not* here, on purpose: nothing calls `Game::tick` (the app's
//! unchanged `fixed_tick` does, with the unchanged fixed `dt`), and nothing
//! pauses the game itself (`Game::pause` stays the pause menu's).
//!
//! # Time control
//!
//! | Function | Mechanism |
//! |---|---|
//! | Freeze | `Time<Virtual>::pause()`: the fixed loop stops, the game stays `Playing` |
//! | Single step | while frozen, one fixed timestep of overstep per frame, then a snap |
//! | Slow motion, fast-forward | `Time<Virtual>::set_relative_speed` |
//!
//! The clocks follow the session's model and never the reverse, and they
//! are written only when they differ from it: a session that never used
//! time control never touches them. When a session ends the virtual clock
//! is back to unpaused at speed 1.
//!
//! The inspector's "freeze while open" is the host's own: it pauses the
//! virtual clock while the panel is open without changing the session's
//! model, so closing the panel puts time back exactly as it was.
//!
//! A freeze takes effect in the frame it is asked for (the frame's share of
//! virtual time is dropped as well as pausing the clock), so no tick runs
//! between "freeze" and the frozen picture. Speed changes and the end of a
//! freeze count from the next frame.
//!
//! # The mouse
//!
//! A start from the launcher captures the mouse, as the menu's "New Game"
//! does; a start from the command line does not (a click captures it, as in
//! every run that skips the menu). The inspector frees the mouse while it is
//! open and puts it back as it found it.

use std::time::Duration;

use asamu_core::coords::{ue_forward_flat, ue_right_flat};
use asamu_core::glam as sim_glam;
use asamu_game::asamu_kismet::Output;
use asamu_game::save::PlayMode;
use asamu_game::{Game, GameState};
use asamu_sandbox::session::{Session, SimCx};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::{ClassicKeep, Lab, LabRequest, LabSet, PanelState, Phase, Stage, lab_active, storage};
use crate::ui::{self, FlowRequest, Play, Screen, UiLaunch, UiState};
use crate::{PendingInput, Sim, kismet};

/// Fly placement speed, UU per real second (ours; placement, not physics).
const FLY_SPEED: f32 = 1200.0;
/// Fly placement speed factor while left shift is held (ours).
const FLY_FAST: f32 = 4.0;
/// Longest frame fly placement moves for, seconds: a hitch does not fling
/// the player away.
const FLY_MAX_FRAME: f32 = 0.1;

/// Why a session cannot start next to saves on disk.
pub(super) const ON_DISK_SAVES: &str = "The Sandbox keeps away from saves on disk: start with \
                                        --sandbox to use it.";

/// A lifecycle step `lifecycle` decided on, carried out by [`carry_out`].
pub(super) enum SimOp {
    /// Show a line in the notice area.
    Notice(String),
    /// Open the stage launcher (a running session ends first).
    OpenLauncher,
    /// Start a session on a hand-made level: `game` takes the place of the
    /// Classic graybox game, which is kept and put back at the end.
    StartHandMade {
        /// The session.
        session: Box<Session>,
        /// The session's game on the stage's level.
        game: Box<Game>,
        /// The stage.
        stage: Stage,
        /// Capture the mouse at once (a start from the launcher, as the
        /// menu's "New Game" does). `false` for a start from the command
        /// line: the window has just opened, and a click captures the mouse
        /// as in every run that skips the menu.
        capture: bool,
    },
    /// Start a session on a converted map through the unchanged load flow.
    StartMap {
        /// The session.
        session: Box<Session>,
        /// The map, as the launch lists it.
        map: String,
    },
    /// A session for the level `main.rs` is already loading (`--level`).
    AwaitLevel {
        /// The session.
        session: Box<Session>,
        /// The level, as it was asked for.
        map: String,
    },
    /// The session's simulation went away (a Kismet map change): the
    /// session waits for the next one.
    AwaitNext,
    /// The simulation is about to be replaced by a map change: a running
    /// recording is finished and written first.
    SaveRecording,
    /// End the session, or close the launcher.
    End {
        /// Why (shown in the notice area), when it was not asked for.
        notice: Option<String>,
    },
    /// Quit the program (a running session ends first).
    Quit,
}

/// The queue of lifecycle steps (filled by `lifecycle`, emptied by
/// [`carry_out`] in the same frame).
#[derive(Resource, Default)]
pub(super) struct SimOps(Vec<SimOp>);

impl SimOps {
    /// Queues `op`.
    pub(super) fn push(&mut self, op: SimOp) {
        self.0.push(op);
    }

    /// Nothing is queued.
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Run condition of [`carry_out`]: something is queued.
pub(super) fn has_ops(ops: Res<SimOps>) -> bool {
    !ops.is_empty()
}

/// The writer's own bookkeeping.
#[derive(Resource, Default)]
struct Book {
    /// The banner the session last put into `Sim`. A simulation that
    /// carries another one (a freshly loaded level) gets the session's.
    banner: String,
    /// The Sandbox changed the virtual clock (it is put back when no
    /// session is left).
    clocks_touched: bool,
    /// The mouse was captured when the inspector opened: closing it
    /// captures the mouse again (and leaves it free otherwise).
    inspector_took_mouse: bool,
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<SimOps>()
        .init_resource::<Book>()
        .add_systems(
            RunFixedMainLoop,
            (
                apply.in_set(LabSet::Apply).run_if(has_session),
                snap.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop)
                    .run_if(snap_pending),
            ),
        )
        .add_systems(FixedFirst, before_tick.run_if(lab_active))
        .add_systems(
            FixedLast,
            after_tick.in_set(LabSet::Observe).run_if(lab_active),
        )
        .add_systems(Update, idle_clock.run_if(clock_left_behind))
        // While the Sandbox holds the fixed loop still, no fixed update tells
        // Bevy that the frame's messages may be dropped, and every message
        // buffer (mouse motion included) would grow until time runs again.
        // Bevy's own signal, sent for the frozen frames, keeps them turning
        // over exactly as when a tick runs every frame.
        .add_systems(
            Last,
            bevy::ecs::message::signal_message_update_system.run_if(frozen_by_the_lab),
        );
}

fn has_session(lab: Res<Lab>) -> bool {
    lab.session.is_some()
}

fn snap_pending(lab: Res<Lab>) -> bool {
    lab.pending_snap
}

fn clock_left_behind(lab: Res<Lab>, book: Res<Book>) -> bool {
    lab.session.is_none() && book.clocks_touched
}

fn frozen_by_the_lab(lab: Res<Lab>, virt: Res<Time<Virtual>>) -> bool {
    lab.session.is_some() && virt.is_paused()
}

/// The virtual clock as Classic has it: running at speed 1. Writes only
/// what differs.
fn reset_clock(virt: &mut ResMut<Time<Virtual>>) {
    if virt.is_paused() {
        virt.unpause();
    }
    if virt.relative_speed() != 1.0 {
        virt.set_relative_speed(1.0);
    }
}

/// Captures (`true`) or releases the mouse (what `ui::flow` does around its
/// own screens).
fn set_grab(cursor: &mut Query<&mut CursorOptions, With<PrimaryWindow>>, grab: bool) {
    if let Ok(mut c) = cursor.single_mut() {
        let mode = if grab {
            CursorGrabMode::Locked
        } else {
            CursorGrabMode::None
        };
        if c.grab_mode != mode {
            c.grab_mode = mode;
        }
        if c.visible == grab {
            c.visible = !grab;
        }
    }
}

/// Drops look and button input gathered for a tick that will not see it.
fn drop_input(pending: &mut Option<ResMut<PendingInput>>) {
    if let Some(p) = pending.as_mut() {
        **p = PendingInput::default();
    }
}

/// Longest notice line that is kept, characters. A refusal can quote a
/// user's file back (the unknown key of a profile, say), and the line is
/// shown, and copied for the view, on every frame.
const NOTICE_LIMIT: usize = 400;

/// `line`, cut to [`NOTICE_LIMIT`] characters (ending in `...` when cut).
fn bounded(line: String) -> String {
    if line.chars().count() <= NOTICE_LIMIT {
        return line;
    }
    let mut cut: String = line.chars().take(NOTICE_LIMIT - 3).collect();
    cut.push_str("...");
    cut
}

/// The notice line for a recording that was written, or was not.
fn saved_line(result: Result<std::path::PathBuf, String>) -> String {
    match result {
        Ok(path) => format!(
            "sandbox recording saved to {} (never a parity trace)",
            path.display()
        ),
        Err(e) => format!("the sandbox recording was NOT saved: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle writes.
// ---------------------------------------------------------------------------

/// Everything a lifecycle step may change.
#[derive(SystemParam)]
pub(super) struct Writer<'w, 's> {
    commands: Commands<'w, 's>,
    lab: ResMut<'w, Lab>,
    ops: ResMut<'w, SimOps>,
    book: ResMut<'w, Book>,
    sim: Option<ResMut<'w, Sim>>,
    virt: ResMut<'w, Time<Virtual>>,
    state: ResMut<'w, UiState>,
    panel: ResMut<'w, PanelState>,
    play: ResMut<'w, Play>,
    saves: Res<'w, ui::Saves>,
    launch: Res<'w, UiLaunch>,
    pending: Option<ResMut<'w, PendingInput>>,
    game_load: Option<ResMut<'w, crate::GameLoad>>,
    cursor: Query<'w, 's, &'static mut CursorOptions, With<PrimaryWindow>>,
    flow: MessageWriter<'w, FlowRequest>,
}

/// Carries out the queued lifecycle steps, in order. Runs right after
/// `lifecycle::guard` and `lifecycle::requests`, before the menu flow.
pub(super) fn carry_out(mut w: Writer) {
    let ops = std::mem::take(&mut w.ops.0);
    let mut lines = Vec::new();
    for op in ops {
        w.run(op, &mut lines);
    }
    if !lines.is_empty() {
        let text = bounded(lines.join(" | "));
        info!("sandbox: {text}");
        w.lab.notice = Some(text);
    }
}

impl Writer<'_, '_> {
    fn run(&mut self, op: SimOp, lines: &mut Vec<String>) {
        match op {
            SimOp::Notice(line) => lines.push(line),
            SimOp::OpenLauncher => self.open_launcher(lines),
            SimOp::StartHandMade {
                session,
                game,
                stage,
                capture,
            } => self.start_hand_made(*session, *game, stage, capture, lines),
            SimOp::StartMap { session, map } => self.start_map(*session, map, true, lines),
            SimOp::AwaitLevel { session, map } => self.start_map(*session, map, false, lines),
            SimOp::AwaitNext => {
                if self.lab.phase == Phase::Active && self.sim.is_none() {
                    self.lab.phase = Phase::Loading;
                }
            }
            SimOp::SaveRecording => self.save_recording(lines),
            SimOp::End { notice } => self.end(notice, lines),
            SimOp::Quit => self.quit(lines),
        }
    }

    /// The rule everything else rests on: the Sandbox does nothing while the
    /// saves of this run are on disk.
    fn saves_on_disk(&mut self, lines: &mut Vec<String>) -> bool {
        if self.saves.0.store().is_none() {
            return false;
        }
        lines.push(ON_DISK_SAVES.to_owned());
        if self.state.screen == Screen::Main {
            self.state.open_with(Screen::Main, ON_DISK_SAVES);
        }
        true
    }

    fn open_launcher(&mut self, lines: &mut Vec<String>) {
        if self.saves_on_disk(lines) {
            return;
        }
        self.end_session(lines);
        if self.lab.phase != Phase::Launcher {
            self.lab.phase = Phase::Launcher;
        }
        if self.state.screen != Screen::Sandbox {
            self.state.open(Screen::Sandbox);
        }
        set_grab(&mut self.cursor, false);
    }

    fn start_hand_made(
        &mut self,
        session: Session,
        game: Game,
        stage: Stage,
        capture: bool,
        lines: &mut Vec<String>,
    ) {
        if self.saves_on_disk(lines) {
            return;
        }
        // The game a hand-made stage replaces is the Classic graybox game:
        // the one set aside by a running session, else the one in `Sim`.
        let classic = match (&self.lab.classic, &self.sim) {
            (Some(keep), _) => Some((&keep.game, false)),
            (None, Some(sim)) => Some((&sim.game, sim.script.is_some())),
            (None, None) => None,
        };
        let refusal = match classic {
            _ if self.launch.converted.is_some() => {
                Some("hand-made stages are not available in a run on converted data")
            }
            None => Some("there is no simulation to start a hand-made stage on"),
            Some((game, scripted)) if scripted || game.scene_map().is_some() => {
                Some("hand-made stages replace the graybox game only")
            }
            Some((game, _)) if game.params().pawn.is_none() => Some(
                "the Sandbox tunes the Classic parameter set: this run uses the placeholder \
                 parameters",
            ),
            Some(_) => None,
        };
        if let Some(why) = refusal {
            lines.push(why.to_owned());
            return;
        }
        self.end_session(lines);
        let Some(sim) = self.sim.as_mut() else {
            return;
        };
        let banner = session.banner(&game);
        let keep = ClassicKeep {
            game: std::mem::replace(&mut sim.game, game),
            banner: std::mem::replace(&mut sim.banner, banner.clone()),
        };
        sim.script = None;
        sim.game.start();
        sim.snap_interpolation();
        lines.push(format!(
            "Sandbox session on {} with the profile {}",
            sim.game.level().name,
            session.profile().name
        ));
        self.book.banner = banner;
        if *self.play != Play::default() {
            *self.play = Play::default();
        }
        let lab = &mut *self.lab;
        lab.classic = Some(Box::new(keep));
        lab.session = Some(session);
        lab.stage = Some(stage);
        lab.phase = Phase::Active;
        lab.pending_snap = false;
        // Play on: no screen, no stale input.
        if self.panel.open {
            self.panel.open = false;
        }
        self.state.close();
        if capture {
            set_grab(&mut self.cursor, true);
        }
        drop_input(&mut self.pending);
    }

    /// A session on a converted map. `request`: ask the menu flow to load
    /// it (`false`: `main.rs` is loading it already).
    fn start_map(&mut self, session: Session, map: String, request: bool, lines: &mut Vec<String>) {
        if self.saves_on_disk(lines) {
            return;
        }
        if self.launch.converted.is_none() {
            lines.push(format!(
                "{map} is a converted map: start with --converted DIR to play it"
            ));
            return;
        }
        self.end_session(lines);
        if request {
            // A game left behind the menu is not the session's: the load
            // replaces it, and the session must not adopt it meanwhile.
            if self.sim.is_some() {
                self.commands.remove_resource::<Sim>();
            }
            self.flow.write(FlowRequest::Load {
                map: map.clone(),
                mode: PlayMode::Story,
            });
        }
        lines.push(format!(
            "Sandbox session: loading {map} with the profile {}",
            session.profile().name
        ));
        self.book.banner.clear();
        if self.panel.open {
            self.panel.open = false;
        }
        let lab = &mut *self.lab;
        lab.session = Some(session);
        lab.stage = Some(Stage::Map(map));
        lab.phase = Phase::Loading;
        lab.pending_snap = false;
    }

    fn save_recording(&mut self, lines: &mut Vec<String>) {
        let lab = &mut *self.lab;
        if let (Some(session), Some(sim)) = (lab.session.as_mut(), self.sim.as_mut())
            && sim.game.is_recording()
        {
            let Sim { game, script, .. } = &mut **sim;
            if let Some(recording) = session.finish(&mut SimCx { game, script }) {
                lines.push(saved_line(storage::write_recording(
                    lab.dirs.as_ref(),
                    &recording,
                )));
            }
        }
    }

    /// Ends the running session, if there is one: its recording is written,
    /// the virtual clock runs at speed 1 again and Classic gets its
    /// simulation back (graybox: the game that was set aside; converted
    /// data: no simulation, as after the original's return to the front
    /// end). Leaves the Sandbox idle; the caller opens the next screen.
    fn end_session(&mut self, lines: &mut Vec<String>) {
        let lab = &mut *self.lab;
        let Some(mut session) = lab.session.take() else {
            return;
        };
        let was_loading = lab.phase == Phase::Loading;
        if let Some(sim) = self.sim.as_mut()
            && sim.game.is_recording()
        {
            let Sim { game, script, .. } = &mut **sim;
            if let Some(recording) = session.finish(&mut SimCx { game, script }) {
                lines.push(saved_line(storage::write_recording(
                    lab.dirs.as_ref(),
                    &recording,
                )));
            }
        }
        reset_clock(&mut self.virt);
        self.book.clocks_touched = false;
        self.book.banner.clear();
        match (lab.classic.take(), self.sim.as_mut()) {
            (Some(keep), Some(sim)) => {
                let keep = *keep;
                sim.game = keep.game;
                sim.banner = keep.banner;
                sim.script = None;
                sim.snap_interpolation();
            }
            (Some(_), None) => {}
            (None, _) if self.launch.converted.is_some() => {
                self.commands.remove_resource::<Sim>();
                if *self.play != Play::default() {
                    *self.play = Play::default();
                }
                if was_loading {
                    // A load still in flight would hand Classic a game later.
                    if let Some(load) = self.game_load.as_mut()
                        && (load.request.is_some() || load.task.is_some())
                    {
                        load.request = None;
                        load.task = None;
                    }
                    if self.state.screen == Screen::Loading {
                        self.flow.write(FlowRequest::CancelLoad);
                    }
                }
            }
            (None, Some(sim)) => {
                // Not reachable through `start_hand_made` (which always
                // keeps the Classic game); if it ever is, Classic still must
                // not inherit the session's game.
                if let Ok(game) = Game::graybox() {
                    let game = game.with_movement_model(sim.game.movement_model());
                    sim.banner = crate::banner(&game);
                    sim.game = game;
                    sim.script = None;
                    sim.snap_interpolation();
                }
            }
            (None, None) => {}
        }
        lab.stage = None;
        lab.pending_snap = false;
        lab.phase = Phase::Idle;
        if self.panel.open {
            self.panel.open = false;
        }
        lines.push("the Sandbox session ended".to_owned());
    }

    /// `LabRequest::End` and the guards: ends the session or closes the
    /// launcher, then shows the launcher again (a `--sandbox` process has no
    /// Classic menu to go back to) or the main menu.
    fn end(&mut self, notice: Option<String>, lines: &mut Vec<String>) {
        if self.lab.session.is_none() && self.lab.phase == Phase::Idle {
            return;
        }
        let first = lines.len();
        self.end_session(lines);
        lines.extend(notice);
        if self.lab.from_cli {
            if self.lab.phase != Phase::Launcher {
                self.lab.phase = Phase::Launcher;
            }
            if self.state.screen != Screen::Sandbox {
                self.state.open(Screen::Sandbox);
            }
        } else {
            self.lab.phase = Phase::Idle;
            let said = bounded(lines.get(first..).unwrap_or_default().join(" | "));
            if said.is_empty() {
                self.state.open(Screen::Main);
            } else {
                self.state.open_with(Screen::Main, said);
            }
        }
        set_grab(&mut self.cursor, false);
    }

    /// `LabRequest::Quit`: ends the session as [`Writer::end_session`] does
    /// (its recording is written, Classic gets its simulation back), then
    /// asks the menu flow to quit, exactly as the main menu's "Quit" button
    /// does. The flow runs after this system in the same frame.
    fn quit(&mut self, lines: &mut Vec<String>) {
        self.end_session(lines);
        self.flow.write(FlowRequest::Quit);
    }
}

// ---------------------------------------------------------------------------
// The session at work.
// ---------------------------------------------------------------------------

/// The menu-side state [`apply`] changes for the inspector.
#[derive(SystemParam)]
pub(super) struct Desk<'w, 's> {
    state: ResMut<'w, UiState>,
    panel: ResMut<'w, PanelState>,
    pending: Option<ResMut<'w, PendingInput>>,
    cursor: Query<'w, 's, &'static mut CursorOptions, With<PrimaryWindow>>,
}

impl Desk<'_, '_> {
    /// Opens the inspector over a running game: `ui::Screen::Sandbox` frees
    /// the mouse and gates the gameplay input, as every menu screen does.
    /// `Some(captured)`: it opened, and the mouse was captured until now.
    fn open_inspector(&mut self) -> Option<bool> {
        if self.panel.open || self.state.screen != Screen::None {
            return None;
        }
        let captured = self.mouse_captured();
        self.panel.open = true;
        self.state.open(Screen::Sandbox);
        set_grab(&mut self.cursor, false);
        // No stale look or button reaches a tick that runs with the panel
        // open (and the grapple stays unarmed until the panel is closed).
        drop_input(&mut self.pending);
        Some(captured)
    }

    /// Closes the inspector and plays on, capturing the mouse again if
    /// `capture`. `false`: it was not open.
    fn close_inspector(&mut self, capture: bool) -> bool {
        if !self.panel.open {
            return false;
        }
        self.panel.open = false;
        if self.state.screen == Screen::Sandbox {
            self.state.close();
            if capture {
                set_grab(&mut self.cursor, true);
            }
            drop_input(&mut self.pending);
        }
        true
    }

    fn mouse_captured(&self) -> bool {
        self.cursor
            .single()
            .is_ok_and(|c| c.grab_mode != CursorGrabMode::None)
    }

    /// The mouse is captured and no screen is open: the keys are the game's.
    fn playing(&self) -> bool {
        self.state.screen == Screen::None && self.mouse_captured()
    }
}

/// Puts the session's banner into the simulation (the stock HUD prints it).
fn write_banner(session: &Session, sim: &mut ResMut<Sim>, book: &mut Book) {
    let banner = session.banner(&sim.game);
    if sim.banner != banner {
        sim.banner.clone_from(&banner);
    }
    book.banner = banner;
}

/// Before the fixed loop of every frame while a session exists.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn apply(
    mut lab: ResMut<Lab>,
    mut requests: MessageReader<LabRequest>,
    sim: Option<ResMut<Sim>>,
    mut virt: ResMut<Time<Virtual>>,
    mut fixed: ResMut<Time<Fixed>>,
    real: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut desk: Desk,
    mut book: ResMut<Book>,
    mut ops: ResMut<SimOps>,
) {
    let Lab {
        phase,
        session,
        stage,
        dirs,
        notice,
        pending_snap,
        ..
    } = &mut *lab;
    let (Some(session), Some(mut sim)) = (session.as_mut(), sim) else {
        // Between two levels: nothing to act on, and nothing is kept for
        // later.
        requests.clear();
        return;
    };

    // A level finished loading: the session takes it over before its first
    // tick.
    if *phase == Phase::Loading {
        *phase = Phase::Active;
        if let Some(map) = sim.game.map_name() {
            *stage = Some(Stage::Map(map.to_owned()));
        }
        let line = format!(
            "Sandbox session on {} with the profile {}",
            sim.game.level().name,
            session.profile().name
        );
        info!("sandbox: {line}");
        *notice = Some(line);
    }
    if *phase != Phase::Active {
        requests.clear();
        return;
    }

    // The game runs the session's parameter set (a loaded level arrives
    // with the Classic set; a pristine session leaves it untouched).
    if *sim.game.params() != *session.params() {
        let Sim { game, script, .. } = &mut *sim;
        if let Err(e) = session.reconcile(&mut SimCx { game, script }) {
            ops.push(SimOp::End {
                notice: Some(format!("the Sandbox cannot run this game: {e}")),
            });
            requests.clear();
            return;
        }
    }
    if sim.banner != book.banner {
        write_banner(session, &mut sim, &mut book);
    }

    // Requests.
    let was_flying = session.flying();
    let mut executed = false;
    for request in requests.read() {
        match request {
            LabRequest::Do(command) => {
                let Sim { game, script, .. } = &mut *sim;
                let line = match session.execute(command.clone(), &mut SimCx { game, script }) {
                    Ok(outcome) => {
                        executed = true;
                        if outcome.discontinuity {
                            *pending_snap = true;
                        }
                        match outcome.recording {
                            Some(recording) => format!(
                                "{} | {}",
                                outcome.message,
                                saved_line(storage::write_recording(dirs.as_ref(), &recording))
                            ),
                            None => outcome.message,
                        }
                    }
                    Err(e) => format!("{}: {e}", command.name()),
                };
                let line = bounded(line);
                info!("sandbox: {line}");
                *notice = Some(line);
            }
            LabRequest::OpenInspector => {
                if let Some(captured) = desk.open_inspector() {
                    book.inspector_took_mouse = captured;
                }
            }
            LabRequest::CloseInspector => {
                desk.close_inspector(book.inspector_took_mouse);
            }
            // `lifecycle` handles these.
            LabRequest::OpenLauncher
            | LabRequest::Start { .. }
            | LabRequest::End
            | LabRequest::Quit
            | LabRequest::SaveProfile { .. } => {}
        }
    }
    if executed {
        write_banner(session, &mut sim, &mut book);
    }

    // The inspector and its screen go together.
    if desk.panel.open && desk.state.screen != Screen::Sandbox {
        desk.panel.open = false;
    } else if !desk.panel.open && desk.state.screen == Screen::Sandbox {
        desk.state.close();
    }

    // Fly placement: the keys move the frozen player on real time.
    if was_flying || session.flying() {
        // Space means "up" here: it must not become a jump (or a rocket
        // boost) in the first tick after.
        if let Some(p) = desk.pending.as_mut()
            && p.jump
        {
            p.jump = false;
        }
    }
    if session.flying() && desk.playing() {
        let look = desk.pending.as_ref().map_or(0.0, |p| p.look_yaw);
        let yaw = sim.game.player().yaw + look;
        let direction = ue_forward_flat(yaw)
            * crate::axis(
                &keys,
                [KeyCode::KeyW, KeyCode::ArrowUp],
                [KeyCode::KeyS, KeyCode::ArrowDown],
            )
            + ue_right_flat(yaw)
                * crate::axis(
                    &keys,
                    [KeyCode::KeyD, KeyCode::ArrowRight],
                    [KeyCode::KeyA, KeyCode::ArrowLeft],
                )
            + sim_glam::Vec3::Z
                * crate::axis(
                    &keys,
                    [KeyCode::Space, KeyCode::Space],
                    [KeyCode::ControlLeft, KeyCode::ControlLeft],
                );
        if direction != sim_glam::Vec3::ZERO {
            let speed = if keys.pressed(KeyCode::ShiftLeft) {
                FLY_SPEED * FLY_FAST
            } else {
                FLY_SPEED
            };
            let delta =
                direction.normalize_or_zero() * speed * real.delta_secs().min(FLY_MAX_FRAME);
            let Sim { game, script, .. } = &mut *sim;
            session.fly_move(&mut SimCx { game, script }, delta);
            *pending_snap = true;
        }
    }

    // The clocks follow the model (written only where they differ).
    let frozen = session.time().frozen() || (desk.panel.open && desk.panel.freeze_while_open);
    if virt.is_paused() != frozen {
        if frozen {
            virt.pause();
            // The pause counts from the next frame on; this frame's share of
            // time is dropped too, so no tick runs after the freeze.
            virt.advance_by(Duration::ZERO);
        } else {
            virt.unpause();
        }
        book.clocks_touched = true;
    }
    let speed = session.time().speed();
    if virt.relative_speed() != speed {
        virt.set_relative_speed(speed);
        book.clocks_touched = true;
    }
    // A single step: one timestep of overstep makes the fixed loop run
    // exactly one tick this frame (it adds the frozen clock's zero delta).
    if frozen
        && virt.delta().is_zero()
        && sim.game.state() == GameState::Playing
        && session.time_mut().take_step()
    {
        let step = fixed.timestep();
        fixed.accumulate_overstep(step);
        *pending_snap = true;
    }
}

/// Before each tick: the session's sticky rules. Nothing when no tick will
/// run (the game is not playing, or a map change has taken the simulation
/// away for now).
fn before_tick(mut lab: ResMut<Lab>, sim: Option<ResMut<Sim>>) {
    let Some(mut sim) = sim else {
        return;
    };
    if sim.game.state() != GameState::Playing {
        return;
    }
    if let Some(session) = lab.session.as_mut() {
        let Sim { game, script, .. } = &mut *sim;
        session.before_tick(&mut SimCx { game, script });
    }
}

/// After each tick: the session's read-outs. Reads the simulation only.
fn after_tick(
    mut lab: ResMut<Lab>,
    sim: Option<Res<Sim>>,
    mut ticks: MessageReader<ui::GameTick>,
    mut frames: MessageReader<kismet::KismetFrame>,
    mut ops: ResMut<SimOps>,
) {
    let Some(sim) = sim else {
        ticks.clear();
        frames.clear();
        return;
    };
    // The report of the tick that just ran (older ones, written before the
    // session, are not this game's).
    let now = sim.game.clock().tick();
    let report = ticks
        .read()
        .map(|t| t.0)
        .filter(|report| report.tick == now)
        .last();
    let leaving = frames.read().any(|frame| {
        frame
            .outputs
            .iter()
            .any(|output| matches!(output, Output::LevelTransition { .. }))
    });
    let Some(session) = lab.session.as_mut() else {
        return;
    };
    if let Some(report) = report {
        session.after_tick(&sim.game, sim.script.as_ref(), &report);
    }
    // The level's Kismet opens another map: this simulation is dropped in
    // the same frame, so its recording is written before that.
    if leaving && sim.game.is_recording() {
        ops.push(SimOp::SaveRecording);
    }
}

/// After the fixed loop: a step, a teleport or a restored state shows at
/// once instead of being blended from the previous position.
fn snap(mut lab: ResMut<Lab>, sim: Option<ResMut<Sim>>) {
    lab.pending_snap = false;
    if let Some(mut sim) = sim {
        sim.snap_interpolation();
    }
}

/// No session, but the virtual clock is still as a session left it: put it
/// back. (A session's end does this itself; this is the safety net.)
fn idle_clock(mut virt: ResMut<Time<Virtual>>, mut book: ResMut<Book>) {
    reset_clock(&mut virt);
    book.clocks_touched = false;
}

#[cfg(test)]
pub(super) mod testing {
    //! A headless app with the Sandbox's runtime (`lifecycle` and this
    //! module), the real `fixed_tick` and Bevy's clocks, advancing exactly
    //! one fixed timestep per `App::update`.

    use asamu_game::Game;
    use bevy::prelude::*;
    use bevy::time::{TimePlugin, TimeUpdateStrategy};

    use super::super::{Lab, LabLaunch, LabRequest, PanelState, configure_sets, lifecycle};
    use crate::ui::{self, FlowRequest, Play, UiAction, UiLaunch, UiState};
    use crate::{PendingInput, SandboxArgs, ScriptedWalk, Sim, kismet};

    /// How the app under test was "launched".
    #[derive(Default)]
    pub(in crate::sandbox) struct Rig {
        /// A `--sandbox` process.
        pub from_cli: bool,
        /// `--arena`, `--sandbox-profile`.
        pub args: SandboxArgs,
        /// `--level`.
        pub level: Option<String>,
        /// `--no-menu`.
        pub no_menu: bool,
        /// The launch (default: the graybox with the main menu).
        pub launch: UiLaunch,
        /// The placeholder configuration (`--placeholder`).
        pub placeholder: bool,
        /// No simulation yet (converted data before a level has loaded).
        pub without_sim: bool,
    }

    /// Times the simulation was written since the app started (its
    /// insertion does not count).
    #[derive(Resource, Default)]
    struct SimWrites(u32);

    fn count_sim_writes(sim: Option<Res<Sim>>, mut writes: ResMut<SimWrites>) {
        if let Some(sim) = sim
            && sim.is_changed()
            && !sim.is_added()
        {
            writes.0 += 1;
        }
    }

    /// Every flow request written so far.
    #[derive(Resource, Default)]
    struct FlowLog(Vec<FlowRequest>);

    fn log_flow_requests(mut requests: MessageReader<FlowRequest>, mut log: ResMut<FlowLog>) {
        log.0.extend(requests.read().cloned());
    }

    /// The banner Classic gives the stock graybox game.
    pub(in crate::sandbox) fn classic_banner() -> String {
        crate::banner(&Game::graybox().expect("the graybox game builds"))
    }

    pub(in crate::sandbox) fn rig(rig: Rig) -> App {
        let mut app = App::new();
        app.add_plugins(TimePlugin);
        let game = if rig.placeholder {
            Game::graybox_placeholder()
        } else {
            Game::graybox()
        }
        .expect("the graybox game builds");
        let fixed = Time::<Fixed>::from_hz(game.clock().tick_rate_hz());
        let step = fixed.timestep();
        let mut lab = Lab::new(rig.from_cli);
        // Tests never touch the user's data directory.
        lab.dirs = None;
        app.insert_resource(fixed)
            .insert_resource(TimeUpdateStrategy::ManualDuration(step))
            .init_resource::<PendingInput>()
            .init_resource::<ScriptedWalk>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<UiState>()
            .init_resource::<ui::Saves>()
            .init_resource::<Play>()
            .insert_resource(rig.launch)
            .insert_resource(lab)
            .insert_resource(LabLaunch {
                args: rig.args,
                level: rig.level,
                no_menu: rig.no_menu,
            })
            .init_resource::<PanelState>()
            .init_resource::<SimWrites>()
            .init_resource::<FlowLog>()
            .add_message::<ui::GameTick>()
            .add_message::<kismet::KismetFrame>()
            .add_message::<FlowRequest>()
            .add_message::<UiAction>()
            .add_message::<LabRequest>()
            .add_systems(
                FixedUpdate,
                crate::fixed_tick.run_if(resource_exists::<Sim>),
            )
            .add_systems(Last, (count_sim_writes, log_flow_requests));
        if !rig.without_sim {
            let banner = crate::banner(&game);
            app.insert_resource(Sim::new(game, banner));
        }
        configure_sets(&mut app);
        lifecycle::build(&mut app);
        super::build(&mut app);
        app
    }

    pub(in crate::sandbox) fn frames(app: &mut App, n: usize) {
        for _ in 0..n {
            app.update();
        }
    }

    pub(in crate::sandbox) fn send(app: &mut App, request: LabRequest) {
        app.world_mut().write_message(request);
    }

    pub(in crate::sandbox) fn lab(app: &App) -> &Lab {
        app.world().resource::<Lab>()
    }

    pub(in crate::sandbox) fn sim(app: &App) -> &Sim {
        app.world().resource::<Sim>()
    }

    pub(in crate::sandbox) fn sim_writes(app: &App) -> u32 {
        app.world().resource::<SimWrites>().0
    }

    pub(in crate::sandbox) fn flow_requests(app: &App) -> Vec<FlowRequest> {
        app.world().resource::<FlowLog>().0.clone()
    }
}

#[cfg(test)]
mod tests {
    use asamu_player::{InputFrame, PlayerParams};
    use asamu_sandbox::command::{Command, SlotOp, TeleportTarget, TimeOp, Toggle};
    use asamu_sandbox::keys::TuneValue;
    use asamu_sandbox::recording::SandboxRecording;
    use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

    use super::super::storage::testing::{TempRoot, tree};
    use super::testing::{Rig, classic_banner, frames, lab, rig, send, sim, sim_writes};
    use super::*;

    fn tick(app: &App) -> u64 {
        sim(app).game.clock().tick()
    }

    fn virt(app: &App) -> &Time<Virtual> {
        app.world().resource::<Time<Virtual>>()
    }

    fn screen(app: &App) -> Screen {
        app.world().resource::<UiState>().screen
    }

    fn do_(app: &mut App, command: Command) {
        send(app, LabRequest::Do(command));
    }

    fn time(app: &mut App, op: TimeOp) {
        do_(app, Command::Time { op });
    }

    /// A session on the stock graybox, playing.
    fn session_on_graybox(profile: Option<&str>) -> App {
        let mut app = rig(Rig::default());
        send(
            &mut app,
            LabRequest::Start {
                stage: Stage::Graybox,
                profile: profile.map(str::to_owned),
            },
        );
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(tick(&app), 0);
        app
    }

    /// The state of a game as text (every float in its shortest exact
    /// form), for comparing two runs.
    fn fingerprint(game: &Game) -> String {
        format!(
            "{:?}|{:?}|{:?}|{}|{:?}",
            game.player(),
            game.objects(),
            game.clock().tick(),
            game.respawn_count(),
            game.active_checkpoint()
        )
    }

    /// A plain Classic graybox game ticked `ticks` times with `input`, with
    /// no app around it.
    fn classic_after(ticks: u64, input: &InputFrame) -> Game {
        let mut game = Game::graybox().unwrap();
        game.start();
        for _ in 0..ticks {
            game.tick(input).unwrap();
        }
        game
    }

    /// Holds forward and sprint in the app, and the same as an input frame
    /// (what `fixed_tick` makes of those keys).
    fn hold_forward_and_sprint(app: &mut App) -> InputFrame {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::ShiftLeft);
        InputFrame {
            move_forward: 1.0,
            sprint_held: true,
            ..InputFrame::default()
        }
    }

    #[test]
    fn a_refusal_that_quotes_its_operand_stays_a_short_notice() {
        assert_eq!(bounded("short".to_owned()), "short");
        let cut = bounded("\u{e9}".repeat(100_000));
        assert_eq!(cut.chars().count(), NOTICE_LIMIT);
        assert!(cut.ends_with("..."));

        // Through the runtime: an unknown key is quoted back by the refusal,
        // however long it is; the notice the view copies every frame is not.
        let mut app = session_on_graybox(None);
        do_(
            &mut app,
            Command::SetParam {
                key: "k".repeat(200_000),
                value: TuneValue::Float(1.0),
            },
        );
        frames(&mut app, 1);
        let notice = lab(&app).notice.clone().unwrap_or_default();
        assert!(
            notice.starts_with("set_param: unknown parameter"),
            "{notice}"
        );
        assert_eq!(notice.chars().count(), NOTICE_LIMIT);
        // The refused command changed nothing and was not logged: the frame's
        // tick ran as in Classic.
        assert_eq!(
            fingerprint(&sim(&app).game),
            fingerprint(&classic_after(1, &InputFrame::default()))
        );
        let session = lab(&app).session.as_ref().unwrap();
        assert!(session.overlay().is_empty());
        assert!(session.log().is_empty());
        assert!(session.is_pristine());
    }

    #[test]
    fn freeze_step_and_speed_drive_the_bevy_clocks() {
        let mut app = session_on_graybox(None);
        // Running: one tick per frame, and the clocks are as Classic has
        // them.
        frames(&mut app, 3);
        assert_eq!(tick(&app), 3);
        assert!(!virt(&app).is_paused());
        assert_eq!(virt(&app).relative_speed(), 1.0);
        assert!(lab(&app).session.as_ref().unwrap().is_pristine());

        // Freeze: the virtual clock pauses and no tick runs any more, not
        // even in the frame of the command. The game itself stays playing
        // (the pause menu does not open).
        time(&mut app, TimeOp::Freeze(Toggle::On));
        frames(&mut app, 1);
        assert!(virt(&app).is_paused());
        assert_eq!(tick(&app), 3);
        frames(&mut app, 4);
        assert_eq!(tick(&app), 3);
        assert_eq!(sim(&app).game.state(), GameState::Playing);

        // Single steps: exactly one tick per frame for each step asked for,
        // shown at once (no blend from the position before).
        time(&mut app, TimeOp::Step { n: 2 });
        frames(&mut app, 1);
        assert_eq!(tick(&app), 4);
        assert_eq!(sim(&app).prev_position, sim(&app).curr_position);
        assert_eq!(sim(&app).curr_position, sim(&app).game.player().position);
        frames(&mut app, 1);
        assert_eq!(tick(&app), 5);
        frames(&mut app, 3);
        assert_eq!(tick(&app), 5);
        assert!(virt(&app).is_paused());
        assert!(!lab(&app).pending_snap);

        // A step while running is a frame advance: freeze, then one tick.
        time(&mut app, TimeOp::Freeze(Toggle::Off));
        frames(&mut app, 3);
        let running = tick(&app);
        assert!(running > 5);
        time(&mut app, TimeOp::Step { n: 1 });
        frames(&mut app, 1);
        assert!(virt(&app).is_paused());
        assert_eq!(tick(&app), running + 1);
        frames(&mut app, 2);
        assert_eq!(tick(&app), running + 1);

        // Speed: the virtual clock's relative speed; ticks follow faster,
        // each still one fixed timestep long.
        time(&mut app, TimeOp::Scale { value: 2.0 });
        time(&mut app, TimeOp::Freeze(Toggle::Off));
        frames(&mut app, 1);
        assert!(!virt(&app).is_paused());
        assert_eq!(virt(&app).relative_speed(), 2.0);
        // (The scaled frame time is rounded to whole nanoseconds, so the
        // count over a stretch may be one short.)
        let before = tick(&app);
        frames(&mut app, 30);
        let ran = tick(&app) - before;
        assert!((59..=60).contains(&ran), "{ran} ticks in 30 frames at 2x");
        time(&mut app, TimeOp::Slower);
        frames(&mut app, 1);
        assert_eq!(virt(&app).relative_speed(), 1.0);
        time(&mut app, TimeOp::Slower);
        frames(&mut app, 1);
        assert_eq!(virt(&app).relative_speed(), 0.5);
        let before = tick(&app);
        frames(&mut app, 30);
        let ran = tick(&app) - before;
        assert!((14..=15).contains(&ran), "{ran} ticks in 30 frames at 0.5x");
        // A speed outside the range is refused and changes nothing.
        time(&mut app, TimeOp::Scale { value: 0.0 });
        frames(&mut app, 1);
        assert_eq!(virt(&app).relative_speed(), 0.5);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("time"))
        );

        // The simulation was only ever ticked by `fixed_tick`: it is the
        // Classic run of that many ticks, bit for bit.
        assert_eq!(
            fingerprint(&sim(&app).game),
            fingerprint(&classic_after(tick(&app), &InputFrame::default()))
        );

        // The end of the session puts the clock back.
        time(&mut app, TimeOp::Freeze(Toggle::On));
        frames(&mut app, 1);
        assert!(virt(&app).is_paused());
        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        assert!(!virt(&app).is_paused());
        assert_eq!(virt(&app).relative_speed(), 1.0);
        frames(&mut app, 2);
        assert!(!virt(&app).is_paused());
    }

    #[test]
    fn a_pristine_session_and_an_idle_lab_tick_the_game_exactly_as_classic_does() {
        // The plugin registered, no session: Classic's own game.
        let mut app = rig(Rig::default());
        let input = hold_forward_and_sprint(&mut app);
        app.world_mut().resource_mut::<Sim>().game.start();
        frames(&mut app, 400);
        let ticks = tick(&app);
        assert!(ticks >= 390, "{ticks}");
        let classic = classic_after(ticks, &input);
        assert_eq!(fingerprint(&sim(&app).game), fingerprint(&classic));
        assert_eq!(sim(&app).banner, classic_banner());
        // The run is not a trivial one: the held keys reached the game.
        assert_ne!(
            fingerprint(&classic),
            fingerprint(&classic_after(ticks, &InputFrame::default()))
        );

        // A session that changes nothing: its hooks run around every tick
        // (reconcile, rules, telemetry, rewind keyframes) and the game is
        // still the Classic run.
        let mut app = session_on_graybox(None);
        hold_forward_and_sprint(&mut app);
        frames(&mut app, 400);
        let ticks = tick(&app);
        assert!(ticks >= 390, "{ticks}");
        assert_eq!(
            fingerprint(&sim(&app).game),
            fingerprint(&classic_after(ticks, &input))
        );
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        let session = lab(&app).session.as_ref().unwrap();
        assert!(session.is_pristine());
        // The hooks did run (a respawn archives the trail as an attempt).
        let telemetry = session.telemetry();
        let observed =
            telemetry.trail().count() + telemetry.attempts().iter().map(Vec::len).sum::<usize>();
        assert!(observed > 100, "{observed}");
        assert!(telemetry.peak_speed() > 0.0);
        assert!(!session.rewind().is_empty());
        assert!(!virt(&app).is_paused());
        assert_eq!(virt(&app).relative_speed(), 1.0);
        // And the harness can tell a tuned game from Classic.
        let mut app = session_on_graybox(Some("sprinter"));
        hold_forward_and_sprint(&mut app);
        frames(&mut app, 400);
        assert_ne!(
            fingerprint(&sim(&app).game),
            fingerprint(&classic_after(tick(&app), &input))
        );
    }

    #[test]
    fn commands_go_through_the_session_and_a_tuned_set_is_never_called_original() {
        let mut app = session_on_graybox(None);
        frames(&mut app, 2);
        let pristine_banner = sim(&app).banner.clone();
        assert!(pristine_banner.contains("SANDBOX"));
        assert!(pristine_banner.contains("not verified against the original"));
        assert_ne!(pristine_banner, classic_banner());

        let key = "movement.custom_gravity_scaling";
        do_(
            &mut app,
            Command::SetParam {
                key: key.to_owned(),
                value: TuneValue::Float(0.5),
            },
        );
        frames(&mut app, 1);
        {
            let session = lab(&app).session.as_ref().unwrap();
            assert_eq!(session.overlay().len(), 1);
            assert_eq!(session.log().len(), 1);
            let sim = sim(&app);
            assert_eq!(sim.game.params(), session.params());
            assert_eq!(sim.game.params().movement.custom_gravity_scaling.value, 0.5);
            assert!(sim.banner.contains("MODIFIED"), "{}", sim.banner);
            assert!(!sim.banner.contains("ORIGINAL values"));
            assert!(lab(&app).notice.as_deref().is_some_and(|n| n.contains(key)));
        }
        // A refused command says so and changes nothing.
        let before = sim(&app).game.params().clone();
        do_(
            &mut app,
            Command::SetParam {
                key: "movement.no_such_key".to_owned(),
                value: TuneValue::Float(1.0),
            },
        );
        frames(&mut app, 1);
        assert_eq!(*sim(&app).game.params(), before);
        assert_eq!(lab(&app).session.as_ref().unwrap().log().len(), 1);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.starts_with("set_param:")),
            "{:?}",
            lab(&app).notice
        );
        // Back to Classic: the banner follows.
        do_(&mut app, Command::ResetAllParams);
        frames(&mut app, 1);
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert!(!sim(&app).banner.contains("MODIFIED"));
        assert!(sim(&app).banner.contains("SANDBOX"));

        // A jump in place (teleport, respawn, save state) is shown at once.
        frames(&mut app, 20);
        do_(
            &mut app,
            Command::Slot {
                op: SlotOp::Save { slot: 0 },
            },
        );
        frames(&mut app, 30);
        let later = tick(&app);
        do_(
            &mut app,
            Command::Teleport {
                to: TeleportTarget::Position {
                    position: [0.0, 0.0, 5000.0],
                    yaw: None,
                    pitch: None,
                },
            },
        );
        time(&mut app, TimeOp::Freeze(Toggle::On));
        frames(&mut app, 1);
        assert_eq!(sim(&app).game.player().position.z, 5000.0);
        assert_eq!(sim(&app).curr_position, sim(&app).game.player().position);
        assert_eq!(sim(&app).prev_position, sim(&app).curr_position);
        assert!(!lab(&app).pending_snap);
        do_(
            &mut app,
            Command::Slot {
                op: SlotOp::Load { slot: 0 },
            },
        );
        frames(&mut app, 1);
        assert!(tick(&app) < later);
        assert_eq!(sim(&app).curr_position, sim(&app).game.player().position);
        assert_eq!(sim(&app).prev_position, sim(&app).curr_position);
    }

    #[test]
    fn the_inspector_takes_the_screen_and_freezes_only_while_open() {
        let mut app = session_on_graybox(None);
        // The mouse is captured, as in play.
        let window = app
            .world_mut()
            .spawn((
                PrimaryWindow,
                CursorOptions {
                    grab_mode: CursorGrabMode::Locked,
                    visible: false,
                    ..default()
                },
            ))
            .id();
        let grab = |app: &App| app.world().get::<CursorOptions>(window).unwrap().grab_mode;
        frames(&mut app, 2);
        let panel = |app: &App| app.world().resource::<PanelState>().clone();
        assert!(panel(&app).freeze_while_open);

        send(&mut app, LabRequest::OpenInspector);
        frames(&mut app, 1);
        assert!(panel(&app).open);
        assert_eq!(screen(&app), Screen::Sandbox);
        assert_eq!(grab(&app), CursorGrabMode::None, "the panel has the mouse");
        assert!(virt(&app).is_paused());
        let opened_at = tick(&app);
        assert_eq!(opened_at, 2, "no tick ran in the frame the panel opened");
        frames(&mut app, 5);
        assert_eq!(tick(&app), opened_at);
        // The freeze is the host's: the session's own time control was not
        // used, and the session is still pristine.
        let session = lab(&app).session.as_ref().unwrap();
        assert!(!session.time().frozen() && !session.time().was_used());
        assert!(session.is_pristine());

        // "Freeze while open" off: live tuning, time runs with the panel.
        app.world_mut()
            .resource_mut::<PanelState>()
            .freeze_while_open = false;
        frames(&mut app, 3);
        assert!(!virt(&app).is_paused());
        assert!(tick(&app) > opened_at);
        app.world_mut()
            .resource_mut::<PanelState>()
            .freeze_while_open = true;
        frames(&mut app, 1);
        assert!(virt(&app).is_paused());
        let frozen_at = tick(&app);

        // A single step works with the panel open, and then the freeze is
        // the session's: closing the panel leaves it frozen.
        time(&mut app, TimeOp::Step { n: 1 });
        frames(&mut app, 2);
        assert_eq!(tick(&app), frozen_at + 1);
        send(&mut app, LabRequest::CloseInspector);
        frames(&mut app, 1);
        assert!(!panel(&app).open);
        assert_eq!(screen(&app), Screen::None);
        assert_eq!(grab(&app), CursorGrabMode::Locked, "back in play");
        assert!(virt(&app).is_paused());
        time(&mut app, TimeOp::Freeze(Toggle::Off));
        frames(&mut app, 2);
        assert!(!virt(&app).is_paused());

        // Opened and closed without a step: time is as it was. Opened with
        // the mouse free (an unattended look): closing does not capture it.
        app.world_mut()
            .get_mut::<CursorOptions>(window)
            .unwrap()
            .grab_mode = CursorGrabMode::None;
        send(&mut app, LabRequest::OpenInspector);
        frames(&mut app, 2);
        assert!(virt(&app).is_paused());
        send(&mut app, LabRequest::CloseInspector);
        frames(&mut app, 1);
        assert!(!virt(&app).is_paused());
        assert_eq!(screen(&app), Screen::None);
        assert_eq!(grab(&app), CursorGrabMode::None);

        // The panel never opens over another screen, and it does not outlive
        // its own.
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Pause);
        send(&mut app, LabRequest::OpenInspector);
        frames(&mut app, 1);
        assert!(!panel(&app).open);
        assert_eq!(screen(&app), Screen::Pause);
        app.world_mut().resource_mut::<UiState>().close();
        send(&mut app, LabRequest::OpenInspector);
        frames(&mut app, 1);
        assert!(panel(&app).open);
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Loading);
        frames(&mut app, 1);
        assert!(!panel(&app).open);
        assert!(!virt(&app).is_paused());
    }

    #[test]
    fn fly_placement_moves_the_frozen_player_and_no_tick_runs() {
        let mut app = session_on_graybox(None);
        // The mouse is captured, as in play.
        app.world_mut().spawn((
            PrimaryWindow,
            CursorOptions {
                grab_mode: CursorGrabMode::Locked,
                ..default()
            },
        ));
        frames(&mut app, 30);
        do_(&mut app, Command::Fly { on: Toggle::On });
        frames(&mut app, 1);
        let session = |app: &App| lab(app).session.clone().unwrap();
        assert!(session(&app).flying());
        assert!(virt(&app).is_paused());
        let at = tick(&app);
        let start = sim(&app).game.player().position;

        // Forward and up for a few frames (real time advances one fixed
        // timestep per frame in this rig).
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::Space);
        }
        // Space was "pressed" for the game too: it must not become a jump.
        app.world_mut().resource_mut::<PendingInput>().jump = true;
        frames(&mut app, 10);
        let moved = sim(&app).game.player().position - start;
        let yaw = sim(&app).game.player().yaw;
        assert!(moved.dot(ue_forward_flat(yaw)) > 50.0, "{moved:?}");
        assert!(moved.z > 50.0, "{moved:?}");
        assert!(moved.length() < FLY_SPEED * 10.0 * FLY_MAX_FRAME + 1.0);
        assert_eq!(tick(&app), at, "fly placement runs no tick");
        assert!(session(&app).flying(), "and no tick ended it");
        assert_eq!(sim(&app).curr_position, sim(&app).game.player().position);
        assert_eq!(sim(&app).prev_position, sim(&app).curr_position);
        assert!(!app.world().resource::<PendingInput>().jump);

        // Leaving: time runs again (it ran before), from where the player
        // was put.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release_all();
        let placed = sim(&app).game.player().position;
        do_(&mut app, Command::Fly { on: Toggle::Off });
        frames(&mut app, 1);
        assert!(!session(&app).flying());
        assert!(!virt(&app).is_paused());
        assert_eq!(sim(&app).game.player().position, placed);
        frames(&mut app, 3);
        assert!(tick(&app) > at);
        // With a menu open the keys are the menu's: nothing moves.
        do_(&mut app, Command::Fly { on: Toggle::On });
        frames(&mut app, 1);
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Pause);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        let held = sim(&app).game.player().position;
        frames(&mut app, 5);
        assert_eq!(sim(&app).game.player().position, held);
    }

    #[test]
    fn recordings_go_to_the_sandbox_directory_and_are_never_parity_traces() {
        let root = TempRoot::new("recordings");
        let mut saves =
            asamu_game::save::SaveSession::open(asamu_game::save::SaveStore::new(root.saves()));
        saves.new_game().unwrap();
        drop(saves);
        let saves_before = tree(&root.saves());

        let mut app = session_on_graybox(None);
        app.world_mut().resource_mut::<Lab>().dirs = Some(root.dirs());
        do_(&mut app, Command::Record { on: Toggle::On });
        frames(&mut app, 12);
        assert!(sim(&app).game.is_recording());
        do_(&mut app, Command::Record { on: Toggle::Off });
        frames(&mut app, 1);
        assert!(!sim(&app).game.is_recording());
        let recordings = |root: &TempRoot| tree(&root.dirs().recordings());
        let files = recordings(&root);
        assert_eq!(files.len(), 1, "{:?}", files.keys());
        let (name, bytes) = files.iter().next().unwrap();
        assert!(name.starts_with("asamu-sandbox-") && name.ends_with(".sbxrec.jsonl"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        let recording = SandboxRecording::read_jsonl(text.as_bytes()).unwrap();
        assert!(recording.header.not_parity);
        assert_eq!(recording.header.param_set, "classic");
        assert!(recording.trace.samples.len() >= 10);
        assert!(asamu_player::Trace::from_jsonl_str(&text).is_err());
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("saved to") && n.contains("never a parity trace")),
            "{:?}",
            lab(&app).notice
        );

        // A recording still running when the session ends is written too.
        do_(
            &mut app,
            Command::SetParam {
                key: "movement.custom_gravity_scaling".to_owned(),
                value: TuneValue::Float(0.5),
            },
        );
        do_(&mut app, Command::Record { on: Toggle::On });
        frames(&mut app, 6);
        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        assert!(lab(&app).session.is_none());
        let files = recordings(&root);
        assert_eq!(files.len(), 2, "{:?}", files.keys());
        let tuned = files
            .values()
            .map(|bytes| SandboxRecording::read_jsonl(bytes.as_slice()).unwrap())
            .find(|r| r.header.param_set == "modified")
            .expect("the second recording ran a modified set");
        assert!(
            tuned
                .trace
                .meta
                .notes
                .iter()
                .all(|note| !note.contains("parameters: original"))
        );
        // Without a user data directory nothing is written anywhere, and
        // the notice says so.
        let mut app = session_on_graybox(None);
        do_(&mut app, Command::Record { on: Toggle::On });
        frames(&mut app, 3);
        do_(&mut app, Command::Record { on: Toggle::Off });
        frames(&mut app, 1);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("NOT saved"))
        );

        // The saves next door are byte-identical; everything new is under
        // the Sandbox's directory.
        assert_eq!(tree(&root.saves()), saves_before);
        let all = tree(&root.0);
        assert!(
            all.keys()
                .all(|name| name.starts_with("saves/") || name.starts_with("sandbox/recordings/")),
            "{:?}",
            all.keys()
        );
    }

    #[test]
    fn a_rule_holds_from_the_first_tick_and_ticks_stay_the_games() {
        // The rules are enforced before each tick, by the session, through
        // the game's own ability calls.
        let mut app = session_on_graybox(Some("infinite-grapple"));
        frames(&mut app, 3);
        let gun = sim(&app).game.player().script.gun;
        assert!(gun.max_grapples >= 32_767, "{}", gun.max_grapples);
        // No session system ticks the game: frames and ticks agree.
        assert_eq!(tick(&app), 3);
        assert!(sim_writes(&app) > 0);
    }
}
