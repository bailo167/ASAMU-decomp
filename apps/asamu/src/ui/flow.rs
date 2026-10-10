//! What the menus do: level loads, start / resume / restart, quitting, the
//! checkpoint snapshot saves after each fixed tick, the time-trial rules and
//! the integration messages of the Kismet / world workstreams.
//!
//! Loading a converted map (New Game, Continue, chapter select, time trial,
//! a Kismet `open`): the running simulation is removed, the render plan is
//! rebuilt when the map differs from the one shown (the level's entities
//! are despawned and `converted.rs` spawns the new plan when it is ready),
//! and the map's [`Game`] with its Kismet and NPCs
//! ([`asamu_game::load_level_with_kismet_options`]) loads on the async pool.
//! When it is ready the save session decides the level start
//! ([`asamu_game::save::SaveSession::begin_level`]: chapter unlock + chapter
//! pointer in story mode), the snapshot is applied at the spawn
//! ([`Game::apply_snapshot`]: abilities, checkpoint table, reset to the
//! latest checkpoint), the level script learns the loaded checkpoint index
//! (`SavedGameStateLoaded`) and the Kismet save strings
//! ([`prepare_script`]), the game starts and the mouse is captured.

use asamu_assets::LevelPlan;
use asamu_game::save::{
    Achievement, ChapterId, Extra, FRONT_END_MAP, PlayMode, checkpoint_saved, format_trial_time,
};
use asamu_game::{Game, GameState, LevelOptions, LevelScript, load_level_with_kismet_options};
use bevy::ecs::system::SystemParam;
use bevy::pbr::DistanceFog;
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::notify::{TimeTrialClock, Toasts};
use super::strings::UiStrings;
use super::{
    AchievementEarned, CollectibleFound, GameFinished, GameTick, OpenMap, Play, SaveStringEdited,
    Saves, Screen, StoryItemFound, TimeTrialEnd, TimeTrialStart, UiLaunch, UiState,
};
use crate::converted::{ConvertedLevel, LevelBsp, LevelEntity, LevelLight, LevelPhase};
use crate::{PendingInput, PlayerCamera, Sim};

/// A request from the menus (or the integration messages) to the flow.
#[derive(Message, Clone, Debug, PartialEq)]
pub(crate) enum FlowRequest {
    /// Load a converted map.
    Load {
        /// Map name (case-insensitive).
        map: String,
        /// Story or time trial.
        mode: PlayMode,
    },
    /// Restart the graybox test level (no converted data).
    Graybox,
    /// Close the menu and play on.
    Resume,
    /// Back to the latest checkpoint (the death sequence, as the original's
    /// quick load), then play on.
    Restart,
    /// Stop a level load.
    CancelLoad,
    /// The game ended (Kismet `open ASAMUFrontEndMap`: the chapters' "back
    /// to the main menu" and the end of a time trial): drop the simulation
    /// and show the main menu, as the original's map change does.
    EndGame,
    /// Quit.
    Quit,
}

/// The map of an `open <map>[?options]` target: options cut off, blanks
/// trimmed (the Kismet host normally passes the bare name already).
#[must_use]
pub(crate) fn open_target(map: &str) -> &str {
    map.split_once('?').map_or(map, |(m, _)| m).trim()
}

/// A loaded converted map: the game and its level script.
pub(crate) type LoadedLevel = (Game, Option<LevelScript>);

/// Loads a converted map with its Kismet and NPCs (time trial: the
/// time-trial game type).
pub(crate) fn load_level(
    root: &std::path::Path,
    map: &str,
    mode: PlayMode,
) -> Result<LoadedLevel, String> {
    let options = LevelOptions {
        time_trial: mode == PlayMode::TimeTrial,
        ..LevelOptions::default()
    };
    load_level_with_kismet_options(root, map, options).map_err(|e| e.to_string())
}

