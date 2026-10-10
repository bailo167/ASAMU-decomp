//! Building blocks of the Sandbox's UI, shared by the panel, the launcher,
//! the HUD and the graphs: the look, the description of a screen as plain
//! data ([`Item`], [`Cell`], [`Btn`]), what a button does ([`Act`]) and the
//! one place where a click turns into a change ([`perform`]).
//!
//! A Sandbox screen is described the way the Classic menu is
//! (`ui::menus::screen_items`): a pure function builds a list of items from
//! the current state, and the tree is spawned again whenever that list
//! changes. Values that move by themselves (the tick count, speeds) are not
//! in the list; they are [`Live`] texts updated in place, so the tree is only
//! rebuilt in answer to a click, never between a press and its release.
//!
//! The view changes nothing but its own state: [`perform`] writes
//! [`ViewState`], the view-owned part of `PanelState` and `VizSettings`, and
//! hands back the [`LabRequest`]s for the runtime. All on-screen text goes
//! through [`ascii`]: the built-in font has ASCII glyphs only.

use asamu_sandbox::command::Command;
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::{InteractionDisabled, Pressed};
use bevy::ui_widgets::{Activate, Button};

use super::{Lab, LabRequest, PanelState, PanelTab, Stage, VizSettings, storage};

// The look (ours). The accent is amber, so nothing here can be taken for a
// Classic menu.
// Opaque: a see-through panel lets the read-out behind it show through
// its rows.
pub(super) const PANEL_BG: Color = Color::srgb(0.07, 0.08, 0.11);
pub(super) const BACKDROP: Color = Color::srgba(0.01, 0.015, 0.03, 0.62);
pub(super) const STRIP_BG: Color = Color::srgba(0.02, 0.03, 0.05, 0.74);
pub(super) const BUTTON: Color = Color::srgb(0.16, 0.19, 0.24);
pub(super) const BUTTON_HOVER: Color = Color::srgb(0.25, 0.31, 0.40);
pub(super) const BUTTON_PRESSED: Color = Color::srgb(0.33, 0.47, 0.62);
pub(super) const BUTTON_SELECTED: Color = Color::srgb(0.42, 0.31, 0.10);
pub(super) const BUTTON_DISABLED: Color = Color::srgba(0.12, 0.13, 0.15, 0.7);
pub(super) const TEXT: Color = Color::srgb(0.94, 0.94, 0.92);
pub(super) const TEXT_DIM: Color = Color::srgb(0.62, 0.64, 0.68);
/// Secondary text over the game (the HUD's key help, graph captions): lighter
/// than [`TEXT_DIM`], because the strip behind it is see-through.
pub(super) const TEXT_HINT: Color = Color::srgb(0.80, 0.83, 0.88);
pub(super) const TEXT_NOTICE: Color = Color::srgb(1.0, 0.80, 0.38);
pub(super) const TEXT_GOOD: Color = Color::srgb(0.55, 0.92, 0.60);
pub(super) const TEXT_WARN: Color = Color::srgb(1.0, 0.55, 0.50);

/// Text size of rows, labels and buttons.
pub(super) const FONT: f32 = 13.0;
/// Text size of a screen's title.
pub(super) const FONT_TITLE: f32 = 17.0;

/// Names offered for bookmarks (there is no text input).
pub(super) const MARK_NAMES: [&str; 4] = ["quick", "a", "b", "c"];
/// The bookmark the B and V keys use.
pub(super) const QUICK_MARK: &str = MARK_NAMES[0];
/// Most parameters the quick tuner holds.
pub(super) const MAX_PINNED: usize = 12;
/// Entries shown per page of a long list (teleport targets, stages,
/// profiles).
pub(super) const PAGE: usize = 10;

/// Marks every entity the Sandbox's view spawns (roots only; their children
/// go with them). Nothing with this marker outlives a session.
#[derive(Component, Clone, Copy, Debug, Default)]
pub(super) struct LabUi;

/// A box of text over the game that the arena's marker labels keep clear of
/// (the Sandbox HUD, the graphs).
#[derive(Component, Clone, Copy, Debug, Default)]
pub(super) struct KeepClear;

/// The view's own state, beside `PanelState` and `VizSettings`.
#[derive(Resource, Clone, Debug, PartialEq)]
pub(super) struct ViewState {
    /// The HUD read-outs are shown (H). The watermark line always is.
    pub hud: bool,
    /// Page of the teleport-target list.
    pub teleport_page: usize,
    /// Page of the stage list.
    pub stage_page: usize,
    /// Page of the profile list.
    pub profile_page: usize,
    /// The launcher's selected profile (index into its list; `None` until
    /// the launcher picked its default).
    pub launcher_profile: Option<usize>,
    /// A line from the view itself (a profile that could not be read).
    pub message: Option<String>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            hud: true,
            teleport_page: 0,
            stage_page: 0,
            profile_page: 0,
            launcher_profile: None,
            message: None,
        }
    }
}

