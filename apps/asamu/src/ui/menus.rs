//! The screens. Each screen is described by a list of [`Item`]s built from
//! the current state ([`screen_items`], pure and unit-tested) and spawned as
//! one UI tree under a [`MenuRoot`] whenever the state changes. Buttons are
//! `bevy_ui_widgets` buttons; their `Activate` events become [`UiAction`]
//! messages, handled in [`handle_actions`].
//!
//! Behaviour follows the original's menu rules (SAVE.md §5–6): Continue only
//! with a legible save and a chapter pointer, chapter select lists the
//! chapters entered (Workshop always), New Game and chapter select replace
//! the checkpoint snapshot after a confirmation when a Continue point
//! exists, time trial unlocks with the finished flag, restart from
//! checkpoint is unavailable in Workshop and Epilogue (the original's
//! quick-load rule). The look is ours.

use asamu_game::GameState;
use asamu_game::save::{
    ChapterId, Medal, PlayMode, SaveSession, Settings, TIME_TRIAL_TARGETS, TimeTrialTimes,
    format_trial_time,
};
use bevy::input_focus::InputFocus;
use bevy::input_focus::tab_navigation::{TabGroup, TabIndex};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::{InteractionDisabled, Pressed};
use bevy::ui_widgets::{Activate, Button};

use super::flow::{FlowRequest, LevelLoad};
use super::notify::TimeTrialClock;
use super::settings::{self, UserSettings};
use super::strings::UiStrings;
use super::{Play, Saves, Screen, UiLaunch, UiState};
use crate::Sim;
use crate::converted::ConvertedLevel;

/// What a button does.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub(crate) enum UiAction {
    /// Main menu: continue from the save.
    Continue,
    /// Main menu: new game (graybox: play the test level).
    NewGame,
    /// Open chapter select.
    OpenChapters,
    /// Start a chapter.
    StartChapter(ChapterId),
    /// Open time-trial select.
    OpenTimeTrial,
    /// Start a time trial.
    StartTimeTrial(ChapterId),
    /// Open the settings.
    OpenSettings,
    /// Back / cancel.
    Back,
    /// Quit the program.
    Quit,
    /// Close the menu and continue playing.
    Resume,
    /// Pause menu: back to the latest checkpoint.
    RestartCheckpoint,
    /// Open the main menu.
    MainMenu,
    /// Confirmation: yes.
    Confirm,
    /// Stop loading.
    CancelLoad,
    /// Change a setting.
    Set(SettingAction),
    /// Next (`true`) / previous language (handled by `ui::locale`).
    Language(bool),
}

/// A settings change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingAction {
    /// FOV ± 5°.
    Fov(i32),
    /// Mouse sensitivity ± 0.1.
    Sensitivity(i32),
    /// Toggle invert mouse.
    Invert,
    /// Volume ± 5 %.
    Volume(VolumeKind, i32),
    /// Toggle fullscreen.
    Fullscreen,
    /// Next (`true`) / previous window size.
    Resolution(bool),
    /// Toggle subtitles.
    Subtitles,
    /// Reset every setting.
    Defaults,
}

/// A volume slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VolumeKind {
    /// Master.
    Master,
    /// Music.
    Music,
    /// Effects.
    Sfx,
    /// Voice.
    Voice,
}

/// Root of the open screen's UI tree.
#[derive(Component)]
pub(crate) struct MenuRoot;

/// A menu button and its action.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct MenuButton(pub UiAction);

/// The live status line of the loading screen.
#[derive(Component)]
pub(crate) struct LoadingStatus;

/// One element of a screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// Heading.
    Title(String),
    /// A line of text.
    Line(String),
    /// A highlighted line (errors, notices).
    Notice(String),
    /// A full-width button.
    Button {
        /// Label.
        label: String,
        /// Action.
        action: UiAction,
        /// Clickable.
        enabled: bool,
    },
    /// A setting with − / + buttons.
    Stepper {
        /// Label.
        label: String,
        /// Current value text.
        value: String,
        /// Decrease.
        minus: UiAction,
        /// Increase.
        plus: UiAction,
    },
    /// An on/off setting.
    Toggle {
        /// Label.
        label: String,
        /// State.
        on: bool,
        /// Toggle action.
        action: UiAction,
    },
    /// The loading screen's live status line.
    LoadingStatus,
}

fn button(label: impl Into<String>, action: UiAction, enabled: bool) -> Item {
    Item::Button {
        label: label.into(),
        action,
        enabled,
    }
}

/// Everything a screen shows, gathered from the resources.
pub(crate) struct MenuContext<'a> {
    pub screen: Screen,
    pub saves: &'a SaveSession,
    pub launch: &'a UiLaunch,
    pub settings: &'a Settings,
    pub strings: &'a UiStrings,
    pub play: &'a Play,
    /// A paused game exists (the main menu offers "Return to game").
    pub in_game: bool,
    pub message: Option<&'a str>,
    /// Where settings are stored (shown on the settings screen).
    pub settings_path: Option<String>,
    /// Name of the level being loaded.
    pub loading: Option<&'a str>,
    /// The chosen language's name (`None`: no choice, the row is hidden).
    pub language: Option<String>,
}

fn medal_name(m: Option<Medal>) -> &'static str {
    match m {
        Some(Medal::Gold) => "gold",
        Some(Medal::Silver) => "silver",
        Some(Medal::Bronze) => "bronze",
        None => "no medal",
    }
}

