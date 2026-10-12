//! First-person hands (`PlayerHand.Meshes.PlayerHand` with the `PlayerHand.Root`
//! animations), the grapple gun's `FirstPersonMesh`.
//!
//! The original draws that mesh in the foreground depth group with its own
//! FOV (70°, horizontal) at the eye with the controller's rotation; the gun's
//! position offsets are zero (never written), so the mesh's own geometry
//! places the hand (CONFIRMED (cdo, src)). Here: an overlay camera on its own
//! render layer, child of the player camera (so it follows it), clearing only
//! depth, with the mesh as its child in camera-local axes. The hand bob of
//! ABILITIES.md A-CM-5 moves it ([`hand_bob_offset`]). The overlay is lit by
//! the level's lights (they are on the overlay layer as well) plus a weak
//! fill light of our own; its materials are the converted ones
//! ([`super::materials`]). The level's post-processing (tonemapper, colour
//! grading, fog) is not applied to the overlay.
//!
//! # When the hand is drawn
//!
//! The original always has the gun and its hand mesh (the owner's recording
//! of the Workshop shows the gun from the first frame, with capacity 0);
//! whether the hand is on screen is decided by the hand's animation tree and
//! one flag (GRAPPLE.md G-AC-3, NPCS.md §11):
//!
//! - the tree's `bHidden` blend list switches between the gameplay branch
//!   (idle, sprint, grapple ...) and a "lowered" branch that holds the
//!   `HandsDown` pose, with the `SuitOn` sequence as a one-shot inside that
//!   lowered branch (CONFIRMED (data): the tree's node links);
//! - the gun's hide function sets the list to "lowered" and its show function
//!   to "gameplay"; entering story mode calls hide (keeping the mesh visible)
//!   and leaving it calls show; Kismet's toggle-visible-grapple action calls
//!   either, and with its visibility option off the hide also hides the mesh
//!   itself and sets the "hand hidden" flag (CONFIRMED (src));
//! - Kismet's play-suit-on action starts the `SuitOn` one-shot, which is seen
//!   only while the hand is lowered (CONFIRMED (data)).
//!
//! So the Workshop, which enters story mode at level start, shows no hand
//! until the suit is taken; then the suit-on animation; then no hand again
//! until the level ends. [`HandPose`] follows that from state the simulation
//! exposes ([`HandObservation`]). What it has to work around: the simulation
//! keeps the "hand hidden" flag but not the lowered state (a hide that keeps
//! the mesh visible leaves no trace in it), so the lowered state is rebuilt
//! here from the story-mode edges, the flag's edges and the activation
//! counts of the level script's toggle actions (see [`toggle_actions`] for
//! the one case that cannot be told apart).
//!
//! PLACEHOLDER (presentation): a lowered hand is not drawn at all, where the
//! original blends to the `HandsDown` pose over 0.3 s (or at once); and
//! during the suit-on one-shot the hand is drawn with its ordinary animation
//! for the length of the `SuitOn` sequence, because the hand's animation
//! selection (`asamu_game::npc::hand_animation`, `skins.rs`) has no suit-on
//! state yet.

use asamu_core::coords::ue_dir_to_bevy;
use asamu_game::asamu_kismet::{Graph, OpClass, Output};
use asamu_game::npc::{HAND_MESH, HAND_MESH_FOV, HandAnim, hand_bob_offset};
use bevy::camera::ClearColorConfig;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::NpcWorld;
use super::skins::{
    NpcSkin, SkinKind, StartAnim, ancestor_with, gltf_path, load_clips, skeletal_settings,
};
use crate::{PlayerCamera, Sim, bevy_vec};

/// Render layer of the first-person overlay (ours).
const OVERLAY_LAYER: usize = crate::FIRST_PERSON_LAYER;

/// Illuminance of the overlay's own fill light, lux (ours, presentation): a
/// weak light from behind the camera so the hands keep their shape where no
/// level light reaches them. The level's lights do the real lighting (they
/// are on the overlay layer too, `converted.rs`).
const FILL_LUX: f32 = 120.0;

