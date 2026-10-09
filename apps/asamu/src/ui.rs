//! Menus and UI: main menu, pause menu, settings, chapter select, time-trial
//! select, a collectibles counter, story-item / achievement notices and the
//! time-trial stopwatch — a **functional** Bevy replacement of the original's
//! Scaleform menus (not a visual clone), driving the save system in
//! [`asamu_game::save`]. Design and parity notes: `docs/UI_AND_SAVES.md`.
//!
//! - [`menus`]: the screens (rebuilt whenever they change), buttons
//!   (`bevy_ui_widgets` `Button` + `Activate`), keyboard (Esc = back, Tab /
//!   Enter through the buttons).
//! - [`flow`]: what the menus do — load a converted chapter (render plan and
//!   game, both on the async pool), apply the save snapshot at the spawn,
//!   start/resume/pause, checkpoint snapshot saves after each fixed tick, the
//!   integration messages for the Kismet / world workstreams.
//! - [`settings`]: user settings (FOV, mouse, volumes, window, subtitles),
//!   persisted in `settings.json` next to the saves and applied live.
//! - [`notify`]: toasts, the collectibles counter, the time-trial stopwatch.
//! - [`strings`]: menu text (ours, English) with optional per-user overrides
//!   produced from the user's own install (`<converted>/ui/strings.json`).
//!
//! Saves live in the user-local data directory
//! ([`asamu_game::save::SaveStore::default_root`], `ASAMU_SAVE_DIR`
//! overrides it); a run started straight into a level (`--level`) keeps its
//! saves in memory only.

mod flow;
mod menus;
mod notify;
mod settings;
mod strings;

use asamu_assets::ConvertedDir;
use asamu_game::TickReport;
use asamu_game::save::{Achievement, ChapterId, FRONT_END_MAP, PlayMode, SaveSession, SaveStore};
use bevy::prelude::*;

pub(crate) use settings::UserSettings;

/// How the app was launched (inserted by `main.rs`; the default is the
/// graybox with the main menu shown).
#[derive(Resource, Clone, Debug)]
pub(crate) struct UiLaunch {
    /// The converted directory (absolute), when running converted data.
    pub converted: Option<ConvertedDir>,
    /// Levels available in it (scene file stems).
    pub levels: Vec<String>,
    /// Start in the main menu.
    pub show_menu: bool,
    /// Read and write saves in the user data directory (off for runs
    /// started straight into a level).
    pub saves: bool,
}

impl Default for UiLaunch {
    fn default() -> Self {
        Self {
            converted: None,
            levels: Vec::new(),
            show_menu: true,
            saves: false,
        }
    }
}

impl UiLaunch {
    /// The converted level named `map` (case-insensitive), as listed.
    #[must_use]
    pub fn level_name(&self, map: &str) -> Option<&str> {
        self.levels
            .iter()
            .find(|l| l.eq_ignore_ascii_case(map))
            .map(String::as_str)
    }

    /// The chapter's map is converted.
    #[must_use]
    pub fn has_chapter(&self, chapter: ChapterId) -> bool {
        self.level_name(chapter.map_name()).is_some()
    }
}

/// The level shown behind the main menu of a converted run: the original's
/// front end (`ASAMUFrontEndMap`) when converted, else the first converted
/// chapter in story order, else the first converted level.
#[must_use]
pub(crate) fn menu_backdrop(levels: &[String]) -> Option<String> {
    let find = |m: &str| levels.iter().find(|l| l.eq_ignore_ascii_case(m)).cloned();
    find(FRONT_END_MAP)
        .or_else(|| ChapterId::ALL.into_iter().find_map(|c| find(c.map_name())))
        .or_else(|| levels.first().cloned())
}

/// Which screen is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Screen {
    /// No menu: playing (or the debug click-to-play state).
    #[default]
    None,
    /// Main menu.
    Main,
    /// Chapter select.
    Chapters,
    /// Time-trial select.
    TimeTrial,
    /// Settings.
    Settings,
    /// Pause menu.
    Pause,
    /// Loading a level.
    Loading,
    /// "This replaces your checkpoint progress" confirmation.
    Confirm,
}