fn percent_text(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

/// The items of the open screen.
#[must_use]
pub(crate) fn screen_items(ctx: &MenuContext<'_>) -> Vec<Item> {
    let t = ctx.strings;
    let converted = ctx.launch.converted.is_some();
    let saves = ctx.saves;
    let mut items = Vec::new();
    match ctx.screen {
        Screen::None => return items,
        Screen::Main => {
            items.push(Item::Title("ASAMU-decomp".into()));
            items.push(Item::Line(
                "engine recreation \u{b7} plays data converted from your own install".into(),
            ));
            if ctx.in_game {
                items.push(button(
                    t.get("menu.return", "Return to game"),
                    UiAction::Resume,
                    true,
                ));
            }
            let target = saves
                .continue_target()
                .filter(|c| converted && ctx.launch.has_chapter(*c));
            let continue_label = match target {
                Some(c) => format!(
                    "{} \u{2014} {}",
                    t.get("menu.continue", "Continue"),
                    t.chapter_title(c)
                ),
                None => t.get("menu.continue", "Continue").to_owned(),
            };
            items.push(button(continue_label, UiAction::Continue, target.is_some()));
            let new_game = !converted || ctx.launch.has_chapter(ChapterId::Workshop);
            items.push(button(
                t.get("menu.new_game", "New Game"),
                UiAction::NewGame,
                new_game,
            ));
            items.push(button(
                t.get("menu.chapters", "Chapter select"),
                UiAction::OpenChapters,
                converted,
            ));
            items.push(button(
                t.get("menu.time_trial", "Time trial"),
                UiAction::OpenTimeTrial,
                converted && saves.progression.time_trial_unlocked(),
            ));
            items.push(button(
                t.get("menu.settings", "Settings"),
                UiAction::OpenSettings,
                true,
            ));
            items.push(button(t.get("menu.quit", "Quit"), UiAction::Quit, true));
            let p = &saves.progression;
            items.push(Item::Line(format!(
                "chapters entered {}/{} \u{b7} collectibles {}/{} \u{b7} story items {}/{} \u{b7} achievements {}/{}",
                ChapterId::ALL
                    .iter()
                    .filter(|c| p.unlocked.contains(c))
                    .count(),
                ChapterId::ALL.len(),
                p.collectible_total(),
                asamu_game::save::TOTAL_COLLECTIBLES,
                p.story_items.len(),
                asamu_game::save::TOTAL_STORY_ITEMS,
                p.achievements.len(),
                asamu_game::save::Achievement::ALL.len(),
            )));
            if !converted {
                items.push(Item::Line(
                    "No converted data: New Game plays the hand-made graybox test level. Convert \
                     your own install with asamu-import and start with --converted to play the \
                     chapters."
                        .into(),
                ));
            } else if !ctx.launch.saves {
                items.push(Item::Line(
                    "Saves are kept in memory for this run (started with --level).".into(),
                ));
            } else if !ctx.launch.has_chapter(ChapterId::Workshop) {
                items.push(Item::Line(
                    "AG-Workshop is not converted: New Game needs it (asamu-import levels --map \
                     AG-Workshop)."
                        .into(),
                ));
            }
        }
        Screen::Chapters => {
            items.push(Item::Title(t.get("menu.chapters", "Chapter select").into()));
            for (n, c) in ChapterId::ALL.into_iter().enumerate() {
                let unlocked = saves.progression.is_unlocked(c);
                let available = ctx.launch.has_chapter(c);
                let mut label = format!("{}. {}", n + 1, t.chapter_title(c));
                if c.has_collectibles() {
                    label.push_str(&format!(
                        "  \u{b7}  {}/{}",
                        saves.progression.collectible_count(c),
                        asamu_game::save::COLLECTIBLES_PER_CHAPTER
                    ));
                }
                if !unlocked {
                    label.push_str("  (locked)");
                } else if !available {
                    label.push_str("  (not converted)");
                }
                items.push(button(
                    label,
                    UiAction::StartChapter(c),
                    unlocked && available,
                ));
            }
            items.push(Item::Line(
                "Starting a chapter replaces the Continue point; unlocked chapters, collectibles \
                 and achievements are kept."
                    .into(),
            ));
            items.push(button(t.get("menu.back", "Back"), UiAction::Back, true));
        }
        Screen::TimeTrial => {
            items.push(Item::Title(t.get("menu.time_trial", "Time trial").into()));
            for (c, [gold, ..]) in ChapterId::WITH_COLLECTIBLES
                .into_iter()
                .zip(TIME_TRIAL_TARGETS)
            {
                let best = saves.time_trial.best.get(&c).copied();
                let label = format!(
                    "{}  \u{b7}  best {}  ({})  \u{b7}  gold {}",
                    t.chapter_title(c),
                    best.map_or_else(
                        || "--:--:--".to_owned(),
                        |b| format_trial_time(f64::from(b))
                    ),
                    medal_name(best.and_then(|b| TimeTrialTimes::medal_for(c, b))),
                    format_trial_time(f64::from(gold)),
                );
                items.push(button(
                    label,
                    UiAction::StartTimeTrial(c),
                    ctx.launch.has_chapter(c),
                ));
            }
            items.push(button(t.get("menu.back", "Back"), UiAction::Back, true));
        }
        Screen::Settings => {
            let s = ctx.settings;
            items.push(Item::Title(t.get("menu.settings", "Settings").into()));
            if let Some(name) = &ctx.language {
                items.push(Item::Stepper {
                    label: t.get("menu.language", "Language").into(),
                    value: name.clone(),
                    minus: UiAction::Language(false),
                    plus: UiAction::Language(true),
                });
            }
            items.push(Item::Stepper {
                label: t.get("settings.fov", "Field of view").into(),
                value: format!("{:.0}\u{b0}", s.fov_degrees),
                minus: UiAction::Set(SettingAction::Fov(-1)),
                plus: UiAction::Set(SettingAction::Fov(1)),
            });
            items.push(Item::Stepper {
                label: t
                    .get("settings.mouse_sensitivity", "Mouse sensitivity")
                    .into(),
                value: format!("{:.1}\u{d7}", s.mouse_sensitivity),
                minus: UiAction::Set(SettingAction::Sensitivity(-1)),
                plus: UiAction::Set(SettingAction::Sensitivity(1)),
            });
            items.push(Item::Toggle {
                label: t.get("settings.invert_mouse", "Invert mouse").into(),
                on: s.invert_mouse,
                action: UiAction::Set(SettingAction::Invert),
            });
            for (key, label, kind, v) in [
                (
                    "settings.master_volume",
                    "Master volume",
                    VolumeKind::Master,
                    s.master_volume,
                ),
                (
                    "settings.music_volume",
                    "Music volume",
                    VolumeKind::Music,
                    s.music_volume,
                ),
                (
                    "settings.sfx_volume",
                    "Effects volume",
                    VolumeKind::Sfx,
                    s.sfx_volume,
                ),
                (
                    "settings.voice_volume",
                    "Voice volume",
                    VolumeKind::Voice,
                    s.voice_volume,
                ),
            ] {
                items.push(Item::Stepper {
                    label: t.get(key, label).into(),
                    value: percent_text(v),
                    minus: UiAction::Set(SettingAction::Volume(kind, -1)),
                    plus: UiAction::Set(SettingAction::Volume(kind, 1)),
                });
            }
            items.push(Item::Toggle {
                label: t.get("settings.fullscreen", "Fullscreen").into(),
                on: s.fullscreen,
                action: UiAction::Set(SettingAction::Fullscreen),
            });
            items.push(Item::Stepper {
                label: t.get("settings.resolution", "Window size").into(),
                value: s
                    .resolution
                    .map_or_else(|| "default".to_owned(), |[w, h]| format!("{w}\u{d7}{h}")),
                minus: UiAction::Set(SettingAction::Resolution(false)),
                plus: UiAction::Set(SettingAction::Resolution(true)),
            });
            items.push(Item::Toggle {
                label: t.get("settings.subtitles", "Subtitles").into(),
                on: s.subtitles,
                action: UiAction::Set(SettingAction::Subtitles),
            });
            items.push(button(
                "Reset to defaults",
                UiAction::Set(SettingAction::Defaults),
                true,
            ));
            items.push(button(t.get("menu.back", "Back"), UiAction::Back, true));
            if let Some(path) = &ctx.settings_path {
                items.push(Item::Line(format!("saved in {path}")));
            }
        }
        Screen::Pause => {
            items.push(Item::Title(t.get("menu.paused", "Paused").into()));
            if let Some(c) = ctx.play.chapter {
                let mut line = t.chapter_title(c);
                if ctx.play.mode == PlayMode::TimeTrial {
                    line.push_str(" \u{b7} time trial");
                }
                items.push(Item::Line(line));
            }
            if let Some(line) = super::notify::counter_line_for(saves, ctx.play) {
                items.push(Item::Line(line));
            }
            items.push(button(
                t.get("menu.resume", "Resume"),
                UiAction::Resume,
                true,
            ));
            let restart = !matches!(
                ctx.play.chapter,
                Some(ChapterId::Workshop | ChapterId::Epilogue)
            );
            items.push(button(
                t.get("menu.restart", "Restart from checkpoint"),
                UiAction::RestartCheckpoint,
                restart,
            ));
            items.push(button(
                t.get("menu.settings", "Settings"),
                UiAction::OpenSettings,
                true,
            ));
            items.push(button(
                t.get("menu.main_menu", "Main menu"),
                UiAction::MainMenu,
                true,
            ));
        }
        Screen::Loading => {
            items.push(Item::Title(t.get("menu.loading", "Loading").into()));
            if let Some(map) = ctx.loading {
                let name = asamu_game::save::chapter_of_map(map, None)
                    .map_or_else(|| map.to_owned(), |c| t.chapter_title(c));
                items.push(Item::Line(name));
            }
            items.push(Item::LoadingStatus);
            items.push(button(
                t.get("menu.cancel", "Cancel"),
                UiAction::CancelLoad,
                true,
            ));
        }
        Screen::Confirm => {
            items.push(Item::Title(
                t.get("menu.confirm_title", "Start over?").into(),
            ));
            items.push(Item::Line(
                t.get(
                    "menu.confirm_text",
                    "This replaces your Continue point (the latest checkpoint). Unlocked \
                     chapters, collectibles, achievements and time-trial times are kept.",
                )
                .into(),
            ));
            items.push(button(t.get("menu.yes", "Yes"), UiAction::Confirm, true));
            items.push(button(t.get("menu.no", "No"), UiAction::Back, true));
        }
    }
    if let Some(m) = ctx.message {
        items.push(Item::Notice(m.to_owned()));
    }
    items
}

const PANEL: Color = Color::srgba(0.07, 0.08, 0.11, 0.94);
const BACKDROP: Color = Color::srgba(0.01, 0.015, 0.03, 0.62);
const BUTTON: Color = Color::srgb(0.16, 0.19, 0.24);
const BUTTON_HOVER: Color = Color::srgb(0.25, 0.31, 0.40);
const BUTTON_PRESSED: Color = Color::srgb(0.33, 0.47, 0.62);
const BUTTON_FOCUS: Color = Color::srgb(0.22, 0.27, 0.35);
const BUTTON_DISABLED: Color = Color::srgba(0.12, 0.13, 0.15, 0.7);
const TEXT: Color = Color::srgb(0.94, 0.94, 0.92);
const TEXT_DIM: Color = Color::srgb(0.62, 0.64, 0.68);
const TEXT_NOTICE: Color = Color::srgb(1.0, 0.82, 0.45);

fn text(s: &str, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(s),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
    )
}

