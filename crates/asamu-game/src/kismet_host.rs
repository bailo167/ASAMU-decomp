//! Kismet host: runs a level's Kismet ([`asamu_kismet::Runtime`]) against a
//! [`Game`] — the original's level scripting (ability unlocks, checkpoints,
//! streaming, Matinee movers, story mode) plus the presentation outputs the
//! app turns into sound, narration and UI.
//!
//! [`LevelScript`] owns the interpreter; [`LevelScript::tick`] replaces
//! [`Game::tick`] for a converted level with Kismet:
//!
//! 1. the base mover under a standing player is noted (simple basing,
//!    below);
//! 2. **Kismet** runs one update (`Runtime::tick`: queued events, latent
//!    actions, Matinee, which moves the movers through the host), then the
//!    narrator's and beat actors' timers;
//! 3. a player standing on a mover that moved is carried with it;
//! 4. the **game tick** runs ([`Game::tick`]: the player's input events, map
//!    actors, controller/pawn, touches);
//! 5. the tick's events (touches, grapple, landing, boosts, interactions,
//!    the death sequence's reset, the NPC events: worm, collectibles, story
//!    items) are handed to the interpreter, which queues the matching Kismet
//!    events for the next update.
//!
//! Between 2 and 4 the update's game-side outputs that act on the NPCs run
//! at once, as the original's actions call the worm controller
//! synchronously: `SeqAct_StartWorm` / `ShutDownWorm` / `PauseWorm` →
//! [`crate::npc::NpcSystem`], `SeqAct_ToggleFollowCollision` → the actor's
//! mover collision. Every output is still returned to the app.
//!
//! Frame order (KISMET_RUNTIME.md §1): the original updates the game
//! sequence once per frame in `UWorld::Tick` just before the first actor
//! tick group (CONFIRMED from the disassembly), after the frame's input
//! events (stock engine flow, not re-read here). Events raised by actors
//! during their ticks queue Kismet ops that run in the next frame's
//! update. Our only deviation: `Game::tick` begins with the input events, so
//! Kismet runs before them instead of just after (an input event sees
//! this frame's Kismet effects one frame early). TENTATIVE impact.
//!
//! **Movers.** Actors bound to a Matinee group, their passengers (actors
//! attached to them through `Base`, which the interpreter carries along as
//! UE3's attachment does), the actor `SetRotationToPlayerRotation` turns and
//! actors Kismet destroys or re-collides get a mover body: their collision is
//! taken out of the static set before play
//! (`CollisionScene::take_actor_statics`) and becomes dynamic instances
//! placed with the actor's transform, which Matinee and attachment set.
//! Moving actors report their location for grapple anchors (GRAPPLE.md
//! G-AT-8). Falling rocks bound to Matinee move through the rock state
//! instead.
//!
//! **Basing (TENTATIVE).** A grounded player standing on a mover (the actor
//! a short downward sweep hits) is moved by the mover's change of transform
//! that frame (position through the full transform, yaw by the yaw change).
//! UE3 bases pawns on movers and moves them in the mover's encroachment
//! pass; that path (and pushing, crushing) is not modelled.
//!
//! **Abilities.** With Kismet attached, the hard-coded level-start ability
//! table (`asamu_world::level_start_abilities`) that [`Game::load_level`]
//! applies is undone (the pawn's script state is reset to a fresh start)
//! and the map's own Kismet sets abilities, as in the original.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_kismet::{ActorInfo, ActorRef, Host, LoadError, Output, Runtime, ToggleMode};
use asamu_player::world::{CollisionShape, CollisionWorld};
use asamu_player::{InputFrame, PlayerParams, PlayerState, SimEvent, pawn};
use asamu_world::collision::Affine;
use asamu_world::scene::{self, LoadedMap, SceneError};
use asamu_world::{WorldEvent, rotation};
use glam::Vec3;
use thiserror::Error;

use crate::npc::{NpcEvent, NpcOptions, NpcSystem};
use crate::{Game, GameError, TickReport};

/// Distance of the downward sweep that finds the base mover, UU.
const BASE_PROBE: f32 = 8.0;

/// Errors loading a converted level with its Kismet.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LevelScriptError {
    /// The converted scene could not be loaded.
    #[error("cannot load the converted level: {0}")]
    Scene(#[from] SceneError),
    /// The game could not start.
    #[error(transparent)]
    Game(#[from] GameError),
    /// The Kismet or Matinee export is invalid.
    #[error("cannot load the level's Kismet: {0}")]
    Kismet(#[from] LoadError),
}

/// One Kismet-driven game tick.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptedTick {
    /// The game tick.
    pub report: TickReport,
    /// Presentation outputs of this frame's Kismet update (audio, narration,
    /// UI, level transitions), in emission order.
    pub outputs: Vec<Output>,
    /// The NPC events of the game tick ([`Game::npc_events`]).
    pub npc_events: Vec<NpcEvent>,
}

/// How [`load_level_with_kismet_options`] starts a converted level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelOptions {
    /// Spawn the map's NPCs and story actors ([`NpcSystem`]) into the game.
    pub npcs: bool,
    /// The time-trial game type (`ASAMUGameInfoTimeTrial`): Kismet's
    /// `IsTimeTrial` reads it, collectibles are hidden and do not collide.
    pub time_trial: bool,
}

impl Default for LevelOptions {
    fn default() -> Self {
        Self {
            npcs: true,
            time_trial: false,
        }
    }
}

/// A moving actor's collision.
#[derive(Debug, Clone, PartialEq)]
struct MoverBody {
    actor: u32,
    location: Vec3,
    rotation: [i32; 3],
    draw_scale: f32,
    draw_scale3d: Vec3,
    pre_pivot: Vec3,
    offset: Vec3,
    /// `(index into the dynamic instances, transform relative to the actor)`.
    parts: Vec<(usize, Affine)>,
    hidden_collision: bool,
}

impl MoverBody {
    fn actor_affine(&self, location: Vec3, rotation: [i32; 3]) -> Affine {
        rotation::actor_local_to_world(
            location + self.offset,
            rotation,
            self.draw_scale,
            self.draw_scale3d,
            self.pre_pivot,
        )
    }
}

/// A level's Kismet bound to a game.
#[derive(Debug, Clone)]
pub struct LevelScript {
    runtime: Runtime,
    /// `Offset` of each loaded level (index = world actor id `>> 16`).
    level_offsets: Vec<Vec3>,
    /// World actor id of each graph actor.
    ids: Vec<Option<u32>>,
    /// Graph actors of each world actor id.
    by_world: BTreeMap<u32, Vec<ActorRef>>,
    movers: BTreeMap<u32, MoverBody>,
    spawn_in_story: bool,
    host_errors: Vec<String>,
    /// `(SeqAct_Interp node, lower-case group name)` → the first bound
    /// actor's path (director cuts name the group whose actor becomes the
    /// view target).
    group_actors: BTreeMap<(usize, String), String>,
}

fn world_id(map: &LoadedMap, a: &ActorInfo) -> Option<u32> {
    let level = map.level_index(&a.package)?;
    scene::actor_id(u8::try_from(level).ok()?, a.slot)
}

/// Actors whose transform or collision Kismet changes at run time:
/// Matinee-bound actors, their passengers (actors attached to them through
/// `Base`, at any depth, which UE3 carries along; the importer lists them in
/// the actor table), the actor `SetRotationToPlayerRotation` turns, and
/// targets of destroy / change-collision actions.
fn kinetic_actors(scripts: &asamu_kismet::LevelScripts) -> BTreeSet<ActorRef> {
    use asamu_kismet::OpClass;
    let g = &scripts.graph;
    let mut bound = BTreeSet::new();
    for a in scripts.matinee.actions.values() {
        for b in &a.bindings {
            for t in &b.targets {
                if let Some(r) = t.object.as_deref().and_then(|p| g.actor_by_path(p)) {
                    bound.insert(r);
                }
            }
        }
    }
    let mut out = bound.clone();
    for (i, a) in g.actors.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else { break };
        let mut base = a.base;
        let mut steps = 0;
        while let Some(b) = base {
            steps += 1;
            if steps > asamu_kismet::runtime::MAX_ATTACH_DEPTH || b == ActorRef(i) {
                break;
            }
            if bound.contains(&b) {
                out.insert(ActorRef(i));
                break;
            }
            base = g.actor(b).and_then(|x| x.base);
        }
    }
    for n in &g.nodes {
        if !matches!(
            n.class,
            OpClass::Destroy | OpClass::ChangeCollision | OpClass::SetRotationToPlayerRotation
        ) {
            continue;
        }
        let params = n.params.values();
        let vars = n
            .variables
            .iter()
            .flat_map(|v| v.vars.iter())
            .filter_map(|var| g.node(*var).and_then(|x| x.var.as_ref()).map(|d| &d.value));
        for value in params.chain(vars) {
            let paths: Vec<&str> = match value {
                asamu_kismet::KValue::Array(items) => items
                    .iter()
                    .filter_map(asamu_kismet::KValue::as_obj)
                    .collect(),
                v => v.as_obj().into_iter().collect(),
            };
            for p in paths {
                if let Some(r) = g.actor_by_path(p) {
                    out.insert(r);
                }
            }
        }
    }
    out
}

