//! A tick with a caller-supplied `dt`, for variable-rate replays.
//!
//! [`asamu_game::Game::tick`] takes its step from the game's fixed clock, so
//! it cannot replay a recording whose frames have different lengths (the
//! original without benchmark mode). The player simulation itself takes `dt`
//! per tick ([`asamu_player::begin_step`] / [`asamu_player::finish_step`];
//! the native-physics port sub-steps inside one call, NATIVE_PHYSICS.md 1.4),
//! and everything a hand-made level's tick does around it is public.
//! [`VariableStepper`] is that tick rebuilt from those public pieces with
//! `dt` as an argument: a stand-in until `asamu-game` has a `dt`-taking tick
//! of its own, at which point this module can be replaced by a call to it.
//!
//! # What it simulates
//!
//! **Hand-made levels (the graybox): everything `Game::tick` does**, in the
//! same order — input events, their handler calls to the level objects, the
//! map-placed actors (movers, crystals, attractor pads), the collision world
//! rebuilt from them, the rest of the player's tick, checkpoints, the
//! `kill_z` respawn. With the clock's own `dt` and `time_after = tick / rate`
//! it is `Game::tick` bit for bit (tested against it tick by tick: player
//! state, events and level objects, through respawns, moving blocks, a
//! crystal grapple and an active attractor pad).
//!
//! **Converted levels: the player and the level objects only.** The player
//! steps against the level's triangle collision as it is at level start, and
//! the level objects that live in [`asamu_game::asamu_world::WorldObjects`]
//! run (recharge crystals with their charge state in the collision world,
//! glow flowers, interactables, attractor pads). The converted level's
//! scene logic is private to `asamu-game` and is **not** simulated: touch
//! volumes and triggers, checkpoints, kill zones, `KillZ`, deaths and
//! respawns, falling rocks (also those that fall when grappled), level
//! streaming changes, NPCs, and the map's Kismet with its Matinee movers.
//! A replay says so in its notes; a scenario that depends on any of them
//! needs the `dt`-taking tick in `asamu-game`.
//!
//! # Frame lengths
//!
//! `dt` goes to the player simulation and to the level objects unchanged,
//! as `Game::tick` hands the clock's step to both. The simulation's own
//! guards apply (`asamu_player::sim`): a `dt` that is not positive and
//! finite is a tick in which nothing moves, a `dt` above
//! `asamu_player::MAX_STEP_DT` is simulated as that bound. Callers that want
//! the level objects bounded the same way pass an already classified length
//! ([`crate::timestep::FrameLength::dt`]), as [`crate::replay`] does.

use asamu_core::glam::Vec3;
use asamu_game::asamu_world::{Level, ObjectEvent, SpawnPoint, WorldEvent, WorldObjects};
use asamu_game::{Game, GameWorld, box_world_with_objects, spawn_state};
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::world::CONTACT_SKIN;
use asamu_player::{
    InputFrame, MovementModelKind, PawnPhysicsState, PlayerParams, PlayerState, SimEvent,
    StepEvents, begin_step, finish_step, pawn, rocket_boots,
};

/// What happened during one [`VariableStepper::tick`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepperTick {
    /// Player simulation events (as in `asamu_game::TickReport::events`).
    pub events: StepEvents,
    /// The player fell below `kill_z` and was respawned (hand-made levels).
    pub respawned: bool,
    /// Id of a checkpoint activated this tick (hand-made levels).
    pub checkpoint_activated: Option<u32>,
    /// The `dt` the player simulation used: `None` when the tick did nothing
    /// (invalid `dt` or a rejected non-finite state), the clamped value when
    /// `dt` exceeded `MAX_STEP_DT`.
    pub dt_used: Option<f32>,
}

/// A level, its objects and the player, advanced one tick at a time with a
/// `dt` chosen by the caller (see the module docs for the scope).
#[derive(Clone, Debug)]
pub struct VariableStepper {
    level: Level,
    world: GameWorld,
    objects: WorldObjects,
    params: PlayerParams,
    model: MovementModelKind,
    player: PlayerState,
    /// A converted level (scene collision; reduced scope).
    converted: bool,
    active_checkpoint: Option<usize>,
    respawns: u32,
    ticks: u64,
}

