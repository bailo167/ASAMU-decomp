//! On-screen notices: toasts (collectibles, story items, achievements,
//! extras, checkpoint saves, errors), the collectibles counter and the
//! time-trial stopwatch.
//!
//! The stopwatch counts **game time** in simulation ticks (deterministic;
//! the original's HUD timer counts game time too, ABILITIES.md A-TT-2): it
//! starts on [`super::TimeTrialStart`] (Kismet `SeqAct_StartTimeTrial`,
//! ignored while running), stops on [`super::TimeTrialEnd`], and restarts
//! when the player dies before any checkpoint is registered.

use std::collections::VecDeque;

use asamu_game::save::{ChapterId, PlayMode, SaveSession, format_trial_time};
use bevy::prelude::*;

use super::{Play, Saves, Screen, UiState};
use crate::Sim;

/// How long a toast stays (s, real time; presentation only).
const TOAST_SECONDS: f32 = 4.0;
/// Most toasts shown at once.
const MAX_TOASTS: usize = 4;

/// Pending notices.
#[derive(Resource, Debug, Default)]
pub(crate) struct Toasts {
    items: VecDeque<(String, f32)>,
    changed: bool,
}

impl Toasts {
    /// Shows `text` for a few seconds.
    pub fn push(&mut self, text: impl Into<String>) {
        let text = text.into();
        info!("notice: {text}");
        self.items.push_back((text, TOAST_SECONDS));
        while self.items.len() > MAX_TOASTS {
            self.items.pop_front();
        }
        self.changed = true;
    }
}

/// The time-trial stopwatch (in ticks of the game's fixed clock).
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct TimeTrialClock {
    /// Running.
    pub running: bool,
    /// Tick at the (re)start.
    start_tick: u64,
    /// Elapsed game time, s.
    pub elapsed: f64,
    /// The time of the finished run, s.
    pub finished: Option<f64>,
}

impl TimeTrialClock {
    /// Starts at `tick` (ignored while running).
    pub fn start(&mut self, tick: u64) {
        if !self.running {
            *self = Self {
                running: true,
                start_tick: tick,
                elapsed: 0.0,
                finished: None,
            };
        }
    }

    /// Restarts from `tick` (death before any checkpoint).
    pub fn restart(&mut self, tick: u64) {
        self.running = false;
        self.start(tick);
    }

    /// Updates the elapsed time after `tick` at `rate_hz`.
    pub fn update(&mut self, tick: u64, rate_hz: f64) {
        if self.running && rate_hz > 0.0 {
            self.elapsed = tick.saturating_sub(self.start_tick) as f64 / rate_hz;
        }
    }

    /// Stops and returns the time (`None` when not running).
    pub fn stop(&mut self) -> Option<f64> {
        if !self.running {
            return None;
        }
        self.running = false;
        self.finished = Some(self.elapsed);
        self.finished
    }
}

#[derive(Component)]
pub(crate) struct ToastText;

#[derive(Component)]
pub(crate) struct CounterText;

#[derive(Component)]
pub(crate) struct TrialText;

/// Spawns the notice areas.
pub(crate) fn spawn_hud(mut commands: Commands) {
    // Toasts: top centre.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(16),
            left: percent(25),
            right: percent(25),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(90),
        children![(
            ToastText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(20.0),
                ..default()
            },
            TextColor(Color::srgb(1.0, 0.95, 0.75)),
            TextLayout::justify(Justify::Center),
            TextShadow::default(),
        )],
    ));
    // Collectibles counter: top right.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(12),
            right: px(14),
            ..default()
        },
        GlobalZIndex(80),
        children![(
            CounterText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(16.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.9, 0.6)),
            TextShadow::default(),
        )],
    ));
    // Time-trial stopwatch: under the toasts.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(60),
            left: percent(40),
            right: percent(40),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(80),
        children![(
            TrialText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(30.0),
                ..default()
            },
            TextColor(Color::WHITE),
            TextShadow::default(),
        )],
    ));
}