/// How a text is coloured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tone {
    /// Plain.
    #[default]
    Normal,
    /// Secondary.
    Dim,
    /// Highlighted (a changed value, a notice, the watermark).
    Notice,
    /// Positive state.
    Good,
    /// A refusal or a warning.
    Warn,
    /// Secondary text over the game.
    Hint,
}

impl Tone {
    /// The tone's colour.
    pub(super) fn color(self) -> Color {
        match self {
            Self::Normal => TEXT,
            Self::Dim => TEXT_DIM,
            Self::Notice => TEXT_NOTICE,
            Self::Good => TEXT_GOOD,
            Self::Warn => TEXT_WARN,
            Self::Hint => TEXT_HINT,
        }
    }
}

/// A visualiser switch of `VizSettings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VizFlag {
    /// Velocity arrow.
    Velocity,
    /// Collision cylinder and floor normal.
    Cylinder,
    /// Aim ray and grapple range.
    Aim,
    /// Trail.
    Trail,
    /// Earlier attempts.
    Attempts,
    /// Predicted arc.
    Prediction,
    /// Arena markers.
    Markers,
    /// Graphs.
    Graphs,
}

impl VizFlag {
    /// Every switch, in display order.
    pub(super) const ALL: [Self; 8] = [
        Self::Velocity,
        Self::Cylinder,
        Self::Aim,
        Self::Trail,
        Self::Attempts,
        Self::Prediction,
        Self::Markers,
        Self::Graphs,
    ];

    /// The switch's state.
    pub(super) fn get(self, viz: &VizSettings) -> bool {
        match self {
            Self::Velocity => viz.velocity,
            Self::Cylinder => viz.cylinder,
            Self::Aim => viz.aim,
            Self::Trail => viz.trail,
            Self::Attempts => viz.attempts,
            Self::Prediction => viz.prediction,
            Self::Markers => viz.markers,
            Self::Graphs => viz.graphs,
        }
    }

    fn flip(self, viz: &mut VizSettings) {
        let flag = match self {
            Self::Velocity => &mut viz.velocity,
            Self::Cylinder => &mut viz.cylinder,
            Self::Aim => &mut viz.aim,
            Self::Trail => &mut viz.trail,
            Self::Attempts => &mut viz.attempts,
            Self::Prediction => &mut viz.prediction,
            Self::Markers => &mut viz.markers,
            Self::Graphs => &mut viz.graphs,
        };
        *flag = !*flag;
    }

    /// What the switch shows.
    pub(super) fn describe(self) -> (&'static str, &'static str) {
        match self {
            Self::Velocity => (
                "velocity",
                "arrow ahead of you: velocity (yellow) and its horizontal part (cyan)",
            ),
            Self::Cylinder => (
                "collision cylinder",
                "the pawn's collision cylinder and the floor normal",
            ),
            Self::Aim => (
                "aim and grapple range",
                "aim ray (green: can be grappled), ring at the gun's reach, release ring while \
                 attached",
            ),
            Self::Trail => ("trail", "where the pawn was during the latest ticks"),
            Self::Attempts => ("earlier attempts", "the trails of the last three attempts"),
            Self::Prediction => (
                "predicted arc",
                "held movement keys, grapple let go, on a static copy of the world (movers and \
                 level script do not advance)",
            ),
            Self::Markers => ("arena markers", "posts and labels of a hand-made arena"),
            Self::Graphs => ("graphs", "speed and height over the latest ticks"),
        }
    }
}

/// A list the view pages through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PageList {
    /// Teleport targets.
    Teleport,
    /// Stages.
    Stage,
    /// Profiles.
    Profile,
}