fn spawn_button(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: UiAction,
    enabled: bool,
    width: Val,
    tab: &mut i32,
) {
    let mut e = parent.spawn((
        Button,
        Hovered::default(),
        MenuButton(action),
        Node {
            width,
            min_height: px(38),
            padding: UiRect::axes(px(14), px(6)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(if enabled { BUTTON } else { BUTTON_DISABLED }),
        children![text(label, 18.0, if enabled { TEXT } else { TEXT_DIM })],
    ));
    if enabled {
        e.insert(TabIndex(*tab));
        *tab += 1;
    } else {
        e.insert(InteractionDisabled);
    }
}

fn spawn_item(parent: &mut ChildSpawnerCommands, item: &Item, tab: &mut i32) {
    match item {
        Item::Title(s) => {
            parent.spawn((
                text(s, 32.0, TEXT),
                Node {
                    margin: UiRect::bottom(px(6)),
                    ..default()
                },
            ));
        }
        Item::Line(s) => {
            parent.spawn(text(s, 15.0, TEXT_DIM));
        }
        Item::Notice(s) => {
            parent.spawn(text(s, 16.0, TEXT_NOTICE));
        }
        Item::LoadingStatus => {
            parent.spawn((LoadingStatus, text("", 15.0, TEXT_DIM)));
        }
        Item::Button {
            label,
            action,
            enabled,
        } => spawn_button(parent, label, *action, *enabled, percent(100), tab),
        Item::Stepper {
            label,
            value,
            minus,
            plus,
        } => {
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px(8),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        text(label, 17.0, TEXT),
                        Node {
                            width: px(200),
                            ..default()
                        },
                    ));
                    spawn_button(row, "\u{2212}", *minus, true, px(44), tab);
                    row.spawn((
                        text(value, 17.0, TEXT),
                        Node {
                            width: px(120),
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                    ));
                    spawn_button(row, "+", *plus, true, px(44), tab);
                });
        }
        Item::Toggle { label, on, action } => {
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px(8),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        text(label, 17.0, TEXT),
                        Node {
                            width: px(200),
                            ..default()
                        },
                    ));
                    spawn_button(
                        row,
                        if *on { "On" } else { "Off" },
                        *action,
                        true,
                        px(96),
                        tab,
                    );
                });
        }
    }
}