/// Ages the toasts and redraws them when they change.
pub(crate) fn update_toasts(
    time: Res<Time<Real>>,
    mut toasts: ResMut<Toasts>,
    mut text: Query<&mut Text, With<ToastText>>,
) {
    let dt = time.delta_secs();
    let before = toasts.items.len();
    for item in &mut toasts.items {
        item.1 -= dt;
    }
    toasts.items.retain(|(_, t)| *t > 0.0);
    if toasts.items.len() != before {
        toasts.changed = true;
    }
    if !toasts.changed {
        return;
    }
    toasts.changed = false;
    let joined = toasts
        .items
        .iter()
        .map(|(s, _)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for mut t in &mut text {
        t.0.clone_from(&joined);
    }
}

/// The counter line for a chapter (`None` for chapters without
/// collectibles).
#[must_use]
pub(crate) fn counter_line(saves: &SaveSession, chapter: Option<ChapterId>) -> Option<String> {
    let chapter = chapter.filter(|c| c.has_collectibles())?;
    let p = &saves.progression;
    Some(format!(
        "Collectibles {}/{} | total {}/{}",
        p.collectible_count(chapter),
        asamu_game::save::COLLECTIBLES_PER_CHAPTER,
        p.collectible_total(),
        asamu_game::save::TOTAL_COLLECTIBLES
    ))
}

/// The counter line while playing (story play only: collectibles are
/// hidden in time trial).
#[must_use]
pub(crate) fn counter_line_for(saves: &SaveSession, play: &Play) -> Option<String> {
    (play.mode == PlayMode::Story)
        .then(|| counter_line(saves, play.chapter))
        .flatten()
}

/// Updates the collectibles counter (story play in a collectible chapter,
/// no menu) and the time-trial stopwatch (time-trial play).
pub(crate) fn update_hud(
    state: Res<UiState>,
    saves: Res<Saves>,
    play: Res<Play>,
    clock: Res<TimeTrialClock>,
    sim: Option<Res<Sim>>,
    mut counter: Query<&mut Text, (With<CounterText>, Without<TrialText>)>,
    mut trial: Query<&mut Text, (With<TrialText>, Without<CounterText>)>,
) {
    let playing = state.screen == Screen::None && sim.is_some();
    let counter_text = if playing {
        counter_line_for(&saves.0, &play).unwrap_or_default()
    } else {
        String::new()
    };
    let trial_text = if playing && play.mode == PlayMode::TimeTrial {
        match (clock.running, clock.finished) {
            (true, _) => format_trial_time(clock.elapsed),
            (false, Some(t)) => format!("{} (finished)", format_trial_time(t)),
            (false, None) => "--:--:--".to_owned(),
        }
    } else {
        String::new()
    };
    for mut t in &mut counter {
        if t.0 != counter_text {
            t.0.clone_from(&counter_text);
        }
    }
    for mut t in &mut trial {
        if t.0 != trial_text {
            t.0.clone_from(&trial_text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwatch_counts_ticks_and_ignores_a_second_start() {
        let mut c = TimeTrialClock::default();
        c.update(100, 60.0);
        assert_eq!(c.elapsed, 0.0, "not running");
        c.start(60);
        c.update(180, 60.0);
        assert_eq!(c.elapsed, 2.0);
        c.start(170);
        c.update(240, 60.0);
        assert_eq!(c.elapsed, 3.0, "start ignored while running");
        c.restart(240);
        c.update(270, 60.0);
        assert_eq!(c.elapsed, 0.5);
        assert_eq!(c.stop(), Some(0.5));
        assert_eq!(c.stop(), None);
        assert!(!c.running);
    }

    #[test]
    fn toasts_are_bounded() {
        let mut t = Toasts::default();
        for i in 0..10 {
            t.push(format!("n{i}"));
        }
        assert_eq!(t.items.len(), MAX_TOASTS);
        assert_eq!(t.items.front().map(|(s, _)| s.as_str()), Some("n6"));
    }

    #[test]
    fn counter_only_in_collectible_chapters() {
        let mut saves = SaveSession::in_memory();
        assert_eq!(counter_line(&saves, Some(ChapterId::Workshop)), None);
        assert_eq!(counter_line(&saves, None), None);
        saves.progression.add_collectible(
            ChapterId::Village,
            "TheWorld.PersistentLevel.ASAMUCollectible_0",
        );
        assert_eq!(
            counter_line(&saves, Some(ChapterId::Village)).as_deref(),
            Some("Collectibles 1/5 | total 1/25")
        );
        let trial = Play {
            chapter: Some(ChapterId::Village),
            mode: PlayMode::TimeTrial,
            map: None,
        };
        assert_eq!(
            counter_line_for(&saves, &trial),
            None,
            "hidden in time trial"
        );
    }
}