/// Menu state.
#[derive(Resource, Debug, Default)]
pub(crate) struct UiState {
    /// The open screen.
    pub screen: Screen,
    /// Where Settings / Confirm return to.
    back: Screen,
    /// The action a confirmation runs.
    confirm: Option<menus::UiAction>,
    /// Rebuild the menu entities this frame.
    dirty: bool,
    /// Info or error line shown on the screen.
    message: Option<String>,
}

impl UiState {
    /// Opens `screen` (rebuilding the menu).
    pub fn open(&mut self, screen: Screen) {
        self.screen = screen;
        self.dirty = true;
    }

    /// Opens `screen` with a message line.
    pub fn open_with(&mut self, screen: Screen, message: impl Into<String>) {
        self.open(screen);
        self.message = Some(message.into());
    }

    /// Closes every menu.
    pub fn close(&mut self) {
        self.open(Screen::None);
        self.message = None;
        self.confirm = None;
    }
}

/// `true` while no menu is open: the gameplay input and the debug hotkeys
/// in `main.rs` run only then.
#[must_use]
pub(crate) fn gameplay_input_enabled(state: Option<Res<UiState>>) -> bool {
    state.is_none_or(|s| s.screen == Screen::None)
}

/// The run's save session.
#[derive(Resource, Debug)]
pub(crate) struct Saves(pub SaveSession);

impl Default for Saves {
    fn default() -> Self {
        Self(SaveSession::in_memory())
    }
}

/// What the menu flow is playing.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Play {
    /// Chapter of the loaded map (`None`: graybox, menu, other maps).
    pub chapter: Option<ChapterId>,
    /// Story or time trial.
    pub mode: PlayMode,
    /// The loaded converted map.
    pub map: Option<String>,
}

/// One simulation tick's report, written by `main.rs` after every
/// `Game::tick` (read here for checkpoint saves and the time-trial rules;
/// other workstreams may read it too).
#[derive(Message, Clone, Copy, Debug)]
pub(crate) struct GameTick(pub TickReport);

// Integration points for the Kismet / world workstreams (written by them,
// handled in `flow`). See docs/UI_AND_SAVES.md "Integration".

/// An `ASAMUCollectible` was picked up; `key` = its actor path relative to
/// the map (e.g. `TheWorld.PersistentLevel.ASAMUCollectible_3`).
#[derive(Message, Clone, Debug)]
#[allow(
    dead_code,
    reason = "integration point: written by the world workstream"
)]
pub(crate) struct CollectibleFound {
    /// Actor path relative to the map.
    pub key: String,
}

/// An optional story item registered itself (`<level file name><parent
/// path or None>`, SAVE.md §6.3).
#[derive(Message, Clone, Debug)]
#[allow(
    dead_code,
    reason = "integration point: written by the world workstream"
)]
pub(crate) struct StoryItemFound {
    /// Story-item key.
    pub key: String,
}

/// Kismet `SeqAct_UnlockASAMUAchievement` (or another unlock).
#[derive(Message, Clone, Copy, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct AchievementEarned(pub Achievement);

/// Kismet `SeqAct_EditOrAddSaveString` (the general save is rewritten).
/// Reads (`SeqAct_GetSaveStringValue`) use [`Saves`] directly.
#[derive(Message, Clone, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct SaveStringEdited {
    /// Flag id.
    pub id: String,
    /// Value.
    pub value: i32,
}

/// Kismet `SeqAct_SetGameFinished` (Epilogue): unlocks time trial.
#[derive(Message, Clone, Copy, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct GameFinished;

/// Kismet `SeqAct_StartTimeTrial` (ignored while the stopwatch runs).
#[derive(Message, Clone, Copy, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct TimeTrialStart;

/// Kismet `SeqAct_EndTimeTrial`: stops the stopwatch and records the time.
#[derive(Message, Clone, Copy, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct TimeTrialEnd;

