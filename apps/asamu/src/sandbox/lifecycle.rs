//! Session lifecycle: what the Sandbox should do next.
//!
//! This module *decides*: it reads the command line ([`startup`]), the
//! main menu's button and the Sandbox's own requests ([`requests`]) and
//! watches the invariants that keep a session away from Classic state
//! ([`guard`]). It changes nothing itself. Every decision is queued as a
//! [`SimOp`] and carried out by `control::carry_out`, the single writer,
//! right after these systems in the same frame.
//!
//! # How Classic is protected
//!
//! - **Saves.** A session cannot start, and is ended at once, unless the
//!   save session of this run is in memory (`ui::Saves.0.store()` is
//!   `None`). A `--sandbox` process never opens the saves on disk, and the
//!   main menu's button is disabled on a disk-backed launch.
//! - **Classic menus.** A session ends as soon as the main menu, chapter
//!   select or time-trial select is open, before the menu acts on anything.
//!   In a `--sandbox` process those menus are not reachable at all: without
//!   a session the launcher is shown.
//! - **The simulation.** A session on a hand-made stage sets the Classic
//!   graybox game aside and puts it back, untouched, when it ends; a session
//!   on converted data loads its map through the unchanged menu flow, as a
//!   story level, and leaves no simulation behind.
//!
//! One limit worth knowing: in a run that is *not* a `--sandbox` process
//! but keeps its saves in memory anyway (`--level`, or no user data
//! directory), a session on a converted map shares that in-memory save
//! session with Classic play of the same run. Nothing reaches the disk.

use asamu_game::asamu_world::graybox_test_level;
use asamu_sandbox::arena::{build_arena, builtin_arenas};
use asamu_sandbox::session::Session;
use bevy::prelude::*;

use super::control::{self, ON_DISK_SAVES, SimOp, SimOps};
use super::{Lab, LabLaunch, LabRequest, Phase, Stage, storage};
use crate::Sim;
use crate::ui::{self, Screen, UiAction, UiLaunch, UiState};

/// Frames a loading session may go without any sign of its level (no load
/// running, no loading screen, no simulation) before it is given up. The
/// frame in which a load hands over its game looks like that for a moment.
const STALLED_FRAMES: u8 = 3;

pub(super) fn build(app: &mut App) {
    app.add_systems(PostStartup, startup).add_systems(
        Update,
        (guard, requests, control::carry_out.run_if(control::has_ops))
            .chain()
            .before(ui::UiFlowSet),
    );
}

/// In a `--sandbox` process: opens the launcher, or starts straight on the
/// stage the command line named (`--arena`, `--level`, `--no-menu`).
fn startup(
    lab: Res<Lab>,
    launch: Res<LabLaunch>,
    ui_launch: Res<UiLaunch>,
    mut requests: MessageWriter<LabRequest>,
    mut ops: ResMut<SimOps>,
) {
    if !lab.from_cli {
        return;
    }
    let profile = launch.args.profile.clone();
    if let Some(id) = &launch.args.arena {
        requests.write(LabRequest::Start {
            stage: Stage::Arena(id.clone()),
            profile,
        });
    } else if ui_launch.converted.is_some() {
        match &launch.level {
            // `main.rs` is loading this level already: the session waits
            // for it. A profile that does not load cannot hold the level
            // back, so the session then runs the Classic set and says so.
            Some(level) => {
                let session = match session_for(&lab, profile.as_deref()) {
                    Ok(session) => session,
                    Err(why) => {
                        warn!("sandbox: {why}; this session runs the Classic set");
                        ops.push(SimOp::Notice(format!(
                            "{why}; this session runs the Classic set"
                        )));
                        Session::classic()
                    }
                };
                ops.push(SimOp::AwaitLevel {
                    session: Box::new(session),
                    map: level.clone(),
                });
            }
            None => {
                requests.write(LabRequest::OpenLauncher);
            }
        }
    } else if launch.no_menu || !ui_launch.show_menu {
        requests.write(LabRequest::Start {
            stage: Stage::Graybox,
            profile,
        });
    } else {
        requests.write(LabRequest::OpenLauncher);
    }
}

/// The session for the profile named `profile` (`None`: Classic).
fn session_for(lab: &Lab, profile: Option<&str>) -> Result<Session, String> {
    match profile {
        None => Ok(Session::classic()),
        Some(name) => {
            let profile = storage::load_profile(lab, name)?;
            Session::new(profile).map_err(|e| format!("profile {name:?}: {e}"))
        }
    }
}

/// Decides how a session on `stage` starts; an `Err` is the sentence that
/// says why it does not.
fn plan_start(
    stage: &Stage,
    profile: Option<&str>,
    lab: &Lab,
    ui_launch: &UiLaunch,
    saves: &ui::Saves,
    sim: Option<&Sim>,
) -> Result<SimOp, String> {
    if saves.0.store().is_some() {
        return Err(ON_DISK_SAVES.to_owned());
    }
    let session = session_for(lab, profile)?;
    let level = match stage {
        Stage::Map(name) => {
            if ui_launch.converted.is_none() {
                return Err(format!(
                    "{name} is a converted map: start with --converted DIR to play it"
                ));
            }
            let Some(map) = ui_launch.level_name(name) else {
                return Err(format!(
                    "{name} is not among the converted maps of this launch"
                ));
            };
            return Ok(SimOp::StartMap {
                session: Box::new(session),
                map: map.to_owned(),
            });
        }
        Stage::Graybox => graybox_test_level(),
        Stage::Arena(id) => build_arena(id).ok_or_else(|| {
            let known: Vec<&str> = builtin_arenas().iter().map(|a| a.id).collect();
            format!("there is no arena {id:?} (built in: {})", known.join(", "))
        })?,
    };
    if ui_launch.converted.is_some() {
        return Err("hand-made stages are not available in a run on converted data".to_owned());
    }
    // The Classic graybox game: the one a running session set aside, else
    // the one in the simulation. The stage keeps its movement model.
    let Some(classic) = lab
        .classic
        .as_ref()
        .map(|keep| &keep.game)
        .or(sim.map(|sim| &sim.game))
    else {
        return Err("there is no simulation to start a hand-made stage on".to_owned());
    };
    if classic.params().pawn.is_none() {
        return Err(
            "the Sandbox tunes the Classic parameter set: this run uses the placeholder \
             parameters"
                .to_owned(),
        );
    }
    let game = session
        .new_game(level)
        .map_err(|e| format!("the stage could not be built: {e}"))?
        .with_movement_model(classic.movement_model());
    Ok(SimOp::StartHandMade {
        session: Box::new(session),
        game: Box::new(game),
        stage: stage.clone(),
        // A start asked for by the command line arrives before the Sandbox
        // has shown anything; one from the launcher or the panel is a click.
        capture: lab.phase != Phase::Idle,
    })
}

