//! First-person hands (`PlayerHand.Meshes.PlayerHand` with the `PlayerHand.Root`
//! animations), the grapple gun's `FirstPersonMesh`.
//!
//! The original draws that mesh in the foreground depth group with its own
//! FOV (70°, horizontal) at the eye with the controller's rotation; the gun's
//! position offsets are zero (never written), so the mesh's own geometry
//! places the hand (CONFIRMED (cdo, src)). Here: an overlay camera on its own
//! render layer, child of the player camera (so it follows it), clearing only
//! depth, with the mesh as its child in camera-local axes. The hand bob of
//! ABILITIES.md A-CM-5 moves it ([`hand_bob_offset`]). Lighting of the overlay
//! is our own (one directional light on the overlay layer).

use asamu_core::coords::ue_dir_to_bevy;
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
const OVERLAY_LAYER: usize = 1;

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
                    illuminance: 4_000.0,
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

/// Overlay FOV (70° horizontal for the window's aspect), hand bob, and
/// visibility (only while the simulation drives the camera).
pub(super) fn update_hands(
    sim: Option<Res<Sim>>,
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
    for (root, mut transform, mut visibility) in &mut hands {
        let Some(sim) = &sim else {
            *visibility = Visibility::Hidden;
            continue;
        };
        *visibility = Visibility::Inherited;
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