/// Activate → [`UiAction`].
pub(crate) fn on_activate(
    ev: On<Activate>,
    buttons: Query<(&MenuButton, Has<InteractionDisabled>)>,
    mut out: MessageWriter<UiAction>,
) {
    if let Ok((b, disabled)) = buttons.get(ev.entity)
        && !disabled
    {
        out.write(b.0);
    }
}

/// Rebuilds the open screen when the state changed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rebuild_menu(
    mut commands: Commands,
    mut state: ResMut<UiState>,
    roots: Query<Entity, With<MenuRoot>>,
    saves: Res<Saves>,
    launch: Res<UiLaunch>,
    user: Res<UserSettings>,
    strings: Res<UiStrings>,
    play: Res<Play>,
    sim: Option<Res<Sim>>,
    load: Res<LevelLoad>,
    mut focus: ResMut<InputFocus>,
    locale: Res<super::locale::Locale>,
) {
    if !state.dirty {
        return;
    }
    state.dirty = false;
    for root in &roots {
        commands.entity(root).despawn();
    }
    focus.clear();
    let in_game = sim
        .as_ref()
        .is_some_and(|s| s.game.state() == GameState::Paused);
    let settings_path = asamu_game::save::SaveStore::default_root()
        .map(|r| r.join("settings.json").display().to_string());
    let ctx = MenuContext {
        screen: state.screen,
        saves: &saves.0,
        launch: &launch,
        settings: &user.settings,
        strings: &strings,
        play: &play,
        in_game,
        message: state.message.as_deref(),
        settings_path,
        loading: load.pending_map(),
        language: locale.display_name(),
    };
    let items = screen_items(&ctx);
    if items.is_empty() {
        return;
    }
    let mut tab = 0;
    commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BACKDROP),
            GlobalZIndex(100),
            TabGroup::default(),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(8),
                    padding: UiRect::all(px(24)),
                    min_width: px(420),
                    max_width: px(760),
                    border_radius: BorderRadius::all(px(10)),
                    ..default()
                },
                BackgroundColor(PANEL),
            ))
            .with_children(|panel| {
                for item in &items {
                    spawn_item(panel, item, &mut tab);
                }
            });
        });
}

/// What [`style_buttons`] reads and writes per button.
type ButtonLook = (
    Entity,
    &'static Hovered,
    Has<Pressed>,
    Has<InteractionDisabled>,
    &'static mut BackgroundColor,
);

