//! Time-trial HUD: the original's target and best-time fields
//! (`ASAMUHUDMovieTimeTrial`, TIME_TRIAL.md) next to the menu flow's
//! stopwatch.
//!
//! The run is modelled by [`asamu_game::timetrial::TimeTrialRun`], fed with
//! the same integration messages the menu flow reads: Kismet's
//! `SeqAct_StartTimeTrial` / `SeqAct_EndTimeTrial` ([`crate::ui::TimeTrialStart`],
//! [`crate::ui::TimeTrialEnd`]) and each fixed tick's report
//! ([`crate::ui::GameTick`]: the death rule at the player reset, the HUD
//! target update). The panel shows the next target medal with its time
//! ("gold 04:20:00"; after bronze "no medal") and the stored best time with
//! its medal, in time-trial play while no menu is open.
//!
//! Hooks for the UI: [`TimeTrialHud::run`] is the faithful run state
//! (stopwatch cleared by a death before any checkpoint, restart allowed
//! while the run is active), and `asamu_game::timetrial::level_entries`
//! builds the time-trial level selection.
//!
//! The plugin also keeps the time-trial HUD's other rule: no tutorial
//! pop-ups (`ASAMUHUDTimeTrial.PushTutorial` is empty), so what Kismet asks
//! to show in time-trial play is dropped before it reaches the overlay
//! ([`drop_tutorial`]).
//!
//! Verified on converted data by `asamu-game`'s ignored test
//! `timetrial_real_data` (the Kismet start gate, the death rule, the end
//! trigger and the return to the front end on all five maps); the app-side
//! wiring is covered by the headless tests below.

use asamu_game::save::{Medal, PlayMode};
use asamu_game::timetrial::{TimeTrialRun, hud_view};
use bevy::prelude::*;

use crate::Sim;
use crate::kismet::Presentation;
use crate::ui::{GameTick, Play, Saves, Screen, TimeTrialEnd, TimeTrialStart, UiState};

/// The current run (`None` outside time-trial play).
#[derive(Resource, Debug, Default)]
pub struct TimeTrialHud {
    /// The run of the loaded time-trial map.
    pub run: Option<TimeTrialRun>,
    /// Map the run belongs to.
    map: Option<String>,
    /// The game's tick when the run was last looked at (a game whose tick
    /// went back is a new game).
    last_tick: u64,
}

#[derive(Component)]
struct TargetText;

/// Time trial mode UI and flow.
pub struct TimeTrialPlugin;

impl Plugin for TimeTrialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimeTrialHud>()
            .add_systems(Startup, spawn_panel)
            .add_systems(
                Update,
                (track_run, feed_kismet, update_panel)
                    .chain()
                    .after(crate::ui::UiFlowSet),
            )
            .add_systems(FixedPostUpdate, feed_ticks)
            // Between Kismet's routing and the pop-up's timer and overlay.
            .add_systems(
                Update,
                suppress_tutorials
                    .after(crate::kismet::route_frames)
                    .before(crate::kismet::update_presentation),
            );
    }
}

/// The time-trial HUD shows no tutorial pop-ups: `SeqAct_ShowTutorialPopup`
/// calls the HUD's `PushTutorial`, which `ASAMUHUDTimeTrial` overrides with
/// an empty body (CONFIRMED src; TIME_TRIAL.md TT-8). Four of the five
/// maps run such an action at the end of a trial. Returns whether a pop-up
/// was dropped.
pub(crate) fn drop_tutorial(mode: PlayMode, presentation: &mut Presentation) -> bool {
    mode == PlayMode::TimeTrial && presentation.tutorial.take().is_some()
}

fn suppress_tutorials(play: Option<Res<Play>>, presentation: Option<ResMut<Presentation>>) {
    let (Some(play), Some(mut presentation)) = (play, presentation) else {
        return;
    };
    // Only touch the resource when there is something to drop.
    if play.mode == PlayMode::TimeTrial && presentation.tutorial.is_some() {
        drop_tutorial(play.mode, &mut presentation);
    }
}

