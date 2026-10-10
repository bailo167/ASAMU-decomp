//! Game-side NPC system: spawns the map's NPCs and story actors from the
//! converted scene ([`asamu_world::npc`]), ticks their state machines, applies
//! their effects on the player (the worm's push and kill) and reports events
//! for Kismet, audio and the UI. Behaviour spec:
//! `docs/reverse-engineering/NPCS.md`; frame order and data flow:
//! `docs/INTEGRATION.md`.
//!
//! # Wiring into [`crate::Game`]
//!
//! A [`NpcSystem`] attached with [`crate::Game::attach_npcs`]
//! ([`crate::load_level_with_kismet`] attaches the map's own) runs inside the
//! converted level's tick, in the original's frame order (GRAPPLE.md G-TM-2:
//! map actors tick after the player's input events and before its
//! controller and pawn):
//!
//! 1. Every handler call or interaction the game hands to the world objects
//!    (input events, the player's step, the death's and the push's grapple
//!    releases, story-mode changes) also reaches
//!    [`NpcSystem::apply_object_event`] (glow flowers on `ActorGrappled`,
//!    story items on `InteractWith`). The story-item model here supersedes
//!    `WorldObjects`' interaction count for Kismet: the Kismet host uses
//!    [`NpcEvent::ActorInteractedWith`] and ignores
//!    `WorldEvent::ActorInteractedWith` while a system is attached.
//! 2. Step 2 of the tick (map actors), after the movers, crystals and rocks:
//!    [`NpcSystem::tick_actors`]; the push's grapple release goes to the
//!    world objects, and a scream timeout starts the death sequence with
//!    `DeathCause::Scripted` (the worm already handled its own
//!    `NotifyKilled`).
//! 3. Step 5 (touches): [`NpcSystem::update_touches`] along the same path
//!    (collectibles, foliage).
//! 4. A death whose cause passes [`death_notifies_npcs`] (kill zone or
//!    dynamic kill zone only; not `KillZ`, not scripted) calls
//!    [`NpcSystem::notify_player_killed`]; every respawn teleport calls
//!    [`NpcSystem::on_player_respawned`].
//! 5. The tick's events are in [`crate::Game::npc_events`]; the Kismet host
//!    ([`crate::kismet_host`]) routes [`NpcEvent::Worm`] to every
//!    `SeqEvent_WormEvents` output `kind.output()`,
//!    [`NpcEvent::CollectibleCollected`] to `SeqEvent_CollectibleCollected`
//!    (and the collectible's own `SeqEvent_Touch`),
//!    [`NpcEvent::ActorInteractedWith`] to `SeqEvent_ActorInteractedWith`
//!    (every instance counts the activation against its `MaxTriggerCount`,
//!    only those whose originator matches fire, CONFIRMED (src)), and the
//!    Kismet worm actions to [`NpcSystem::start_worm`],
//!    [`NpcSystem::shut_down_worm`] and [`NpcSystem::pause_worm`]
//!    (`SeqAct_PauseWorm` input 0 `UnPause`, 1 `Pause`, in that order, so
//!    both inputs in one impulse end paused).
//! 6. Save: the app turns [`NpcEvent::CollectibleCollected`] /
//!    [`NpcEvent::StoryItemRegistered`] into progression records
//!    ([`collectible_save_key`], [`story_item_save_key`]); a snapshot restore
//!    calls [`NpcSystem::set_collected`].

use std::path::Path;

use crate::Game;

use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::pawn::PowerJumpStateName;
use asamu_player::{BootsStateName, PlayerParams, PlayerState, SimEvent, StepEvents, pawn};
use asamu_world::gameplay::DeathCause;
use asamu_world::objects::ObjectEvent;
use asamu_world::scene::{DataSource, DirSource, LoadOptions, SceneError, SubLevel};
use glam::Vec3;

