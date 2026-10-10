//! Time-trial rules: the stopwatch, the restart rules, the HUD's target
//! medal, and the level-selection entries
//! (`docs/reverse-engineering/TIME_TRIAL.md`; best times and medals are
//! kept by [`crate::save`]).
//!
//! The original splits a run across three objects (local reading of the
//! shipped script, CONFIRMED src unless marked):
//!
//! - the game type `ASAMUGameInfoTimeTrial` keeps `bTimeTrialActive`: set by
//!   Kismet `SeqAct_StartTimeTrial`, cleared by `SeqAct_EndTimeTrial` (which
//!   does nothing unless it is set), and **not** cleared by a death;
//! - its HUD `ASAMUHUDTimeTrial` keeps the stopwatch as an actor timer
//!   (`Timerrun`, a rate that never fires): a start creates it only when it
//!   does not exist yet, the end pauses it and reports its count, a death
//!   before any checkpoint of the level clears it (stopped and gone: the
//!   display shows 0 until the next start). Actor timers count the game's
//!   frame time and stop while the game is paused; `IsTimerActive` is true
//!   for a paused timer too, so a start after the end is ignored and the
//!   frozen time stays on screen; the count of a missing timer is −1
//!   (CONFIRMED native: `AActor::IsTimerActive`, `GetTimerCount`,
//!   `PauseTimer`, `UpdateTimers`);
//! - the HUD movie `ASAMUHUDMovieTimeTrial` shows the time, the next target
//!   (gold first; each update that finds the time past the current target
//!   moves on by one; past bronze it shows its `NoDiceLabel` text and no
//!   time) and the best time with its medal, and stores the result at the
//!   end ([`crate::save::SaveSession::on_time_trial_end`]).
//!
//! In every shipped time-trial map (CONFIRMED map; exercised on converted
//! data by `tests/timetrial_real_data.rs`) the start is the touch of a
//! gate-like trigger volume the route passes through, 307 to 3,935 UU from
//! the respawn point of a run without a registered checkpoint; its event
//! re-fires without limit (`MaxTriggerCount` 0). The end is the touch of a
//! trigger at the level exit whose event is disabled in the map, turned on
//! by the level-start Kismet in time trial only, and fires once
//! (`MaxTriggerCount` 1); Kismet opens the front end 4 s later (5 s in
//! AG-BeautifulCity). So a death under the death rule leaves the player
//! outside the gate with a cleared stopwatch, and the run counts again from
//! the next crossing. "No checkpoint" means none beyond the level's first:
//! touching the first checkpoint registers nothing (ABILITIES.md A-CP-3).
//! F8 (`TimeTrialRestart`) reloads the level while `bTimeTrialActive` is
//! set. Time trial unlocks once the game was finished.
//!
//! [`TimeTrialRun`] is a deterministic model of the stopwatch in simulation
//! ticks; the app feeds it the Kismet start/end outputs and the respawns.

use serde::{Deserialize, Serialize};

use crate::save::{
    ChapterId, Medal, Progression, TIME_TRIAL_TARGETS, TimeTrialTimes, format_trial_time,
};

/// The count `GetTimerCount` returns for a timer that does not exist
/// (CONFIRMED native: the constant −1.0).
pub const MISSING_TIMER_COUNT: f64 = -1.0;

/// The HUD stopwatch (`ASAMUHUDTimeTrial`'s `Timerrun` timer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stopwatch {
    /// No timer (never started, or cleared by the death rule).
    #[default]
    Cleared,
    /// Counting since tick `start_tick`.
    Running {
        /// Simulation tick at which the count was 0.
        start_tick: u64,
    },
    /// Paused by the end of the run after `ticks` ticks.
    Paused {
        /// The frozen count, in ticks.
        ticks: u64,
    },
}

/// What [`TimeTrialRun::end`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EndOutcome {
    /// The run was not active: the end does nothing (the original's
    /// `EndTimeTrial` guard).
    Inactive,
    /// The run ended with this time (s).
    Finished(f64),
    /// The run ended while the stopwatch was cleared (a death before any
    /// checkpoint and no new start since): the original reports −1 and
    /// would store it as the best time, losing the real best (quirk TT-Q1,
    /// TIME_TRIAL.md). Nothing is recorded here.
    NoTime,
}

/// The HUD's "next target" (`currenttarget` of the HUD movie).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Gold target.
    #[default]
    Gold,
    /// Silver target.
    Silver,
    /// Bronze target.
    Bronze,
    /// Past the bronze target (the HUD shows its `NoDiceLabel` text and no
    /// time).
    NoDice,
}

impl Target {
    /// The medal this target stands for (`None` for [`Target::NoDice`]).
    #[must_use]
    pub fn medal(self) -> Option<Medal> {
        match self {
            Self::Gold => Some(Medal::Gold),
            Self::Silver => Some(Medal::Silver),
            Self::Bronze => Some(Medal::Bronze),
            Self::NoDice => None,
        }
    }