/// Kismet console command `open <map>` (story chain): the next map loads
/// with the current snapshot (checkpoint table and abilities carry over);
/// `ASAMUFrontEndMap` returns to the main menu.
#[derive(Message, Clone, Debug)]
#[allow(dead_code, reason = "integration point: written by the Kismet host")]
pub(crate) struct OpenMap {
    /// Map package name.
    pub map: String,
}

/// A snapshot was applied at the level start: the Kismet host fires every
/// `SaveGameState_SeqEvent_SavedGameStateLoaded` with this index (−1 = no
/// checkpoint stored for this chapter; SAVE.md §4.3).
#[derive(Message, Clone, Copy, Debug)]
#[allow(dead_code, reason = "integration point: read by the Kismet host")]
pub(crate) struct SnapshotLoaded {
    /// Checkpoint index.
    pub checkpoint_index: i32,
}

/// Menus and UI: main menu, pause, settings, chapter select, collectibles.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<bevy::input_focus::tab_navigation::TabNavigationPlugin>() {
            app.add_plugins(bevy::input_focus::tab_navigation::TabNavigationPlugin);
        }
        app.init_resource::<UiLaunch>()
            .init_resource::<UiState>()
            .init_resource::<Saves>()
            .init_resource::<Play>()
            .init_resource::<UserSettings>()
            .init_resource::<strings::UiStrings>()
            .init_resource::<notify::Toasts>()
            .init_resource::<notify::TimeTrialClock>()
            .init_resource::<flow::LevelLoad>()
            .add_message::<GameTick>()
            .add_message::<menus::UiAction>()
            .add_message::<flow::FlowRequest>()
            .add_message::<CollectibleFound>()
            .add_message::<StoryItemFound>()
            .add_message::<AchievementEarned>()
            .add_message::<SaveStringEdited>()
            .add_message::<GameFinished>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .add_message::<OpenMap>()
            .add_message::<SnapshotLoaded>()
            .add_observer(menus::on_activate)
            .add_systems(Startup, (setup, notify::spawn_hud).chain())
            .add_systems(
                Update,
                (
                    flow::handle_integration,
                    menus::menu_keys,
                    menus::handle_actions,
                    flow::run_flow,
                    flow::poll_level_load,
                    flow::detect_pause,
                    menus::rebuild_menu,
                    menus::style_buttons,
                    menus::update_loading_status,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    settings::apply_settings,
                    notify::update_toasts,
                    notify::update_hud,
                ),
            )
            .add_systems(FixedUpdate, flow::process_ticks.after(crate::fixed_tick))
            .add_systems(
                PostUpdate,
                (flow::enforce_menu_cursor, settings::hide_subtitles_when_off),
            );
    }
}

/// A run started straight into a converted level (`--level`, `main.rs`):
/// the same level start as a menu load — the (in-memory) save session's
/// story level start, the snapshot (none: −1) and [`SnapshotLoaded`] for the
/// Kismet host — so such runs see the original's "save loaded" event too.
pub(crate) fn start_direct_level(
    game: &mut asamu_game::Game,
    saves: &mut Saves,
    play: &mut Play,
    loaded: &mut MessageWriter<SnapshotLoaded>,
) {
    let Some(map) = game.map_name().map(str::to_owned) else {
        return;
    };
    let (start, result, index) = flow::level_start(game, &mut saves.0, &map, PlayMode::Story);
    if let Err(e) = result {
        warn!("save failed: {e}");
    }
    if let Some(checkpoint_index) = index {
        loaded.write(SnapshotLoaded { checkpoint_index });
    }
    *play = Play {
        chapter: start.chapter,
        mode: PlayMode::Story,
        map: Some(map),
    };
}

/// `ASAMU_MENU_ACTION` (unattended checks of the menu flow): `new-game`,
/// `continue`, `chapter:<ChapterName>` or `time-trial:<ChapterName>`
/// (chapter names as in `ASAMULevels`, e.g. `chapter:Sanctuary`); a
/// confirmation it raises is answered with yes.
#[must_use]
pub(crate) fn parse_menu_action(value: &str) -> Option<menus::UiAction> {
    let value = value.trim();
    match value {
        "new-game" => return Some(menus::UiAction::NewGame),
        "continue" => return Some(menus::UiAction::Continue),
        _ => {}
    }
    let (kind, name) = value.split_once(':')?;
    let chapter = ChapterId::from_enum_name(name.trim())?;
    match kind {
        "chapter" => Some(menus::UiAction::StartChapter(chapter)),
        "time-trial" => Some(menus::UiAction::StartTimeTrial(chapter)),
        _ => None,
    }
}

