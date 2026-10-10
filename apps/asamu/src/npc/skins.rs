//! Placed skinned actors: villagers and Maddie as `SkeletalMeshActor`s, the
//! worm and the (never placed) NPC pawns, from the importer's skeletal glTF
//! files, with Bevy skins and an `AnimationPlayer`.
//!
//! Animation choice: the worm follows its state machine (the active node of
//! its emulated `AnimTree`); a placed skinned actor follows the simulation's
//! node (`NpcRuntime::skinned`: its component's own `AnimNodeSequence`, or
//! the slot node Matinee's animation-control tracks drive, held where
//! Matinee left it) — a playing node runs on the `AnimationPlayer` and is
//! re-synchronised when it drifts, a held one is posed at its position every
//! frame; without simulation data, the mesh's first idle-named sequence
//! (ours); villager and Maddie pawns pick idle/walk/talk sequences by name
//! from their state (ours: their `AnimTree` nodes carry no sequence names).
//!
//! Look-at ([`apply_look_at`], after the animation, before transform
//! propagation): the head and eye `SkelControlLookAt` controls of
//! `SeqAct_SetLookAtTarget` actors aim at the player pawn's location plus
//! the actor's offsets, blended by the controls' strengths (Matinee's
//! skeletal-control strength tracks); the worm's four controls aim at its
//! aim while it is alerted or screaming. The bone is turned so its look-at
//! axis points at the target, limited to `MaxAngle` (TENTATIVE: an
//! approximation of `USkelControlLookAt`; up-axis and per-axis rotation
//! limits, the target interpolation and the limit's reference pose are not
//! modelled).

use std::collections::HashMap;
use std::time::Duration;

use asamu_assets::transform::{decompose, instance_matrix, ue_row_matrix_to_mat4};
use asamu_core::glam as sim_glam;
use asamu_game::npc::defs::{MaddieStateName, SkinnedComponent};
use asamu_game::npc::{
    HandAnim, SkeletalMeshInfo, VillagerStateName, WormStateName, hand_animation, worm_animation,
};
use bevy::asset::{AssetPath, RenderAssetUsages};
use bevy::gltf::convert_coordinates::GltfConvertCoordinates;
use bevy::gltf::{GltfAssetLabel, GltfLoaderSettings};
use bevy::prelude::*;

use super::NpcWorld;
use crate::{Sim, bevy_quat, bevy_vec, converted};

/// Blend between NPC animations, s (ours, presentation).
const BLEND: Duration = Duration::from_millis(300);

/// What drives a skinned entity's animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SkinKind {
    /// Its own looping node (or the stand-in idle).
    Ambient,
    /// The worm `n` (index into `NpcScene::worms`).
    Worm(usize),
    /// The Maddie pawn `n`.
    Maddie(usize),
    /// The villager pawn `n` (moves).
    Villager(usize),
    /// The first-person hands (animation from the player's state).
    Hands,
}

/// The animation a skin starts with (an ambient actor: its component's own
/// `AnimNodeSequence` when the scene provides it).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StartAnim {
    /// `AnimSeqName`.
    pub sequence: String,
    /// `CurrentTime` at level start, s.
    pub time: f32,
    /// `bLooping`.
    pub looping: bool,
    /// `bPlaying` (a node that does not play holds the pose at `time`).
    pub playing: bool,
    /// `Rate`.
    pub rate: f32,
}

impl StartAnim {
    /// A looping animation from its start at rate 1.
    pub(super) fn looping(sequence: &str) -> Self {
        Self {
            sequence: sequence.to_owned(),
            time: 0.0,
            looping: true,
            playing: true,
            rate: 1.0,
        }
    }
}

/// A spawned skinned mesh.
#[derive(Component)]
pub(super) struct NpcSkin {
    kind: SkinKind,
    /// `(SequenceName, clip)` of every animation this skin may play.
    clips: Vec<(String, Handle<AnimationClip>)>,
    /// First animation.
    start: Option<StartAnim>,
    /// Graph nodes, same order as `clips`.
    nodes: Vec<AnimationNodeIndex>,
    /// The entity holding the `AnimationPlayer`.
    player: Option<Entity>,
    /// Sequence currently playing.
    current: Option<String>,
    /// Component-space → render transform of the actor at rest (villagers
    /// move from here).
    base: Transform,
    /// Index into `NpcScene::skinned` (placed skinned actors).
    skinned: Option<usize>,
    /// Bone entities by lower-case name (filled on first use).
    bones: HashMap<String, Entity>,
    /// Look-at strengths shown per control (smoothed for the worm).
    look_alpha: HashMap<String, f32>,
}