/// The overlay camera.
#[derive(Component)]
pub(super) struct HandCamera;

/// Root of an overlay scene (its meshes go to the overlay layer).
#[derive(Component)]
pub(super) struct OverlayRoot;

/// The hand mesh root (camera-local placement).
#[derive(Component)]
pub(super) struct HandRoot {
    /// Render units per glTF unit.
    scale: f32,
}

/// `AnimSeqName` of the suit-on one-shot (CONFIRMED (data)).
const SUIT_ON_SEQUENCE: &str = "SuitOn";

/// Which inputs of a toggle-visible-grapple action the level script wires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToggleWiring {
    /// Only "Hide" (input 0): every activation lowers the hand.
    Hide,
    /// Only "Show" (input 1): every activation raises it.
    Show,
    /// Both: an activation count does not say which input fired.
    Both,
}

/// The toggle-visible-grapple actions of a level script and which of their
/// two inputs ("Hide", "Show") any enabled output link reaches.
///
/// The presentation reads these because the simulation only keeps the
/// result of a hide that also switches the mesh off (`hand_hidden`). An
/// action wired on both inputs cannot be followed by its activation count;
/// when its visibility option is off the flag's edges still tell (the one
/// such action in the shipped maps, in AG-Darkcave, is of that kind), and
/// otherwise its activations are ignored (no shipped map has one: of the 8
/// actions in 4 maps, 5 are hide-only, 2 hide-only or show-only with the
/// mesh kept visible, 1 is wired on both with visibility off).
pub(super) fn toggle_actions(graph: &Graph) -> Vec<(usize, ToggleWiring)> {
    let mut found: Vec<(usize, bool, bool)> = graph
        .nodes
        .iter()
        .filter(|n| n.class == OpClass::ToggleVisibleGrapple)
        .map(|n| (n.id, false, false))
        .collect();
    if found.is_empty() {
        return Vec::new();
    }
    for node in &graph.nodes {
        for output in node.outputs.iter().filter(|o| !o.disabled) {
            for &(op, input) in &output.links {
                if let Some(action) = found.iter_mut().find(|a| a.0 == op) {
                    match input {
                        0 => action.1 = true,
                        1 => action.2 = true,
                        _ => {}
                    }
                }
            }
        }
    }
    found
        .into_iter()
        .filter_map(|(op, hide, show)| match (hide, show) {
            (true, false) => Some((op, ToggleWiring::Hide)),
            (false, true) => Some((op, ToggleWiring::Show)),
            (true, true) => Some((op, ToggleWiring::Both)),
            (false, false) => None,
        })
        .collect()
}

/// What one frame sees of the simulation, for [`HandPose`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HandObservation {
    /// The pawn is in story mode (`StoryState` or `Zooming`).
    pub story: bool,
    /// The gun's "hand hidden" flag (G-AC-3): the mesh itself is off.
    pub hard_hidden: bool,
    /// New activations of hide-only toggle actions.
    pub hides: u32,
    /// New activations of show-only toggle actions.
    pub shows: u32,
    /// Kismet started the suit-on one-shot; its length in simulation ticks.
    pub suit_on: Option<u64>,
    /// The simulation's tick.
    pub tick: u64,
}

/// Whether the first-person hand is on screen (see the module docs).
#[derive(Resource, Debug, Default)]
pub(crate) struct HandPose {
    /// The tree's `bHidden` list is on its lowered branch.
    lowered: bool,
    /// Story mode at the last observation (`None`: nothing observed yet).
    story: Option<bool>,
    /// The "hand hidden" flag at the last observation.
    hard_hidden: bool,
    /// The suit-on one-shot runs until this tick.
    suit_on_until: Option<u64>,
    /// The tick of the last observation.
    tick: u64,
    /// The level script's toggle actions with the activation count seen.
    toggles: Vec<(usize, ToggleWiring, u32)>,
}

