//! High-level game state for the ASAMU recreation (no Bevy).
//!
//! [`Game`] ties a [`Level`], the player simulation ([`PlayerState`] +
//! [`PlayerParams`]) and a [`FixedClock`] together behind a fixed-step
//! [`Game::tick`]. The Bevy app calls `tick` from its fixed-update schedule;
//! tests call it directly. Everything here is deterministic.
//!
//! The locomotion model is selectable ([`Game::with_movement_model`]); the
//! default is still [`MovementModelKind::Placeholder`], and
//! [`MovementModelKind::Ue3Pawn`] runs the port of the original's native pawn
//! physics.
//!
//! Graybox behaviour (ours, not the original's): touching a checkpoint volume
//! makes its spawn the respawn point; falling below the level's `kill_z`
//! respawns the player at the active checkpoint, or at `player_start` if none
//! is active. How the original handles checkpoints and death is **UNKNOWN**.

use asamu_core::{ClockError, DEFAULT_TICK_RATE_HZ, FixedClock};
use asamu_player::grapple::{self, Aim};
use asamu_player::movement::place_on_floor;
use asamu_player::params::ParamError;
use asamu_player::trace::TraceSample;
use asamu_player::world::{Aabb, CONTACT_SKIN, SolidBox};
use asamu_player::{
    BoxWorld, InputFrame, MovementModelKind, PlayerParams, PlayerState, StepEvents, Trace,
    TraceMeta, step_with,
};
use asamu_world::{Level, LevelError, SpawnPoint, graybox_test_level};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level game state (skeleton).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameState {
    /// Created, not yet started; ticks do nothing.
    #[default]
    Boot,
    /// Simulation advancing.
    Playing,
    /// Simulation frozen (clock does not advance).
    Paused,
}