impl NpcSkin {
    /// `true` for the first-person hands (they outlive a map change).
    pub(super) fn is_hands(&self) -> bool {
        self.kind == SkinKind::Hands
    }

    /// A skin that has not met its `AnimationPlayer` yet.
    pub(super) fn new(
        kind: SkinKind,
        clips: Vec<(String, Handle<AnimationClip>)>,
        start: Option<StartAnim>,
        base: Transform,
    ) -> Self {
        Self {
            kind,
            clips,
            start,
            nodes: Vec::new(),
            player: None,
            current: None,
            base,
            skinned: None,
            bones: HashMap::new(),
            look_alpha: HashMap::new(),
        }
    }
}

/// Loader settings shared by every skeletal glTF load (scene and clips must
/// use the same settings): materials are the importer's placeholders,
/// coordinates are already converted by the importer.
pub(super) fn skeletal_settings(s: &mut GltfLoaderSettings) {
    s.load_materials = RenderAssetUsages::RENDER_WORLD;
    s.load_cameras = false;
    s.load_lights = false;
    s.load_animations = true;
    s.convert_coordinates = Some(GltfConvertCoordinates {
        rotate_scene_entity: false,
        rotate_meshes: false,
    });
}

/// The asset path of a converted skeletal glTF.
pub(super) fn gltf_path(mesh: &SkeletalMeshInfo) -> AssetPath<'static> {
    AssetPath::from(format!("{}://{}", converted::SOURCE, mesh.gltf))
}

/// Loads the clips of `sequences` (those the mesh has).
pub(super) fn load_clips(
    server: &AssetServer,
    mesh: &SkeletalMeshInfo,
    sequences: &[&str],
) -> Vec<(String, Handle<AnimationClip>)> {
    let path = gltf_path(mesh);
    let mut out: Vec<(String, Handle<AnimationClip>)> = Vec::new();
    for s in sequences {
        if out.iter().any(|(n, _)| n.eq_ignore_ascii_case(s)) {
            continue;
        }
        if let Some(a) = mesh.animation(s) {
            let h: Handle<AnimationClip> = server
                .load_builder()
                .with_settings(skeletal_settings)
                .load(GltfAssetLabel::Animation(a.index).from_asset(path.clone()));
            out.push((a.sequence.clone(), h));
        }
    }
    out
}

/// The render transform of a skeletal component.
pub(super) fn component_transform(
    comp: &SkinnedComponent,
    mesh: &SkeletalMeshInfo,
) -> Option<Transform> {
    let m = instance_matrix(
        &ue_row_matrix_to_mat4(&comp.local_to_world),
        mesh.scale,
        crate::SCALE,
    );
    let t = decompose(&m)?;
    Some(Transform {
        translation: bevy_vec(t.translation),
        rotation: bevy_quat(t.rotation),
        scale: bevy_vec(t.scale),
    })
}

/// What to spawn for one skeletal component.
struct SpawnSpec<'a> {
    kind: SkinKind,
    sequences: &'a [&'a str],
    start: Option<StartAnim>,
    name: &'a str,
    /// World actor id of an actor Kismet may move or hide (skeletal Matinee
    /// actors; `crate::kismet`).
    actor: Option<u32>,
    /// Index into `NpcScene::skinned`.
    skinned: Option<usize>,
}