impl VariableStepper {
    /// Takes over `game`'s level, collision world, level objects,
    /// parameters, movement model and player state as they are now. Meant
    /// for a game that has not ticked yet (level start, with any initial
    /// state already written through `Game::player_mut`): state that `Game`
    /// keeps private (a converted level's scene runtime, a running death
    /// sequence, NPCs) is not carried over.
    #[must_use]
    pub fn from_game(game: &Game) -> Self {
        let level = game.level().clone();
        let converted = game.scene_map().is_some();
        let active_checkpoint = if converted {
            None
        } else {
            game.active_checkpoint()
                .and_then(|id| level.checkpoints.iter().position(|c| c.id == id))
        };
        Self {
            world: game.world().clone(),
            objects: game.objects().clone(),
            params: game.params().clone(),
            model: game.movement_model(),
            player: *game.player(),
            converted,
            active_checkpoint,
            respawns: game.respawn_count(),
            ticks: 0,
            level,
        }
    }

    /// Player state.
    #[must_use]
    pub fn player(&self) -> &PlayerState {
        &self.player
    }

    /// Mutable player state (test setups; bypasses the simulation's rules).
    pub fn player_mut(&mut self) -> &mut PlayerState {
        &mut self.player
    }

    /// Simulation parameters.
    #[must_use]
    pub fn params(&self) -> &PlayerParams {
        &self.params
    }

    /// The level.
    #[must_use]
    pub fn level(&self) -> &Level {
        &self.level
    }

    /// Run-time state of the level objects.
    #[must_use]
    pub fn objects(&self) -> &WorldObjects {
        &self.objects
    }

    /// Current horizontal FOV, degrees.
    #[must_use]
    pub fn fov(&self) -> f32 {
        self.player.fov(&self.params)
    }

    /// `true` on a converted level (reduced scope, see the module docs).
    #[must_use]
    pub fn is_converted(&self) -> bool {
        self.converted
    }