/// The world-side definitions and state machines.
pub use asamu_world::npc as defs;
pub use asamu_world::npc::{
    AnimHint, BackpackAnim, BackpackState, CollectibleDef, NpcEffect, NpcEvent, NpcOutput,
    NpcRuntime, NpcScene, SkeletalAnimInfo, SkeletalIndex, SkeletalMeshInfo, SkinnedActorDef,
    SkinnedComponent, SkinnedDrive, StoryItemDef, StoryItemStateName, VillagerStateName, WormAnim,
    WormDef, WormEventKind, WormStateName, load_npc_scene, load_npc_scene_for_map,
};

/// Seed of the NPC random streams (ours; the original's random stream is not
/// reproducible).
pub const NPC_RANDOM_SEED: u64 = 0x4153_414D_554E_5043;

/// Options of an [`NpcSystem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NpcOptions {
    /// Random seed.
    pub seed: u64,
    /// Time-trial game (`ASAMUGameInfoTimeTrial`): collectibles hidden and
    /// non-colliding.
    pub time_trial: bool,
}

impl Default for NpcOptions {
    fn default() -> Self {
        Self {
            seed: NPC_RANDOM_SEED,
            time_trial: false,
        }
    }
}

/// What one [`NpcSystem::tick_actors`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NpcTickReport {
    /// Events in order (queued Kismet-action events first).
    pub events: Vec<NpcEvent>,
    /// Grapple-release events caused by the worm's push (route their handler
    /// calls to the world objects).
    pub released: StepEvents,
    /// The worm pushed the player this tick.
    pub pushed: bool,
    /// The worm's scream timed out: start the death sequence.
    pub kill_player: bool,
}

/// What the `use` key does (`ASAMUPlayerController.Use`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseOutcome {
    /// Outside story mode the controller ignores the key (CONFIRMED (src)).
    NotInStoryMode,
    /// In story mode the stock use search runs; it only finds actors with a
    /// Kismet `SeqEvent_Used`, and no shipped map has one (CONFIRMED (map
    /// census); the stock search itself TENTATIVE). Story interactables use
    /// the fire button instead (GRAPPLE.md G-AC-0).
    NothingToUse,
}

/// NPCs and story actors of a running map.
#[derive(Clone, Debug, Default)]
pub struct NpcSystem {
    scene: NpcScene,
    runtime: NpcRuntimeHolder,
    pending: Vec<NpcEvent>,
    /// `(pawn actor id, index into the scene's dynamic instances)` of the
    /// NPC pawns' collision cylinders ([`NpcSystem::set_pawn_colliders`]).
    pawn_colliders: Vec<(u32, usize)>,
}

/// The runtime (a wrapper so `NpcSystem` can derive `Default`).
#[derive(Clone, Debug)]
struct NpcRuntimeHolder(NpcRuntime);

impl Default for NpcRuntimeHolder {
    fn default() -> Self {
        Self(NpcRuntime::new(
            &NpcScene::default(),
            NPC_RANDOM_SEED,
            false,
        ))
    }
}

/// Applies the worm's push (`PushPlayer`): `ReleaseGrappleButton` (the common
/// release, G-RL-1/5), `SetPhysics(PHYS_Falling)`, then `delta_v` added to
/// the velocity. Returns the release's events.
pub fn apply_worm_push(player: &mut PlayerState, delta_v: Vec3) -> StepEvents {
    let released = pawn::release_grapple(player, ReleaseReason::External);
    player.pawn.flying = false;
    player.grounded = false;
    player.pawn.based = false;
    if delta_v.is_finite() {
        player.velocity += delta_v;
    }
    released
}

fn merge(into: &mut StepEvents, from: &StepEvents) {
    into.kismet.extend(&from.kismet);
    if from.gun.released.is_some() {
        into.gun.released = from.gun.released;
    }
}

impl NpcSystem {
    /// A system for `scene`.
    #[must_use]
    pub fn new(scene: NpcScene, options: NpcOptions) -> Self {
        let runtime = NpcRuntime::new(&scene, options.seed, options.time_trial);
        Self {
            scene,
            runtime: NpcRuntimeHolder(runtime),
            pending: Vec::new(),
            pawn_colliders: Vec::new(),
        }
    }