    /// Index into a level's targets (gold 0, silver 1, bronze 2).
    #[must_use]
    pub fn index(self) -> Option<usize> {
        match self {
            Self::Gold => Some(0),
            Self::Silver => Some(1),
            Self::Bronze => Some(2),
            Self::NoDice => None,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Gold => Self::Silver,
            Self::Silver => Self::Bronze,
            Self::Bronze | Self::NoDice => Self::NoDice,
        }
    }
}

/// One time-trial run on one map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeTrialRun {
    /// The chapter (`None`: a map without time-trial targets).
    pub chapter: Option<ChapterId>,
    /// `bTimeTrialActive`.
    pub active: bool,
    /// The stopwatch.
    pub stopwatch: Stopwatch,
    /// The HUD's next target.
    pub target: Target,
    /// The last finished time (s).
    pub finished: Option<f64>,
    /// Simulation tick rate (Hz) used to turn ticks into seconds.
    pub tick_rate_hz: f64,
}

impl TimeTrialRun {
    /// A run on `chapter` at the game's tick rate. The HUD starts with the
    /// gold target shown (`ASAMUHUDMovieTimeTrial.Init`).
    #[must_use]
    pub fn new(chapter: Option<ChapterId>, tick_rate_hz: f64) -> Self {
        Self {
            chapter,
            active: false,
            stopwatch: Stopwatch::Cleared,
            target: Target::Gold,
            finished: None,
            tick_rate_hz,
        }
    }

    /// `ticks` in seconds; 0 for a tick rate that is not a positive finite
    /// number or too small to give a finite time.
    fn seconds(&self, ticks: u64) -> f64 {
        if self.tick_rate_hz > 0.0 && self.tick_rate_hz.is_finite() {
            let s = ticks as f64 / self.tick_rate_hz;
            if s.is_finite() { s } else { 0.0 }
        } else {
            0.0
        }
    }

    /// Kismet `SeqAct_StartTimeTrial` at `tick`: the run becomes active and
    /// a stopwatch that does not exist starts from 0. A running or paused
    /// stopwatch is left alone.
    pub fn start(&mut self, tick: u64) {
        self.active = true;
        if self.stopwatch == Stopwatch::Cleared {
            self.stopwatch = Stopwatch::Running { start_tick: tick };
        }
    }

    /// Kismet `SeqAct_EndTimeTrial` at `tick`.
    pub fn end(&mut self, tick: u64) -> EndOutcome {
        if !self.active {
            return EndOutcome::Inactive;
        }
        self.active = false;
        match self.stopwatch {
            Stopwatch::Cleared => EndOutcome::NoTime,
            Stopwatch::Running { start_tick } => {
                let ticks = tick.saturating_sub(start_tick);
                self.stopwatch = Stopwatch::Paused { ticks };
                let t = self.seconds(ticks);
                self.finished = Some(t);
                EndOutcome::Finished(t)
            }
            Stopwatch::Paused { ticks } => {
                let t = self.seconds(ticks);
                self.finished = Some(t);
                EndOutcome::Finished(t)
            }
        }
    }

    /// The game's death hook at the player reset (`PlayerDied`, 0.3 s into
    /// the death sequence): with no checkpoint registered in this level the
    /// stopwatch is cleared (the run stays active). `latest_checkpoint` is
    /// the level's latest checkpoint index, `None` for −1.
    pub fn on_player_reset(&mut self, latest_checkpoint: Option<i32>) {
        if latest_checkpoint.is_none_or(|i| i < 0) {
            self.stopwatch = Stopwatch::Cleared;
        }
    }

    /// F8 (`TimeTrialRestart`): the level may be restarted.
    #[must_use]
    pub fn restart_allowed(&self) -> bool {
        self.active
    }

    /// The stopwatch count at `tick` (s): what `GetTimerCount` reports
    /// ([`MISSING_TIMER_COUNT`] without a timer).
    #[must_use]
    pub fn count(&self, tick: u64) -> f64 {
        match self.stopwatch {
            Stopwatch::Cleared => MISSING_TIMER_COUNT,
            Stopwatch::Running { start_tick } => self.seconds(tick.saturating_sub(start_tick)),
            Stopwatch::Paused { ticks } => self.seconds(ticks),
        }
    }

    /// The time on the HUD at `tick`: the count, 0 without a timer (the
    /// clear shows 0).
    #[must_use]
    pub fn display_seconds(&self, tick: u64) -> f64 {
        self.count(tick).max(0.0)
    }

    /// The stopwatch is counting.
    #[must_use]
    pub fn running(&self) -> bool {
        matches!(self.stopwatch, Stopwatch::Running { .. })
    }

    /// The HUD update of a frame at `tick` (the original runs it every HUD
    /// tick while a timer exists): moves the target on by one when the
    /// time is past it.
    pub fn update_target(&mut self, tick: u64) {
        if self.stopwatch == Stopwatch::Cleared {
            return;
        }
        let Some(targets) = self.targets() else {
            return;
        };
        let time = self.count(tick);
        if let Some(i) = self.target.index()
            && let Some(t) = targets.get(i)
            && time > f64::from(*t)
        {
            self.target = self.target.next();
        }
    }