    /// Ticks run so far.
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Respawns so far (including the game's before the snapshot).
    #[must_use]
    pub fn respawn_count(&self) -> u32 {
        self.respawns
    }

    /// Advances one tick of `dt` seconds. `time_after` is the time since
    /// level start at the end of this tick, in seconds (hand-made levels'
    /// movers are placed by it; `Game::tick` passes `tick / rate`).
    pub fn tick(&mut self, input: &InputFrame, dt: f32, time_after: f64) -> StepperTick {
        let mut world_events = Vec::new();
        // 1. Input events: the fire trace sees the map actors where the
        // previous tick left them; the attach's handler calls reach them
        // before they tick.
        let pending = begin_step(&mut self.player, input, &self.params, &self.world, dt);
        let dt_used = pending.dt();
        let input_events = *pending.events();
        self.apply_object_events(&input_events, 0, &mut world_events);
        // 2. Map-placed actors (skipped with the player's tick when `dt` is
        // invalid).
        if dt_used.is_some() {
            let pawn_location = self.player.position;
            self.objects.tick(
                &self.level,
                dt,
                time_after,
                pawn_location,
                &mut self.player.velocity,
                &mut world_events,
            );
            match &mut self.world.scene {
                None => {
                    self.world.boxes = box_world_with_objects(&self.level, Some(&self.objects));
                }
                Some(scene) => {
                    for (id, charged) in &mut scene.crystals {
                        *charged = self.objects.crystal_charged(&self.level, *id);
                    }
                }
            }
        }
        // 3. Controller, pawn, power jump, rocket boots, gun.
        let mut events = finish_step(
            &self.model,
            &mut self.player,
            pending,
            &self.params,
            &self.world,
        );
        self.apply_object_events(&events, input_events.kismet.len(), &mut world_events);
        self.ticks = self.ticks.saturating_add(1);

        let mut checkpoint_activated = None;
        let mut respawned = false;
        if !self.converted {
            if let Some(index) = self.level.checkpoint_index_at(self.player.position)
                && self.active_checkpoint != Some(index)
            {
                self.active_checkpoint = Some(index);
                checkpoint_activated = self.level.checkpoints.get(index).map(|c| c.id);
            }
            respawned = self.player.position.z < self.level.kill_z;
            if respawned {
                // The death's grapple release belongs to this tick's events.
                let died = self.respawn(&mut world_events);
                events.kismet.extend(&died.kismet);
                if died.gun.released.is_some() {
                    events.gun.released = died.gun.released;
                }
            }
        }
        StepperTick {
            events,
            respawned,
            checkpoint_activated,
            dt_used,
        }
    }

    /// Hands the grapple's handler calls and story interactions to the level
    /// objects, skipping the first `skip` events (already handed over).
    fn apply_object_events(&mut self, events: &StepEvents, skip: usize, out: &mut Vec<WorldEvent>) {
        for e in events.kismet.iter().skip(skip) {
            let object_event = match e {
                SimEvent::ActorGrappled { actor } => ObjectEvent::Grappled(actor),
                SimEvent::ActorUngrappled { actor } => ObjectEvent::UnGrappled(actor),
                SimEvent::InteractWith { actor } => ObjectEvent::InteractWith(actor),
                _ => continue,
            };
            self.objects.apply(&self.level, object_event, out);
        }
    }

    fn respawn_point(&self) -> SpawnPoint {
        self.active_checkpoint
            .and_then(|i| self.level.checkpoints.get(i))
            .map_or(self.level.player_start, |c| c.spawn)
    }

    /// The hand-made levels' respawn at the active checkpoint or the player
    /// start (ABILITIES.md A-DT-2: grapple released, teleport, velocity 0,
    /// script state and physics mode kept, story mode exited). Returns the
    /// events of the death's grapple release.
    fn respawn(&mut self, world_events: &mut Vec<WorldEvent>) -> StepEvents {
        let mut released = StepEvents::default();
        let spawn = self.respawn_point();
        let mut old = self.player;
        let mut fresh = spawn_state(&self.world, &self.params, &spawn);
        if self.params.pawn.is_some() && old.script.started {
            released = pawn::release_grapple(&mut old, ReleaseReason::Death);
            self.apply_object_events(&released, 0, world_events);
            if old.script.boots.enabled {
                rocket_boots::reset_boots(&mut old.script.boots);
            }
            fresh.script = old.script;
            if !old.grounded {
                fresh.position = spawn.feet
                    + Vec3::Z * (self.params.movement.capsule_half_height.value + CONTACT_SKIN);
                fresh.grounded = false;
                fresh.pawn = PawnPhysicsState::default();
            }
            pawn::exit_story_mode(&mut fresh, &self.params);
        }
        fresh.grapple_was_held = old.grapple_was_held;
        self.player = fresh;
        self.respawns = self.respawns.saturating_add(1);
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_core::DEFAULT_TICK_RATE_HZ;
    use asamu_player::trace::TraceSample;

    /// Deterministic pseudo-random numbers.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as u32) % n
        }
    }

    /// Inputs that change every few ticks: walking in all directions,
    /// sprinting, turning, looking up and down, jumps (held and tapped),
    /// grapple presses, power jumps, `use`.
    fn wandering_inputs(seed: u64, n: usize) -> Vec<InputFrame> {
        let mut r = Lcg(seed);
        let mut cur = InputFrame::default();
        let mut out = Vec::with_capacity(n);
        let mut jump_was = false;
        for i in 0..n {
            if i % 6 == 0 {
                cur.move_forward = [1.0, 1.0, 1.0, 0.0, -1.0][r.below(5) as usize];
                cur.move_right = [0.0, 0.0, 1.0, -1.0][r.below(4) as usize];
                cur.sprint_held = r.below(3) == 0;
                cur.jump_held = r.below(4) == 0;
                cur.grapple_held = r.below(3) == 0;
                cur.power_jump_held = r.below(9) == 0;
            }
            let mut f = cur;
            f.look_yaw_delta = (r.below(2001) as f32 - 1000.0) * 1e-4;
            f.look_pitch_delta = (r.below(2001) as f32 - 1000.0) * 4e-5;
            f.jump_pressed = f.jump_held && !jump_was;
            f.use_pressed = r.below(40) == 0;
            jump_was = f.jump_held;
            out.push(f);
        }
        out
    }