    /// Spawns from converted scenes in `source` for the levels of a loaded
    /// map (`LoadedMap::levels`; actor ids then match the game's). Problems
    /// are warnings in [`NpcScene::warnings`].
    #[must_use]
    pub fn from_source(source: &dyn DataSource, levels: &[SubLevel], options: NpcOptions) -> Self {
        Self::new(
            load_npc_scene(source, levels, &LoadOptions::default()),
            options,
        )
    }

    /// [`Self::from_source`] over a converted directory on disk.
    #[must_use]
    pub fn load_from_dir(dir: impl AsRef<Path>, levels: &[SubLevel], options: NpcOptions) -> Self {
        Self::from_source(&DirSource::new(dir.as_ref()), levels, options)
    }

    /// Spawns from `map` and its streamed levels in a converted directory
    /// (without a loaded map).
    ///
    /// # Errors
    /// The map's scene is missing or malformed.
    pub fn load_for_map(
        dir: impl AsRef<Path>,
        map: &str,
        options: NpcOptions,
    ) -> Result<Self, SceneError> {
        let source = DirSource::new(dir.as_ref());
        Ok(Self::new(
            load_npc_scene_for_map(&source, map, &LoadOptions::default())?,
            options,
        ))
    }

    /// The definitions.
    #[must_use]
    pub fn scene(&self) -> &NpcScene {
        &self.scene
    }

    /// The run-time state.
    #[must_use]
    pub fn runtime(&self) -> &NpcRuntime {
        &self.runtime.0
    }