/// What a Sandbox button does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Act {
    /// Run a command through the session.
    Do(Command),
    /// Run several commands, in order.
    DoAll(Vec<Command>),
    /// Step a parameter. Left Alt held at the click multiplies the step by
    /// ten, Left Ctrl by a tenth.
    Nudge {
        /// Parameter key.
        key: String,
        /// Steps (negative: down).
        steps: i32,
        /// Multiplier of the button itself (1 or 10).
        scale: f64,
    },
    /// Load a profile into the running session, by name.
    LoadProfile(String),
    /// Save the session's tuning as a user profile.
    SaveProfile(String),
    /// Start a session (the launcher), or end the running one and start
    /// another (the Stage tab).
    Start {
        /// What to play on.
        stage: Stage,
        /// Profile name (`None`: Classic).
        profile: Option<String>,
    },
    /// End the session, or leave the launcher.
    End,
    /// Quit the program (the launcher of a `--sandbox` process).
    Quit,
    /// Close the inspector.
    Close,
    /// Show a tab.
    Tab(PanelTab),
    /// Show a parameter group.
    Group(usize),
    /// Select a parameter row.
    Row(usize),
    /// Add a parameter to the quick tuner, or take it out.
    Pin(String),
    /// Flip "freeze while the inspector is open".
    FreezeWhileOpen,
    /// Flip a visualiser.
    Viz(VizFlag),
    /// Show or hide the HUD read-outs.
    Hud,
    /// Show a page of a list (from 0).
    Page(PageList, usize),
    /// The launcher: previous or next profile.
    LauncherProfile(i32),
}

/// Modifier keys held at a click or a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Modifiers {
    /// Left Alt: ten times the step.
    pub coarse: bool,
    /// Left Ctrl: a tenth of the step.
    pub fine: bool,
}

impl Modifiers {
    /// The modifiers held now.
    pub(super) fn held(keys: &ButtonInput<KeyCode>) -> Self {
        Self {
            coarse: keys.pressed(KeyCode::AltLeft),
            fine: keys.pressed(KeyCode::ControlLeft),
        }
    }

    /// The step multiplier (coarse wins when both are held).
    pub(super) fn scale(self) -> f64 {
        if self.coarse {
            10.0
        } else if self.fine {
            0.1
        } else {
            1.0
        }
    }
}

/// Carries out `act`: changes the view's own state and returns the requests
/// for the runtime.
pub(super) fn perform(
    act: &Act,
    lab: &Lab,
    modifiers: Modifiers,
    panel: &mut PanelState,
    viz: &mut VizSettings,
    view: &mut ViewState,
) -> Vec<LabRequest> {
    let mut out = Vec::new();
    // A new click replaces what the view last said.
    view.message = None;
    match act {
        Act::Do(command) => out.push(LabRequest::Do(command.clone())),
        Act::DoAll(commands) => out.extend(commands.iter().cloned().map(LabRequest::Do)),
        Act::Nudge { key, steps, scale } => out.push(LabRequest::Do(Command::NudgeParam {
            key: key.clone(),
            steps: *steps,
            scale: scale * modifiers.scale(),
        })),
        Act::LoadProfile(name) => match storage::load_profile(lab, name) {
            Ok(profile) => out.push(LabRequest::Do(Command::LoadProfile {
                profile: Box::new(profile),
            })),
            // The message can quote the file back (an unknown key): kept
            // short, since the view copies it on every frame.
            Err(message) => view.message = Some(clip(&message, 400)),
        },
        Act::SaveProfile(name) => out.push(LabRequest::SaveProfile { name: name.clone() }),
        Act::Start { stage, profile } => out.push(LabRequest::Start {
            stage: stage.clone(),
            profile: profile.clone(),
        }),
        Act::End => out.push(LabRequest::End),
        Act::Quit => out.push(LabRequest::Quit),
        Act::Close => out.push(LabRequest::CloseInspector),
        Act::Tab(tab) => panel.tab = *tab,
        Act::Group(group) => {
            if panel.group != *group {
                panel.group = *group;
                panel.row = 0;
            }
        }
        Act::Row(row) => panel.row = *row,
        Act::Pin(key) => {
            if let Some(at) = panel.pinned.iter().position(|k| k == key) {
                panel.pinned.remove(at);
            } else if panel.pinned.len() < MAX_PINNED {
                panel.pinned.push(key.clone());
            } else {
                view.message = Some(format!(
                    "the quick tuner holds at most {MAX_PINNED} parameters"
                ));
            }
            panel.pinned_index = panel.pinned_index.min(panel.pinned.len().saturating_sub(1));
        }
        // `control` reads the flag every frame while the inspector is open,
        // so the clock follows at once; the session's own time model is not
        // touched.
        Act::FreezeWhileOpen => panel.freeze_while_open = !panel.freeze_while_open,
        Act::Viz(flag) => flag.flip(viz),
        Act::Hud => view.hud = !view.hud,
        Act::Page(list, page) => {
            let shown = match list {
                PageList::Teleport => &mut view.teleport_page,
                PageList::Stage => &mut view.stage_page,
                PageList::Profile => &mut view.profile_page,
            };
            *shown = *page;
        }
        Act::LauncherProfile(delta) => {
            let now = view.launcher_profile.unwrap_or(0);
            view.launcher_profile = Some(now.saturating_add_signed(*delta as isize));
        }
    }
    out
}