/// Starts a fresh run whenever a new game starts in time-trial play (a new
/// simulation resource, a game whose clock went back — the level was
/// restarted in place — or another map), drops it otherwise. A level
/// restart reloads the original's HUD too, so the target starts at gold
/// again (TIME_TRIAL.md TT-5).
fn track_run(mut hud: ResMut<TimeTrialHud>, sim: Option<Res<Sim>>, play: Option<Res<Play>>) {
    let (Some(sim), Some(play)) = (sim, play) else {
        *hud = TimeTrialHud::default();
        return;
    };
    if play.mode != PlayMode::TimeTrial {
        *hud = TimeTrialHud::default();
        return;
    }
    let tick = sim.game.clock().tick();
    let restarted = tick < hud.last_tick;
    hud.last_tick = tick;
    if hud.run.is_none() || sim.is_added() || restarted || hud.map != play.map {
        hud.run = Some(TimeTrialRun::new(
            play.chapter,
            sim.game.clock().tick_rate_hz(),
        ));
        hud.map.clone_from(&play.map);
    }
}

fn feed_kismet(
    mut hud: ResMut<TimeTrialHud>,
    mut starts: MessageReader<TimeTrialStart>,
    mut ends: MessageReader<TimeTrialEnd>,
    sim: Option<Res<Sim>>,
) {
    let tick = sim.as_ref().map_or(0, |s| s.game.clock().tick());
    let Some(run) = hud.run.as_mut() else {
        starts.clear();
        ends.clear();
        return;
    };
    for _ in starts.read() {
        run.start(tick);
    }
    for _ in ends.read() {
        let out = run.end(tick);
        debug!("time trial end: {out:?}");
    }
}

/// After each fixed tick: the death rule at the player reset (no
/// checkpoint registered in this level clears the stopwatch) and the HUD
/// target update.
fn feed_ticks(
    mut hud: ResMut<TimeTrialHud>,
    mut ticks: MessageReader<GameTick>,
    sim: Option<Res<Sim>>,
) {
    let (Some(run), Some(sim)) = (hud.run.as_mut(), sim) else {
        ticks.clear();
        return;
    };
    for GameTick(report) in ticks.read() {
        if report.respawned {
            run.on_player_reset(sim.game.latest_checkpoint_index());
        }
        run.update_target(report.tick);
    }
}

fn spawn_panel(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: px(100),
            left: percent(30),
            right: percent(30),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(80),
        children![(
            TargetText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(18.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.9, 0.7)),
            TextLayout::justify(Justify::Center),
            TextShadow::default(),
        )],
    ));
}

fn medal_word(m: Option<Medal>) -> &'static str {
    match m {
        Some(Medal::Gold) => "gold",
        Some(Medal::Silver) => "silver",
        Some(Medal::Bronze) => "bronze",
        None => "no medal",
    }
}

/// The panel line of a run (empty without one).
#[must_use]
pub fn panel_line(run: Option<&TimeTrialRun>, saves: &Saves, tick: u64) -> String {
    let Some(run) = run else {
        return String::new();
    };
    let v = hud_view(run, &saves.0.time_trial, tick);
    let target = match (v.target, v.target_time) {
        (Some(m), Some(t)) => format!("Target {} {t}", medal_word(Some(m))),
        _ => "Target: no medal".to_owned(),
    };
    let best = match v.best {
        Some(b) => format!("Best {b} ({})", medal_word(v.best_medal)),
        None => "Best --:--:--".to_owned(),
    };
    format!("{target} | {best}")
}

