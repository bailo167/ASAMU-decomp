//! Converted (original) levels in [`Game`]: loading, the per-tick scene
//! logic (touches, checkpoints, deaths and respawns, falling rocks, level
//! streaming) and the Kismet-style controls. See the crate docs.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::trace::TraceSample;
use asamu_player::world::{CollisionShape, TraceExtent};
use asamu_player::{
    BoxWorld, EventLog, InputFrame, PlayerParams, PlayerState, StepEvents, begin_step, finish_step,
    pawn, rocket_boots,
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

/// An extent trace of [`find_spot`] as the original's line check reports it.
struct LineHit {
    /// Reported hit time (the exact contact's, moved back by the rule of
    /// the routine that answered).
    time: f32,
    /// The box's centre at the reported hit.
    location: Vec3,
    /// Hit normal.
    normal: Vec3,
}

/// The original's single line check with an extent, against what blocks
/// the pawn: the hit as reported (its location is the trace's start plus the
/// reported fraction of the trace).
fn line_check(world: &GameWorld, start: Vec3, end: Vec3, extent: Vec3) -> Option<LineHit> {
    use asamu_player::CollisionWorld;
    let hit = world.sweep_box(start, end, extent)?;
    let time = hit.reported_time(TraceExtent::Box, (end - start).length());
    Some(LineHit {
        time,
        location: start + (end - start) * time,
        normal: hit.hit.normal,
    })
}

/// How far down the placement search looks for a floor, in half heights of
/// the box. NativeCode: the `f32` −2.0 at 0x1016457D0 of the user's macOS
/// executable, read by `UWorld::CheckSlice @ 0x1008E8A00` at 0x1008E8ACC.
const FIND_SPOT_DOWN_HALF_HEIGHTS: f32 = 2.0;
/// Half height (UU) of the slice the placement search probes with.
/// NativeCode: the immediate 1.0 at 0x1008E8A8E (`UWorld::CheckSlice`) and
/// at 0x1008E66C9 (`UWorld::FindSpot @ 0x1008E6570`).
const FIND_SPOT_SLICE_HALF_HEIGHT: f32 = 1.0;
/// Added (UU) to the height the floor trace gives. NativeCode: the `f32` 1.0
/// at 0x101633D88, added at 0x1008E8B86 and 0x1008E8C86
/// (`UWorld::CheckSlice`).
const FIND_SPOT_LIFT: f32 = 1.0;
/// The part of the box's X and Y half sizes a corner slice has. NativeCode:
/// the vector (0.5, 0.5) at 0x101662F80, used at 0x1008E66BE
/// (`UWorld::FindSpot`).
const FIND_SPOT_CORNER_SLICE: f32 = 0.5;
/// Where a corner slice is tried, in half sizes from the wanted location.
/// NativeCode: the `f32` 0.55 at 0x10171DA70, used at 0x1008E66F9.
const FIND_SPOT_CORNER_OFFSET: f32 = 0.55;
/// How far the location moves towards a corner slice that fits, in that
/// slice's offsets. NativeCode: the `f32` 1.1 at 0x1016844A8 (and −1.1 at
/// 0x10171DA74 for the mirrored Y), used at 0x1008E677A and 0x1008E6840.
const FIND_SPOT_CORNER_STEP: f32 = 1.1;
/// How far beyond the wanted location the last trace of the corner search
/// aims, as a part of the distance moved. NativeCode: the `f32` 0.2 at
/// 0x1016825B8 (and the vector at 0x10171DA50), used at 0x1008E6970.
const FIND_SPOT_BACK: f32 = 0.2;

/// What the slice step of the placement search found.
enum Slice {
    /// A place for the whole box; the location was updated.
    Fits,
    /// The slice fits at the location but no place was found from it: the
    /// search ends without one.
    Fails,
    /// The slice itself does not fit at the location: the search goes on
    /// with the corner slices.
    Blocked,
}

/// `UWorld::CheckSlice` (0x1008E8A00 in the user's macOS executable; read
/// there, described in our own words):
///
/// 1. A flat slice of the box (its X and Y half sizes, 1 uu half height) is
///    tested at the location. If it does not fit, the caller goes on.
/// 2. The slice is traced straight down by twice the box's half height. A
///    hit at the reported time `t` sets the location's height so that the
///    box's bottom is 2 uu above the slice's bottom at that reported hit:
///    `z − (2t − 1) × half height + 1`. Because the trace reports its hit
///    short of the floor (by a tenth of its length, at most 1.0 uu on BSP,
///    brushes and simple bodies, 4.0 uu on mesh triangles), the bottom ends
///    3.0 or 6.0 uu above the floor for the player's 88 uu trace. Without a
///    hit the location goes down by one half height.
/// 3. If the whole box fits there, that is the place (when the floor was
///    more than a half height below, or absent, the box is first traced up
///    by a half height and stops at what it meets). If it does not fit, the
///    location is pushed sideways by the floor hit's horizontal normal times
///    the X half size and tested once more.
fn check_slice(world: &GameWorld, location: &mut Vec3, extent: Vec3) -> Slice {
    let slice = Vec3::new(extent.x, extent.y, FIND_SPOT_SLICE_HALF_HEIGHT);
    if world.overlaps_box(*location, slice) {
        return Slice::Blocked;
    }
    let down = *location - Vec3::Z * (FIND_SPOT_DOWN_HALF_HEIGHTS * extent.z);
    let floor = line_check(world, *location, down, slice);
    let normal = floor.as_ref().map_or(Vec3::ZERO, |h| h.normal);
    let near_floor = match floor {
        None => {
            location.z -= extent.z;
            false
        }
        Some(h) if h.time > 0.5 => {
            location.z = location.z - (h.time + h.time - 1.0) * extent.z + FIND_SPOT_LIFT;
            false
        }
        Some(h) => {
            location.z = (0.5 - h.time) * 2.0 * extent.z + location.z + FIND_SPOT_LIFT;
            true
        }
    };
    if !world.overlaps_box(*location, extent) {
        if !near_floor
            && let Some(above) =
                line_check(world, *location, *location + Vec3::Z * extent.z, extent)
        {
            *location = above.location;
        }
        return Slice::Fits;
    }
    location.x += normal.x * extent.x;
    location.y += normal.y * extent.x;
    if world.overlaps_box(*location, extent) {
        Slice::Fails
    } else {
        Slice::Fits
    }
}

/// Where a spawn or a teleport puts the pawn's box when `wanted` is asked
/// for: `UWorld::FindSpot` (0x1008E6570 in the user's macOS executable),
/// which both `UWorld::SpawnActor` (call at 0x1008E5AE6) and
/// `UWorld::FarMoveActor` (the teleport behind a script's `SetLocation`;
/// call at 0x1008E9205) run with the actor's unscaled cylinder extent.
/// `None` when the search finds no place (the teleport then fails and the
/// pawn stays where it was). Read in the executable and described in our
/// own words (CONFIRMED for the code; see below for what was measured):
///
/// 1. If the box does not encroach on the world at `wanted`, that is the
///    place, unchanged.
/// 2. Else the slice step ([`check_slice`]) looks for the floor below
///    `wanted` and stands the box 2 uu above the reported hit.
/// 3. If the slice does not fit at `wanted` either, four slices of half the
///    box's width are tested around it, at ±0.55 half sizes in X and Y
///    (X negative first, each with Y negative first; at most two may fit).
///    Each one that fits moves the location 1.1 of its offset; with exactly
///    one, the move is doubled. Nothing may lie on the line from `wanted`
///    to the new location; the box must fit there, or the slice step must
///    find a place from there; and at the end the box is traced from there
///    to a point 0.2 of the way beyond `wanted` and stops at what it meets.
///
/// **Measured** (`docs/PARITY_FINDINGS.md` P10, five recorded respawns): at
/// two the pawn appears at the checkpoint's spawn point exactly (step 1).
/// At three it appears 6.09, 8.80 and 7.61 uu above a spawn point that is
/// inside the floor, falls and comes to rest 0.71, 0.72 and 0.72 uu lower.
/// Step 2 on a simple-collision floor gives exactly that: the bottom 3.0 uu
/// above the floor (2 + the 1.0 uu of the trace), and after the fall (which
/// lands in its fifth 60 Hz frame, a 1.30 uu move reported 0.13 short) and
/// the first walking floor check, 0.13 + 2.15 = 2.28 uu: 0.72 lower.
///
/// **Ours, not the original's:** "encroaches" is the original's point check
/// of the box against world geometry, whose routines were not read; here
/// it is the exact overlap test of the pawn's collision
/// ([`GameWorld::overlaps_box`]), so a spot within a hair of touching may be
/// judged differently. The original's check also leaves out other pawns;
/// ours counts NPC cylinders.
pub(crate) fn find_spot(world: &GameWorld, wanted: Vec3, extent: Vec3) -> Option<Vec3> {
    if !(wanted.is_finite() && extent.is_finite()) {
        return None;
    }
    if !world.overlaps_box(wanted, extent) {
        return Some(wanted);
    }
    if extent == Vec3::ZERO {
        return None;
    }
    let mut location = wanted;
    match check_slice(world, &mut location, extent) {
        Slice::Fits => return Some(location),
        Slice::Fails => return None,
        Slice::Blocked => {}
    }
    location = wanted;
    let slice = Vec3::new(
        extent.x * FIND_SPOT_CORNER_SLICE,
        extent.y * FIND_SPOT_CORNER_SLICE,
        FIND_SPOT_SLICE_HALF_HEIGHT,
    );
    let mut fits = 0;
    for side in [-1.0_f32, 1.0] {
        if fits >= 2 {
            break;
        }
        let x = extent.x * FIND_SPOT_CORNER_OFFSET * side;
        let y = extent.y * FIND_SPOT_CORNER_OFFSET;
        if !world.overlaps_box(wanted + Vec3::new(x, -y, 0.0), slice) {
            fits += 1;
            location.x += x * FIND_SPOT_CORNER_STEP;
            location.y += y * -FIND_SPOT_CORNER_STEP;
            if fits >= 2 {
                break;
            }
        }
        if !world.overlaps_box(wanted + Vec3::new(x, y, 0.0), slice) {
            fits += 1;
            location.x += x * FIND_SPOT_CORNER_STEP;
            location.y += y * FIND_SPOT_CORNER_STEP;
        }
    }
    match fits {
        0 => return None,
        1 => location = location + location - wanted,
        _ => {}
    }
    if !location.is_finite() || world.line_blocked(wanted, location) {
        return None;
    }
    if world.overlaps_box(location, extent)
        && !matches!(check_slice(world, &mut location, extent), Slice::Fits)
    {
        return None;
    }
    let beyond = (wanted - location) * FIND_SPOT_BACK + wanted;
    if let Some(hit) = line_check(world, location, beyond, extent) {
        location = hit.location;
    }
    location.is_finite().then_some(location)
}

/// Half sizes of the box the original collides the pawn as: its collision
/// cylinder's radius on X and Y and its half height on Z (P4).
pub(crate) fn extent_of(shape: CollisionShape) -> Vec3 {
    Vec3::new(shape.radius, shape.radius, shape.half_height)
}

/// Gives the actors `actors` dynamic collision instances (their static ones
/// are taken out of the shared static set, unchanged: kind, switch, flags
/// and sweep rule stay), so that their collision can be switched during
/// play ([`SceneCollision::set_actor_collision`]). Actors without static
/// collision are left alone.
pub(crate) fn make_switchable(map: &mut LoadedMap, actors: &BTreeSet<u32>) {
    let taken = map.collision.take_actor_statics(actors);
    map.dynamic.extend(taken);
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

    /// Starts a game on an already loaded map.
    ///
    /// **Level start (A-CP-5, P10).** The original spawns the pawn at the
    /// `PlayerStart`'s location with only its yaw, and possessing it puts it
    /// in **walking** physics: the player controller's start state sets the
    /// pawn's walking physics unless the pawn is falling, and a new pawn is
    /// not (CONFIRMED (src): the stock controller's restart and the begin
    /// of its walking state; this build's pawn does nothing of its own at
    /// a restart). It has no base, so its first tick checks the floor
    /// (NATIVE_PHYSICS.md 3.3): a floor within the check's 28 uu takes the
    /// pawn to its standing height at once, without a landing; with no
    /// floor in reach the pawn starts to fall in that tick, with zero
    /// velocity and without having moved, and lands like any other fall.
    ///
    /// Measured on six recorded level starts (`docs/PARITY_FINDINGS.md`
    /// P10; ABILITIES.md A-CP-5 has the numbers): the AG-Workshop and
    /// AG-ParadiseCave pawns stand at the floor check's height from their
    /// first recorded frames, 13 ms and 135 ms after the first tick, where
    /// a falling pawn would still be in the air (and, in AG-ParadiseCave,
    /// would have switched `AirControl` to 0.35 by landing; it reads 0.30);
    /// the AG-BeautifulCity and AG-Darkcave pawns, 33.7 and 36.4 uu above
    /// their floors, fall from the start height from the end of the first
    /// tick on, and the AG-Darkcave landing switched `AirControl` (A-AC-2).
    /// So the pawn starts here at the `PlayerStart`, walking, with no base
    /// and its floor check forced, and the first tick is that check.
    ///
    /// A `PlayerStart` at which the pawn's box is not free goes through the
    /// engine's placement search first (`find_spot`, the same routine the
    /// spawn and the teleports run; no recorded start needs it).
    ///
    /// **The save's reset.** In a story level the original then loads the
    /// save and resets the player to the level's latest checkpoint (the
    /// first one when the save names none), before the first tick:
    /// [`Game::apply_snapshot`]. Every recorded start had a save (a new game
    /// writes one), so the recorded pawns start at their level's first
    /// checkpoint; in AG-BeautifulCity and AG-Darkcave that is the
    /// `PlayerStart` itself. A game that is given no snapshot is the
    /// original's failed load: the pawn stays at the `PlayerStart`.
    ///
    /// Not reproduced: the original's first tick after a level load is a
    /// long frame that the engine clamps to 0.4 s (`UWorld::Tick`,
    /// NATIVE_PHYSICS.md 10.3), so its world clock reads 0.400 s when the
    /// second tick, and with it a fall, starts; ours reads one tick.
    ///
    /// Actors whose collision starts switched off (blocking volumes with
    /// `bEnabled` false, P5) get dynamic instances, so that
    /// [`Game::set_actor_collision`] can switch them.
    ///
    /// # Errors
    /// Invalid parameters or tick rate, no player start.
    pub fn from_loaded_map(
        mut map: LoadedMap,
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
        let off = map.actors_without_collision();
        make_switchable(&mut map, &off);
        let map = Arc::new(map);
        let objects = WorldObjects::new(&level);
        let world = GameWorld {
            boxes: BoxWorld::default(),
            scene: Some(SceneCollision::new(Arc::clone(&map))),
        };
        let runtime = SceneRuntime::new(&map.actors, map.initial_level_mask(), SCENE_RANDOM_SEED);
        let extent = extent_of(CollisionShape {
            radius: params.movement.capsule_radius.value,
            half_height: params.movement.capsule_half_height.value,
        });
        let spot = find_spot(&world, start.location, extent).unwrap_or(start.location);
        let mut player = PlayerState::new(spot, rotation::units_to_radians(start.rotation[1]));
        // Walking physics without a base: the first tick's floor check
        // stands the pawn on a floor in reach, or starts its fall.
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
            npcs: None,
            npc_pending: Vec::new(),
            npc_events: Vec::new(),
            size_factor: 1.0,
        })
    }

    /// Switches the collision of the actor `id` on or off, as a level
    /// script's toggle does to a blocking volume (P5; KISMET_RUNTIME.md).
    /// Returns whether the actor has collision that can be switched: only
    /// actors that started switched off, and (with the level's Kismet) the
    /// targets of its toggle actions, have
    /// ([`SceneCollision::set_actor_collision`]).
    pub fn set_actor_collision(&mut self, id: u32, enabled: bool) -> bool {
        self.world
            .scene
            .as_mut()
            .is_some_and(|sc| sc.set_actor_collision(id, enabled) > 0)
    }

    /// Whether the collision of the actor `id` is switched on (`None` for an
    /// actor without collision, and on hand-made levels).
    #[must_use]
    pub fn actor_collision(&self, id: u32) -> Option<bool> {
        self.world.scene.as_ref()?.actor_collision(id)
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

    /// Starts (or restarts) the death sequence (A-DT-2, t = 0). A kill-zone
    /// or dynamic-kill-zone death also reaches the NPC controllers'
    /// `NotifyKilled` ([`crate::npc::death_notifies_npcs`]; a scripted death,
    /// the worm's own kill and `KillZ` do not).
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
        if crate::npc::death_notifies_npcs(cause)
            && let Some(npcs) = &mut self.npcs
        {
            let events = npcs.notify_player_killed();
            self.npc_pending.extend(events);
        }
        released
    }

    /// The reset at the end of the death fade (A-DT-2, t = 0.3). Returns the
    /// cause when the respawn point itself kills (see below).
    pub(crate) fn scene_reset_player(
        &mut self,
        world_events: &mut Vec<WorldEvent>,
    ) -> Option<DeathCause> {
        let shape = self.pawn_cylinder();
        let extent = extent_of(shape);
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
        // Teleport (a failed teleport only logs in the original): the
        // engine's placement search decides where (`find_spot`), the pawn's
        // base is dropped and its physics mode kept.
        if let Some((location, rot, _)) = target
            && let Some(spot) = find_spot(&self.world, location, extent)
        {
            self.player.position = spot;
            self.player.yaw = rotation::units_to_radians(rot[1]);
            self.player.pitch = rotation::units_to_radians(rotation::normalize_axis(rot[0]));
            self.player.pawn.based = false;
            self.player.pawn.force_floor_check = true;
        }
        if self.player.script.started && self.player.script.boots.enabled {
            rocket_boots::reset_boots(&mut self.player.script.boots);
        }
        self.player.velocity = Vec3::ZERO;
        self.respawn_count = self.respawn_count.saturating_add(1);
        // The NPC touches (collectibles, foliage) restart at the new place.
        if let Some(npcs) = &mut self.npcs {
            npcs.on_player_respawned();
        }
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
        let sized = self.sized_params();
        // 1. Input events.
        let pending = begin_step(
            &mut self.player,
            input,
            sized.as_ref().unwrap_or(&self.params),
            &self.world,
            dt,
        );
        let input_events = *pending.events();
        self.apply_object_events(&input_events, 0, &mut world_events);
        let mut respawned = false;
        let mut died_at_respawn = None;
        // Events raised outside the player's step (NPC push, NPC kill).
        let mut extra = StepEvents::default();
        let mut npc_death = None;
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
            // NPCs (map actors too: the worm's push and kill, collectibles'
            // timers, villagers, story items, glow flowers).
            let npc_tick = self
                .npcs
                .as_mut()
                .map(|npcs| npcs.tick_actors(&mut self.player, dt));
            if let Some(r) = npc_tick {
                self.npc_pending.extend(r.events);
                if !r.released.kismet.is_empty() || r.released.gun.released.is_some() {
                    // A push can release the grapple onto a crystal.
                    self.apply_object_events(&r.released, 0, &mut world_events);
                    merge_events(&mut extra, &r.released);
                }
                if r.kill_player && !self.is_dying() {
                    // The worm's scream timed out: `PlayerDied` (its own
                    // `NotifyKilled` is already applied; scripted cause).
                    let released = self.scene_die(DeathCause::Scripted, &mut world_events);
                    merge_events(&mut extra, &released);
                    npc_death = Some(DeathCause::Scripted);
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
            sized.as_ref().unwrap_or(&self.params),
            &self.world,
        );
        self.apply_object_events(&events, input_events.kismet.len(), &mut world_events);
        // Step 2's handler calls (the push's release, the scream kill's) go
        // between the input events' and the step's own, as they happened.
        insert_events(&mut events, input_events.kismet.len(), &extra);
        let tick = self.clock.advance_tick();

        // 5. Touches along the tick's path, KillZ.
        let shape = self.pawn_cylinder();
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
        // The NPC side's touches along the same path (collectibles,
        // foliage).
        if let Some(npcs) = &mut self.npcs {
            let touched =
                npcs.update_touches(before, after, sized.as_ref().unwrap_or(&self.params));
            self.npc_pending.extend(touched);
        }
        let mut died = died_at_respawn.or(npc_death);
        if let Some((cause, _)) = outcome.death {
            let released = self.scene_die(cause, &mut world_events);
            merge_events(&mut events, &released);
            died = Some(cause);
        }
        self.last_events = events;
        self.npc_events = std::mem::take(&mut self.npc_pending);

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

/// Appends `from`'s handler calls to `into` and takes over its grapple
/// release.
fn merge_events(into: &mut StepEvents, from: &StepEvents) {
    into.kismet.extend(&from.kismet);
    if from.gun.released.is_some() {
        into.gun.released = from.gun.released;
    }
}

/// Puts `extra`'s handler calls into `events` after its first `at` (the
/// input events') and before the rest (the step's own), and takes over
/// `extra`'s grapple release when the step itself reported none. Rebuilding
/// the log pushes the same events, so a full log overflows exactly as
/// appending would.
fn insert_events(events: &mut StepEvents, at: usize, extra: &StepEvents) {
    if !extra.kismet.is_empty() {
        let mut merged = EventLog::new();
        for e in events.kismet.iter().take(at) {
            merged.push(e);
        }
        merged.extend(&extra.kismet);
        for e in events.kismet.iter().skip(at) {
            merged.push(e);
        }
        events.kismet = merged;
    }
    if events.gun.released.is_none() {
        events.gun.released = extra.gun.released;
    }
}

/// Copies the rocks' locations and rotations into the collision world
/// (anchor following: a helper based on a rock rides both, GRAPPLE.md
/// G-AT-8) and re-places their collision after a reset.
fn sync_rock_collision(s: &SceneGame, sc: &mut SceneCollision) {
    for (state, def) in s.runtime.rocks.iter().zip(&s.map.actors.rocks) {
        let Some(known) = sc.actor_location(def.id) else {
            continue;
        };
        if known != state.location
            && let Some(body) = def.body.and_then(|b| s.map.bodies.get(b))
        {
            let actor = scene::body_transform(body, state.location, state.rotation);
            for (index, relative) in &body.parts {
                if let Some(inst) = sc.dynamic.get_mut(*index) {
                    s.map.collision.place_dynamic(inst, relative.then(&actor));
                }
            }
        }
        sc.set_actor_pose(def.id, state.location, state.rotation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::SimEvent;

    fn log(events: &[SimEvent]) -> StepEvents {
        let mut e = StepEvents::default();
        for x in events {
            e.kismet.push(*x);
        }
        e
    }

    #[test]
    fn step_two_events_go_between_the_input_events_and_the_steps_own() {
        // Input event (attach), then the step's landing; the push in step 2
        // released the grapple.
        let mut events = log(&[
            SimEvent::PlayerGrappled { originator: None },
            SimEvent::PlayerLanded,
        ]);
        let extra = log(&[
            SimEvent::ActorUngrappled { actor: 3 },
            SimEvent::PlayerReleasedGrapple,
        ]);
        insert_events(&mut events, 1, &extra);
        assert_eq!(
            events.kismet.iter().collect::<Vec<_>>(),
            vec![
                SimEvent::PlayerGrappled { originator: None },
                SimEvent::ActorUngrappled { actor: 3 },
                SimEvent::PlayerReleasedGrapple,
                SimEvent::PlayerLanded,
            ]
        );
        assert!(!events.kismet.overflowed());
        // Nothing extra: unchanged.
        let before = events;
        insert_events(&mut events, 1, &StepEvents::default());
        assert_eq!(events, before);
        // A full log overflows as appending would.
        let mut full = log(&[SimEvent::PlayerLanded; asamu_player::events::EVENT_CAPACITY]);
        insert_events(&mut full, 0, &log(&[SimEvent::PlayerReleasedGrapple]));
        assert!(full.kismet.overflowed());
        assert_eq!(full.kismet.len(), asamu_player::events::EVENT_CAPACITY);
        assert_eq!(
            full.kismet.iter().next(),
            Some(SimEvent::PlayerReleasedGrapple)
        );
    }
}
