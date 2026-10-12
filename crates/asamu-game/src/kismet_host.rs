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
//!    narrator's and beat actors' timers; then the actors the engine turns
//!    by itself turn, with what is attached to them ("Turning actors");
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
//! Moving actors report their location and rotation for grapple anchors,
//! which ride them (GRAPPLE.md G-AT-8). Falling rocks bound to Matinee move
//! through the rock state instead.
//!
//! In the original a Matinee does not move its actors in the sequence
//! update: that only advances the action's position (the movement track's
//! own update is an empty function), and each actor in the interpolating
//! physics moves itself to its track's value in its own tick
//! (`AActor::physInterpolating @ 0x100AE5A70`, CONFIRMED (native)), after
//! the frame's input events. Ours are moved in step 2, before them: a fire
//! trace meets a mover one tick's motion further on than the original's
//! (the deviation of the frame order below).
//!
//! **Turning actors.** The engine turns an actor by itself when its
//! `RotationRate` is not zero and its physics is neither `PHYS_None` nor the
//! interpolating one: at the end of its physics step the rotation gains,
//! member by member, the rate times the tick's time with the fraction
//! dropped, and the actor is moved to that rotation where it stands, which
//! carries what is attached to it. No remainder is kept, so the speed
//! depends on the frame time: 9102 units/s are 116 units in a frame of
//! 12.82 ms (9048 units/s) and 151 in a tick of 1/60 s (9060). CONFIRMED
//! (native): `AActor::performPhysics @ 0x100AD86E0` returns at once for
//! physics 0 and otherwise, after its dispatch, calls
//! `AActor::physicsRotation @ 0x100AD93C0` unless the physics is 7
//! (interpolating) or 10, when the low 16 bits of a member of the rate
//! (Actor+0x1FC) are set; that function converts each product with a
//! truncating `cvttss2si` (`0x100AD9403`..`0x100AD9438`) and moves the actor
//! with a zero translation (`0x100AD951C`).
//!
//! A Matinee movement track moves an actor only in the interpolating
//! physics (above: the dispatch calls `physInterpolating` for physics 7
//! only). So an actor the level places in `PHYS_Rotating` with a rate turns
//! by that rate even when a Matinee group names it, and an actor in
//! `PHYS_Interpolating` with a rate follows its track and never the rate.
//!
//! Measured on the recording KM1 in AG-ParadiseCave, from the path of the
//! grapple's anchor helper riding these rocks (`InterpActor_*`; frames of
//! 12.82 ms):
//!
//! | Rock | Physics, rate | Frames | Turned, units | Against the truncated steps | Against rate × time |
//! |---|---|---|---|---|---|
//! | `_51` | rotating, yaw −9102, no Matinee | 2,252 | 261,135 | −2.2 to +12.9 | 1,483 short |
//! | `_25`/`_101` | rotating, roll −4551, in a Matinee group (−359° in 14.4 s) | 1,828 | 105,968 | −3.5 to +0.8 | 611 short (318 short of the track) |
//! | eight stretches on six wheels | interpolating, rates 4551 to 9102, tracks of one turn | 11,624 | 27,473 to 296,038 | up to 735 ahead | 3 short to 64 ahead of the track's length |
//!
//! `_51` turned 116 units in 2,249 of its 2,251 single frames. The rocks in
//! the interpolating physics follow their tracks: each loop of a track that
//! ends at 359.9° shows as a jump of 17.7 to 19.4 units (0.1° is 18.2).
//!
//! Ours: the level's own data names these actors
//! (`asamu_world::scene::PlacedActor`: `PHYS_Rotating` and a rate). Each is
//! a mover, with everything attached to it; it is taken out of the Matinee
//! groups that name it, and step 2 turns it by [`rotation_step`] of the
//! game's tick. The shipped story levels have 14: 7 in AG-ParadiseCave (3
//! of them in Matinee groups), 2 in AG-BeautifulCity and 5 in AG-StarHaven,
//! which has 8 more in that physics without a rate; those never turn
//! (census of the level data; gated test `tests/parity_moving_anchor.rs`).
//! An actor with a rate in another physics than these two is not modelled
//! (none is placed). [`LevelScript::moved_actors`] reports the turning
//! actors for drawing like the actors Kismet moves.
//!
//! **Attached actors.** When an actor has moved, the original moves every
//! actor in its attached list, by the attached actor's kind (CONFIRMED
//! (native), `UWorld::MoveActor @ 0x1008E94A0`, the loop from
//! `0x1008E9E9B`; a move that only turns the actor runs it too, and each
//! attached actor is moved by the same routine, so chains follow):
//!
//! - in the interpolating physics, or hard-attached and not blocking
//!   actors: to the transform it keeps relative to its base
//!   (`RelativeLocation`, `RelativeRotation`) on the base's new transform,
//!   its rotation rebuilt from the matrix (kept as it is with
//!   `bIgnoreBaseRotation`). What it keeps is set when its base is set
//!   (`AActor::SetBase @ 0x1006D0EB0`): for a hard attachment its whole
//!   transform relative to the base's; without one its offset from the
//!   base in world axes and its rotation relative to the base's;
//! - otherwise with `bIgnoreBaseRotation`: by the base's translation;
//! - otherwise: by the base's change of transform (its location through
//!   the inverse of the base's old transform and through the new one), and
//!   its rotation gains the base's change, each member with its low two
//!   bits left out on both sides. The grapple's anchor helper is carried
//!   this way (`asamu_player::grapple_gun::carried_by_base`).
//!
//! All matrices use the 16 384-entry sine table. A pawn's location is
//! carried like any actor's; its rotation goes through its script. A
//! hard-attached actor that is blocked on the way undoes its base's whole
//! move; that, and pushing in general, is not modelled (below).
//!
//! Ours: what is attached to a turning actor is carried by these rules
//! ([`Passenger`]), with the relative transform taken from the two actors'
//! placements. The passengers of an actor a Matinee moves are carried by
//! the interpreter, which treats every attachment as a hard one
//! (`asamu_kismet::Runtime`; hard attachments are 118 of the 120 passengers
//! of AG-StarHaven's airships and all of AG-ParadiseCave's and
//! AG-BeautifulCity's).
//!
//! **Basing (TENTATIVE).** A grounded player standing on a mover (the actor
//! a short downward sweep of the pawn's box hits: the shape its floor check
//! stands it with, so a mover whose top leans along both X and Y is found
//! under the box's corner) is moved by the mover's change of transform
//! that frame (position through the full transform, yaw by the yaw change),
//! whatever moved the mover: a Matinee, the engine's turn, or its own base.
//! The original carries a based pawn's location in the same way ("Attached
//! actors") and hands the turn to the pawn's script, which was not read;
//! pushing and crushing by a mover are not modelled.
//!
//! **Abilities.** With Kismet attached, the hard-coded level-start ability
//! table (`asamu_world::level_start_abilities`) that [`Game::load_level`]
//! applies is undone (the pawn's script state is reset to a fresh start)
//! and the map's own Kismet sets abilities, as in the original.
//!
//! **Camera animations.** The level script owns the player camera's
//! animation pool ([`asamu_kismet::camera_anim::CameraAnims`], loaded from
//! `matinee/camera_anims.json` and the maps' own camera animations): after
//! the game tick the frame's gameplay events become the script's
//! `PlayCameraAnim` / `StopCameraAnim` calls (grapple attach and release,
//! landings, power-jump charge, cancel and jump, rocket-boots charge, boost,
//! end and reset (a cancelling landing, the death reset), the worm's growl),
//! Kismet's `SeqAct_PlayCameraAnim` plays
//! or stops in step 2b, and the pool advances by the tick
//! ([`LevelScript::camera_anims`] gives the app the point-of-view offsets).
//!
//! **Skeletal actors.** Matinee animation-control tracks, skeletal-control
//! strength tracks and `SeqAct_SetLookAtTarget` reach the NPC system
//! (`SetAnimPosition`: the slot node and its notifies; Kismet notifies fired
//! by Matinee activate their events inside the update); the skinned actors'
//! own animations tick with the NPCs and their Kismet notifies reach the
//! interpreter in step 5 ([`NpcEvent::AnimNotify`]). Matinee property tracks
//! are kept by the interpreter for rendering (lights); `DrawScale3D` also
//! rescales a mover's collision.
//!
//! **NPC pawn collision.** The NPC pawns' collision cylinders (the worm's
//! 200 × 500 cylinder in AG-Darkcave) are added as dynamic instances that
//! block the player (not traces) and follow the pawns before each tick.
//!
//! **Blocking volumes (P5).** A `SeqAct_Toggle` whose target is a blocking
//! volume switches that volume's collision: its "Turn On" input makes the
//! volume collide, "Turn Off" stops it and "Toggle" flips it, each keeping
//! the volume's block flag (CONFIRMED (src): the toggle handler of
//! `Engine.Volume`, which the blocking volumes inherit; read locally from
//! the user's `Engine.u`). Every blocking volume the shipped level scripts
//! refer to is an `Engine.DynamicBlockingVolume` (24 of them in five
//! levels; 22 are targets of toggle actions and of nothing else, and
//! AG-StarHaven's `_2` and `_3` are the target of no action); such a
//! volume starts colliding or not by its `bEnabled`
//! (`asamu_world::scene::switchable_volume`). The blocking volumes in the
//! script's actor table get dynamic collision instances before play, and
//! the host's toggle sets their switch
//! ([`Game::set_actor_collision`]). The switch is also what
//! `SeqAct_ChangeCollision`, `SeqAct_Destroy` and the follow-collision
//! actors turn on a mover body: the instances keep what each blocks (a
//! mesh's triangles the bullet traces, its simple body the pawn).
//!
//! **Every target of an action.** An action acts on every object linked to
//! its `Target` variable link: `SequenceAction.Targets` is an array of
//! objects (CONFIRMED: the class model of the user's `Engine.u`), which the
//! engine fills with one element per linked variable before the action
//! runs. The converted graphs store no value for that property (it is empty
//! in the editor: 318 fed links in the shipped graphs, 65 of them with two
//! or more variables, none with a stored value), and the Kismet runtime
//! takes a property without a stored value for a single object, the first
//! linked one. So the host gives the property its declared type when it
//! binds a script (`declare_target_arrays`); without that only the first
//! target of each action is reached. Measured for one case: AG-BeautifulCity's
//! cutscene toggle has five blocking volumes linked, and the recorded pawn
//! walks inside the third of them (`DynamicBlockingVolume_5`, which starts
//! colliding) after that cutscene's end (KISMET_RUNTIME.md §8).
//!
//! **Console commands (P6).** `SeqAct_ConsoleCommand` hands each command
//! line to the player's controller. After the Kismet update the host gives
//! every such line to the pawn script layer ([`Game::console_command`]),
//! which runs the one it models: `SetSpeed F` (the stock cheat-manager
//! command, spelled `setspeed` and `SetSpeed` in the maps: `GroundSpeed` =
//! the class default 440 × `F`; AG-Workshop's `setspeed 0.3` gives the
//! recorded 132 uu/s). Lines are applied in emission order, but after the
//! update's host calls rather than between them; in the three shipped
//! graphs with a speed command the story-mode toggle that also writes
//! `GroundSpeed` comes before the command, so the result is the same.
//! `changesize F` (AG-Epilogue, 1.4) is the stock command too: the pawn's
//! collision cylinder becomes the class default × `F` and the pawn is
//! re-placed where it stands by the engine's placement search
//! ([`Game::change_size`]; not measured, AG-Epilogue is in no recording).
//! [`LevelScript::console_commands`] lists what was issued and what was
//! done with it; the app receives every line as an output as before.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_kismet::anim::AnimPosition;
use asamu_kismet::camera_anim::{CameraAnimSet, CameraAnims, GameplayCue};
use asamu_kismet::matinee::{
    Matrix, matrix_mul, matrix_rotator, rigid_inverse, rotation_translation_matrix,
};
use asamu_kismet::{
    ActorInfo, ActorRef, Host, LoadError, Output, PropertyValue, Runtime, ToggleMode,
};
use asamu_player::grapple_gun::{BasePose, carried_by_base};
use asamu_player::pawn::{LandingHandler, PowerJumpEvent, PowerJumpStateName};
use asamu_player::rocket_boots::BootsEvent;
use asamu_player::world::CollisionWorld;
use asamu_player::{InputFrame, PlayerParams, PlayerState, SimEvent, pawn};
use asamu_world::collision::{Affine, CollisionClass, InstanceInfo};
use asamu_world::scene::{self, LoadedMap, PlacedActor, PlacedPhysics, SceneError};
use asamu_world::{WorldEvent, rotation};
use glam::Vec3;
use thiserror::Error;

use crate::npc::{NpcEvent, NpcOptions, NpcSystem};
use crate::{Game, GameError, TickReport};

/// Distance of the downward sweep that finds the base mover, UU. Ours (the
/// original keeps the base its floor check found): longer than the greatest
/// height a pawn stands above its floor, 2.4 uu reported, which is at most
/// 2.4 + 2.8 = 5.2 uu above mesh triangles (NATIVE_PHYSICS.md 3.3, 10.2).
const BASE_PROBE: f32 = 8.0;

/// Scene kind of a blocking volume in the level script's actor table (the
/// importer's name for `Engine.BlockingVolume` and its subclasses).
const BLOCKING_VOLUME_KIND: &str = "blocking_volume";

/// Most console commands [`LevelScript::console_commands`] keeps.
const MAX_CONSOLE_LOG: usize = 256;

/// The property a sequence action receives its targets in, as the graph
/// keeps property names (lower case): `SequenceAction.Targets`.
const TARGETS_PROPERTY: &str = "targets";

/// Gives the `Targets` property of every node that is fed targets through a
/// variable link its declared type, an array, where the converted graph
/// stores no value for it (module docs, "Every target of an action").
/// Stored values are left alone.
fn declare_target_arrays(graph: &mut asamu_kismet::Graph) {
    for node in &mut graph.nodes {
        let fed = node
            .variables
            .iter()
            .any(|l| l.property.as_deref() == Some(TARGETS_PROPERTY));
        if !fed {
            continue;
        }
        let value = node.params.entry(TARGETS_PROPERTY.to_owned()).or_default();
        if matches!(value, asamu_kismet::KValue::None) {
            *value = asamu_kismet::KValue::Array(Vec::new());
        }
    }
}

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

    /// Puts the body's collision parts where the actor is now and records
    /// the actor's pose for what is based on it (grapple anchors).
    fn place(&self, game: &mut Game) {
        let affine = self.actor_affine(self.location, self.rotation);
        if let Some(sc) = &mut game.world.scene {
            for (i, rel) in &self.parts {
                if let Some(inst) = sc.dynamic.get_mut(*i) {
                    let info = inst.info;
                    sc.map.collision.place_dynamic(inst, rel.then(&affine));
                    inst.info = info;
                }
            }
            sc.set_actor_pose(self.actor, self.location + self.offset, self.rotation);
        }
    }
}