    /// `true` when the map has nothing this system simulates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let s = &self.scene;
        s.worms.is_empty()
            && s.maddies.is_empty()
            && s.villagers.is_empty()
            && s.collectibles.is_empty()
            && s.story_items.is_empty()
            && s.flowers.is_empty()
            && s.foliage.is_empty()
    }

    /// Ticks the NPC actors (step 2 of the frame, see the module docs) and
    /// applies their effects on `player`.
    pub fn tick_actors(&mut self, player: &mut PlayerState, dt: f32) -> NpcTickReport {
        let mut out = NpcOutput {
            events: std::mem::take(&mut self.pending),
            effects: Vec::new(),
        };
        self.runtime
            .0
            .tick_actors(&self.scene, player.position, dt, &mut out);
        let mut report = NpcTickReport::default();
        for effect in &out.effects {
            match *effect {
                NpcEffect::PushPlayer { delta_v, .. } => {
                    let released = apply_worm_push(player, delta_v);
                    merge(&mut report.released, &released);
                    report.pushed = true;
                }
                NpcEffect::KillPlayer { .. } => report.kill_player = true,
            }
        }
        report.events = out.events;
        report
    }

    /// Touches of the player's move this tick (collectibles, foliage).
    pub fn update_touches(
        &mut self,
        before: Vec3,
        after: Vec3,
        params: &PlayerParams,
    ) -> Vec<NpcEvent> {
        let mut out = NpcOutput::default();
        self.runtime.0.update_touches(
            &self.scene,
            before,
            after,
            params.movement.capsule_radius.value,
            params.movement.capsule_half_height.value,
            &mut out,
        );
        out.events
    }

    /// Routes the player simulation's handler calls and interactions.
    pub fn apply_sim_events(&mut self, events: &StepEvents) -> Vec<NpcEvent> {
        let mut out = NpcOutput::default();
        for e in events.kismet.iter() {
            let ev = match e {
                SimEvent::ActorGrappled { actor } => ObjectEvent::Grappled(actor),
                SimEvent::ActorUngrappled { actor } => ObjectEvent::UnGrappled(actor),
                SimEvent::InteractWith { actor } => ObjectEvent::InteractWith(actor),
                _ => continue,
            };
            self.runtime.0.apply_object_event(&self.scene, ev, &mut out);
        }
        out.events
    }

    /// Routes one handler call or interaction.
    pub fn apply_object_event(&mut self, event: ObjectEvent) -> Vec<NpcEvent> {
        let mut out = NpcOutput::default();
        self.runtime
            .0
            .apply_object_event(&self.scene, event, &mut out);
        out.events
    }

    /// `NotifyKilled` for a player death not caused by the worm.
    pub fn notify_player_killed(&mut self) -> Vec<NpcEvent> {
        let mut out = NpcOutput::default();
        self.runtime.0.notify_player_killed(&self.scene, &mut out);
        out.events
    }

    /// After a respawn teleport: touches restart.
    pub fn on_player_respawned(&mut self) {
        self.runtime.0.clear_touches();
    }

    /// Kismet `SeqAct_StartWorm`; its events join the next tick's report.
    pub fn start_worm(&mut self, worm: u32) -> bool {
        let mut out = NpcOutput::default();
        let ok = self.runtime.0.start_worm(&self.scene, worm, &mut out);
        self.pending.extend(out.events);
        ok
    }

    /// Kismet `SeqAct_ShutDownWorm`.
    pub fn shut_down_worm(&mut self, worm: u32) -> bool {
        self.runtime.0.shut_down_worm(&self.scene, worm)
    }

    /// Kismet `SeqAct_PauseWorm` (`paused` = input 1 `Pause`).
    pub fn pause_worm(&mut self, worm: u32, paused: bool) -> bool {
        self.runtime.0.pause_worm(&self.scene, worm, paused)
    }

    /// Kismet `SeqAct_WormResetSleepTimer`.
    pub fn worm_reset_sleep_timer(&mut self, worm: u32) -> bool {
        self.runtime.0.worm_reset_sleep_timer(&self.scene, worm)
    }

    /// Kismet `SeqAct_MaddieBackpack` (input 0 `Enable`, 1 `Disable`).
    pub fn maddie_backpack(&mut self, enable: bool) {
        self.runtime.0.backpack.set_enabled(enable);
    }

    /// Kismet `SeqAct_PlayMaddieBackpackAnim`; `false` without the arms.
    pub fn play_maddie_backpack_anim(&mut self, anim: BackpackAnim) -> bool {
        self.runtime.0.backpack.play(anim)
    }

    /// Puts the Maddie pawn `id` into `TalkingWithPlayer` (no original
    /// caller; for tools).
    pub fn maddie_talk(&mut self, id: u32) -> bool {
        let Some(i) = self.scene.maddies.iter().position(|m| m.id == id) else {
            return false;
        };
        self.runtime
            .0
            .maddies
            .get_mut(i)
            .map(|m| m.talk())
            .is_some()
    }

    /// `StartTalkingWithPawn` on villager `id` (no original caller).
    pub fn villager_start_talking(&mut self, id: u32, pawn_location: Vec3) -> bool {
        let Some(i) = self.scene.villagers.iter().position(|v| v.id == id) else {
            return false;
        };
        self.runtime
            .0
            .villagers
            .get_mut(i)
            .map(|v| v.start_talking(pawn_location))
            .is_some()
    }

    /// `StopTalking` on villager `id`.
    pub fn villager_stop_talking(&mut self, id: u32) -> bool {
        let Some(i) = self.scene.villagers.iter().position(|v| v.id == id) else {
            return false;
        };
        self.runtime
            .0
            .villagers
            .get_mut(i)
            .map(|v| v.stop_talking())
            .is_some()
    }

    /// Restores a collectible's collected state (save snapshot).
    pub fn set_collected(&mut self, id: u32, collected: bool) -> bool {
        self.runtime.0.set_collected(&self.scene, id, collected)
    }

    /// Matinee's `SetAnimPosition` on skinned actor `id` (see
    /// [`NpcRuntime::set_anim_position`]): sound notifies join the next
    /// tick's events; the Kismet notify names fired are returned. `None` for
    /// an actor that is not a skinned actor of the map.
    pub fn set_anim_position(
        &mut self,
        id: u32,
        sequence: &str,
        position: f32,
        fire_notifies: bool,
        looping: bool,
    ) -> Option<Vec<String>> {
        let mut out = NpcOutput::default();
        let names = self.runtime.0.set_anim_position(
            &self.scene,
            id,
            sequence,
            position,
            fire_notifies,
            looping,
            &mut out,
        );
        self.pending.extend(out.events);
        names
    }

    /// The `SequenceLength` of `sequence` on skinned actor `id`'s mesh.
    #[must_use]
    pub fn anim_sequence_length(&self, id: u32, sequence: &str) -> Option<f32> {
        self.runtime
            .0
            .anim_sequence_length(&self.scene, id, sequence)
    }

    /// Matinee `SetSkelControlStrength` on skinned actor `id`.
    pub fn set_skel_control_strength(&mut self, id: u32, control: &str, strength: f32) -> bool {
        self.runtime
            .0
            .set_skel_control_strength(&self.scene, id, control, strength)
    }

    /// `SeqAct_SetLookAtTarget` on look-at actor `id`.
    pub fn set_look_at(
        &mut self,
        id: u32,
        target: Option<u32>,
        head_offset: Vec3,
        eyes_offset: Vec3,
    ) -> bool {
        self.runtime
            .0
            .set_look_at(&self.scene, id, target, head_offset, eyes_offset)
    }

    /// Records where the pawns' collision cylinders sit among the scene's
    /// dynamic instances (`load_level_with_kismet` adds them, see
    /// [`crate::kismet_host`]).
    pub fn set_pawn_colliders(&mut self, colliders: Vec<(u32, usize)>) {
        self.pawn_colliders = colliders;
    }

    /// The pawns' collision cylinders as `(actor id, dynamic instance index)`.
    #[must_use]
    pub fn pawn_colliders(&self) -> &[(u32, usize)] {
        &self.pawn_colliders
    }

    /// Moves the pawns' collision cylinders to where the pawns are now.
    pub fn place_pawn_colliders(
        &self,
        collision: &asamu_world::collision::CollisionScene,
        dynamic: &mut [asamu_world::collision::Instance],
    ) {
        if self.pawn_colliders.is_empty() {
            return;
        }
        for (id, cyl) in self.runtime.0.pawn_cylinders(&self.scene) {
            let Some(&(_, index)) = self.pawn_colliders.iter().find(|(c, _)| *c == id) else {
                continue;
            };
            // Only the instance recorded for this pawn is ever moved: an
            // index that no longer names it (a dynamic list rebuilt by
            // someone else) must not drag another actor's collision here.
            if let Some(inst) = dynamic.get_mut(index)
                && inst.info.actor == Some(id)
            {
                let center = cyl.center.as_dvec3();
                if inst.to_world.translation == center {
                    continue;
                }
                let info = inst.info;
                let to_world = asamu_world::collision::Affine::from_translation(center);
                if collision.place_dynamic(inst, to_world) {
                    inst.info = info;
                }
            }
        }
    }

    /// The `use` key (see [`UseOutcome`]).
    #[must_use]
    pub fn use_action(&self, player: &PlayerState) -> UseOutcome {
        if player.script.started && player.script.is_story() {
            UseOutcome::NothingToUse
        } else {
            UseOutcome::NotInStoryMode
        }
    }
}