/// Takes the kinetic actors' static collision into dynamic instances.
fn prepare_movers(
    map: &mut LoadedMap,
    scripts: &asamu_kismet::LevelScripts,
) -> BTreeMap<u32, MoverBody> {
    let g = &scripts.graph;
    let mut wanted: BTreeMap<u32, ActorInfo> = BTreeMap::new();
    for r in kinetic_actors(scripts) {
        if let Some(info) = g.actor(r)
            && let Some(id) = world_id(map, info)
        {
            wanted.entry(id).or_insert_with(|| info.clone());
        }
    }
    // Falling rocks move through their rock state (and already have dynamic
    // collision); every other kinetic actor gets a body, with or without
    // collision parts, so its transform (and its passengers') is tracked.
    for r in &map.actors.rocks {
        wanted.remove(&r.id);
    }
    let ids: BTreeSet<u32> = wanted.keys().copied().collect();
    let taken = map.collision.take_actor_statics(&ids);
    let mut bodies: BTreeMap<u32, MoverBody> = wanted
        .iter()
        .map(|(id, info)| {
            let level = (id >> 16) as usize;
            let offset = map.levels.get(level).map_or(Vec3::ZERO, |l| l.offset);
            let body = MoverBody {
                actor: *id,
                location: Vec3::from_array(info.location),
                rotation: info.rotation,
                draw_scale: info.draw_scale,
                draw_scale3d: Vec3::from_array(info.draw_scale3d),
                pre_pivot: Vec3::from_array(info.pre_pivot),
                offset,
                parts: Vec::new(),
                hidden_collision: false,
            };
            (*id, body)
        })
        .collect();
    for inst in taken {
        let Some(id) = inst.info.actor else { continue };
        let Some(body) = bodies.get_mut(&id) else {
            continue;
        };
        let actor = body.actor_affine(body.location, body.rotation);
        // A singular actor transform cannot carry its parts: they stay where
        // they are (placing them on it later fails and leaves them), rather
        // than losing the collision taken out of the static set.
        let relative = actor
            .inverse()
            .map_or(Affine::IDENTITY, |inv| inst.to_world.then(&inv));
        let Some(dynamic) = map
            .collision
            .dynamic_instance(inst.mesh, inst.to_world, inst.info)
        else {
            continue;
        };
        body.parts.push((map.dynamic.len(), relative));
        map.dynamic.push(dynamic);
    }
    bodies
}

/// Loads a converted map (as [`Game::load_level`]) together with its
/// Kismet and Matinee exports (`asamu-import kismet`, `asamu-import
/// matinee`) and its NPCs ([`LevelOptions::default`]). The script is `None`
/// when the map has no Kismet export; the game then keeps the level-start
/// ability table.
///
/// # Errors
/// Missing or malformed converted data.
pub fn load_level_with_kismet(
    converted_dir: impl AsRef<Path>,
    map: &str,
) -> Result<(Game, Option<LevelScript>), LevelScriptError> {
    load_level_with_kismet_options(converted_dir, map, LevelOptions::default())
}

/// [`load_level_with_kismet`] with explicit [`LevelOptions`].
///
/// # Errors
/// Missing or malformed converted data.
pub fn load_level_with_kismet_options(
    converted_dir: impl AsRef<Path>,
    map: &str,
    options: LevelOptions,
) -> Result<(Game, Option<LevelScript>), LevelScriptError> {
    let dir = converted_dir.as_ref();
    let mut loaded = scene::load_map_from_dir(dir, map)?;
    let subs: Vec<String> = loaded
        .levels
        .iter()
        .skip(1)
        .map(|l| l.name.clone())
        .collect();
    let scripts = match asamu_kismet::load_level_scripts(dir, &loaded.map, &subs) {
        Ok(s) => Some(s),
        Err(LoadError::Missing(_)) => None,
        Err(e) => return Err(e.into()),
    };
    let movers = scripts
        .as_ref()
        .map(|s| prepare_movers(&mut loaded, s))
        .unwrap_or_default();
    let levels = loaded.levels.clone();
    let mut game =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)?;
    if options.npcs {
        let npc_options = NpcOptions {
            time_trial: options.time_trial,
            ..NpcOptions::default()
        };
        game.attach_npcs(NpcSystem::load_from_dir(dir, &levels, npc_options));
    }
    let script = scripts.map(|s| {
        let mut script = LevelScript::attach(&mut game, s, movers);
        script.runtime.set_time_trial(options.time_trial);
        script
    });
    Ok((game, script))
}

impl LevelScript {
    /// Binds loaded scripts to `game` without collision preparation (no
    /// actor becomes a mover; use [`load_level_with_kismet`] for converted
    /// levels).
    #[must_use]
    pub fn new(game: &mut Game, scripts: asamu_kismet::LevelScripts) -> LevelScript {
        LevelScript::attach(game, scripts, BTreeMap::new())
    }

    fn attach(
        game: &mut Game,
        scripts: asamu_kismet::LevelScripts,
        movers: BTreeMap<u32, MoverBody>,
    ) -> LevelScript {
        // Undo the level-start ability table: the pawn's script state as a
        // fresh start, so the map's Kismet decides; the table is dropped so a
        // save snapshot applied later (`Game::apply_snapshot`) does not put it
        // back over the snapshot's abilities.
        if game.params.pawn.is_some() {
            let mut fresh = PlayerState::new(game.player.position, game.player.yaw);
            pawn::start(&mut fresh, &game.params);
            game.player.script = fresh.script;
        }
        game.level.abilities = asamu_world::LevelAbilities::default();
        let graph = std::sync::Arc::new(scripts.graph);
        let mut ids = Vec::with_capacity(graph.actors.len());
        let mut by_world: BTreeMap<u32, Vec<ActorRef>> = BTreeMap::new();
        for (i, a) in graph.actors.iter().enumerate() {
            let id = game.scene.as_ref().and_then(|s| world_id(&s.map, a));
            if let (Some(id), Ok(i)) = (id, u32::try_from(i)) {
                by_world.entry(id).or_default().push(ActorRef(i));
            }
            ids.push(id);
        }
        if let Some(sc) = &mut game.world.scene {
            for m in movers.values() {
                if let Err(i) = sc.actor_locations.binary_search_by_key(&m.actor, |a| a.0) {
                    sc.actor_locations
                        .insert(i, (m.actor, m.location + m.offset));
                }
            }
        }
        let level_offsets = game
            .scene
            .as_ref()
            .map(|s| s.map.levels.iter().map(|l| l.offset).collect())
            .unwrap_or_default();
        let mut group_actors = BTreeMap::new();
        for (node, a) in &scripts.matinee.actions {
            for b in &a.bindings {
                let name = b.group.as_deref().unwrap_or(&b.label).to_ascii_lowercase();
                if let Some(path) = b.targets.iter().find_map(|t| t.object.clone()) {
                    group_actors.entry((*node, name)).or_insert(path);
                }
            }
        }
        LevelScript {
            runtime: Runtime::new(graph, std::sync::Arc::new(scripts.matinee)),
            level_offsets,
            ids,
            by_world,
            movers,
            spawn_in_story: false,
            host_errors: Vec::new(),
            group_actors,
        }
    }

    /// The world actor id of the actor at object `path` (graph actor table).
    #[must_use]
    pub fn actor_id_by_path(&self, path: &str) -> Option<u32> {
        self.runtime
            .graph()
            .actor_by_path(path)
            .and_then(|r| self.world_id(r))
    }

    /// The current transform of the graph actor at object `path`: Matinee's
    /// (or attachment's) latest, else its placement; sub-level offset
    /// included. `None` for actors outside the actor table.
    #[must_use]
    pub fn actor_transform_by_path(&self, path: &str) -> Option<(Vec3, [i32; 3])> {
        let r = self.runtime.graph().actor_by_path(path)?;
        let (location, rotation) = self.runtime.actor_transform(r)?;
        let offset = self
            .world_id(r)
            .and_then(|id| self.level_offsets.get((id >> 16) as usize).copied())
            .unwrap_or(Vec3::ZERO);
        Some((Vec3::from_array(location) + offset, rotation))
    }

    /// The placement of world actor `id` in the actor table (location with
    /// the sub-level offset, rotation): where Matinee starts moving it from.
    #[must_use]
    pub fn actor_placement(&self, id: u32) -> Option<(Vec3, [i32; 3])> {
        let r = *self.actor_refs(id).first()?;
        let a = self.runtime.graph().actor(r)?;
        let offset = self
            .level_offsets
            .get((id >> 16) as usize)
            .copied()
            .unwrap_or(Vec3::ZERO);
        Some((Vec3::from_array(a.location) + offset, a.rotation))
    }

    /// The actor bound to Matinee group `group` of `SeqAct_Interp` `node`
    /// (case-insensitive group name), as an object path.
    #[must_use]
    pub fn matinee_group_actor(&self, node: usize, group: &str) -> Option<&str> {
        self.group_actors
            .get(&(node, group.to_ascii_lowercase()))
            .map(String::as_str)
    }