fn spawn_one(
    commands: &mut Commands,
    server: &AssetServer,
    mesh: &SkeletalMeshInfo,
    comp: &SkinnedComponent,
    spec: SpawnSpec<'_>,
) -> bool {
    let SpawnSpec {
        kind,
        sequences,
        start,
        name,
        actor,
        skinned,
    } = spec;
    let Some(transform) = component_transform(comp, mesh) else {
        return false;
    };
    let clips = load_clips(server, mesh, sequences);
    let scene: Handle<WorldAsset> = server
        .load_builder()
        .with_settings(skeletal_settings)
        .load(GltfAssetLabel::Scene(0).from_asset(gltf_path(mesh)));
    let mut e = commands.spawn((
        Name::new(format!("npc {name}")),
        WorldAssetRoot(scene),
        transform,
        if comp.hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        },
        NpcSkin {
            skinned,
            ..NpcSkin::new(kind, clips, start, transform)
        },
    ));
    if let Some(id) = actor {
        e.insert(crate::kismet::KismetActor(id));
    }
    true
}

/// Sequence names a villager/Maddie pawn may use (by name; ours).
fn pawn_sequences(mesh: &SkeletalMeshInfo) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["idle", "walk", "talk"] {
        if let Some(a) = mesh
            .animations
            .iter()
            .find(|a| a.sequence.to_ascii_lowercase().contains(key))
        {
            out.push(a.sequence.clone());
        }
    }
    out
}

/// Spawns every skinned actor once the NPC system and skeletal index exist.
pub(super) fn spawn_skins(
    mut commands: Commands,
    world: Option<ResMut<NpcWorld>>,
    sim: Option<Res<Sim>>,
    server: Res<AssetServer>,
) {
    let Some(mut world) = world else {
        return;
    };
    if world.skins_spawned {
        return;
    }
    let world_ref = &*world;
    let Some(system) = world_ref.system(sim.as_deref()) else {
        return;
    };
    let Some(index) = &world_ref.skeletal else {
        world.skins_spawned = true;
        return;
    };
    let scene = system.scene();
    let mut spawned = 0usize;
    let mut missing = 0usize;
    for (ai, actor) in scene.skinned.iter().enumerate() {
        // Every sequence of the mesh may play (Matinee picks them at run
        // time); the clip handles are shared assets.
        for (ci, comp) in actor.components.iter().enumerate() {
            let Some(mesh) = index.get(&comp.mesh) else {
                missing += 1;
                continue;
            };
            let (start, mut sequences): (Option<StartAnim>, Vec<String>) = match &comp.animation {
                Some(h) => (
                    Some(StartAnim {
                        sequence: h.sequence.clone(),
                        time: h.start_time,
                        looping: h.looping,
                        playing: h.playing,
                        rate: h.rate,
                    }),
                    vec![h.sequence.clone()],
                ),
                None => match mesh.idle_animation() {
                    Some(a) => (
                        Some(StartAnim::looping(&a.sequence)),
                        vec![a.sequence.clone()],
                    ),
                    None => (None, Vec::new()),
                },
            };
            if ci == 0 && actor.drive == asamu_game::npc::SkinnedDrive::Matinee {
                sequences.extend(mesh.animations.iter().map(|a| a.sequence.clone()));
            }
            let seqs: Vec<&str> = sequences.iter().map(String::as_str).collect();
            if spawn_one(
                &mut commands,
                &server,
                mesh,
                comp,
                SpawnSpec {
                    kind: SkinKind::Ambient,
                    sequences: &seqs,
                    start,
                    name: &actor.name,
                    actor: Some(actor.id),
                    // The first component follows the simulation's node.
                    skinned: (ci == 0).then_some(ai),
                },
            ) {
                spawned += 1;
            }
        }
    }
    let worm_sequences: Vec<&str> = asamu_game::npc::WormAnim::ALL
        .iter()
        .map(|a| a.sequence())
        .collect();
    for (i, w) in scene.worms.iter().enumerate() {
        for comp in &w.meshes {
            if let Some(mesh) = index.get(&comp.mesh)
                && spawn_one(
                    &mut commands,
                    &server,
                    mesh,
                    comp,
                    SpawnSpec {
                        kind: SkinKind::Worm(i),
                        sequences: &worm_sequences,
                        start: None,
                        name: &w.name,
                        actor: None,
                        skinned: None,
                    },
                )
            {
                spawned += 1;
            }
        }
    }
    for (i, m) in scene.maddies.iter().enumerate() {
        for comp in &m.meshes {
            if let Some(mesh) = index.get(&comp.mesh) {
                let seqs = pawn_sequences(mesh);
                let refs: Vec<&str> = seqs.iter().map(String::as_str).collect();
                spawned += usize::from(spawn_one(
                    &mut commands,
                    &server,
                    mesh,
                    comp,
                    SpawnSpec {
                        kind: SkinKind::Maddie(i),
                        sequences: &refs,
                        start: None,
                        name: &m.name,
                        actor: None,
                        skinned: None,
                    },
                ));
            }
        }
    }
    for (i, v) in scene.villagers.iter().enumerate() {
        for comp in &v.meshes {
            if let Some(mesh) = index.get(&comp.mesh) {
                let seqs = pawn_sequences(mesh);
                let refs: Vec<&str> = seqs.iter().map(String::as_str).collect();
                spawned += usize::from(spawn_one(
                    &mut commands,
                    &server,
                    mesh,
                    comp,
                    SpawnSpec {
                        kind: SkinKind::Villager(i),
                        sequences: &refs,
                        start: None,
                        name: &v.name,
                        actor: None,
                        skinned: None,
                    },
                ));
            }
        }
    }
    info!("skinned NPC meshes: {spawned} spawned, {missing} without a converted mesh");
    world.skins_spawned = true;
}

