//! Converted (original) levels in [`Game`]: loading, the per-tick scene
//! logic (touches, checkpoints, deaths and respawns, falling rocks, level
//! streaming) and the Kismet-style controls. See the crate docs.

use std::path::Path;
use std::sync::Arc;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::trace::TraceSample;
use asamu_player::world::{CONTACT_SKIN, CollisionShape};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, begin_step, finish_step, pawn,
    rocket_boots,
};
use asamu_world::gameplay::{DeathCause, PlayerStartDef, SceneRuntime};
use asamu_world::scene::{self, LoadedMap};
use asamu_world::{
    Checkpoint, Level, LevelOrigin, ObjectEvent, SpawnPoint, WorldEvent, WorldObjects,
    level_start_abilities, rotation,
};
use glam::Vec3;

use crate::world::{GameWorld, SceneCollision};
use crate::{Game, GameError, GameState, TickReport, WorldEventLog, apply_level_abilities};

/// `playerDiedFadeDownTime`: the death sequence's one-shot timer, s (A-DT-2).
/// ScriptDefault `asamu.ASAMUPawn.playerDiedFadeDownTime`. CONFIRMED (cdo).
pub const DEATH_FADE_DOWN_TIME: f32 = 0.3;

/// Seed of the scene runtime's random stream (falling-rock speeds and
/// spins). Ours: the original's random stream is not reproducible.
pub const SCENE_RANDOM_SEED: u64 = 0x4153_414D_5553_4545;

/// Converted-level state of a [`Game`].
#[derive(Clone, Debug)]
pub(crate) struct SceneGame {
    pub(crate) map: Arc<LoadedMap>,
    pub(crate) runtime: SceneRuntime,
    /// Elapsed time of the running death timer (A-DT-2), if dying.
    pub(crate) death: Option<f32>,
    /// Pending rock events (from grapple handler calls).
    pub(crate) pending: Vec<WorldEvent>,
}

impl SceneGame {
    /// Routes grapple handler calls to the falling rocks.
    pub(crate) fn apply_rock_event(&mut self, event: ObjectEvent, out: &mut Vec<WorldEvent>) {
        match event {
            ObjectEvent::Grappled(id) => {
                self.runtime.rock_grappled(&self.map.actors, id, out);
            }
            ObjectEvent::UnGrappled(id) => {
                self.runtime.rock_ungrappled(&self.map.actors, id);
            }
            ObjectEvent::InteractWith(_) => {}
        }
    }

    pub(crate) fn active_checkpoint(&self) -> Option<u32> {
        let m = &self.runtime.checkpoints;
        m.latest?;
        m.respawn(&self.map.actors.checkpoints).map(|r| r.2)
    }
}

/// The [`Level`] view of a converted map: its world objects (crystals,
/// flowers, interactables, attractors), checkpoint volumes for display,
/// `KillZ` and the level-start abilities. Its collision comes from the
/// scene, not from boxes.
fn converted_level(map: &LoadedMap, start: &PlayerStartDef) -> Level {
    let kill_z = map.world.kill_z;
    let a = &map.actors;
    let checkpoints = a
        .checkpoints
        .iter()
        .filter(|c| c.radius > 0.0 && c.half_height > 0.0 && c.spawn_location.z >= kill_z)
        .map(|c| {
            let e = Vec3::new(c.radius, c.radius, c.half_height);
            Checkpoint {
                id: c.id,
                min: c.location - e,
                max: c.location + e,
                spawn: SpawnPoint {
                    feet: c.spawn_location,
                    yaw: rotation::units_to_radians(c.spawn_rotation[1]),
                },
            }
        })
        .collect();
    Level {
        name: format!(
            "{} (converted locally from the original)",
            map.world.title.as_deref().unwrap_or(&map.map)
        ),
        origin: LevelOrigin::ConvertedFromOriginal {
            map: map.map.clone(),
        },
        static_boxes: Vec::new(),
        grapple_points: Vec::new(),
        player_start: SpawnPoint {
            feet: start.location - Vec3::Z * start.half_height,
            yaw: rotation::units_to_radians(start.rotation[1]),
        },
        checkpoints,
        kill_z,
        crystals: a.crystals.clone(),
        flowers: a.flowers.clone(),
        movers: Vec::new(),
        interactables: a.interactables.clone(),
        attractors: a.attractors.clone(),
        abilities: level_start_abilities(&map.map),
    }
}