/// A button of a Sandbox screen.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Btn {
    /// Label.
    pub label: String,
    /// What it does.
    pub act: Act,
    /// Clickable.
    pub enabled: bool,
    /// Drawn as the current choice.
    pub selected: bool,
    /// Width in pixels (0: as wide as the label).
    pub width: f32,
    /// The label starts at the left edge instead of being centred.
    pub left: bool,
}

impl Btn {
    /// An enabled button as wide as its label.
    pub(super) fn new(label: impl Into<String>, act: Act) -> Self {
        Self {
            label: label.into(),
            act,
            enabled: true,
            selected: false,
            width: 0.0,
            left: false,
        }
    }

    /// With the label at the left edge.
    #[must_use]
    pub(super) fn left(mut self) -> Self {
        self.left = true;
        self
    }

    /// With a fixed width.
    #[must_use]
    pub(super) fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Drawn as the current choice when `selected`.
    #[must_use]
    pub(super) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Clickable only when `enabled`.
    #[must_use]
    pub(super) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A text that changes by itself and is updated in place (`panel` writes
/// them every frame).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Live {
    /// Tick, time state, pending steps.
    Clock,
    /// Position, speed, physics mode.
    Player,
    /// The run-time values the script layer latched.
    Latched,
    /// Grapple budget, boots, story mode.
    Abilities,
    /// What the crosshair points at.
    Aim,
    /// Peak speed, last jump, last swing.
    Telemetry,
    /// Rewind keyframes.
    Rewind,
    /// Recording state.
    Recording,
    /// Facts about the level.
    World,
}

/// One cell of a row.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Cell {
    /// A text of fixed width (0: as wide as the text).
    Label {
        /// The text.
        text: String,
        /// Width in pixels.
        width: f32,
        /// Colour.
        tone: Tone,
    },
    /// A heading-sized text.
    Title(String),
    /// A button.
    Button(Btn),
    /// A text updated in place.
    Live(Live),
    /// Takes the row's spare width.
    Fill,
}

impl Cell {
    /// A label cell.
    pub(super) fn label(text: impl Into<String>, width: f32, tone: Tone) -> Self {
        Self::Label {
            text: text.into(),
            width,
            tone,
        }
    }
}

impl From<Btn> for Cell {
    fn from(button: Btn) -> Self {
        Self::Button(button)
    }
}

/// One element of a Sandbox screen.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    /// Heading.
    Title(String),
    /// A line of text (it wraps).
    Text(String, Tone),
    /// A text updated in place.
    Live(Live),
    /// A row of cells.
    Row(Vec<Cell>),
    /// A thin separator.
    Rule,
}

impl Item {
    /// A plain line.
    pub(super) fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into(), Tone::Normal)
    }

    /// A secondary line.
    pub(super) fn dim(text: impl Into<String>) -> Self {
        Self::Text(text.into(), Tone::Dim)
    }

    /// A highlighted line.
    pub(super) fn notice(text: impl Into<String>) -> Self {
        Self::Text(text.into(), Tone::Notice)
    }
}

/// The buttons of `items`, in order (tests and checks).
#[cfg(test)]
pub(super) fn buttons(items: &[Item]) -> Vec<&Btn> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Row(cells) => Some(cells),
            _ => None,
        })
        .flatten()
        .filter_map(|cell| match cell {
            Cell::Button(button) => Some(button),
            _ => None,
        })
        .collect()
}

/// Every text of `items` (labels, lines, titles, button labels), in order.
#[cfg(test)]
pub(super) fn texts(items: &[Item]) -> Vec<&str> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Title(s) | Item::Text(s, _) => out.push(s.as_str()),
            Item::Row(cells) => {
                for cell in cells {
                    match cell {
                        Cell::Label { text, .. } | Cell::Title(text) => out.push(text.as_str()),
                        Cell::Button(button) => out.push(button.label.as_str()),
                        Cell::Live(_) | Cell::Fill => {}
                    }
                }
            }
            Item::Live(_) | Item::Rule => {}
        }
    }
    out
}

/// `text` in the glyphs the built-in font has: ASCII. Common signs are
/// spelled out, anything else becomes `?`.
pub(super) fn ascii(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if c.is_ascii() => out.push(c),
            '\u{d7}' => out.push('x'),
            '\u{b7}' | '\u{2022}' => out.push('-'),
            '\u{2013}' | '\u{2014}' | '\u{2212}' => out.push('-'),
            '\u{2192}' => out.push_str("->"),
            '\u{2190}' => out.push_str("<-"),
            '\u{2026}' => out.push_str("..."),
            '\u{b0}' => out.push_str(" deg"),
            '\u{b2}' => out.push_str("^2"),
            '\u{b3}' => out.push_str("^3"),
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2264}' => out.push_str("<="),
            '\u{2265}' => out.push_str(">="),
            '\u{2248}' => out.push('~'),
            '\u{a0}' => out.push(' '),
            _ => out.push('?'),
        }
    }
    out
}