/// Errors from constructing a [`Game`].
#[derive(Debug, Clone, PartialEq, Error)]
pub enum GameError {
    /// The level failed validation.
    #[error("invalid level: {0}")]
    Level(#[from] LevelError),
    /// The parameters failed validation.
    #[error("invalid parameters: {0}")]
    Params(#[from] ParamError),
    /// The tick rate is invalid.
    #[error("invalid clock: {0}")]
    Clock(#[from] ClockError),
}

/// What happened during one [`Game::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TickReport {
    /// Tick count after this tick.
    pub tick: u64,
    /// Player simulation events.
    pub events: StepEvents,
    /// The player fell below `kill_z` and was respawned.
    pub respawned: bool,
    /// Id of a checkpoint activated this tick.
    pub checkpoint_activated: Option<u32>,
}

/// Builds the collision world for a level (static boxes, then grapple points).
#[must_use]
pub fn box_world_from_level(level: &Level) -> BoxWorld {
    BoxWorld {
        ground: None,
        boxes: level
            .collision_boxes()
            .into_iter()
            .map(|(min, max, grapple_able)| SolidBox {
                bounds: Aabb::from_corners(min, max),
                grapple_able,
            })
            .collect(),
    }
}

/// A running game: level + player + parameters + fixed-step clock.
#[derive(Clone, Debug)]
pub struct Game {
    state: GameState,
    level: Level,
    world: BoxWorld,
    params: PlayerParams,
    movement: MovementModelKind,
    clock: FixedClock,
    player: PlayerState,
    active_checkpoint: Option<usize>,
    respawn_count: u32,
    last_events: StepEvents,
    recording: Option<Trace>,
}

impl Game {
    /// Creates a game in [`GameState::Boot`] with the player at `player_start`.
    ///
    /// # Errors
    /// Invalid level, parameters or tick rate.
    pub fn new(level: Level, params: PlayerParams, tick_rate_hz: f64) -> Result<Self, GameError> {
        level.validate()?;
        params.validate()?;
        let clock = FixedClock::new(tick_rate_hz)?;
        let world = box_world_from_level(&level);
        let player = spawn_state(&world, &params, &level.player_start);
        Ok(Self {
            state: GameState::Boot,
            level,
            world,
            params,
            movement: MovementModelKind::default(),
            clock,
            player,
            active_checkpoint: None,
            respawn_count: 0,
            last_events: StepEvents::default(),
            recording: None,
        })
    }

    /// The hand-made graybox level with placeholder parameters at the default
    /// tick rate.
    ///
    /// # Errors
    /// Only if the built-in data were invalid (covered by tests).
    pub fn graybox() -> Result<Self, GameError> {
        Self::new(
            graybox_test_level(),
            PlayerParams::default(),
            DEFAULT_TICK_RATE_HZ,
        )
    }

    /// Selects the locomotion model (builder style).
    #[must_use]
    pub fn with_movement_model(mut self, model: MovementModelKind) -> Self {
        self.movement = model;
        self
    }

    /// Selects the locomotion model for subsequent ticks.
    pub fn set_movement_model(&mut self, model: MovementModelKind) {
        self.movement = model;
    }

    /// The locomotion model in use.
    #[must_use]
    pub fn movement_model(&self) -> MovementModelKind {
        self.movement
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> GameState {
        self.state
    }

    /// Boot → Playing (no effect otherwise).
    pub fn start(&mut self) {
        if self.state == GameState::Boot {
            self.state = GameState::Playing;
        }
    }

    /// Playing → Paused.
    pub fn pause(&mut self) {
        if self.state == GameState::Playing {
            self.state = GameState::Paused;
        }
    }

    /// Paused → Playing.
    pub fn resume(&mut self) {
        if self.state == GameState::Paused {
            self.state = GameState::Playing;
        }
    }

    /// Toggles between Playing and Paused.
    pub fn toggle_pause(&mut self) {
        match self.state {
            GameState::Playing => self.pause(),
            GameState::Paused => self.resume(),
            GameState::Boot => {}
        }
    }

    /// Advances one fixed tick when [`GameState::Playing`]; returns `None`
    /// (and changes nothing) otherwise.
    pub fn tick(&mut self, input: &InputFrame) -> Option<TickReport> {
        if self.state != GameState::Playing {
            return None;
        }
        let events = step_with(
            &self.movement,
            &mut self.player,
            input,
            &self.params,
            &self.world,
            self.clock.dt(),
        );
        let tick = self.clock.advance_tick();
        self.last_events = events;

        let mut checkpoint_activated = None;
        if let Some(index) = self.level.checkpoint_index_at(self.player.position)
            && self.active_checkpoint != Some(index)
        {
            self.active_checkpoint = Some(index);
            checkpoint_activated = self.level.checkpoints.get(index).map(|c| c.id);
        }

        let respawned = self.player.position.z < self.level.kill_z;
        if respawned {
            self.respawn_with_reason("kill_z");
        }

        let fov = self.params.camera.fov_degrees.value;
        let time = self.clock.time_seconds();
        if let Some(trace) = &mut self.recording {
            trace
                .samples
                .push(TraceSample::capture(tick, time, input, &self.player, fov));
        }

        Some(TickReport {
            tick,
            events,
            respawned,
            checkpoint_activated,
        })
    }

    /// Respawns at the active checkpoint (or `player_start`).
    ///
    /// The grapple button level is carried over, so a button still held from
    /// before the respawn does not fire the grapple on the next tick (it must
    /// be released and pressed again). A running recording gets a note, since
    /// a respawn is a discontinuity that replaying the inputs alone does not
    /// reproduce.
    pub fn respawn(&mut self) {
        self.respawn_with_reason("manual");
    }

    fn respawn_with_reason(&mut self, reason: &str) {
        let spawn = self.respawn_point();
        let grapple_was_held = self.player.grapple_was_held;
        self.player = spawn_state(&self.world, &self.params, &spawn);
        self.player.grapple_was_held = grapple_was_held;
        self.respawn_count = self.respawn_count.saturating_add(1);
        let tick = self.clock.tick();
        if let Some(trace) = &mut self.recording {
            trace
                .meta
                .notes
                .push(format!("respawn ({reason}) after tick {tick}"));
        }
    }

    /// Where [`Self::respawn`] would place the player.
    #[must_use]
    pub fn respawn_point(&self) -> SpawnPoint {
        self.active_checkpoint
            .and_then(|i| self.level.checkpoints.get(i))
            .map_or(self.level.player_start, |c| c.spawn)
    }

    /// Starts recording a runtime trace (initial sample = current state).
    pub fn start_recording(&mut self) {
        let mut meta = TraceMeta::runtime(
            Some(self.level.name.clone()),
            Some(self.clock.tick_rate_hz() as f32),
        );
        meta.notes.push("recorded by asamu-game".to_owned());
        meta.notes
            .push(format!("movement model: {}", self.movement.name()));
        let mut trace = Trace::new(meta);
        trace.samples.push(TraceSample::capture(
            self.clock.tick(),
            self.clock.time_seconds(),
            &InputFrame::default(),
            &self.player,
            self.params.camera.fov_degrees.value,
        ));
        self.recording = Some(trace);
    }

    /// Stops recording and returns the trace, if one was running.
    pub fn stop_recording(&mut self) -> Option<Trace> {
        self.recording.take()
    }

    /// `true` while recording.
    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// What the grapple would hit if fired now.
    #[must_use]
    pub fn aim(&self) -> Aim {
        grapple::aim(&self.player, &self.params, &self.world)
    }

    /// Eye position (UU).
    #[must_use]
    pub fn eye_position(&self) -> Vec3 {
        grapple::eye_position(&self.player, &self.params)
    }

    /// Player state.
    #[must_use]
    pub fn player(&self) -> &PlayerState {
        &self.player
    }

    /// The level.
    #[must_use]
    pub fn level(&self) -> &Level {
        &self.level
    }

    /// The collision world built from the level.
    #[must_use]
    pub fn world(&self) -> &BoxWorld {
        &self.world
    }

    /// Simulation parameters.
    #[must_use]
    pub fn params(&self) -> &PlayerParams {
        &self.params
    }

    /// The fixed-step clock.
    #[must_use]
    pub fn clock(&self) -> &FixedClock {
        &self.clock
    }

    /// Id of the active checkpoint.
    #[must_use]
    pub fn active_checkpoint(&self) -> Option<u32> {
        self.active_checkpoint
            .and_then(|i| self.level.checkpoints.get(i))
            .map(|c| c.id)
    }

    /// Number of respawns so far.
    #[must_use]
    pub fn respawn_count(&self) -> u32 {
        self.respawn_count
    }

    /// Events of the most recent tick.
    #[must_use]
    pub fn last_events(&self) -> &StepEvents {
        &self.last_events
    }
}

/// A player state standing on `spawn` (snapped to the floor when there is one
/// just below the feet point).
#[must_use]
pub fn spawn_state(world: &BoxWorld, params: &PlayerParams, spawn: &SpawnPoint) -> PlayerState {
    let lift = params.movement.capsule_half_height.value + CONTACT_SKIN;
    let mut s = PlayerState::new(spawn.feet + Vec3::Z * lift, spawn.yaw);
    place_on_floor(&mut s, &params.movement, world, 4.0 * CONTACT_SKIN);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::GrappleEvent;

    fn forward() -> InputFrame {
        InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        }
    }

    #[test]
    fn state_machine() {
        let mut g = Game::graybox().unwrap();
        assert_eq!(g.state(), GameState::Boot);
        assert!(g.player().grounded, "spawned standing");
        assert_eq!(g.tick(&forward()), None);
        assert_eq!(g.clock().tick(), 0);
        g.start();
        assert_eq!(g.state(), GameState::Playing);
        let r = g.tick(&forward()).unwrap();
        assert_eq!(r.tick, 1);
        g.toggle_pause();
        assert_eq!(g.state(), GameState::Paused);
        let frozen = *g.player();
        assert_eq!(g.tick(&forward()), None);
        assert_eq!(*g.player(), frozen);
        assert_eq!(g.clock().tick(), 1);
        g.toggle_pause();
        assert_eq!(g.state(), GameState::Playing);
        g.start();
        assert_eq!(g.state(), GameState::Playing);
    }

    #[test]
    fn rejects_invalid_inputs() {
        let mut level = graybox_test_level();
        level.kill_z = f32::NAN;
        assert!(matches!(
            Game::new(level, PlayerParams::default(), 60.0),
            Err(GameError::Level(_))
        ));
        let mut params = PlayerParams::default();
        params.movement.capsule_radius.value = -1.0;
        assert!(matches!(
            Game::new(graybox_test_level(), params, 60.0),
            Err(GameError::Params(_))
        ));
        assert!(matches!(
            Game::new(graybox_test_level(), PlayerParams::default(), 0.0),
            Err(GameError::Clock(_))
        ));
    }

    #[test]
    fn falling_below_kill_z_respawns_at_player_start() {
        let mut g = Game::graybox().unwrap();
        g.start();
        let start = *g.player();
        let back = InputFrame {
            move_forward: -1.0,
            ..InputFrame::default()
        };
        let mut respawned_at = None;
        for _ in 0..1200 {
            let r = g.tick(&back).unwrap();
            if r.respawned {
                respawned_at = Some(r.tick);
                break;
            }
        }
        assert!(respawned_at.is_some(), "walked off the back and fell");
        assert_eq!(g.respawn_count(), 1);
        assert_eq!(g.player().position, start.position);
        assert_eq!(g.player().velocity, Vec3::ZERO);
        assert!(g.player().grounded);
    }

    /// Scripted crossing of the first gap on the graybox level.
    fn cross_first_gap(g: &mut Game) -> Vec<TickReport> {
        let hook = g.level().grapple_points[0].position;
        let mut reports = Vec::new();
        let mut jump_tick = None;
        let mut released = false;
        for _ in 0..1500 {
            let p = *g.player();
            let tick = g.clock().tick() + 1;
            let mut input = forward();
            if jump_tick.is_none() && p.grounded && p.position.x >= 520.0 {
                input.jump_pressed = true;
                jump_tick = Some(tick);
            }
            if let Some(j) = jump_tick {
                if tick == j + 9 {
                    let d = hook - g.eye_position();
                    input.look_yaw_delta = d.y.atan2(d.x) - p.yaw;
                    input.look_pitch_delta = d.z.atan2(d.truncate().length()) - p.pitch;
                }
                let past_bottom = p
                    .grapple
                    .anchor()
                    .is_some_and(|a| p.position.x > a.x + 250.0)
                    && p.velocity.z > 0.0;
                if past_bottom {
                    released = true;
                }
                input.grapple_held = tick >= j + 9 && !released;
            }
            let r = g.tick(&input).unwrap();
            reports.push(r);
            if r.checkpoint_activated == Some(1) {
                // Keep going a little to settle on the platform.
                for _ in 0..60 {
                    reports.push(g.tick(&InputFrame::default()).unwrap());
                }
                break;
            }
        }
        reports
    }

    #[test]
    fn scripted_crossing_reaches_checkpoint_one() {
        let mut g = Game::graybox().unwrap();
        g.start();
        let reports = cross_first_gap(&mut g);
        let attached = reports
            .iter()
            .any(|r| matches!(r.events.grapple, Some(GrappleEvent::Attached { .. })));
        let released = reports
            .iter()
            .any(|r| matches!(r.events.grapple, Some(GrappleEvent::Released { .. })));
        assert!(
            attached && released,
            "attached {attached} released {released}"
        );
        assert!(reports.iter().all(|r| !r.respawned), "never fell");
        assert_eq!(g.active_checkpoint(), Some(1), "player at {:?}", g.player());
        assert!(g.player().grounded);
        let expected_z = -300.0 + g.params().movement.capsule_half_height.value + CONTACT_SKIN;
        assert!((g.player().position.z - expected_z).abs() < 1e-3);
        assert_eq!(g.respawn_point(), g.level().checkpoints[0].spawn);
    }

    #[test]
    fn recording_produces_a_valid_round_tripping_trace() {
        let mut g = Game::graybox().unwrap();
        g.start();
        g.start_recording();
        assert!(g.is_recording());
        cross_first_gap(&mut g);
        let trace = g.stop_recording().unwrap();
        assert!(!g.is_recording());
        assert!(trace.samples.len() > 100);
        trace.validate().unwrap();
        let back = Trace::from_jsonl_str(&trace.to_jsonl_string().unwrap()).unwrap();
        assert!(asamu_player::compare(&trace, &back).is_exact());
    }

    #[test]
    fn games_are_deterministic() {
        let run = || {
            let mut g = Game::graybox().unwrap();
            g.start();
            g.start_recording();
            cross_first_gap(&mut g);
            g.stop_recording().unwrap().to_jsonl_string().unwrap()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn respawn_does_not_fire_a_still_held_grapple() {
        let mut g = Game::graybox().unwrap();
        g.start();
        let hold = InputFrame {
            grapple_held: true,
            ..InputFrame::default()
        };
        // Press (misses or attaches, either way the button is now held).
        g.tick(&hold).unwrap();
        g.start_recording();
        g.respawn();
        let r = g.tick(&hold).unwrap();
        assert_eq!(r.events.grapple, None, "held button must not re-fire");
        assert!(!g.player().grapple.is_attached());
        // Release and press again: fires.
        g.tick(&InputFrame::default()).unwrap();
        assert!(g.tick(&hold).unwrap().events.grapple.is_some());
        let trace = g.stop_recording().unwrap();
        assert!(
            trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("respawn (manual)")),
            "{:?}",
            trace.meta.notes
        );
    }

    #[test]
    fn ue3_pawn_model_is_selectable_and_deterministic() {
        let g = Game::graybox().unwrap();
        assert_eq!(g.movement_model(), MovementModelKind::Placeholder);
        let run = || {
            let mut g = Game::graybox()
                .unwrap()
                .with_movement_model(MovementModelKind::Ue3Pawn);
            assert_eq!(g.movement_model(), MovementModelKind::Ue3Pawn);
            g.start();
            g.start_recording();
            let start = *g.player();
            // Settle, then walk forward on the start platform.
            for _ in 0..30 {
                let r = g.tick(&InputFrame::default()).unwrap();
                assert!(!r.respawned);
            }
            let settled = *g.player();
            assert!(settled.grounded);
            // Native walking hovers 2.15 uu above the floor.
            let floor_z =
                start.position.z - g.params().movement.capsule_half_height.value - CONTACT_SKIN;
            let hover =
                settled.position.z - floor_z - g.params().movement.capsule_half_height.value;
            assert!((hover - 2.15).abs() < 1e-3, "hover {hover}");
            for i in 0..240 {
                let mut input = forward();
                input.jump_pressed = i % 60 == 0;
                g.tick(&input).unwrap();
                assert!(g.player().is_finite());
            }
            let trace = g.stop_recording().unwrap();
            assert!(
                trace
                    .meta
                    .notes
                    .iter()
                    .any(|n| n == "movement model: ue3_pawn")
            );
            trace.to_jsonl_string().unwrap()
        };
        assert_eq!(run(), run());
        let mut g = Game::graybox().unwrap();
        g.set_movement_model(MovementModelKind::Ue3Pawn);
        assert_eq!(g.movement_model(), MovementModelKind::Ue3Pawn);
    }

    #[test]
    fn collision_world_mirrors_the_level() {
        let g = Game::graybox().unwrap();
        let level = g.level();
        assert_eq!(
            g.world().boxes.len(),
            level.static_boxes.len() + level.grapple_points.len()
        );
        assert!(g.world().ground.is_none());
        assert!(matches!(g.aim(), Aim::OutOfRange | Aim::Blocked { .. }));
    }
}