    /// The interpreter.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The interpreter, for tools and tests (firing events by hand).
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
    }

    /// The world actor id of graph actor `r` (`None` when it is not in a
    /// loaded level).
    #[must_use]
    pub fn world_id(&self, r: ActorRef) -> Option<u32> {
        self.ids.get(r.0 as usize).copied().flatten()
    }

    /// The graph actors (for the runtime's event API, e.g.
    /// `Runtime::anim_notify`) of world actor `id`.
    #[must_use]
    pub fn actor_refs(&self, id: u32) -> &[ActorRef] {
        self.by_world.get(&id).map_or(&[], Vec::as_slice)
    }

    /// World actor ids of the movers.
    #[must_use]
    pub fn mover_ids(&self) -> Vec<u32> {
        self.movers.keys().copied().collect()
    }

    /// Current location of mover `id` (actor location, sub-level offset
    /// included).
    #[must_use]
    pub fn mover_location(&self, id: u32) -> Option<Vec3> {
        self.movers.get(&id).map(|m| m.location + m.offset)
    }

    /// Current transform of mover `id`: actor location (sub-level offset
    /// included) and rotation.
    #[must_use]
    pub fn mover_transform(&self, id: u32) -> Option<(Vec3, [i32; 3])> {
        self.movers
            .get(&id)
            .map(|m| (m.location + m.offset, m.rotation))
    }

    /// Every actor Kismet has moved (movers and their passengers, skeletal
    /// actors, cameras, lights driven by Matinee) as `(world actor id,
    /// location, rotation)`, for rendering; locations include the actor's
    /// sub-level offset. Actors outside the loaded levels are left out.
    #[must_use]
    pub fn moved_actors(&self) -> Vec<(u32, Vec3, [i32; 3])> {
        self.runtime
            .moved_actors()
            .filter_map(|(r, l, rot)| {
                let id = self.world_id(r)?;
                let offset = self
                    .level_offsets
                    .get((id >> 16) as usize)
                    .copied()
                    .unwrap_or(Vec3::ZERO);
                Some((id, Vec3::from_array(l) + offset, rot))
            })
            .collect()
    }

    /// One Kismet-driven tick (see the module docs). `None` (and nothing
    /// runs) when the game is not playing.
    pub fn tick(&mut self, game: &mut Game, input: &InputFrame) -> Option<ScriptedTick> {
        if game.state() != crate::GameState::Playing {
            return None;
        }
        let dt = game.clock.dt();
        // 1. Base mover under a standing player.
        let base = self.base_mover(game);
        let base_before =
            base.and_then(|id| self.movers.get(&id).map(|m| (id, m.location, m.rotation)));
        // 2. Kismet, then its game-side outputs on the NPCs.
        {
            let mut host = GameHost {
                game,
                movers: &mut self.movers,
                spawn_in_story: &mut self.spawn_in_story,
            };
            self.runtime.tick(dt, &mut host);
        }
        let mut outputs = self.runtime.take_outputs();
        self.apply_game_outputs(game, &outputs);
        // 3. Carry the player.
        if let Some((id, loc0, rot0)) = base_before
            && let Some(m) = self.movers.get(&id)
            && (m.location != loc0 || m.rotation != rot0)
        {
            let old = m.actor_affine(loc0, rot0);
            let new = m.actor_affine(m.location, m.rotation);
            if let Some(inv) = old.inverse() {
                let delta = inv.then(&new);
                let p = game.player.position;
                game.player.position = delta.point(p.as_dvec3()).as_vec3();
                let dyaw = rotation::units_to_radians(m.rotation[1].wrapping_sub(rot0[1]));
                game.player.yaw = asamu_core::rotator::wrap_radians(game.player.yaw + dyaw);
            }
        }
        // 4. The game tick.
        let was_story = game.in_story_mode();
        let report = game.tick(input)?;
        // 5. Events for the next Kismet update. (Event activation only
        // queues ops, so this rarely emits anything; whatever it emits is
        // applied and returned like the update's own.)
        self.feed(game, &report, was_story);
        let late = self.runtime.take_outputs();
        self.apply_game_outputs(game, &late);
        outputs.extend(late);
        Some(ScriptedTick {
            report,
            outputs,
            npc_events: game.npc_events().to_vec(),
        })
    }

    /// The world actor id an object path names (graph actor table first,
    /// then the NPC scene's names).
    fn actor_id_of(&self, game: &Game, path: &str) -> Option<u32> {
        self.runtime
            .graph()
            .actor_by_path(path)
            .and_then(|r| self.world_id(r))
            .or_else(|| game.npcs().and_then(|n| n.scene().actor_by_name(path)))
    }

    /// The outputs whose original actions act on game actors at once: the
    /// worm controller and the follow-collision actors.
    fn apply_game_outputs(&mut self, game: &mut Game, outputs: &[Output]) {
        for o in outputs {
            match o {
                Output::Worm { action, worm } => {
                    let Some(id) = worm.as_deref().and_then(|p| self.actor_id_of(game, p)) else {
                        continue;
                    };
                    let Some(npcs) = game.npcs_mut() else {
                        continue;
                    };
                    let known = match *action {
                        "start" => npcs.start_worm(id),
                        "shutdown" => npcs.shut_down_worm(id),
                        "pause" => npcs.pause_worm(id, true),
                        "unpause" => npcs.pause_worm(id, false),
                        _ => false,
                    };
                    if !known {
                        self.note(format!("worm action {action} on unknown worm {worm:?}"));
                    }
                }
                Output::FollowCollision {
                    actor: Some(path),
                    enable,
                } => {
                    // `EnableCollision`: block-all-but-weapons or none. Only
                    // collision the converted scene has (a mover body's
                    // static-mesh parts) changes; the socket-attached
                    // collision component of these skeletal actors is not
                    // converted (TENTATIVE stand-in).
                    if let Some(id) = self.actor_id_of(game, path) {
                        set_body_collision(game, &mut self.movers, id, *enable);
                    }
                }
                _ => {}
            }
        }
    }

    /// Host-side problems (kept with the interpreter's own, capped).
    fn note(&mut self, message: String) {
        if self.host_errors.len() < asamu_kismet::runtime::MAX_ERRORS {
            self.host_errors.push(message);
        }
    }

    /// Problems the host met routing outputs (unknown worm, ...).
    #[must_use]
    pub fn host_errors(&self) -> &[String] {
        &self.host_errors
    }

    fn base_mover(&self, game: &Game) -> Option<u32> {
        if self.movers.is_empty() || !game.player.grounded {
            return None;
        }
        let shape = CollisionShape {
            radius: game.params.movement.capsule_radius.value,
            half_height: game.params.movement.capsule_half_height.value,
        };
        let p = game.player.position;
        let hit = game
            .world
            .sweep_capsule(p, p - Vec3::Z * BASE_PROBE, shape)?;
        let id = hit.surface.actor?;
        self.movers.contains_key(&id).then_some(id)
    }

    fn refs(&self, id: u32) -> Vec<ActorRef> {
        self.by_world.get(&id).cloned().unwrap_or_default()
    }

    fn feed(&mut self, game: &mut Game, report: &TickReport, was_story: bool) {
        for e in report.events.kismet.iter() {
            match e {
                SimEvent::PlayerGrappled { originator } => {
                    let r = originator.and_then(|id| self.refs(id).first().copied());
                    self.runtime.player_grappled(r);
                }
                SimEvent::PlayerReleasedGrapple => self.runtime.player_released_grapple(),
                SimEvent::PlayerLanded => self.runtime.player_landed(),
                SimEvent::PlayerRocketBoosted { boosting } => {
                    self.runtime.player_rocket_boosted(boosting);
                }
                _ => {}
            }
        }
        for e in report.world.iter() {
            match e {
                WorldEvent::Touch { id } => {
                    for r in self.refs(id) {
                        self.runtime.touch(r, true);
                    }
                }
                WorldEvent::UnTouch { id } => {
                    for r in self.refs(id) {
                        self.runtime.touch(r, false);
                    }
                }
                // With an NPC system the story-item model reports the
                // interaction (below); the world objects' count would double
                // it.
                WorldEvent::ActorInteractedWith { id } if game.npcs().is_none() => {
                    for r in self.refs(id) {
                        self.runtime.actor_interacted_with(r);
                    }
                }
                WorldEvent::PlayerDied { .. } => {
                    // `PlayerDied` exec: a pawn in story state that may spawn
                    // in story mode re-enters it (the death left it).
                    if was_story && self.spawn_in_story {
                        game.enter_story_mode();
                    }
                }
                WorldEvent::PlayerRespawned => self.runtime.player_died(),
                _ => {}
            }
        }
        for e in game.npc_events() {
            match *e {
                NpcEvent::Worm { kind, .. } => self.runtime.worm_event(kind.output()),
                NpcEvent::CollectibleCollected { id } => {
                    // The pick-up is the player's touch of the collectible:
                    // its own `SeqEvent_Touch` (the engine's touch
                    // notification) and the game's collected event.
                    for r in self.refs(id) {
                        self.runtime.touch(r, true);
                    }
                    self.runtime.collectible_collected();
                }
                NpcEvent::ActorInteractedWith { originator } => {
                    if let Some(r) = self.refs(originator).first().copied() {
                        self.runtime.actor_interacted_with(r);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Turns a mover body's collision on (blocking) or off.
fn set_body_collision(
    game: &mut Game,
    movers: &mut BTreeMap<u32, MoverBody>,
    id: u32,
    blocks: bool,
) {
    let Some(body) = movers.get_mut(&id) else {
        return;
    };
    body.hidden_collision = !blocks;
    if let Some(sc) = &mut game.world.scene {
        for (i, _) in &body.parts {
            if let Some(inst) = sc.dynamic.get_mut(*i) {
                inst.info.blocks_pawn = blocks;
                inst.info.blocks_traces = blocks;
            }
        }
    }
}

/// [`Host`] over a [`Game`].
struct GameHost<'a> {
    game: &'a mut Game,
    movers: &'a mut BTreeMap<u32, MoverBody>,
    spawn_in_story: &'a mut bool,
}

impl GameHost<'_> {
    fn id(&self, actor: &ActorInfo) -> Option<u32> {
        let map = self.game.scene.as_ref().map(|s| &s.map)?;
        world_id(map, actor)
    }

    fn set_trigger_enabled(&mut self, id: u32, mode: ToggleMode) -> bool {
        let Some(s) = &mut self.game.scene else {
            return false;
        };
        let Some(i) = s.map.actors.triggers.iter().position(|t| t.id == id) else {
            return false;
        };
        let Some(e) = s.runtime.trigger_enabled.get_mut(i) else {
            return false;
        };
        *e = match mode {
            ToggleMode::On => true,
            ToggleMode::Off => false,
            ToggleMode::Toggle => !*e,
        };
        if !*e && let Some(t) = s.runtime.trigger_touching.get_mut(i) {
            *t = false;
        }
        true
    }

    fn set_volume_mode(&mut self, id: u32, mode: ToggleMode) -> bool {
        let current = self.game.scene.as_ref().and_then(|s| {
            let i = s.map.actors.volumes.iter().position(|v| v.id == id)?;
            s.runtime.volume_enabled.get(i).copied()
        });
        let Some(cur) = current else { return false };
        let enabled = match mode {
            ToggleMode::On => true,
            ToggleMode::Off => false,
            ToggleMode::Toggle => !cur,
        };
        self.game.set_volume_enabled(id, enabled)
    }

    fn set_body_collision(&mut self, id: u32, blocks: bool) {
        set_body_collision(self.game, self.movers, id, blocks);
    }
}

impl Host for GameHost<'_> {
    fn in_story_mode(&self) -> bool {
        self.game.in_story_mode()
    }

    fn set_story_mode(&mut self, on: bool) {
        if on {
            self.game.enter_story_mode();
        } else {
            self.game.exit_story_mode();
        }
    }

    fn set_spawn_in_story_mode(&mut self, on: bool) {
        *self.spawn_in_story = on;
    }

    fn set_max_grapples(&mut self, n: i32) {
        self.game.set_max_grapples(n);
    }

    fn enable_grapple(&mut self, enable: bool) {
        self.game.enable_grapple(enable);
    }

    fn enable_rocket_boots(&mut self, enable: bool) {
        self.game.enable_rocket_boots(enable);
    }

    fn hide_grapple_gun(&mut self, hide: bool, animate: bool, visibility: bool) {
        self.game.hide_grapple_gun(hide, animate, visibility);
    }

    fn set_zoom_available(&mut self, on: bool) {
        self.game.set_zoom_available(on);
    }

    fn trigger_checkpoint(&mut self, actor: &ActorInfo) -> bool {
        let Some(id) = self.id(actor) else {
            return false;
        };
        let is_checkpoint = self
            .game
            .scene
            .as_ref()
            .is_some_and(|s| s.map.actors.checkpoints.iter().any(|c| c.id == id));
        // An already activated or disabled checkpoint ignores the call (A-CP
        // rules); that is not an error.
        self.game.trigger_checkpoint(id);
        is_checkpoint
    }

    fn set_checkpoint_enabled(&mut self, actor: &ActorInfo, enabled: bool) -> bool {
        self.id(actor)
            .is_some_and(|id| self.game.set_checkpoint_enabled(id, enabled))
    }

    fn activate_attractor(&mut self, actor: &ActorInfo) -> bool {
        self.id(actor)
            .is_some_and(|id| self.game.activate_attractor(id))
    }

    fn set_falling_rocks_active(&mut self, active: bool) {
        self.game.set_falling_rocks_active(active);
    }

    fn set_level_streamed(&mut self, package: &str, loaded: bool, _visible: bool) -> bool {
        self.game.set_level_streamed(package, loaded)
    }

    fn toggle_actor(&mut self, actor: &ActorInfo, mode: ToggleMode) {
        let Some(id) = self.id(actor) else { return };
        if !self.set_trigger_enabled(id, mode) {
            self.set_volume_mode(id, mode);
        }
    }

    fn destroy_actor(&mut self, actor: &ActorInfo) {
        if let Some(id) = self.id(actor) {
            self.set_body_collision(id, false);
            self.set_volume_mode(id, ToggleMode::Off);
            self.set_trigger_enabled(id, ToggleMode::Off);
        }
    }

    fn change_collision(&mut self, actor: &ActorInfo, collide: bool, block: bool) {
        if let Some(id) = self.id(actor) {
            self.set_body_collision(id, collide && block);
        }
    }

    fn actor_transform(&self, actor: &ActorInfo) -> Option<([f32; 3], [i32; 3])> {
        let id = self.id(actor)?;
        if let Some(m) = self.movers.get(&id) {
            return Some((m.location.to_array(), m.rotation));
        }
        let s = self.game.scene.as_ref()?;
        let i = s.map.actors.rocks.iter().position(|r| r.id == id)?;
        let st = s.runtime.rocks.get(i)?;
        Some((st.location.to_array(), st.rotation))
    }

    fn set_actor_transform(
        &mut self,
        actor: &ActorInfo,
        location: [f32; 3],
        rot: Option<[i32; 3]>,
    ) {
        let Some(id) = self.id(actor) else { return };
        let loc = Vec3::from_array(location);
        if !loc.is_finite() {
            return;
        }
        if let Some(body) = self.movers.get_mut(&id) {
            body.location = loc;
            if let Some(r) = rot {
                body.rotation = r;
            }
            let affine = body.actor_affine(body.location, body.rotation);
            if let Some(sc) = &mut self.game.world.scene {
                for (i, rel) in &body.parts {
                    if let Some(inst) = sc.dynamic.get_mut(*i) {
                        let info = inst.info;
                        sc.map.collision.place_dynamic(inst, rel.then(&affine));
                        inst.info = info;
                    }
                }
                if let Ok(i) = sc.actor_locations.binary_search_by_key(&id, |a| a.0)
                    && let Some(slot) = sc.actor_locations.get_mut(i)
                {
                    slot.1 = loc + body.offset;
                }
            }
            return;
        }
        // A falling rock driven by Matinee moves through its rock state (the
        // scene syncs its collision).
        if let Some(s) = &mut self.game.scene
            && let Some(i) = s.map.actors.rocks.iter().position(|r| r.id == id)
            && let Some(st) = s.runtime.rocks.get_mut(i)
        {
            st.location = loc;
            if let Some(r) = rot {
                st.rotation = r;
            }
        }
    }

    fn player_rotation(&self) -> [i32; 3] {
        let to_units = |r: f32| (f64::from(r) * 32768.0 / std::f64::consts::PI).round() as i32;
        [
            to_units(self.game.player.pitch),
            to_units(self.game.player.yaw),
            0,
        ]
    }

    fn teleport_player(&mut self, location: [f32; 3], rot: Option<[i32; 3]>) -> bool {
        let p = Vec3::from_array(location);
        if !p.is_finite() {
            return false;
        }
        self.game.player.position = p;
        if let Some(r) = rot {
            self.game.player.yaw = rotation::units_to_radians(r[1]);
            self.game.player.pitch = rotation::units_to_radians(rotation::normalize_axis(r[0]));
        }
        self.game.player.pawn.force_floor_check = true;
        true
    }

    fn set_player_velocity(&mut self, velocity: [f32; 3]) {
        let v = Vec3::from_array(velocity);
        if v.is_finite() {
            self.game.player.velocity = v;
        }
    }

    fn kill_player(&mut self) {
        self.game.kill_player();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_kismet::{Graph, MatineeSet};
    use asamu_world::SurfaceTag;
    use asamu_world::collision::{
        CollisionClass, CollisionSceneBuilder, InstanceInfo, QueryFilter,
    };
    use asamu_world::gameplay::{PlayerStartDef, SceneActors};
    use asamu_world::scene::{LoadStats, SubLevel, WorldSettings};
    use glam::DVec3;

    /// A runtime graph from JSON text (this crate has no JSON dependency of
    /// its own; the graph parser takes bytes).
    fn graph(nodes: &str, actors: &str) -> Graph {
        let doc = format!(
            r#"{{"format": "{}", "version": {}, "package": "T", "nodes": {nodes}, "actors": {actors}}}"#,
            asamu_kismet::RUNTIME_FORMAT,
            asamu_kismet::RUNTIME_VERSION
        );
        Graph::from_json_slice(doc.as_bytes()).unwrap()
    }

    fn scripts(g: Graph, m: MatineeSet) -> asamu_kismet::LevelScripts {
        asamu_kismet::LevelScripts {
            graph: g,
            matinee: m,
            missing_sublevels: Vec::new(),
        }
    }

    fn action(
        id: usize,
        class: &str,
        inputs: &[&str],
        next: Option<usize>,
        params: &str,
    ) -> String {
        let inputs: Vec<String> = inputs
            .iter()
            .map(|d| format!(r#"{{"desc": "{d}"}}"#))
            .collect();
        let links = next.map_or(String::new(), |n| format!(r#"{{"op": {n}, "input": 0}}"#));
        format!(
            r#"{{"id": {id}, "class": "{class}", "kind": "action", "parent": 0,
                "inputs": [{}], "outputs": [{{"desc": "Out", "links": [{links}]}}],
                "params": {params}, "auto_activate_outputs": true}}"#,
            inputs.join(",")
        )
    }

    #[test]
    fn level_start_kismet_sets_abilities_story_mode_and_transitions() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5, 6]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {}, {}, {},
                {{"id": 5, "class": "Engine.SeqAct_ConsoleCommand", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "In"}}], "outputs": [{{"desc": "Out"}}],
                  "variables": [{{"desc": "Target", "property": "Targets", "vars": [6]}}],
                  "params": {{"Commands": ["open AG-Next?game=X", "ToggleCrosshair"], "Targets": []}},
                  "auto_activate_outputs": true}},
                {{"id": 6, "class": "Engine.SeqVar_Player", "kind": "variable", "parent": 0}}
            ]"#,
            action(
                2,
                "asamu.SeqAct_SetMaxGrapples",
                &["In"],
                Some(3),
                r#"{"Grapples": 3}"#
            ),
            action(
                3,
                "asamu.SeqAct_ToggleRocketBoots",
                &["In"],
                Some(4),
                r#"{"Enable": true}"#
            ),
            action(
                4,
                "asamu.SeqAct_ToggleStoryMode",
                &["Enable", "Disable", "Toggle"],
                Some(5),
                "{}"
            ),
        );
        let g = graph(&nodes, "[]");
        let mut script = LevelScript::new(&mut game, scripts(g, MatineeSet::default()));
        // The graybox's own ability table was undone.
        assert_eq!(game.player().script.gun.max_grapples, 0);
        let t = script.tick(&mut game, &InputFrame::default()).unwrap();
        assert_eq!(game.player().script.gun.max_grapples, 3);
        assert!(game.player().script.boots.enabled);
        assert!(game.in_story_mode());
        assert!(t.outputs.contains(&Output::LevelTransition {
            map: "AG-Next".into(),
            options: Some("game=X".into())
        }));
        assert!(t.outputs.contains(&Output::ConsoleCommand {
            command: "ToggleCrosshair".into()
        }));
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
        // Fires once: later ticks change nothing.
        let t2 = script.tick(&mut game, &InputFrame::default()).unwrap();
        assert!(t2.outputs.is_empty());
    }

    /// A hand-made converted map: a floor and a 400×400×50 platform (actor 5)
    /// the player starts on; Kismet plays a Matinee that lifts the platform
    /// by 200 UU in 1 s, and its `Completed` output sets the grapple limit.
    fn lift_map() -> (LoadedMap, asamu_kismet::LevelScripts) {
        let mut b = CollisionSceneBuilder::new();
        let v: Vec<Vec3> = (0..8)
            .map(|i| Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32))
            .collect();
        let t = vec![
            [0, 1, 3],
            [0, 3, 2],
            [4, 6, 7],
            [4, 7, 5],
            [0, 4, 5],
            [0, 5, 1],
            [2, 3, 7],
            [2, 7, 6],
            [0, 2, 6],
            [0, 6, 4],
            [1, 5, 7],
            [1, 7, 3],
        ];
        let cube = b.add_mesh(v, t).unwrap();
        let info = |actor: Option<u32>, class| InstanceInfo {
            actor,
            class,
            tag: SurfaceTag::None,
            grapple_able: true,
            blocks_pawn: true,
            blocks_traces: true,
            sublevel: 0,
        };
        let floor = Affine {
            rows: [DVec3::X * 4000.0, DVec3::Y * 4000.0, DVec3::Z * 100.0],
            translation: DVec3::new(-2000.0, -2000.0, -1100.0),
        };
        b.add_static(cube, floor, info(None, CollisionClass::WorldGeometry))
            .unwrap();
        let platform = Affine {
            rows: [DVec3::X * 400.0, DVec3::Y * 400.0, DVec3::Z * 50.0],
            translation: DVec3::new(-200.0, -200.0, 0.0),
        };
        b.add_static(cube, platform, info(Some(5), CollisionClass::InterpActor))
            .unwrap();
        // A 50 UU crate attached to the lift (a passenger: not bound to the
        // Matinee itself, carried through its `Base`).
        let crate_box = Affine {
            rows: [DVec3::X * 50.0, DVec3::Y * 50.0, DVec3::Z * 50.0],
            translation: DVec3::new(100.0, 100.0, 50.0),
        };
        b.add_static(cube, crate_box, info(Some(6), CollisionClass::InterpActor))
            .unwrap();
        let map = LoadedMap {
            map: "T".into(),
            levels: vec![SubLevel {
                name: "T".into(),
                streaming_class: None,
                initially_loaded: true,
                offset: Vec3::ZERO,
                actors: 0,
            }],
            world: WorldSettings {
                title: None,
                kill_z: -100_000.0,
                soft_kill_z: false,
                default_gravity_z: -520.0,
                global_gravity_z: 0.0,
            },
            collision: b.build(),
            dynamic: Vec::new(),
            bodies: Vec::new(),
            actors: SceneActors {
                player_starts: vec![PlayerStartDef {
                    id: 1,
                    name: "PlayerStart_0".into(),
                    location: Vec3::new(0.0, 0.0, 50.0 + 46.15),
                    rotation: [0; 3],
                    enabled: true,
                    primary: true,
                    half_height: 44.0,
                }],
                ..SceneActors::default()
            },
            stats: LoadStats::default(),
            warnings: Vec::new(),
        };
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {{"id": 2, "class": "Engine.SeqAct_Interp", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "Play"}}, {{"desc": "Reverse"}}, {{"desc": "Stop"}}, {{"desc": "Pause"}}, {{"desc": "Change Dir"}}],
                  "outputs": [{{"desc": "Completed", "links": [{{"op": 5, "input": 0}}]}}, {{"desc": "Reversed"}}],
                  "variables": [{{"desc": "Data", "vars": [3]}}, {{"desc": "Lift", "vars": [4]}}],
                  "auto_activate_outputs": true, "latent": true, "latent_base": true}},
                {{"id": 3, "path": "T.Data", "class": "Engine.InterpData", "kind": "variable", "parent": 0}},
                {{"id": 4, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
                  "var": {{"value": {{"$obj": "T.TheWorld.PersistentLevel.Lift"}}}}}},
                {}
            ]"#,
            action(
                5,
                "asamu.SeqAct_SetMaxGrapples",
                &["In"],
                None,
                r#"{"Grapples": 2}"#
            ),
        );
        let g = graph(
            &nodes,
            r#"[{"path": "T.TheWorld.PersistentLevel.Lift", "name": "Lift", "class": "Engine.InterpActor",
                 "kind": "interp_actor", "package": "T", "slot": 5},
                {"path": "T.TheWorld.PersistentLevel.Crate", "name": "Crate", "class": "Engine.InterpActor",
                 "kind": "interp_actor", "package": "T", "slot": 6, "location": [100.0, 100.0, 50.0],
                 "base": "T.TheWorld.PersistentLevel.Lift"}]"#,
        );
        let key = |t: f32, z: f32| {
            format!(
                r#"{{"in": {t:?}, "out": [0.0, 0.0, {z:?}], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"}}"#
            )
        };
        let doc = format!(
            r#"{{"format": "asamu-matinee", "version": 1, "package": "T",
                "actions": [{{"path": "T.Interp", "node": 2, "scope": "level", "interp_data": "T.Data",
                              "bindings": [{{"link": 1, "label": "Lift", "group": "Lift",
                                            "targets": [{{"variable": "T.V", "object": "T.TheWorld.PersistentLevel.Lift"}}]}}]}}],
                "interp_data": [{{"path": "T.Data", "length": 1.0, "groups": [
                    {{"kind": "group", "name": "Lift", "tracks": [{{"class": "Engine.InterpTrackMove",
                      "data": {{"type": "move", "move_frame": "relative_to_initial",
                                "pos": {{"points": [{}, {}]}}, "euler": {{"points": [{}]}}}}}}]}}]}}]}}"#,
            key(0.0, 0.0),
            key(1.0, 200.0),
            key(0.0, 0.0)
        );
        let mut m = MatineeSet::default();
        m.add_json(doc.as_bytes(), 0).unwrap();
        (map, scripts(g, m))
    }

    #[test]
    fn matinee_bound_actors_become_dynamic_instances() {
        let (mut map, s) = lift_map();
        let movers = prepare_movers(&mut map, &s);
        assert_eq!(movers.keys().copied().collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(
            map.collision.statics().len(),
            1,
            "platform and its passenger left the static set"
        );
        assert_eq!(map.dynamic.len(), 2);
        // A second preparation finds no collision left to take.
        assert!(
            prepare_movers(&mut map, &s)
                .values()
                .all(|b| b.parts.is_empty())
        );
    }

    #[test]
    fn matinee_mover_moves_collision_and_carries_the_player() {
        let (mut map, s) = lift_map();
        let movers = prepare_movers(&mut map, &s);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let mut script = LevelScript::attach(&mut game, s, movers);
        game.start();
        let start_z = game.player().position.z;
        for _ in 0..90 {
            script.tick(&mut game, &InputFrame::default()).unwrap();
        }
        let lift = script.mover_location(5).unwrap();
        assert!((lift.z - 200.0).abs() < 1e-3, "lift at {lift:?}");
        assert_eq!(script.mover_transform(5), Some((lift, [0, 0, 0])));
        // The crate rode along (UE3 attachment).
        assert_eq!(
            script.moved_actors(),
            vec![
                (5, lift, [0, 0, 0]),
                (6, Vec3::new(100.0, 100.0, 250.0), [0, 0, 0])
            ]
        );
        assert_eq!(
            script.mover_location(6),
            Some(Vec3::new(100.0, 100.0, 250.0))
        );
        assert_eq!(script.actor_refs(5).len(), 1);
        assert!(script.actor_refs(99).is_empty());
        assert!(game.player().grounded, "still standing on the lift");
        let dz = game.player().position.z - start_z;
        assert!((dz - 200.0).abs() < 5.0, "player carried by {dz}");
        // The collision moved with it: a ray down the centre hits the top at
        // z = 250.
        let sc = game.world().scene.as_ref().unwrap();
        let hit = sc
            .map
            .collision
            .raycast(
                &sc.dynamic,
                Vec3::new(0.0, 0.0, 1000.0),
                Vec3::ZERO,
                QueryFilter::TRACE,
            )
            .unwrap();
        let z = 1000.0 - 1000.0 * hit.t;
        assert!((z - 250.0).abs() < 1e-3, "{z}");
        // ... and the crate's collision rode along: its top is now at 300.
        let hit = sc
            .map
            .collision
            .raycast(
                &sc.dynamic,
                Vec3::new(125.0, 125.0, 1000.0),
                Vec3::new(125.0, 125.0, 0.0),
                QueryFilter::TRACE,
            )
            .unwrap();
        let z = 1000.0 - 1000.0 * hit.t;
        assert!((z - 300.0).abs() < 1e-3, "{z}");
        // Grapple anchors follow the lift.
        assert_eq!(sc.actor_location(5).map(|l| l.z), Some(200.0));
        // `Completed` fired one update after the end: grapples set to 2.
        assert_eq!(game.player().script.gun.max_grapples, 2);
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
    }

    #[test]
    fn scripted_ticks_are_deterministic() {
        let run = || {
            let (mut map, s) = lift_map();
            let movers = prepare_movers(&mut map, &s);
            let mut game =
                Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
            let mut script = LevelScript::attach(&mut game, s, movers);
            game.start();
            let mut out = Vec::new();
            for i in 0..120 {
                let input = InputFrame {
                    move_forward: if i % 40 < 20 { 1.0 } else { 0.0 },
                    jump_pressed: i % 50 == 0,
                    ..InputFrame::default()
                };
                let t = script.tick(&mut game, &input).unwrap();
                out.push(format!("{:?} {:?}", game.player().position, t.outputs));
            }
            out
        };
        assert_eq!(run(), run());
    }

    /// The lift map's game with a worm (actor 7) and a collectible (actor 8)
    /// around the player start attached, and `nodes` as its Kismet.
    fn npc_game(nodes: &str) -> (Game, LevelScript) {
        use crate::npc::defs::{CollectibleDef, NpcCylinder, WormLight, WormParams};
        use crate::npc::{NpcScene, WormDef};
        let (mut map, _) = lift_map();
        let s = scripts(
            graph(
                nodes,
                r#"[{"path": "T.TheWorld.PersistentLevel.Worm", "name": "Worm", "class": "asamu.ASAMUNPC_WormPawn",
                     "kind": "pawn", "package": "T", "slot": 7},
                    {"path": "T.TheWorld.PersistentLevel.Gem", "name": "Gem", "class": "asamu.ASAMUCollectible",
                     "kind": "collectible", "package": "T", "slot": 8}]"#,
            ),
            MatineeSet::default(),
        );
        let movers = prepare_movers(&mut map, &s);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let scene = NpcScene {
            worms: vec![WormDef {
                id: 7,
                name: "Worm".into(),
                location: Vec3::new(5000.0, 0.0, 0.0),
                rotation: [0; 3],
                look_targets: Vec::new(),
                light: WormLight::default(),
                params: WormParams::ORIGINAL,
                meshes: Vec::new(),
            }],
            collectibles: vec![CollectibleDef {
                id: 8,
                level: "T".into(),
                name: "Gem".into(),
                location: Vec3::new(0.0, 0.0, 96.0),
                trigger: NpcCylinder {
                    center: Vec3::new(0.0, 0.0, 96.0),
                    radius: 60.0,
                    half_height: 60.0,
                },
            }],
            ..NpcScene::default()
        };
        game.attach_npcs(NpcSystem::new(scene, NpcOptions::default()));
        let script = LevelScript::attach(&mut game, s, movers);
        game.start();
        (game, script)
    }

    /// The lift map's game (no Kismet) with a worm 1,000 UU west of the
    /// player start, a scream volume around everything and a kill zone
    /// (actor 20) on the floor far away; the worm is started and the game
    /// ticked until it screams.
    fn screaming_worm_game() -> Game {
        use crate::npc::defs::{NpcHull, WormLight, WormParams, WormVolumeDef, WormVolumeRole};
        use crate::npc::{NpcScene, WormDef, WormStateName};
        use asamu_world::gameplay::{Hull, TouchVolumeDef, VolumeKind};
        let (mut map, _) = lift_map();
        let (lo, hi) = (
            Vec3::new(2900.0, 2900.0, -1000.0),
            Vec3::new(3100.0, 3100.0, -800.0),
        );
        map.actors.volumes.push(TouchVolumeDef {
            id: 20,
            name: "ASAMUKillZone_0".into(),
            class: "asamu.ASAMUKillZone".into(),
            kind: VolumeKind::KillZone,
            hulls: vec![Hull {
                planes: vec![
                    (DVec3::X, f64::from(hi.x)),
                    (DVec3::NEG_X, -f64::from(lo.x)),
                    (DVec3::Y, f64::from(hi.y)),
                    (DVec3::NEG_Y, -f64::from(lo.y)),
                    (DVec3::Z, f64::from(hi.z)),
                    (DVec3::NEG_Z, -f64::from(lo.z)),
                ],
                min: lo,
                max: hi,
            }],
            enabled: true,
        });
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let scene = NpcScene {
            worms: vec![WormDef {
                id: 7,
                name: "Worm".into(),
                location: Vec3::new(-1000.0, 0.0, 96.0),
                rotation: [0; 3],
                look_targets: Vec::new(),
                light: WormLight::default(),
                params: WormParams::ORIGINAL,
                meshes: Vec::new(),
            }],
            worm_volumes: vec![WormVolumeDef {
                id: 21,
                name: "ASAMUWormScreamVolume_0".into(),
                role: WormVolumeRole::Scream,
                hulls: vec![NpcHull::from_box(Vec3::splat(-5000.0), Vec3::splat(5000.0))],
            }],
            ..NpcScene::default()
        };
        game.attach_npcs(NpcSystem::new(scene, NpcOptions::default()));
        game.start();
        assert!(game.npcs_mut().unwrap().start_worm(7));
        // The worm notices only a moving player: walk in small circles on
        // the platform.
        for i in 0..1200u16 {
            let a = f32::from(i) * 0.05;
            let p = game.player_mut();
            p.position.x = 100.0 * a.cos();
            p.position.y = 100.0 * a.sin();
            p.velocity = Vec3::ZERO;
            game.tick(&InputFrame::default()).unwrap();
            if game.npcs().unwrap().runtime().worms[0].state == WormStateName::Screaming {
                return game;
            }
        }
        panic!("the worm never screamed");
    }

    #[test]
    fn npcs_act_inside_the_game_frame() {
        use crate::npc::{WormEventKind, WormStateName};
        use asamu_world::gameplay::DeathCause;
        let is_worm =
            |e: &NpcEvent, k: WormEventKind| matches!(e, NpcEvent::Worm { kind, .. } if *kind == k);
        // The push acts in the frame the worm ticks in: the player is moved
        // away from the worm by that frame's physics (no input).
        let mut game = screaming_worm_game();
        let mut pushed = false;
        for _ in 0..600 {
            let x0 = game.player().position.x;
            game.tick(&InputFrame::default()).unwrap();
            if game.player().velocity.x > 0.0 && game.player().position.x > x0 {
                pushed = true;
                break;
            }
        }
        assert!(
            pushed,
            "the scream pushes the player away within Game::tick"
        );
        // The scream times out: the worm's kill is that frame's death
        // (scripted cause), reported with the worm's own stop.
        // (The player is put back on the platform every frame, so the pushes
        // do not throw it off the map.)
        let mut killed = false;
        for i in 0..3600u16 {
            let a = f32::from(i) * 0.05;
            let p = game.player_mut();
            p.position = Vec3::new(100.0 * a.cos(), 100.0 * a.sin(), 96.15);
            p.velocity = Vec3::ZERO;
            let r = game.tick(&InputFrame::default()).unwrap();
            if r.died.is_some() {
                assert_eq!(r.died, Some(DeathCause::Scripted));
                assert!(
                    game.npc_events()
                        .iter()
                        .any(|e| is_worm(e, WormEventKind::StoppedScreaming)),
                    "{:?}",
                    game.npc_events()
                );
                killed = true;
                break;
            }
        }
        assert!(killed, "the scream timed out and killed");

        // A kill-zone death reaches the worm's `NotifyKilled`: it stops
        // screaming in that frame.
        let mut game = screaming_worm_game();
        let p = game.player_mut();
        p.position = Vec3::new(3000.0, 3000.0, -1000.0 + 46.0);
        p.velocity = Vec3::ZERO;
        let r = game.tick(&InputFrame::default()).unwrap();
        assert_eq!(r.died, Some(DeathCause::KillZone));
        assert!(
            game.npc_events()
                .iter()
                .any(|e| is_worm(e, WormEventKind::StoppedScreaming)),
            "{:?}",
            game.npc_events()
        );
        assert_eq!(
            game.npcs().unwrap().runtime().worms[0].state,
            WormStateName::Sleeping
        );

        // A scripted death (Kismet's `PlayerDied`, F7) does not.
        let mut game = screaming_worm_game();
        game.kill_player();
        game.tick(&InputFrame::default()).unwrap();
        assert!(
            !game
                .npc_events()
                .iter()
                .any(|e| is_worm(e, WormEventKind::StoppedScreaming)),
            "{:?}",
            game.npc_events()
        );
        assert_eq!(
            game.npcs().unwrap().runtime().worms[0].state,
            WormStateName::Screaming
        );
    }

    #[test]
    fn worm_actions_reach_the_npcs_and_worm_events_reach_kismet() {
        // Level start → StartWorm → PauseWorm with both inputs in one
        // impulse (UnPause, then Pause: the worm ends paused). The worm's
        // `WakingUp` event fires the level's `SeqEvent_WormEvents` output 0,
        // which sets the grapple limit to 4.
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {{"id": 2, "class": "asamu.SeqAct_StartWorm", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "Start"}}],
                  "outputs": [{{"desc": "Out", "links": [{{"op": 3, "input": 0}}, {{"op": 3, "input": 1}}]}}],
                  "params": {{"wormPawn": {{"$obj": "T.TheWorld.PersistentLevel.Worm"}}}}}},
                {{"id": 3, "class": "asamu.SeqAct_PauseWorm", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "UnPause"}}, {{"desc": "Pause"}}], "outputs": [{{"desc": "Out"}}],
                  "params": {{"wormPawn": {{"$obj": "T.TheWorld.PersistentLevel.Worm"}}}}}},
                {{"id": 4, "class": "asamu.SeqEvent_WormEvents", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "WakingUp", "links": [{{"op": 5, "input": 0}}]}}, {{"desc": "Awaken"}},
                              {{"desc": "FallingAsleep"}}, {{"desc": "Alerted"}}, {{"desc": "Screaming"}},
                              {{"desc": "StoppedScreaming"}}, {{"desc": "FinishedAlerted"}}],
                  "event": {{"max_trigger_count": 0}}}},
                {}
            ]"#,
            action(
                5,
                "asamu.SeqAct_SetMaxGrapples",
                &["In"],
                None,
                r#"{"Grapples": 4}"#
            ),
        );
        let (mut game, mut script) = npc_game(&nodes);
        let t = script.tick(&mut game, &InputFrame::default()).unwrap();
        let worm: Vec<&str> = t
            .outputs
            .iter()
            .filter_map(|o| match o {
                Output::Worm { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
        assert_eq!(worm, vec!["start", "unpause", "pause"]);
        let w = &game.npcs().unwrap().runtime().worms[0];
        assert!(w.paused, "both inputs in one impulse end paused");
        assert!(
            script.host_errors().is_empty(),
            "{:?}",
            script.host_errors()
        );
        // The start's events are reported with the next NPC tick; Kismet
        // sees them in the update after.
        for _ in 0..3 {
            script.tick(&mut game, &InputFrame::default()).unwrap();
        }
        assert_eq!(game.player().script.gun.max_grapples, 4);
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
    }

    #[test]
    fn pause_worm_inputs_on_their_own() {
        // Start → PauseWorm A with "Pause" only (input 1) → PauseWorm B with
        // "UnPause" only (input 0): each emits only its own input's action,
        // in that order, and the worm ends unpaused.
        let nodes = r#"[
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4]},
            {"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
              "outputs": [{"desc": "Loaded and Visible", "links": [{"op": 2, "input": 0}]}],
              "event": {"max_trigger_count": 1}},
            {"id": 2, "class": "asamu.SeqAct_StartWorm", "kind": "action", "parent": 0,
              "inputs": [{"desc": "Start"}],
              "outputs": [{"desc": "Out", "links": [{"op": 3, "input": 1}]}],
              "params": {"wormPawn": {"$obj": "T.TheWorld.PersistentLevel.Worm"}}},
            {"id": 3, "class": "asamu.SeqAct_PauseWorm", "kind": "action", "parent": 0,
              "inputs": [{"desc": "UnPause"}, {"desc": "Pause"}],
              "outputs": [{"desc": "Out", "links": [{"op": 4, "input": 0}]}],
              "params": {"wormPawn": {"$obj": "T.TheWorld.PersistentLevel.Worm"}}},
            {"id": 4, "class": "asamu.SeqAct_PauseWorm", "kind": "action", "parent": 0,
              "inputs": [{"desc": "UnPause"}, {"desc": "Pause"}], "outputs": [{"desc": "Out"}],
              "params": {"wormPawn": {"$obj": "T.TheWorld.PersistentLevel.Worm"}}}
        ]"#;
        let (mut game, mut script) = npc_game(nodes);
        let t = script.tick(&mut game, &InputFrame::default()).unwrap();
        let worm: Vec<&str> = t
            .outputs
            .iter()
            .filter_map(|o| match o {
                Output::Worm { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
        assert_eq!(worm, vec!["start", "pause", "unpause"]);
        assert!(!game.npcs().unwrap().runtime().worms[0].paused);
        assert!(
            script.host_errors().is_empty(),
            "{:?}",
            script.host_errors()
        );
    }

    #[test]
    fn a_collected_collectible_fires_its_kismet_events() {
        // The player starts inside the collectible's trigger: the pick-up's
        // `SeqEvent_CollectibleCollected` sets 2 grapples, the collectible's
        // own touch event sets nothing else but counts.
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3]}},
                {{"id": 1, "class": "asamu.SeqEvent_CollectibleCollected", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Out", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 0}}}},
                {},
                {{"id": 3, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Touched"}}, {{"desc": "UnTouched"}}, {{"desc": "Empty"}}],
                  "event": {{"originator": "T.TheWorld.PersistentLevel.Gem", "max_trigger_count": 0}}}}
            ]"#,
            action(
                2,
                "asamu.SeqAct_SetMaxGrapples",
                &["In"],
                None,
                r#"{"Grapples": 2}"#
            ),
        );
        let (mut game, mut script) = npc_game(&nodes);
        let mut collected = false;
        for _ in 0..4 {
            let t = script.tick(&mut game, &InputFrame::default()).unwrap();
            collected |= t
                .npc_events
                .iter()
                .any(|e| matches!(e, NpcEvent::CollectibleCollected { id: 8 }));
        }
        assert!(collected);
        assert!(game.npcs().unwrap().runtime().collectibles[0].collected);
        assert_eq!(game.player().script.gun.max_grapples, 2);
        assert_eq!(
            script.runtime().activate_count(3),
            1,
            "the collectible's touch"
        );
        assert_eq!(
            crate::npc::collectible_save_key(&game.npcs().unwrap().scene().collectibles[0]),
            "TheWorld.PersistentLevel.Gem"
        );
    }

    #[test]
    fn level_options_and_the_ability_table_under_kismet() {
        // With a script the level-start table is dropped, so a snapshot's
        // abilities are not overwritten by it.
        let (mut game, _script) = npc_game(
            r#"[{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": []}]"#,
        );
        assert_eq!(
            game.level().abilities,
            asamu_world::LevelAbilities::default()
        );
        let snap = crate::save::Snapshot {
            abilities: Some(crate::save::Abilities {
                max_grapples: 3,
                rocket_boots: true,
                grapple_enabled: true,
            }),
            ..crate::save::Snapshot::default()
        };
        game.apply_snapshot(&snap);
        assert_eq!(game.player().script.gun.max_grapples, 3);
        assert!(game.player().script.boots.enabled);
        let o = LevelOptions::default();
        assert!(o.npcs && !o.time_trial);
    }

    /// What happened in a 600-tick run of a converted map.
    #[derive(Debug, Default)]
    struct RunSummary {
        max_grapples: Vec<i32>,
        boots_enabled_at: Option<usize>,
        story_mode_at: Option<usize>,
        outputs: BTreeMap<String, usize>,
        transitions: Vec<String>,
        errors: Vec<String>,
        activations: BTreeMap<String, u64>,
        movers: usize,
        moved: usize,
    }

    impl RunSummary {
        #[allow(dead_code)]
        fn movers(&self) -> usize {
            self.movers
        }
    }

    fn run_map(dir: &Path, map: &str, ticks: usize) -> Option<RunSummary> {
        let (mut game, script) = load_level_with_kismet(dir, map).ok()?;
        let mut script = script?;
        game.start();
        let mut sum = RunSummary {
            movers: script.mover_ids().len(),
            ..RunSummary::default()
        };
        let starts: BTreeMap<u32, Vec3> = script
            .mover_ids()
            .into_iter()
            .filter_map(|id| script.mover_location(id).map(|l| (id, l)))
            .collect();
        for i in 0..ticks {
            let t = script.tick(&mut game, &InputFrame::default())?;
            let g = game.player().script.gun.max_grapples;
            if sum.max_grapples.last() != Some(&g) {
                sum.max_grapples.push(g);
            }
            if sum.boots_enabled_at.is_none() && game.player().script.boots.enabled {
                sum.boots_enabled_at = Some(i);
            }
            if sum.story_mode_at.is_none() && game.in_story_mode() {
                sum.story_mode_at = Some(i);
            }
            for o in &t.outputs {
                let name = format!("{o:?}");
                let kind = name.split([' ', '{', '(']).next().unwrap_or("").to_owned();
                *sum.outputs.entry(kind).or_insert(0) += 1;
                if let Output::LevelTransition { map, .. } = o {
                    sum.transitions.push(map.clone());
                }
            }
        }
        sum.moved = starts
            .iter()
            .filter(|(id, l)| script.mover_location(**id).is_some_and(|n| n != **l))
            .count();
        sum.errors = script.runtime().errors().to_vec();
        sum.activations = script.runtime().stats().activations.clone();
        Some(sum)
    }

    fn converted_dir() -> Option<std::path::PathBuf> {
        let d = std::path::PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
        d.join("kismet").is_dir().then_some(d)
    }

    /// The originators of touch events from which output links lead (within
    /// a few ops) to an op of `class` that `pick` accepts.
    fn touches_reaching(
        g: &asamu_kismet::Graph,
        class: asamu_kismet::OpClass,
        pick: impl Fn(&asamu_kismet::graph::Node) -> bool,
    ) -> Vec<ActorRef> {
        let mut out = Vec::new();
        for ev in g
            .nodes
            .iter()
            .filter(|n| n.class == asamu_kismet::OpClass::Touch)
        {
            let mut frontier = vec![ev.id];
            let mut seen = BTreeSet::new();
            let mut hit = false;
            for _ in 0..4 {
                let mut next = Vec::new();
                for id in frontier {
                    let Some(n) = g.node(id) else { continue };
                    for o in &n.outputs {
                        for (t, _) in &o.links {
                            if seen.insert(*t) {
                                if g.node(*t).is_some_and(|x| x.class == class && pick(x)) {
                                    hit = true;
                                }
                                next.push(*t);
                            }
                        }
                    }
                }
                frontier = next;
            }
            if hit
                && let Some(a) = ev
                    .event
                    .as_ref()
                    .and_then(|e| e.originator.as_deref())
                    .and_then(|p| g.actor_by_path(p))
            {
                out.push(a);
            }
        }
        out
    }

    /// Gated on converted data (see below): the story's later steps the
    /// shipped maps script. IceCave's exit trigger streams `TheCore` in, whose
    /// scripts attach and begin play; the end of the credits opens
    /// AG-Epilogue. Re-triggering an already activated checkpoint (Darkcave's
    /// level-start one) is not an error. StarHaven's airships carry their
    /// passengers (actors attached through `Base`) rigidly.
    #[test]
    fn converted_story_steps_streaming_checkpoints_and_passengers() {
        let Some(dir) = converted_dir() else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted Kismet not set");
            return;
        };
        if let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-IceCave") {
            game.start();
            script.tick(&mut game, &InputFrame::default()).unwrap();
            assert!(!script.runtime().is_level_attached("TheCore"));
            let g = script.runtime().graph().clone();
            let triggers =
                touches_reaching(&g, asamu_kismet::OpClass::MultiLevelStreaming, |_| true);
            assert_eq!(triggers.len(), 1, "one streaming trigger");
            script.runtime_mut().touch(triggers[0], true);
            for _ in 0..3 {
                script.tick(&mut game, &InputFrame::default()).unwrap();
            }
            assert!(script.runtime().is_level_attached("TheCore"));
            let core = g.level_index("TheCore").unwrap();
            assert!(
                g.nodes.iter().any(|n| n.level == core
                    && n.class == asamu_kismet::OpClass::LevelLoaded
                    && script.runtime().activate_count(n.id) > 0),
                "TheCore's level-loaded event ran"
            );
            script.runtime_mut().credits_ended();
            // The `open` action's input waits 6 s (its `ActivateDelay`).
            let mut transitions = Vec::new();
            for _ in 0..420 {
                let t = script.tick(&mut game, &InputFrame::default()).unwrap();
                for o in t.outputs {
                    if let Output::LevelTransition { map, .. } = o {
                        transitions.push(map);
                    }
                }
            }
            assert_eq!(transitions, vec!["AG-Epilogue".to_owned()]);
            assert!(
                script.runtime().errors().is_empty(),
                "{:?}",
                script.runtime().errors()
            );
        }
        if let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-Darkcave") {
            game.start();
            script.tick(&mut game, &InputFrame::default()).unwrap();
            let again: Vec<usize> = script
                .runtime()
                .graph()
                .nodes
                .iter()
                .filter(|n| {
                    n.class == asamu_kismet::OpClass::TriggerCheckpoint
                        && script.runtime().activate_count(n.id) > 0
                })
                .map(|n| n.id)
                .collect();
            assert_eq!(again.len(), 1, "one checkpoint triggered at level start");
            script.runtime_mut().force_input(again[0], 0);
            script.tick(&mut game, &InputFrame::default()).unwrap();
            assert_eq!(script.runtime().activate_count(again[0]), 2);
            assert!(
                script.runtime().errors().is_empty(),
                "{:?}",
                script.runtime().errors()
            );
        }
        if let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-StarHaven") {
            game.start();
            let g = script.runtime().graph().clone();
            // The Matinee-moved actor with the most passengers, and an
            // action that moves it.
            let mut counts: BTreeMap<ActorRef, usize> = BTreeMap::new();
            for a in &g.actors {
                if let Some(b) = a.base {
                    *counts.entry(b).or_insert(0) += 1;
                }
            }
            let (ship, n) = counts
                .iter()
                .max_by_key(|(r, n)| (**n, std::cmp::Reverse(**r)))
                .map(|(r, n)| (*r, *n))
                .unwrap();
            assert!(n >= 30, "airship passengers: {n}");
            let ship_path = g.actor(ship).unwrap().path.clone();
            let ops: Vec<usize> = g
                .nodes
                .iter()
                .filter(|x| x.class == asamu_kismet::OpClass::Interp)
                .filter(|x| {
                    x.variables.iter().flat_map(|v| v.vars.iter()).any(|v| {
                        g.node(*v)
                            .and_then(|y| y.var.as_ref())
                            .and_then(|d| d.value.as_obj())
                            .is_some_and(|p| p.eq_ignore_ascii_case(&ship_path))
                    })
                })
                .map(|x| x.id)
                .collect();
            assert!(!ops.is_empty());
            let ship_id = script.world_id(ship).unwrap();
            let passengers: Vec<u32> = g
                .actors
                .iter()
                .enumerate()
                .filter(|(_, a)| a.base == Some(ship))
                .filter_map(|(i, _)| script.world_id(ActorRef(u32::try_from(i).ok()?)))
                .filter(|id| script.mover_location(*id).is_some())
                .collect();
            let start = script.mover_location(ship_id).unwrap();
            let offsets: Vec<f32> = passengers
                .iter()
                .map(|p| (script.mover_location(*p).unwrap() - start).length())
                .collect();
            script.runtime_mut().force_input(ops[0], 0);
            for _ in 0..300 {
                script.tick(&mut game, &InputFrame::default()).unwrap();
            }
            let end = script.mover_location(ship_id).unwrap();
            assert!((end - start).length() > 100.0, "the airship flew");
            let mut rigid = 0;
            for (p, d0) in passengers.iter().zip(&offsets) {
                let d = (script.mover_location(*p).unwrap() - end).length();
                if (d - d0).abs() < 0.5 {
                    rigid += 1;
                }
            }
            // Passengers with Matinee of their own (propellers, wings) move
            // relative to the ship; the rest stay rigidly in place on it.
            assert!(
                rigid + 3 >= passengers.len(),
                "{rigid} of {}",
                passengers.len()
            );
        }
    }

    /// Gated on converted data: AG-Darkcave's worm, started by its Kismet
    /// (a touch volume's `SeqAct_StartWorm`), wakes up in the game's NPC
    /// system and its `WakingUp` event reaches the level's
    /// `SeqEvent_WormEvents`.
    #[test]
    fn converted_darkcave_worm_wakes_through_kismet() {
        let Some(dir) = converted_dir() else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted Kismet not set");
            return;
        };
        let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-Darkcave") else {
            eprintln!("SKIP: AG-Darkcave not converted");
            return;
        };
        game.start();
        script.tick(&mut game, &InputFrame::default()).unwrap();
        let npcs = game.npcs().expect("NPCs attached");
        assert_eq!(npcs.scene().worms.len(), 1, "one worm");
        assert_eq!(
            npcs.runtime().worms[0].state,
            crate::npc::WormStateName::Disabled
        );
        let g = script.runtime().graph().clone();
        let volumes = touches_reaching(&g, asamu_kismet::OpClass::StartWorm, |_| true);
        assert!(!volumes.is_empty(), "touch volumes start the worm");
        script.runtime_mut().touch(volumes[0], true);
        let mut woke = false;
        for _ in 0..120 {
            let t = script.tick(&mut game, &InputFrame::default()).unwrap();
            woke |= t.npc_events.iter().any(|e| {
                matches!(
                    e,
                    NpcEvent::Worm {
                        kind: crate::npc::WormEventKind::WakingUp,
                        ..
                    }
                )
            });
        }
        assert!(woke, "the worm woke up");
        assert_ne!(
            game.npcs().unwrap().runtime().worms[0].state,
            crate::npc::WormStateName::Disabled
        );
        // The ops behind the enabled worm events' `WakingUp` output ran.
        let linked: Vec<usize> = g
            .nodes
            .iter()
            .filter(|n| n.class == asamu_kismet::OpClass::WormEvents && n.enabled)
            .filter_map(|n| n.outputs.first())
            .flat_map(|o| o.links.iter().map(|(t, _)| *t))
            .collect();
        assert!(!linked.is_empty());
        assert!(
            linked
                .iter()
                .any(|t| script.runtime().activate_count(*t) > 0),
            "SeqEvent_WormEvents fired its WakingUp output"
        );
        assert!(
            script.host_errors().is_empty(),
            "{:?}",
            script.host_errors()
        );
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
    }

    /// Gated on converted data (`ASAMU_CONVERTED_DIR` with `levels`,
    /// `meshes`, `kismet` and `matinee`; skips without it): every story map
    /// runs 600 ticks from level start without interpreter errors, with the
    /// level-start ability milestones the maps' Kismet produces on a fresh
    /// shipped-game start (no editor-only `IsPIE` paths), and the touch
    /// unlocks of ParadiseCave (grapple) and StarHaven (rocket boots).
    #[test]
    fn converted_maps_run_their_kismet() {
        let Some(dir) = converted_dir() else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted Kismet not set");
            return;
        };
        // (map, grapple limits seen from level start, boots enabled at start,
        // story mode at start)
        let expect: [(&str, &[i32], bool, bool); 7] = [
            ("AG-Workshop", &[0], false, true),
            ("AG-ParadiseCave", &[0], false, false),
            ("AG-BeautifulCity", &[2], false, true),
            ("AG-Darkcave", &[3], false, false),
            ("AG-StarHaven", &[3], false, true),
            ("AG-IceCave", &[3], true, false),
            ("AG-Epilogue", &[0], false, true),
        ];
        let mut ran = 0;
        for (map, limits, boots, story) in expect {
            let Some(sum) = run_map(&dir, map, 600) else {
                eprintln!("SKIP {map}: not converted");
                continue;
            };
            ran += 1;
            eprintln!(
                "{map}: limits {:?} boots {:?} story {:?} movers {}/{} outputs {:?}",
                sum.max_grapples,
                sum.boots_enabled_at,
                sum.story_mode_at,
                sum.moved,
                sum.movers,
                sum.outputs
            );
            assert!(sum.errors.is_empty(), "{map}: {:?}", sum.errors);
            assert_eq!(sum.max_grapples, limits, "{map}");
            assert_eq!(sum.boots_enabled_at.is_some(), boots, "{map}");
            assert_eq!(sum.story_mode_at == Some(0), story, "{map}");
            assert!(
                sum.activations.contains_key("SeqEvent_LevelLoaded"),
                "{map}"
            );
        }
        if ran == 0 {
            return;
        }
        // ParadiseCave: the touch volume that unlocks the grapple.
        if let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-ParadiseCave") {
            game.start();
            script.tick(&mut game, &InputFrame::default()).unwrap();
            let volumes = touches_reaching(
                script.runtime().graph(),
                asamu_kismet::OpClass::ToggleGrapple,
                |_| true,
            );
            assert_eq!(volumes.len(), 1, "one grapple unlock volume");
            game.player_mut().script.gun.can_grapple = false;
            script.runtime_mut().touch(volumes[0], true);
            for _ in 0..3 {
                script.tick(&mut game, &InputFrame::default()).unwrap();
            }
            assert_eq!(game.player().script.gun.max_grapples, 1);
            assert!(game.player().script.gun.can_grapple, "grapple enabled");
        }
        // StarHaven: the trigger that enables the rocket boots.
        if let Ok((mut game, Some(mut script))) = load_level_with_kismet(&dir, "AG-StarHaven") {
            game.start();
            script.tick(&mut game, &InputFrame::default()).unwrap();
            assert!(!game.player().script.boots.enabled);
            let triggers = touches_reaching(
                script.runtime().graph(),
                asamu_kismet::OpClass::ToggleRocketBoots,
                |n| n.param("Enable").is_some_and(asamu_kismet::KValue::as_bool),
            );
            assert!(!triggers.is_empty());
            // The trigger's touch event starts disabled; the cutscene that
            // precedes it turns it on with a `SeqAct_Toggle`. Pulse those
            // toggles' "Turn On" input first.
            let g = script.runtime().graph().clone();
            let events: Vec<usize> = g
                .nodes
                .iter()
                .filter(|n| {
                    n.class == asamu_kismet::OpClass::Touch
                        && n.event
                            .as_ref()
                            .and_then(|e| e.originator.as_deref())
                            .and_then(|p| g.actor_by_path(p))
                            == Some(triggers[0])
                })
                .map(|n| n.id)
                .collect();
            for e in &events {
                if !script.runtime().is_enabled(*e) {
                    for t in g.nodes.iter().filter(|n| {
                        n.class == asamu_kismet::OpClass::Toggle
                            && n.event_links.iter().any(|l| l.events.contains(e))
                    }) {
                        script.runtime_mut().force_input(t.id, 0);
                    }
                }
            }
            script.tick(&mut game, &InputFrame::default()).unwrap();
            assert!(events.iter().all(|e| script.runtime().is_enabled(*e)));
            script.runtime_mut().touch(triggers[0], true);
            for _ in 0..3 {
                script.tick(&mut game, &InputFrame::default()).unwrap();
            }
            assert!(game.player().script.boots.enabled, "rocket boots enabled");
        }
    }
}