/// Hover / press / focus colours.
pub(crate) fn style_buttons(
    focus: Res<InputFocus>,
    mut buttons: Query<ButtonLook, With<MenuButton>>,
) {
    for (e, hovered, pressed, disabled, mut bg) in &mut buttons {
        let c = if disabled {
            BUTTON_DISABLED
        } else if pressed {
            BUTTON_PRESSED
        } else if hovered.get() {
            BUTTON_HOVER
        } else if focus.get() == Some(e) {
            BUTTON_FOCUS
        } else {
            BUTTON
        };
        if bg.0 != c {
            bg.0 = c;
        }
    }
}

/// Live text of the loading screen.
pub(crate) fn update_loading_status(
    load: Res<LevelLoad>,
    level: Option<Res<ConvertedLevel>>,
    mut texts: Query<&mut Text, With<LoadingStatus>>,
) {
    if texts.is_empty() {
        return;
    }
    let render = level.map_or_else(String::new, |l| l.status());
    let line = format!(
        "{}\ngameplay: {}",
        render,
        if load.is_loading() {
            "loading collision and actors ..."
        } else {
            "ready"
        }
    );
    for mut t in &mut texts {
        if t.0 != line {
            t.0.clone_from(&line);
        }
    }
}

/// Esc goes back (pause: resumes); F8 restarts a running time trial (the
/// original's `TimeTrialRestart`).
pub(crate) fn menu_keys(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<UiState>,
    sim: Option<Res<Sim>>,
    play: Res<Play>,
    clock: Res<TimeTrialClock>,
    mut actions: MessageWriter<UiAction>,
    mut flow: MessageWriter<FlowRequest>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        let paused_game = sim
            .as_ref()
            .is_some_and(|s| s.game.state() == GameState::Paused);
        match state.screen {
            Screen::None | Screen::Loading => {}
            Screen::Main => {
                if paused_game {
                    actions.write(UiAction::Resume);
                }
            }
            Screen::Pause => {
                actions.write(UiAction::Resume);
            }
            Screen::Chapters | Screen::TimeTrial | Screen::Settings | Screen::Confirm => {
                actions.write(UiAction::Back);
            }
        }
    }
    if keys.just_pressed(KeyCode::F8)
        && state.screen == Screen::None
        && play.mode == PlayMode::TimeTrial
        && clock.running
        && let Some(map) = &play.map
    {
        flow.write(FlowRequest::Load {
            map: map.clone(),
            mode: PlayMode::TimeTrial,
        });
    }
}

/// Applies a settings change.
#[must_use]
pub(crate) fn apply_setting(s: Settings, change: SettingAction) -> Settings {
    let mut s = s;
    match change {
        SettingAction::Fov(d) => {
            s.fov_degrees = settings::step_value(s.fov_degrees, 5.0, d, Settings::FOV_RANGE);
        }
        SettingAction::Sensitivity(d) => {
            s.mouse_sensitivity =
                settings::step_value(s.mouse_sensitivity, 0.1, d, Settings::SENSITIVITY_RANGE);
        }
        SettingAction::Invert => s.invert_mouse = !s.invert_mouse,
        SettingAction::Volume(kind, d) => {
            let v = match kind {
                VolumeKind::Master => &mut s.master_volume,
                VolumeKind::Music => &mut s.music_volume,
                VolumeKind::Sfx => &mut s.sfx_volume,
                VolumeKind::Voice => &mut s.voice_volume,
            };
            *v = settings::step_value(*v, 0.05, d, [0.0, 1.0]);
        }
        SettingAction::Fullscreen => s.fullscreen = !s.fullscreen,
        SettingAction::Resolution(forward) => {
            s.resolution = settings::cycle_resolution(s.resolution, forward);
        }
        SettingAction::Subtitles => s.subtitles = !s.subtitles,
        SettingAction::Defaults => s = Settings::default(),
    }
    s.sanitized()
}