/// A converted map's game loading on the async pool.
#[derive(Resource, Default)]
pub(crate) struct LevelLoad {
    task: Option<Task<Result<LoadedLevel, String>>>,
    /// Map and mode being loaded.
    pending: Option<(String, PlayMode)>,
}

impl LevelLoad {
    /// A load is running.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.task.is_some()
    }

    /// The map being loaded.
    #[must_use]
    pub fn pending_map(&self) -> Option<&str> {
        self.pending.as_ref().map(|(m, _)| m.as_str())
    }
}

/// Captures (`true`) or releases the mouse.
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

/// A fresh graybox game with the running game's configuration.
fn fresh_graybox(current: &Game) -> Option<Game> {
    let game = if current.uses_original_params() {
        Game::graybox()
    } else {
        Game::graybox_placeholder()
    };
    game.ok()
        .map(|g| g.with_movement_model(current.movement_model()))
}

/// Filter of every spawned converted-level entity (meshes, BSP, lights,
/// and the light-mapped BSP meshes `lightmaps.rs` adds).
type LevelEntities = Or<(
    With<LevelEntity>,
    With<LevelBsp>,
    With<LevelLight>,
    With<crate::lightmaps::LightmappedBsp>,
)>;

/// Resources the flow changes.
#[derive(SystemParam)]
pub(crate) struct FlowCtx<'w, 's> {
    commands: Commands<'w, 's>,
    launch: Res<'w, UiLaunch>,
    state: ResMut<'w, UiState>,
    play: ResMut<'w, Play>,
    load: ResMut<'w, LevelLoad>,
    sim: Option<ResMut<'w, Sim>>,
    level: Option<ResMut<'w, ConvertedLevel>>,
    game_load: Option<ResMut<'w, crate::GameLoad>>,
    level_entities: Query<'w, 's, Entity, LevelEntities>,
    cameras: Query<'w, 's, Entity, With<PlayerCamera>>,
    cursor: Query<'w, 's, &'static mut CursorOptions, With<PrimaryWindow>>,
    pending_input: Option<ResMut<'w, PendingInput>>,
    exit: MessageWriter<'w, AppExit>,
}

impl FlowCtx<'_, '_> {
    /// Closes the menu, captures the mouse, drops stale look input.
    fn play_on(&mut self) {
        self.state.close();
        set_grab(&mut self.cursor, true);
        if let Some(p) = self.pending_input.as_mut() {
            **p = PendingInput::default();
        }
    }

    /// Starts loading a converted map.
    fn start_load(&mut self, map: &str, mode: PlayMode) -> Result<(), String> {
        let Some(dir) = self.launch.converted.clone() else {
            return Err("no converted data (start with --converted DIR)".to_owned());
        };
        let Some(name) = self.launch.level_name(map).map(str::to_owned) else {
            return Err(format!(
                "{map} is not converted (asamu-import levels --map {map}, plus meshes and textures)"
            ));
        };
        // A pending load of the `--level` start must not replace this one.
        if let Some(gl) = self.game_load.as_mut() {
            gl.request = None;
            gl.task = None;
        }
        self.commands.remove_resource::<Sim>();
        if let Some(level) = self.level.as_mut() {
            let same = level.level.eq_ignore_ascii_case(&name)
                && matches!(
                    level.phase,
                    Some(LevelPhase::Planning(_) | LevelPhase::Spawned(_))
                );
            if !same {
                for e in &self.level_entities {
                    self.commands.entity(e).despawn();
                }
                for c in &self.cameras {
                    self.commands.entity(c).remove::<DistanceFog>();
                }
                level.level.clone_from(&name);
                level.progress = (0, 0, 0);
                let (plan_dir, plan_name, options) =
                    (level.dir.clone(), name.clone(), level.options);
                level.phase = Some(LevelPhase::Planning(AsyncComputeTaskPool::get().spawn(
                    async move {
                        LevelPlan::load(&plan_dir, &plan_name, &options).map_err(|e| e.to_string())
                    },
                )));
            }
        }
        let root = dir.root().to_path_buf();
        let game_name = name.clone();
        self.load.task = Some(
            AsyncComputeTaskPool::get().spawn(async move { load_level(&root, &game_name, mode) }),
        );
        info!("loading {name} ({mode:?})");
        self.load.pending = Some((name, mode));
        self.state.message = None;
        self.state.open(Screen::Loading);
        set_grab(&mut self.cursor, false);
        Ok(())
    }
}