impl HandPose {
    /// Applies one frame's observation. Story-mode edges first, then the
    /// flag's edges, then Kismet's toggles: a toggle is the most direct
    /// order to the hand, so it wins a frame in which several happen.
    pub(super) fn observe(&mut self, o: HandObservation) {
        if self.story != Some(o.story) {
            // Entering story mode lowers the hand, leaving it raises it; a
            // game first seen outside story mode starts raised.
            self.lowered = o.story;
            self.story = Some(o.story);
        }
        if o.hard_hidden != self.hard_hidden {
            // A hide with the mesh switched off also lowers; a show raises.
            self.lowered = o.hard_hidden;
            self.hard_hidden = o.hard_hidden;
        }
        match (o.hides > 0, o.shows > 0) {
            (true, false) => self.lowered = true,
            (false, true) => self.lowered = false,
            // Both in one frame: their order is not known here.
            _ => {}
        }
        self.tick = o.tick;
        if let Some(length) = o.suit_on {
            self.suit_on_until = Some(o.tick.saturating_add(length));
        }
        if self.suit_on_until.is_some_and(|until| o.tick >= until) {
            self.suit_on_until = None;
        }
    }

    /// The suit-on one-shot is running.
    #[must_use]
    pub(super) fn suit_on(&self) -> bool {
        self.suit_on_until.is_some_and(|until| self.tick < until)
    }

    /// The hand is drawn: the mesh is not switched off, and the hand is
    /// either raised or showing the suit-on one-shot of the lowered branch.
    #[must_use]
    pub(crate) fn shown(&self) -> bool {
        !self.hard_hidden && (!self.lowered || self.suit_on())
    }
}

/// Length of the suit-on one-shot in simulation ticks (at least one), from
/// the converted animation's own length and rate; `None` when the sequence
/// is not converted or its numbers are unusable.
fn suit_on_ticks(length: f32, rate_scale: f32, tick_rate_hz: f64) -> Option<u64> {
    let seconds = f64::from(length) / f64::from(rate_scale);
    let ticks = (seconds * tick_rate_hz).round();
    (seconds > 0.0 && ticks.is_finite() && ticks <= 1.0e6).then_some(ticks.max(1.0) as u64)
}

/// Spawns the overlay camera and the hands once the skeletal index and the
/// player camera exist.
pub(super) fn spawn_hands(
    mut commands: Commands,
    world: Option<Res<NpcWorld>>,
    server: Res<AssetServer>,
    camera: Query<Entity, With<PlayerCamera>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let (Some(world), Some(cam)) = (world, camera.iter().next()) else {
        return;
    };
    *done = true;
    let Some(mesh) = world.skeletal.as_ref().and_then(|i| i.get(HAND_MESH)) else {
        info!("first-person hands: {HAND_MESH} is not converted");
        return;
    };
    let sequences: Vec<&str> = [
        HandAnim::HandsDown,
        HandAnim::GrappleBegin,
        HandAnim::GrappleLoop,
        HandAnim::GrappleRelease,
        HandAnim::RocketBoots,
        HandAnim::PowerJumpIdle,
        HandAnim::JumpIdle,
        HandAnim::Sprint,
        HandAnim::Idle,
    ]
    .iter()
    .map(|a| a.sequence())
    .collect();
    let clips = load_clips(&server, mesh, &sequences);
    let scene: Handle<WorldAsset> = server
        .load_builder()
        .with_settings(skeletal_settings)
        .load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(gltf_path(mesh)));
    let scale = crate::SCALE.bevy_units_per_uu / mesh.scale;
    let base = Transform::from_scale(Vec3::splat(scale));
    let start = Some(StartAnim::looping(HandAnim::Idle.sequence()));
    commands.entity(cam).with_children(|c| {
        c.spawn((
            Name::new("first-person overlay camera"),
            HandCamera,
            Camera3d::default(),
            Camera {
                order: 1,
                clear_color: ClearColorConfig::None,
                ..default()
            },
            Projection::from(PerspectiveProjection {
                near: 0.01,
                far: 100.0,
                ..default()
            }),
            RenderLayers::layer(OVERLAY_LAYER),
            Transform::IDENTITY,
        ))
        .with_children(|h| {
            h.spawn((
                Name::new("first-person hands"),
                HandRoot { scale },
                OverlayRoot,
                NpcSkin::new(SkinKind::Hands, clips, start, base),
                WorldAssetRoot(scene),
                base,
                Visibility::Hidden,
            ));
            h.spawn((
                Name::new("first-person overlay light"),
                DirectionalLight {
                    illuminance: FILL_LUX,
                    shadow_maps_enabled: false,
                    ..default()
                },
                RenderLayers::layer(OVERLAY_LAYER),
                Transform::from_xyz(0.0, 1.0, 1.0).looking_at(Vec3::new(0.3, -0.2, -1.0), Vec3::Y),
            ));
        });
    });
    info!(
        "first-person hands: {HAND_MESH} ({} animations)",
        mesh.animations.len()
    );
}