/// The main menu's "Sandbox" button and the Sandbox's own lifecycle
/// requests (`OpenLauncher`, `Start`, `End`, `Quit`, `SaveProfile`).
#[allow(clippy::too_many_arguments)]
fn requests(
    mut actions: MessageReader<UiAction>,
    mut requests: MessageReader<LabRequest>,
    lab: Res<Lab>,
    ui_launch: Res<UiLaunch>,
    saves: Res<ui::Saves>,
    sim: Option<Res<Sim>>,
    mut ops: ResMut<SimOps>,
) {
    for action in actions.read() {
        if matches!(action, UiAction::OpenSandbox) {
            ops.push(SimOp::OpenLauncher);
        }
    }
    for request in requests.read() {
        match request {
            LabRequest::OpenLauncher => ops.push(SimOp::OpenLauncher),
            LabRequest::Start { stage, profile } => {
                match plan_start(
                    stage,
                    profile.as_deref(),
                    &lab,
                    &ui_launch,
                    &saves,
                    sim.as_deref(),
                ) {
                    Ok(op) => ops.push(op),
                    Err(why) => {
                        warn!("sandbox: the session did not start: {why}");
                        ops.push(SimOp::Notice(format!("The session did not start: {why}")));
                    }
                }
            }
            LabRequest::End => ops.push(SimOp::End { notice: None }),
            LabRequest::Quit => ops.push(SimOp::Quit),
            LabRequest::SaveProfile { name } => {
                let line = match &lab.session {
                    None => "there is no session whose tuning could be saved".to_owned(),
                    Some(session) => match storage::save_profile(&lab, &session.to_profile(name)) {
                        Ok(path) => format!("profile {name} saved to {}", path.display()),
                        Err(why) => format!("the profile was not saved: {why}"),
                    },
                };
                ops.push(SimOp::Notice(line));
            }
            // `control::apply` handles these.
            LabRequest::Do(_) | LabRequest::OpenInspector | LabRequest::CloseInspector => {}
        }
    }
}

/// The invariants, checked every frame before the menu flow acts.
fn guard(
    lab: Res<Lab>,
    state: Res<UiState>,
    saves: Res<ui::Saves>,
    sim: Option<Res<Sim>>,
    game_load: Option<Res<crate::GameLoad>>,
    mut ops: ResMut<SimOps>,
    mut stalled: Local<u8>,
) {
    // Steps already queued (by `startup`, or by a tick) change the state
    // this would judge: they go first.
    if has_queued(&ops) {
        return;
    }
    let on_disk = saves.0.store().is_some();
    // 1. Never next to saves on disk.
    if on_disk && (lab.session.is_some() || (lab.phase == Phase::Launcher && !lab.from_cli)) {
        error!("sandbox: the saves of this run are on disk; the Sandbox closes");
        ops.push(SimOp::End {
            notice: Some(ON_DISK_SAVES.to_owned()),
        });
        return;
    }
    let loading = lab.phase == Phase::Loading;
    match lab.phase {
        Phase::Active => {
            if matches!(
                state.screen,
                Screen::Main | Screen::Chapters | Screen::TimeTrial
            ) {
                // 2. A Classic menu is open: the session is over before the
                // menu acts on anything.
                ops.push(SimOp::End { notice: None });
            } else if sim.is_none() {
                // The level's Kismet opened another map: wait for it.
                ops.push(SimOp::AwaitNext);
            }
        }
        Phase::Loading => {
            // 4. The level never arrives: back out with a word on why.
            let direct = game_load
                .as_ref()
                .is_some_and(|load| load.request.is_some() || load.task.is_some());
            let failed = if sim.is_some() || state.screen == Screen::Loading || direct {
                *stalled = 0;
                false
            } else if state.screen == Screen::Main {
                // The menu flow reports a failed or cancelled load there.
                true
            } else {
                *stalled = stalled.saturating_add(1);
                *stalled >= STALLED_FRAMES
            };
            if failed {
                ops.push(SimOp::End {
                    notice: Some("The session's level did not load (the log says why).".to_owned()),
                });
            }
        }
        Phase::Launcher => {
            if state.screen != Screen::Sandbox {
                // The launcher lost its screen.
                ops.push(if lab.from_cli {
                    SimOp::OpenLauncher
                } else {
                    SimOp::End { notice: None }
                });
            }
        }
        Phase::Idle => {
            // 3. A `--sandbox` process has no Classic menus: without a
            // session it shows the launcher.
            if lab.from_cli && !on_disk {
                ops.push(SimOp::OpenLauncher);
            }
        }
    }
    if !loading {
        *stalled = 0;
    }
}