/// `text` cut to at most `limit` characters, ending in `...` when cut.
pub(super) fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

/// A text bundle in the Sandbox's font size and `color` (ASCII only).
pub(super) fn text(s: &str, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(ascii(s)),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
    )
}

/// A Sandbox button: its action and whether it is drawn as the current
/// choice.
#[derive(Component, Clone, Debug)]
pub(super) struct LabButton {
    /// What a click does.
    pub act: Act,
    /// Drawn as the current choice.
    pub selected: bool,
}

/// Height of a button, pixels: a screen's row pitch.
const BUTTON_HEIGHT: f32 = 19.0;

fn spawn_button(parent: &mut ChildSpawnerCommands, button: &Btn) {
    let mut node = Node {
        min_height: Val::Px(BUTTON_HEIGHT),
        padding: UiRect::axes(Val::Px(7.0), Val::Px(1.0)),
        justify_content: if button.left {
            JustifyContent::FlexStart
        } else {
            JustifyContent::Center
        },
        align_items: AlignItems::Center,
        border_radius: BorderRadius::all(Val::Px(4.0)),
        flex_shrink: 0.0,
        ..default()
    };
    if button.width > 0.0 {
        node.width = Val::Px(button.width);
        node.overflow = Overflow::clip_x();
    }
    let background = if !button.enabled {
        BUTTON_DISABLED
    } else if button.selected {
        BUTTON_SELECTED
    } else {
        BUTTON
    };
    let mut entity = parent.spawn((
        Button,
        Hovered::default(),
        LabButton {
            act: button.act.clone(),
            selected: button.selected,
        },
        node,
        BackgroundColor(background),
        children![(
            text(
                &button.label,
                FONT,
                if button.enabled { TEXT } else { TEXT_DIM }
            ),
            TextLayout::no_wrap(),
        )],
    ));
    if !button.enabled {
        entity.insert(InteractionDisabled);
    }
}

fn spawn_cell(parent: &mut ChildSpawnerCommands, cell: &Cell) {
    match cell {
        Cell::Label {
            text: label,
            width,
            tone,
        } => {
            // A fixed-width label is one line, cut at the cell's edge.
            let mut node = Node {
                flex_shrink: 0.0,
                ..default()
            };
            if *width > 0.0 {
                node.width = Val::Px(*width);
                node.overflow = Overflow::clip_x();
            }
            parent.spawn((
                node,
                children![(text(label, FONT, tone.color()), TextLayout::no_wrap())],
            ));
        }
        Cell::Title(title) => {
            parent.spawn((text(title, FONT_TITLE, TEXT_NOTICE), TextLayout::no_wrap()));
        }
        Cell::Button(button) => spawn_button(parent, button),
        Cell::Live(which) => {
            parent.spawn((*which, text("", FONT, TEXT), TextLayout::no_wrap()));
        }
        Cell::Fill => {
            parent.spawn(Node {
                flex_grow: 1.0,
                ..default()
            });
        }
    }
}

/// Spawns `items` as children of a column.
pub(super) fn spawn_items(parent: &mut ChildSpawnerCommands, items: &[Item]) {
    for item in items {
        match item {
            Item::Title(s) => {
                parent.spawn(text(s, FONT_TITLE, TEXT_NOTICE));
            }
            Item::Text(s, tone) => {
                parent.spawn(text(s, FONT, tone.color()));
            }
            Item::Live(which) => {
                parent.spawn((*which, text("", FONT, TEXT)));
            }
            Item::Row(cells) => {
                parent
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(5.0),
                        ..default()
                    })
                    .with_children(|row| {
                        for cell in cells {
                            spawn_cell(row, cell);
                        }
                    });
            }
            Item::Rule => {
                parent.spawn((
                    Node {
                        height: Val::Px(1.0),
                        margin: UiRect::vertical(Val::Px(2.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.14)),
                ));
            }
        }
    }
}