/// Puts the meshes of overlay scenes on the overlay layer as they spawn.
pub(super) fn tag_overlay_meshes(
    mut commands: Commands,
    meshes: Query<Entity, Added<Mesh3d>>,
    parents: Query<&ChildOf>,
    roots: Query<(), With<OverlayRoot>>,
) {
    for e in &meshes {
        if ancestor_with(&parents, e, |a| roots.contains(a)).is_some() {
            commands.entity(e).insert((
                RenderLayers::layer(OVERLAY_LAYER),
                NoFrustumCulling,
                NotShadowCaster,
            ));
        }
    }
}

/// Follows the simulation for [`HandPose`]: story mode, the gun's flag, the
/// level script's toggle actions and the suit-on one-shot.
pub(super) fn observe_hand(
    sim: Option<Res<Sim>>,
    world: Option<Res<NpcWorld>>,
    mut frames: MessageReader<crate::kismet::KismetFrame>,
    mut pose: ResMut<HandPose>,
) {
    let Some(sim) = sim else {
        frames.clear();
        if pose.story.is_some() {
            *pose = HandPose::default();
        }
        return;
    };
    let runtime = sim.script.as_ref().map(asamu_game::LevelScript::runtime);
    if sim.is_added() {
        // A new game (level start, level change): nothing carries over.
        *pose = HandPose::default();
        if let Some(rt) = runtime {
            pose.toggles = toggle_actions(rt.graph())
                .into_iter()
                .map(|(op, wiring)| (op, wiring, rt.activate_count(op)))
                .collect();
        }
    }
    let (mut hides, mut shows) = (0u32, 0u32);
    if let Some(rt) = runtime {
        for (op, wiring, seen) in &mut pose.toggles {
            let count = rt.activate_count(*op);
            let new = count.saturating_sub(*seen);
            *seen = count;
            match wiring {
                ToggleWiring::Hide => hides = hides.saturating_add(new),
                ToggleWiring::Show => shows = shows.saturating_add(new),
                ToggleWiring::Both => {}
            }
        }
    }
    let started = frames
        .read()
        .flat_map(|f| f.outputs.iter())
        .any(|o| matches!(o, Output::SuitOnAnimation));
    let game = &sim.game;
    let suit_on = started
        .then(|| {
            let mesh = world.as_ref()?.skeletal.as_ref()?.get(HAND_MESH)?;
            let anim = mesh.animation(SUIT_ON_SEQUENCE)?;
            suit_on_ticks(anim.length, anim.rate_scale, game.clock().tick_rate_hz())
        })
        .flatten();
    let gun = &game.player().script.gun;
    pose.observe(HandObservation {
        story: game.in_story_mode(),
        hard_hidden: gun.hand_hidden,
        hides,
        shows,
        suit_on,
        tick: game.clock().tick(),
    });
}