/// Runs the flow requests.
pub(crate) fn run_flow(mut requests: MessageReader<FlowRequest>, mut ctx: FlowCtx) {
    for request in requests.read().cloned().collect::<Vec<_>>() {
        match request {
            FlowRequest::Load { map, mode } => {
                if let Err(e) = ctx.start_load(&map, mode) {
                    warn!("{e}");
                    let back = if ctx.state.screen == Screen::None {
                        Screen::Main
                    } else {
                        ctx.state.screen
                    };
                    // A game still running behind the menu (a Kismet `open`
                    // of a map that is not converted) waits paused.
                    if let Some(sim) = ctx.sim.as_mut()
                        && sim.game.state() == GameState::Playing
                    {
                        sim.game.pause();
                    }
                    ctx.state.open_with(back, e);
                }
            }
            FlowRequest::EndGame => {
                ctx.commands.remove_resource::<Sim>();
                *ctx.play = Play::default();
                ctx.state.message = None;
                ctx.state.confirm = None;
                ctx.state.open(Screen::Main);
                set_grab(&mut ctx.cursor, false);
            }
            FlowRequest::Graybox => {
                let Some(sim) = ctx.sim.as_mut() else {
                    continue;
                };
                if let Some(game) = fresh_graybox(&sim.game) {
                    sim.game = game;
                    sim.script = None;
                    sim.snap_interpolation();
                    sim.game.start();
                    info!("graybox test level started");
                    *ctx.play = Play::default();
                    ctx.play_on();
                }
            }
            FlowRequest::Resume => {
                let Some(sim) = ctx.sim.as_mut() else {
                    ctx.state.open(Screen::Main);
                    continue;
                };
                match sim.game.state() {
                    GameState::Boot => sim.game.start(),
                    GameState::Paused => sim.game.resume(),
                    GameState::Playing => {}
                }
                ctx.play_on();
            }
            FlowRequest::Restart => {
                let Some(sim) = ctx.sim.as_mut() else {
                    continue;
                };
                // The original's quick load: the death sequence, then the
                // latest checkpoint (converted levels; graybox: at once).
                sim.game.kill_player();
                match sim.game.state() {
                    GameState::Boot => sim.game.start(),
                    GameState::Paused => sim.game.resume(),
                    GameState::Playing => {}
                }
                ctx.play_on();
            }
            FlowRequest::CancelLoad => {
                ctx.load.task = None;
                ctx.load.pending = None;
                ctx.state.open(Screen::Main);
            }
            FlowRequest::Quit => {
                ctx.exit.write(AppExit::Success);
            }
        }
    }
}

/// The save side of a level start (SAVE.md §5): the session's level start
/// (story: chapter unlock + pointer), then the snapshot applied at the spawn.
/// Returns the decision, the write result and the checkpoint index the
/// Kismet "save loaded" event receives (`None`: no event — time trial loads
/// no save). Without a snapshot story play still fires the event with −1,
/// as the original does when its load fails; the player then stays at the
/// `PlayerStart` (no "reset all", our fix of SAVE.md Q2).
pub(crate) fn level_start(
    game: &mut Game,
    saves: &mut asamu_game::save::SaveSession,
    map: &str,
    mode: PlayMode,
) -> (
    asamu_game::save::LevelStart,
    Result<(), asamu_game::save::SaveError>,
    Option<i32>,
) {
    let title = game.scene_map().and_then(|m| m.world.title.clone());
    let (start, result) = saves.begin_level_lossy(map, title.as_deref());
    if start.newly_unlocked
        && let Some(c) = start.chapter
    {
        info!("chapter unlocked: {}", c.enum_name());
    }
    let loaded = match &start.snapshot {
        Some(snapshot) => {
            let applied = game.apply_snapshot(snapshot);
            info!(
                "save snapshot applied ({}): checkpoint {}",
                snapshot.map, applied.checkpoint_index
            );
            Some(applied.checkpoint_index)
        }
        None => (mode == PlayMode::Story && saves.mode() == PlayMode::Story).then_some(-1),
    };
    (start, result, loaded)
}