fn has_queued(ops: &SimOps) -> bool {
    !ops.is_empty()
}

#[cfg(test)]
mod tests {
    use asamu_assets::ConvertedDir;
    use asamu_game::save::{PlayMode, SaveSession, SaveStore};
    use asamu_game::{Game, GameState};
    use asamu_player::PlayerParams;
    use asamu_sandbox::arena::{MOVEMENT_LAB, arena_level_name};
    use asamu_sandbox::command::{Command, TimeOp, Toggle};
    use asamu_sandbox::overlay::{ParamSetLabel, param_set_label};

    use super::super::control::testing::{
        Rig, classic_banner, flow_requests, frames, lab, rig, send, sim, sim_writes,
    };
    use super::super::storage::testing::{TempRoot, tree};
    use super::super::{PanelState, SandboxArgs};
    use super::*;
    use crate::ui::{FlowRequest, Play};

    fn start(stage: Stage, profile: Option<&str>) -> LabRequest {
        LabRequest::Start {
            stage,
            profile: profile.map(str::to_owned),
        }
    }

    fn screen(app: &App) -> Screen {
        app.world().resource::<UiState>().screen
    }

    fn converted_launch(levels: &[&str]) -> UiLaunch {
        UiLaunch {
            converted: Some(ConvertedDir::new(std::path::PathBuf::from("conv"))),
            levels: levels.iter().map(|l| (*l).to_owned()).collect(),
            show_menu: true,
            saves: false,
        }
    }

    #[test]
    fn a_session_cannot_start_on_a_disk_backed_save_session() {
        let root = TempRoot::new("disk-saves");
        let mut saves = SaveSession::open(SaveStore::new(root.saves()));
        saves.new_game().unwrap();
        let before = tree(&root.saves());
        assert!(!before.is_empty(), "the save files exist");

        let mut app = rig(Rig::default());
        app.world_mut().resource_mut::<Lab>().dirs = Some(root.dirs());
        app.insert_resource(ui::Saves(saves));
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 2);

        // The menu button (disabled on such a launch, so this is a stray
        // message), the launcher and every kind of start are refused.
        app.world_mut().write_message(UiAction::OpenSandbox);
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert_eq!(screen(&app), Screen::Main);
        for request in [
            LabRequest::OpenLauncher,
            start(Stage::Graybox, None),
            start(Stage::Arena(MOVEMENT_LAB.to_owned()), Some("moon")),
            start(Stage::Map("AG-Workshop".to_owned()), None),
        ] {
            send(&mut app, request);
            frames(&mut app, 2);
            let lab = lab(&app);
            assert_eq!(lab.phase, Phase::Idle);
            assert!(lab.session.is_none() && lab.stage.is_none());
            assert!(
                lab.notice
                    .as_deref()
                    .is_some_and(|n| n.contains("--sandbox")),
                "{:?}",
                lab.notice
            );
        }
        // Classic's simulation was never written, no load was requested ...
        assert_eq!(sim_writes(&app), 0);
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert!(flow_requests(&app).is_empty());
        // ... and the files are byte-identical; nothing else appeared.
        assert_eq!(tree(&root.saves()), before);
        assert!(!root.dirs().root.exists());

