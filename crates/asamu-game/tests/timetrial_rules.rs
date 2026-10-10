//! Time-trial rules (`asamu_game::timetrial`, TIME_TRIAL.md): the
//! stopwatch, the death rule, the restart rule, the HUD target, the level
//! selection and the hand-off to the save session. Synthetic, no game data.

use asamu_game::save::{
    Achievement, ChapterId, Medal, PlayMode, Progression, SaveSession, TIME_TRIAL_TARGETS,
    TimeTrialTimes,
};
use asamu_game::timetrial::{
    EndOutcome, MISSING_TIMER_COUNT, Stopwatch, Target, TimeTrialRun, hud_view, level_entries,
    targets_for,
};

const HZ: f64 = 60.0;

fn secs(s: u64) -> u64 {
    s * 60
}

#[test]
fn a_death_before_any_checkpoint_clears_the_stopwatch_until_the_next_start() {
    let mut r = TimeTrialRun::new(Some(ChapterId::Sanctuary), HZ);
    r.start(secs(1));
    assert_eq!(r.count(secs(11)), 10.0);
    // Death with no checkpoint registered: cleared, shown as 0, still active.
    r.on_player_reset(None);
    assert_eq!(r.stopwatch, Stopwatch::Cleared);
    assert_eq!(r.count(secs(12)), MISSING_TIMER_COUNT);
    assert_eq!(r.display_seconds(secs(12)), 0.0);
    assert!(r.active, "bTimeTrialActive survives the clear");
    assert!(r.restart_allowed(), "F8 still restarts the level");
    // The start volume is touched again: counting from that touch.
    r.start(secs(20));
    assert_eq!(r.count(secs(25)), 5.0);
    // A death after a checkpoint keeps counting.
    r.on_player_reset(Some(3));
    assert_eq!(r.count(secs(30)), 10.0);
    r.on_player_reset(Some(-1));
    assert_eq!(r.stopwatch, Stopwatch::Cleared, "index −1 means none");
}

#[test]
fn the_end_pauses_and_reports_once() {
    let mut r = TimeTrialRun::new(Some(ChapterId::Village), HZ);
    assert_eq!(r.end(secs(5)), EndOutcome::Inactive, "not started");
    r.start(0);
    assert_eq!(r.end(secs(199)), EndOutcome::Finished(199.0));
    assert!(!r.active && !r.restart_allowed());
    assert_eq!(r.finished, Some(199.0));
    // The frozen time stays; a start after the end does not restart it
    // (the paused timer still counts as active).
    r.start(secs(300));
    assert!(r.active);
    assert_eq!(r.count(secs(400)), 199.0);
    // A second end reports the same frozen time.
    assert_eq!(r.end(secs(400)), EndOutcome::Finished(199.0));
}

#[test]
fn ending_a_cleared_run_reports_no_time() {
    let mut r = TimeTrialRun::new(Some(ChapterId::IceCave), HZ);
    r.start(0);
    r.on_player_reset(None);
    assert_eq!(r.end(secs(900)), EndOutcome::NoTime);
    assert_eq!(r.finished, None);
}

#[test]
fn the_hud_target_moves_on_one_medal_per_update() {
    let mut r = TimeTrialRun::new(Some(ChapterId::Village), HZ);
    assert_eq!(r.target, Target::Gold, "gold is shown from the start");
    assert_eq!(r.target_seconds(), Some(200.0));
    r.update_target(secs(500));
    assert_eq!(r.target, Target::Gold, "no timer, no update");
    r.start(0);
    r.update_target(secs(200));
    assert_eq!(r.target, Target::Gold, "at the target is still within it");
    r.update_target(secs(200) + 1);
    assert_eq!(r.target, Target::Silver);
    // Far past every target: one step per update.
    r.update_target(secs(1000));
    assert_eq!(r.target, Target::Bronze);
    r.update_target(secs(1000));
    assert_eq!(r.target, Target::NoDice);
    assert_eq!(r.target.medal(), None);
    assert_eq!(r.target_seconds(), None);
    r.update_target(secs(2000));
    assert_eq!(r.target, Target::NoDice);
    // A restart does not reset the shown target (the HUD keeps it until
    // the level reloads).
    r.on_player_reset(None);
    r.start(secs(2000));
    assert_eq!(r.target, Target::NoDice);
    // Maps without targets never move.
    let mut none = TimeTrialRun::new(None, HZ);
    none.start(0);
    none.update_target(secs(10_000));
    assert_eq!(none.target, Target::Gold);
    assert_eq!(none.targets(), None);
}