/// The progression key of a collectible (SAVE.md §6.3, §9.2): its actor
/// path relative to the map, `TheWorld.PersistentLevel.<name>`.
#[must_use]
pub fn collectible_save_key(def: &CollectibleDef) -> String {
    format!("TheWorld.PersistentLevel.{}", def.name)
}

/// The progression key of an optional story item (SAVE.md §6.3): the level
/// file name followed by the registered actor's path relative to the map, or
/// by `None` for the stand-alone items' null parent (quirk Q5). Our format
/// for the path part follows [`collectible_save_key`] (TENTATIVE: the
/// original's exact string is not traced; our saves never mix with the
/// original's).
#[must_use]
pub fn story_item_save_key(map: &str, item: Option<&StoryItemDef>) -> String {
    match item {
        Some(d) => format!("{map}TheWorld.PersistentLevel.{}", d.name),
        None => format!("{map}None"),
    }
}

impl Game {
    /// Attaches an NPC system: from the next tick its actors run inside the
    /// frame (see the [`crate::npc`] module docs). Converted levels only
    /// (hand-made levels never tick it).
    pub fn attach_npcs(&mut self, system: NpcSystem) {
        self.npcs = Some(Box::new(system));
    }

    /// The attached NPC system.
    #[must_use]
    pub fn npcs(&self) -> Option<&NpcSystem> {
        self.npcs.as_deref()
    }