/// The level script's side of a level start: the checkpoint index the
/// saved-game-state events receive (`None`: no event, time trial), and the
/// Kismet save strings kept in the general save (the original keeps them in
/// `GeneralSave.bin` across maps).
pub(crate) fn prepare_script(
    script: &mut LevelScript,
    saves: &asamu_game::save::SaveSession,
    index: Option<i32>,
) {
    let rt = script.runtime_mut();
    rt.set_start_save_index(index);
    rt.set_save_strings(saves.general.flags.clone());
}

/// Finishes a level load: save-session level start, snapshot, start.
#[allow(clippy::too_many_arguments)]
pub(crate) fn poll_level_load(
    mut commands: Commands,
    mut load: ResMut<LevelLoad>,
    mut saves: ResMut<Saves>,
    mut play: ResMut<Play>,
    mut state: ResMut<UiState>,
    mut clock: ResMut<TimeTrialClock>,
    mut toasts: ResMut<Toasts>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut pending_input: Option<ResMut<PendingInput>>,
) {
    let Some(task) = load.task.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    load.task = None;
    let Some((map, mode)) = load.pending.take() else {
        return;
    };
    let (mut game, mut script) = match result {
        Ok(loaded) => loaded,
        Err(e) => {
            warn!("could not load {map}: {e}");
            state.open_with(Screen::Main, format!("Could not load {map}: {e}"));
            return;
        }
    };
    if let Some(m) = game.scene_map()
        && !m.warnings.is_empty()
    {
        warn!(
            "{map}: gameplay data loaded with {} warnings, e.g. {:?}",
            m.warnings.len(),
            m.warnings.iter().take(3).collect::<Vec<_>>()
        );
    }
    let (start, result, loaded_index) = level_start(&mut game, &mut saves.0, &map, mode);
    if let Err(e) = result {
        warn!("save failed: {e}");
        toasts.push(format!("Could not write the save: {e}"));
    }
    if let Some(script) = script.as_mut() {
        prepare_script(script, &saves.0, loaded_index);
    }
    game.start();
    info!(
        "{map} started ({mode:?}, chapter {:?})",
        start.chapter.map(ChapterId::enum_name)
    );
    *play = Play {
        chapter: start.chapter,
        mode,
        map: Some(map),
    };
    *clock = TimeTrialClock::default();
    let banner = crate::banner(&game);
    commands.insert_resource(Time::<Fixed>::from_hz(game.clock().tick_rate_hz()));
    commands.insert_resource(Sim::new(game, banner).with_script(script));
    state.close();
    set_grab(&mut cursor, true);
    if let Some(p) = pending_input.as_mut() {
        **p = PendingInput::default();
    }
}

/// Opens the pause menu when the game was paused (Esc in `main.rs`).
pub(crate) fn detect_pause(mut state: ResMut<UiState>, sim: Option<Res<Sim>>) {
    if state.screen == Screen::None
        && sim
            .as_ref()
            .is_some_and(|s| s.game.state() == GameState::Paused)
    {
        state.open(Screen::Pause);
    }
}

/// Keeps the mouse free while a menu is open (the fly camera and the
/// debug click-to-play capture it on any click).
pub(crate) fn enforce_menu_cursor(
    state: Res<UiState>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if state.screen != Screen::None {
        set_grab(&mut cursor, false);
    }
}