/// Overlay FOV (70° horizontal for the window's aspect), hand bob, and
/// visibility: only while the simulation drives the camera, and only while
/// [`HandPose`] says the hand is on screen.
pub(super) fn update_hands(
    sim: Option<Res<Sim>>,
    pose: Option<Res<HandPose>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut cams: Query<&mut Projection, With<HandCamera>>,
    mut hands: Query<(&HandRoot, &mut Transform, &mut Visibility)>,
) {
    if let Some(w) = window.iter().next() {
        let aspect = (w.width() / w.height().max(1.0)).max(0.1);
        for mut p in &mut cams {
            if let Projection::Perspective(p) = p.as_mut() {
                p.fov = 2.0 * ((HAND_MESH_FOV.to_radians() * 0.5).tan() / aspect).atan();
            }
        }
    }
    let shown = pose.as_ref().is_none_or(|p| p.shown());
    for (root, mut transform, mut visibility) in &mut hands {
        let Some(sim) = &sim else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let wanted = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
        let player = sim.game.player();
        // World-space bob → camera-local (inverse of the view rotation).
        let bob =
            bevy_vec(ue_dir_to_bevy(hand_bob_offset(player))) * crate::SCALE.bevy_units_per_uu;
        let view = asamu_core::coords::ue_view_to_bevy_rotation(player.yaw, player.pitch);
        let local = crate::bevy_quat(view).inverse() * bob;
        transform.translation = local;
        transform.scale = Vec3::splat(root.scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(story: bool, hard_hidden: bool, tick: u64) -> HandObservation {
        HandObservation {
            story,
            hard_hidden,
            tick,
            ..HandObservation::default()
        }
    }

    #[test]
    fn a_game_outside_story_mode_shows_the_hand() {
        let mut pose = HandPose::default();
        pose.observe(seen(false, false, 0));
        assert!(pose.shown());
        // Story mode lowers it (a talk in the village), leaving raises it.
        pose.observe(seen(true, false, 10));
        assert!(!pose.shown());
        pose.observe(seen(true, false, 500));
        assert!(!pose.shown());
        pose.observe(seen(false, false, 501));
        assert!(pose.shown());
    }

    /// The Workshop as its level script runs it: story mode from the level
    /// start, the suit-on one-shot after the suit's Matinee, a hide that
    /// keeps the mesh 15 s after the fade-in, a hide that switches the mesh
    /// off at the launch.
    #[test]
    fn the_workshop_shows_the_hand_only_for_the_suit_on_animation() {
        let mut pose = HandPose::default();
        pose.observe(seen(true, false, 0));
        assert!(!pose.shown(), "no glove before the suit is taken");
        pose.observe(seen(true, false, 3000));
        assert!(!pose.shown());
        // Kismet: the hide that keeps the mesh (already lowered), then the
        // suit-on one-shot of 5.97 s at 60 ticks per second.
        pose.observe(HandObservation {
            hides: 1,
            ..seen(true, false, 3100)
        });
        assert!(!pose.shown());
        let length = suit_on_ticks(5.966_666_7, 1.0, 60.0).unwrap();
        assert_eq!(length, 358);
        pose.observe(HandObservation {
            suit_on: Some(length),
            ..seen(true, false, 3200)
        });
        assert!(pose.shown() && pose.suit_on());
        pose.observe(seen(true, false, 3200 + length - 1));
        assert!(pose.shown());
        pose.observe(seen(true, false, 3200 + length));
        assert!(!pose.shown(), "lowered again when the one-shot ends");
        // The launch: mesh off.
        pose.observe(HandObservation {
            hides: 1,
            ..seen(true, true, 9000)
        });
        assert!(!pose.shown());
    }

    #[test]
    fn kismet_toggles_and_the_hidden_flag_move_the_hand() {
        let mut pose = HandPose::default();
        pose.observe(seen(false, false, 0));
        // A hide that keeps the mesh, outside story mode: seen only through
        // the action's activation.
        pose.observe(HandObservation {
            hides: 1,
            ..seen(false, false, 1)
        });
        assert!(!pose.shown());
        pose.observe(HandObservation {
            shows: 1,
            ..seen(false, false, 2)
        });
        assert!(pose.shown());
        // The mesh switched off and on again (an action wired on both
        // inputs is followed through the flag alone).
        pose.observe(seen(false, true, 3));
        assert!(!pose.shown());
        pose.observe(seen(false, false, 4));
        assert!(pose.shown());
        // A show while in story mode raises the hand although the pawn
        // stays in story mode ...
        pose.observe(seen(true, false, 5));
        assert!(!pose.shown());
        pose.observe(HandObservation {
            shows: 1,
            ..seen(true, false, 6)
        });
        assert!(pose.shown());
        // ... and a hide and a show in one frame change nothing.
        pose.observe(HandObservation {
            hides: 1,
            shows: 1,
            ..seen(true, false, 7)
        });
        assert!(pose.shown());
        // The mesh switched off beats the suit-on one-shot.
        pose.observe(HandObservation {
            suit_on: Some(100),
            ..seen(true, true, 8)
        });
        assert!(pose.suit_on() && !pose.shown());
    }

    #[test]
    fn suit_on_lengths_are_checked() {
        assert_eq!(suit_on_ticks(1.0, 1.0, 60.0), Some(60));
        assert_eq!(suit_on_ticks(1.0, 2.0, 60.0), Some(30));
        assert_eq!(suit_on_ticks(0.001, 1.0, 60.0), Some(1));
        for (length, rate) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (1.0, 0.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
            (1.0e9, 1.0),
        ] {
            assert_eq!(suit_on_ticks(length, rate, 60.0), None, "{length} {rate}");
        }
    }

    /// A node of a test graph: its class, the links of its one output as
    /// `(op, input)`, and whether that output is disabled.
    type NodeSpec<'a> = (&'a str, &'a [(usize, usize)], bool);

    /// The JSON of a runtime graph of our own; every node has two inputs.
    fn graph_json(nodes: &[NodeSpec]) -> String {
        let nodes: Vec<String> = nodes
            .iter()
            .enumerate()
            .map(|(id, (class, links, disabled))| {
                let links: Vec<String> = links
                    .iter()
                    .map(|(op, input)| format!(r#"{{"op": {op}, "input": {input}}}"#))
                    .collect();
                format!(
                    r#"{{"id": {id}, "path": "Map.TheWorld.PersistentLevel.Main_Sequence.N{id}",
                        "class": "{class}", "kind": "action",
                        "inputs": [{{"desc": "Hide"}}, {{"desc": "Show"}}],
                        "outputs": [{{"desc": "Out", "disabled": {disabled},
                                      "links": [{}]}}]}}"#,
                    links.join(", ")
                )
            })
            .collect();
        format!(
            r#"{{"format": "{}", "version": {}, "package": "Map", "nodes": [{}]}}"#,
            asamu_game::asamu_kismet::RUNTIME_FORMAT,
            asamu_game::asamu_kismet::RUNTIME_VERSION,
            nodes.join(", ")
        )
    }

    /// A graph of our own: one action fed on "Hide", one on "Show", one on
    /// both, one fed by a disabled link only, one fed by nothing; links to a
    /// missing node and to a missing input are dropped by the graph reader.
    #[test]
    fn toggle_actions_are_read_from_the_graph() {
        let toggle = "asamu.SeqAct_ToggleVisibleGrapple";
        let gate = "Engine.SeqAct_Gate";
        let json = graph_json(&[
            (toggle, &[], false),
            (toggle, &[], false),
            (toggle, &[], false),
            (toggle, &[], false),
            (toggle, &[], false),
            (gate, &[(0, 0), (1, 1), (2, 0)], false),
            (gate, &[(2, 1), (9, 0), (0, 7)], false),
            (gate, &[(3, 0)], true),
        ]);
        let graph = Graph::from_json_slice(json.as_bytes()).unwrap();
        assert_eq!(
            toggle_actions(&graph),
            vec![
                (0, ToggleWiring::Hide),
                (1, ToggleWiring::Show),
                (2, ToggleWiring::Both)
            ]
        );
        // A graph without the action finds nothing.
        let json = graph_json(&[(gate, &[(0, 0)], false)]);
        let graph = Graph::from_json_slice(json.as_bytes()).unwrap();
        assert!(toggle_actions(&graph).is_empty());
    }
}
