//! Bevy UI HUD shared by every mode: an information panel (top left), the
//! crosshair (centre), an ability panel (bottom left: grapple count and
//! ability state) and a subtitle area (bottom centre).
//!
//! The original's HUD and menus are Scaleform movies (not ported); this is a
//! functional stand-in. The audio module writes the subtitle line
//! ([`Subtitle`]; narration and Kismet sounds) and the box hides itself when
//! it is `None` (or when subtitles are off in the settings). Kismet hides the
//! crosshair and the ability panel (`crate::kismet`).

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

/// Top-left information text.
#[derive(Component)]
pub struct HudInfo;

/// Centre crosshair text (its colour shows the grapple target state).
#[derive(Component)]
pub struct Crosshair;

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

/// Current subtitle line (placeholder; nothing sets it yet).
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Subtitle(pub Option<String>);

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
            .init_resource::<HudStyle>()
            .add_systems(Startup, spawn_hud)
            .add_systems(Update, sync_subtitle);
    }
}

fn spawn_hud(mut commands: Commands, style: Res<HudStyle>) {
    commands.spawn((
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
    commands.spawn((
        AbilityPanel,
        Node {
            position_type: PositionType::Absolute,
            left: px(12),
            bottom: px(12),
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
    mut boxes: Query<&mut Visibility, With<SubtitleBox>>,
    mut texts: Query<&mut Text, With<SubtitleText>>,
) {
    if !subtitle.is_changed() {
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
        t.0 = subtitle.0.clone().unwrap_or_default();
    }
}

/// Smoothed frames per second, when the diagnostic has data.
#[must_use]
pub fn fps(diagnostics: &DiagnosticsStore) -> Option<f64> {
    diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(bevy::diagnostic::Diagnostic::smoothed)
}