/// A click on a Sandbox button: [`perform`], then the requests go out.
#[allow(clippy::too_many_arguments)]
fn on_activate(
    event: On<Activate>,
    buttons: Query<(&LabButton, Has<InteractionDisabled>)>,
    lab: Res<Lab>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut panel: ResMut<PanelState>,
    mut viz: ResMut<VizSettings>,
    mut view: ResMut<ViewState>,
    mut requests: MessageWriter<LabRequest>,
) {
    let Ok((button, disabled)) = buttons.get(event.entity) else {
        // Not a Sandbox button (the Classic menu has its own observer).
        return;
    };
    if disabled {
        return;
    }
    let modifiers = keys.as_deref().map(Modifiers::held).unwrap_or_default();
    // Written through plain copies, so a click that changes nothing does not
    // flag the resources as changed.
    let (mut next_panel, mut next_viz, mut next_view) = (panel.clone(), *viz, view.clone());
    let out = perform(
        &button.act,
        &lab,
        modifiers,
        &mut next_panel,
        &mut next_viz,
        &mut next_view,
    );
    panel.set_if_neq(next_panel);
    viz.set_if_neq(next_viz);
    view.set_if_neq(next_view);
    for request in out {
        requests.write(request);
    }
}

/// What [`style_buttons`] reads and writes per button.
type ButtonLook = (
    &'static LabButton,
    &'static Hovered,
    Has<Pressed>,
    Has<InteractionDisabled>,
    &'static mut BackgroundColor,
);