#[test]
fn targets_are_the_class_defaults() {
    assert_eq!(
        targets_for(ChapterId::Sanctuary),
        Some([260.0, 290.0, 320.0])
    );
    assert_eq!(
        targets_for(ChapterId::IceCave),
        Some([810.0, 960.0, 1150.0])
    );
    assert_eq!(targets_for(ChapterId::Workshop), None);
    assert_eq!(targets_for(ChapterId::Epilogue), None);
}

#[test]
fn level_selection_unlocks_with_the_finished_game() {
    let mut p = Progression::default();
    let mut t = TimeTrialTimes::default();
    t.record(ChapterId::DarkCave, 250.0);
    let locked = level_entries(&p, &t, |_| true);
    assert_eq!(locked.len(), 5);
    assert!(locked.iter().all(|e| !e.enabled));
    p.finished_game = true;
    let entries = level_entries(&p, &t, |c| c != ChapterId::StarHaven);
    let order: Vec<_> = entries.iter().map(|e| e.chapter).collect();
    assert_eq!(order, ChapterId::WITH_COLLECTIBLES.to_vec());
    assert_eq!(
        entries.iter().map(|e| e.map).collect::<Vec<_>>(),
        [
            "AG-ParadiseCave",
            "AG-BeautifulCity",
            "AG-Darkcave",
            "AG-StarHaven",
            "AG-IceCave"
        ]
    );
    let dark = &entries[2];
    assert!(dark.enabled);
    assert_eq!(dark.best, Some(250.0));
    assert_eq!(dark.medal, Some(Medal::Silver));
    assert_eq!(dark.best_text(), "04:10:00");
    assert_eq!(dark.targets, TIME_TRIAL_TARGETS[2]);
    assert!(!entries[3].enabled, "not available");
    assert_eq!(entries[0].best_text(), "--:--:--");
}

#[test]
fn hud_view_shows_time_target_and_best() {
    let mut t = TimeTrialTimes::default();
    t.record(ChapterId::Sanctuary, 255.5);
    let mut r = TimeTrialRun::new(Some(ChapterId::Sanctuary), HZ);
    let v = hud_view(&r, &t, 0);
    assert_eq!(v.time, "00:00:00");
    assert_eq!(v.target, Some(Medal::Gold));
    assert_eq!(v.target_time.as_deref(), Some("04:20:00"));
    assert_eq!(v.best.as_deref(), Some("04:15:50"));
    assert_eq!(v.best_medal, Some(Medal::Gold));
    r.start(0);
    let v = hud_view(&r, &t, secs(61) + 15);
    assert_eq!(v.time, "01:01:25");
    let none = hud_view(&r, &TimeTrialTimes::default(), 0);
    assert_eq!(none.best, None);
    assert_eq!(none.best_medal, None);
}

#[test]
fn a_finished_run_goes_into_the_save_session() {
    let mut s = SaveSession::in_memory();
    s.progression.finished_game = true;
    assert!(s.start_time_trial(ChapterId::Village));
    assert_eq!(s.mode(), PlayMode::TimeTrial);
    let mut r = TimeTrialRun::new(Some(ChapterId::Village), HZ);
    r.start(0);
    let EndOutcome::Finished(t) = r.end(secs(199)) else {
        panic!("finished");
    };
    #[allow(clippy::cast_possible_truncation)]
    let out = s.on_time_trial_end(ChapterId::Village, t as f32).unwrap();
    assert!(out.new_best);
    assert_eq!(out.medal, Some(Medal::Gold));
    assert!(!out.all_gold);
    // Four more golds earn ALL_GOLD_MEDALS.
    for (c, [gold, ..]) in ChapterId::WITH_COLLECTIBLES
        .into_iter()
        .zip(TIME_TRIAL_TARGETS)
    {
        if c != ChapterId::Village {
            s.on_time_trial_end(c, gold).unwrap();
        }
    }
    assert!(
        s.progression
            .achievements
            .contains(&Achievement::ALL_GOLD_MEDALS)
    );
}