/// Gives every new `AnimationPlayer` below an [`NpcSkin`] its graph and
/// starts the first animation.
pub(super) fn attach_animations(
    mut commands: Commands,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
    parents: Query<&ChildOf>,
    mut skins: Query<&mut NpcSkin>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    for (entity, mut player) in &mut players {
        let Some(root) = ancestor_with(&parents, entity, |e| skins.contains(e)) else {
            continue;
        };
        let Ok(mut skin) = skins.get_mut(root) else {
            continue;
        };
        if skin.player.is_some() || skin.clips.is_empty() {
            continue;
        }
        let (graph, nodes) = AnimationGraph::from_clips(skin.clips.iter().map(|(_, h)| h.clone()));
        let mut transitions = AnimationTransitions::new();
        if let Some(start) = skin.start.clone()
            && let Some(i) = skin
                .clips
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(&start.sequence))
            && let Some(node) = nodes.get(i)
        {
            let active = transitions.play(&mut player, *node, Duration::ZERO);
            if start.looping {
                active.repeat();
            }
            if start.rate.is_finite() && start.rate != 1.0 {
                active.set_speed(start.rate);
            }
            active.seek_to(start.time.max(0.0));
            if !start.playing {
                // `bPlaying` false: the pose at `CurrentTime`, held.
                active.pause();
            }
            skin.current = Some(start.sequence);
        }
        commands
            .entity(entity)
            .insert((AnimationGraphHandle(graphs.add(graph)), transitions));
        skin.nodes = nodes;
        skin.player = Some(entity);
    }
}

/// The nearest ancestor of `entity` (itself included) matching `pred`.
pub(super) fn ancestor_with(
    parents: &Query<&ChildOf>,
    entity: Entity,
    pred: impl Fn(Entity) -> bool,
) -> Option<Entity> {
    let mut e = entity;
    for _ in 0..64 {
        if pred(e) {
            return Some(e);
        }
        e = parents.get(e).ok()?.parent();
    }
    None
}

