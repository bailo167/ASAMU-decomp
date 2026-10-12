//! Bevy UI HUD shared by every mode: an information panel (top left), the
//! crosshair (centre), an ability line (bottom left: grapple count and
//! ability state) and a subtitle area (bottom centre).
//!
//! **Developer drawings.** The information panel (model, parameters, speeds,
//! states, key help) and the ability line are developer read-outs: the
//! original shows neither. [`DebugInfo`] switches them, together with the
//! aim and anchor gizmos of `main.rs`; F1 flips it at any time (menus and the
//! title card included). It starts on in the graybox test level and off on
//! converted levels (`--debug-info` starts it on), whichever way the level
//! was started.
//!
//! **Crosshair.** The "+" is the stand-in for the original's gameplay
//! crosshair; the ring that `crate::vfx` draws around it follows the
//! original's crosshair state machine (VFX_DECALS.md §5). The "+" is drawn
//! only while a game is being played with no menu open and the pawn is not in
//! story mode ([`plus_crosshair_shown`]): the original's story crosshair is a
//! different, smaller picture, which the ring alone stands for (PLACEHOLDER:
//! the crosshair movie's art is not ported).
//!
//! The original's HUD and menus are Scaleform movies (not ported); this is a
//! functional stand-in. The audio module writes the subtitle line
//! ([`Subtitle`]; narration and Kismet sounds) and the box hides itself when
//! it is `None` (or when subtitles are off in the settings). Kismet hides the
//! crosshair and the ability line (`crate::kismet`).
//!
//! Subtitle language: the line is shown through [`SubtitleLanguage`] (set by
//! the UI's language setting, `ui::locale`), which turns the audio export's
//! line into the chosen language's line for the same wave.

use std::sync::Arc;

use asamu_assets::localization::SubtitleTranslation;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

/// Top-left information text.
#[derive(Component)]
pub struct HudInfo;

/// The information panel's box.
#[derive(Component)]
pub struct HudInfoPanel;

/// Whether the developer drawings are shown: the information panel, the
/// ability line ([`DevReadout`]) and the aim and anchor gizmos (`main.rs`).
/// F1 toggles it.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugInfo(pub bool);

impl Default for DebugInfo {
    fn default() -> Self {
        Self(true)
    }
}

/// Centre crosshair text (its colour shows the grapple target state).
/// Kismet switches this entity; the rule of [`plus_crosshair_shown`] switches
/// its parent ([`CrosshairBox`]).
#[derive(Component)]
pub struct Crosshair;

/// The full-window box that centres the [`Crosshair`]; hidden whenever the
/// "+" has no place on screen ([`plus_crosshair_shown`]).
#[derive(Component)]
pub struct CrosshairBox;

/// The box around the ability line ([`AbilityPanel`]): a developer read-out,
/// shown with [`DebugInfo`]. Kismet's HUD switch acts on the panel inside.
#[derive(Component)]
pub struct DevReadout;

/// What decides whether the gameplay "+" is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrosshairView {
    /// A game is running (a simulation exists and has been started).
    pub playing: bool,
    /// A menu screen is open (main menu, pause, settings, loading ...).
    pub menu_open: bool,
    /// The pawn is in story mode (or zooming, which is pushed on top of it).
    pub story: bool,
}

/// Whether the gameplay "+" is drawn.
///
/// - Not behind a menu: the original's pause menu hides the crosshair while
///   it is open (VFX_DECALS.md §5, CONFIRMED (src)), and the main menu has no
///   HUD at all.
/// - Not without a running game (the level behind the main menu, the fly
///   camera): there is no pawn to aim with.
/// - Not in story mode: the original switches its crosshair movie to the
///   story frames there (`ASAMUHUD.ToggleStoryMode`, CONFIRMED (src)); the
///   ring of `crate::vfx` shows that state, and the gameplay "+" would claim
///   a grapple crosshair where the original shows none. PLACEHOLDER for the
///   story frames' art, which is not ported.
#[must_use]
pub fn plus_crosshair_shown(view: CrosshairView) -> bool {
    view.playing && !view.menu_open && !view.story
}

