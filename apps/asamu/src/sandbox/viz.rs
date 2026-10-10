//! Visualisers drawn with the `SandboxGizmos` group: velocity, collision
//! cylinder, aim ray and grapple range, trail, earlier attempts, predicted
//! arc, arena markers.
//!
//! Lines only, in the Sandbox's own gizmo group, so the level gizmos and the
//! default group are left alone. Everything is read through the session's
//! `Inspection` and telemetry; nothing here changes the game. The predicted
//! arc runs `asamu_sandbox::predict` (the game's own step function on a copy
//! of the player) and is therefore a statement about this recreation on a
//! static copy of the world, not about the original.
//!
//! The camera is first person, so what belongs to the player is drawn where
//! it can be seen: the velocity arrow a few steps ahead and below eye level,
//! the trail at foot level.

use asamu_core::coords::{ue_forward_flat, ue_right_flat};
use asamu_core::glam as sim_glam;
use asamu_player::InputFrame;
use asamu_sandbox::inspect::{AimView, PlayerView};
use asamu_sandbox::predict::predict;
use bevy::prelude::*;

use super::arena_view::ArenaScene;
use super::{Lab, LabSet, SandboxGizmos, VizSettings, inspection, lab_active};
use crate::{PlayerCamera, ROPE_HAND_DOWN_UU, ROPE_HAND_RIGHT_UU, Sim, to_render};

// Presentation constants (ours).
/// How far ahead of the eye the velocity arrow starts, UU.
const ARROW_AHEAD: f32 = 170.0;
/// How far below the eye the velocity arrow starts, UU.
const ARROW_BELOW: f32 = 46.0;
/// Seconds of travel the velocity arrow shows.
const ARROW_SECONDS: f32 = 0.2;
/// Ticks the predicted arc looks ahead.
const PREDICT_TICKS: usize = 150;
/// The arc is computed again after this many ticks ...
const PREDICT_EVERY_TICKS: u64 = 6;
/// ... or frames (so it follows tuning while time is frozen).
const PREDICT_EVERY_FRAMES: u32 = 20;
/// Arena markers farther than this from the player are not drawn, UU.
const MARKER_RANGE: f32 = 6000.0;

const VELOCITY: Color = Color::srgb(1.0, 0.9, 0.2);
const HORIZONTAL: Color = Color::srgb(0.3, 0.9, 1.0);
const FAINT: Color = Color::srgba(0.8, 0.8, 0.8, 0.6);
const CYLINDER: Color = Color::srgb(0.95, 0.95, 0.95);
const FLOOR_NORMAL: Color = Color::srgb(1.0, 0.4, 0.9);
const CAN_GRAPPLE: Color = Color::srgb(0.2, 1.0, 0.35);
const NO_GRAPPLE: Color = Color::srgb(1.0, 0.3, 0.25);
const NOTHING_HIT: Color = Color::srgba(0.75, 0.75, 0.75, 0.7);
const REACH: Color = Color::srgb(1.0, 0.75, 0.25);
const RELEASE: Color = Color::srgba(1.0, 0.55, 0.2, 0.8);
const TRAIL: Color = Color::srgb(0.25, 1.0, 0.85);
const ATTEMPTS: [Color; 3] = [
    Color::srgba(0.50, 0.36, 1.0, 0.40),
    Color::srgba(0.50, 0.36, 1.0, 0.65),
    Color::srgba(0.50, 0.36, 1.0, 0.95),
];
const PREDICTION: Color = Color::srgb(1.0, 0.45, 1.0);
const MARKER: Color = Color::srgb(1.0, 0.9, 0.4);