/// Switches state-driven skins to the animation their state wants and moves
/// the villager pawns.
pub(super) fn update_skin_animations(
    world: Option<Res<NpcWorld>>,
    sim: Option<Res<Sim>>,
    mut skins: Query<(&mut NpcSkin, &mut Transform)>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    let Some(world) = world else {
        return;
    };
    let Some(system) = world.system(sim.as_deref()) else {
        return;
    };
    let rt = system.runtime();
    let scene = system.scene();
    for (mut skin, mut transform) in &mut skins {
        let (wanted, looping, seek): (Option<String>, bool, Option<f32>) = match skin.kind {
            SkinKind::Ambient => {
                // Only a running game ticks the nodes (fly mode shows the
                // rest state and lets the clips play).
                if sim.is_some() {
                    follow_sim_node(&mut skin, scene, rt, &mut players);
                }
                continue;
            }
            SkinKind::Hands => {
                let Some(sim) = &sim else {
                    continue;
                };
                let anim = hand_animation(sim.game.player());
                let target = match anim.entry() {
                    Some(entry) => {
                        let cur = skin.current.as_deref();
                        if cur == Some(anim.sequence()) {
                            anim
                        } else if cur == Some(entry.sequence()) {
                            // The attach clip plays once, then the loop.
                            let finished = clip_finished(&skin, entry.sequence(), &players);
                            if finished { anim } else { entry }
                        } else {
                            entry
                        }
                    }
                    None => anim,
                };
                // A one-shot release (the gun's flag lasts one tick) plays out
                // unless the hand is attached again or lowered.
                let release = HandAnim::GrappleRelease.sequence();
                let target = if skin.current.as_deref() == Some(release)
                    && !matches!(
                        target,
                        HandAnim::GrappleBegin | HandAnim::GrappleLoop | HandAnim::HandsDown
                    )
                    && !clip_finished(&skin, release, &players)
                {
                    HandAnim::GrappleRelease
                } else {
                    target
                };
                (Some(target.sequence().to_owned()), target.looping(), None)
            }
            SkinKind::Worm(i) => match rt.worms.get(i) {
                Some(w) => {
                    let (seq, pos, looping) = worm_animation(w);
                    (Some(seq.to_owned()), looping, Some(pos))
                }
                None => continue,
            },
            SkinKind::Maddie(i) => match rt.maddies.get(i) {
                Some(m) => {
                    let key = if m.state == MaddieStateName::TalkingWithPlayer {
                        "talk"
                    } else {
                        "idle"
                    };
                    (find_clip(&skin, key), true, None)
                }
                None => continue,
            },
            SkinKind::Villager(i) => match (rt.villagers.get(i), scene.villagers.get(i)) {
                (Some(v), Some(def)) => {
                    // Move with the pawn (render-only offset from the rest pose).
                    let offset = v.location - def.location;
                    transform.translation = skin.base.translation + crate::to_render(offset);
                    let yaw = (v.yaw - def.rotation[1]) as f32 * std::f32::consts::PI / 32_768.0;
                    transform.rotation = Quat::from_rotation_y(-yaw) * skin.base.rotation;
                    let key = if v.moving {
                        "walk"
                    } else if v.state == VillagerStateName::TalkWithPawn {
                        "talk"
                    } else {
                        "idle"
                    };
                    (find_clip(&skin, key), true, None)
                }
                _ => continue,
            },
        };
        let Some(wanted) = wanted else {
            continue;
        };
        if skin.current.as_deref() == Some(wanted.as_str()) {
            continue;
        }
        let Some(entity) = skin.player else {
            continue;
        };
        let Some(i) = skin
            .clips
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(&wanted))
        else {
            continue;
        };
        let Some(node) = skin.nodes.get(i).copied() else {
            continue;
        };
        if let Ok((mut player, mut transitions)) = players.get_mut(entity) {
            let active = transitions.play(&mut player, node, BLEND);
            if looping {
                active.repeat();
            }
            if let Some(t) = seek {
                active.seek_to(t.max(0.0));
            }
            skin.current = Some(wanted);
        }
    }
}

/// How far a playing clip may drift from the simulation's position before
/// it is put back, s (ours, presentation).
const DRIFT: f32 = 0.25;

/// A placed skinned actor follows its simulation node: the sequence it
/// plays, and its position (held nodes every frame; playing ones when they
/// drift). A playing node whose sequence the simulation has no data for
/// (no skeletal manifest when the game loaded) is left to the
/// `AnimationPlayer`.
fn follow_sim_node(
    skin: &mut NpcSkin,
    scene: &asamu_game::npc::NpcScene,
    rt: &asamu_game::npc::NpcRuntime,
    players: &mut Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    let Some(index) = skin.skinned else {
        return;
    };
    let Some(node) = rt.skinned.get(index).and_then(|s| s.current()) else {
        return;
    };
    let (Some(seq), Some(entity)) = (node.sequence.clone(), skin.player) else {
        return;
    };
    let info = scene
        .skinned
        .get(index)
        .and_then(|d| d.components.first())
        .and_then(|c| scene.sequence_info(&c.mesh, &seq));
    if node.playing && info.is_none() {
        return;
    }
    let Some(i) = skin
        .clips
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case(&seq))
    else {
        return;
    };
    let Some(anim_node) = skin.nodes.get(i).copied() else {
        return;
    };
    let Ok((mut player, mut transitions)) = players.get_mut(entity) else {
        return;
    };
    let position = node.position.max(0.0);
    if skin.current.as_deref() != Some(seq.as_str()) {
        let active = transitions.play(&mut player, anim_node, BLEND);
        if node.looping {
            active.repeat();
        }
        active.seek_to(position);
        skin.current = Some(seq);
    }
    let Some(active) = player.animation_mut(anim_node) else {
        return;
    };
    if node.playing {
        if active.is_paused() {
            active.resume();
        }
        let speed = node.rate * info.map_or(1.0, |i| i.rate_scale);
        if speed.is_finite() && active.speed() != speed {
            active.set_speed(speed);
        }
        // Drift, measured around the loop.
        let mut drift = (active.seek_time() - position).abs();
        if let Some(len) = info.map(|i| i.length).filter(|l| node.looping && *l > 0.0) {
            drift = drift.min((len - drift).abs());
        }
        if drift > DRIFT {
            active.seek_to(position);
        }
    } else {
        active.seek_to(position);
        active.pause();
    }
}