/// What a mover body is made from: the actor's placement.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BodySeed {
    location: Vec3,
    rotation: [i32; 3],
    draw_scale: f32,
    draw_scale3d: Vec3,
    pre_pivot: Vec3,
}

impl BodySeed {
    fn of_graph(info: &ActorInfo) -> Self {
        Self {
            location: Vec3::from_array(info.location),
            rotation: info.rotation,
            draw_scale: info.draw_scale,
            draw_scale3d: Vec3::from_array(info.draw_scale3d),
            pre_pivot: Vec3::from_array(info.pre_pivot),
        }
    }

    fn of_placed(a: &PlacedActor) -> Self {
        Self {
            location: a.location,
            rotation: a.rotation,
            draw_scale: a.draw_scale,
            draw_scale3d: a.draw_scale3d,
            pre_pivot: a.pre_pivot,
        }
    }
}

/// An actor the engine turns by itself (module docs, "Turning actors").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spinner {
    actor: u32,
    /// `RotationRate` (pitch, yaw, roll), rotator units per second.
    rate: [i32; 3],
}

/// An actor attached to an actor the host moves (the level's `Base`), with
/// how the original carries it (module docs, "Attached actors").
#[derive(Debug, Clone, Copy, PartialEq)]
struct Passenger {
    actor: u32,
    /// Carried through the transform it keeps relative to its base
    /// (`relative`): an actor in `PHYS_Interpolating`, or a hard-attached
    /// one that does not block actors. Otherwise it is carried by the
    /// base's change of transform.
    rigid: bool,
    /// `bIgnoreBaseRotation`: its own rotation stays; without `rigid` it
    /// only takes the base's translation.
    ignore_base_rotation: bool,
    /// The actor's transform relative to its base's, as the original keeps
    /// it for the rigid path.
    relative: Matrix,
}

impl Passenger {
    /// The passenger for the placed actor `a` on its base `b`.
    fn of(a: &PlacedActor, b: &PlacedActor) -> Self {
        let actor = rotation_translation_matrix(a.rotation, a.location.to_array());
        let base = rotation_translation_matrix(b.rotation, b.location.to_array());
        let relative = if a.hard_attach {
            // The whole transform relative to the base's.
            matrix_mul(&actor, &rigid_inverse(&base))
        } else {
            // Without a hard attachment the original keeps the offset from
            // the base in world axes and the rotation relative to the
            // base's.
            let turned = matrix_rotator(&matrix_mul(&actor, &rigid_inverse(&base)), false);
            let offset = a.location - b.location;
            rotation_translation_matrix(turned, offset.to_array())
        };
        Self {
            actor: a.id,
            rigid: a.physics == PlacedPhysics::Interpolating || (a.hard_attach && !a.block_actors),
            ignore_base_rotation: a.ignore_base_rotation,
            relative,
        }
    }

    /// Where the passenger, now at `current` (location, rotation), is after
    /// its base moved from `old` to `new`.
    fn carried(
        &self,
        current: (Vec3, [i32; 3]),
        old: (Vec3, [i32; 3]),
        new: (Vec3, [i32; 3]),
    ) -> (Vec3, [i32; 3]) {
        if self.rigid {
            let base = rotation_translation_matrix(new.1, new.0.to_array());
            let m = matrix_mul(&self.relative, &base);
            let [x, y, z, _] = m[3];
            let location = Vec3::new(x, y, z);
            if !location.is_finite() {
                return current;
            }
            let rotation = if self.ignore_base_rotation {
                current.1
            } else {
                matrix_rotator(&m, false)
            };
            return (location, rotation);
        }
        if self.ignore_base_rotation {
            return (current.0 + (new.0 - old.0), current.1);
        }
        let pose = |p: (Vec3, [i32; 3])| BasePose {
            location: p.0,
            rotation: p.1,
        };
        let location = carried_by_base(current.0, pose(old), pose(new));
        // Its own rotation takes the base's change, each member's low two
        // bits left out on both sides.
        let reduced = |i: usize| (new.1[i] & 0xFFFC).wrapping_sub(old.1[i] & 0xFFFC);
        let rotation = [
            current.1[0].wrapping_add(reduced(0)),
            current.1[1].wrapping_add(reduced(1)),
            current.1[2].wrapping_add(reduced(2)),
        ];
        (location, rotation)
    }
}

/// What the host moves by itself in `map`, among the actors that have a
/// mover body: the actors the engine turns, what is attached to each actor
/// of that set, and every such actor's placement (location with its
/// sub-level's offset, rotation).
#[allow(clippy::type_complexity)]
fn host_driven(
    map: &LoadedMap,
    movers: &BTreeMap<u32, MoverBody>,
) -> (
    Vec<Spinner>,
    BTreeMap<u32, Vec<Passenger>>,
    BTreeMap<u32, (Vec3, [i32; 3])>,
) {
    let turning = turning_actors(map);
    let mut spinners = Vec::new();
    let mut passengers: BTreeMap<u32, Vec<Passenger>> = BTreeMap::new();
    let mut placements = BTreeMap::new();
    for id in &turning {
        let (Some(a), Some(body)) = (map.placed_actor(*id), movers.get(id)) else {
            continue;
        };
        placements.insert(*id, (a.location + body.offset, a.rotation));
        if turns_by_itself(a) {
            spinners.push(Spinner {
                actor: *id,
                rate: a.rotation_rate,
            });
        }
        let base = a
            .base
            .filter(|b| turning.contains(b) && movers.contains_key(b))
            .and_then(|b| map.placed_actor(b));
        if let Some(b) = base {
            passengers
                .entry(b.id)
                .or_default()
                .push(Passenger::of(a, b));
        }
    }
    (spinners, passengers, placements)
}

/// The step the engine turns an actor by in one tick of `dt` seconds: each
/// member of its `RotationRate` times `dt`, the fraction dropped (towards
/// zero), so a rate of 9102 units/s is 151 units in a 1/60 s tick and 116
/// in a 12.82 ms frame (module docs, "Turning actors").
#[must_use]
pub fn rotation_step(rate: [i32; 3], dt: f32) -> [i32; 3] {
    rate.map(|r| {
        let step = r as f32 * dt;
        // Truncation; a product that is not a number is no step.
        if step.is_finite() { step as i32 } else { 0 }
    })
}

/// `true` for an actor the engine turns by itself: in `PHYS_Rotating` with
/// a `RotationRate` of which the low 16 bits of a member are not zero
/// (module docs, "Turning actors").
fn turns_by_itself(a: &PlacedActor) -> bool {
    a.physics == PlacedPhysics::Rotating && a.rotation_rate.iter().any(|r| r & 0xFFFF != 0)
}

/// The actors of `map` the engine turns by itself and everything attached
/// to them (through `Base`, to [`asamu_kismet::runtime::MAX_ATTACH_DEPTH`]
/// levels). Falling rocks move through their rock state and are left out.
fn turning_actors(map: &LoadedMap) -> BTreeSet<u32> {
    let rock = |id: u32| map.actors.rocks.iter().any(|r| r.id == id);
    let mut out: BTreeSet<u32> = map
        .placed
        .iter()
        .filter(|a| turns_by_itself(a) && !rock(a.id))
        .map(|a| a.id)
        .collect();
    for _ in 0..asamu_kismet::runtime::MAX_ATTACH_DEPTH {
        let more: Vec<u32> = map
            .placed
            .iter()
            .filter(|a| !out.contains(&a.id) && !rock(a.id))
            .filter(|a| a.base.is_some_and(|b| out.contains(&b)))
            .map(|a| a.id)
            .collect();
        if more.is_empty() {
            break;
        }
        out.extend(more);
    }
    out
}