/// Teleport placement (`FarMoveActor` → `FindSpot`, simplified): the spot
/// itself when free, else the first free spot straight above (4 uu steps,
/// up to twice the shape's height and radius), settled down onto what is
/// below it. TENTATIVE stand-in for the engine's `FindSpot`.
fn find_spot(world: &GameWorld, p: Vec3, shape: CollisionShape) -> Option<Vec3> {
    if !p.is_finite() {
        return None;
    }
    if !world.overlaps(p, shape) {
        return Some(p);
    }
    let max = 2.0 * (shape.half_height + shape.radius);
    let mut lift = 4.0;
    while lift <= max {
        let up = p + Vec3::Z * lift;
        if !world.overlaps(up, shape) {
            use asamu_player::CollisionWorld;
            return Some(match world.sweep_capsule(up, up - Vec3::Z * lift, shape) {
                Some(hit) => hit.position + hit.normal * CONTACT_SKIN,
                None => up - Vec3::Z * lift,
            });
        }
        lift += 4.0;
    }
    None
}

fn shape_of(params: &PlayerParams) -> CollisionShape {
    CollisionShape {
        radius: params.movement.capsule_radius.value,
        half_height: params.movement.capsule_half_height.value,
    }
}

impl Game {
    /// Loads the map `map` (case-insensitive, e.g. `AG-Workshop`) from a
    /// directory converted by `asamu-import levels` (+ `meshes --collision`)
    /// and starts it with the original's parameters at the default tick
    /// rate, in [`GameState::Boot`].
    ///
    /// # Errors
    /// Missing or malformed converted data, no player start.
    pub fn load_level(converted_dir: impl AsRef<Path>, map: &str) -> Result<Self, GameError> {
        let loaded = scene::load_map_from_dir(converted_dir, map)?;
        Self::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)
    }

    /// Starts a game on an already loaded map (A-CP-5: the pawn spawns at the
    /// `PlayerStart` with only its yaw, walking).
    ///
    /// # Errors
    /// Invalid parameters or tick rate, no player start.
    pub fn from_loaded_map(
        map: LoadedMap,
        params: PlayerParams,
        tick_rate_hz: f64,
    ) -> Result<Self, GameError> {
        params.validate()?;
        let clock = asamu_core::FixedClock::new(tick_rate_hz)?;
        let start = map
            .actors
            .player_start()
            .cloned()
            .ok_or(GameError::NoPlayerStart)?;
        let level = converted_level(&map, &start);
        level.validate()?;
        let map = Arc::new(map);
        let objects = WorldObjects::new(&level);
        let world = GameWorld {
            boxes: BoxWorld::default(),
            scene: Some(SceneCollision::new(Arc::clone(&map))),
        };
        let runtime = SceneRuntime::new(&map.actors, map.initial_level_mask(), SCENE_RANDOM_SEED);
        let shape = shape_of(&params);
        let spot = find_spot(&world, start.location, shape).unwrap_or(start.location);
        let mut player = PlayerState::new(spot, rotation::units_to_radians(start.rotation[1]));
        player.grounded = true;
        player.pawn.force_floor_check = true;
        pawn::start(&mut player, &params);
        apply_level_abilities(&mut player, &level);
        if level.abilities.story_mode == Some(true) {
            pawn::enter_story_mode(&mut player, &params);
        }
        Ok(Self {
            state: GameState::Boot,
            level,
            world,
            params,
            movement: asamu_player::MovementModelKind::Ue3Pawn,
            clock,
            player,
            objects,
            active_checkpoint: None,
            respawn_count: 0,
            last_events: StepEvents::default(),
            recording: None,
            scene: Some(Box::new(SceneGame {
                map,
                runtime,
                death: None,
                pending: Vec::new(),
            })),
        })
    }

    /// The converted map, if this game runs one.
    #[must_use]
    pub fn scene_map(&self) -> Option<&LoadedMap> {
        self.scene.as_ref().map(|s| s.map.as_ref())
    }

    /// Run-time state of the converted map's gameplay actors.
    #[must_use]
    pub fn scene_runtime(&self) -> Option<&SceneRuntime> {
        self.scene.as_ref().map(|s| &s.runtime)
    }

    /// The death sequence is running (between the death and the reset).
    #[must_use]
    pub fn is_dying(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| s.death.is_some())
    }

    /// Where the converted level's respawn would teleport the pawn's centre,
    /// and with which rotation (A-CP-4).
    #[must_use]
    pub fn scene_respawn_point(&self) -> Option<(Vec3, [i32; 3])> {
        let s = self.scene.as_ref()?;
        s.runtime
            .checkpoints
            .respawn(&s.map.actors.checkpoints)
            .map(|(p, r, _)| (p, r))
    }

    /// Kismet `SeqAct_PlayerDied` / the `PlayerDied` exec: starts the death
    /// sequence (converted levels; on hand-made levels it respawns at once).
    pub fn kill_player(&mut self) {
        if self.scene.is_none() {
            self.respawn();
            return;
        }
        let mut events = Vec::new();
        self.scene_die(DeathCause::Scripted, &mut events);
        if let Some(s) = &mut self.scene {
            s.pending.extend(events);
        }
    }

    /// Kismet `SeqAct_TriggerCheckpoint`. Returns whether the checkpoint
    /// exists.
    pub fn trigger_checkpoint(&mut self, id: u32) -> bool {
        let Some(s) = &mut self.scene else {
            return false;
        };
        let mut events = Vec::new();
        let ok = s.runtime.trigger_checkpoint(&s.map.actors, id, &mut events);
        s.pending.extend(events);
        ok
    }

    /// Kismet `SeqAct_ToggleCheckpointEnable`.
    pub fn set_checkpoint_enabled(&mut self, id: u32, enabled: bool) -> bool {
        match &mut self.scene {
            Some(s) => s.runtime.set_checkpoint_enabled(&s.map.actors, id, enabled),
            None => false,
        }
    }

    /// Kismet `SeqAct_Toggle` on a dynamic volume (e.g. an
    /// `ASAMUDynamicKillZone`).
    pub fn set_volume_enabled(&mut self, id: u32, enabled: bool) -> bool {
        match &mut self.scene {
            Some(s) => s.runtime.set_volume_enabled(&s.map.actors, id, enabled),
            None => false,
        }
    }

    /// Kismet `SeqAct_ToggleFallingRocksActive`.
    pub fn set_falling_rocks_active(&mut self, active: bool) {
        if let Some(s) = &mut self.scene {
            s.runtime.set_falling_rocks_active(&s.map.actors, active);
        }
    }

    /// Streams the sub-level `name` in or out (Kismet `SeqAct_LevelStreaming`
    /// / `SeqAct_MultiLevelStreaming`; e.g. `TheCore` in AG-IceCave). Its
    /// collision and touch actors follow. Returns whether the level exists.
    pub fn set_level_streamed(&mut self, name: &str, loaded: bool) -> bool {
        let Some(s) = &mut self.scene else {
            return false;
        };
        let Some(i) = s.map.level_index(name).filter(|i| *i > 0 && *i < 64) else {
            return false;
        };
        let bit = 1u64 << i;
        let mask = if loaded {
            s.runtime.level_mask | bit
        } else {
            s.runtime.level_mask & !bit
        };
        s.runtime.level_mask = mask;
        if let Some(sc) = &mut self.world.scene {
            sc.level_mask = mask;
        }
        true
    }

    /// `true` when the sub-level `name` is loaded.
    #[must_use]
    pub fn is_level_streamed(&self, name: &str) -> bool {
        self.scene.as_ref().is_some_and(|s| {
            s.map
                .level_index(name)
                .is_some_and(|i| i < 64 && s.runtime.level_mask & (1u64 << i) != 0)
        })
    }

    /// Releases the grapple and leaves story mode (the death's first
    /// effects, A-DT-2 t = 0); returns the release's events.
    pub(crate) fn release_for_death(&mut self, world_events: &mut Vec<WorldEvent>) -> StepEvents {
        let mut released = StepEvents::default();
        if self.params.pawn.is_some() && self.player.script.started {
            released = pawn::release_grapple(&mut self.player, ReleaseReason::Death);
            self.apply_object_events(&released, 0, world_events);
            pawn::exit_story_mode(&mut self.player, &self.params);
        }
        released
    }

    /// Starts (or restarts) the death sequence (A-DT-2, t = 0).
    pub(crate) fn scene_die(
        &mut self,
        cause: DeathCause,
        world_events: &mut Vec<WorldEvent>,
    ) -> StepEvents {
        let released = self.release_for_death(world_events);
        if let Some(s) = &mut self.scene {
            s.death = Some(0.0);
        }
        world_events.push(WorldEvent::PlayerDied { cause });
        released
    }

    /// The reset at the end of the death fade (A-DT-2, t = 0.3). Returns the
    /// cause when the respawn point itself kills (see below).
    pub(crate) fn scene_reset_player(
        &mut self,
        world_events: &mut Vec<WorldEvent>,
    ) -> Option<DeathCause> {
        let shape = shape_of(&self.params);
        let Some(s) = &mut self.scene else {
            return None;
        };
        s.death = None;
        // Game-level death hook: the falling-when-grappled rocks reset.
        s.runtime.reset_grapple_rocks(&s.map.actors);
        if let Some(sc) = &mut self.world.scene {
            sync_rock_collision(s, sc);
        }
        world_events.push(WorldEvent::PlayerRespawned);
        let target = s.runtime.checkpoints.respawn(&s.map.actors.checkpoints);
        // Teleport (a failed teleport only logs in the original).
        if let Some((location, rot, _)) = target
            && let Some(spot) = find_spot(&self.world, location, shape)
        {
            self.player.position = spot;
            self.player.yaw = rotation::units_to_radians(rot[1]);
            self.player.pitch = rotation::units_to_radians(rotation::normalize_axis(rot[0]));
            self.player.pawn.force_floor_check = true;
        }
        if self.player.script.started && self.player.script.boots.enabled {
            rocket_boots::reset_boots(&mut self.player.script.boots);
        }
        self.player.velocity = Vec3::ZERO;
        self.respawn_count = self.respawn_count.saturating_add(1);
        // Touches are re-evaluated at the new place (no sweep across the
        // teleport): volumes still overlapped stay touched, new overlaps
        // touch, as the engine's teleport updates touching (TENTATIVE: the
        // reset's teleport path was not traced). A kill zone touched there
        // kills again (A-DT-1), so a respawn point inside one loops deaths
        // as it would in the original; KillZ keeps its once-per-descent rule.
        let p = self.player.position;
        let outcome = match &mut self.scene {
            Some(s) => s.runtime.update_touches(
                &s.map.actors,
                s.map.world.kill_z,
                p,
                p,
                shape.radius,
                shape.half_height,
                world_events,
            ),
            None => Default::default(),
        };
        let tick = self.clock.tick();
        if let Some(trace) = &mut self.recording {
            trace
                .meta
                .notes
                .push(format!("respawn (death sequence) after tick {tick}"));
        }
        let (cause, _) = outcome.death?;
        let _ = self.scene_die(cause, world_events);
        Some(cause)
    }

    /// One fixed tick of a converted level (see the crate docs for the order).
    pub(crate) fn tick_scene(&mut self, input: &InputFrame) -> TickReport {
        let dt = self.clock.dt();
        let mut world_events: Vec<WorldEvent> = Vec::new();
        if let Some(s) = &mut self.scene {
            world_events.append(&mut s.pending);
        }
        // 1. Input events.
        let pending = begin_step(&mut self.player, input, &self.params, &self.world, dt);
        let input_events = *pending.events();
        self.apply_object_events(&input_events, 0, &mut world_events);
        let mut respawned = false;
        let mut died_at_respawn = None;
        if pending.dt().is_some() {
            // 2. Map-placed actors.
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
            if let (Some(s), Some(sc)) = (&mut self.scene, &mut self.world.scene) {
                s.runtime.tick_rocks(
                    &s.map.actors,
                    &s.map.bodies,
                    &s.map.collision,
                    &mut sc.dynamic,
                    dt,
                );
                sync_rock_collision(s, sc);
                for (id, charged) in &mut sc.crystals {
                    *charged = self.objects.crystal_charged(&self.level, *id);
                }
            }
            // 3. The pawn's timers: the death fade (A-DT-2).
            let fade_over = match self.scene.as_mut().and_then(|s| s.death.as_mut()) {
                Some(count) => {
                    *count += dt;
                    *count > DEATH_FADE_DOWN_TIME
                }
                None => false,
            };
            if fade_over {
                died_at_respawn = self.scene_reset_player(&mut world_events);
                respawned = true;
            }
        }
        // 4. Controller, pawn (physics), power jump, boots, gun.
        let before = self.player.position;
        let mut events = finish_step(
            &self.movement,
            &mut self.player,
            pending,
            &self.params,
            &self.world,
        );
        self.apply_object_events(&events, input_events.kismet.len(), &mut world_events);
        let tick = self.clock.advance_tick();

        // 5. Touches along the tick's path, KillZ.
        let shape = shape_of(&self.params);
        let after = self.player.position;
        let outcome = match &mut self.scene {
            Some(s) => s.runtime.update_touches(
                &s.map.actors,
                s.map.world.kill_z,
                before,
                after,
                shape.radius,
                shape.half_height,
                &mut world_events,
            ),
            None => Default::default(),
        };
        let mut died = died_at_respawn;
        if let Some((cause, _)) = outcome.death {
            let released = self.scene_die(cause, &mut world_events);
            events.kismet.extend(&released.kismet);
            if released.gun.released.is_some() {
                events.gun.released = released.gun.released;
            }
            died = Some(cause);
        }
        self.last_events = events;

        let checkpoint_activated = world_events.iter().find_map(|e| match e {
            WorldEvent::CheckpointActivated { id, .. } => Some(*id),
            _ => None,
        });
        let fov = self.player.fov(&self.params);
        let time = self.clock.time_seconds();
        if let Some(trace) = &mut self.recording {
            trace
                .samples
                .push(TraceSample::capture(tick, time, input, &self.player, fov));
        }
        let mut log = WorldEventLog::default();
        for e in world_events {
            log.push(e);
        }
        TickReport {
            tick,
            events,
            respawned,
            checkpoint_activated,
            world: log,
            died,
        }
    }
}

/// Copies the rocks' locations into the collision world (anchor following)
/// and re-places their collision after a reset.
fn sync_rock_collision(s: &SceneGame, sc: &mut SceneCollision) {
    for (state, def) in s.runtime.rocks.iter().zip(&s.map.actors.rocks) {
        if let Ok(i) = sc.actor_locations.binary_search_by_key(&def.id, |a| a.0)
            && let Some(slot) = sc.actor_locations.get_mut(i)
        {
            if slot.1 != state.location
                && let Some(body) = def.body.and_then(|b| s.map.bodies.get(b))
            {
                let actor = scene::body_transform(body, state.location, state.rotation);
                for (index, relative) in &body.parts {
                    if let Some(inst) = sc.dynamic.get_mut(*index) {
                        s.map.collision.place_dynamic(inst, relative.then(&actor));
                    }
                }
            }
            slot.1 = state.location;
        }
    }
}