/// glTF bone-space vector of a UE3 bone axis (`AXIS_X`, ...): the importer
/// maps UE (x, y, z) to glTF (y, z, −x) (skeletal manifest `coordinates`).
fn bone_axis(axis: &str, invert: bool) -> Vec3 {
    let v = match axis {
        "AXIS_Y" => Vec3::X,
        "AXIS_Z" => Vec3::Y,
        _ => Vec3::NEG_Z,
    };
    if invert { -v } else { v }
}

/// The descendant of `root` named `name` (case-insensitive, bounded walk).
fn find_bone(
    root: Entity,
    name: &str,
    children: &Query<&Children>,
    names: &Query<&Name>,
) -> Option<Entity> {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(e) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            return None;
        }
        if names
            .get(e)
            .is_ok_and(|n| n.as_str().eq_ignore_ascii_case(name))
        {
            return Some(e);
        }
        if let Ok(c) = children.get(e) {
            stack.extend(c.iter());
        }
    }
    None
}

/// Look-at controls (see the module docs).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn apply_look_at(
    world: Option<Res<NpcWorld>>,
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    mut skins: Query<(Entity, &mut NpcSkin)>,
    children: Query<&Children>,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
    globals: Query<&GlobalTransform>,
    mut bones: Query<&mut Transform, Without<NpcSkin>>,
) {
    let (Some(world), Some(sim)) = (world, sim) else {
        return;
    };
    let Some(system) = world.system(Some(&*sim)) else {
        return;
    };
    let scene = system.scene();
    let rt = system.runtime();
    let player = sim.game.player().position;
    let dt = time.delta_secs().clamp(0.0, 0.25);
    for (root, mut skin) in &mut skins {
        // (look-at setup, head target, eye target, target strength override)
        let (def, head_target, eye_target, worm_alpha) = match skin.kind {
            SkinKind::Ambient => {
                let Some(def) = skin
                    .skinned
                    .and_then(|i| scene.skinned.get(i))
                    .and_then(|d| scene.look_at_of(d.id))
                else {
                    continue;
                };
                let state = skin.skinned.and_then(|i| rt.skinned.get(i));
                let (h, e) = state.map_or((sim_glam::Vec3::ZERO, sim_glam::Vec3::ZERO), |s| {
                    (s.look_at.head_offset, s.look_at.eyes_offset)
                });
                (def, player + h, player + e, None)
            }
            SkinKind::Worm(i) => {
                let (Some(w), Some(d)) = (rt.worms.get(i), scene.worms.get(i)) else {
                    continue;
                };
                let Some(def) = scene.look_at_of(d.id) else {
                    continue;
                };
                // `SetWormLookAtAlpha`: 1 while alerted or screaming, else 0
                // (0.3 s blends; CONFIRMED (src)).
                let on = matches!(w.state, WormStateName::Alerted | WormStateName::Screaming);
                (
                    def,
                    w.current_aim,
                    w.current_aim,
                    Some(if on { 1.0 } else { 0.0 }),
                )
            }
            _ => continue,
        };
        let id = def.id;
        for (names_list, target) in [(&def.head, head_target), (&def.eyes, eye_target)] {
            for control_name in names_list {
                let Some(control) = def
                    .controls
                    .iter()
                    .find(|c| c.control.eq_ignore_ascii_case(control_name))
                else {
                    continue;
                };
                let Some(bone_name) = control.bone.as_deref() else {
                    continue;
                };
                let key = bone_name.to_ascii_lowercase();
                let bone = match skin.bones.get(&key) {
                    Some(b) => *b,
                    None => {
                        let Some(b) = find_bone(root, bone_name, &children, &names) else {
                            continue;
                        };
                        skin.bones.insert(key.clone(), b);
                        b
                    }
                };
                let wanted = match worm_alpha {
                    Some(a) => a,
                    None => rt.control_strength(scene, id, &control.control),
                };
                // Blend towards the strength over the control's blend time
                // (the worm's 0.3 s).
                let blend = if worm_alpha.is_some() {
                    0.3
                } else {
                    control.blend_in_time.max(0.0)
                };
                let shown = skin
                    .look_alpha
                    .entry(control.control.to_ascii_lowercase())
                    .or_insert(wanted);
                *shown = if blend > 0.0 {
                    let step = dt / blend;
                    if wanted > *shown {
                        (*shown + step).min(wanted)
                    } else {
                        (*shown - step).max(wanted)
                    }
                } else {
                    wanted
                };
                let strength = shown.clamp(0.0, 1.0);
                if strength <= 0.0 || !target.is_finite() {
                    continue;
                }
                let (Ok(bone_global), Ok(mut local)) = (globals.get(bone), bones.get_mut(bone))
                else {
                    continue;
                };
                let parent_rot = parents
                    .get(bone)
                    .ok()
                    .and_then(|p| globals.get(p.parent()).ok())
                    .map_or(Quat::IDENTITY, |g| g.to_scale_rotation_translation().1);
                let axis = bone_axis(&control.look_at_axis, control.invert_look_at_axis);
                let limit = (control.enable_limit && control.max_angle > 0.0)
                    .then(|| control.max_angle.to_radians());
                if let Some(turned) = look_at_rotation(
                    parent_rot,
                    local.rotation,
                    axis,
                    bone_global.translation(),
                    crate::to_render(target),
                    limit,
                    strength,
                ) {
                    local.rotation = turned;
                }
            }
        }
    }
}

