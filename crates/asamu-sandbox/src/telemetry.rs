//! Read-outs of a session: trail, speed and height history, jump and swing
//! statistics.
//!
//! [`Telemetry`] only **observes**: it is fed the game and the tick report
//! after each tick and never changes either. Jumps and swings are detected
//! from the simulation's own events (`StepEvents::{jumped, left_ground,
//! landed, landing}`, `GunEvents::{attached, released}`); the statistics are
//! measurements of this recreation, not of the original.
//!
//! How the statistics are measured (all in whole ticks):
//!
//! - A **jump** starts on the tick the pawn leaves the ground (a jump, or
//!   walking off a ledge) and ends on the tick it lands. Heights and
//!   distances are relative to where the pawn stood before the take-off
//!   tick. A jump that turns into a grapple swing, or is cut by a respawn or
//!   a teleport, is not reported.
//! - A **swing** starts on the tick the grapple attaches and ends on the
//!   tick it releases.
//!
//! The history is a ring of [`TELEMETRY_SAMPLES`] ticks. A discontinuity (a
//! respawn, a teleport, a loaded save state) ends the current *attempt*: its
//! trail moves to the archive ([`Telemetry::attempts`], the last
//! [`ATTEMPTS_KEPT`]) so the lines of successive tries can be compared.

use std::collections::VecDeque;

use asamu_game::{Game, TickReport};
use glam::Vec3;
use serde::Serialize;

/// Samples kept in the history ring (ten seconds at the default tick rate).
pub const TELEMETRY_SAMPLES: usize = 600;

/// Archived attempt trails kept by [`Telemetry::archive_attempt`].
pub const ATTEMPTS_KEPT: usize = 3;

/// One measured jump, from leaving the ground to landing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct JumpStats {
    /// Speed after the take-off tick, uu/s (includes the jump's vertical
    /// velocity).
    pub takeoff_speed: f32,
    /// Highest point above the take-off height, UU.
    pub apex_height: f32,
    /// Time in the air, seconds: the ticks from the take-off tick through
    /// the landing tick.
    pub airtime: f32,
    /// Horizontal distance from take-off to landing, UU.
    pub distance: f32,
    /// Vertical velocity at landing, uu/s (negative: downwards).
    pub landing_velocity_z: f32,
}

/// One measured grapple swing, from attach to release.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SwingStats {
    /// Distance from the pawn's centre to the anchor at attach, UU.
    pub attach_distance: f32,
    /// Time attached, seconds: the ticks from the attach tick through the
    /// release tick.
    pub duration: f32,
    /// Highest speed while attached, uu/s.
    pub peak_speed: f32,
    /// Speed handed over at release, uu/s.
    pub release_speed: f32,
}

/// A jump being measured.
#[derive(Clone, Copy, Debug)]
struct JumpTrack {
    origin: Vec3,
    takeoff_speed: f32,
    apex_z: f32,
    ticks: u32,
}

/// A swing being measured.
#[derive(Clone, Copy, Debug)]
struct SwingTrack {
    attach_distance: f32,
    peak_speed: f32,
    ticks: u32,
}

/// The session's history ring and latest measurements.
#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    trail: VecDeque<Vec3>,
    speed: VecDeque<f32>,
    height: VecDeque<f32>,
    peak_speed: f32,
    last_jump: Option<JumpStats>,
    last_swing: Option<SwingStats>,
    attempts: Vec<Vec<Vec3>>,
    jump: Option<JumpTrack>,
    swing: Option<SwingTrack>,
    /// Tick and collision-centre position of the previous observation.
    previous: Option<(u64, Vec3)>,
}

