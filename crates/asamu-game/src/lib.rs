//! High-level game state for the ASAMU recreation (no Bevy).
//!
//! [`Game`] ties a [`Level`], the player simulation ([`PlayerState`] +
//! [`PlayerParams`]) and a [`FixedClock`] together behind a fixed-step
//! [`Game::tick`]. The Bevy app calls `tick` from its fixed-update schedule;
//! tests call it directly. Everything here is deterministic.
//!
//! Defaults: the original's parameters ([`PlayerParams::asamu_original`],
//! values with provenance, ASAMU script layer with the original grapple gun
//! and rocket boots) on the port of the original's native pawn physics
//! ([`MovementModelKind::Ue3Pawn`]). For debugging,
//! [`Game::graybox_placeholder`] (or [`Game::with_movement_model`] with
//! [`MovementModelKind::Placeholder`]) keeps the old placeholder model and
//! parameters (with the placeholder rope grapple).
//!
//! Each tick follows the original's frame order (GRAPPLE.md G-TM-2): the
//! player's input events first ([`asamu_player::begin_step`]; the fire
//! trace sees the map actors where the previous tick left them, and the
//! attach's handler calls reach them at once), then the map-placed actors
//! ([`WorldObjects`]: movers, recharge crystals, attractor pads), the
//! collision world rebuilt from them (mover positions, crystal charge), then
//! the rest of the player's tick ([`asamu_player::finish_step`]), whose
//! handler calls and story-mode interactions go back to the world objects.
//! The level's ability state
//! ([`asamu_world::LevelAbilities`], the original's Kismet actions) is
//! applied at level start; [`Game::set_max_grapples`],
//! [`Game::enable_grapple`], [`Game::enable_rocket_boots`],
//! [`Game::hide_grapple_gun`] and [`Game::activate_attractor`] are the
//! Kismet actions' effects.
//!
//! Graybox behaviour (ours, not the original's): touching a checkpoint volume
//! makes its spawn the respawn point; falling below the level's `kill_z`
//! respawns the player at the active checkpoint, or at `player_start` if none
//! is active. With the script layer, a respawn follows the original's player
//! reset (A-DT-2: teleport, velocity 0, spawn rotation, story mode exited;
//! the script state and the physics mode are kept), without the 0.3 s fade.
//!
//! # Converted (original) levels
//!
//! [`Game::load_level`] loads a map converted by `asamu-import` from the
//! user's own install ([`asamu_world::scene`]): triangle collision
//! ([`GameWorld`], [`world::SceneCollision`]), the streamed sub-levels, and
//! the gameplay actors with the original's rules ([`asamu_world::gameplay`]):
//! the player spawns at the `PlayerStart` (A-CP-5), checkpoints activate on
//! touch and respawns use the checkpoint list's lookup (A-CP-1..4), kill
//! zones, dynamic kill zones and `KillZ` start the death sequence (A-DT-1/2:
//! grapple released and story mode left at once, the pawn keeps simulating
//! for `playerDiedFadeDownTime` 0.3 s, then the falling-when-grappled rocks
//! reset and the player is teleported to the latest checkpoint's spawn with
//! its rotation and zero velocity), triggers and trigger volumes report
//! touches, recharge crystals, glow flowers, interactables and attractor pads
//! run on [`WorldObjects`], falling rocks on the scene runtime, movers stay
//! where they are placed (Matinee is not imported yet), and the map's
//! level-start abilities apply ([`asamu_world::level_start_abilities`]).

mod converted;
pub mod world;