/// The bone's new local rotation for a look-at control: the bone (local
/// rotation `local` under a parent with world rotation `parent`, at
/// `bone_pos`) is turned by the shortest arc that points its `axis` at
/// `target`, by at most `limit` radians, blended by `strength` (0..1).
/// `None` when there is nothing to turn towards or the result would not be
/// finite (a degenerate parent or bone transform must not reach the bone).
fn look_at_rotation(
    parent: Quat,
    local: Quat,
    axis: Vec3,
    bone_pos: Vec3,
    target: Vec3,
    limit: Option<f32>,
    strength: f32,
) -> Option<Quat> {
    let world_rot = parent * local;
    let current = (world_rot * axis).normalize_or_zero();
    let to_target = (target - bone_pos).normalize_or_zero();
    if current == Vec3::ZERO || to_target == Vec3::ZERO {
        return None;
    }
    let mut delta = Quat::from_rotation_arc(current, to_target);
    if let Some(max) = limit {
        let (axis_r, angle) = delta.to_axis_angle();
        if angle > max {
            delta = Quat::from_axis_angle(axis_r, max);
        }
    }
    let delta = Quat::IDENTITY.slerp(delta, strength.clamp(0.0, 1.0));
    let turned = (parent.inverse() * delta * world_rot).normalize();
    turned.is_finite().then_some(turned)
}

fn clip_finished(
    skin: &NpcSkin,
    sequence: &str,
    players: &Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) -> bool {
    let Some(entity) = skin.player else {
        return true;
    };
    let Some(i) = skin
        .clips
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case(sequence))
    else {
        return true;
    };
    let (Some(node), Ok((player, _))) = (skin.nodes.get(i), players.get(entity)) else {
        return true;
    };
    player.animation(*node).is_none_or(|a| a.is_finished())
}