impl Telemetry {
    /// Records the state after a tick. Never changes the game.
    ///
    /// A respawn in `report`, or a tick that does not follow the previously
    /// observed one (a save state was loaded, the level changed), ends the
    /// current attempt first.
    pub fn observe(&mut self, game: &Game, report: &TickReport) {
        let player = game.player();
        if !(player.position.is_finite() && player.velocity.is_finite()) {
            // Nothing sensible to record; the simulation guards its own state.
            return;
        }
        let follows = self
            .previous
            .is_none_or(|(tick, _)| tick.checked_add(1) == Some(report.tick));
        if report.respawned || !follows {
            self.archive_attempt();
        }
        let dt = game.clock().dt();
        let position = player.position;
        let speed = player.speed();
        // Where the pawn was before this tick (for take-offs and attaches,
        // which happen at the start of a tick).
        let before = self.previous.map_or(position, |(_, p)| p);

        push_capped(&mut self.trail, position);
        push_capped(&mut self.speed, speed);
        push_capped(&mut self.height, position.z);
        self.peak_speed = self.peak_speed.max(speed);

        let events = &report.events;
        let gun = &events.gun;

        // Swings. A tick reports at most one attach and one release. With a
        // swing in progress the release ends it and an attach starts the
        // next one; otherwise the attach came first, and a release in the
        // same tick ends that very swing.
        let mut released = gun.released;
        if let Some(mut track) = self.swing.take() {
            track.ticks = track.ticks.saturating_add(1);
            track.peak_speed = track.peak_speed.max(speed);
            match released.take() {
                Some(release) => {
                    self.last_swing = Some(finish_swing(track, release.velocity.length(), dt));
                }
                None => self.swing = Some(track),
            }
        }
        if let Some(attached) = gun.attached
            && self.swing.is_none()
        {
            // A swing ends any jump being measured.
            self.jump = None;
            let track = SwingTrack {
                attach_distance: (attached.anchor - before).length(),
                peak_speed: speed,
                ticks: 1,
            };
            match released {
                Some(release) => {
                    self.last_swing = Some(finish_swing(track, release.velocity.length(), dt));
                }
                None => self.swing = Some(track),
            }
        }

        // Jumps.
        let landed = events
            .landed
            .or(events.landing.map(|landing| landing.velocity_z));
        if let Some(track) = &mut self.jump {
            track.ticks = track.ticks.saturating_add(1);
            track.apex_z = track.apex_z.max(position.z);
            if landed.is_some() || player.grounded {
                let offset = position - track.origin;
                self.last_jump = Some(JumpStats {
                    takeoff_speed: track.takeoff_speed,
                    apex_height: (track.apex_z - track.origin.z).max(0.0),
                    airtime: track.ticks as f32 * dt,
                    distance: offset.truncate().length(),
                    landing_velocity_z: landed.unwrap_or(player.velocity.z),
                });
                self.jump = None;
            }
        } else if (events.jumped || events.left_ground)
            && !player.grounded
            && self.swing.is_none()
            && gun.attached.is_none()
        {
            self.jump = Some(JumpTrack {
                origin: before,
                takeoff_speed: speed,
                apex_z: before.z.max(position.z),
                ticks: 1,
            });
        }

        self.previous = Some((report.tick, position));
    }

    /// Collision-centre positions of the latest ticks, oldest first (UU).
    pub fn trail(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.trail.iter().copied()
    }

    /// Speed per tick, oldest first (uu/s).
    pub fn speed_history(&self) -> impl Iterator<Item = f32> + '_ {
        self.speed.iter().copied()
    }

    /// Height (world Z of the collision centre) per tick, oldest first (UU).
    pub fn height_history(&self) -> impl Iterator<Item = f32> + '_ {
        self.height.iter().copied()
    }

    /// Highest speed seen since the session or the attempt started, uu/s.
    #[must_use]
    pub fn peak_speed(&self) -> f32 {
        self.peak_speed
    }

    /// The latest completed jump.
    #[must_use]
    pub fn last_jump(&self) -> Option<JumpStats> {
        self.last_jump
    }

    /// The latest completed swing.
    #[must_use]
    pub fn last_swing(&self) -> Option<SwingStats> {
        self.last_swing
    }

    /// Ends the current attempt: its trail moves to the archived attempts
    /// (keeping the last [`ATTEMPTS_KEPT`]; a trail of fewer than two points
    /// is not worth keeping), the history and the peak speed start again and
    /// a jump or swing being measured is dropped. The latest completed jump
    /// and swing stay.
    pub fn archive_attempt(&mut self) {
        if self.trail.len() >= 2 {
            self.attempts.push(self.trail.iter().copied().collect());
            let extra = self.attempts.len().saturating_sub(ATTEMPTS_KEPT);
            self.attempts.drain(..extra);
        }
        self.trail.clear();
        self.speed.clear();
        self.height.clear();
        self.peak_speed = 0.0;
        self.jump = None;
        self.swing = None;
        self.previous = None;
    }

    /// The archived attempt trails, oldest first.
    #[must_use]
    pub fn attempts(&self) -> &[Vec<Vec3>] {
        &self.attempts
    }

    /// A jump is being measured (the pawn is in the air after a take-off).
    #[must_use]
    pub fn measuring_jump(&self) -> bool {
        self.jump.is_some()
    }

    /// A swing is being measured (the grapple is attached).
    #[must_use]
    pub fn measuring_swing(&self) -> bool {
        self.swing.is_some()
    }
}