/// After each fixed tick: the checkpoint snapshot save (story play in a
/// chapter map), the time-trial death rule (A-TT-2) and the stopwatch.
pub(crate) fn process_ticks(
    mut ticks: MessageReader<GameTick>,
    sim: Option<Res<Sim>>,
    mut saves: ResMut<Saves>,
    play: Res<Play>,
    mut clock: ResMut<TimeTrialClock>,
    mut toasts: ResMut<Toasts>,
) {
    let Some(sim) = sim else {
        ticks.clear();
        return;
    };
    let rate = sim.game.clock().tick_rate_hz();
    for GameTick(report) in ticks.read() {
        if let Some(index) = checkpoint_saved(report)
            && play.mode == PlayMode::Story
            && saves.0.mode() == PlayMode::Story
            && sim.game.chapter().is_some()
        {
            let snapshot = sim.game.capture_snapshot(saves.0.snapshot.as_ref());
            match saves.0.on_checkpoint_saved(snapshot) {
                Ok(()) => {
                    info!("checkpoint {index} saved");
                    toasts.push("Checkpoint saved");
                }
                Err(e) => {
                    warn!("checkpoint save failed: {e}");
                    toasts.push(format!("Could not write the save: {e}"));
                }
            }
        }
        // A-TT-2: the rule runs in the game's death hook, i.e. at the
        // player reset 0.3 s into the death sequence (ABILITIES.md A-DT-2),
        // so the restarted stopwatch counts from the respawn.
        if play.mode == PlayMode::TimeTrial
            && report.respawned
            && sim.game.latest_checkpoint_index().is_none()
            && clock.running
        {
            clock.restart(report.tick);
        }
        clock.update(report.tick, rate);
    }
}

fn extra_name(e: Extra) -> &'static str {
    match e {
        Extra::BeamColour => "beam colour",
        Extra::GoatMode => "goat mode",
        Extra::MidasMode => "Midas mode",
        Extra::ParkourMode => "parkour mode",
    }
}

/// `FLOOR_IS_LAVA` → `Floor is lava` (the fallback of
/// [`UiStrings::achievement_title`]).
#[cfg(test)]
fn achievement_label(a: Achievement) -> String {
    super::strings::achievement_words(a)
}