/// Hover, press and "current choice" colours.
fn style_buttons(mut buttons: Query<ButtonLook>) {
    for (button, hovered, pressed, disabled, mut background) in &mut buttons {
        let color = if disabled {
            BUTTON_DISABLED
        } else if pressed {
            BUTTON_PRESSED
        } else if hovered.get() {
            BUTTON_HOVER
        } else if button.selected {
            BUTTON_SELECTED
        } else {
            BUTTON
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<ViewState>()
        .add_observer(on_activate)
        .add_systems(
            Update,
            style_buttons.run_if(any_with_component::<LabButton>),
        );
}

#[cfg(test)]
mod tests {
    use asamu_sandbox::session::Session;

    use super::super::Phase;
    use super::*;

    fn lab() -> Lab {
        let mut lab = Lab::new(true);
        lab.phase = Phase::Active;
        lab.session = Some(Session::classic());
        // No directory: nothing in these tests reads or writes a file.
        lab.dirs = None;
        lab
    }

    fn run(act: &Act, panel: &mut PanelState, viz: &mut VizSettings) -> Vec<LabRequest> {
        let mut view = ViewState::default();
        perform(act, &lab(), Modifiers::default(), panel, viz, &mut view)
    }

    #[test]
    fn on_screen_text_is_ascii() {
        assert_eq!(ascii("plain"), "plain");
        assert_eq!(
            ascii("0.5\u{d7} \u{b7} a\u{2192}b \u{2026} 90\u{b0} uu/s\u{b2}"),
            "0.5x - a->b ... 90 deg uu/s^2"
        );
        assert_eq!(ascii("caf\u{e9} \u{4e16}"), "caf? ?");
        assert!(ascii("\u{201c}q\u{201d} \u{2014} \u{2264}").is_ascii());
        assert_eq!(clip("abcdef", 6), "abcdef");
        assert_eq!(clip("abcdefg", 6), "abc...");
    }

    #[test]
    fn commands_and_requests_pass_through_unchanged() {
        let (mut panel, mut viz) = (PanelState::default(), VizSettings::default());
        let before = (panel.clone(), viz);
        let out = run(&Act::Do(Command::Respawn), &mut panel, &mut viz);
        assert!(matches!(out.as_slice(), [LabRequest::Do(Command::Respawn)]));
        let out = run(
            &Act::DoAll(vec![Command::RefillGrapples, Command::ResetBoots]),
            &mut panel,
            &mut viz,
        );
        assert!(matches!(
            out.as_slice(),
            [
                LabRequest::Do(Command::RefillGrapples),
                LabRequest::Do(Command::ResetBoots)
            ]
        ));
        assert!(matches!(
            run(&Act::Close, &mut panel, &mut viz).as_slice(),
            [LabRequest::CloseInspector]
        ));
        assert!(matches!(
            run(&Act::End, &mut panel, &mut viz).as_slice(),
            [LabRequest::End]
        ));
        assert!(matches!(
            run(&Act::Quit, &mut panel, &mut viz).as_slice(),
            [LabRequest::Quit]
        ));
        let out = run(
            &Act::Start {
                stage: Stage::Arena("movement-lab".into()),
                profile: Some("moon".into()),
            },
            &mut panel,
            &mut viz,
        );
        assert!(matches!(
            out.as_slice(),
            [LabRequest::Start { stage: Stage::Arena(id), profile: Some(p) }]
                if id == "movement-lab" && p == "moon"
        ));
        let out = run(&Act::SaveProfile("custom-1".into()), &mut panel, &mut viz);
        assert!(matches!(
            out.as_slice(),
            [LabRequest::SaveProfile { name }] if name == "custom-1"
        ));
        // None of these touched the view's state.
        assert_eq!((panel, viz), before);
    }

    #[test]
    fn a_nudge_is_scaled_by_the_held_modifier() {
        let (mut panel, mut viz, mut view) = (
            PanelState::default(),
            VizSettings::default(),
            ViewState::default(),
        );
        let act = Act::Nudge {
            key: "movement.jump_velocity".into(),
            steps: -1,
            scale: 10.0,
        };
        for (modifiers, expected) in [
            (Modifiers::default(), 10.0),
            (
                Modifiers {
                    coarse: true,
                    fine: false,
                },
                100.0,
            ),
            (
                Modifiers {
                    coarse: false,
                    fine: true,
                },
                1.0,
            ),
        ] {
            let out = perform(&act, &lab(), modifiers, &mut panel, &mut viz, &mut view);
            match out.as_slice() {
                [LabRequest::Do(Command::NudgeParam { key, steps, scale })] => {
                    assert_eq!(key, "movement.jump_velocity");
                    assert_eq!(*steps, -1);
                    assert!((scale - expected).abs() < 1e-9, "{scale} vs {expected}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn view_acts_change_only_view_state() {
        let (mut panel, mut viz) = (PanelState::default(), VizSettings::default());
        assert!(run(&Act::Tab(PanelTab::Time), &mut panel, &mut viz).is_empty());
        assert_eq!(panel.tab, PanelTab::Time);

        panel.row = 5;
        assert!(run(&Act::Group(2), &mut panel, &mut viz).is_empty());
        assert_eq!((panel.group, panel.row), (2, 0));
        assert!(run(&Act::Row(3), &mut panel, &mut viz).is_empty());
        assert_eq!(panel.row, 3);
        // The same group again keeps the row.
        assert!(run(&Act::Group(2), &mut panel, &mut viz).is_empty());
        assert_eq!(panel.row, 3);

        for flag in VizFlag::ALL {
            let before = flag.get(&viz);
            assert!(run(&Act::Viz(flag), &mut panel, &mut viz).is_empty());
            assert_eq!(flag.get(&viz), !before, "{flag:?}");
            assert!(run(&Act::Viz(flag), &mut panel, &mut viz).is_empty());
        }
        assert_eq!(viz, VizSettings::default());
        // The inspector's own flag is never written by the view.
        assert!(!panel.open);
    }

    #[test]
    fn the_quick_tuner_starts_with_keys_that_do_something() {
        use asamu_sandbox::keys::{Catalog, Effect};
        // Every starting pin is a key of the catalogue, and none of them is
        // one the Classic pipeline never reads or only reads at a spawn.
        let catalog = Catalog::classic();
        let panel = PanelState::default();
        assert!(!panel.pinned.is_empty());
        for key in &panel.pinned {
            let info = catalog
                .get(key)
                .unwrap_or_else(|| panic!("{key} is not a key"));
            assert!(
                !matches!(info.effect, Effect::Inert(_) | Effect::SpawnOnly),
                "{key}: {:?}",
                info.effect
            );
        }
        // Air steering is pinned as the value a pawn runs on from its first
        // landing; the start value stops reaching it then (shown by
        // `asamu-sandbox/tests/tuning_effects.rs`).
        assert!(
            panel
                .pinned
                .iter()
                .any(|key| key == "pawn.landed_air_control")
        );
        assert!(!panel.pinned.iter().any(|key| key == "movement.air_control"));
    }

    #[test]
    fn pinning_keeps_the_quick_tuner_in_bounds() {
        let (mut panel, mut viz) = (PanelState::default(), VizSettings::default());
        let first = panel.pinned[0].clone();
        panel.pinned_index = panel.pinned.len() - 1;
        run(&Act::Pin(first.clone()), &mut panel, &mut viz);
        assert!(!panel.pinned.contains(&first));
        assert_eq!(panel.pinned_index, panel.pinned.len() - 1);
        run(&Act::Pin(first.clone()), &mut panel, &mut viz);
        assert_eq!(panel.pinned.last(), Some(&first));
        // Full: one more is refused with a message, nothing is dropped.
        let mut view = ViewState::default();
        for i in 0..MAX_PINNED {
            perform(
                &Act::Pin(format!("group.key{i}")),
                &lab(),
                Modifiers::default(),
                &mut panel,
                &mut viz,
                &mut view,
            );
        }
        assert_eq!(panel.pinned.len(), MAX_PINNED);
        assert!(view.message.is_some());
        // Emptying the list leaves a valid index.
        for key in panel.pinned.clone() {
            run(&Act::Pin(key), &mut panel, &mut viz);
        }
        assert!(panel.pinned.is_empty());
        assert_eq!(panel.pinned_index, 0);
    }

    #[test]
    fn freeze_while_open_is_a_preference_not_a_command() {
        let (mut panel, mut viz) = (PanelState::default(), VizSettings::default());
        assert!(panel.freeze_while_open);
        for open in [false, true] {
            panel.open = open;
            let before = panel.freeze_while_open;
            // The runtime holds the clock from the flag: no time command is
            // sent, so the session's own time model stays as it is.
            assert!(run(&Act::FreezeWhileOpen, &mut panel, &mut viz).is_empty());
            assert_eq!(panel.freeze_while_open, !before);
            assert_eq!(panel.open, open, "the view never opens or closes it");
        }
    }

    #[test]
    fn profiles_resolve_built_ins_and_report_what_is_missing() {
        let (mut panel, mut viz, mut view) = (
            PanelState::default(),
            VizSettings::default(),
            ViewState::default(),
        );
        let out = perform(
            &Act::LoadProfile("moon".into()),
            &lab(),
            Modifiers::default(),
            &mut panel,
            &mut viz,
            &mut view,
        );
        match out.as_slice() {
            [LabRequest::Do(Command::LoadProfile { profile })] => {
                assert_eq!(profile.name, "moon");
                assert!(!profile.overrides.is_empty());
            }
            other => panic!("{other:?}"),
        }
        assert!(view.message.is_none());
        // Unknown and no directory: a message, no request, no panic.
        let out = perform(
            &Act::LoadProfile("no-such-profile".into()),
            &lab(),
            Modifiers::default(),
            &mut panel,
            &mut viz,
            &mut view,
        );
        assert!(out.is_empty());
        assert!(view.message.as_deref().is_some_and(|m| m.is_ascii()));
        // The next click clears the message.
        perform(
            &Act::Hud,
            &lab(),
            Modifiers::default(),
            &mut panel,
            &mut viz,
            &mut view,
        );
        assert!(view.message.is_none());
        assert!(!view.hud);
    }

    #[test]
    fn a_click_on_a_sandbox_button_is_carried_out_unless_disabled() {
        let mut app = App::new();
        app.insert_resource(lab())
            .init_resource::<PanelState>()
            .init_resource::<VizSettings>()
            .init_resource::<ViewState>()
            .add_message::<LabRequest>()
            .add_observer(on_activate);
        let button = |act: Act| LabButton {
            act,
            selected: false,
        };
        let respawn = app
            .world_mut()
            .spawn(button(Act::Do(Command::Respawn)))
            .id();
        let disabled = app
            .world_mut()
            .spawn((button(Act::Do(Command::Kill)), InteractionDisabled))
            .id();
        let tab = app.world_mut().spawn(button(Act::Tab(PanelTab::View))).id();
        // Not a Sandbox button (a Classic menu button, say): not ours.
        let other = app.world_mut().spawn_empty().id();
        for entity in [respawn, disabled, tab, other] {
            app.world_mut().trigger(Activate { entity });
        }
        let written: Vec<LabRequest> = app
            .world()
            .resource::<Messages<LabRequest>>()
            .iter_current_update_messages()
            .cloned()
            .collect();
        assert!(
            matches!(written.as_slice(), [LabRequest::Do(Command::Respawn)]),
            "{written:?}"
        );
        assert_eq!(app.world().resource::<PanelState>().tab, PanelTab::View);
        assert!(!app.world().resource::<PanelState>().open);
    }

    #[test]
    fn pages_are_set_and_the_launcher_profile_never_underflows() {
        let (mut panel, mut viz, mut view) = (
            PanelState::default(),
            VizSettings::default(),
            ViewState::default(),
        );
        let mut go = |act: Act, view: &mut ViewState| {
            perform(
                &act,
                &lab(),
                Modifiers::default(),
                &mut panel,
                &mut viz,
                view,
            );
        };
        go(Act::Page(PageList::Teleport, 2), &mut view);
        go(Act::Page(PageList::Stage, 1), &mut view);
        go(Act::Page(PageList::Profile, 3), &mut view);
        assert_eq!(
            (view.teleport_page, view.stage_page, view.profile_page),
            (2, 1, 3)
        );
        go(Act::Page(PageList::Teleport, 0), &mut view);
        assert_eq!(view.teleport_page, 0);
        go(Act::LauncherProfile(-1), &mut view);
        assert_eq!(view.launcher_profile, Some(0));
        go(Act::LauncherProfile(1), &mut view);
        assert_eq!(view.launcher_profile, Some(1));
    }
}