fn finish_swing(track: SwingTrack, release_speed: f32, dt: f32) -> SwingStats {
    SwingStats {
        attach_distance: track.attach_distance,
        duration: track.ticks as f32 * dt,
        peak_speed: track.peak_speed,
        release_speed,
    }
}

fn push_capped<T>(ring: &mut VecDeque<T>, value: T) {
    while ring.len() >= TELEMETRY_SAMPLES {
        ring.pop_front();
    }
    ring.push_back(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::InputFrame;

    fn forward() -> InputFrame {
        InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        }
    }

    #[test]
    fn the_history_is_a_ring_of_the_latest_ticks() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut telemetry = Telemetry::default();
        let mut last = Vec3::ZERO;
        for _ in 0..(TELEMETRY_SAMPLES + 25) {
            // Standing still: no respawn, so one attempt.
            let report = game.tick(&InputFrame::default()).unwrap();
            telemetry.observe(&game, &report);
            last = game.player().position;
        }
        assert_eq!(telemetry.trail().count(), TELEMETRY_SAMPLES);
        assert_eq!(telemetry.speed_history().count(), TELEMETRY_SAMPLES);
        assert_eq!(telemetry.height_history().count(), TELEMETRY_SAMPLES);
        assert_eq!(telemetry.trail().last(), Some(last));
        assert_eq!(telemetry.height_history().last(), Some(last.z));
        assert!(telemetry.attempts().is_empty());
    }

    #[test]
    fn archiving_keeps_the_last_attempts_and_restarts_the_history() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut telemetry = Telemetry::default();
        // An empty or one-point trail is not archived.
        telemetry.archive_attempt();
        let report = game.tick(&forward()).unwrap();
        telemetry.observe(&game, &report);
        telemetry.archive_attempt();
        assert!(telemetry.attempts().is_empty());

        let mut lengths = Vec::new();
        for attempt in 0..(ATTEMPTS_KEPT + 2) {
            let ticks = 3 + attempt;
            for _ in 0..ticks {
                let report = game.tick(&forward()).unwrap();
                telemetry.observe(&game, &report);
            }
            assert!(telemetry.peak_speed() > 0.0);
            lengths.push(ticks);
            telemetry.archive_attempt();
            assert_eq!(telemetry.trail().count(), 0);
            assert_eq!(telemetry.peak_speed(), 0.0);
        }
        let kept: Vec<usize> = telemetry.attempts().iter().map(Vec::len).collect();
        assert_eq!(kept, lengths[lengths.len() - ATTEMPTS_KEPT..]);
    }

    #[test]
    fn a_tick_that_does_not_follow_starts_a_new_attempt() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let mut telemetry = Telemetry::default();
        let snapshot = game.clone();
        for _ in 0..5 {
            let report = game.tick(&forward()).unwrap();
            telemetry.observe(&game, &report);
        }
        // Time goes back (as after a loaded save state).
        let mut game = snapshot;
        let report = game.tick(&forward()).unwrap();
        telemetry.observe(&game, &report);
        assert_eq!(telemetry.attempts().len(), 1);
        assert_eq!(telemetry.attempts()[0].len(), 5);
        assert_eq!(telemetry.trail().count(), 1);
    }
}