/// Bottom-left ability panel text.
#[derive(Component)]
pub struct HudAbilities;

/// The ability panel's box (hidden with the HUD by Kismet).
#[derive(Component)]
pub struct AbilityPanel;

/// The subtitle box (shown only while [`Subtitle`] is `Some`).
#[derive(Component)]
pub struct SubtitleBox;

/// The subtitle text inside [`SubtitleBox`].
#[derive(Component)]
pub struct SubtitleText;

/// Current subtitle line (written by the audio module).
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Subtitle(pub Option<String>);

/// The subtitle language: lines of the converted audio (any language) →
/// the chosen language (`None`: shown as written).
#[derive(Resource, Default, Debug, Clone)]
pub struct SubtitleLanguage(pub Option<Arc<SubtitleTranslation>>);

impl SubtitleLanguage {
    /// `line` in the chosen language.
    #[must_use]
    pub fn show(&self, line: &str) -> String {
        let text = self.0.as_ref().map_or(line, |t| t.translate(line));
        asamu_assets::localization::display_text(text)
    }
}

/// Text colour of the information panel (dark on the graybox sky, light on
/// converted levels).
#[derive(Resource, Clone, Copy, Debug)]
pub struct HudStyle {
    /// Information panel text colour.
    pub info_color: Color,
}

impl Default for HudStyle {
    fn default() -> Self {
        Self {
            info_color: Color::srgb(0.05, 0.05, 0.08),
        }
    }
}

/// Spawns the HUD and keeps the subtitle box in sync.
pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<FrameTimeDiagnosticsPlugin>() {
            app.add_plugins(FrameTimeDiagnosticsPlugin::default());
        }
        app.init_resource::<Subtitle>()
            .init_resource::<SubtitleLanguage>()
            .init_resource::<HudStyle>()
            .init_resource::<DebugInfo>()
            .add_systems(Startup, spawn_hud)
            .add_systems(
                Update,
                (sync_subtitle, toggle_debug_info, sync_crosshair_box),
            );
    }
}