    fn sample_of(tick: u64, input: &InputFrame, s: &VariableStepper) -> TraceSample {
        TraceSample::capture(tick, 0.0, input, s.player(), s.fov())
    }

    /// With the clock's `dt` and `time_after = tick / rate` the stepper is
    /// `Game::tick` bit for bit: player state, events, respawns, level
    /// objects — on runs long enough to fall off the level and respawn.
    #[test]
    fn equals_game_tick_with_the_clocks_step() {
        let mut total_respawns = 0;
        let mut attached = 0;
        let (mut crystal_ticks, mut mover_ticks) = (0, 0);
        for (seed, rate, placeholder) in [
            (1_u64, 60.0_f64, false),
            (2, 60.0, false),
            (3, 30.0, false),
            (4, 144.0, false),
            (5, 3.0, false),
            (6, 60.0, true),
        ] {
            let mut game = if placeholder {
                Game::graybox_placeholder().unwrap()
            } else {
                let level = Game::graybox().unwrap().level().clone();
                Game::new(level, PlayerParams::asamu_original(), rate).unwrap()
            };
            let mut stepper = VariableStepper::from_game(&game);
            assert!(!stepper.is_converted());
            let at_rest = game.objects().clone();
            game.start();
            let dt = game.clock().dt();
            for (i, input) in wandering_inputs(seed, 2500).iter().enumerate() {
                let report = game.tick(input).unwrap();
                let time_after = (i + 1) as f64 / game.clock().tick_rate_hz();
                let tick = stepper.tick(input, dt, time_after);
                let what = format!("seed {seed} rate {rate} tick {i}");
                assert_eq!(stepper.player(), game.player(), "{what}");
                assert_eq!(tick.events, report.events, "{what}");
                assert_eq!(tick.respawned, report.respawned, "{what}");
                assert_eq!(
                    tick.checkpoint_activated, report.checkpoint_activated,
                    "{what}"
                );
                assert_eq!(stepper.objects(), game.objects(), "{what}");
                assert_eq!(stepper.fov(), game.fov(), "{what}");
                attached += usize::from(stepper.player().is_grapple_attached());
                crystal_ticks += usize::from(game.objects().crystals != at_rest.crystals);
                mover_ticks += usize::from(game.objects().mover_offsets != at_rest.mover_offsets);
            }
            assert_eq!(stepper.respawn_count(), game.respawn_count());
            assert_eq!(stepper.ticks(), 2500);
            total_respawns += game.respawn_count();
            if (rate - DEFAULT_TICK_RATE_HZ).abs() < 1e-9 {
                assert!(
                    game.player().position != Game::graybox().unwrap().player().position,
                    "the player moved"
                );
            }
        }
        assert!(total_respawns > 0, "some run falls off the level");
        assert!(attached > 0, "some run attaches the grapple");
        assert!(mover_ticks > 0, "the level's movers move");
        // Wandering never reaches the crystal; the next test grapples it.
        assert_eq!(crystal_ticks, 0);
    }