/// Takes the actors the engine turns by itself out of the Matinee groups
/// they are bound to: a movement track moves an actor only in
/// `PHYS_Interpolating` (module docs, "Turning actors"). Returns how many
/// bindings were removed.
fn unbind_turning_actors(scripts: &mut asamu_kismet::LevelScripts, map: &LoadedMap) -> usize {
    let graph = &scripts.graph;
    let turning = |path: &str| {
        graph
            .actor_by_path(path)
            .and_then(|r| graph.actor(r))
            .and_then(|info| world_id(map, info))
            .and_then(|id| map.placed_actor(id))
            .is_some_and(turns_by_itself)
    };
    let mut removed = 0;
    for action in scripts.matinee.actions.values_mut() {
        for binding in &mut action.bindings {
            let before = binding.targets.len();
            binding
                .targets
                .retain(|t| !t.object.as_deref().is_some_and(turning));
            removed += before - binding.targets.len();
        }
    }
    removed
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
    /// The actors the engine turns by itself, by id.
    spinners: Vec<Spinner>,
    /// What is attached to each actor the host moves (base id → passengers).
    passengers: BTreeMap<u32, Vec<Passenger>>,
    /// Placement (location with the sub-level's offset, rotation) of every
    /// actor the host moves by itself: the turning actors and what they
    /// carry.
    host_placements: BTreeMap<u32, (Vec3, [i32; 3])>,
    /// The actors of `host_placements` that have moved.
    host_moved: BTreeSet<u32>,
    spawn_in_story: bool,
    host_errors: Vec<String>,
    /// `(SeqAct_Interp node, lower-case group name)` → the first bound
    /// actor's path (director cuts name the group whose actor becomes the
    /// view target).
    group_actors: BTreeMap<(usize, String), String>,
    /// The player camera's animations.
    camera: CameraAnims,
    /// Console commands the level script issued, with what the simulation
    /// did with each (capped).
    console: Vec<(String, crate::ConsoleCommand)>,
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
    let mut wanted: BTreeMap<u32, BodySeed> = BTreeMap::new();
    for r in kinetic_actors(scripts) {
        if let Some(info) = g.actor(r)
            && let Some(id) = world_id(map, info)
        {
            wanted.entry(id).or_insert_with(|| BodySeed::of_graph(info));
        }
    }
    // The actors the engine turns by itself and what is attached to them:
    // the level's script need not know them.
    for id in turning_actors(map) {
        if let Some(a) = map.placed_actor(id) {
            wanted.entry(id).or_insert_with(|| BodySeed::of_placed(a));
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
        .map(|(id, seed)| {
            let level = (id >> 16) as usize;
            let offset = map.levels.get(level).map_or(Vec3::ZERO, |l| l.offset);
            let body = MoverBody {
                actor: *id,
                location: seed.location,
                rotation: seed.rotation,
                draw_scale: seed.draw_scale,
                draw_scale3d: seed.draw_scale3d,
                pre_pivot: seed.pre_pivot,
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
        // The instance as it was placed: its kind, switch, flags and sweep
        // rule stay (a rebuilt one would be an enabled instance of the
        // default kind, whatever the level loader had decided).
        body.parts.push((map.dynamic.len(), relative));
        map.dynamic.push(inst);
    }
    bodies
}

/// The blocking volumes the level script knows (its actor table lists the
/// actors its nodes refer to): the targets of its toggle actions, whose
/// collision must be switchable during play (P5; module docs, "Blocking
/// volumes"). Volumes already taken as movers have no static collision left
/// and are switched through their body's instances.
fn prepare_switchable_volumes(map: &mut LoadedMap, scripts: &asamu_kismet::LevelScripts) {
    let ids: BTreeSet<u32> = scripts
        .graph
        .actors
        .iter()
        .filter(|a| a.kind == BLOCKING_VOLUME_KIND)
        .filter_map(|a| world_id(map, a))
        .collect();
    crate::converted::make_switchable(map, &ids);
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
    let mut scripts = match asamu_kismet::load_level_scripts(dir, &loaded.map, &subs) {
        Ok(s) => Some(s),
        Err(LoadError::Missing(_)) => None,
        Err(e) => return Err(e.into()),
    };
    let movers = scripts
        .as_ref()
        .map(|s| prepare_movers(&mut loaded, s))
        .unwrap_or_default();
    if let Some(s) = &mut scripts {
        // After the movers are chosen: what is attached to such an actor
        // stays a mover.
        unbind_turning_actors(s, &loaded);
    }
    if let Some(s) = &scripts {
        prepare_switchable_volumes(&mut loaded, s);
    }
    let levels = loaded.levels.clone();
    let level_names: Vec<String> = levels.iter().map(|l| l.name.clone()).collect();
    let npcs = options.npcs.then(|| {
        let npc_options = NpcOptions {
            time_trial: options.time_trial,
            ..NpcOptions::default()
        };
        let mut npcs = NpcSystem::load_from_dir(dir, &levels, npc_options);
        npcs.set_pawn_colliders(prepare_pawn_collision(&mut loaded, &npcs));
        npcs
    });
    let mut game =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)?;
    if let Some(npcs) = npcs {
        game.attach_npcs(npcs);
    }
    let script = scripts.map(|s| {
        let mut script = LevelScript::attach(&mut game, s, movers);
        script.runtime.set_time_trial(options.time_trial);
        let (anims, problems) = asamu_kismet::camera_anim::load_camera_anims(dir, &level_names);
        for p in problems {
            script.note(format!("camera animations: {p}"));
        }
        script.set_camera_anims(anims);
        script
    });
    Ok((game, script))
}

/// The NPC pawns' collision cylinders as dynamic instances of `map`
/// (blocking the player's moves, not zero-extent traces: the pawns'
/// block-all-but-weapons collision, NPCS.md §2); returns `(pawn actor id,
/// dynamic index)`.
fn prepare_pawn_collision(map: &mut LoadedMap, npcs: &NpcSystem) -> Vec<(u32, usize)> {
    let mut out = Vec::new();
    for (id, cyl) in npcs.runtime().pawn_cylinders(npcs.scene()) {
        let Some(mesh) = map.collision.add_cylinder_mesh(cyl.radius, cyl.half_height) else {
            continue;
        };
        let info = InstanceInfo {
            actor: Some(id),
            class: CollisionClass::StaticMesh,
            tag: asamu_world::SurfaceTag::None,
            grapple_able: false,
            blocks_pawn: true,
            blocks_traces: false,
            sublevel: u8::try_from(id >> 16).unwrap_or(u8::MAX),
        };
        if let Some(inst) = map.collision.dynamic_instance(
            mesh,
            Affine::from_translation(cyl.center.as_dvec3()),
            info,
        ) {
            out.push((id, map.dynamic.len()));
            map.dynamic.push(inst);
        }
    }
    out
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
        mut scripts: asamu_kismet::LevelScripts,
        movers: BTreeMap<u32, MoverBody>,
    ) -> LevelScript {
        declare_target_arrays(&mut scripts.graph);
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
                if sc.actor_location(m.actor).is_none() {
                    sc.set_actor_pose(m.actor, m.location + m.offset, m.rotation);
                }
            }
        }
        let (spinners, passengers, host_placements) = game
            .scene
            .as_ref()
            .map(|s| host_driven(&s.map, &movers))
            .unwrap_or_default();
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
            spinners,
            passengers,
            host_placements,
            host_moved: BTreeSet::new(),
            spawn_in_story: false,
            host_errors: Vec::new(),
            group_actors,
            camera: CameraAnims::default(),
            console: Vec::new(),
        }
    }

    /// Uses `set` for the player camera's animations (a fresh pool).
    pub fn set_camera_anims(&mut self, set: CameraAnimSet) {
        self.camera = CameraAnims::new(std::sync::Arc::new(set));
    }

    /// The player camera's animations (apply them to the point of view with
    /// `CameraAnims::apply`).
    #[must_use]
    pub fn camera_anims(&self) -> &CameraAnims {
        &self.camera
    }

    /// The values Matinee property tracks wrote, as `(world actor id,
    /// lower-case property name, value)` (lights' `Brightness`, `Radius`,
    /// `LightColor`; `DrawScale3D`).
    #[must_use]
    pub fn actor_properties(&self) -> Vec<(u32, String, PropertyValue)> {
        self.runtime
            .actor_properties()
            .filter_map(|(r, n, v)| Some((self.world_id(r)?, n.to_owned(), v)))
            .collect()
    }

    /// The gameplay script's camera-animation calls of one tick (see the
    /// module docs; `pj_before` is the power-jump actor before the tick).
    fn camera_cues(&mut self, game: &Game, report: &TickReport, pj_before: pawn::PowerJump) {
        let ev = &report.events;
        // `ASAMUPawn.ResetPlayer` (the death sequence's reset) calls the
        // boots' `ResetBoots` whenever the boots are enabled, whatever they
        // were doing (CONFIRMED (src)): the kept charge and boosting
        // instances are stopped.
        if report.respawned && game.player.script.boots.enabled {
            self.camera.cue(GameplayCue::BootsReset);
        }
        if ev.gun.released.is_some() {
            self.camera.cue(GameplayCue::GrappleReleased);
        }
        if ev.gun.attached.is_some() {
            self.camera.cue(GameplayCue::GrappleAttached);
        }
        // `ASAMUPowerJump`: the charge animation starts in `Charging`'s begin
        // code and stops in `Canceled`'s; the jump and leap bobs play with
        // the jump.
        let pj = game.player.script.power_jump;
        let charging = PowerJumpStateName::Charging;
        if pj.state == charging
            && !pj.begin_pending
            && (pj_before.state != charging || pj_before.begin_pending)
        {
            self.camera.cue(GameplayCue::PowerJumpChargeStarted);
        }
        let canceled = PowerJumpStateName::Canceled;
        let cancel_ran = (pj_before.state == canceled && pj.state != canceled)
            || (ev.power_jump == Some(PowerJumpEvent::Canceled) && pj.state != canceled);
        if cancel_ran {
            self.camera.cue(GameplayCue::PowerJumpCanceled);
        }
        if let Some(PowerJumpEvent::Fired { leap, .. }) = ev.power_jump {
            self.camera.cue(GameplayCue::PowerJumped { leap });
        }
        match ev.boots {
            Some(BootsEvent::Started) => self.camera.cue(GameplayCue::BootsChargeStarted),
            Some(BootsEvent::BoostBegan) => self.camera.cue(GameplayCue::BootsBoostBegan),
            Some(BootsEvent::Finished) => self.camera.cue(GameplayCue::BootsFinished),
            Some(BootsEvent::Canceled) => self.camera.cue(GameplayCue::BootsReset),
            _ => {}
        }
        if let Some(l) = &ev.landing
            && l.handler == LandingHandler::Normal
        {
            self.camera.cue(GameplayCue::Landed { hard: l.hard });
        }
        for e in game.npc_events() {
            if let NpcEvent::CameraShake { start, .. } = *e {
                self.camera.cue(GameplayCue::WormGrowl { start });
            }
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

    /// The placement of world actor `id` (location with the sub-level
    /// offset, rotation): where Matinee starts moving it from, or where the
    /// engine starts turning it. From the script's actor table, else from
    /// the level's own placement of an actor the engine turns by itself or
    /// of what is attached to one.
    #[must_use]
    pub fn actor_placement(&self, id: u32) -> Option<(Vec3, [i32; 3])> {
        let Some(r) = self.actor_refs(id).first().copied() else {
            // An actor the level's script does not know: one the engine
            // turns by itself, or one attached to it.
            return self.host_placements.get(&id).copied();
        };
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
    /// actors, cameras, lights driven by Matinee) and every actor the engine
    /// has turned by itself, with what it carries, as `(world actor id,
    /// location, rotation)`, for rendering; locations include the actor's
    /// sub-level offset. Actors outside the loaded levels are left out.
    /// Kismet's come first, in actor-table order; then the others, by id.
    #[must_use]
    pub fn moved_actors(&self) -> Vec<(u32, Vec3, [i32; 3])> {
        let mut out: Vec<(u32, Vec3, [i32; 3])> = self
            .runtime
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
            .collect();
        for id in &self.host_moved {
            let Some(m) = self.movers.get(id) else {
                continue;
            };
            // Where the host put it is where it is, whatever Kismet last
            // said of it.
            let pose = (*id, m.location + m.offset, m.rotation);
            match out.iter_mut().find(|e| e.0 == *id) {
                Some(e) => *e = pose,
                None => out.push(pose),
            }
        }
        out
    }

    /// World actor ids of the actors the engine turns by itself (module
    /// docs, "Turning actors"), with their `RotationRate` (pitch, yaw, roll;
    /// rotator units per second).
    #[must_use]
    pub fn turning_actors(&self) -> Vec<(u32, [i32; 3])> {
        self.spinners.iter().map(|s| (s.actor, s.rate)).collect()
    }

    /// One Kismet-driven tick (see the module docs). `None` (and nothing
    /// runs) when the game is not playing.
    pub fn tick(&mut self, game: &mut Game, input: &InputFrame) -> Option<ScriptedTick> {
        if game.state() != crate::GameState::Playing {
            return None;
        }
        let dt = game.clock.dt();
        // The NPC pawns' collision follows the pawns.
        if let (Some(npcs), Some(sc)) = (game.npcs.as_deref(), game.world.scene.as_mut()) {
            npcs.place_pawn_colliders(&sc.map.collision, &mut sc.dynamic);
        }
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
        // 2b. The actors the engine turns by itself, with what they carry.
        self.turn_actors(game, dt);
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
        // 4. The game tick, then the camera's animations of the frame.
        let was_story = game.in_story_mode();
        let pj_before = game.player.script.power_jump;
        let report = game.tick(input)?;
        self.camera_cues(game, &report, pj_before);
        self.camera.advance(dt);
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
                Output::ConsoleCommand { command } => {
                    // The action hands the line to the player's controller;
                    // two commands change the simulation (`SetSpeed`, P6,
                    // and `ChangeSize`). The rest is presentation or not
                    // modelled (module docs, "Console commands").
                    let done = game.console_command(command);
                    if self.console.len() < MAX_CONSOLE_LOG {
                        self.console.push((command.clone(), done));
                    }
                }
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
                Output::CameraAnim {
                    play,
                    anim: Some(anim),
                    looping,
                    rate,
                    scale,
                    blend_in,
                    blend_out,
                    random_start,
                } => {
                    if *play {
                        if !self.camera.kismet_play(
                            anim,
                            *rate,
                            *scale,
                            *blend_in,
                            *blend_out,
                            *looping,
                            *random_start,
                        ) && self.camera.set().get(anim).is_none()
                        {
                            self.note(format!("camera animation {anim} not converted"));
                        }
                    } else {
                        self.camera.kismet_stop(anim);
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

    /// The console commands the level script issued so far, in order, each
    /// with what the simulation did with it
    /// ([`crate::ConsoleCommand::NotHandled`]: nothing; the app still gets
    /// every command as an output). At most 256 are kept.
    #[must_use]
    pub fn console_commands(&self) -> &[(String, crate::ConsoleCommand)] {
        &self.console
    }

    /// One tick of the actors the engine turns by itself: each turns by
    /// [`rotation_step`] of its `RotationRate` about its location, and what
    /// is attached to it rides along.
    fn turn_actors(&mut self, game: &mut Game, dt: f32) {
        for i in 0..self.spinners.len() {
            let Some(spinner) = self.spinners.get(i).copied() else {
                break;
            };
            let Some(body) = self.movers.get(&spinner.actor) else {
                continue;
            };
            let step = rotation_step(spinner.rate, dt);
            if step == [0; 3] {
                continue;
            }
            let rotation = [
                body.rotation[0].wrapping_add(step[0]),
                body.rotation[1].wrapping_add(step[1]),
                body.rotation[2].wrapping_add(step[2]),
            ];
            let location = body.location;
            self.move_body(game, spinner.actor, location, rotation, 0);
        }
    }

    /// Puts the mover `id` at `location` and `rotation` (its collision and
    /// its pose record follow) and carries what is attached to it
    /// ([`Passenger`]), to [`asamu_kismet::runtime::MAX_ATTACH_DEPTH`]
    /// levels.
    fn move_body(
        &mut self,
        game: &mut Game,
        id: u32,
        location: Vec3,
        rotation: [i32; 3],
        depth: usize,
    ) {
        let Some(body) = self.movers.get_mut(&id) else {
            return;
        };
        let old = (body.location, body.rotation);
        body.location = location;
        body.rotation = rotation;
        body.place(game);
        self.host_moved.insert(id);
        if depth >= asamu_kismet::runtime::MAX_ATTACH_DEPTH || old == (location, rotation) {
            return;
        }
        let Some(passengers) = self.passengers.get(&id).cloned() else {
            return;
        };
        for p in passengers {
            let Some(current) = self.movers.get(&p.actor).map(|m| (m.location, m.rotation)) else {
                continue;
            };
            let (l, r) = p.carried(current, old, (location, rotation));
            self.move_body(game, p.actor, l, r, depth + 1);
        }
    }

    fn base_mover(&self, game: &Game) -> Option<u32> {
        if self.movers.is_empty() || !game.player.grounded {
            return None;
        }
        // The pawn's box, as its floor check sweeps it (P4): the original's
        // base is the actor that check hits. An upright cylinder touches a
        // floor that leans along both X and Y up to 21 × (|a| + |b| −
        // √(a² + b²)) uu lower than the box's corner does, further than this
        // probe reaches on a mover that leans enough.
        let extent = crate::converted::extent_of(game.pawn_cylinder());
        let p = game.player.position;
        let hit = game.world.sweep_box(p, p - Vec3::Z * BASE_PROBE, extent)?;
        let id = hit.hit.surface.actor?;
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
                NpcEvent::AnimNotify { actor, ref name } => {
                    for r in self.refs(actor) {
                        self.runtime.anim_notify(r, name);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Turns a mover body's collision on (blocking) or off: the switch of each
/// of its instances ([`asamu_world::collision::Instance::enabled`]).
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
            // The switch, not the flags: what each part blocks when the
            // actor collides (its triangles the bullet traces, its simple
            // body the pawn, ...) is the level loader's and stays.
            if let Some(inst) = sc.dynamic.get_mut(*i) {
                inst.enabled = blocks;
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

    /// A toggle action on a blocking volume (module docs, "Blocking
    /// volumes"): "turn on" makes it collide, "turn off" stops it, "toggle"
    /// flips it. `false` when the actor has no collision to switch.
    fn toggle_blocking_volume(&mut self, id: u32, mode: ToggleMode) -> bool {
        let Some(current) = self.game.actor_collision(id) else {
            return false;
        };
        let enabled = match mode {
            ToggleMode::On => true,
            ToggleMode::Off => false,
            ToggleMode::Toggle => !current,
        };
        self.game.set_actor_collision(id, enabled)
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
        if actor.kind == BLOCKING_VOLUME_KIND {
            self.toggle_blocking_volume(id, mode);
        } else if !self.set_trigger_enabled(id, mode) {
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
            body.place(self.game);
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
        // The action sets the pawn's location (CONFIRMED (src): the stock
        // teleport handler), which runs the engine's placement search: the
        // destination itself when the pawn's box is free there, else a
        // place over the floor below, else the teleport fails and nothing
        // changes (NATIVE_PHYSICS.md 10.3). A teleport also drops the base.
        let extent = crate::converted::extent_of(self.game.pawn_cylinder());
        let Some(spot) = crate::converted::find_spot(&self.game.world, p, extent) else {
            return false;
        };
        self.game.player.position = spot;
        if let Some(r) = rot {
            self.game.player.yaw = rotation::units_to_radians(r[1]);
            self.game.player.pitch = rotation::units_to_radians(rotation::normalize_axis(r[0]));
        }
        self.game.player.pawn.based = false;
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

    fn anim_sequence_length(&self, actor: &ActorInfo, sequence: &str) -> Option<f32> {
        let id = self.id(actor)?;
        self.game.npcs()?.anim_sequence_length(id, sequence)
    }

    fn set_anim_position(&mut self, actor: &ActorInfo, call: &AnimPosition) -> Vec<String> {
        let Some(id) = self.id(actor) else {
            return Vec::new();
        };
        self.game
            .npcs_mut()
            .and_then(|n| {
                n.set_anim_position(
                    id,
                    &call.sequence,
                    call.position,
                    call.fire_notifies,
                    call.looping,
                )
            })
            .unwrap_or_default()
    }

    fn set_skel_control_strength(&mut self, actor: &ActorInfo, control: &str, strength: f32) {
        if let Some(id) = self.id(actor)
            && let Some(n) = self.game.npcs_mut()
        {
            n.set_skel_control_strength(id, control, strength);
        }
    }

    fn set_look_at(
        &mut self,
        actor: &ActorInfo,
        target: Option<&ActorInfo>,
        head_offset: [f32; 3],
        eyes_offset: [f32; 3],
    ) {
        let target = target.and_then(|t| self.id(t));
        if let Some(id) = self.id(actor)
            && let Some(n) = self.game.npcs_mut()
        {
            n.set_look_at(
                id,
                target,
                Vec3::from_array(head_offset),
                Vec3::from_array(eyes_offset),
            );
        }
    }

    fn set_actor_property(&mut self, actor: &ActorInfo, name: &str, value: PropertyValue) {
        // `DrawScale3D` rescales the actor and so its collision; the other
        // properties the shipped tracks drive (light brightness, radius and
        // colour) are presentation (the app reads them from the runtime).
        if !name.eq_ignore_ascii_case("DrawScale3D") {
            return;
        }
        let (Some(id), PropertyValue::Vector(v)) = (self.id(actor), value) else {
            return;
        };
        let scale = Vec3::from_array(v);
        if !scale.is_finite() {
            return;
        }
        let Some(body) = self.movers.get_mut(&id) else {
            return;
        };
        body.draw_scale3d = scale;
        body.place(self.game);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_kismet::{Graph, MatineeSet};
    use asamu_player::world::{ActorClass, CollisionShape};
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
        lift_map_tilted(0.0)
    }

    /// [`lift_map`] with the platform sheared so that its top rises by
    /// `gradient` uu per uu along X and along Y: the top is the plane
    /// z = 50 + 400 × gradient + gradient × (x + y). The player starts over
    /// the origin, its box 2.15 uu above where it touches that plane (the
    /// box touches with its corner at (21, 21): 21 × 2 × gradient above the
    /// plane's height under its centre).
    fn lift_map_tilted(gradient: f64) -> (LoadedMap, asamu_kismet::LevelScripts) {
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
            rows: [
                DVec3::X * 400.0 + DVec3::Z * (400.0 * gradient),
                DVec3::Y * 400.0 + DVec3::Z * (400.0 * gradient),
                DVec3::Z * 50.0,
            ],
            translation: DVec3::new(-200.0, -200.0, 0.0),
        };
        b.add_static(cube, platform, info(Some(5), CollisionClass::InterpActor))
            .unwrap();
        // 50 + 44 + 2.15 over a level top (the sum as it always was here),
        // plus the top's rise to the origin and to the box's corner.
        let start_z = 50.0_f32 + 46.15 + (442.0 * gradient) as f32;
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
            placed: Vec::new(),
            actors: SceneActors {
                player_starts: vec![PlayerStartDef {
                    id: 1,
                    name: "PlayerStart_0".into(),
                    location: Vec3::new(0.0, 0.0, start_z),
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

    /// The base under a standing pawn is what its **box** stands on (the
    /// original's base is the actor its floor check hits, and that check
    /// sweeps the box, P4). On a mover whose top leans along both X and Y the
    /// box rests on its corner, well above where an upright cylinder of the
    /// same radius would touch, so a cylinder probe of a few uu misses the
    /// mover.
    #[test]
    fn a_mover_whose_top_leans_both_ways_carries_the_player() {
        // Gradient 0.4 along X and along Y: the floor's normal is
        // (−0.4, −0.4, 1)/√1.32, z = 0.870, walkable (the limit is 0.7).
        let (mut map, s) = lift_map_tilted(0.4);
        let movers = prepare_movers(&mut map, &s);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let mut script = LevelScript::attach(&mut game, s, movers);
        game.start();
        // The top under the origin is at 50 + 160 = 210; the box touches it
        // with its corner at (21, 21), where the top is 0.4 × 42 = 16.8
        // higher: the centre at 210 + 16.8 + 44 = 270.8 at contact, and the
        // start is 2.15 uu above that.
        let contact = 270.8_f32;
        let start_z = game.player().position.z;
        assert!((start_z - (contact + 2.15)).abs() < 1e-4, "{start_z}");
        // An upright cylinder of radius 21 would touch 21 × √0.32 = 11.879
        // above the top under its centre: its centre at 265.879, 4.921 uu
        // lower than the box's. From where the pawn stands, at least 2.15 uu
        // above the box's contact, a cylinder is more than 7 uu from the
        // mover; after the first floor check (which reports the triangles
        // 2.8 uu nearer than they are, at distance 0, and raises the pawn by
        // 2.15) more than 9.
        for _ in 0..90 {
            script.tick(&mut game, &InputFrame::default()).unwrap();
        }
        let lift = script.mover_location(5).unwrap();
        assert!((lift.z - 200.0).abs() < 1e-3, "lift at {lift:?}");
        let p = game.player().position;
        assert!(game.player().grounded, "still standing on the lift");
        assert_eq!((p.x, p.y), (0.0, 0.0), "a standing pawn does not slide");
        // Carried by the lift's 200 uu. Above the lifted contact it stands
        // where the floor checks leave it: 4.30 uu after the first one (the
        // start's 2.15 plus the check's 2.15), and at most the standing
        // height on mesh triangles, 2.15 + 2.8 = 4.95 uu, if a later check
        // (a moved base forces one) raises it once more.
        let above = p.z - (contact + 200.0);
        assert!(
            (4.30 - 1e-3..=4.95 + 1e-3).contains(&above),
            "{above} uu above the lifted mover (carried by {})",
            p.z - start_z
        );
        let extent = crate::converted::extent_of(game.pawn_cylinder());
        assert!(!game.world().overlaps_box(p, extent), "inside the mover");
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

    /// A skinned actor (slot 9) animated by a Matinee animation-control
    /// track, its look-at control driven by a strength track, and a light
    /// (slot 10) whose brightness a float property track drives. The
    /// sequence's Kismet notify at 1 s fires the actor's
    /// `SeqEvent_AnimNotify` (→ grapple limit 6), its sound notify at 2 s
    /// becomes an NPC event.
    fn anim_game() -> (Game, LevelScript) {
        use crate::npc::defs::{SkinnedComponent, SkinnedDrive};
        use crate::npc::{NpcScene, SkinnedActorDef};
        use asamu_world::anim::{NotifyDef, NotifyKind, SequenceInfo};
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5, 6, 7]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {{"id": 2, "class": "Engine.SeqAct_Interp", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "Play"}}, {{"desc": "Reverse"}}, {{"desc": "Stop"}}, {{"desc": "Pause"}}, {{"desc": "Change Dir"}}],
                  "outputs": [{{"desc": "Completed"}}, {{"desc": "Reversed"}}],
                  "variables": [{{"desc": "Data", "vars": [3]}}, {{"desc": "Villager", "vars": [4]}}, {{"desc": "Lamp", "vars": [7]}}],
                  "auto_activate_outputs": true, "latent": true, "latent_base": true}},
                {{"id": 3, "path": "T.Data", "class": "Engine.InterpData", "kind": "variable", "parent": 0}},
                {{"id": 4, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
                  "var": {{"value": {{"$obj": "T.TheWorld.PersistentLevel.Villager"}}}}}},
                {{"id": 5, "class": "Engine.SeqEvent_AnimNotify", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Out", "links": [{{"op": 6, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 0, "originator": "T.TheWorld.PersistentLevel.Villager"}},
                  "params": {{"NotifyName": "Talk_Standing_1"}}}},
                {},
                {{"id": 7, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
                  "var": {{"value": {{"$obj": "T.TheWorld.PersistentLevel.Lamp"}}}}}}
            ]"#,
            action(
                6,
                "asamu.SeqAct_SetMaxGrapples",
                &["In"],
                None,
                r#"{"Grapples": 6}"#
            ),
        );
        let g = graph(
            &nodes,
            r#"[{"path": "T.TheWorld.PersistentLevel.Villager", "name": "Villager",
                 "class": "Engine.SkeletalMeshActorMAT", "kind": "other", "package": "T", "slot": 9},
                {"path": "T.TheWorld.PersistentLevel.Lamp", "name": "Lamp",
                 "class": "Engine.PointLightMovable", "kind": "light", "package": "T", "slot": 10}]"#,
        );
        let pt = |t: f32, v: f32| {
            format!(
                r#"{{"in": {t:?}, "out": {v:?}, "arrive": 0.0, "leave": 0.0, "mode": "linear"}}"#
            )
        };
        let doc = format!(
            r#"{{"format": "asamu-matinee", "version": 1, "package": "T",
                "actions": [{{"path": "T.Interp", "node": 2, "scope": "level", "interp_data": "T.Data",
                              "bindings": [
                                {{"link": 1, "label": "Villager", "group": "Villager",
                                  "targets": [{{"variable": "T.V", "object": "T.TheWorld.PersistentLevel.Villager"}}]}},
                                {{"link": 2, "label": "Lamp", "group": "Lamp",
                                  "targets": [{{"variable": "T.L", "object": "T.TheWorld.PersistentLevel.Lamp"}}]}}]}}],
                "interp_data": [{{"path": "T.Data", "length": 3.0, "groups": [
                    {{"kind": "group", "name": "Villager", "tracks": [
                      {{"class": "Engine.InterpTrackAnimControl", "data": {{"type": "anim_control", "slot": "FullBody",
                        "keys": [{{"start_time": 0.0, "sequence": "Talk", "start_offset": 0.0, "end_offset": 0.0,
                                   "play_rate": 1.0, "looping": false, "reverse": false}}]}}}},
                      {{"class": "Engine.InterpTrackSkelControlStrength", "data": {{"type": "skel_control_strength",
                        "name": "LookAtController", "curve": {{"points": [{}]}}}}}}]}},
                    {{"kind": "group", "name": "Lamp", "tracks": [
                      {{"class": "Engine.InterpTrackFloatProp", "data": {{"type": "float_property",
                        "name": "Brightness", "curve": {{"points": [{}, {}]}}}}}},
                      {{"class": "Engine.InterpTrackColorProp", "data": {{"type": "color_property",
                        "name": "LightColor", "curve": {{"points": []}}}}}}]}}]}}]}}"#,
            pt(0.0, 0.5),
            pt(0.0, 0.0),
            pt(2.0, 4.0)
        );
        let mut m = MatineeSet::default();
        m.add_json(doc.as_bytes(), 0).unwrap();
        let (mut map, _) = lift_map();
        let s = scripts(g, m);
        let movers = prepare_movers(&mut map, &s);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let mut scene = NpcScene {
            skinned: vec![SkinnedActorDef {
                id: 9,
                name: "Villager".into(),
                class: "Engine.SkeletalMeshActorMAT".into(),
                hidden: false,
                drive: SkinnedDrive::Matinee,
                components: vec![SkinnedComponent {
                    name: "SkeletalMeshComponent_0".into(),
                    mesh: "Villagers.Meshes.V".into(),
                    local_to_world: [[0.0; 4]; 4],
                    hidden: false,
                    animation: None,
                }],
            }],
            ..NpcScene::default()
        };
        scene
            .anims
            .entry("villagers.meshes.v".into())
            .or_default()
            .insert(
                "talk".into(),
                SequenceInfo {
                    length: 5.0,
                    rate_scale: 1.0,
                    notifies: vec![
                        NotifyDef {
                            time: 1.0,
                            duration: 0.0,
                            kind: NotifyKind::Kismet {
                                name: "Talk_Standing_1".into(),
                            },
                        },
                        NotifyDef {
                            time: 2.0,
                            duration: 0.0,
                            kind: NotifyKind::Sound {
                                cue: "Cue.Talk".into(),
                                follow_actor: true,
                                bone: None,
                                volume: 1.0,
                                pitch: 1.0,
                                percent_to_play: 1.0,
                                ignore_if_hidden: false,
                            },
                        },
                    ],
                },
            );
        game.attach_npcs(NpcSystem::new(scene, NpcOptions::default()));
        let script = LevelScript::attach(&mut game, s, movers);
        game.start();
        (game, script)
    }

    #[test]
    fn matinee_animation_tracks_reach_skinned_actors_lights_and_kismet() {
        let (mut game, mut script) = anim_game();
        let mut limit_at = None;
        let mut sound_at = None;
        for i in 1..=240u32 {
            let t = script.tick(&mut game, &InputFrame::default()).unwrap();
            if limit_at.is_none() && game.player().script.gun.max_grapples == 6 {
                limit_at = Some(i);
            }
            if t.npc_events.iter().any(
                |e| matches!(e, NpcEvent::AnimSound { actor: 9, cue, .. } if cue == "Cue.Talk"),
            ) {
                sound_at.get_or_insert(i);
            }
        }
        // The Matinee starts in the first update; its position crosses 1 s
        // in update 61 and the notify's event runs in that same update.
        assert_eq!(limit_at, Some(61), "{limit_at:?}");
        // The sound notify at 2 s is reported with the NPC events of the game
        // tick after the crossing update.
        assert_eq!(sound_at, Some(121), "{sound_at:?}");
        let state = &game.npcs().unwrap().runtime().skinned[0];
        let node = state.current().unwrap();
        assert_eq!(node.sequence.as_deref(), Some("Talk"));
        assert!((node.position - 3.0).abs() < 0.02, "{}", node.position);
        assert_eq!(state.controls.get("lookatcontroller"), Some(&0.5));
        // The lamp's brightness followed its curve (0 → 4 over 2 s, held);
        // the colour track has no keys and wrote nothing.
        let props = script.actor_properties();
        assert_eq!(
            props,
            vec![(10, "brightness".to_owned(), PropertyValue::Float(4.0))]
        );
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
        assert!(
            script.host_errors().is_empty(),
            "{:?}",
            script.host_errors()
        );
    }

    #[test]
    fn gameplay_events_and_kismet_play_camera_animations() {
        use asamu_kismet::camera_anim::{CameraAnim, Pov, paths};
        use asamu_kismet::matinee::{CurveMode, CurvePoint, InterpCurve, MoveFrame, MoveTrack};
        let (mut game, mut script) = npc_game("[]");
        let mut set = CameraAnimSet::default();
        let anim = |path: &str, z: f32| CameraAnim {
            path: path.into(),
            length: 1.0,
            base_fov: 90.0,
            move_track: Some(MoveTrack {
                pos: InterpCurve {
                    points: vec![
                        CurvePoint {
                            in_val: 0.0,
                            out_val: [0.0; 3],
                            arrive: [0.0; 3],
                            leave: [0.0; 3],
                            mode: CurveMode::Linear,
                        },
                        CurvePoint {
                            in_val: 1.0,
                            out_val: [0.0, 0.0, z],
                            arrive: [0.0; 3],
                            leave: [0.0; 3],
                            mode: CurveMode::Linear,
                        },
                    ],
                    ..InterpCurve::default()
                },
                // A move track needs rotation keys to move anything
                // (`GetLocationAtTime`).
                euler: InterpCurve {
                    points: vec![CurvePoint {
                        in_val: 0.0,
                        out_val: [0.0; 3],
                        arrive: [0.0; 3],
                        leave: [0.0; 3],
                        mode: CurveMode::Linear,
                    }],
                    ..InterpCurve::default()
                },
                move_frame: MoveFrame::RelativeToInitial,
                ..MoveTrack::default()
            }),
            fov_track: None,
        };
        set.insert(anim(paths::HARD_LAND, -60.0));
        set.insert(anim(paths::NORMAL_LAND, -6.0));
        set.insert(anim(paths::WORM_GROWL, 3.0));
        set.insert(anim("Map.Kismet.Anim", 12.0));
        script.set_camera_anims(set);
        let mut report = script
            .tick(&mut game, &InputFrame::default())
            .unwrap()
            .report;
        // A hard landing of the normal handler plays `HardLanding`.
        report.events.landing = Some(asamu_player::LandingOutcome {
            handler: LandingHandler::Normal,
            velocity_z: -2500.0,
            hard: true,
            sound: true,
            cue: None,
            sprint_applied: false,
            grapples_refilled: true,
            boost_canceled: false,
        });
        let pj = game.player.script.power_jump;
        script.camera_cues(&game, &report, pj);
        script.camera.advance(0.5);
        let playing: Vec<&str> = script
            .camera_anims()
            .player()
            .active()
            .into_iter()
            .filter_map(|h| script.camera_anims().player().playing(h))
            .collect();
        assert_eq!(playing, vec![paths::HARD_LAND]);
        let pov = Pov {
            location: [0.0, 0.0, 100.0],
            rotation: [0; 3],
            fov: 90.0,
        };
        let out = script.camera_anims().apply(pov);
        assert!((out.location[2] - 70.0).abs() < 1e-3, "{:?}", out.location);
        // A story-mode landing plays nothing.
        report.events.landing = report.events.landing.map(|mut l| {
            l.handler = LandingHandler::Story;
            l
        });
        script.camera_cues(&game, &report, pj);
        assert_eq!(script.camera_anims().player().active().len(), 1);
        // Kismet's `SeqAct_PlayCameraAnim` plays and stops through the
        // update's outputs.
        let play = |play: bool| Output::CameraAnim {
            play,
            anim: Some("Map.Kismet.Anim".into()),
            looping: true,
            rate: 1.0,
            scale: 1.0,
            blend_in: 0.0,
            blend_out: 0.0,
            random_start: false,
        };
        script.apply_game_outputs(&mut game, &[play(true)]);
        assert_eq!(script.camera_anims().player().active().len(), 2);
        script.apply_game_outputs(&mut game, &[play(false)]);
        script.camera.advance(0.01);
        assert!(
            script
                .camera_anims()
                .player()
                .active()
                .iter()
                .all(|h| script.camera_anims().player().playing(*h) != Some("Map.Kismet.Anim"))
        );
        // A missing animation is reported once per call, not played.
        script.apply_game_outputs(
            &mut game,
            &[Output::CameraAnim {
                play: true,
                anim: Some("Missing.Anim".into()),
                looping: false,
                rate: 1.0,
                scale: 1.0,
                blend_in: 0.0,
                blend_out: 0.0,
                random_start: false,
            }],
        );
        assert_eq!(script.host_errors().len(), 1);
    }

    #[test]
    fn npc_pawn_cylinders_block_the_player() {
        use crate::npc::defs::{PawnCollisionDef, WormLight, WormParams};
        use crate::npc::{NpcScene, WormDef};
        let (mut map, _) = lift_map();
        let scene = NpcScene {
            worms: vec![WormDef {
                id: 7,
                name: "Worm".into(),
                location: Vec3::new(150.0, 0.0, 150.0),
                rotation: [0; 3],
                look_targets: Vec::new(),
                light: WormLight::default(),
                params: WormParams::ORIGINAL,
                meshes: Vec::new(),
            }],
            pawn_collision: vec![PawnCollisionDef {
                id: 7,
                radius: 40.0,
                half_height: 100.0,
            }],
            ..NpcScene::default()
        };
        let mut npcs = NpcSystem::new(scene, NpcOptions::default());
        let colliders = prepare_pawn_collision(&mut map, &npcs);
        assert_eq!(colliders, vec![(7, 0)]);
        npcs.set_pawn_colliders(colliders);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        game.attach_npcs(npcs);
        let shape = CollisionShape {
            radius: 21.0,
            half_height: 44.0,
        };
        let start = Vec3::new(0.0, 0.0, 120.0);
        let hit = game
            .world
            .sweep_capsule(start, start + Vec3::X * 300.0, shape)
            .unwrap();
        assert_eq!(hit.surface.actor, Some(7));
        assert!(
            (hit.distance - (150.0 - 61.0)).abs() < 1e-2,
            "{}",
            hit.distance
        );
        // Traces (the grapple) pass through.
        assert!(
            game.world
                .raycast(start, Vec3::X, 300.0)
                .is_none_or(|h| h.surface.actor != Some(7))
        );
        // The colliders follow the pawns every scripted tick (the worm stays).
        let mut script =
            LevelScript::new(&mut game, scripts(graph("[]", "[]"), MatineeSet::default()));
        game.start();
        script.tick(&mut game, &InputFrame::default()).unwrap();
        let sc = game.world.scene.as_ref().unwrap();
        assert_eq!(
            sc.dynamic[0].to_world.translation,
            DVec3::new(150.0, 0.0, 150.0)
        );
    }

    /// The lift map (no Kismet, so the platform stays put) with or without
    /// an NPC pawn (id 7) whose 40 × 100 cylinder stands on the platform at
    /// `pawn_at`.
    fn pawn_obstacle_game(pawn_at: Option<Vec3>) -> (Game, LevelScript) {
        use crate::npc::defs::{PawnCollisionDef, WormLight, WormParams};
        use crate::npc::{NpcScene, WormDef};
        let (mut map, _) = lift_map();
        let scene = match pawn_at {
            Some(location) => NpcScene {
                worms: vec![WormDef {
                    id: 7,
                    name: "Worm".into(),
                    location,
                    rotation: [0; 3],
                    look_targets: Vec::new(),
                    light: WormLight::default(),
                    params: WormParams::ORIGINAL,
                    meshes: Vec::new(),
                }],
                pawn_collision: vec![PawnCollisionDef {
                    id: 7,
                    radius: 40.0,
                    half_height: 100.0,
                }],
                ..NpcScene::default()
            },
            None => NpcScene::default(),
        };
        let mut npcs = NpcSystem::new(scene, NpcOptions::default());
        npcs.set_pawn_colliders(prepare_pawn_collision(&mut map, &npcs));
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        game.attach_npcs(npcs);
        let script = LevelScript::new(&mut game, scripts(graph("[]", "[]"), MatineeSet::default()));
        game.start();
        (game, script)
    }

    /// The player's own movement (not just a sweep query) is stopped by an
    /// NPC pawn's cylinder: walking straight at it ends where the two
    /// cylinders touch, walking at it off-centre slides around it without
    /// ever entering, and without the pawn the same input walks through
    /// the spot.
    #[test]
    fn the_player_cannot_walk_through_an_npc_pawn() {
        let pawn = Vec3::new(150.0, 0.0, 150.0);
        let reach = 40.0 + 21.0; // pawn radius + player radius
        let forward = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        let axis_distance = |g: &Game| {
            let p = g.player().position;
            ((p.x - pawn.x).powi(2) + (p.y - pawn.y).powi(2)).sqrt()
        };
        // Straight at it.
        let (mut game, mut script) = pawn_obstacle_game(Some(pawn));
        let mut closest = f32::MAX;
        for _ in 0..180 {
            script.tick(&mut game, &forward).unwrap();
            closest = closest.min(axis_distance(&game));
        }
        let x = game.player().position.x;
        assert!(
            closest >= reach - 0.05,
            "entered the pawn's cylinder: {closest} < {reach}"
        );
        assert!(
            (x - (pawn.x - reach)).abs() < 1.0,
            "stopped at x = {x}, the cylinders touch at {}",
            pawn.x - reach
        );
        assert!(game.player().velocity.x.abs() < 1.0, "still moving");
        // Off-centre: the player slides around and gets past, never inside.
        let (mut game, mut script) = pawn_obstacle_game(Some(pawn + Vec3::Y * 30.0));
        let pawn_y = pawn + Vec3::Y * 30.0;
        let mut closest = f32::MAX;
        let mut passed = false;
        for _ in 0..240 {
            script.tick(&mut game, &forward).unwrap();
            let p = game.player().position;
            closest = closest.min(((p.x - pawn_y.x).powi(2) + (p.y - pawn_y.y).powi(2)).sqrt());
            passed |= p.x > pawn_y.x;
        }
        assert!(closest >= reach - 0.05, "{closest}");
        assert!(passed, "slid around the pawn: {:?}", game.player().position);
        // Without the pawn the same walk goes straight through the spot.
        let (mut game, mut script) = pawn_obstacle_game(None);
        for _ in 0..180 {
            script.tick(&mut game, &forward).unwrap();
        }
        assert!(game.player().position.x > pawn.x, "{:?}", game.player());
    }

    /// The death reset stops the rocket-boots camera animations when the
    /// boots are enabled (`ResetPlayer` → `ResetBoots`), and only then.
    #[test]
    fn the_death_reset_stops_the_boots_camera_animations() {
        use asamu_kismet::camera_anim::{CameraAnim, paths};
        let (mut game, mut script) = npc_game("[]");
        let mut set = CameraAnimSet::default();
        for p in [paths::BOOTS_CHARGE, paths::BOOTS_BOOSTING] {
            set.insert(CameraAnim {
                path: p.into(),
                length: 100.0,
                base_fov: 90.0,
                move_track: None,
                fov_track: None,
            });
        }
        script.set_camera_anims(set);
        let mut report = script
            .tick(&mut game, &InputFrame::default())
            .unwrap()
            .report;
        let pj = game.player.script.power_jump;
        let boost = |script: &mut LevelScript| {
            script.camera.cue(GameplayCue::BootsChargeStarted);
            script.camera.cue(GameplayCue::BootsBoostBegan);
            script.camera.advance(0.1);
            assert_eq!(script.camera_anims().player().active().len(), 2);
        };
        // Boots not enabled: the reset is not called.
        game.enable_rocket_boots(false);
        boost(&mut script);
        report.respawned = true;
        script.camera_cues(&game, &report, pj);
        script.camera.advance(0.1);
        assert_eq!(script.camera_anims().player().active().len(), 2);
        // Enabled: both kept instances stop (no blend-out time: at once).
        game.enable_rocket_boots(true);
        script.camera_cues(&game, &report, pj);
        script.camera.advance(0.1);
        assert!(script.camera_anims().player().active().is_empty());
        // No respawn, no reset.
        boost(&mut script);
        report.respawned = false;
        script.camera_cues(&game, &report, pj);
        script.camera.advance(0.1);
        assert_eq!(script.camera_anims().player().active().len(), 2);
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

    /// Gated on converted data (`levels`, `meshes --collision`, `kismet`,
    /// `matinee`, `skeletal`): the gameplay camera animations and the maps'
    /// own are loaded, BeautifulCity's talking villagers fire their
    /// `SeqEvent_AnimNotify` events from their own animations, Matinee
    /// animation tracks pose their actors, and Darkcave's worm pawn blocks
    /// the player with its placed cylinder.
    #[test]
    fn converted_camera_anims_notifies_look_at_and_pawn_collision() {
        use asamu_kismet::camera_anim::paths;
        let Some(dir) = converted_dir() else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted Kismet not set");
            return;
        };
        // Camera animations.
        if dir.join("matinee").join("camera_anims.json").is_file()
            && let Ok((_, Some(script))) = load_level_with_kismet(&dir, "AG-Darkcave")
        {
            let set = script.camera_anims().set();
            for p in [
                paths::GRAPPLE_BEGIN,
                paths::GRAPPLE_LOOP,
                paths::NORMAL_LAND,
                paths::HARD_LAND,
                paths::POWER_JUMP_CHARGE,
                paths::POWER_JUMP_BOB,
                paths::POWER_LEAP_BOB,
                paths::BOOTS_CHARGE,
                paths::BOOTS_BOOSTING,
                paths::WORM_GROWL,
            ] {
                assert!(set.get(p).is_some(), "{p}");
            }
            // Every animation a `SeqAct_PlayCameraAnim` of the map names.
            let g = script.runtime().graph();
            let named: Vec<String> = g
                .nodes
                .iter()
                .filter(|n| n.class == asamu_kismet::OpClass::PlayCameraAnim)
                .filter_map(|n| n.param("CameraAnim").and_then(|v| v.as_obj()))
                .map(str::to_owned)
                .collect();
            assert!(!named.is_empty());
            for p in &named {
                assert!(set.get(p).is_some(), "{p}");
            }
            assert!(
                script.host_errors().is_empty(),
                "{:?}",
                script.host_errors()
            );
        } else {
            eprintln!("SKIP: no converted camera animations (run `asamu-import matinee`)");
        }
        // Animation notifies and Matinee animation on BeautifulCity.
        if dir.join("skeletal").join("manifest.json").is_file()
            && dir.join("matinee").join("anim_notifies.json").is_file()
            && let Ok((mut game, Some(mut script))) =
                load_level_with_kismet(&dir, "AG-BeautifulCity")
        {
            game.start();
            let mut notifies = 0usize;
            let mut sounds = 0usize;
            for _ in 0..1500 {
                let t = script.tick(&mut game, &InputFrame::default()).unwrap();
                for e in &t.npc_events {
                    match e {
                        NpcEvent::AnimNotify { .. } => notifies += 1,
                        NpcEvent::AnimSound { .. } => sounds += 1,
                        _ => {}
                    }
                }
            }
            assert!(notifies > 0, "ambient animations fire Kismet notifies");
            let g = script.runtime().graph();
            let fired = g
                .nodes
                .iter()
                .filter(|n| n.class == asamu_kismet::OpClass::AnimNotify)
                .filter(|n| script.runtime().activate_count(n.id) > 0)
                .count();
            let total = g
                .nodes
                .iter()
                .filter(|n| n.class == asamu_kismet::OpClass::AnimNotify)
                .count();
            println!(
                "AG-BeautifulCity: {notifies} anim notifies, {sounds} anim sounds in 25 s; \
                 {fired} of {total} SeqEvent_AnimNotify fired"
            );
            assert!(fired >= total / 2, "{fired} of {total}");
            let npcs = game.npcs().unwrap();
            let ambient = npcs
                .runtime()
                .skinned
                .iter()
                .filter(|s| s.ambient.as_ref().is_some_and(|n| n.playing))
                .count();
            let look = npcs.scene().look_at.len();
            println!("AG-BeautifulCity: {ambient} ambient nodes playing, {look} look-at setups");
            assert!(ambient >= 40, "{ambient}");
            assert!(look >= 3, "{look}");
            assert!(
                script.runtime().errors().is_empty(),
                "{:?}",
                script.runtime().errors()
            );
            assert!(
                script.host_errors().is_empty(),
                "{:?}",
                script.host_errors()
            );
        } else {
            eprintln!("SKIP: no converted skeletal data or anim notifies");
        }
        // The worm pawn's collision in Darkcave.
        if let Ok((game, _)) = load_level_with_kismet(&dir, "AG-Darkcave") {
            let npcs = game.npcs().unwrap();
            assert_eq!(npcs.pawn_colliders().len(), 1);
            let worm = &npcs.scene().worms[0];
            let c = npcs.scene().pawn_collision[0];
            assert_eq!((c.radius, c.half_height), (200.0, 500.0));
            let shape = CollisionShape {
                radius: 21.0,
                half_height: 44.0,
            };
            // From above the worm's cylinder straight down onto its top.
            let top = worm.location + Vec3::Z * (c.half_height + 300.0);
            let hit = game
                .world
                .sweep_capsule(top, worm.location, shape)
                .expect("the worm blocks");
            assert_eq!(hit.surface.actor, Some(worm.id));
            assert!(
                (hit.position.z - (worm.location.z + 544.0)).abs() < 0.1,
                "{hit:?}"
            );
        }
    }

    /// Gated on converted data: on AG-Darkcave the *moving* player is kept
    /// out of the worm pawn's placed cylinder (radius 200 + the player's
    /// 21), from every direction it can be reached from, while the same
    /// flight without the NPCs passes straight through that space; the
    /// worm's cylinder does not stop zero-extent traces (the grapple).
    #[test]
    fn converted_worm_blocks_the_moving_player() {
        let Some(dir) = converted_dir() else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted Kismet not set");
            return;
        };
        let Ok((game, Some(_))) = load_level_with_kismet(&dir, "AG-Darkcave") else {
            eprintln!("SKIP: AG-Darkcave not converted");
            return;
        };
        let (worm, cyl) = {
            let npcs = game.npcs().unwrap();
            (
                npcs.scene().worms[0].location,
                npcs.scene().pawn_collision[0],
            )
        };
        let reach = cyl.radius + 21.0;
        // Fly at the worm's axis from `from` (at the worm's own height) for
        // half a second; the closest the player gets to the axis, or `None`
        // when the level's own geometry is in the way first.
        let fly = |npcs: bool, dir_xy: Vec3| -> Option<f32> {
            let options = LevelOptions {
                npcs,
                time_trial: false,
            };
            let (mut game, script) =
                load_level_with_kismet_options(&dir, "AG-Darkcave", options).ok()?;
            let mut script = script?;
            game.start();
            let start = worm - dir_xy * (reach + 150.0);
            // The start must be free of the level's geometry.
            let shape = CollisionShape {
                radius: 21.0,
                half_height: 44.0,
            };
            if game.world.overlaps(start, shape) {
                return None;
            }
            game.player.position = start;
            game.player.velocity = dir_xy * 1500.0;
            let mut closest = f32::MAX;
            for _ in 0..30 {
                script.tick(&mut game, &InputFrame::default())?;
                let p = game.player.position;
                if !p.is_finite() {
                    return None;
                }
                closest = closest.min(((p.x - worm.x).powi(2) + (p.y - worm.y).powi(2)).sqrt());
                // Keep flying level: this checks the side of the cylinder.
                game.player.position.z = start.z;
                game.player.velocity.z = 0.0;
            }
            Some(closest)
        };
        let mut blocked = 0usize;
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            let dir_xy = Vec3::new(a.cos(), a.sin(), 0.0);
            let (Some(with), Some(without)) = (fly(true, dir_xy), fly(false, dir_xy)) else {
                continue;
            };
            assert!(
                with >= reach - 0.1,
                "direction {k}: the player got {with} from the worm's axis (cylinders touch at {reach})"
            );
            // Only directions the level leaves open show the pawn at work.
            if without < reach - 10.0 {
                assert!(
                    with < reach + 2.0,
                    "direction {k}: stopped at {with}, before the worm ({reach})"
                );
                blocked += 1;
            }
        }
        println!(
            "AG-Darkcave: the worm's {} x {} cylinder stopped the flying player from {blocked} of 8 directions",
            cyl.radius, cyl.half_height
        );
        assert!(blocked >= 1, "no free approach to the worm was found");
        // Zero-extent traces pass through the pawn.
        let from = worm - Vec3::X * (reach + 150.0);
        assert!(
            game.world
                .raycast(from, Vec3::X, reach + 150.0)
                .is_none_or(|h| h.surface.actor != Some(game.npcs().unwrap().scene().worms[0].id))
        );
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

    /// P6, the Workshop's graph shape: the level-loaded event links the
    /// story-mode toggle and a console command `setspeed 0.3`, which links a
    /// second console command (AG-Epilogue's `changesize 1.4`, spelled
    /// differently here: commands are matched without regard to case).
    #[test]
    fn console_commands_of_the_level_script_set_the_speed_and_the_size() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let command = |id: usize, commands: &str, next: &str| {
            format!(
                r#"{{"id": {id}, "class": "Engine.SeqAct_ConsoleCommand", "kind": "action", "parent": 0,
                    "inputs": [{{"desc": "In"}}], "outputs": [{{"desc": "Out", "links": [{next}]}}],
                    "variables": [{{"desc": "Target", "property": "Targets", "vars": [5]}}],
                    "params": {{"Commands": {commands}, "Targets": []}},
                    "auto_activate_outputs": true}}"#
            )
        };
        let nodes = format!(
            r#"[
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible",
                               "links": [{{"op": 2, "input": 0}}, {{"op": 3, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {}, {}, {},
                {{"id": 5, "class": "Engine.SeqVar_Player", "kind": "variable", "parent": 0}}
            ]"#,
            action(
                2,
                "asamu.SeqAct_ToggleStoryMode",
                &["Enable", "Disable", "Toggle"],
                None,
                "{}"
            ),
            command(3, r#"["setspeed 0.3"]"#, r#"{"op": 4, "input": 0}"#),
            command(4, r#"["ChangeSize 1.4", "NormalMode"]"#, ""),
        );
        let g = graph(&nodes, "[]");
        let mut script = LevelScript::new(&mut game, scripts(g, MatineeSet::default()));
        let floor_z = game.player().position.z - 44.0 - asamu_player::world::CONTACT_SKIN;
        let t = script.tick(&mut game, &InputFrame::default()).unwrap();
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
        // Story mode (which writes `MoveSpeed × 0.6` = 264), then the
        // command: the pawn class's default `GroundSpeed` 440 × 0.3 = 132,
        // the speed of all 3,001 recorded Workshop frames.
        assert!(game.in_story_mode());
        assert_eq!(game.player().script.ground_speed, 132.0);
        // The size: the class default cylinder 21 × 44 times 1.4. It is
        // state of the game; the parameter set is untouched.
        let m = game.pawn_cylinder();
        assert!((m.radius - 29.4).abs() < 1e-4);
        assert!((m.half_height - 61.6).abs() < 1e-4);
        assert_eq!(*game.params(), PlayerParams::asamu_original());
        assert_eq!(
            script.console_commands(),
            &[
                (
                    "setspeed 0.3".to_owned(),
                    crate::ConsoleCommand::SetSpeed {
                        factor: 0.3,
                        ground_speed: 132.0
                    }
                ),
                (
                    "ChangeSize 1.4".to_owned(),
                    crate::ConsoleCommand::ChangeSize {
                        factor: 1.4,
                        radius: m.radius,
                        half_height: m.half_height,
                        placed: true
                    }
                ),
                ("NormalMode".to_owned(), crate::ConsoleCommand::NotHandled),
            ]
        );
        // The app still receives every line.
        for line in ["setspeed 0.3", "ChangeSize 1.4", "NormalMode"] {
            assert!(t.outputs.contains(&Output::ConsoleCommand {
                command: line.into()
            }));
        }
        // The grown pawn was re-placed by the placement search (bottom 3.0 uu
        // above the floor, see `change_size_replaces_the_pawn`), its base
        // dropped; the tick's walking floor check then found the floor as a
        // new base, reported 2.0 uu away, and set the 2.15: a true 3.15.
        let z = game.player().position.z;
        assert!((z - (floor_z + 61.6 + 3.15)).abs() < 1e-3, "{z}");
        assert!(game.player().grounded);
        // Walking forward reaches 132 uu/s and stays there.
        let forward = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        for _ in 0..30 {
            script.tick(&mut game, &forward).unwrap();
        }
        let speed = game.player().horizontal_speed();
        assert!((speed - 132.0).abs() < 0.01, "{speed}");
        // The other writers still write (A-WK-3): leaving story mode gives
        // `MoveSpeed` 440.
        game.exit_story_mode();
        assert_eq!(game.player().script.ground_speed, 440.0);
    }

    /// `ChangeSize F` (the stock command; AG-Epilogue runs 1.4): the
    /// cylinder becomes the class default × F and the pawn is re-placed
    /// where it stands by the placement search.
    #[test]
    fn change_size_replaces_the_pawn() {
        let mut game = Game::graybox().unwrap();
        game.start();
        let start = game.player().position;
        let floor_z = start.z - 44.0 - asamu_player::world::CONTACT_SKIN;
        // The grown box (half height 61.6) reaches 17.55 uu into the floor.
        // The search traces a 1 uu slice down 2 × 61.6 = 123.2 uu from the
        // centre: its bottom is 43.05 uu above the floor, reported 1.0 uu
        // short (a tenth of the trace, at most 1.0 on a BSP-kind floor):
        // 42.05. New centre: z − (2 × 42.05/123.2 − 1) × 61.6 + 1 =
        // z + 20.55: the bottom 3.0 uu above the floor.
        let done = game.console_command("changesize 1.4");
        assert!(matches!(
            done,
            crate::ConsoleCommand::ChangeSize { placed: true, .. }
        ));
        let p = game.player().position;
        assert_eq!((p.x, p.y), (start.x, start.y));
        assert!((p.z - (floor_z + 61.6 + 3.0)).abs() < 1e-3, "{}", p.z);
        assert!(!game.player().pawn.based && game.player().pawn.force_floor_check);
        // Back to the class default: nothing encroaches, the pawn is left
        // where it is (now 20.6 uu above the floor).
        let done = game.console_command("ChangeSize 1");
        assert_eq!(
            done,
            crate::ConsoleCommand::ChangeSize {
                factor: 1.0,
                radius: 21.0,
                half_height: 44.0,
                placed: true
            }
        );
        assert_eq!(game.player().position, p);
        // Guards of ours, and commands that are not modelled.
        for line in [
            "changesize",
            "changesize 0",
            "changesize -1",
            "changesize nan",
            "changesize 1 2",
        ] {
            assert_eq!(
                game.console_command(line),
                crate::ConsoleCommand::NotHandled,
                "{line}"
            );
        }
        assert_eq!(game.pawn_cylinder().half_height, 44.0);
        assert_eq!(*game.params(), PlayerParams::asamu_original());
        // Without the pawn script layer the commands do nothing.
        let mut placeholder = Game::graybox_placeholder().unwrap();
        assert_eq!(
            placeholder.console_command("changesize 1.4"),
            crate::ConsoleCommand::NotHandled
        );
        assert_eq!(
            placeholder.console_command("setspeed 0.3"),
            crate::ConsoleCommand::NotHandled
        );
    }

    /// A floor and a 100 × 400 × 400 blocking volume (actor 7, its face at
    /// x = 250) that starts switched off, as a `DynamicBlockingVolume` with
    /// `bEnabled` false does; the level script toggles it.
    fn volume_map() -> (LoadedMap, asamu_kismet::LevelScripts) {
        volumes_map(1)
    }

    /// [`volume_map`] with `count` volumes, all the targets of the one
    /// toggle action: actors 7, 8, ... (`DynamicBlockingVolume_0`, `_1`,
    /// ...), each 1000 uu further along Y than the one before. Only the
    /// first starts switched off. The action has no stored `Targets`
    /// value, as in the converted graphs.
    fn volumes_map(count: u32) -> (LoadedMap, asamu_kismet::LevelScripts) {
        use asamu_world::collision::{InstanceOptions, PrimitiveKind};
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
        let floor = Affine {
            rows: [DVec3::X * 4000.0, DVec3::Y * 4000.0, DVec3::Z * 100.0],
            translation: DVec3::new(-2000.0, -2000.0, -100.0),
        };
        b.add_static(
            cube,
            floor,
            InstanceInfo {
                actor: None,
                class: CollisionClass::WorldGeometry,
                tag: SurfaceTag::None,
                grapple_able: true,
                blocks_pawn: true,
                blocks_traces: true,
                sublevel: 0,
            },
        )
        .unwrap();
        let hull = b.add_box_mesh(Vec3::new(50.0, 200.0, 200.0)).unwrap();
        for n in 0..count {
            b.add_static_with(
                hull,
                Affine::from_translation(DVec3::new(300.0, 1000.0 * f64::from(n), 200.0)),
                InstanceInfo {
                    actor: Some(7 + n),
                    class: CollisionClass::BlockingVolume,
                    tag: SurfaceTag::None,
                    grapple_able: true,
                    blocks_pawn: true,
                    blocks_traces: false,
                    sublevel: 0,
                },
                InstanceOptions {
                    kind: Some(PrimitiveKind::BrushHull),
                    enabled: n > 0,
                },
            )
            .unwrap();
        }
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
            placed: Vec::new(),
            actors: SceneActors {
                player_starts: vec![PlayerStartDef {
                    id: 1,
                    name: "PlayerStart_0".into(),
                    location: Vec3::new(0.0, 0.0, 100.0),
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
        // A toggle whose targets are the volumes, one object variable each
        // on its `Target` link; nothing fires it by itself.
        let members: Vec<String> = (1..=count + 1).map(|i| i.to_string()).collect();
        let vars: Vec<String> = (0..count).map(|n| (n + 2).to_string()).collect();
        let mut nodes = vec![
            format!(
                r#"{{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [{}]}}"#,
                members.join(", ")
            ),
            format!(
                r#"{{"id": 1, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
                    "inputs": [{{"desc": "Turn On"}}, {{"desc": "Turn Off"}}, {{"desc": "Toggle"}}],
                    "outputs": [{{"desc": "Out"}}],
                    "variables": [{{"desc": "Target", "property": "Targets", "vars": [{}]}}],
                    "auto_activate_outputs": true}}"#,
                vars.join(", ")
            ),
        ];
        let mut actors = Vec::new();
        for n in 0..count {
            nodes.push(format!(
                r#"{{"id": {}, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
                    "var": {{"value": {{"$obj": "T.TheWorld.PersistentLevel.DynamicBlockingVolume_{n}"}}}}}}"#,
                n + 2
            ));
            actors.push(format!(
                r#"{{"path": "T.TheWorld.PersistentLevel.DynamicBlockingVolume_{n}",
                    "name": "DynamicBlockingVolume_{n}", "class": "Engine.DynamicBlockingVolume",
                    "kind": "blocking_volume", "package": "T", "slot": {}}}"#,
                7 + n
            ));
        }
        let g = graph(
            &format!("[{}]", nodes.join(", ")),
            &format!("[{}]", actors.join(", ")),
        );
        (map, scripts(g, MatineeSet::default()))
    }

    /// P5: a toggle action on a blocking volume switches its collision.
    /// Input 0 turns it on, input 1 off, input 2 flips it (the volume
    /// classes' toggle handler).
    #[test]
    fn toggle_actions_switch_a_blocking_volume() {
        let (mut map, s) = volume_map();
        let movers = prepare_movers(&mut map, &s);
        assert!(movers.is_empty(), "the volume is not moved by anything");
        let placed = map.collision.statics()[1].clone();
        prepare_switchable_volumes(&mut map, &s);
        // The volume's hull left the static set unchanged: still a brush,
        // still switched off.
        assert_eq!(map.collision.statics().len(), 1);
        assert_eq!(map.dynamic, vec![placed]);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        game.start();
        let mut script = LevelScript::attach(&mut game, s, movers);
        let half = Vec3::new(21.0, 21.0, 44.0);
        let (from, to) = (Vec3::new(100.0, 0.0, 100.0), Vec3::new(400.0, 0.0, 100.0));
        let blocked = |game: &Game| game.world().sweep_box(from, to, half);
        assert_eq!(game.actor_collision(7), Some(false));
        assert!(blocked(&game).is_none());
        // "Turn On".
        script.runtime_mut().force_input(1, 0);
        script.tick(&mut game, &InputFrame::default()).unwrap();
        assert_eq!(game.actor_collision(7), Some(true));
        // The box stops at the face x = 250 with its centre at 229: 129 uu
        // into the 300 uu trace.
        let hit = blocked(&game).expect("blocked");
        assert_eq!(hit.kind, asamu_player::world::PrimitiveKind::BrushHull);
        assert!((hit.hit.time * 300.0 - 129.0).abs() < 1e-3, "{hit:?}");
        // "Turn On" again changes nothing; "Turn Off"; "Toggle" twice.
        for (input, expected) in [(0, true), (1, false), (1, false), (2, true), (2, false)] {
            script.runtime_mut().force_input(1, input);
            script.tick(&mut game, &InputFrame::default()).unwrap();
            assert_eq!(game.actor_collision(7), Some(expected), "input {input}");
            assert_eq!(blocked(&game).is_some(), expected, "input {input}");
        }
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
    }

    /// P5 on a toggle with several targets, as the shipped graphs have them
    /// (AG-BeautifulCity's cutscene toggle has five volumes on its `Target`
    /// link and no stored `Targets` value): the action acts on every linked
    /// volume, not on the first alone. `SequenceAction.Targets` is an array
    /// of objects with one element per linked variable.
    #[test]
    fn a_toggle_switches_every_blocking_volume_linked_to_it() {
        let (mut map, s) = volumes_map(3);
        // The converted form: no value stored for the action's targets.
        assert!(!s.graph.nodes[1].params.contains_key("targets"));
        let movers = prepare_movers(&mut map, &s);
        prepare_switchable_volumes(&mut map, &s);
        assert_eq!(map.collision.statics().len(), 1, "the floor");
        assert_eq!(map.dynamic.len(), 3);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        game.start();
        let mut script = LevelScript::attach(&mut game, s, movers);
        let half = Vec3::new(21.0, 21.0, 44.0);
        // A sweep through each volume (they are 1000 uu apart along Y).
        let blocked = |game: &Game, n: u32| {
            let y = 1000.0 * n as f32;
            game.world()
                .sweep_box(Vec3::new(100.0, y, 100.0), Vec3::new(400.0, y, 100.0), half)
                .is_some()
        };
        let state = |game: &Game| [7, 8, 9].map(|id| game.actor_collision(id));
        // As placed: the first off, the others on (`bEnabled`).
        assert_eq!(state(&game), [Some(false), Some(true), Some(true)]);
        // Input 0 turns on, 1 off, 2 flips: every volume, each time.
        for (input, expected) in [
            (0, [true, true, true]),
            (1, [false, false, false]),
            (1, [false, false, false]),
            (2, [true, true, true]),
            (2, [false, false, false]),
            (0, [true, true, true]),
        ] {
            script.runtime_mut().force_input(1, input);
            let tick = script.tick(&mut game, &InputFrame::default()).unwrap();
            assert_eq!(state(&game), expected.map(Some), "input {input}");
            for n in 0..3 {
                assert_eq!(
                    blocked(&game, n),
                    expected[n as usize],
                    "input {input}, volume {n}"
                );
            }
            // One toggle output per target, in the link's order.
            let toggled: Vec<&str> = tick
                .outputs
                .iter()
                .filter_map(|o| match o {
                    Output::ActorToggled { actor, .. } => Some(actor.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                toggled,
                [
                    "T.TheWorld.PersistentLevel.DynamicBlockingVolume_0",
                    "T.TheWorld.PersistentLevel.DynamicBlockingVolume_1",
                    "T.TheWorld.PersistentLevel.DynamicBlockingVolume_2",
                ],
                "input {input}"
            );
        }
        // A "Toggle" flips each volume from its own state: with the first
        // switched off by hand, the three end up on, off, off.
        assert!(game.set_actor_collision(7, false));
        script.runtime_mut().force_input(1, 2);
        script.tick(&mut game, &InputFrame::default()).unwrap();
        assert_eq!(state(&game), [Some(true), Some(false), Some(false)]);
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
    }

    /// A stored `Targets` value is left as it is, and a node without a
    /// `Targets` link gets none.
    #[test]
    fn target_arrays_are_declared_only_where_they_are_missing() {
        let nodes = r#"[
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4]},
            {"id": 1, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
             "variables": [{"desc": "Target", "property": "Targets", "vars": [4]}]},
            {"id": 2, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
             "variables": [{"desc": "Target", "property": "Targets", "vars": [4]}],
             "params": {"Targets": [{"$obj": null}]}},
            {"id": 3, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
             "variables": [{"desc": "Bool", "vars": []}]},
            {"id": 4, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0}
        ]"#;
        let mut g = graph(nodes, "[]");
        let stored = g.nodes[2].params.clone();
        declare_target_arrays(&mut g);
        assert_eq!(
            g.nodes[1].params.get("targets"),
            Some(&asamu_kismet::KValue::Array(Vec::new()))
        );
        assert_eq!(g.nodes[2].params, stored);
        assert!(g.nodes[3].params.is_empty() && g.nodes[4].params.is_empty());
    }

    /// Instances taken out of the static set for a moving actor stay as the
    /// level loader placed them (kind, switch, flags, sweep rule), and
    /// turning a mover body's collision off and on again changes only the
    /// switch.
    #[test]
    fn mover_bodies_keep_their_instances_as_placed() {
        let (mut map, s) = lift_map();
        // The lift's platform as a level loader may place it: switched off.
        assert_eq!(map.collision.set_actor_enabled(5, false), 1);
        let placed: Vec<_> = map
            .collision
            .statics()
            .iter()
            .filter(|i| i.info.actor.is_some())
            .cloned()
            .collect();
        assert_eq!(placed.len(), 2);
        let mut movers = prepare_movers(&mut map, &s);
        assert_eq!(map.dynamic, placed);
        assert!(!map.dynamic[0].enabled && map.dynamic[1].enabled);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let flags = |game: &Game, i: usize| {
            let inst = &game.world.scene.as_ref().unwrap().dynamic[i];
            (inst.info, inst.kind, inst.blocks_lines, inst.enabled)
        };
        let (info, kind, lines, _) = flags(&game, 1);
        set_body_collision(&mut game, &mut movers, 6, false);
        assert_eq!(flags(&game, 1), (info, kind, lines, false));
        set_body_collision(&mut game, &mut movers, 6, true);
        assert_eq!(flags(&game, 1), (info, kind, lines, true));
        // The other body was not touched.
        assert!(!flags(&game, 0).3);
    }

    // --- Turning actors and what is attached to them.

    /// A hand-made converted map with a wheel: a 400×400×50 platform (actor
    /// 5, "Hub") at the origin, which the level places in the rotating
    /// physics with a `RotationRate` of `rate` units/s of yaw, and a 50 uu
    /// rock (actor 6, "Rock") hard-attached to it, 300 to 350 uu out along
    /// the hub's X axis and 100 to 150 uu up (its actor location is (300, 0,
    /// 100)). With `wall`, a slab of world geometry stands at x = 420 to 440
    /// on the +Y side (y from 60). The level's script is empty: it knows
    /// neither actor. The player starts on the platform at (100, 0).
    fn wheel_map(rate: i32, wall: bool) -> (LoadedMap, asamu_kismet::LevelScripts) {
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
        let slab = |min: DVec3, max: DVec3| Affine {
            rows: [
                DVec3::X * (max.x - min.x),
                DVec3::Y * (max.y - min.y),
                DVec3::Z * (max.z - min.z),
            ],
            translation: min,
        };
        b.add_static(
            cube,
            slab(
                DVec3::new(-4000.0, -4000.0, -1100.0),
                DVec3::new(4000.0, 4000.0, -1000.0),
            ),
            info(None, CollisionClass::WorldGeometry),
        )
        .unwrap();
        b.add_static(
            cube,
            slab(
                DVec3::new(-200.0, -200.0, 0.0),
                DVec3::new(200.0, 200.0, 50.0),
            ),
            info(Some(5), CollisionClass::InterpActor),
        )
        .unwrap();
        b.add_static(
            cube,
            slab(
                DVec3::new(300.0, -25.0, 100.0),
                DVec3::new(350.0, 25.0, 150.0),
            ),
            info(Some(6), CollisionClass::InterpActor),
        )
        .unwrap();
        if wall {
            b.add_static(
                cube,
                slab(
                    DVec3::new(420.0, 60.0, -100.0),
                    DVec3::new(440.0, 1200.0, 600.0),
                ),
                info(None, CollisionClass::WorldGeometry),
            )
            .unwrap();
        }
        let placed = |id: u32, name: &str, location: Vec3| PlacedActor {
            id,
            name: name.into(),
            kind: "interp_actor".into(),
            location,
            rotation: [0; 3],
            draw_scale: 1.0,
            draw_scale3d: Vec3::ONE,
            pre_pivot: Vec3::ZERO,
            physics: PlacedPhysics::Interpolating,
            rotation_rate: [0; 3],
            base: None,
            hard_attach: false,
            ignore_base_rotation: false,
            block_actors: true,
        };
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
            placed: vec![
                PlacedActor {
                    physics: PlacedPhysics::Rotating,
                    rotation_rate: [0, rate, 0],
                    ..placed(5, "Hub", Vec3::ZERO)
                },
                PlacedActor {
                    base: Some(5),
                    hard_attach: true,
                    ..placed(6, "Rock", Vec3::new(300.0, 0.0, 100.0))
                },
            ],
            actors: SceneActors {
                player_starts: vec![PlayerStartDef {
                    id: 1,
                    name: "PlayerStart_0".into(),
                    location: Vec3::new(100.0, 0.0, 50.0 + 46.15),
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
        let g = graph(
            r#"[{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": []}]"#,
            "[]",
        );
        (map, scripts(g, MatineeSet::default()))
    }

    fn wheel_game(rate: i32, wall: bool) -> (Game, LevelScript) {
        let (mut map, s) = wheel_map(rate, wall);
        let movers = prepare_movers(&mut map, &s);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let script = LevelScript::attach(&mut game, s, movers);
        game.start();
        (game, script)
    }

    /// The point at `radius` from the Z axis, `units` of yaw from the X
    /// axis, at height `z`: where a yaw of `units` takes (radius, 0, z).
    fn at_yaw(radius: f64, units: i32, z: f64) -> Vec3 {
        let angle = f64::from(units) * std::f64::consts::TAU / 65_536.0;
        Vec3::new(
            (radius * angle.cos()) as f32,
            (radius * angle.sin()) as f32,
            z as f32,
        )
    }

    #[test]
    fn the_step_of_a_turn_is_the_rate_times_the_tick_truncated() {
        let tick = 1.0_f32 / 60.0;
        // 9102 / 60 = 151.7, 4551 / 60 = 75.85, 2184 / 60 = 36.4.
        assert_eq!(rotation_step([0, -9102, 0], tick), [0, -151, 0]);
        assert_eq!(rotation_step([9102, 4551, -2184], tick), [151, 75, -36]);
        // The recorded frames of 12.8206 ms: 9102 × 0.0128206 = 116.69 and
        // 5461 × 0.0128206 = 70.01, the steps the recorded rocks turn by.
        assert_eq!(rotation_step([9102, 5461, 0], 0.012_820_6), [116, 70, 0]);
        // A product below one unit is no turn in that tick; nothing is
        // carried over to the next.
        assert_eq!(rotation_step([59, -59, 60], tick), [0, 0, 1]);
        assert_eq!(rotation_step([1, 2, 3], f32::NAN), [0; 3]);
        assert_eq!(rotation_step([1, 2, 3], 0.0), [0; 3]);
    }

    #[test]
    fn the_engine_turns_an_actor_in_the_rotating_physics_and_what_is_attached() {
        // 6030 units/s: 100.5 units in a tick of 1/60 s, so 100.
        let (mut game, mut script) = wheel_game(6030, false);
        assert_eq!(script.mover_ids(), vec![5, 6]);
        assert_eq!(script.turning_actors(), vec![(5, [0, 6030, 0])]);
        assert!(script.moved_actors().is_empty(), "nothing has moved yet");
        const TICKS: i32 = 64;
        for _ in 0..TICKS {
            script.tick(&mut game, &InputFrame::default()).unwrap();
        }
        // 64 ticks of 100 units: 6400 units of yaw, about its own location.
        let yaw = TICKS * 100;
        assert_eq!(script.mover_transform(5), Some((Vec3::ZERO, [0, yaw, 0])));
        // The rock is hard-attached: it keeps its transform relative to the
        // hub, (300, 0, 100) without a turn of its own.
        let (rock, rock_rotation) = script.mover_transform(6).unwrap();
        let expected = at_yaw(300.0, yaw, 100.0);
        assert!((rock - expected).length() < 1e-2, "{rock} vs {expected}");
        assert_eq!(rock_rotation, [0, yaw, 0]);
        // What draws the level is told, with where each started.
        assert_eq!(
            script.moved_actors(),
            vec![(5, Vec3::ZERO, [0, yaw, 0]), (6, rock, rock_rotation)]
        );
        assert_eq!(script.actor_placement(5), Some((Vec3::ZERO, [0; 3])));
        assert_eq!(
            script.actor_placement(6),
            Some((Vec3::new(300.0, 0.0, 100.0), [0; 3]))
        );
        assert_eq!(script.actor_placement(7), None);
        // The collision turned with them: straight down onto where the
        // rock's centre (325 uu out) is now, its top is at 150.
        let sc = game.world().scene.as_ref().unwrap();
        let centre = at_yaw(325.0, yaw, 0.0);
        let down = |x: f32, y: f32| {
            sc.map
                .collision
                .raycast(
                    &sc.dynamic,
                    Vec3::new(x, y, 1000.0),
                    Vec3::new(x, y, -2000.0),
                    QueryFilter::TRACE,
                )
                .map(|h| 1000.0 - 3000.0 * h.t)
        };
        let top = down(centre.x, centre.y).unwrap();
        assert!((top - 150.0).abs() < 1e-3, "{top}");
        // ... and it is gone from where it was placed (y from −25 to 25 at
        // x = 325): the platform does not reach there, the floor is below.
        let was = down(325.0, -20.0).unwrap();
        assert!((was + 1000.0).abs() < 1e-3, "{was}");
        // The poses a grapple anchor rides.
        assert_eq!(sc.actor_location(5), Some(Vec3::ZERO));
        assert_eq!(sc.actor_rotation(5), Some([0, yaw, 0]));
        assert_eq!(sc.actor_location(6), Some(rock));
        assert_eq!(sc.actor_rotation(6), Some(rock_rotation));
        // The pawn stood at (100, 0) on the platform and is carried around
        // the hub's axis and turned with it (the basing rule of the module
        // docs: position through the mover's change of transform, yaw by
        // its change of yaw).
        let p = game.player();
        assert!(p.grounded, "still standing on the hub");
        let carried = at_yaw(100.0, yaw, f64::from(p.position.z));
        assert!(
            (p.position - carried).length() < 0.5,
            "the pawn is at {}, the hub took its place to {carried}",
            p.position
        );
        let turned = f64::from(yaw) * std::f64::consts::TAU / 65_536.0;
        assert!((f64::from(p.yaw) - turned).abs() < 1e-3, "{}", p.yaw);
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
    }

    /// Puts the pawn in the air outside the wheel, `out` uu from the hub's
    /// axis on the ray at `units` of yaw, its eye at the rock's mid height
    /// (125), looking at the axis.
    fn look_at_the_wheel(game: &mut Game, out: f64, units: i32) {
        let eye = at_yaw(out, units, 125.0);
        let eye_offset = game.player().view_location(game.params()) - game.player().position;
        let p = game.player_mut();
        p.position = eye - eye_offset;
        p.velocity = Vec3::ZERO;
        p.grounded = false;
        p.pawn.based = false;
        p.yaw = rotation::units_to_radians(units + 32_768);
        p.pitch = 0.0;
    }

    #[test]
    fn a_grapple_on_a_rock_attached_to_a_turning_actor_rides_the_wheel() {
        let (mut game, mut script) = wheel_game(6030, false);
        let idle = InputFrame::default();
        let fire = InputFrame {
            grapple_held: true,
            ..InputFrame::default()
        };
        for _ in 0..10 {
            script.tick(&mut game, &idle).unwrap();
        }
        game.set_max_grapples(3);
        game.enable_grapple(true);
        // The fire tick turns the wheel once more before the trace (the
        // movers move before the input events): the rock will be at 11
        // steps, 1100 units. The pawn looks at it from 1025 uu out on that
        // ray.
        let at_fire = 11 * 100;
        look_at_the_wheel(&mut game, 1025.0, at_fire);
        let t = script.tick(&mut game, &fire).unwrap();
        let attached = t.report.events.gun.attached.expect("the grapple attaches");
        assert_eq!(attached.surface.actor, Some(6), "the attached rock");
        assert_eq!(attached.surface.class, ActorClass::InterpActor);
        // The anchor: before the rock's outer face (350 uu out) on the
        // pawn's ray, 4.0 uu short of it as before any mesh's triangles
        // (G-AT-4): 354 uu out.
        let anchor0 = game.player().script.gun.grapple_location;
        let expected0 = at_yaw(354.0, at_fire, 125.0);
        assert!(
            (anchor0 - expected0).length() < 0.05,
            "{anchor0} vs {expected0}"
        );
        let follow = game.player().script.gun.follow.expect("the anchor follows");
        assert_eq!(follow.actor, 6);
        // Held: the anchor rides the rock, which rides the hub: 100 units of
        // yaw a tick about the hub's axis.
        let mut released = None;
        let mut previous = (anchor0 - game.player().position).length();
        for tick in 1..=60 {
            let t = script.tick(&mut game, &fire).unwrap();
            let expected = at_yaw(354.0, at_fire + tick * 100, 125.0);
            let gun = game.player().script.gun;
            assert!(
                (gun.grapple_location - expected).length() < 0.05,
                "tick {tick}: the anchor is at {}, the wheel took its place to {expected}",
                gun.grapple_location
            );
            // The rope: the pull's distance is measured to the anchor where
            // the wheel has taken it (G-PH-2).
            let d = (game.player().position - expected).length();
            assert!(
                (gun.distance - d).abs() < 0.05,
                "tick {tick}: measured {}, the anchor is {d} uu away",
                gun.distance
            );
            if let Some(r) = t.report.events.gun.released {
                released = Some((tick, r.reason, d));
                break;
            }
            assert!(gun.is_attached());
            previous = d;
        }
        // The proximity release measures to the moved anchor too: it comes
        // in the first tick whose distance is below the release distance,
        // 200 uu (G-RL-2).
        let (tick, reason, d) = released.expect("the pull brings the pawn to the anchor");
        assert_eq!(reason, asamu_player::grapple_gun::ReleaseReason::Proximity);
        assert!(d < 200.0 && previous >= 200.0, "{previous} then {d}");
        // By then the anchor has gone a long way from where the grapple hit.
        let travelled = (at_yaw(354.0, at_fire + tick * 100, 125.0) - anchor0).length();
        assert!(travelled > 30.0, "{travelled} uu in {tick} ticks");
    }

    #[test]
    fn a_grapple_stays_attached_when_its_rock_passes_behind_a_wall() {
        // 12 000 units/s: 200 units a tick.
        let (mut game, mut script) = wheel_game(12_000, true);
        let fire = InputFrame {
            grapple_held: true,
            ..InputFrame::default()
        };
        game.set_max_grapples(3);
        game.enable_grapple(true);
        // From 1500 uu out on the rock's ray of the fire tick (one step).
        look_at_the_wheel(&mut game, 1500.0, 200);
        let t = script.tick(&mut game, &fire).unwrap();
        let attached = t.report.events.gun.attached.expect("the grapple attaches");
        assert_eq!(attached.surface.actor, Some(6));
        let mut hidden_for = 0;
        for tick in 1..=40 {
            script.tick(&mut game, &fire).unwrap();
            let expected = at_yaw(354.0, 200 * (1 + tick), 125.0);
            let gun = game.player().script.gun;
            // No release looks at the line to the anchor (GRAPPLE.md §8:
            // button, proximity, the instant timer, death, story mode).
            assert!(gun.is_attached(), "released in tick {tick}");
            assert!(
                (gun.grapple_location - expected).length() < 0.05,
                "tick {tick}: {} vs {expected}",
                gun.grapple_location
            );
            let eye = game.player().view_location(game.params());
            if game.world().line_blocked(eye, gun.grapple_location) {
                hidden_for += 1;
                if hidden_for == 12 {
                    break;
                }
            } else {
                assert_eq!(hidden_for, 0, "tick {tick}: in sight again");
            }
        }
        assert_eq!(
            hidden_for, 12,
            "the rock went behind the wall and the grapple held"
        );
    }

    /// A Matinee movement track moves an actor only in the interpolating
    /// physics: the lift of [`lift_map`], placed in the rotating physics
    /// with a rate, turns by the rate and is not lifted. Its Matinee still
    /// plays to its end.
    #[test]
    fn a_matinee_does_not_move_an_actor_in_the_rotating_physics() {
        let (mut map, mut s) = lift_map();
        let placed = |id: u32, name: &str, location: Vec3| PlacedActor {
            id,
            name: name.into(),
            kind: "interp_actor".into(),
            location,
            rotation: [0; 3],
            draw_scale: 1.0,
            draw_scale3d: Vec3::ONE,
            pre_pivot: Vec3::ZERO,
            physics: PlacedPhysics::Interpolating,
            rotation_rate: [0; 3],
            base: None,
            hard_attach: false,
            ignore_base_rotation: false,
            block_actors: true,
        };
        map.placed = vec![
            PlacedActor {
                physics: PlacedPhysics::Rotating,
                // 630 / 60 = 10.5: 10 units a tick.
                rotation_rate: [0, 630, 0],
                ..placed(5, "Lift", Vec3::ZERO)
            },
            PlacedActor {
                base: Some(5),
                hard_attach: true,
                ..placed(6, "Crate", Vec3::new(100.0, 100.0, 50.0))
            },
        ];
        let movers = prepare_movers(&mut map, &s);
        assert_eq!(movers.keys().copied().collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(unbind_turning_actors(&mut s, &map), 1);
        assert_eq!(unbind_turning_actors(&mut s, &map), 0);
        let mut game = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        let mut script = LevelScript::attach(&mut game, s, movers);
        game.start();
        for _ in 0..90 {
            script.tick(&mut game, &InputFrame::default()).unwrap();
        }
        // Not lifted (the track's 200 uu), turned 90 × 10 units.
        assert_eq!(script.mover_transform(5), Some((Vec3::ZERO, [0, 900, 0])));
        // The crate at (100, 100, 50) rode the turn: √2 × 100 uu out, at an
        // eighth of a turn (8192 units) plus 900.
        let (crate_at, crate_rotation) = script.mover_transform(6).unwrap();
        let expected = at_yaw(100.0 * std::f64::consts::SQRT_2, 8192 + 900, 50.0);
        assert!(
            (crate_at - expected).length() < 1e-2,
            "{crate_at} vs {expected}"
        );
        assert_eq!(crate_rotation, [0, 900, 0]);
        // The Matinee ran to its end all the same: `Completed` set the
        // grapple limit.
        assert_eq!(game.player().script.gun.max_grapples, 2);
        assert!(
            script.runtime().errors().is_empty(),
            "{:?}",
            script.runtime().errors()
        );
    }

    /// The three ways the original carries an attached actor
    /// (`UWorld::MoveActor`, module docs "Attached actors").
    #[test]
    fn an_attached_actor_is_carried_by_its_kind_of_attachment() {
        let placed = |location: Vec3, rotation: [i32; 3]| PlacedActor {
            id: 2,
            name: "A".into(),
            kind: "interp_actor".into(),
            location,
            rotation,
            draw_scale: 1.0,
            draw_scale3d: Vec3::ONE,
            pre_pivot: Vec3::ZERO,
            physics: PlacedPhysics::Other,
            rotation_rate: [0; 3],
            base: Some(1),
            hard_attach: false,
            ignore_base_rotation: false,
            block_actors: false,
        };
        // The base at (1000, 0, 0), turned a quarter turn of yaw already;
        // the actor 100 uu from it along world +Y, which is the base's own
        // X axis, its rotation 500 units of yaw more than the base's.
        let base = PlacedActor {
            id: 1,
            base: None,
            ..placed(Vec3::new(1000.0, 0.0, 0.0), [0, 16_384, 0])
        };
        let actor = placed(Vec3::new(1000.0, 100.0, 0.0), [0, 16_884, 0]);
        let old = (base.location, base.rotation);
        // The base moves by (0, 0, 10) and turns another quarter turn.
        let new = (Vec3::new(1000.0, 0.0, 10.0), [0, 32_768, 0]);
        let current = (actor.location, actor.rotation);
        let close = |a: Vec3, b: Vec3| (a - b).length() < 1e-2;

        // Hard-attached, not blocking: the transform relative to the base
        // is kept. The offset, world (0, 100, 0), is (100, 0, 0) in the
        // base's frame at a quarter turn; at half a turn that is world
        // (−100, 0, 0). Its rotation stays 500 units ahead of the base's:
        // 33 268, which a rotation rebuilt from a matrix gives as
        // 33 268 − 65 536 = −32 268.
        let hard = Passenger::of(
            &PlacedActor {
                hard_attach: true,
                ..actor.clone()
            },
            &base,
        );
        assert!(hard.rigid);
        let (l, r) = hard.carried(current, old, new);
        assert!(close(l, Vec3::new(900.0, 0.0, 10.0)), "{l}");
        assert_eq!(r, [0, -32_268, 0]);
        // ... and with `bIgnoreBaseRotation` it goes to the same place and
        // keeps its own rotation.
        let hard_unturned = Passenger::of(
            &PlacedActor {
                hard_attach: true,
                ignore_base_rotation: true,
                ..actor.clone()
            },
            &base,
        );
        let (l, r) = hard_unturned.carried(current, old, new);
        assert!(close(l, Vec3::new(900.0, 0.0, 10.0)), "{l}");
        assert_eq!(r, actor.rotation);

        // Not hard-attached (and not interpolating): carried by the base's
        // change of transform from where it is, the same place for a rigid
        // motion; its rotation takes the base's change (16 384 units).
        let soft = Passenger::of(&actor, &base);
        assert!(!soft.rigid);
        let (l, r) = soft.carried(current, old, new);
        assert!(close(l, Vec3::new(900.0, 0.0, 10.0)), "{l}");
        assert_eq!(r, [0, 33_268, 0]);
        // The change leaves out each rotation's low two bits: from 16 387
        // (which is 16 384 to the sine table) to 32 769 (32 768) it is
        // still 16 384.
        let (_, r) = soft.carried(current, (old.0, [0, 16_387, 0]), (new.0, [0, 32_769, 0]));
        assert_eq!(r, [0, 33_268, 0]);
        // Hard-attached but blocking actors: carried the same way, not
        // through a kept relative transform.
        let blocking = Passenger::of(
            &PlacedActor {
                hard_attach: true,
                block_actors: true,
                ..actor.clone()
            },
            &base,
        );
        assert!(!blocking.rigid);
        assert_eq!(
            blocking.carried(current, old, new),
            soft.carried(current, old, new)
        );
        // With `bIgnoreBaseRotation`: the base's translation only.
        let unturned = Passenger::of(
            &PlacedActor {
                ignore_base_rotation: true,
                ..actor.clone()
            },
            &base,
        );
        let (l, r) = unturned.carried(current, old, new);
        assert_eq!(l, Vec3::new(1000.0, 100.0, 10.0));
        assert_eq!(r, actor.rotation);

        // An actor in the interpolating physics is carried through its kept
        // relative transform even without a hard attachment, and what the
        // original keeps for it then is its offset from the base in world
        // axes: (0, 100, 0), which the base's new rotation (half a turn)
        // takes to (0, −100, 0).
        let interpolating = Passenger::of(
            &PlacedActor {
                physics: PlacedPhysics::Interpolating,
                ..actor.clone()
            },
            &base,
        );
        assert!(interpolating.rigid);
        let (l, r) = interpolating.carried(current, old, new);
        assert!(close(l, Vec3::new(1000.0, -100.0, 10.0)), "{l}");
        assert_eq!(r, [0, -32_268, 0]);
    }
}
