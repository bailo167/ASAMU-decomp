//! Render-only fly camera for converted levels (`--fly`, and while the
//! level's game is loading or when it could not be loaded). Its systems stop
//! once the simulation (`Sim`) exists.
//!
//! The camera state is kept in UE3 coordinates (UU, yaw/pitch radians), like
//! the simulation, and converted to render space with `asamu_core::coords`
//! once per frame. Not gameplay: no collision, no physics.
//!
//! Controls: click to capture the mouse, mouse look, WASD move along the view,
//! Space / E up, Left Ctrl / Q down, hold Left Shift for 4x speed, mouse wheel
//! changes speed, Esc releases the mouse.

use asamu_core::coords::{ue_right_flat, ue_view_direction, ue_view_to_bevy_rotation};
use asamu_core::glam as sim_glam;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::{MOUSE_RADIANS_PER_COUNT, PlayerCamera, Sim, bevy_quat, to_render};

/// Fly camera state (UE3 space).
#[derive(Resource, Debug, Clone, Copy)]
pub struct FlyCam {
    /// Eye position (UU).
    pub position: sim_glam::Vec3,
    /// Yaw (radians, UE3 convention: positive turns right).
    pub yaw: f32,
    /// Pitch (radians, positive looks up).
    pub pitch: f32,
    /// Base speed (UU/s).
    pub speed: f32,
}

impl Default for FlyCam {
    fn default() -> Self {
        Self {
            position: sim_glam::Vec3::new(0.0, 0.0, 200.0),
            yaw: 0.0,
            pitch: 0.0,
            speed: 600.0,
        }
    }
}

/// Fly camera systems.
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyCam>().add_systems(
            Update,
            (fly_cursor, fly_move, fly_sync_camera)
                .chain()
                .run_if(not(resource_exists::<Sim>)),
        );
    }
}

fn fly_cursor(
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    let grabbed = cursor.grab_mode != CursorGrabMode::None;
    if !grabbed && mouse.just_pressed(MouseButton::Left) {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    } else if grabbed && keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

fn axis(keys: &ButtonInput<KeyCode>, positive: &[KeyCode], negative: &[KeyCode]) -> f32 {
    let pos = positive.iter().any(|k| keys.pressed(*k));
    let neg = negative.iter().any(|k| keys.pressed(*k));
    f32::from(u8::from(pos)) - f32::from(u8::from(neg))
}

fn fly_move(
    time: Res<Time>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cam: ResMut<FlyCam>,
) {
    if cursor.grab_mode == CursorGrabMode::None {
        return;
    }
    let max_pitch = 89.0_f32.to_radians();
    cam.yaw += motion.delta.x * MOUSE_RADIANS_PER_COUNT;
    cam.pitch = (cam.pitch - motion.delta.y * MOUSE_RADIANS_PER_COUNT).clamp(-max_pitch, max_pitch);
    if scroll.delta.y != 0.0 {
        cam.speed =
            (cam.speed * 1.2_f32.powf(scroll.delta.y.clamp(-10.0, 10.0))).clamp(25.0, 50_000.0);
    }
    let forward = ue_view_direction(cam.yaw, cam.pitch);
    let right = ue_right_flat(cam.yaw);
    let f = axis(
        &keys,
        &[KeyCode::KeyW, KeyCode::ArrowUp],
        &[KeyCode::KeyS, KeyCode::ArrowDown],
    );
    let r = axis(
        &keys,
        &[KeyCode::KeyD, KeyCode::ArrowRight],
        &[KeyCode::KeyA, KeyCode::ArrowLeft],
    );
    let u = axis(
        &keys,
        &[KeyCode::Space, KeyCode::KeyE],
        &[KeyCode::ControlLeft, KeyCode::KeyQ],
    );
    let mut dir = forward * f + right * r + sim_glam::Vec3::Z * u;
    if dir.length_squared() > 1.0 {
        dir = dir.normalize();
    }
    let boost = if keys.pressed(KeyCode::ShiftLeft) {
        4.0
    } else {
        1.0
    };
    let step = dir * cam.speed * boost * time.delta_secs();
    cam.position += step;
}

fn fly_sync_camera(cam: Res<FlyCam>, mut camera: Single<&mut Transform, With<PlayerCamera>>) {
    camera.translation = to_render(cam.position);
    camera.rotation = bevy_quat(ue_view_to_bevy_rotation(cam.yaw, cam.pitch));
}