    /// The level's gold, silver and bronze targets (s).
    #[must_use]
    pub fn targets(&self) -> Option<[f32; 3]> {
        targets_for(self.chapter?)
    }

    /// The seconds of the shown target (`None` after bronze).
    #[must_use]
    pub fn target_seconds(&self) -> Option<f32> {
        self.targets()?.get(self.target.index()?).copied()
    }
}

/// The gold, silver and bronze targets of a chapter (s), CONFIRMED (cdo:
/// `TimeTrialSavefile.LevelTargetScores`).
#[must_use]
pub fn targets_for(chapter: ChapterId) -> Option<[f32; 3]> {
    TIME_TRIAL_TARGETS.get(chapter.time_trial_slot()?).copied()
}

/// One entry of the time-trial level selection (the main menu's
/// `ttmap*` buttons, in their order: Sanctuary, Village, Darkcave,
/// StarHaven, IceCave).
#[derive(Clone, Debug, PartialEq)]
pub struct TimeTrialEntry {
    /// The chapter.
    pub chapter: ChapterId,
    /// Map package the entry opens (with the time-trial game type).
    pub map: &'static str,
    /// Selectable: the game was finished and the map is available.
    pub enabled: bool,
    /// Best time (s).
    pub best: Option<f32>,
    /// Medal of the best time.
    pub medal: Option<Medal>,
    /// Gold, silver and bronze targets (s).
    pub targets: [f32; 3],
}

impl TimeTrialEntry {
    /// The best time in the menu format (`--:--:--` without one).
    #[must_use]
    pub fn best_text(&self) -> String {
        self.best.map_or_else(
            || "--:--:--".to_owned(),
            |b| format_trial_time(f64::from(b)),
        )
    }
}

/// The level-selection entries: unlocked once the game was finished
/// (CONFIRMED src: the menu disables its time-trial button until
/// `bFinishedGame`), each enabled when `available(chapter)` (the app: the
/// map is converted).
#[must_use]
pub fn level_entries(
    progression: &Progression,
    times: &TimeTrialTimes,
    available: impl Fn(ChapterId) -> bool,
) -> Vec<TimeTrialEntry> {
    let unlocked = progression.time_trial_unlocked();
    ChapterId::WITH_COLLECTIBLES
        .into_iter()
        .zip(TIME_TRIAL_TARGETS)
        .map(|(chapter, targets)| TimeTrialEntry {
            chapter,
            map: chapter.map_name(),
            enabled: unlocked && available(chapter),
            best: times.best.get(&chapter).copied(),
            medal: times.medal(chapter),
            targets,
        })
        .collect()
}

/// What the time-trial HUD shows (the original's three text fields:
/// current time, target time, best time with its medal).
#[derive(Clone, Debug, PartialEq)]
pub struct HudView {
    /// Current time.
    pub time: String,
    /// The target medal (`None`: past bronze).
    pub target: Option<Medal>,
    /// The target time (`None`: past bronze).
    pub target_time: Option<String>,
    /// Best time (`None`: no time yet).
    pub best: Option<String>,
    /// Medal of the best time.
    pub best_medal: Option<Medal>,
}

/// The HUD view of `run` at `tick` with the stored best `times`.
#[must_use]
pub fn hud_view(run: &TimeTrialRun, times: &TimeTrialTimes, tick: u64) -> HudView {
    let best = run.chapter.and_then(|c| times.best.get(&c).copied());
    HudView {
        time: format_trial_time(run.display_seconds(tick)),
        target: run.target.medal(),
        target_time: run
            .target_seconds()
            .map(|s| format_trial_time(f64::from(s))),
        best: best.map(|b| format_trial_time(f64::from(b))),
        best_medal: run.chapter.and_then(|c| times.medal(c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwatch_counts_ticks_from_the_start() {
        let mut r = TimeTrialRun::new(Some(ChapterId::Village), 60.0);
        assert_eq!(r.count(10), MISSING_TIMER_COUNT);
        assert_eq!(r.display_seconds(10), 0.0);
        r.start(60);
        assert!(r.active && r.running());
        assert_eq!(r.count(180), 2.0);
        // A second start is ignored while the timer exists.
        r.start(170);
        assert_eq!(r.count(240), 3.0);
        assert_eq!(r.end(240), EndOutcome::Finished(3.0));
        assert!(!r.active);
        assert_eq!(r.count(1000), 3.0, "frozen after the end");
        assert_eq!(r.end(1000), EndOutcome::Inactive);
    }

    #[test]
    fn zero_rate_counts_nothing() {
        let mut r = TimeTrialRun::new(Some(ChapterId::Village), 0.0);
        r.start(0);
        assert_eq!(r.count(600), 0.0);
    }
}