    /// The level objects' side of the tick: a grapple onto the recharge
    /// crystal (handler calls, charge state in the collision world, fade
    /// and recharge timers) with the attractor pad active, through a
    /// respawn — again `Game::tick` bit for bit.
    #[test]
    fn equals_game_tick_through_a_crystal_grapple() {
        let mut game = Game::graybox().unwrap();
        let rate = game.clock().tick_rate_hz();
        assert!(game.activate_attractor(501), "the graybox attractor pad");
        let crystal = game.level().crystals[0].clone();
        // In the air 400 uu in front of the crystal, the eye at its height,
        // looking at it.
        let eye_offset = game.eye_position().z - game.player().position.z;
        {
            let p = game.player_mut();
            p.position = crystal.center - Vec3::new(0.0, 400.0, eye_offset);
            p.velocity = Vec3::ZERO;
            p.yaw = core::f32::consts::FRAC_PI_2;
            p.pitch = 0.0;
            p.grounded = false;
            p.pawn = PawnPhysicsState::default();
            p.script.pov_yaw = p.yaw;
            p.script.pov_pitch = 0.0;
        }
        let mut stepper = VariableStepper::from_game(&game);
        assert!(stepper.objects().attractors[0].active);
        let charged = stepper.objects().crystals.clone();
        game.start();
        let dt = game.clock().dt();
        let (mut attached, mut crystal_ticks, mut respawns) = (0, 0, 0);
        let mut grappled_the_crystal = false;
        for i in 0..1500 {
            let input = InputFrame {
                grapple_held: i < 300,
                ..InputFrame::default()
            };
            let report = game.tick(&input).unwrap();
            let tick = stepper.tick(&input, dt, f64::from(i + 1) / rate);
            assert_eq!(stepper.player(), game.player(), "tick {i}");
            assert_eq!(tick.events, report.events, "tick {i}");
            assert_eq!(tick.respawned, report.respawned, "tick {i}");
            assert_eq!(stepper.objects(), game.objects(), "tick {i}");
            attached += usize::from(stepper.player().is_grapple_attached());
            crystal_ticks += usize::from(stepper.objects().crystals != charged);
            respawns += usize::from(tick.respawned);
            grappled_the_crystal |= tick
                .events
                .kismet
                .iter()
                .any(|e| e == SimEvent::ActorGrappled { actor: crystal.id });
        }
        assert!(grappled_the_crystal, "the grapple attached to the crystal");
        assert!(attached > 0);
        assert!(crystal_ticks > 0, "the crystal left its charged state");
        assert!(respawns > 0, "the run falls and respawns");
    }

    #[test]
    fn invalid_and_long_frames() {
        let game = Game::graybox().unwrap();
        let forward = InputFrame {
            move_forward: 1.0,
            look_yaw_delta: 0.1,
            ..InputFrame::default()
        };
        // Not positive or not finite: nothing moves, not even the view, and
        // the level objects do not tick.
        for dt in [0.0, -0.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut s = VariableStepper::from_game(&game);
            let before = (*s.player(), s.objects().clone());
            let t = s.tick(&forward, dt, 1.0);
            assert_eq!(t.dt_used, None, "{dt}");
            assert_eq!((*s.player(), s.objects().clone()), before, "{dt}");
            assert_eq!(t.events, StepEvents::default(), "{dt}");
            assert_eq!(s.ticks(), 1);
        }
        // Longer than the bound: simulated as the bound.
        let mut long = VariableStepper::from_game(&game);
        let mut bound = VariableStepper::from_game(&game);
        let t = long.tick(&forward, 3.0, 0.25);
        assert_eq!(t.dt_used, Some(asamu_player::MAX_STEP_DT));
        assert!(t.events.dt_clamped);
        let b = bound.tick(&forward, asamu_player::MAX_STEP_DT, 0.25);
        assert!(!b.events.dt_clamped);
        assert_eq!(long.player(), bound.player());
        // Tiny frames run and stay finite.
        let mut tiny = VariableStepper::from_game(&game);
        let mut time = 0.0;
        for dt in [1e-6_f32, 1e-9, f32::MIN_POSITIVE, 1e-40, 3e-4, 1e-3] {
            time += f64::from(dt);
            let t = tiny.tick(&forward, dt, time);
            assert_eq!(t.dt_used, Some(dt));
            assert!(tiny.player().is_finite(), "{dt}");
        }
        assert_ne!(tiny.player().yaw, game.player().yaw, "the view turned");
    }

    #[test]
    fn frame_lengths_change_the_result_and_runs_are_deterministic() {
        let game = Game::graybox().unwrap();
        let inputs = wandering_inputs(11, 400);
        let run = |lengths: &dyn Fn(usize) -> f32| {
            let mut s = VariableStepper::from_game(&game);
            let mut time = 0.0;
            let mut out = Vec::new();
            for (i, input) in inputs.iter().enumerate() {
                let dt = lengths(i);
                time += f64::from(dt);
                s.tick(input, dt, time);
                out.push(sample_of(i as u64 + 1, input, &s));
            }
            out
        };
        let varied = |i: usize| 0.012 + 0.001 * (i % 9) as f32;
        let a = run(&varied);
        let b = run(&varied);
        assert_eq!(a, b, "same lengths, same run");
        let fixed = run(&|_| 1.0 / 60.0);
        assert_ne!(a, fixed, "other lengths, another run");
        assert!(a.iter().all(|s| s.position.is_finite()));
    }
}