fn spawn_hud(mut commands: Commands, style: Res<HudStyle>) {
    commands.spawn((
        HudInfoPanel,
        Node {
            position_type: PositionType::Absolute,
            top: px(10),
            left: px(12),
            right: px(12),
            ..default()
        },
        children![(
            HudInfo,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(14.0),
                ..default()
            },
            TextColor(style.info_color),
        )],
    ));
    // A full-window box that centres the crosshair glyph (the box itself
    // takes no clicks).
    commands.spawn((
        CrosshairBox,
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(0),
            bottom: px(0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        Pickable::IGNORE,
        Visibility::Hidden,
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
    // The ability line is a developer read-out: its box follows `DebugInfo`
    // (hidden until the first `toggle_debug_info` run says otherwise).
    commands.spawn((
        DevReadout,
        Node {
            position_type: PositionType::Absolute,
            left: px(12),
            bottom: px(12),
            ..default()
        },
        Pickable::IGNORE,
        Visibility::Hidden,
        children![(
            AbilityPanel,
            Node {
                padding: UiRect::axes(px(10), px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
            children![(
                HudAbilities,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.95, 0.9)),
            )],
        )],
    ));
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: percent(20),
            right: percent(20),
            bottom: px(48),
            justify_content: JustifyContent::Center,
            ..default()
        },
        children![(
            SubtitleBox,
            Node {
                padding: UiRect::axes(px(14), px(8)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            Visibility::Hidden,
            children![(
                SubtitleText,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(20.0),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::justify(Justify::Center),
            )],
        )],
    ));
}

fn sync_subtitle(
    subtitle: Res<Subtitle>,
    language: Res<SubtitleLanguage>,
    mut boxes: Query<&mut Visibility, With<SubtitleBox>>,
    mut texts: Query<&mut Text, With<SubtitleText>>,
) {
    if !subtitle.is_changed() && !language.is_changed() {
        return;
    }
    for mut v in &mut boxes {
        *v = if subtitle.0.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
    for mut t in &mut texts {
        t.0 = subtitle
            .0
            .as_deref()
            .map(|l| language.show(l))
            .unwrap_or_default();
    }
}

/// F1 flips [`DebugInfo`]; the developer read-outs (the information panel and
/// the ability line) follow it. The key is read in every state of the app:
/// in the menus, while a level loads and over the title card.
#[allow(clippy::type_complexity)]
fn toggle_debug_info(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut info: ResMut<DebugInfo>,
    mut panels: Query<&mut Visibility, Or<(With<HudInfoPanel>, With<DevReadout>)>>,
) {
    if keys.is_some_and(|k| k.just_pressed(KeyCode::F1)) {
        info.0 = !info.0;
    }
    let wanted = if info.0 {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut panels {
        if *v != wanted {
            *v = wanted;
        }
    }
}

/// Shows the "+" box by [`plus_crosshair_shown`].
fn sync_crosshair_box(
    sim: Option<Res<crate::Sim>>,
    ui: Option<Res<crate::ui::UiState>>,
    mut boxes: Query<&mut Visibility, With<CrosshairBox>>,
) {
    let view = CrosshairView {
        playing: sim
            .as_ref()
            .is_some_and(|s| s.game.state() != asamu_game::GameState::Boot),
        menu_open: ui
            .as_ref()
            .is_some_and(|u| u.screen != crate::ui::Screen::None),
        story: sim.as_ref().is_some_and(|s| s.game.in_story_mode()),
    };
    let wanted = if plus_crosshair_shown(view) {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut v in &mut boxes {
        if *v != wanted {
            *v = wanted;
        }
    }
}

/// Smoothed frames per second, when the diagnostic has data.
#[must_use]
pub fn fps(diagnostics: &DiagnosticsStore) -> Option<f64> {
    diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(bevy::diagnostic::Diagnostic::smoothed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::audio::SubtitleLine;
    use asamu_assets::localization::{LanguageTable, Localization};

    #[test]
    fn the_plus_needs_a_game_no_menu_and_no_story_mode() {
        let playing = CrosshairView {
            playing: true,
            menu_open: false,
            story: false,
        };
        assert!(plus_crosshair_shown(playing));
        // The level behind the main menu, the fly camera, a game not started.
        assert!(!plus_crosshair_shown(CrosshairView {
            playing: false,
            ..playing
        }));
        // Pause, settings, loading.
        assert!(!plus_crosshair_shown(CrosshairView {
            menu_open: true,
            ..playing
        }));
        // The Workshop before the launch, every story section.
        assert!(!plus_crosshair_shown(CrosshairView {
            story: true,
            ..playing
        }));
    }

    /// The visibility of every entity carrying `C`.
    fn visibility_of<C: Component>(app: &mut App) -> Vec<Visibility> {
        let world = app.world_mut();
        let mut q = world.query_filtered::<&Visibility, With<C>>();
        q.iter(world).copied().collect()
    }

    #[test]
    fn developer_read_outs_follow_debug_info_and_f1() {
        let mut app = App::new();
        app.insert_resource(DebugInfo(false))
            .init_resource::<HudStyle>()
            .init_resource::<ButtonInput<KeyCode>>()
            .add_systems(Startup, spawn_hud)
            .add_systems(Update, toggle_debug_info);
        app.update();
        // A clean screen by default: neither read-out, and the ability line
        // stays inside its (hidden) box so that Kismet's HUD switch cannot
        // bring it back.
        assert_eq!(
            visibility_of::<HudInfoPanel>(&mut app),
            [Visibility::Hidden]
        );
        assert_eq!(visibility_of::<DevReadout>(&mut app), [Visibility::Hidden]);
        let world = app.world_mut();
        let mut q = world.query_filtered::<&ChildOf, With<AbilityPanel>>();
        let parents: Vec<Entity> = q.iter(world).map(ChildOf::parent).collect();
        assert_eq!(parents.len(), 1);
        assert!(world.get::<DevReadout>(parents[0]).is_some());
        // F1 shows both, F1 again hides both.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::F1);
        app.update();
        assert!(app.world().resource::<DebugInfo>().0);
        assert_eq!(
            visibility_of::<HudInfoPanel>(&mut app),
            [Visibility::Inherited]
        );
        assert_eq!(
            visibility_of::<DevReadout>(&mut app),
            [Visibility::Inherited]
        );
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.release(KeyCode::F1);
            keys.clear();
            keys.press(KeyCode::F1);
        }
        app.update();
        assert!(!app.world().resource::<DebugInfo>().0);
        assert_eq!(visibility_of::<DevReadout>(&mut app), [Visibility::Hidden]);
    }

    #[test]
    fn the_plus_box_is_hidden_without_a_game_and_in_story_mode() {
        let mut app = App::new();
        app.init_resource::<HudStyle>()
            .add_systems(Startup, spawn_hud)
            .add_systems(Update, sync_crosshair_box);
        // No simulation (the level behind the main menu, the fly camera).
        app.update();
        assert_eq!(
            visibility_of::<CrosshairBox>(&mut app),
            [Visibility::Hidden]
        );
        // A started graybox game (never in story mode): shown.
        let mut game = asamu_game::Game::graybox().unwrap();
        game.start();
        app.insert_resource(crate::Sim::new(game, String::new()));
        app.update();
        assert_eq!(
            visibility_of::<CrosshairBox>(&mut app),
            [Visibility::Inherited]
        );
        // Story mode (the Workshop, every story section): hidden.
        app.world_mut()
            .resource_mut::<crate::Sim>()
            .game
            .toggle_story_mode();
        assert!(app.world().resource::<crate::Sim>().game.in_story_mode());
        app.update();
        assert_eq!(
            visibility_of::<CrosshairBox>(&mut app),
            [Visibility::Hidden]
        );
        // Back in gameplay with a menu open (pause): hidden.
        app.world_mut()
            .resource_mut::<crate::Sim>()
            .game
            .toggle_story_mode();
        app.update();
        assert_eq!(
            visibility_of::<CrosshairBox>(&mut app),
            [Visibility::Inherited]
        );
        let mut ui = crate::ui::UiState::default();
        ui.open(crate::ui::Screen::Pause);
        app.insert_resource(ui);
        app.update();
        assert_eq!(
            visibility_of::<CrosshairBox>(&mut app),
            [Visibility::Hidden]
        );
    }

    #[test]
    fn subtitles_follow_the_language() {
        let line = |t: &str| SubtitleLine {
            text: t.to_owned(),
            time: 0.0,
        };
        let mut int = LanguageTable::new("INT");
        int.insert_subtitles("P.W", vec![line("Hello")]);
        let mut tst = LanguageTable::new("TST");
        tst.insert_subtitles("P.W", vec![line("Hallo\\nWelt")]);
        let l = Localization::from_tables(vec![int, tst]);
        let mut app = App::new();
        app.init_resource::<Subtitle>()
            .init_resource::<SubtitleLanguage>()
            .add_systems(Update, sync_subtitle);
        let text = app.world_mut().spawn((SubtitleText, Text::new(""))).id();
        app.world_mut().resource_mut::<Subtitle>().0 = Some("Hello".into());
        app.update();
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "Hello");
        // Changing the language alone re-renders the shown line.
        app.world_mut().resource_mut::<SubtitleLanguage>().0 =
            Some(Arc::new(l.subtitle_translation("TST")));
        app.update();
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "Hallo\nWelt");
        app.world_mut().resource_mut::<Subtitle>().0 = Some("Unknown".into());
        app.update();
        assert_eq!(app.world().get::<Text>(text).unwrap().0, "Unknown");
    }
}