fn update_panel(
    hud: Res<TimeTrialHud>,
    saves: Res<Saves>,
    state: Res<UiState>,
    sim: Option<Res<Sim>>,
    mut text: Query<&mut Text, With<TargetText>>,
) {
    let line = if state.screen == Screen::None {
        let tick = sim.as_ref().map_or(0, |s| s.game.clock().tick());
        panel_line(hud.run.as_ref(), &saves, tick)
    } else {
        String::new()
    };
    for mut t in &mut text {
        if t.0 != line {
            t.0.clone_from(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use asamu_game::save::ChapterId;

    use super::*;

    #[test]
    fn panel_shows_target_and_best() {
        let mut saves = Saves::default();
        let mut run = TimeTrialRun::new(Some(ChapterId::Village), 60.0);
        assert_eq!(panel_line(None, &saves, 0), "");
        assert_eq!(
            panel_line(Some(&run), &saves, 0),
            "Target gold 03:20:00 | Best --:--:--"
        );
        saves.0.time_trial.record(ChapterId::Village, 215.0);
        run.start(0);
        run.update_target(60 * 201);
        assert_eq!(
            panel_line(Some(&run), &saves, 60 * 201),
            "Target silver 03:40:00 | Best 03:35:00 (silver)"
        );
        run.update_target(60 * 900);
        run.update_target(60 * 900);
        assert_eq!(
            panel_line(Some(&run), &saves, 60 * 900),
            "Target: no medal | Best 03:35:00 (silver)"
        );
    }

    #[test]
    fn runs_follow_the_messages() {
        let mut app = App::new();
        app.add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .insert_resource(TimeTrialHud {
                run: Some(TimeTrialRun::new(Some(ChapterId::Sanctuary), 60.0)),
                map: Some("AG-ParadiseCave".to_owned()),
                last_tick: 0,
            })
            .add_systems(Update, feed_kismet);
        app.world_mut().write_message(TimeTrialStart);
        app.update();
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(run.active && run.running());
        app.world_mut().write_message(TimeTrialEnd);
        app.update();
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(!run.active);
        assert_eq!(run.finished, Some(0.0));
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

    fn sim() -> Sim {
        Sim::new(asamu_game::Game::graybox().unwrap(), String::new())
    }

    #[test]
    fn a_death_before_any_checkpoint_clears_the_stopwatch_until_the_next_start() {
        // The graybox game has no registered checkpoint (index −1 in the
        // original's terms).
        let mut run = TimeTrialRun::new(Some(ChapterId::Village), 60.0);
        run.start(0);
        let mut app = App::new();
        app.add_message::<GameTick>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .insert_resource(TimeTrialHud {
                run: Some(run),
                map: None,
                last_tick: 0,
            })
            .insert_resource(sim())
            .add_systems(Update, (feed_ticks, feed_kismet).chain());
        assert_eq!(
            app.world().resource::<Sim>().game.latest_checkpoint_index(),
            None
        );
        // Past the gold target: the HUD target moves on with the ticks.
        app.world_mut().write_message(tick(60 * 201, false));
        app.update();
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(run.running());
        assert_eq!(run.target.medal(), Some(Medal::Silver));
        // The death's reset: cleared, still active, the target kept.
        app.world_mut().write_message(tick(60 * 202, true));
        app.update();
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(!run.running() && run.active && run.restart_allowed());
        assert_eq!(run.display_seconds(60 * 300), 0.0);
        assert_eq!(run.target.medal(), Some(Medal::Silver));
        // More ticks do not start it; the start volume's touch does.
        app.world_mut().write_message(tick(60 * 203, false));
        app.update();
        assert!(
            !app.world()
                .resource::<TimeTrialHud>()
                .run
                .unwrap()
                .running()
        );
        app.world_mut().write_message(TimeTrialStart);
        app.update();
        assert!(
            app.world()
                .resource::<TimeTrialHud>()
                .run
                .unwrap()
                .running()
        );
        // An end while cleared records nothing (quirk TT-Q1).
        app.world_mut().write_message(tick(60 * 204, true));
        app.world_mut().write_message(TimeTrialEnd);
        app.update();
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(!run.active);
        assert_eq!(run.finished, None);
    }

    #[test]
    fn a_new_game_starts_a_fresh_run_and_story_play_has_none() {
        let mut app = App::new();
        app.init_resource::<TimeTrialHud>()
            .insert_resource(Play {
                chapter: Some(ChapterId::IceCave),
                mode: PlayMode::TimeTrial,
                map: Some("AG-IceCave".to_owned()),
            })
            .add_systems(Update, track_run);
        // No game yet: no run.
        app.update();
        assert!(app.world().resource::<TimeTrialHud>().run.is_none());
        app.insert_resource(sim());
        app.update();
        let fresh = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert_eq!(fresh.chapter, Some(ChapterId::IceCave));
        assert!(!fresh.active && !fresh.running());
        assert_eq!(fresh.target_seconds(), Some(810.0));
        // The run goes on while the game stays.
        if let Some(run) = app.world_mut().resource_mut::<TimeTrialHud>().run.as_mut() {
            run.start(0);
            run.update_target(60 * 2000);
        }
        app.update();
        let kept = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(kept.running());
        assert_eq!(kept.target.medal(), Some(Medal::Silver));
        // A level restart (F8) as the menu flow does it: the game goes
        // away while the map loads, then a new one arrives.
        app.world_mut().remove_resource::<Sim>();
        app.update();
        assert!(app.world().resource::<TimeTrialHud>().run.is_none());
        app.insert_resource(sim());
        app.update();
        assert_eq!(app.world().resource::<TimeTrialHud>().run, Some(fresh));
        // A game replaced in place (its clock went back) starts over too.
        {
            let mut sim = app.world_mut().resource_mut::<Sim>();
            sim.game.start();
            for _ in 0..3 {
                sim.game.tick(&asamu_player::InputFrame::default());
            }
            assert_eq!(sim.game.clock().tick(), 3);
        }
        if let Some(run) = app.world_mut().resource_mut::<TimeTrialHud>().run.as_mut() {
            run.start(1);
        }
        app.update();
        assert!(
            app.world()
                .resource::<TimeTrialHud>()
                .run
                .unwrap()
                .running()
        );
        app.insert_resource(sim());
        app.update();
        assert_eq!(app.world().resource::<TimeTrialHud>().run, Some(fresh));
        // Story play has no run.
        app.world_mut().resource_mut::<Play>().mode = PlayMode::Story;
        app.update();
        assert!(app.world().resource::<TimeTrialHud>().run.is_none());
        // Back in time trial without a game: still none.
        app.world_mut().resource_mut::<Play>().mode = PlayMode::TimeTrial;
        app.world_mut().remove_resource::<Sim>();
        app.update();
        assert!(app.world().resource::<TimeTrialHud>().run.is_none());
    }

    #[test]
    fn time_trial_shows_no_tutorial_pop_ups() {
        let pop_up = || Presentation {
            tutorial: Some((3, "text".to_owned(), Some(2.0))),
            ..Presentation::default()
        };
        // Story play keeps Kismet's pop-up.
        let mut p = pop_up();
        assert!(!drop_tutorial(PlayMode::Story, &mut p));
        assert!(p.tutorial.is_some());
        // Time trial drops it, and nothing else of the presentation.
        let mut p = pop_up();
        p.pause_menu = false;
        assert!(drop_tutorial(PlayMode::TimeTrial, &mut p));
        assert!(p.tutorial.is_none() && !p.pause_menu && p.crosshair);
        assert!(!drop_tutorial(PlayMode::TimeTrial, &mut p));
        // As a system: between Kismet's routing and the overlay.
        let mut app = App::new();
        app.insert_resource(pop_up())
            .insert_resource(Play {
                chapter: Some(ChapterId::DarkCave),
                mode: PlayMode::TimeTrial,
                map: Some("AG-Darkcave".to_owned()),
            })
            .add_systems(Update, suppress_tutorials);
        app.update();
        assert!(app.world().resource::<Presentation>().tutorial.is_none());
        app.insert_resource(pop_up());
        app.world_mut().resource_mut::<Play>().mode = PlayMode::Story;
        app.update();
        assert!(app.world().resource::<Presentation>().tutorial.is_some());
    }

    /// The plugin as registered, without a window or the rest of the app:
    /// its systems' parameters and orderings are valid, and a run shows in
    /// the panel from the start message to the end.
    #[test]
    fn the_plugin_runs_headless_from_start_to_end() {
        let panel = |app: &mut App| -> String {
            let world = app.world_mut();
            let mut q = world.query_filtered::<&Text, With<TargetText>>();
            q.single(world).map(|t| t.0.clone()).unwrap_or_default()
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<GameTick>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .init_resource::<Saves>()
            .init_resource::<UiState>()
            .init_resource::<Presentation>()
            .insert_resource(Play {
                chapter: Some(ChapterId::Village),
                mode: PlayMode::TimeTrial,
                map: Some("AG-BeautifulCity".to_owned()),
            })
            .add_plugins(TimeTrialPlugin);
        // No game yet: an empty panel.
        app.update();
        assert_eq!(panel(&mut app), "");
        app.insert_resource(sim());
        app.update();
        assert_eq!(panel(&mut app), "Target gold 03:20:00 | Best --:--:--");
        app.world_mut().write_message(TimeTrialStart);
        app.update();
        assert!(
            app.world()
                .resource::<TimeTrialHud>()
                .run
                .unwrap()
                .running()
        );
        // A pop-up Kismet asked for is dropped.
        app.world_mut().resource_mut::<Presentation>().tutorial =
            Some((1, "text".to_owned(), None));
        app.world_mut().write_message(TimeTrialEnd);
        app.update();
        assert!(app.world().resource::<Presentation>().tutorial.is_none());
        let run = app.world().resource::<TimeTrialHud>().run.unwrap();
        assert!(!run.active && run.finished.is_some());
        // A menu over the game hides the panel.
        app.world_mut()
            .resource_mut::<UiState>()
            .open(Screen::Pause);
        app.update();
        assert_eq!(panel(&mut app), "");
    }
}