use asamu_core::{ClockError, DEFAULT_TICK_RATE_HZ, FixedClock};
use asamu_player::grapple::{self, Aim};
use asamu_player::grapple_gun::{self, GunAim, ReleaseReason};
use asamu_player::movement::place_on_floor;
use asamu_player::params::ParamError;
use asamu_player::rocket_boots;
use asamu_player::trace::TraceSample;
use asamu_player::ue3_movement::PawnPhysicsState;
use asamu_player::world::{Aabb, ActorClass, ActorTag, CONTACT_SKIN, SolidBox, Surface};
use asamu_player::{
    BoxWorld, InputFrame, MovementModelKind, PlayerParams, PlayerState, SimEvent, StepEvents,
    Trace, TraceMeta, begin_step, finish_step, pawn,
};
use asamu_world::gameplay::DeathCause;
use asamu_world::scene::SceneError;
use asamu_world::{
    Level, LevelError, ObjectEvent, PrimitiveKind, SpawnPoint, SurfaceTag, WorldEvent,
    WorldObjects, graybox_test_level,
};

pub use converted::{DEATH_FADE_DOWN_TIME, SCENE_RANDOM_SEED};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use world::{GameWorld, SceneCollision};

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
    /// A converted level could not be loaded.
    #[error("cannot load the converted level: {0}")]
    Scene(#[from] SceneError),
    /// The converted level has no `PlayerStart`.
    #[error("the converted level has no PlayerStart")]
    NoPlayerStart,
}

/// World-object events of one tick (fixed capacity so [`TickReport`] stays
/// `Copy`; a full log drops further events and sets `overflowed`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEventLog {
    items: [Option<WorldEvent>; 16],
    len: u8,
    /// An event was dropped because the log was full.
    pub overflowed: bool,
}

impl WorldEventLog {
    fn push(&mut self, event: WorldEvent) {
        match self.items.get_mut(usize::from(self.len)) {
            Some(slot) => {
                *slot = Some(event);
                self.len = self.len.saturating_add(1);
            }
            None => self.overflowed = true,
        }
    }

    /// The events in order.
    pub fn iter(&self) -> impl Iterator<Item = WorldEvent> + '_ {
        self.items
            .iter()
            .take(usize::from(self.len))
            .flatten()
            .copied()
    }

    /// `true` if `event` was raised.
    #[must_use]
    pub fn contains(&self, event: &WorldEvent) -> bool {
        self.iter().any(|e| e == *event)
    }

    /// `true` when no event was raised.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// What happened during one [`Game::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TickReport {
    /// Tick count after this tick.
    pub tick: u64,
    /// Player simulation events.
    pub events: StepEvents,
    /// The player was respawned this tick (hand-made levels: fell below
    /// `kill_z`; converted levels: the death sequence's reset at
    /// `DEATH_FADE_DOWN_TIME`).
    pub respawned: bool,
    /// Id of a checkpoint activated this tick.
    pub checkpoint_activated: Option<u32>,
    /// World-object events (crystals, interactions; converted levels also
    /// touches, checkpoints, deaths, respawns).
    pub world: WorldEventLog,
    /// A death sequence started this tick (converted levels; also when the
    /// respawn point itself is inside a kill zone).
    #[serde(default)]
    pub died: Option<DeathCause>,
}

/// The surface the player simulation sees for a level primitive.
fn surface_of(
    p: &asamu_world::CollisionPrimitive,
    objects: Option<&WorldObjects>,
    level: &Level,
) -> Surface {
    let class = match p.kind {
        PrimitiveKind::WorldGeometry => ActorClass::WorldGeometry,
        PrimitiveKind::StaticMesh => ActorClass::StaticMesh,
        PrimitiveKind::Mover => ActorClass::InterpActor,
        PrimitiveKind::RechargeCrystal => ActorClass::RechargeCrystal {
            charged: match (objects, p.actor) {
                (Some(o), Some(id)) => o.crystal_charged(level, id),
                _ => true,
            },
        },
        PrimitiveKind::GlowFlower => ActorClass::GlowFlower,
        PrimitiveKind::Interactable => ActorClass::Interactable,
    };
    let tag = match p.tag {
        SurfaceTag::None => ActorTag::None,
        SurfaceTag::TopOnlyGrappleAble => ActorTag::TopOnlyGrappleAble,
        SurfaceTag::BottomOnlyGrappleAble => ActorTag::BottomOnlyGrappleAble,
        SurfaceTag::GrappleInteractable => ActorTag::GrappleInteractable,
        SurfaceTag::NotLandable => ActorTag::NotLandable,
    };
    Surface {
        actor: p.actor,
        class,
        tag,
    }
}