    /// The attached NPC system, for the Kismet host and tools.
    pub fn npcs_mut(&mut self) -> Option<&mut NpcSystem> {
        self.npcs.as_deref_mut()
    }

    /// NPC events of the latest tick (including those raised by calls made
    /// between ticks, e.g. Kismet's worm actions), in order.
    #[must_use]
    pub fn npc_events(&self) -> &[NpcEvent] {
        &self.npc_events
    }
}

/// Whether a player death reaches the NPC controllers' `NotifyKilled`
/// (`GameInfo.NotifyKilled`, which only the screaming worm handles): only
/// the touch handlers of `ASAMUKillZone` and `ASAMUDynamicKillZone` call it
/// after `PlayerDied` (the worm's own scream timeout calls it too and is
/// already applied by the worm). Falling below `KillZ` (`Died`), Kismet
/// `SeqAct_PlayerDied`, the restart key and the pause-menu restart only call
/// `PlayerDied`, so a screaming worm keeps screaming through them.
/// CONFIRMED (src: every `NotifyKilled` caller in the `asamu` package).
#[must_use]
pub fn death_notifies_npcs(cause: DeathCause) -> bool {
    matches!(cause, DeathCause::KillZone | DeathCause::DynamicKillZone)
}

/// Reads the skeletal manifest (`skeletal/manifest.json`) of a converted
/// directory (presentation: which glTF file and animation to play).
///
/// # Errors
/// Missing or malformed manifest.
pub fn load_skeletal_index(dir: impl AsRef<Path>) -> Result<SkeletalIndex, SceneError> {
    SkeletalIndex::load(&DirSource::new(dir.as_ref()), &LoadOptions::default())
}

/// Mesh of the first-person hands (template
/// `asamu.Default__GrappleGun.FirstPersonMesh`). CONFIRMED (cdo).
pub const HAND_MESH: &str = "PlayerHand.Meshes.PlayerHand";
/// `FOV` of the first-person mesh component, degrees (horizontal, UE3).
/// Template `asamu.Default__GrappleGun.FirstPersonMesh`. CONFIRMED (cdo).
pub const HAND_MESH_FOV: f32 = 70.0;
/// `BobDamping` of the grapple gun (hand bob factor). ScriptDefault
/// `asamu.GrappleGun`. CONFIRMED (cdo).
pub const HAND_BOB_DAMPING: f32 = 0.15;

/// First-person hand animations (sequence names of `PlayerHand.Root`, as the
/// nodes of `PlayerHand.PlayerHand_AnimTree` name them; CONFIRMED (data)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HandAnim {
    /// `HandsDown` (hand hidden / lowered).
    HandsDown,
    /// `Grapple` (attach).
    GrappleBegin,
    /// `Grapple_Loop`.
    GrappleLoop,
    /// `Grapple_Release`.
    GrappleRelease,
    /// `rocketBoots`.
    RocketBoots,
    /// `PowerJumpIdle`.
    PowerJumpIdle,
    /// `JumpIdle` (airborne).
    JumpIdle,
    /// `Sprint`.
    Sprint,
    /// `Idle`.
    Idle,
}

impl HandAnim {
    /// The `AnimSeqName`.
    #[must_use]
    pub fn sequence(self) -> &'static str {
        match self {
            Self::HandsDown => "HandsDown",
            Self::GrappleBegin => "Grapple",
            Self::GrappleLoop => "Grapple_Loop",
            Self::GrappleRelease => "Grapple_Release",
            Self::RocketBoots => "rocketBoots",
            Self::PowerJumpIdle => "PowerJumpIdle",
            Self::JumpIdle => "JumpIdle",
            Self::Sprint => "Sprint",
            Self::Idle => "Idle",
        }
    }

    /// The node's `bLooping` in the hand tree (CONFIRMED (data); `Idle` is
    /// played by an `AnimNodeRandom`, looped here).
    #[must_use]
    pub fn looping(self) -> bool {
        matches!(
            self,
            Self::GrappleLoop | Self::PowerJumpIdle | Self::JumpIdle | Self::Sprint | Self::Idle
        )
    }

    /// A one-shot clip the tree plays before this one (attach → loop).
    #[must_use]
    pub fn entry(self) -> Option<Self> {
        (self == Self::GrappleLoop).then_some(Self::GrappleBegin)
    }
}