/// Handles the buttons' actions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_actions(
    mut actions: MessageReader<UiAction>,
    mut state: ResMut<UiState>,
    mut saves: ResMut<Saves>,
    launch: Res<UiLaunch>,
    mut user: ResMut<UserSettings>,
    mut flow: MessageWriter<FlowRequest>,
) {
    for action in actions.read().copied().collect::<Vec<_>>() {
        run_action(
            action, false, &mut state, &mut saves, &launch, &mut user, &mut flow,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_action(
    action: UiAction,
    confirmed: bool,
    state: &mut UiState,
    saves: &mut Saves,
    launch: &UiLaunch,
    user: &mut UserSettings,
    flow: &mut MessageWriter<FlowRequest>,
) {
    let converted = launch.converted.is_some();
    let needs_confirm = !confirmed && saves.0.can_continue();
    let ask = |state: &mut UiState, action: UiAction| {
        state.back = state.screen;
        state.confirm = Some(action);
        state.message = None;
        state.open(Screen::Confirm);
    };
    let report = |state: &mut UiState, result: Result<(), asamu_game::save::SaveError>| {
        if let Err(e) = result {
            warn!("save failed: {e}");
            state.message = Some(format!("Could not write the save: {e}"));
        }
    };
    match action {
        UiAction::Continue => {
            if let Some(chapter) = saves.0.continue_game()
                && converted
            {
                flow.write(FlowRequest::Load {
                    map: chapter.map_name().to_owned(),
                    mode: PlayMode::Story,
                });
            }
        }
        UiAction::NewGame => {
            if !converted {
                flow.write(FlowRequest::Graybox);
            } else if !launch.has_chapter(ChapterId::Workshop) {
                // Do not reset the Continue point for a map that cannot load.
                state.open_with(Screen::Main, "AG-Workshop is not converted");
            } else if needs_confirm {
                ask(state, action);
            } else {
                let result = saves.0.new_game().map(|_| ());
                report(state, result);
                flow.write(FlowRequest::Load {
                    map: asamu_game::save::ChapterId::Workshop.map_name().to_owned(),
                    mode: PlayMode::Story,
                });
            }
        }
        UiAction::StartChapter(chapter) => {
            if !launch.has_chapter(chapter) {
                state.open_with(
                    Screen::Chapters,
                    format!("{} is not converted", chapter.map_name()),
                );
            } else if !saves.0.progression.is_unlocked(chapter) {
                state.open_with(Screen::Chapters, "That chapter is locked");
            } else if needs_confirm {
                ask(state, action);
            } else {
                let result = saves.0.start_chapter(chapter).map(|_| ());
                report(state, result);
                flow.write(FlowRequest::Load {
                    map: chapter.map_name().to_owned(),
                    mode: PlayMode::Story,
                });
            }
        }
        UiAction::StartTimeTrial(chapter) => {
            // Convertedness first: a refused start must not leave the save
            // session in time-trial mode (it would drop the story's saves if
            // a paused story game is resumed).
            if launch.has_chapter(chapter) && saves.0.start_time_trial(chapter) {
                flow.write(FlowRequest::Load {
                    map: chapter.map_name().to_owned(),
                    mode: PlayMode::TimeTrial,
                });
            } else {
                state.open_with(Screen::TimeTrial, "Time trial is not available");
            }
        }
        UiAction::OpenChapters => {
            state.message = None;
            state.open(Screen::Chapters);
        }
        UiAction::OpenTimeTrial => {
            state.message = None;
            state.open(Screen::TimeTrial);
        }
        UiAction::OpenSettings => {
            state.back = state.screen;
            state.message = None;
            state.open(Screen::Settings);
        }
        UiAction::MainMenu => {
            state.message = None;
            state.open(Screen::Main);
        }
        UiAction::Back => {
            let to = match state.screen {
                Screen::Settings | Screen::Confirm => state.back,
                _ => Screen::Main,
            };
            state.confirm = None;
            state.message = None;
            state.open(if to == Screen::None { Screen::Main } else { to });
        }
        UiAction::Confirm => {
            if let Some(pending) = state.confirm.take() {
                state.screen = state.back;
                run_action(pending, true, state, saves, launch, user, flow);
            }
        }
        UiAction::Quit => {
            flow.write(FlowRequest::Quit);
        }
        UiAction::Resume => {
            flow.write(FlowRequest::Resume);
        }
        UiAction::RestartCheckpoint => {
            flow.write(FlowRequest::Restart);
        }
        UiAction::CancelLoad => {
            flow.write(FlowRequest::CancelLoad);
        }
        UiAction::Set(change) => {
            let next = apply_setting(user.settings, change);
            user.set(next);
            state.dirty = true;
        }
        // `ui::locale::handle_language_actions` changes the language; the
        // menu rebuilds when it is applied.
        UiAction::Language(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::ConvertedDir;

    fn ctx<'a>(
        screen: Screen,
        saves: &'a SaveSession,
        launch: &'a UiLaunch,
        settings: &'a Settings,
        strings: &'a UiStrings,
        play: &'a Play,
    ) -> MenuContext<'a> {
        MenuContext {
            screen,
            saves,
            launch,
            settings,
            strings,
            play,
            in_game: false,
            message: None,
            settings_path: None,
            loading: None,
            language: None,
        }
    }

    fn buttons(items: &[Item]) -> Vec<(String, UiAction, bool)> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Button {
                    label,
                    action,
                    enabled,
                } => Some((label.clone(), *action, *enabled)),
                _ => None,
            })
            .collect()
    }

    fn converted_launch(levels: &[&str]) -> UiLaunch {
        UiLaunch {
            converted: Some(ConvertedDir::new("/conv")),
            levels: levels.iter().map(|s| (*s).to_owned()).collect(),
            show_menu: true,
            saves: true,
        }
    }

    #[test]
    fn graybox_main_menu_offers_new_game_but_no_chapters() {
        let saves = SaveSession::in_memory();
        let launch = UiLaunch::default();
        let (s, t, p) = (Settings::default(), UiStrings::default(), Play::default());
        let items = screen_items(&ctx(Screen::Main, &saves, &launch, &s, &t, &p));
        let b = buttons(&items);
        let get = |a: UiAction| b.iter().find(|x| x.1 == a).map(|x| x.2);
        assert_eq!(get(UiAction::Continue), Some(false));
        assert_eq!(get(UiAction::NewGame), Some(true));
        assert_eq!(get(UiAction::OpenChapters), Some(false));
        assert_eq!(get(UiAction::OpenTimeTrial), Some(false));
        assert_eq!(get(UiAction::OpenSettings), Some(true));
        assert_eq!(get(UiAction::Quit), Some(true));
        assert_eq!(get(UiAction::Resume), None, "no game in progress");
    }

    #[test]
    fn converted_main_menu_follows_the_save() {
        let mut saves = SaveSession::in_memory();
        let launch = converted_launch(&["AG-Workshop", "AG-ParadiseCave"]);
        let (s, t, p) = (Settings::default(), UiStrings::default(), Play::default());
        // In-memory sessions are not legible until a new game starts.
        saves.new_game().unwrap();
        saves
            .begin_level("AG-ParadiseCave", Some("Sanctuary"))
            .unwrap();
        let items = screen_items(&ctx(Screen::Main, &saves, &launch, &s, &t, &p));
        let b = buttons(&items);
        let cont = b.iter().find(|x| x.1 == UiAction::Continue).unwrap();
        assert!(cont.2);
        assert!(cont.0.contains("Sanctuary"), "{}", cont.0);
        // Time trial unlocks with the finished flag.
        assert!(!b.iter().find(|x| x.1 == UiAction::OpenTimeTrial).unwrap().2);
        saves.on_game_finished().unwrap();
        let b = buttons(&screen_items(&ctx(
            Screen::Main,
            &saves,
            &launch,
            &s,
            &t,
            &p,
        )));
        assert!(b.iter().find(|x| x.1 == UiAction::OpenTimeTrial).unwrap().2);
        // Continue is off when its chapter is not converted.
        let launch = converted_launch(&["AG-Workshop"]);
        let b = buttons(&screen_items(&ctx(
            Screen::Main,
            &saves,
            &launch,
            &s,
            &t,
            &p,
        )));
        assert!(!b.iter().find(|x| x.1 == UiAction::Continue).unwrap().2);
    }

    #[test]
    fn chapter_select_lists_entered_and_converted_chapters() {
        let mut saves = SaveSession::in_memory();
        saves.begin_level("AG-ParadiseCave", None).unwrap();
        saves.begin_level("AG-IceCave", None).unwrap();
        let launch = converted_launch(&["AG-Workshop", "AG-ParadiseCave"]);
        let (s, t, p) = (Settings::default(), UiStrings::default(), Play::default());
        let b = buttons(&screen_items(&ctx(
            Screen::Chapters,
            &saves,
            &launch,
            &s,
            &t,
            &p,
        )));
        let state = |c: ChapterId| b.iter().find(|x| x.1 == UiAction::StartChapter(c)).unwrap();
        assert!(state(ChapterId::Workshop).2, "always unlocked");
        assert!(state(ChapterId::Sanctuary).2);
        assert!(!state(ChapterId::IceCave).2);
        assert!(state(ChapterId::IceCave).0.contains("not converted"));
        assert!(!state(ChapterId::Village).2);
        assert!(state(ChapterId::Village).0.contains("locked"));
        assert!(state(ChapterId::Sanctuary).0.contains("0/5"));
    }

    #[test]
    fn pause_menu_disables_restart_in_workshop_and_epilogue() {
        let saves = SaveSession::in_memory();
        let launch = converted_launch(&[]);
        let (s, t) = (Settings::default(), UiStrings::default());
        for (chapter, enabled) in [
            (ChapterId::Workshop, false),
            (ChapterId::Epilogue, false),
            (ChapterId::DarkCave, true),
        ] {
            let p = Play {
                chapter: Some(chapter),
                ..Play::default()
            };
            let b = buttons(&screen_items(&ctx(
                Screen::Pause,
                &saves,
                &launch,
                &s,
                &t,
                &p,
            )));
            let r = b
                .iter()
                .find(|x| x.1 == UiAction::RestartCheckpoint)
                .unwrap();
            assert_eq!(r.2, enabled, "{chapter:?}");
        }
    }

    #[test]
    fn settings_actions_step_and_clamp() {
        let s = Settings::default();
        let s = apply_setting(s, SettingAction::Fov(1));
        assert_eq!(s.fov_degrees, 95.0);
        let mut v = s;
        for _ in 0..10 {
            v = apply_setting(v, SettingAction::Volume(VolumeKind::Music, 1));
        }
        assert_eq!(v.music_volume, 1.0);
        let v = apply_setting(v, SettingAction::Invert);
        assert!(v.invert_mouse);
        let v = apply_setting(v, SettingAction::Resolution(true));
        assert_eq!(v.resolution, Some([1280, 720]));
        assert_eq!(
            apply_setting(v, SettingAction::Defaults),
            Settings::default()
        );
        let items = screen_items(&ctx(
            Screen::Settings,
            &SaveSession::in_memory(),
            &UiLaunch::default(),
            &v,
            &UiStrings::default(),
            &Play::default(),
        ));
        assert!(
            items.iter().any(
                |i| matches!(i, Item::Toggle { label, on: true, .. } if label == "Invert mouse")
            )
        );
    }

    #[test]
    fn settings_offer_the_language_and_screens_use_localized_labels() {
        let saves = SaveSession::in_memory();
        let launch = UiLaunch::default();
        let (s, p) = (Settings::default(), Play::default());
        let mut t = UiStrings::default();
        t.set_localized(
            [
                ("menu.language", "Tongue"),
                ("settings.fov", "View angle"),
                ("menu.yes", "Aye"),
                ("menu.confirm_title", "Really?"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        );
        let mut c = ctx(Screen::Settings, &saves, &launch, &s, &t, &p);
        let items = screen_items(&c);
        assert!(
            !items.iter().any(|i| matches!(
                i,
                Item::Stepper {
                    minus: UiAction::Language(_),
                    ..
                }
            )),
            "no language row without a choice"
        );
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Stepper { label, .. } if label == "View angle"))
        );
        c.language = Some("Tst-own".into());
        let items = screen_items(&c);
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Stepper { label, value, minus: UiAction::Language(false), plus: UiAction::Language(true) }
                if label == "Tongue" && value == "Tst-own"
        )));
        let confirm = screen_items(&ctx(Screen::Confirm, &saves, &launch, &s, &t, &p));
        assert_eq!(confirm[0], Item::Title("Really?".into()));
        let b = buttons(&confirm);
        assert_eq!(b[0].0, "Aye");
        assert_eq!(b[1].0, "No");
    }

    #[test]
    fn activating_a_button_writes_its_action_unless_disabled() {
        let mut app = App::new();
        app.add_message::<UiAction>().add_observer(on_activate);
        let quit = app.world_mut().spawn(MenuButton(UiAction::Quit)).id();
        let off = app
            .world_mut()
            .spawn((MenuButton(UiAction::NewGame), InteractionDisabled))
            .id();
        app.world_mut().trigger(Activate { entity: quit });
        app.world_mut().trigger(Activate { entity: off });
        let written: Vec<UiAction> = app
            .world()
            .resource::<Messages<UiAction>>()
            .iter_current_update_messages()
            .copied()
            .collect();
        assert_eq!(written, vec![UiAction::Quit]);
    }

    /// Runs `actions` through [`handle_actions`]; returns the flow requests.
    fn run_actions(app: &mut App, actions: &[UiAction]) -> Vec<FlowRequest> {
        for a in actions {
            app.world_mut().write_message(*a);
        }
        app.update();
        app.world()
            .resource::<Messages<FlowRequest>>()
            .iter_current_update_messages()
            .cloned()
            .collect()
    }

    fn menu_app(saves: SaveSession, launch: UiLaunch) -> App {
        let mut app = App::new();
        app.add_message::<UiAction>()
            .add_message::<FlowRequest>()
            .insert_resource(Saves(saves))
            .insert_resource(launch)
            .init_resource::<UiState>()
            .init_resource::<UserSettings>()
            .add_systems(Update, handle_actions);
        app
    }

    #[test]
    fn refused_time_trial_leaves_the_session_in_story_mode() {
        let mut saves = SaveSession::in_memory();
        saves.on_game_finished().unwrap();
        let mut app = menu_app(saves, converted_launch(&["AG-Workshop"]));
        let requests = run_actions(&mut app, &[UiAction::StartTimeTrial(ChapterId::Village)]);
        assert!(requests.is_empty());
        assert_eq!(app.world().resource::<Saves>().0.mode(), PlayMode::Story);
        assert_eq!(app.world().resource::<UiState>().screen, Screen::TimeTrial);
    }

    #[test]
    fn new_game_and_chapter_select_confirm_before_replacing_a_continue_point() {
        let mut saves = SaveSession::in_memory();
        saves.new_game().unwrap();
        saves
            .begin_level("AG-ParadiseCave", Some("Sanctuary"))
            .unwrap();
        let mut snap = asamu_game::save::Snapshot::fresh();
        snap.map = "AG-ParadiseCave".into();
        snap.checkpoints.insert(ChapterId::Sanctuary, 3);
        saves.on_checkpoint_saved(snap.clone()).unwrap();
        let launch = converted_launch(&["AG-Workshop", "AG-ParadiseCave"]);
        let mut app = menu_app(saves, launch);
        // New Game asks first; "No" keeps everything.
        assert!(run_actions(&mut app, &[UiAction::NewGame]).is_empty());
        assert_eq!(app.world().resource::<UiState>().screen, Screen::Confirm);
        assert!(run_actions(&mut app, &[UiAction::Back]).is_empty());
        assert_eq!(app.world().resource::<UiState>().screen, Screen::Main);
        assert_eq!(
            app.world().resource::<Saves>().0.snapshot.as_ref(),
            Some(&snap)
        );
        // Chapter select + yes: fresh snapshot, pointer kept until the map
        // starts, the chapter loads.
        let requests = run_actions(
            &mut app,
            &[
                UiAction::StartChapter(ChapterId::Sanctuary),
                UiAction::Confirm,
            ],
        );
        assert_eq!(
            requests,
            vec![FlowRequest::Load {
                map: "AG-ParadiseCave".into(),
                mode: PlayMode::Story
            }]
        );
        let s = &app.world().resource::<Saves>().0;
        assert_eq!(s.snapshot, Some(asamu_game::save::Snapshot::fresh()));
        assert_eq!(s.general.current, Some(ChapterId::Sanctuary));
        // Continue: no confirmation, opens the pointer's chapter.
        let requests = run_actions(&mut app, &[UiAction::Continue]);
        assert_eq!(
            requests,
            vec![FlowRequest::Load {
                map: "AG-ParadiseCave".into(),
                mode: PlayMode::Story
            }]
        );
        // New Game + yes: pointer cleared, Workshop loads.
        let requests = run_actions(&mut app, &[UiAction::NewGame, UiAction::Confirm]);
        assert_eq!(
            requests,
            vec![FlowRequest::Load {
                map: "AG-Workshop".into(),
                mode: PlayMode::Story
            }]
        );
        assert_eq!(app.world().resource::<Saves>().0.general.current, None);
    }

    #[test]
    fn every_screen_has_a_way_out() {
        let saves = SaveSession::in_memory();
        let launch = converted_launch(&["AG-Workshop"]);
        let (s, t, p) = (Settings::default(), UiStrings::default(), Play::default());
        for screen in [
            Screen::Main,
            Screen::Chapters,
            Screen::TimeTrial,
            Screen::Settings,
            Screen::Pause,
            Screen::Loading,
            Screen::Confirm,
        ] {
            let b = buttons(&screen_items(&ctx(screen, &saves, &launch, &s, &t, &p)));
            assert!(
                b.iter().any(|x| x.2
                    && matches!(
                        x.1,
                        UiAction::Back | UiAction::Quit | UiAction::Resume | UiAction::CancelLoad
                    )),
                "{screen:?}"
            );
        }
        assert!(screen_items(&ctx(Screen::None, &saves, &launch, &s, &t, &p)).is_empty());
    }
}