fn find_clip(skin: &NpcSkin, key: &str) -> Option<String> {
    skin.clips
        .iter()
        .find(|(n, _)| n.to_ascii_lowercase().contains(key))
        .map(|(n, _)| n.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bone_axes_follow_the_importers_coordinates() {
        // UE (x, y, z) → glTF (y, z, −x).
        assert_eq!(bone_axis("AXIS_X", false), Vec3::NEG_Z);
        assert_eq!(bone_axis("AXIS_Y", false), Vec3::X);
        assert_eq!(bone_axis("AXIS_Z", false), Vec3::Y);
        assert_eq!(bone_axis("AXIS_Z", true), Vec3::NEG_Y);
        // Anything else is read as the default axis (X).
        assert_eq!(bone_axis("", false), Vec3::NEG_Z);
    }

    #[test]
    fn look_at_turns_the_bone_axis_towards_the_target() {
        let parent = Quat::from_rotation_y(0.7) * Quat::from_rotation_x(-0.2);
        let local = Quat::from_rotation_z(0.3);
        let axis = bone_axis("AXIS_X", false);
        let bone_pos = Vec3::new(1.0, 2.0, 3.0);
        let target = Vec3::new(4.0, 2.5, -1.0);
        let world_axis = |local: Quat| (parent * local) * axis;
        let want = (target - bone_pos).normalize();
        // Full strength, no limit: the axis points at the target.
        let full = look_at_rotation(parent, local, axis, bone_pos, target, None, 1.0).unwrap();
        assert!((world_axis(full) - want).length() < 1e-5);
        assert!(full.is_normalized());
        // Half strength: half the angle.
        let angle = world_axis(local).angle_between(want);
        let half = look_at_rotation(parent, local, axis, bone_pos, target, None, 0.5).unwrap();
        assert!((world_axis(half).angle_between(want) - angle * 0.5).abs() < 1e-4);
        // No strength: unchanged.
        let none = look_at_rotation(parent, local, axis, bone_pos, target, None, 0.0).unwrap();
        assert!(none.angle_between(local) < 1e-5);
        // Limited: the bone turns by the limit only.
        let limit = 0.2f32;
        assert!(angle > limit);
        let limited =
            look_at_rotation(parent, local, axis, bone_pos, target, Some(limit), 1.0).unwrap();
        assert!((world_axis(limited).angle_between(world_axis(local)) - limit).abs() < 1e-4);
        assert!((world_axis(limited).angle_between(want) - (angle - limit)).abs() < 1e-4);
        // A limit wider than the turn changes nothing.
        let wide = look_at_rotation(parent, local, axis, bone_pos, target, Some(3.0), 1.0).unwrap();
        assert!(wide.angle_between(full) < 1e-5);
        // Strength is clamped.
        let over = look_at_rotation(parent, local, axis, bone_pos, target, None, 7.0).unwrap();
        assert!(over.angle_between(full) < 1e-5);
    }

    #[test]
    fn look_at_refuses_degenerate_input() {
        let axis = Vec3::NEG_Z;
        let q = Quat::IDENTITY;
        // The target on the bone: no direction.
        assert!(look_at_rotation(q, q, axis, Vec3::ONE, Vec3::ONE, None, 1.0).is_none());
        // Non-finite transforms or targets never reach the bone.
        let nan = Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0);
        assert!(look_at_rotation(nan, q, axis, Vec3::ZERO, Vec3::X, None, 1.0).is_none());
        assert!(look_at_rotation(q, nan, axis, Vec3::ZERO, Vec3::X, None, 1.0).is_none());
        assert!(
            look_at_rotation(q, q, axis, Vec3::ZERO, Vec3::splat(f32::NAN), None, 1.0).is_none()
        );
        assert!(
            look_at_rotation(q, q, axis, Vec3::splat(f32::INFINITY), Vec3::X, None, 1.0).is_none()
        );
        assert!(look_at_rotation(q, q, Vec3::ZERO, Vec3::ZERO, Vec3::X, None, 1.0).is_none());
        // Looking straight back (the arc's worst case) still gives a
        // finite, unit rotation.
        let back = look_at_rotation(q, q, axis, Vec3::ZERO, Vec3::Z, None, 1.0).unwrap();
        assert!(back.is_finite() && back.is_normalized());
        assert!(((back * axis) - Vec3::Z).length() < 1e-5);
    }
}
