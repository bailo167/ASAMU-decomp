//! Placed skinned actors: villagers and Maddie as `SkeletalMeshActor`s, the
//! worm and the (never placed) NPC pawns, from the importer's skeletal glTF
//! files, with Bevy skins and an `AnimationPlayer`.
//!
//! Animation choice: the worm follows its state machine (the active node of
//! its emulated `AnimTree`); an ambient actor plays its component's own node
//! (`AnimSeqName`, looping, start time) when the scene provides it, else the
//! mesh's first idle-named sequence (ours); Matinee-driven actors play the
//! same until Matinee animation tracks are imported; villager and Maddie pawns
//! pick idle/walk/talk sequences by name from their state (ours: their
//! `AnimTree` nodes carry no sequence names).

use std::time::Duration;

use asamu_assets::transform::{decompose, instance_matrix, ue_row_matrix_to_mat4};
use asamu_game::npc::defs::{MaddieStateName, SkinnedComponent};
use asamu_game::npc::{
    HandAnim, SkeletalMeshInfo, VillagerStateName, hand_animation, worm_animation,
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
        NpcSkin::new(kind, clips, start, transform),
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
    for actor in &scene.skinned {
        for comp in &actor.components {
            let Some(mesh) = index.get(&comp.mesh) else {
                missing += 1;
                continue;
            };
            let (start, sequences): (Option<StartAnim>, Vec<String>) = match &comp.animation {
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
            SkinKind::Ambient => continue,
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