/// Builds the collision world for a level at rest (static boxes, grapple
/// points, then the level objects; crystals charged, movers at rest).
#[must_use]
pub fn box_world_from_level(level: &Level) -> BoxWorld {
    box_world_with_objects(level, None)
}

/// Builds the collision world for a level with the objects' current state
/// (mover positions, crystal charge) and the movers' locations.
#[must_use]
pub fn box_world_with_objects(level: &Level, objects: Option<&WorldObjects>) -> BoxWorld {
    let mut world = BoxWorld {
        ground: None,
        boxes: level
            .collision_primitives(objects)
            .iter()
            .map(|p| SolidBox {
                bounds: Aabb::from_corners(p.min, p.max),
                grapple_able: p.grapple_able,
                surface: surface_of(p, objects, level),
            })
            .collect(),
        actors: Vec::new(),
    };
    let rest = WorldObjects::new(level);
    let objects = objects.unwrap_or(&rest);
    for m in &level.movers {
        if let Some(location) = level.mover_location(objects, m.id) {
            world.set_actor_location(m.id, location);
        }
    }
    world
}

/// A running game: level + player + parameters + fixed-step clock.
#[derive(Clone, Debug)]
pub struct Game {
    state: GameState,
    level: Level,
    world: GameWorld,
    params: PlayerParams,
    movement: MovementModelKind,
    clock: FixedClock,
    player: PlayerState,
    objects: WorldObjects,
    active_checkpoint: Option<usize>,
    respawn_count: u32,
    last_events: StepEvents,
    recording: Option<Trace>,
    /// Converted-level state (`None` for hand-made levels).
    scene: Option<Box<converted::SceneGame>>,
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
        let objects = WorldObjects::new(&level);
        let world = GameWorld::from_boxes(box_world_with_objects(&level, Some(&objects)));
        let mut player = spawn_state(&world, &params, &level.player_start);
        apply_level_abilities(&mut player, &level);
        Ok(Self {
            state: GameState::Boot,
            level,
            world,
            params,
            movement: MovementModelKind::Ue3Pawn,
            clock,
            player,
            objects,
            active_checkpoint: None,
            respawn_count: 0,
            last_events: StepEvents::default(),
            recording: None,
            scene: None,
        })
    }

    /// The hand-made graybox level with the original's parameters and the
    /// native-physics port at the default tick rate.
    ///
    /// # Errors
    /// Only if the built-in data were invalid (covered by tests).
    pub fn graybox() -> Result<Self, GameError> {
        Self::new(
            graybox_test_level(),
            PlayerParams::asamu_original(),
            DEFAULT_TICK_RATE_HZ,
        )
    }

    /// The graybox level with the old **placeholder** parameters and
    /// [`MovementModelKind::Placeholder`] (debugging only; no ASAMU script
    /// layer).
    ///
    /// # Errors
    /// Only if the built-in data were invalid (covered by tests).
    pub fn graybox_placeholder() -> Result<Self, GameError> {
        Ok(Self::new(
            graybox_test_level(),
            PlayerParams::placeholder(),
            DEFAULT_TICK_RATE_HZ,
        )?
        .with_movement_model(MovementModelKind::Placeholder))
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
        if self.scene.is_some() {
            return Some(self.tick_scene(input));
        }
        let dt = self.clock.dt();
        let mut world_events = Vec::new();
        // 1. Input events (G-IN-5, G-TM-2): the fire trace sees the map
        // actors where the previous tick left them; the attach's handler
        // calls reach them before they tick.
        let pending = begin_step(&mut self.player, input, &self.params, &self.world, dt);
        let input_events = *pending.events();
        self.apply_object_events(&input_events, 0, &mut world_events);
        // 2. Map-placed actors: movers, crystals, attractor pads (which write
        // the pawn's velocity). Skipped with the player's tick when `dt` is
        // invalid (the clock's `dt` always is valid).
        if pending.dt().is_some() {
            let time_after = (self.clock.tick() + 1) as f64 / self.clock.tick_rate_hz();
            let pawn_location = self.player.position;
            self.objects.tick(
                &self.level,
                dt,
                time_after,
                pawn_location,
                &mut self.player.velocity,
                &mut world_events,
            );
            self.world.boxes = box_world_with_objects(&self.level, Some(&self.objects));
        }
        // 3. Controller, pawn, power jump, rocket boots, gun.
        let mut events = finish_step(
            &self.movement,
            &mut self.player,
            pending,
            &self.params,
            &self.world,
        );
        self.apply_object_events(&events, input_events.kismet.len(), &mut world_events);
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
            // The death's grapple release belongs to this tick's report.
            let died = self.respawn_with_reason("kill_z", &mut world_events);
            events.kismet.extend(&died.kismet);
            if died.gun.released.is_some() {
                events.gun.released = died.gun.released;
            }
            self.last_events = events;
        }

        let fov = self.player.fov(&self.params);
        let time = self.clock.time_seconds();
        if let Some(trace) = &mut self.recording {
            trace
                .samples
                .push(TraceSample::capture(tick, time, input, &self.player, fov));
        }

        let mut world = WorldEventLog::default();
        for e in world_events {
            world.push(e);
        }
        Some(TickReport {
            tick,
            events,
            respawned,
            checkpoint_activated,
            world,
            died: None,
        })
    }

    /// Hands the grapple's handler calls and story interactions to the world
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
            if let Some(scene) = &mut self.scene {
                scene.apply_rock_event(object_event, out);
            }
        }
    }

    /// Respawns at the active checkpoint (or `player_start`).
    ///
    /// The grapple button level is carried over, so a button still held from
    /// before the respawn does not fire the grapple on the next tick (it must
    /// be released and pressed again). A running recording gets a note, since
    /// a respawn is a discontinuity that replaying the inputs alone does not
    /// reproduce.
    pub fn respawn(&mut self) {
        let mut world_events = Vec::new();
        if self.scene.is_some() {
            // Converted levels: the original's player reset at the latest
            // checkpoint, at once (no fade; the `ResetPlayer` path).
            self.release_for_death(&mut world_events);
            let _ = self.scene_reset_player(&mut world_events);
            return;
        }
        self.respawn_with_reason("manual", &mut world_events);
    }

    /// The respawn; returns the events of the death's grapple release and
    /// adds the world objects' reactions to `world_events`.
    fn respawn_with_reason(
        &mut self,
        reason: &str,
        world_events: &mut Vec<WorldEvent>,
    ) -> StepEvents {
        let mut released = StepEvents::default();
        let spawn = self.respawn_point();
        let mut old = self.player;
        let mut fresh = spawn_state(&self.world, &self.params, &spawn);
        if self.params.pawn.is_some() && old.script.started {
            // The original's player reset (ABILITIES.md A-DT-2): grapple
            // released (physics Falling, pawn state `Release`, one
            // `ReleaseGrapple` controller tick: GRAPPLE.md G-RL-1/5/7), then
            // teleport, spawn rotation, rocket boots reset when enabled,
            // velocity 0; the script state (sprint flags, AirControl,
            // move-input lock, eye height, power jump, grapple budget) and
            // the physics mode are kept — a pawn that died falling falls (and
            // lands) again at the spawn point. Story mode is exited.
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
        self.respawn_count = self.respawn_count.saturating_add(1);
        let tick = self.clock.tick();
        if let Some(trace) = &mut self.recording {
            trace
                .meta
                .notes
                .push(format!("respawn ({reason}) after tick {tick}"));
        }
        released
    }

    /// Where [`Self::respawn`] would place the player (converted levels:
    /// `feet` is the teleport target of the pawn's centre, see
    /// [`Self::scene_respawn_point`]).
    #[must_use]
    pub fn respawn_point(&self) -> SpawnPoint {
        if let Some((location, rotation)) = self.scene_respawn_point() {
            return SpawnPoint {
                feet: location,
                yaw: asamu_world::rotation::units_to_radians(rotation[1]),
            };
        }
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
        meta.notes.push(format!(
            "parameters: {}",
            if self.uses_original_params() {
                "original (PlayerParams::asamu_original, script layer on; original grapple gun and rocket boots)"
            } else {
                "placeholder"
            }
        ));
        let mut trace = Trace::new(meta);
        trace.samples.push(TraceSample::capture(
            self.clock.tick(),
            self.clock.time_seconds(),
            &InputFrame::default(),
            &self.player,
            self.player.fov(&self.params),
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

    /// What the placeholder rope grapple would hit if fired now (debug
    /// configuration; see [`Self::gun_aim`] for the original grapple).
    #[must_use]
    pub fn aim(&self) -> Aim {
        grapple::aim(&self.player, &self.params, &self.world)
    }

    /// What the original grapple gun's fire trace would hit now (`None`
    /// without the gun).
    #[must_use]
    pub fn gun_aim(&self) -> Option<GunAim> {
        grapple_gun::aim(&self.player, &self.params, &self.world)
    }

    /// The HUD crosshair state of the original grapple gun (GRAPPLE.md
    /// G-TG-2).
    #[must_use]
    pub fn crosshair(&self) -> bool {
        self.player.script.gun.crosshair
    }

    /// Kismet `SeqAct_SetMaxGrapples` (G-CT-2; negative = unlimited).
    pub fn set_max_grapples(&mut self, n: i32) {
        grapple_gun::set_max_grapples(&mut self.player, n);
    }

    /// Kismet `SeqAct_ToggleGrapple` (G-IN-4: a one-shot latch).
    pub fn enable_grapple(&mut self, enable: bool) {
        grapple_gun::enable_grapple(&mut self.player, enable);
    }

    /// The `UnlimitedGrapples` exec (G-CT-2).
    pub fn unlimited_grapples(&mut self, enable: bool) {
        grapple_gun::unlimited_grapples(&mut self.player, enable);
    }

    /// Kismet `SeqAct_ToggleVisibleGrapple` (G-AC-3).
    pub fn hide_grapple_gun(&mut self, hide: bool, animate: bool, visibility: bool) {
        grapple_gun::hide_grapple_gun(&mut self.player, hide, animate, visibility);
    }

    /// Kismet `SeqAct_ToggleRocketBoots` (A-RB-1).
    pub fn enable_rocket_boots(&mut self, enable: bool) {
        rocket_boots::enable_rocket_boots(&mut self.player, enable);
    }

    /// Kismet `SeqAct_ToggleAttractor`: activates the attractor pad `id`
    /// (ABILITIES.md §13). Returns whether the level has it.
    pub fn activate_attractor(&mut self, id: u32) -> bool {
        self.objects.activate_attractor(&self.level, id)
    }

    /// Run-time state of the level objects.
    #[must_use]
    pub fn objects(&self) -> &WorldObjects {
        &self.objects
    }

    /// Eye (view) position, UU: with the script layer this includes the
    /// run-time eye height and the walk bob (ABILITIES.md A-CM-1).
    #[must_use]
    pub fn eye_position(&self) -> Vec3 {
        grapple::eye_position(&self.player, &self.params)
    }

    /// Current horizontal FOV, degrees (the story-mode zoom changes it).
    #[must_use]
    pub fn fov(&self) -> f32 {
        self.player.fov(&self.params)
    }

    /// `true` when running the original's parameters with the ASAMU pawn
    /// script layer (`PlayerParams::pawn` is set).
    #[must_use]
    pub fn uses_original_params(&self) -> bool {
        self.params.pawn.is_some()
    }

    /// Enters story mode (Kismet `SeqAct_ToggleStoryMode`; ABILITIES.md
    /// A-ST-1; releases the grapple). No effect without the script layer.
    pub fn enter_story_mode(&mut self) {
        let released = pawn::enter_story_mode(&mut self.player, &self.params);
        let mut world_events = Vec::new();
        self.apply_object_events(&released, 0, &mut world_events);
    }

    /// Leaves story mode (A-ST-3). No effect outside story mode.
    pub fn exit_story_mode(&mut self) {
        pawn::exit_story_mode(&mut self.player, &self.params);
    }

    /// Toggles story mode; returns whether it is now on.
    pub fn toggle_story_mode(&mut self) -> bool {
        if self.in_story_mode() {
            self.exit_story_mode();
        } else {
            self.enter_story_mode();
        }
        self.in_story_mode()
    }

    /// In story mode.
    #[must_use]
    pub fn in_story_mode(&self) -> bool {
        self.player.script.started && self.player.script.is_story()
    }

    /// Makes the story-mode zoom (un)available (Kismet
    /// `SeqAct_ToggleZoomAvailable`).
    pub fn set_zoom_available(&mut self, enabled: bool) {
        pawn::set_zoom_available(&mut self.player, enabled);
    }

    /// Player state.
    #[must_use]
    pub fn player(&self) -> &PlayerState {
        &self.player
    }

    /// Mutable player state, for tools and tests (debug teleports, test
    /// setups). Writing it bypasses the simulation's rules.
    pub fn player_mut(&mut self) -> &mut PlayerState {
        &mut self.player
    }

    /// The level.
    #[must_use]
    pub fn level(&self) -> &Level {
        &self.level
    }

    /// The collision world (boxes of hand-made levels; triangle collision of
    /// converted levels).
    #[must_use]
    pub fn world(&self) -> &GameWorld {
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

    /// Id of the active checkpoint (converted levels: the checkpoint the
    /// respawn uses, once one has been registered).
    #[must_use]
    pub fn active_checkpoint(&self) -> Option<u32> {
        if let Some(scene) = &self.scene {
            return scene.active_checkpoint();
        }
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

/// Applies the level's ability state at level start (the original's
/// Kismet actions; `asamu_world::abilities`). Story mode, which needs the
/// parameters, is applied by [`Game::load_level`].
pub fn apply_level_abilities(player: &mut PlayerState, level: &Level) {
    if let Some(n) = level.abilities.max_grapples {
        grapple_gun::set_max_grapples(player, n);
    }
    if let Some(enable) = level.abilities.rocket_boots {
        rocket_boots::enable_rocket_boots(player, enable);
    }
    if let Some(enable) = level.abilities.grapple_enabled {
        grapple_gun::enable_grapple(player, enable);
    }
}

/// A player state standing on `spawn` (snapped to the floor when there is one
/// just below the feet point).
#[must_use]
pub fn spawn_state<W: asamu_player::CollisionWorld + ?Sized>(
    world: &W,
    params: &PlayerParams,
    spawn: &SpawnPoint,
) -> PlayerState {
    let lift = params.movement.capsule_half_height.value + CONTACT_SKIN;
    let mut s = PlayerState::new(spawn.feet + Vec3::Z * lift, spawn.yaw);
    place_on_floor(&mut s, &params.movement, world, 4.0 * CONTACT_SKIN);
    // Pawn start (`PostBeginPlay`) of the script layer, if any.
    pawn::start(&mut s, params);
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
        // Placeholder configuration: a fresh standing state.
        let mut g = Game::graybox_placeholder().unwrap();
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

    #[test]
    fn original_respawn_keeps_script_state_and_falling_physics() {
        // A-DT-2: velocity 0, teleport; physics mode and script state kept,
        // so the pawn lands again at the spawn point.
        let mut g = Game::graybox().unwrap();
        g.start();
        let start = *g.player();
        let back = InputFrame {
            move_forward: -1.0,
            sprint_held: true,
            ..InputFrame::default()
        };
        let mut respawned = false;
        for _ in 0..1200 {
            let r = g.tick(&back).unwrap();
            if r.respawned {
                respawned = true;
                break;
            }
        }
        assert!(respawned);
        let p = *g.player();
        assert_eq!(p.position, start.position);
        assert_eq!(p.velocity, Vec3::ZERO);
        assert!(!p.grounded, "died falling: still falling");
        assert!(
            p.script.started && p.script.sprint.active,
            "sprint flags kept"
        );
        let r = g.tick(&back).unwrap();
        assert!(g.player().grounded);
        let landing = r.events.landing.expect("lands at the spawn point");
        assert_eq!(landing.handler, asamu_player::LandingHandler::Normal);
    }

    #[test]
    fn respawn_while_attached_releases_into_release_state() {
        // A-DT-2 / GRAPPLE.md G-RL-5: death releases the grapple (pawn
        // `Release`, which ends a `Jumped`/`ReleasedJump` damping; one
        // `ReleaseGrapple` controller tick without look).
        let mut g = Game::graybox().unwrap();
        g.start();
        g.player.script.gun.attached = Some(asamu_player::grapple_gun::Attachment::default());
        g.player.script.gun.grapple_location = g.player.position + Vec3::new(0.0, 0.0, 500.0);
        g.player.pawn.flying = true;
        g.player.grounded = false;
        g.player.script.code.state = asamu_player::PawnStateName::Jumped;
        g.respawn();
        let p = *g.player();
        assert!(!p.is_grapple_attached());
        assert!(!p.pawn.flying, "released into falling physics");
        assert_eq!(p.script.code.state, asamu_player::PawnStateName::Release);
        assert!(p.script.release_gap);
        let yaw = p.yaw;
        let look = InputFrame {
            look_yaw_delta: 0.5,
            ..InputFrame::default()
        };
        g.tick(&look).unwrap();
        assert_eq!(g.player().yaw, yaw, "release tick drops the look");
        g.tick(&look).unwrap();
        assert_ne!(g.player().yaw, yaw);
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
        // The scripted crossing is tuned for the placeholder configuration.
        let mut g = Game::graybox_placeholder().unwrap();
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
        let mut g = Game::graybox_placeholder().unwrap();
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
            let mut g = Game::graybox_placeholder().unwrap();
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
        // Press: the fire is evaluated (nothing is in front of the start, so
        // the fail sound plays); the button is now held.
        let r = g.tick(&hold).unwrap();
        assert_eq!(r.events.gun.fire, Some(asamu_player::FireOutcome::Failed));
        g.start_recording();
        g.respawn();
        let r = g.tick(&hold).unwrap();
        assert_eq!(r.events.gun.fire, None, "held button must not re-fire");
        assert!(!g.player().is_grapple_attached());
        // Release, let the weapon's refire check return it to `Active`
        // (G-IN-2), press again: fires at once.
        for _ in 0..10 {
            g.tick(&InputFrame::default()).unwrap();
        }
        assert!(g.tick(&hold).unwrap().events.gun.fire.is_some());
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
    fn ue3_pawn_model_is_the_default_and_deterministic() {
        let g = Game::graybox().unwrap();
        assert_eq!(g.movement_model(), MovementModelKind::Ue3Pawn);
        let g = Game::graybox_placeholder().unwrap();
        assert_eq!(g.movement_model(), MovementModelKind::Placeholder);
        assert!(!g.uses_original_params());
        let run = || {
            let mut g = Game::graybox().unwrap();
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
        g.set_movement_model(MovementModelKind::Placeholder);
        assert_eq!(g.movement_model(), MovementModelKind::Placeholder);
    }

    #[test]
    fn default_game_runs_the_original_parameters() {
        let mut g = Game::graybox().unwrap();
        assert!(g.uses_original_params());
        assert_eq!(*g.params(), PlayerParams::asamu_original());
        let m = &g.params().movement;
        assert_eq!(m.capsule_radius.value, 21.0);
        assert_eq!(m.capsule_half_height.value, 44.0);
        // Eye 38 above the collision centre, FOV 90, pawn started.
        assert!(g.player().script.started);
        assert_eq!(g.eye_position(), g.player().position + Vec3::Z * 38.0);
        assert_eq!(g.fov(), 90.0);
        g.start();
        // Walk 440, sprint 880 (ABILITIES.md A1), staying on the start
        // platform (x from -400 to about +200).
        for _ in 0..30 {
            g.tick(&forward()).unwrap();
        }
        assert!((g.player().horizontal_speed() - 440.0).abs() < 0.05);
        let sprint = InputFrame {
            sprint_held: true,
            ..forward()
        };
        for _ in 0..30 {
            g.tick(&sprint).unwrap();
        }
        assert!(g.player().grounded);
        assert!((g.player().horizontal_speed() - 880.0).abs() < 0.05);
        // Jump: V.z = JumpZ 1000 (one tick of −1040 uu/s² after).
        let jump = InputFrame {
            jump_pressed: true,
            jump_held: true,
            ..sprint
        };
        let r = g.tick(&jump).unwrap();
        assert!(r.events.jumped);
        assert!((g.player().velocity.z - (1000.0 - 1040.0 / 60.0)).abs() < 0.01);
    }

    #[test]
    fn story_mode_and_zoom_through_the_game_api() {
        let mut g = Game::graybox().unwrap();
        g.start();
        assert!(!g.in_story_mode());
        assert!(g.toggle_story_mode());
        assert!((g.player().script.ground_speed - 264.0).abs() < 1e-3);
        g.start_recording();
        let zoom = InputFrame {
            power_jump_held: true,
            ..InputFrame::default()
        };
        for _ in 0..25 {
            g.tick(&zoom).unwrap();
        }
        assert_eq!(g.fov(), 50.0);
        let trace = g.stop_recording().unwrap();
        assert_eq!(
            trace.samples.last().unwrap().fov,
            50.0,
            "trace records the zoom"
        );
        assert!(
            trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("parameters: original")),
            "{:?}",
            trace.meta.notes
        );
        assert!(!g.toggle_story_mode());
        assert_eq!(g.fov(), 90.0);
        g.set_zoom_available(false);
        assert!(!g.player().script.zoom_enabled);
        // Without the script layer the story API does nothing.
        let mut p = Game::graybox_placeholder().unwrap();
        assert!(!p.toggle_story_mode());
        assert_eq!(p.fov(), 90.0);
    }

    #[test]
    fn collision_world_mirrors_the_level() {
        let g = Game::graybox().unwrap();
        let level = g.level();
        assert_eq!(
            g.world().boxes.boxes.len(),
            level.static_boxes.len()
                + level.grapple_points.len()
                + level.movers.len()
                + level.crystals.len()
                + level.flowers.len()
                + level.interactables.len()
        );
        assert!(g.world().boxes.ground.is_none());
        assert!(g.world().scene.is_none());
        assert!(matches!(g.aim(), Aim::OutOfRange | Aim::Blocked { .. }));
        // Movers report their location for the grapple anchor (G-AT-8).
        assert_eq!(g.world().boxes.actors.len(), level.movers.len());
    }
}