/// How far between the two latest ticks the camera stands: `sync_camera`
/// has placed it on the interpolated eye already, so this is the fraction
/// the rest of the frame was drawn with (1 when the eye did not move).
fn tick_fraction(prev_eye: Vec3, curr_eye: Vec3, camera: Vec3) -> f32 {
    let step = curr_eye - prev_eye;
    let length = step.length_squared();
    if length < 1.0e-10 {
        return 1.0;
    }
    let fraction = (camera - prev_eye).dot(step) / length;
    if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// A render-space direction for a UE direction (unit length, or zero).
fn render_direction(direction: sim_glam::Vec3) -> Vec3 {
    (to_render(direction) - to_render(sim_glam::Vec3::ZERO)).normalize_or_zero()
}

/// A length in render units for a length in UU.
fn render_length(length: f32) -> f32 {
    (to_render(sim_glam::Vec3::X * length) - to_render(sim_glam::Vec3::ZERO)).length()
}

/// The movement keys held now, as the input the predicted arc holds: move,
/// sprint and a held jump; the grapple is let go, nothing is pressed anew.
fn held_input(keys: &ButtonInput<KeyCode>) -> InputFrame {
    InputFrame {
        move_forward: crate::axis(
            keys,
            [KeyCode::KeyW, KeyCode::ArrowUp],
            [KeyCode::KeyS, KeyCode::ArrowDown],
        ),
        move_right: crate::axis(
            keys,
            [KeyCode::KeyD, KeyCode::ArrowRight],
            [KeyCode::KeyA, KeyCode::ArrowLeft],
        ),
        jump_held: keys.pressed(KeyCode::Space),
        sprint_held: keys.pressed(KeyCode::ShiftLeft),
        ..InputFrame::default()
    }
}

/// The predicted arc, kept between frames.
#[derive(Default)]
struct ArcCache {
    tick: u64,
    age: u32,
    input: Option<InputFrame>,
    points: Vec<sim_glam::Vec3>,
}

fn draw_velocity(gizmos: &mut Gizmos<SandboxGizmos>, player: &PlayerView, eye: sim_glam::Vec3) {
    let velocity = player.velocity;
    if !velocity.is_finite() || velocity.length() < 1.0 {
        return;
    }
    let base = eye + ue_forward_flat(player.yaw) * ARROW_AHEAD - sim_glam::Vec3::Z * ARROW_BELOW;
    let tip = base + velocity * ARROW_SECONDS;
    let flat_tip = base + sim_glam::Vec3::new(velocity.x, velocity.y, 0.0) * ARROW_SECONDS;
    gizmos.arrow(to_render(base), to_render(flat_tip), HORIZONTAL);
    gizmos.arrow(to_render(base), to_render(tip), VELOCITY);
    gizmos.line(to_render(flat_tip), to_render(tip), FAINT);
}

fn draw_cylinder(gizmos: &mut Gizmos<SandboxGizmos>, player: &PlayerView, centre: sim_glam::Vec3) {
    let (radius, half) = (player.capsule_radius, player.capsule_half_height);
    if !(radius > 0.0 && half > 0.0) {
        return;
    }
    let up = sim_glam::Vec3::Z * half;
    // A ring's plane is horizontal: UE +Z is the render up axis.
    let flat = Quat::from_rotation_arc(Vec3::Z, render_direction(sim_glam::Vec3::Z));
    for at in [centre - up, centre, centre + up] {
        gizmos.circle(
            Isometry3d::new(to_render(at), flat),
            render_length(radius),
            CYLINDER,
        );
    }
    // Four of the cylinder's edges, to either side of the view direction
    // (the cylinder is round: any edge will do, and none sits on the
    // crosshair this way).
    for degrees in [30.0_f32, -30.0, 150.0, -150.0] {
        let edge = centre + ue_forward_flat(player.yaw + degrees.to_radians()) * radius;
        gizmos.line(to_render(edge - up), to_render(edge + up), CYLINDER);
    }
    let normal = player.floor_normal;
    if player.grounded && normal.is_finite() && normal.length_squared() > 0.25 {
        let feet = centre - up;
        gizmos.arrow(
            to_render(feet),
            to_render(feet + normal * (half * 1.5)),
            FLOOR_NORMAL,
        );
    }
}

fn draw_aim(
    gizmos: &mut Gizmos<SandboxGizmos>,
    player: &PlayerView,
    aim: Option<&AimView>,
    eye: sim_glam::Vec3,
) {
    if let Some(anchor) = player.grapple_anchor {
        // Attached: the gun lets go inside this distance of the anchor.
        if player.release_distance > 0.0 {
            gizmos
                .sphere(
                    Isometry3d::from_translation(to_render(anchor)),
                    render_length(player.release_distance),
                    RELEASE,
                )
                .resolution(48);
        }
        return;
    }
    let Some(aim) = aim else {
        return;
    };
    let ray = aim.location - aim.start;
    let direction = ray.normalize_or_zero();
    if direction == sim_glam::Vec3::ZERO || !aim.location.is_finite() {
        return;
    }
    let color = match (aim.hit, aim.acceptable) {
        (true, true) => CAN_GRAPPLE,
        (true, false) => NO_GRAPPLE,
        (false, _) => NOTHING_HIT,
    };
    // From the hand, so the line is not seen end-on.
    let hand = eye - sim_glam::Vec3::Z * ROPE_HAND_DOWN_UU
        + ue_right_flat(player.yaw) * ROPE_HAND_RIGHT_UU;
    gizmos.line(to_render(hand), to_render(aim.location), color);
    let facing = Quat::from_rotation_arc(Vec3::Z, render_direction(direction));
    if aim.hit {
        let normal = aim
            .normal
            .map(render_direction)
            .filter(|n| *n != Vec3::ZERO);
        let on_surface = normal.map_or(facing, |n| Quat::from_rotation_arc(Vec3::Z, n));
        gizmos.circle(
            Isometry3d::new(to_render(aim.location), on_surface),
            render_length(18.0),
            color,
        );
    }
    // The gun's reach along the ray, when nothing nearer stops the trace.
    if aim.max_distance > 0.0 && (!aim.hit || aim.distance >= aim.max_distance) {
        gizmos.circle(
            Isometry3d::new(to_render(aim.start + direction * aim.max_distance), facing),
            render_length(40.0),
            REACH,
        );
    }
}

/// `points` (collision centres) as a line at foot level.
fn draw_path(
    gizmos: &mut Gizmos<SandboxGizmos>,
    points: impl Iterator<Item = sim_glam::Vec3>,
    drop: f32,
    color: Color,
) {
    gizmos.linestrip(
        points
            .filter(|p| p.is_finite())
            .map(|p| to_render(p - sim_glam::Vec3::Z * drop)),
        color,
    );
}

/// Draws the visualisers that are switched on.
#[allow(clippy::too_many_arguments)]
fn draw(
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    viz: Res<VizSettings>,
    scene: Res<ArenaScene>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    camera: Query<&Transform, With<PlayerCamera>>,
    mut arc: Local<ArcCache>,
    mut gizmos: Gizmos<SandboxGizmos>,
) {
    let Some(sim) = sim.as_deref() else {
        return;
    };
    let (Some(inspection), Some(session)) = (inspection(sim, &lab), lab.session.as_ref()) else {
        return;
    };
    let player = inspection.player();
    // The frame's interpolated pose, as the camera shows it.
    let fraction = camera.iter().next().map_or(1.0, |camera| {
        tick_fraction(
            to_render(sim.prev_eye),
            to_render(sim.curr_eye),
            camera.translation,
        )
    });
    let centre = sim.prev_position.lerp(sim.curr_position, fraction);
    let eye = sim.prev_eye.lerp(sim.curr_eye, fraction);
    // Paths are drawn at foot level: the pawn's own line is not in its face.
    let drop = (player.capsule_half_height - 4.0).max(0.0);

    if viz.velocity {
        draw_velocity(&mut gizmos, &player, eye);
    }
    if viz.cylinder {
        draw_cylinder(&mut gizmos, &player, centre);
    }
    if viz.aim {
        draw_aim(&mut gizmos, &player, inspection.aim().as_ref(), eye);
    }
    let telemetry = session.telemetry();
    if viz.attempts {
        let attempts = telemetry.attempts();
        let first = ATTEMPTS.len().saturating_sub(attempts.len());
        for (trail, color) in attempts.iter().zip(ATTEMPTS.iter().skip(first)) {
            draw_path(&mut gizmos, trail.iter().copied(), drop, *color);
        }
    }
    if viz.trail {
        draw_path(&mut gizmos, telemetry.trail(), drop, TRAIL);
    }
    if viz.prediction {
        let tick = sim.game.clock().tick();
        let input = keys.as_deref().map(held_input).unwrap_or_default();
        arc.age = arc.age.saturating_add(1);
        let stale = arc.input != Some(input)
            || tick < arc.tick
            || tick >= arc.tick.saturating_add(PREDICT_EVERY_TICKS)
            || arc.age >= PREDICT_EVERY_FRAMES;
        if stale {
            arc.points = predict(&sim.game, &input, PREDICT_TICKS);
            arc.tick = tick;
            arc.age = 0;
            arc.input = Some(input);
        }
        draw_path(
            &mut gizmos,
            std::iter::once(player.position).chain(arc.points.iter().copied()),
            drop,
            PREDICTION,
        );
        if let Some(end) = arc.points.last().filter(|p| p.is_finite()) {
            gizmos.sphere(
                Isometry3d::from_translation(to_render(*end - sim_glam::Vec3::Z * drop)),
                render_length(10.0),
                PREDICTION,
            );
        }
    } else if !arc.points.is_empty() {
        *arc = ArcCache::default();
    }
    if viz.markers {
        let stem = sim_glam::Vec3::Z * 50.0;
        for marker in scene.markers() {
            if marker.position.distance(player.position) > MARKER_RANGE {
                continue;
            }
            gizmos.line(
                to_render(marker.position - stem),
                to_render(marker.position),
                MARKER,
            );
            gizmos.sphere(
                Isometry3d::from_translation(to_render(marker.position)),
                render_length(6.0),
                MARKER,
            );
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.add_systems(Update, draw.run_if(lab_active).in_set(LabSet::Draw));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tick_fraction_is_where_the_camera_stands() {
        let (a, b) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(4.0, 1.0, 0.0));
        assert_eq!(tick_fraction(a, b, a), 0.0);
        assert_eq!(tick_fraction(a, b, b), 1.0);
        assert!((tick_fraction(a, b, Vec3::new(1.0, 1.0, 0.0)) - 0.25).abs() < 1e-6);
        // A camera somewhere else (a script's view target) stays in range.
        assert_eq!(tick_fraction(a, b, Vec3::new(100.0, 50.0, 3.0)), 1.0);
        assert_eq!(tick_fraction(a, b, Vec3::new(-100.0, 50.0, 3.0)), 0.0);
        // Standing still, or a broken pose: the latest tick.
        assert_eq!(tick_fraction(a, a, b), 1.0);
        assert_eq!(tick_fraction(a, b, Vec3::splat(f32::NAN)), 1.0);
    }

    #[test]
    fn render_helpers_keep_directions_and_lengths() {
        // UE up is the render up axis, and a direction stays a unit vector.
        let up = render_direction(sim_glam::Vec3::Z);
        assert!((up - Vec3::Y).length() < 1e-5, "{up:?}");
        let diagonal = render_direction(sim_glam::Vec3::new(3.0, 4.0, 0.0));
        assert!((diagonal.length() - 1.0).abs() < 1e-5);
        assert_eq!(render_direction(sim_glam::Vec3::ZERO), Vec3::ZERO);
        // Lengths scale with the presentation scale, whatever the axis.
        let unit = render_length(1.0);
        assert!(unit > 0.0);
        assert!((render_length(250.0) - 250.0 * unit).abs() < 1e-3);
        assert_eq!(render_length(0.0), 0.0);
    }

    #[test]
    fn the_predicted_arc_holds_the_movement_keys_and_lets_the_grapple_go() {
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(held_input(&keys), InputFrame::default());
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::Space);
        keys.press(KeyCode::ShiftLeft);
        let input = held_input(&keys);
        assert_eq!((input.move_forward, input.move_right), (1.0, -1.0));
        assert!(input.jump_held && input.sprint_held);
        // Held, never pressed anew; no grapple, no power jump, no use.
        assert!(!input.jump_pressed && !input.use_pressed);
        assert!(!input.grapple_held && !input.power_jump_held);
        assert_eq!((input.look_yaw_delta, input.look_pitch_delta), (0.0, 0.0));
    }
}