        // A session that finds itself next to saves on disk ends at once.
        let mut app = rig(Rig::default());
        send(&mut app, start(Stage::Graybox, None));
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Active);
        app.insert_resource(ui::Saves(SaveSession::open(SaveStore::new(root.saves()))));
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert!(lab(&app).session.is_none());
        assert_eq!(screen(&app), Screen::Main);
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert_eq!(tree(&root.saves()), before);
    }

    #[test]
    fn ending_a_session_restores_the_classic_graybox_and_virtual_time() {
        let mut app = rig(Rig::default());
        // A Classic graybox game in progress: played for a few ticks, then
        // paused behind the main menu.
        app.world_mut().resource_mut::<Sim>().game.start();
        frames(&mut app, 6);
        let classic_tick = sim(&app).game.clock().tick();
        assert!(classic_tick >= 4, "{classic_tick}");
        app.world_mut().resource_mut::<Sim>().game.pause();
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        let classic_player = format!("{:?}", sim(&app).game.player());
        let classic_banner = classic_banner();
        assert_eq!(sim(&app).banner, classic_banner);

        // A tuned session on an arena, with time control in use.
        send(
            &mut app,
            start(Stage::Arena(MOVEMENT_LAB.to_owned()), Some("moon")),
        );
        frames(&mut app, 4);
        {
            let lab = lab(&app);
            assert_eq!(lab.phase, Phase::Active);
            assert_eq!(lab.stage, Some(Stage::Arena(MOVEMENT_LAB.to_owned())));
            let sim = sim(&app);
            assert_eq!(sim.game.level().name, arena_level_name(MOVEMENT_LAB));
            assert_eq!(sim.game.state(), GameState::Playing);
            assert!(matches!(
                param_set_label(sim.game.params()),
                ParamSetLabel::Modified { .. }
            ));
            assert!(sim.banner.contains("SANDBOX") && sim.banner.contains("MODIFIED"));
            assert!(!sim.banner.contains("ORIGINAL values"));
            assert_eq!(screen(&app), Screen::None);
        }
        for op in [TimeOp::Scale { value: 4.0 }, TimeOp::Freeze(Toggle::On)] {
            send(&mut app, LabRequest::Do(Command::Time { op }));
        }
        frames(&mut app, 2);
        {
            let virt = app.world().resource::<Time<Virtual>>();
            assert!(virt.is_paused());
            assert_eq!(virt.relative_speed(), 4.0);
        }

        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        let lab = lab(&app);
        assert_eq!(lab.phase, Phase::Idle);
        assert!(lab.session.is_none() && lab.stage.is_none() && lab.classic.is_none());
        assert_eq!(screen(&app), Screen::Main);
        assert!(!app.world().resource::<PanelState>().open);
        // The Classic game is back exactly as it was left ...
        let sim = sim(&app);
        assert_eq!(*sim.game.params(), PlayerParams::asamu_original());
        assert_eq!(param_set_label(sim.game.params()), ParamSetLabel::Classic);
        assert_eq!(sim.game.level(), Game::graybox().unwrap().level());
        assert_eq!(sim.game.clock().tick(), classic_tick);
        assert_eq!(sim.game.state(), GameState::Paused);
        assert_eq!(format!("{:?}", sim.game.player()), classic_player);
        assert_eq!(sim.banner, classic_banner);
        assert!(sim.script.is_none());
        assert_eq!(sim.prev_position, sim.game.player().position);
        assert_eq!(sim.curr_position, sim.game.player().position);
        // ... and so is the virtual clock.
        let virt = app.world().resource::<Time<Virtual>>();
        assert!(!virt.is_paused());
        assert_eq!(virt.relative_speed(), 1.0);
        assert_eq!(*app.world().resource::<Play>(), Play::default());

        // It stays that way, and the paused Classic game does not move.
        frames(&mut app, 3);
        assert_eq!(self::sim(&app).game.clock().tick(), classic_tick);
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    }

    #[test]
    fn an_idle_lab_never_changes_the_sim() {
        let mut app = rig(Rig::default());
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 3);
        // Requests that only mean something to a session, and an `End` with
        // nothing to end.
        for request in [
            LabRequest::Do(Command::Respawn),
            LabRequest::Do(Command::Time {
                op: TimeOp::Freeze(Toggle::On),
            }),
            LabRequest::OpenInspector,
            LabRequest::CloseInspector,
            LabRequest::SaveProfile {
                name: "nothing".to_owned(),
            },
            LabRequest::End,
        ] {
            send(&mut app, request);
            frames(&mut app, 2);
        }
        assert_eq!(sim_writes(&app), 0, "the simulation was written while idle");
        let lab = lab(&app);
        assert_eq!(lab.phase, Phase::Idle);
        assert!(lab.session.is_none() && lab.classic.is_none() && !lab.pending_snap);
        assert_eq!(screen(&app), Screen::Main);
        assert!(!app.world().resource::<PanelState>().open);
        let virt = app.world().resource::<Time<Virtual>>();
        assert!(!virt.is_paused());
        assert_eq!(virt.relative_speed(), 1.0);
        assert_eq!(sim(&app).game.state(), GameState::Boot);
        assert_eq!(sim(&app).banner, classic_banner());
        assert!(flow_requests(&app).is_empty());
    }

    #[test]
    fn the_menu_button_opens_the_launcher_and_end_closes_it() {
        let mut app = rig(Rig::default());
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 2);
        app.world_mut().write_message(UiAction::OpenSandbox);
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);
        assert!(lab(&app).session.is_none());
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Launcher);

        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert_eq!(screen(&app), Screen::Main);
        frames(&mut app, 2);
        assert_eq!(screen(&app), Screen::Main);
        assert_eq!(
            sim_writes(&app),
            0,
            "the launcher alone writes no simulation"
        );
    }

    #[test]
    fn a_sandbox_process_shows_the_launcher_and_never_the_classic_menus() {
        let mut app = rig(Rig {
            from_cli: true,
            ..Rig::default()
        });
        // `ui::setup` opens the main menu at start-up.
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);

        // `End` at the launcher: there is nothing to go back to.
        send(&mut app, LabRequest::End);
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);

        // A session, then the pause menu's "Main menu": the session ends
        // and the launcher is back before the menu can act.
        send(&mut app, start(Stage::Graybox, None));
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(screen(&app), Screen::None);
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);
        assert!(lab(&app).session.is_none());
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert_eq!(sim(&app).game.state(), GameState::Boot);
        // Chapter select and time-trial select end a session too.
        for classic_menu in [Screen::Chapters, Screen::TimeTrial] {
            send(&mut app, start(Stage::Graybox, None));
            frames(&mut app, 2);
            assert_eq!(lab(&app).phase, Phase::Active);
            app.world_mut().resource_mut::<UiState>().open(classic_menu);
            frames(&mut app, 1);
            assert_eq!(lab(&app).phase, Phase::Launcher, "{classic_menu:?}");
            assert_eq!(screen(&app), Screen::Sandbox);
        }
    }

    /// `Quit` (the button of a `--sandbox` process's launcher): a running
    /// session ends first, Classic gets its game back, and the menu flow is
    /// asked to quit as the main menu's own button asks it.
    #[test]
    fn quit_ends_the_session_and_asks_the_menu_flow_to_quit() {
        // From the launcher: nothing to end, the flow is asked once.
        let mut app = rig(Rig {
            from_cli: true,
            ..Rig::default()
        });
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        send(&mut app, LabRequest::Quit);
        frames(&mut app, 1);
        assert_eq!(flow_requests(&app), vec![FlowRequest::Quit]);
        assert_eq!(sim_writes(&app), 0, "quitting the launcher writes no game");

        // From a tuned, frozen session.
        let mut app = rig(Rig {
            from_cli: true,
            ..Rig::default()
        });
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        send(&mut app, start(Stage::Graybox, Some("moon")));
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_ne!(*sim(&app).game.params(), PlayerParams::asamu_original());
        send(
            &mut app,
            LabRequest::Do(Command::Time {
                op: TimeOp::Freeze(Toggle::On),
            }),
        );
        frames(&mut app, 2);
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
        send(&mut app, LabRequest::Quit);
        frames(&mut app, 1);
        assert!(lab(&app).session.is_none());
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert_eq!(sim(&app).banner, classic_banner());
        let virt = app.world().resource::<Time<Virtual>>();
        assert!(!virt.is_paused());
        assert_eq!(virt.relative_speed(), 1.0);
        assert_eq!(flow_requests(&app), vec![FlowRequest::Quit]);
    }

    #[test]
    fn a_sandbox_process_starts_on_the_stage_the_command_line_names() {
        // `--sandbox --arena movement-lab --sandbox-profile heavy`.
        let mut app = rig(Rig {
            from_cli: true,
            args: SandboxArgs {
                enabled: true,
                arena: Some(MOVEMENT_LAB.to_owned()),
                profile: Some("heavy".to_owned()),
            },
            ..Rig::default()
        });
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(lab(&app).stage, Some(Stage::Arena(MOVEMENT_LAB.to_owned())));
        assert_eq!(
            lab(&app)
                .session
                .as_ref()
                .map(|s| s.profile().name.as_str()),
            Some("heavy")
        );
        assert_eq!(sim(&app).game.level().name, arena_level_name(MOVEMENT_LAB));
        assert_eq!(screen(&app), Screen::None);

        // `--sandbox --no-menu`: straight onto the graybox, Classic set.
        let mut app = rig(Rig {
            from_cli: true,
            no_menu: true,
            ..Rig::default()
        });
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(lab(&app).stage, Some(Stage::Graybox));
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());
        assert!(sim(&app).banner.contains("SANDBOX"));
        assert!(lab(&app).session.as_ref().is_some_and(Session::is_pristine));

        // An unattended run (`--sandbox --screenshot ...` shows no menu):
        // straight onto the graybox too.
        let mut app = rig(Rig {
            from_cli: true,
            launch: UiLaunch {
                show_menu: false,
                ..UiLaunch::default()
            },
            ..Rig::default()
        });
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(lab(&app).stage, Some(Stage::Graybox));

        // A profile that does not exist: no session, the launcher says why.
        let mut app = rig(Rig {
            from_cli: true,
            args: SandboxArgs {
                enabled: true,
                arena: Some(MOVEMENT_LAB.to_owned()),
                profile: Some("no-such-profile".to_owned()),
            },
            ..Rig::default()
        });
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("no-such-profile")),
            "{:?}",
            lab(&app).notice
        );
        assert_eq!(sim_writes(&app), 0);
    }

    #[test]
    fn a_start_from_the_launcher_captures_the_mouse_and_one_from_the_command_line_does_not() {
        use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
        let grab = |app: &mut App| {
            let world = app.world_mut();
            world
                .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
                .single(world)
                .map(|c| (c.grab_mode, c.visible))
                .unwrap()
        };
        let free = (CursorGrabMode::None, true);

        // `--sandbox --arena ...`: the window has just opened. The session
        // runs, and a click captures the mouse as in any run without a menu.
        let mut app = rig(Rig {
            from_cli: true,
            args: SandboxArgs {
                enabled: true,
                arena: Some(MOVEMENT_LAB.to_owned()),
                profile: None,
            },
            ..Rig::default()
        });
        app.world_mut()
            .spawn((PrimaryWindow, CursorOptions::default()));
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(sim(&app).game.state(), GameState::Playing);
        assert_eq!(grab(&mut app), free);

        // From the launcher: a click on a stage plays on at once.
        let mut app = rig(Rig {
            from_cli: true,
            ..Rig::default()
        });
        app.world_mut()
            .spawn((PrimaryWindow, CursorOptions::default()));
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(grab(&mut app), free);
        send(&mut app, start(Stage::Graybox, None));
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(grab(&mut app), (CursorGrabMode::Locked, false));
        // The end frees it again for the launcher.
        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        assert_eq!(grab(&mut app), free);
    }

    #[test]
    fn a_start_that_cannot_work_is_refused_and_changes_nothing() {
        let mut app = rig(Rig::default());
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        app.world_mut().write_message(UiAction::OpenSandbox);
        frames(&mut app, 2);
        for (request, says) in [
            (start(Stage::Arena("nowhere".to_owned()), None), "nowhere"),
            (
                start(Stage::Graybox, Some("no-such-profile")),
                "no-such-profile",
            ),
            (start(Stage::Graybox, Some("Not A Name")), "Not A Name"),
            (
                start(Stage::Map("AG-Workshop".to_owned()), None),
                "--converted",
            ),
        ] {
            send(&mut app, request);
            frames(&mut app, 2);
            let lab = lab(&app);
            assert_eq!(lab.phase, Phase::Launcher, "{says}");
            assert!(lab.session.is_none());
            assert!(
                lab.notice.as_deref().is_some_and(|n| n.contains(says)),
                "{:?}",
                lab.notice
            );
            assert_eq!(screen(&app), Screen::Sandbox);
        }
        assert_eq!(sim_writes(&app), 0);
        assert!(flow_requests(&app).is_empty());

        // The placeholder configuration has no Classic set to tune.
        let mut app = rig(Rig {
            placeholder: true,
            ..Rig::default()
        });
        send(&mut app, start(Stage::Graybox, None));
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("placeholder"))
        );
        assert_eq!(sim_writes(&app), 0);

        // Hand-made stages belong to the graybox composition.
        let mut app = rig(Rig {
            launch: converted_launch(&["AG-Workshop"]),
            without_sim: true,
            ..Rig::default()
        });
        send(&mut app, start(Stage::Graybox, None));
        send(&mut app, start(Stage::Map("AG-Nowhere".to_owned()), None));
        frames(&mut app, 2);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert!(flow_requests(&app).is_empty());
    }

    #[test]
    fn a_session_on_a_converted_map_loads_through_the_menu_flow_and_tunes_before_the_first_tick() {
        let mut app = rig(Rig {
            from_cli: true,
            launch: converted_launch(&["AG-Workshop", "AG-IceCave"]),
            without_sim: true,
            ..Rig::default()
        });
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);

        // The launcher starts a map: a story load is requested, nothing else.
        send(
            &mut app,
            start(Stage::Map("ag-workshop".to_owned()), Some("moon")),
        );
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Loading);
        assert_eq!(lab(&app).stage, Some(Stage::Map("AG-Workshop".to_owned())));
        assert_eq!(
            flow_requests(&app),
            [FlowRequest::Load {
                map: "AG-Workshop".to_owned(),
                mode: PlayMode::Story
            }]
        );
        // What the menu flow does next: the loading screen, later the game.
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Loading);
        frames(&mut app, 5);
        assert_eq!(lab(&app).phase, Phase::Loading);

        let mut loaded = Game::graybox().unwrap();
        loaded.start();
        let banner = crate::banner(&loaded);
        app.insert_resource(Sim::new(loaded, banner));
        app.world_mut().resource_mut::<UiState>().close();
        frames(&mut app, 1);
        // Adopted before its first tick: the session's set, the session's
        // banner, and the tick that then ran used them.
        let session_params = lab(&app).session.as_ref().unwrap().params().clone();
        assert_ne!(session_params, PlayerParams::asamu_original());
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(*sim(&app).game.params(), session_params);
        assert!(sim(&app).banner.contains("SANDBOX"));
        assert!(sim(&app).game.clock().tick() <= 1);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.starts_with("Sandbox session on") && n.contains("moon")),
            "{:?}",
            lab(&app).notice
        );

        // The level's Kismet opens the next map: the simulation goes, the
        // session waits, and the next game is tuned the same way.
        app.world_mut().remove_resource::<Sim>();
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Loading);
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Loading);
        assert!(lab(&app).session.is_some());
        let next = Game::graybox().unwrap();
        let banner = crate::banner(&next);
        app.insert_resource(Sim::new(next, banner));
        app.world_mut().resource_mut::<UiState>().close();
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert_eq!(*sim(&app).game.params(), session_params);

        // The end leaves no simulation behind, and the launcher is back.
        send(&mut app, LabRequest::End);
        frames(&mut app, 1);
        assert!(app.world().get_resource::<Sim>().is_none());
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(screen(&app), Screen::Sandbox);
        assert_eq!(*app.world().resource::<Play>(), Play::default());

        // A load that fails: the menu flow reports on the main menu, and
        // the Sandbox goes back to its launcher with a word on it.
        send(&mut app, start(Stage::Map("AG-IceCave".to_owned()), None));
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Loading);
        app.world_mut().resource_mut::<UiState>().open(Screen::Main);
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert!(lab(&app).session.is_none());
        assert_eq!(screen(&app), Screen::Sandbox);
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("did not load"))
        );
    }

    #[test]
    fn a_level_given_on_the_command_line_gets_its_session_or_an_honest_end() {
        // `--sandbox --level AG-Workshop`: `main.rs` loads the level.
        let cli = || Rig {
            from_cli: true,
            args: SandboxArgs {
                enabled: true,
                arena: None,
                profile: Some("no-such-profile".to_owned()),
            },
            level: Some("AG-Workshop".to_owned()),
            launch: UiLaunch {
                show_menu: false,
                ..converted_launch(&["AG-Workshop"])
            },
            without_sim: true,
            ..Rig::default()
        };
        let mut app = rig(cli());
        app.insert_resource(crate::GameLoad {
            request: Some((std::path::PathBuf::from("conv"), "AG-Workshop".to_owned())),
            task: None,
        });
        frames(&mut app, 4);
        // Waiting, with the Classic set because the profile does not exist.
        assert_eq!(lab(&app).phase, Phase::Loading);
        assert!(flow_requests(&app).is_empty(), "no second load");
        assert!(
            lab(&app)
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("no-such-profile") && n.contains("Classic set"))
        );
        let level = Game::graybox().unwrap();
        let banner = crate::banner(&level);
        app.insert_resource(Sim::new(level, banner));
        app.world_mut().resource_mut::<crate::GameLoad>().request = None;
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert!(sim(&app).banner.contains("SANDBOX"));
        assert_eq!(*sim(&app).game.params(), PlayerParams::asamu_original());

        // The same start, but the level never loads.
        let mut app = rig(cli());
        app.insert_resource(crate::GameLoad {
            request: None,
            task: None,
        });
        frames(&mut app, 1);
        assert_eq!(lab(&app).phase, Phase::Loading);
        frames(&mut app, usize::from(STALLED_FRAMES) + 1);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert!(lab(&app).session.is_none());
        assert_eq!(screen(&app), Screen::Sandbox);
    }

    // -----------------------------------------------------------------
    // A Classic process with the whole plugin registered, against one that
    // has no trace of the Sandbox.
    // -----------------------------------------------------------------

    /// The game's own input chain, registered exactly as `main.rs` registers
    /// it (`add_simulation_systems`), and a window with the mouse captured.
    fn add_classic_input(app: &mut App) {
        use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

        app.init_resource::<crate::LevelGizmos>()
            .init_resource::<bevy::input::mouse::AccumulatedMouseMotion>()
            .init_resource::<ui::UserSettings>()
            .add_systems(
                RunFixedMainLoop,
                (crate::cursor_and_pause, crate::gather_input)
                    .chain()
                    .run_if(resource_exists::<Sim>.and_then(ui::gameplay_input_enabled))
                    .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
            );
        app.world_mut().spawn((
            PrimaryWindow,
            CursorOptions {
                grab_mode: CursorGrabMode::Locked,
                visible: false,
                ..default()
            },
        ));
    }

    /// Classic alone: the real tick, the real input chain and Bevy's clocks
    /// (one fixed timestep per frame). No Sandbox resource, system or set.
    fn classic_alone() -> App {
        use bevy::time::{TimePlugin, TimeUpdateStrategy};

        let game = Game::graybox().unwrap();
        let fixed = Time::<Fixed>::from_hz(game.clock().tick_rate_hz());
        let step = fixed.timestep();
        let banner = crate::banner(&game);
        let mut app = App::new();
        app.add_plugins(TimePlugin)
            .insert_resource(fixed)
            .insert_resource(TimeUpdateStrategy::ManualDuration(step))
            .init_resource::<crate::PendingInput>()
            .init_resource::<crate::ScriptedWalk>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<UiState>()
            .add_message::<ui::GameTick>()
            .add_message::<crate::kismet::KismetFrame>()
            .insert_resource(Sim::new(game, banner))
            .add_systems(
                FixedUpdate,
                crate::fixed_tick.run_if(resource_exists::<Sim>),
            );
        add_classic_input(&mut app);
        app
    }

    /// The same Classic app with the Sandbox registered as the plugin
    /// registers it: the runtime and every part of the view except the gizmo
    /// visualisers (which need a renderer; they only draw, under the same
    /// run condition as the rest).
    fn classic_with_the_plugin() -> App {
        use super::super::{VizSettings, arena_view, graphs, hud, input, launcher, panel, widgets};

        let mut app = rig(Rig::default());
        app.init_resource::<VizSettings>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>();
        input::build(&mut app);
        panel::build(&mut app);
        widgets::build(&mut app);
        launcher::build(&mut app);
        hud::build(&mut app);
        graphs::build(&mut app);
        arena_view::build(&mut app);
        add_classic_input(&mut app);
        app
    }

    /// What one frame of the key script left behind.
    #[derive(Debug, PartialEq)]
    struct Frame {
        /// The whole player state, as text (every float in its exact
        /// shortest form).
        player: String,
        tick: u64,
        /// The key tapped in this frame is still "just pressed" after the
        /// frame: nothing took it away from the game.
        tap_left: Option<bool>,
        mouse_captured: bool,
        gizmos: bool,
    }

    /// Plays a fixed script of key presses, one frame at a time: forward and
    /// sprint held, mouse look, and a tap every few frames that walks
    /// through every key the Sandbox reads in a session (Classic's own debug
    /// keys among them), the game's keys, one F9 (Classic's trace recording
    /// starts) and one pause and click-to-resume.
    fn play_the_key_script(app: &mut App, frames: usize) -> Vec<Frame> {
        use bevy::input::mouse::AccumulatedMouseMotion;
        use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

        const TAPS: [KeyCode; 36] = [
            KeyCode::Backquote,
            KeyCode::F5,
            KeyCode::BracketLeft,
            KeyCode::BracketRight,
            KeyCode::Minus,
            KeyCode::Equal,
            KeyCode::Backspace,
            KeyCode::KeyP,
            KeyCode::KeyO,
            KeyCode::Comma,
            KeyCode::Period,
            KeyCode::Slash,
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::KeyK,
            KeyCode::KeyL,
            KeyCode::KeyZ,
            KeyCode::KeyT,
            KeyCode::KeyG,
            KeyCode::KeyB,
            KeyCode::KeyV,
            KeyCode::KeyN,
            KeyCode::KeyH,
            KeyCode::Space,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F6,
            KeyCode::Space,
            KeyCode::F7,
            KeyCode::F10,
            KeyCode::KeyE,
            KeyCode::Space,
            KeyCode::KeyR,
        ];
        /// F9, once: Classic's handler starts its trace recording. (A second
        /// press would write the trace to a file.)
        const RECORD_FRAME: usize = 3;
        /// Esc pauses the game and frees the mouse; a click resumes.
        const PAUSE_FRAME: usize = 401;
        const CLICK_FRAME: usize = 411;

        app.world_mut().resource_mut::<Sim>().game.start();
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::ShiftLeft);
        }
        let mut out = Vec::with_capacity(frames);
        for frame in 0..frames {
            let tap = match frame {
                RECORD_FRAME => Some(KeyCode::F9),
                PAUSE_FRAME => Some(KeyCode::Escape),
                _ if frame.is_multiple_of(5) => Some(TAPS[(frame / 5) % TAPS.len()]),
                _ => None,
            };
            if let Some(code) = tap {
                app.world_mut()
                    .resource_mut::<ButtonInput<KeyCode>>()
                    .press(code);
            }
            if frame == CLICK_FRAME {
                app.world_mut()
                    .resource_mut::<ButtonInput<MouseButton>>()
                    .press(MouseButton::Left);
            }
            app.world_mut()
                .resource_mut::<AccumulatedMouseMotion>()
                .delta = if frame.is_multiple_of(7) {
                Vec2::new(4.0, -1.5)
            } else {
                Vec2::ZERO
            };

            app.update();

            let tap_left = tap.map(|code| {
                app.world()
                    .resource::<ButtonInput<KeyCode>>()
                    .just_pressed(code)
            });
            // The rig has no input plugin: a tap lasts one frame by hand.
            {
                let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
                if let Some(code) = tap {
                    keys.release(code);
                }
                keys.clear();
                let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
                mouse.release(MouseButton::Left);
                mouse.clear();
            }
            let world = app.world_mut();
            let mouse_captured = world
                .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
                .single(world)
                .is_ok_and(|c| c.grab_mode != CursorGrabMode::None);
            let sim = world.resource::<Sim>();
            out.push(Frame {
                player: format!("{:?}", sim.game.player()),
                tick: sim.game.clock().tick(),
                tap_left,
                mouse_captured,
                gizmos: world.resource::<crate::LevelGizmos>().0,
            });
        }
        out
    }

    /// The trace Classic's recorder holds (and stops it), as written.
    fn take_trace(app: &mut App) -> String {
        app.world_mut()
            .resource_mut::<Sim>()
            .game
            .stop_recording()
            .expect("F9 started Classic's trace recording")
            .to_jsonl_string()
            .unwrap()
    }

    /// The claim the Sandbox rests on, at the level of the app: a Classic
    /// process is the same process with or without the plugin compiled in.
    /// Same keys in, the same game out, tick for tick, and the same parity
    /// trace out of Classic's own F9 recorder, byte for byte. Differential:
    /// a legitimate change to Classic moves both sides.
    #[test]
    fn a_classic_process_is_the_same_with_and_without_the_plugin() {
        const FRAMES: usize = 900;

        let mut alone = classic_alone();
        let control = play_the_key_script(&mut alone, FRAMES);

        let mut idle = classic_with_the_plugin();
        assert_eq!(lab(&idle).phase, Phase::Idle);
        let observed = play_the_key_script(&mut idle, FRAMES);

        // The run is one worth comparing: the game ran nearly every frame,
        // stood still while paused, took its keys and moved a long way.
        let last = control.last().unwrap();
        assert!(last.tick as usize >= FRAMES - 20, "{}", last.tick);
        assert!(control.iter().any(|f| !f.mouse_captured), "the pause");
        assert!(last.mouse_captured, "the click resumed the game");
        assert!(control.iter().any(|f| !f.gizmos), "F10 reached Classic");
        assert!(
            control.iter().all(|f| f.tap_left != Some(false)),
            "the script's own bookkeeping"
        );
        let distinct: std::collections::BTreeSet<&str> =
            control.iter().map(|f| f.player.as_str()).collect();
        assert!(distinct.len() > FRAMES / 2, "{} states", distinct.len());

        // Frame for frame the same.
        for (index, (a, b)) in control.iter().zip(&observed).enumerate() {
            assert!(
                a == b,
                "frame {index} differs with the plugin registered:\n  alone:  {a:?}\n  plugin: {b:?}"
            );
        }
        assert_eq!(control.len(), observed.len());

        // The Sandbox never woke up: no session, no request, its state
        // untouched, the clocks as Classic has them.
        {
            let lab = lab(&idle);
            assert_eq!(lab.phase, Phase::Idle);
            assert!(lab.session.is_none() && lab.stage.is_none() && lab.classic.is_none());
            assert!(lab.notice.is_none(), "{:?}", lab.notice);
            assert!(idle.world().resource::<Messages<LabRequest>>().is_empty());
            assert_eq!(
                *idle.world().resource::<PanelState>(),
                PanelState::default()
            );
            assert_eq!(
                *idle.world().resource::<super::super::VizSettings>(),
                super::super::VizSettings::default()
            );
            assert_eq!(screen(&idle), Screen::None);
            let virt = idle.world().resource::<Time<Virtual>>();
            assert!(!virt.is_paused());
            assert_eq!(virt.relative_speed(), 1.0);
            assert!(flow_requests(&idle).is_empty());
            assert_eq!(sim(&idle).banner, classic_banner());
        }

        // Classic's own recorder (started by F9 in both) wrote the same
        // parity trace, and the games are the same value.
        assert!(sim(&idle).game.is_recording());
        let (trace_alone, trace_idle) = (take_trace(&mut alone), take_trace(&mut idle));
        assert!(trace_alone.lines().count() > FRAMES - 30);
        assert!(
            trace_alone == trace_idle,
            "Classic's recorded trace differs with the plugin registered"
        );
        assert!(
            format!("{:?}", alone.world().resource::<Sim>().game)
                == format!("{:?}", sim(&idle).game),
            "the games differ somewhere outside the player and the trace"
        );

        // The harness can see the Sandbox when it does act: the same keys in
        // a session are the Sandbox's (P freezes, the debug keys are logged
        // commands), and the run is no longer Classic's.
        let mut session = classic_with_the_plugin();
        send(&mut session, start(Stage::Graybox, None));
        frames(&mut session, 1);
        assert_eq!(lab(&session).phase, Phase::Active);
        let in_session = play_the_key_script(&mut session, 300);
        assert!(
            in_session
                .iter()
                .zip(&control)
                .any(|(a, b)| a.player != b.player || a.tick != b.tick),
            "a session that takes the keys left the run exactly Classic's"
        );
        // F9 in a session is the Sandbox's: Classic's handler never saw it.
        assert!(in_session[3].tap_left == Some(false), "{:?}", in_session[3]);
        assert!(!lab(&session).session.as_ref().unwrap().log().is_empty());

        // And it would see a leak into an idle process. A stand-in for one:
        // a system in the Sandbox's own input set that takes one of Classic's
        // debug keys although no session exists.
        fn takes_a_classic_key(mut keys: ResMut<ButtonInput<KeyCode>>) {
            keys.clear_just_pressed(KeyCode::F4);
        }
        let mut leaky = classic_with_the_plugin();
        leaky.add_systems(
            RunFixedMainLoop,
            takes_a_classic_key.in_set(super::super::LabSet::Input),
        );
        let leaked = play_the_key_script(&mut leaky, FRAMES);
        let first = leaked.iter().zip(&control).position(|(a, b)| a != b);
        let first = first.expect("a key taken from Classic in an idle process went unnoticed");
        assert_eq!(leaked[first].tap_left, Some(false), "{:?}", leaked[first]);
        // Not only the key: the game went another way from there on.
        assert!(
            leaked[first + 1..]
                .iter()
                .zip(&control[first + 1..])
                .any(|(a, b)| a.player != b.player),
            "the lost key changed nothing in the game"
        );
    }
}