// ---------------------------------------------------------------------------
// Verification pass: boundaries, the HUD after the end, the save hand-off of
// the quirks, damaged clocks.
// ---------------------------------------------------------------------------

#[test]
fn the_hud_target_and_the_medal_agree_at_the_boundary() {
    // The HUD moves on only when the time is strictly past the target; the
    // medal rule accepts a time equal to the target (`score <= target`).
    for (chapter, targets) in ChapterId::WITH_COLLECTIBLES
        .into_iter()
        .zip(TIME_TRIAL_TARGETS)
    {
        for (i, (target, medal)) in targets
            .into_iter()
            .zip([Medal::Gold, Medal::Silver, Medal::Bronze])
            .enumerate()
        {
            let mut r = TimeTrialRun::new(Some(chapter), HZ);
            r.start(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let at = secs(target as u64);
            // Walk the HUD to this target, then stand exactly on it.
            for _ in 0..i {
                r.update_target(at);
            }
            r.update_target(at);
            assert_eq!(r.target.index(), Some(i), "{chapter:?} target {i}");
            assert_eq!(r.end(at), EndOutcome::Finished(f64::from(target)));
            assert_eq!(TimeTrialTimes::medal_for(chapter, target), Some(medal));
            // One tick later the HUD has moved on and the medal is the next.
            let mut late = TimeTrialRun::new(Some(chapter), HZ);
            late.start(0);
            for _ in 0..=i {
                late.update_target(at + 1);
            }
            assert_eq!(late.target.index(), [Some(1), Some(2), None][i]);
            let EndOutcome::Finished(t) = late.end(at + 1) else {
                panic!("finished");
            };
            #[allow(clippy::cast_possible_truncation)]
            let next = TimeTrialTimes::medal_for(chapter, t as f32);
            assert_eq!(
                next,
                [Some(Medal::Silver), Some(Medal::Bronze), None][i],
                "{chapter:?} just past target {i}"
            );
        }
    }
}

#[test]
fn the_frozen_time_keeps_moving_the_target_after_the_end() {
    // The HUD keeps updating with the paused timer's count (a paused timer
    // counts as active), so a finish far past bronze still walks the target
    // one step per update.
    let mut r = TimeTrialRun::new(Some(ChapterId::DarkCave), HZ);
    r.start(0);
    assert_eq!(r.end(secs(2000)), EndOutcome::Finished(2000.0));
    assert_eq!(r.target, Target::Gold);
    r.update_target(secs(2001));
    r.update_target(secs(2002));
    assert_eq!(r.target, Target::Bronze);
    r.update_target(secs(2003));
    assert_eq!(r.target, Target::NoDice);
    assert_eq!(r.display_seconds(secs(9999)), 2000.0);
}

#[test]
fn a_cleared_end_never_reaches_the_stored_best() {
    // Quirk TT-Q1: the original would store −1 over the real best. Our end
    // reports no time, and the save session refuses −1, 0 and non-finite
    // times even when handed one.
    let mut s = SaveSession::in_memory();
    s.progression.finished_game = true;
    assert!(s.start_time_trial(ChapterId::StarHaven));
    assert!(
        s.on_time_trial_end(ChapterId::StarHaven, 500.0)
            .unwrap()
            .new_best
    );
    let mut r = TimeTrialRun::new(Some(ChapterId::StarHaven), HZ);
    r.start(0);
    r.on_player_reset(None);
    assert_eq!(r.count(secs(700)), MISSING_TIMER_COUNT);
    assert_eq!(r.end(secs(700)), EndOutcome::NoTime);
    assert!(!r.active, "the end still clears bTimeTrialActive");
    assert_eq!(r.end(secs(701)), EndOutcome::Inactive);
    for bad in [-1.0, 0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let out = s.on_time_trial_end(ChapterId::StarHaven, bad).unwrap();
        assert!(!out.new_best, "{bad}");
        assert_eq!(out.medal, Some(Medal::Silver));
    }
    assert_eq!(s.time_trial.best.get(&ChapterId::StarHaven), Some(&500.0));
}

#[test]
fn only_a_strictly_faster_time_is_a_new_best() {
    // `lasttime < best || best <= 0` (TIME_TRIAL.md TT-7).
    let mut s = SaveSession::in_memory();
    s.progression.finished_game = true;
    assert!(s.start_time_trial(ChapterId::IceCave));
    let end = |s: &mut SaveSession, t: f32| s.on_time_trial_end(ChapterId::IceCave, t).unwrap();
    assert!(end(&mut s, 900.0).new_best, "the first time");
    assert!(!end(&mut s, 900.0).new_best, "an equal time");
    assert!(!end(&mut s, 1200.0).new_best, "a slower time");
    let out = end(&mut s, 809.99);
    assert!(out.new_best);
    assert_eq!(out.medal, Some(Medal::Gold));
    assert_eq!(s.time_trial.best.get(&ChapterId::IceCave), Some(&809.99));
    // Chapters without a time trial store nothing.
    assert!(
        !s.on_time_trial_end(ChapterId::Workshop, 10.0)
            .unwrap()
            .new_best
    );
    assert!(!s.time_trial.best.contains_key(&ChapterId::Workshop));
}

#[test]
fn damaged_clocks_never_panic_or_go_non_finite() {
    for hz in [60.0, 0.0, -60.0, f64::NAN, f64::INFINITY, f64::MIN_POSITIVE] {
        let mut r = TimeTrialRun::new(Some(ChapterId::Village), hz);
        // A start "after" the ticks asked about (a clock that restarted).
        r.start(u64::MAX);
        for tick in [0, 1, u64::MAX / 2, u64::MAX] {
            let c = r.count(tick);
            assert!(c.is_finite(), "{hz}: {c}");
            assert!(r.display_seconds(tick) >= 0.0, "{hz}");
            r.update_target(tick);
            let _ = hud_view(&r, &TimeTrialTimes::default(), tick);
        }
        assert_eq!(r.count(0), 0.0, "{hz}: a tick before the start counts 0");
        let out = r.end(0);
        assert_eq!(out, EndOutcome::Finished(0.0), "{hz}");
        // A zero time is not a time for the save.
        assert!(!TimeTrialTimes::default().record(ChapterId::Village, 0.0));
    }
    // The largest tick count stays finite at the game's rate.
    let mut r = TimeTrialRun::new(Some(ChapterId::Village), HZ);
    r.start(0);
    assert!(r.count(u64::MAX).is_finite());
    assert_eq!(
        hud_view(&r, &TimeTrialTimes::default(), u64::MAX).time,
        "99:99:99"
    );
}

#[test]
fn two_runs_fed_the_same_events_agree() {
    // Deterministic: plain integer ticks, no clock of its own.
    let feed = |r: &mut TimeTrialRun| {
        r.start(3);
        r.on_player_reset(Some(-1));
        r.start(40);
        r.update_target(secs(201) + 40);
        r.on_player_reset(Some(2));
        r.end(secs(230) + 40)
    };
    let (mut a, mut b) = (
        TimeTrialRun::new(Some(ChapterId::Village), HZ),
        TimeTrialRun::new(Some(ChapterId::Village), HZ),
    );
    assert_eq!(feed(&mut a), feed(&mut b));
    assert_eq!(a, b);
    assert_eq!(a.finished, Some(230.0));
    assert_eq!(a.target, Target::Silver);
}

#[test]
fn a_run_without_a_chapter_shows_no_target_and_no_best() {
    let mut r = TimeTrialRun::new(None, HZ);
    r.start(0);
    let mut t = TimeTrialTimes::default();
    t.record(ChapterId::Village, 100.0);
    let v = hud_view(&r, &t, secs(5));
    assert_eq!(v.time, "00:05:00");
    assert_eq!(v.target, Some(Medal::Gold), "the HUD starts on gold");
    assert_eq!(v.target_time, None, "no targets for this map");
    assert_eq!(v.best, None);
    assert_eq!(v.best_medal, None);
    assert_eq!(r.end(secs(5)), EndOutcome::Finished(5.0));
}