/// Opens the saves and settings and shows the first screen.
#[allow(clippy::too_many_arguments)]
fn setup(
    launch: Res<UiLaunch>,
    mut saves: ResMut<Saves>,
    mut user: ResMut<UserSettings>,
    mut texts: ResMut<strings::UiStrings>,
    mut state: ResMut<UiState>,
    mut toasts: ResMut<notify::Toasts>,
    mut actions: MessageWriter<menus::UiAction>,
) {
    let root = SaveStore::default_root();
    *user = UserSettings::load(root.as_deref());
    if let Some(dir) = &launch.converted {
        *texts = strings::UiStrings::load(dir.root());
    }
    saves.0 = match (&root, launch.saves) {
        (Some(root), true) => {
            let session = SaveSession::open(SaveStore::new(root.join("saves")));
            info!("saves: {}", root.join("saves").display());
            for issue in session.issues() {
                warn!(
                    "{} save unreadable ({}){}",
                    issue.kind,
                    issue.error,
                    issue
                        .quarantined
                        .as_ref()
                        .map(|q| format!(", moved to {}", q.display()))
                        .unwrap_or_default()
                );
                toasts.push(format!(
                    "The {} save could not be read and was set aside",
                    issue.kind
                ));
            }
            session
        }
        (None, true) => {
            warn!("no user data directory (set ASAMU_SAVE_DIR): saves stay in memory");
            SaveSession::in_memory()
        }
        (_, false) => SaveSession::in_memory(),
    };
    if launch.show_menu {
        state.open(Screen::Main);
        if let Ok(value) = std::env::var("ASAMU_MENU_ACTION") {
            match parse_menu_action(&value) {
                Some(action) => {
                    info!("ASAMU_MENU_ACTION: {action:?}");
                    actions.write(action);
                    // Answers "replace the Continue point?" with yes (a
                    // no-op when nothing asks).
                    actions.write(menus::UiAction::Confirm);
                }
                None => warn!("ASAMU_MENU_ACTION {value:?} not understood"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_prefers_the_front_end_then_story_order() {
        let levels = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            menu_backdrop(&levels(&["AG-IceCave", "asamufrontendmap"])).as_deref(),
            Some("asamufrontendmap")
        );
        assert_eq!(
            menu_backdrop(&levels(&["AG-IceCave", "AG-ParadiseCave", "Zzz"])).as_deref(),
            Some("AG-ParadiseCave")
        );
        assert_eq!(menu_backdrop(&levels(&["Zzz"])).as_deref(), Some("Zzz"));
        assert_eq!(menu_backdrop(&[]), None);
    }

    #[test]
    fn menu_actions_parse() {
        assert_eq!(
            parse_menu_action("new-game"),
            Some(menus::UiAction::NewGame)
        );
        assert_eq!(
            parse_menu_action(" continue "),
            Some(menus::UiAction::Continue)
        );
        assert_eq!(
            parse_menu_action("chapter:Sanctuary"),
            Some(menus::UiAction::StartChapter(ChapterId::Sanctuary))
        );
        assert_eq!(
            parse_menu_action("time-trial:icecave"),
            Some(menus::UiAction::StartTimeTrial(ChapterId::IceCave))
        );
        assert_eq!(parse_menu_action("chapter:Mars"), None);
        assert_eq!(parse_menu_action("quit"), None);
    }

    #[test]
    fn launch_finds_levels_case_insensitively() {
        let launch = UiLaunch {
            levels: vec!["AG-Darkcave".into()],
            ..UiLaunch::default()
        };
        assert_eq!(launch.level_name("AG-DarkCave"), Some("AG-Darkcave"));
        assert!(launch.has_chapter(ChapterId::DarkCave));
        assert!(!launch.has_chapter(ChapterId::Workshop));
    }
}