/// The integration messages: progression events, Kismet flags, time
/// trial, story transitions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_integration(
    mut collectibles: MessageReader<CollectibleFound>,
    mut story: MessageReader<StoryItemFound>,
    mut achievements: MessageReader<AchievementEarned>,
    mut strings: MessageReader<SaveStringEdited>,
    mut finished: MessageReader<GameFinished>,
    mut trial_start: MessageReader<TimeTrialStart>,
    mut trial_end: MessageReader<TimeTrialEnd>,
    mut open: MessageReader<OpenMap>,
    mut saves: ResMut<Saves>,
    play: Res<Play>,
    mut toasts: ResMut<Toasts>,
    mut clock: ResMut<TimeTrialClock>,
    sim: Option<Res<Sim>>,
    mut flow: MessageWriter<FlowRequest>,
    texts: Res<UiStrings>,
) {
    let achievement_label = |a: Achievement| texts.achievement_title(a);
    fn report(toasts: &mut Toasts, e: asamu_game::save::SaveError) {
        warn!("save failed: {e}");
        toasts.push(format!("Could not write the save: {e}"));
    }
    for m in collectibles.read() {
        let Some(chapter) = play.chapter else {
            continue;
        };
        match saves.0.on_collectible(chapter, &m.key) {
            Ok(out) if out.new => {
                toasts.push(format!(
                    "Collectible found | {}/{} here | {}/{} in total",
                    out.chapter_count,
                    asamu_game::save::COLLECTIBLES_PER_CHAPTER,
                    out.total,
                    asamu_game::save::TOTAL_COLLECTIBLES
                ));
                if let Some(e) = out.extra_unlocked {
                    toasts.push(format!("Extra unlocked: {}", extra_name(e)));
                }
                if out.all_found {
                    toasts.push(format!(
                        "Achievement: {}",
                        achievement_label(Achievement::ALL_COLLECTIBLES_FOUND)
                    ));
                }
            }
            Ok(_) => {}
            Err(e) => report(&mut toasts, e),
        }
    }
    for m in story.read() {
        match saves.0.on_story_item(&m.key) {
            Ok((true, all)) => {
                toasts.push(format!(
                    "Story item found ({}/{})",
                    saves.0.progression.story_items.len(),
                    asamu_game::save::TOTAL_STORY_ITEMS
                ));
                if all {
                    toasts.push(format!(
                        "Achievement: {}",
                        achievement_label(Achievement::INTERACT_ALL_STORY)
                    ));
                }
            }
            Ok(_) => {}
            Err(e) => report(&mut toasts, e),
        }
    }
    for AchievementEarned(a) in achievements.read() {
        match saves.0.on_achievement(*a) {
            Ok(true) => toasts.push(format!("Achievement: {}", achievement_label(*a))),
            Ok(false) => {}
            Err(e) => report(&mut toasts, e),
        }
    }
    for m in strings.read() {
        if let Err(e) = saves.0.on_save_string(&m.id, m.value) {
            report(&mut toasts, e);
        }
    }
    for _ in finished.read() {
        match saves.0.on_game_finished() {
            Ok(()) => toasts.push("Time trial unlocked"),
            Err(e) => report(&mut toasts, e),
        }
    }
    for _ in trial_start.read() {
        if play.mode == PlayMode::TimeTrial
            && let Some(sim) = &sim
        {
            clock.start(sim.game.clock().tick());
        }
    }
    for _ in trial_end.read() {
        let (Some(seconds), Some(chapter)) = (clock.stop(), play.chapter) else {
            continue;
        };
        match saves.0.on_time_trial_end(chapter, seconds as f32) {
            Ok(out) => {
                let medal = match out.medal {
                    Some(asamu_game::save::Medal::Gold) => " | gold",
                    Some(asamu_game::save::Medal::Silver) => " | silver",
                    Some(asamu_game::save::Medal::Bronze) => " | bronze",
                    None => "",
                };
                toasts.push(format!(
                    "Time {}{}{}",
                    format_trial_time(seconds),
                    if out.new_best { " | new best" } else { "" },
                    medal
                ));
                if out.all_gold {
                    toasts.push(format!(
                        "Achievement: {}",
                        achievement_label(Achievement::ALL_GOLD_MEDALS)
                    ));
                }
            }
            Err(e) => report(&mut toasts, e),
        }
    }
    for m in open.read() {
        let map = open_target(&m.map);
        if map.eq_ignore_ascii_case(FRONT_END_MAP) {
            // The chapters' "back to the main menu" commands and the end of
            // a time trial: the game is over (no "Return to game").
            flow.write(FlowRequest::EndGame);
        } else if play.mode == PlayMode::Story && !map.is_empty() {
            // Time trial never chains into the next chapter (its Kismet
            // opens the main menu instead, KISMET.md).
            flow.write(FlowRequest::Load {
                map: map.to_owned(),
                mode: PlayMode::Story,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn achievement_labels_read_as_words() {
        assert_eq!(
            achievement_label(Achievement::FLOOR_IS_LAVA),
            "Floor is lava"
        );
        assert_eq!(
            achievement_label(Achievement::ALL_COLLECTIBLES_FOUND),
            "All collectibles found"
        );
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// user-local `asamu-import` output with `AG-ParadiseCave`): a checkpoint
    /// that becomes the latest during a tick is written as the snapshot by
    /// [`process_ticks`], with the checkpoint table entry of the chapter.
    #[test]
    fn checkpoint_saved_in_a_tick_writes_the_snapshot() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let mut game = match Game::load_level(&root, "AG-ParadiseCave") {
            Ok(g) => g,
            Err(e) => {
                eprintln!("skipped: AG-ParadiseCave not loadable ({e})");
                return;
            }
        };
        let id = game
            .scene_map()
            .and_then(|m| m.actors.checkpoints.iter().find(|c| c.index == 1))
            .map(|c| c.id)
            .expect("ParadiseCave has a checkpoint with index 1");
        game.start();
        let mut saves = Saves::default();
        saves.0.new_game().unwrap();
        let start = saves
            .0
            .begin_level(
                "AG-ParadiseCave",
                game.scene_map().and_then(|m| m.world.title.as_deref()),
            )
            .unwrap();
        assert_eq!(start.chapter, Some(ChapterId::Sanctuary));
        assert!(game.trigger_checkpoint(id));
        let report = game.tick(&asamu_player::InputFrame::default()).unwrap();
        assert_eq!(checkpoint_saved(&report), Some(1));

        let mut app = App::new();
        app.add_message::<GameTick>()
            .insert_resource(saves)
            .insert_resource(Play {
                chapter: start.chapter,
                mode: PlayMode::Story,
                map: Some("AG-ParadiseCave".into()),
            })
            .init_resource::<TimeTrialClock>()
            .init_resource::<Toasts>()
            .insert_resource(Sim::new(game, String::new()))
            .add_systems(Update, process_ticks);
        app.world_mut().write_message(GameTick(report));
        app.update();
        let snap = app
            .world()
            .resource::<Saves>()
            .0
            .snapshot
            .clone()
            .expect("snapshot written");
        assert_eq!(snap.map, "AG-ParadiseCave");
        assert_eq!(snap.checkpoints.get(&ChapterId::Sanctuary), Some(&1));
        assert!(snap.abilities.is_some());
    }

    #[test]
    fn open_targets_drop_url_options() {
        assert_eq!(
            open_target("ASAMUFrontEndMap?game=ASAMU.GFxASAMUMenuGameInfo"),
            "ASAMUFrontEndMap"
        );
        assert_eq!(open_target(" AG-DarkCave "), "AG-DarkCave");
        assert_eq!(open_target("?x"), "");
    }

    #[test]
    fn level_start_fires_save_loaded_in_story_play_only() {
        use asamu_game::save::SaveSession;
        // Story play without a snapshot (e.g. it was unreadable): -1.
        let mut g = Game::graybox().unwrap();
        let mut s = SaveSession::in_memory();
        let (start, result, loaded) = level_start(&mut g, &mut s, "AG-Workshop", PlayMode::Story);
        assert!(result.is_ok());
        assert_eq!(start.chapter, Some(ChapterId::Workshop));
        assert_eq!(start.snapshot, None);
        assert_eq!(loaded, Some(-1));
        assert_eq!(s.general.current, Some(ChapterId::Workshop));
        // New Game: the fresh snapshot is applied (no entry for the chapter).
        let mut s = SaveSession::in_memory();
        s.new_game().unwrap();
        let (_, _, loaded) = level_start(&mut g, &mut s, "AG-Workshop", PlayMode::Story);
        assert_eq!(loaded, Some(-1));
        // Time trial loads no save and fires nothing.
        let mut s = SaveSession::in_memory();
        s.on_game_finished().unwrap();
        assert!(s.start_time_trial(ChapterId::Village));
        let (start, _, loaded) =
            level_start(&mut g, &mut s, "AG-BeautifulCity", PlayMode::TimeTrial);
        assert_eq!(loaded, None);
        assert_eq!(start.snapshot, None);
        assert!(s.progression.unlocked.is_empty());
    }

    fn tick(n: u64, respawned: bool) -> GameTick {
        GameTick(asamu_game::TickReport {
            tick: n,
            events: Default::default(),
            respawned,
            checkpoint_activated: None,
            world: Default::default(),
            died: None,
        })
    }

    #[test]
    fn time_trial_stopwatch_restarts_at_the_respawn_before_any_checkpoint() {
        let mut app = App::new();
        let mut clock = TimeTrialClock::default();
        clock.start(0);
        app.add_message::<GameTick>()
            .insert_resource(Saves::default())
            .insert_resource(Play {
                chapter: Some(ChapterId::Village),
                mode: PlayMode::TimeTrial,
                map: None,
            })
            .insert_resource(clock)
            .init_resource::<Toasts>()
            .insert_resource(Sim::new(Game::graybox().unwrap(), String::new()))
            .add_systems(Update, process_ticks);
        app.world_mut().write_message(tick(120, false));
        app.update();
        let c = *app.world().resource::<TimeTrialClock>();
        assert!(c.running);
        let rate = Game::graybox().unwrap().clock().tick_rate_hz();
        assert!((c.elapsed - 120.0 / rate).abs() < 1e-9);
        // The death's reset tick (0.3 s into the death sequence) restarts it.
        app.world_mut().write_message(tick(150, true));
        app.update();
        let c = *app.world().resource::<TimeTrialClock>();
        assert!(c.running);
        assert_eq!(c.elapsed, 0.0);
        app.world_mut().write_message(tick(180, false));
        app.update();
        let c = *app.world().resource::<TimeTrialClock>();
        assert!((c.elapsed - 30.0 / rate).abs() < 1e-9);
    }

    #[test]
    fn opening_the_front_end_ends_the_game_and_shows_the_main_menu() {
        let mut app = App::new();
        app.add_message::<CollectibleFound>()
            .add_message::<StoryItemFound>()
            .add_message::<AchievementEarned>()
            .add_message::<SaveStringEdited>()
            .add_message::<GameFinished>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .add_message::<OpenMap>()
            .add_message::<FlowRequest>()
            .add_message::<AppExit>()
            .init_resource::<Saves>()
            .insert_resource(Play {
                chapter: Some(ChapterId::Epilogue),
                mode: PlayMode::Story,
                map: Some("AG-Epilogue".into()),
            })
            .init_resource::<Toasts>()
            .init_resource::<TimeTrialClock>()
            .init_resource::<UiLaunch>()
            .init_resource::<UiState>()
            .init_resource::<LevelLoad>()
            .init_resource::<UiStrings>()
            .insert_resource(Sim::new(Game::graybox().unwrap(), String::new()))
            .add_systems(Update, (handle_integration, run_flow).chain());
        app.world_mut().write_message(OpenMap {
            map: "ASAMUFrontEndMap?game=ASAMU.GFxASAMUMenuGameInfo".into(),
        });
        app.update();
        assert!(app.world().get_resource::<Sim>().is_none(), "game over");
        assert_eq!(*app.world().resource::<Play>(), Play::default());
        assert_eq!(app.world().resource::<UiState>().screen, Screen::Main);
    }

    #[test]
    fn a_story_open_of_a_missing_map_pauses_the_running_game() {
        let mut app = App::new();
        let mut sim = Sim::new(Game::graybox().unwrap(), String::new());
        sim.game.start();
        app.add_message::<FlowRequest>()
            .add_message::<AppExit>()
            .init_resource::<UiLaunch>()
            .init_resource::<UiState>()
            .init_resource::<Play>()
            .init_resource::<LevelLoad>()
            .insert_resource(sim)
            .add_systems(Update, run_flow);
        app.world_mut().write_message(FlowRequest::Load {
            map: "AG-Nowhere".into(),
            mode: PlayMode::Story,
        });
        app.update();
        let state = app.world().resource::<UiState>();
        assert_eq!(state.screen, Screen::Main);
        assert!(state.message.is_some());
        assert_eq!(
            app.world().resource::<Sim>().game.state(),
            GameState::Paused
        );
    }

    #[test]
    fn graybox_restart_keeps_the_configuration() {
        let g = Game::graybox_placeholder().unwrap();
        let fresh = fresh_graybox(&g).unwrap();
        assert!(!fresh.uses_original_params());
        assert_eq!(fresh.movement_model(), g.movement_model());
        let g = Game::graybox().unwrap();
        assert!(fresh_graybox(&g).unwrap().uses_original_params());
    }
}