/// The hand animation for the player's state (TENTATIVE mapping of the
/// grapple gun's blend-node switching: hidden > attached > released >
/// boosting > power leap > airborne > sprinting > idle).
#[must_use]
pub fn hand_animation(player: &PlayerState) -> HandAnim {
    let s = &player.script;
    if !s.started {
        return HandAnim::Idle;
    }
    if s.gun.hand_hidden {
        HandAnim::HandsDown
    } else if s.gun.attached.is_some() {
        HandAnim::GrappleLoop
    } else if s.gun.released {
        HandAnim::GrappleRelease
    } else if s.boots.state == BootsStateName::Boosting {
        HandAnim::RocketBoots
    } else if s.power_jump.state == PowerJumpStateName::Jumping && !player.grounded {
        HandAnim::PowerJumpIdle
    } else if !player.grounded {
        HandAnim::JumpIdle
    } else if s.sprint.active {
        HandAnim::Sprint
    } else {
        HandAnim::Idle
    }
}

/// The hand mesh's bob offset (UE3 world axes, UU): ABILITIES.md A-CM-5 with
/// the walk bob of the script layer — `BobDamping`·walk bob laterally,
/// `(0.10 + 0.15·0.15)`·walk bob vertically. The landing/jump bob term is not
/// modelled by the simulation and is left out (TENTATIVE partial).
#[must_use]
pub fn hand_bob_offset(player: &PlayerState) -> Vec3 {
    let wb = player.script.walk_bob;
    if !player.script.started || !wb.is_finite() {
        return Vec3::ZERO;
    }
    Vec3::new(
        wb.x * HAND_BOB_DAMPING,
        wb.y * HAND_BOB_DAMPING,
        wb.z * (0.10 + HAND_BOB_DAMPING * HAND_BOB_DAMPING),
    )
}

/// The worm's animation for presentation: sequence name, its time and whether
/// the node loops.
#[must_use]
pub fn worm_animation(state: &defs::WormState) -> (&'static str, f32, bool) {
    let (anim, position, _, _) = state.presentation();
    let node = state.anim.node(anim);
    (anim.sequence(), position, node.looping)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_animation_priorities() {
        let params = PlayerParams::asamu_original();
        let mut p = PlayerState::new(Vec3::ZERO, 0.0);
        assert_eq!(hand_animation(&p), HandAnim::Idle, "no script layer");
        pawn::start(&mut p, &params);
        p.grounded = true;
        assert_eq!(hand_animation(&p), HandAnim::Idle);
        p.script.sprint.active = true;
        assert_eq!(hand_animation(&p), HandAnim::Sprint);
        p.grounded = false;
        assert_eq!(hand_animation(&p), HandAnim::JumpIdle);
        p.script.gun.hand_hidden = true;
        assert_eq!(hand_animation(&p), HandAnim::HandsDown);
        assert_eq!(HandAnim::GrappleLoop.entry(), Some(HandAnim::GrappleBegin));
        assert!(HandAnim::Idle.looping() && !HandAnim::GrappleRelease.looping());
    }

    #[test]
    fn only_kill_zone_deaths_reach_the_worm() {
        assert!(death_notifies_npcs(DeathCause::KillZone));
        assert!(death_notifies_npcs(DeathCause::DynamicKillZone));
        assert!(!death_notifies_npcs(DeathCause::KillZ));
        assert!(!death_notifies_npcs(DeathCause::Scripted));
    }

    #[test]
    fn use_key_only_matters_in_story_mode() {
        let params = PlayerParams::asamu_original();
        let mut p = PlayerState::new(Vec3::ZERO, 0.0);
        pawn::start(&mut p, &params);
        let npcs = NpcSystem::default();
        assert_eq!(npcs.use_action(&p), UseOutcome::NotInStoryMode);
        pawn::enter_story_mode(&mut p, &params);
        assert_eq!(npcs.use_action(&p), UseOutcome::NothingToUse);
        assert!(npcs.is_empty());
    }
}
