//! The speed-line cone (`asamu.ASAMUVelocityCone`, spawned by the pawn):
//! its material parameter, placement and orientation. Pure functions.
//!
//! Local reading of the script (VFX_DECALS.md §3, GRAPPLE.md G-FX-2): every
//! tick the cone is placed at the camera's view point plus `V·dt`, turned
//! to face along `V`, and the material instance's `Velocity` scalar is set:
//! 0 below `fadeMinVelocity`, else `clamp(|V| / fadeMaxVelocity, 0, 1)`,
//! scaled by `1 − (|v̂.z| − 0.9) / 0.1` when the motion is within about 26°
//! of vertical (`|v̂.z| ≥ 0.9`). `ToggleSpeedlines` (setting
//! `SpeedlinesActive`, default true) hides the cone.

use asamu_core::glam::Vec3 as UeVec3;
use asamu_player::grapple_gun::{PULL_DISTANCE_SCALE, PULL_NUMERATOR};
use asamu_player::{BootsStateName, PlayerState};

/// `ASAMUVelocityCone.fadeMinVelocity`, UU/s. CONFIRMED (cdo).
pub const FADE_MIN_VELOCITY: f32 = 1200.0;
/// `ASAMUVelocityCone.fadeMaxVelocity`, UU/s. CONFIRMED (cdo).
pub const FADE_MAX_VELOCITY: f32 = 5000.0;
/// `|v̂.z|` from which the cone fades out (literal in
/// `UpdateInstanceParameter`). CONFIRMED (src).
pub const VERTICAL_FADE_START: f32 = 0.9;
/// Span of that fade (literal). CONFIRMED (src).
pub const VERTICAL_FADE_SPAN: f32 = 0.1;
/// The material instance parameter (`matInstanceParamName`). CONFIRMED (cdo).
pub const PARAMETER_NAME: &str = "Velocity";
/// The cone's static mesh (`StaticMeshComponent0.StaticMesh`). CONFIRMED (cdo).
pub const CONE_MESH: &str = "ASAMUVelocityEffect.VelocityCone";
/// `SpeedlinesActive` default (`DefaultSettings.ini`). CONFIRMED (config).
pub const SPEEDLINES_DEFAULT: bool = true;

/// The `Velocity` material parameter for pawn velocity `v` (UU/s).
#[must_use]
pub fn cone_parameter(v: UeVec3) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let speed = v.length();
    if speed < FADE_MIN_VELOCITY {
        return 0.0;
    }
    let z = v.normalize_or_zero().z.abs();
    let base = (speed / FADE_MAX_VELOCITY).clamp(0.0, 1.0);
    if z < VERTICAL_FADE_START {
        base
    } else {
        (1.0 - (z - VERTICAL_FADE_START) / VERTICAL_FADE_SPAN) * base
    }
}

/// `Rotator(Normal(v))` as (yaw, pitch) in radians (UE3 convention; the
/// zero vector gives the zero rotator).
#[must_use]
pub fn cone_rotation(v: UeVec3) -> (f32, f32) {
    let n = v.normalize_or_zero();
    if n == UeVec3::ZERO || !n.is_finite() {
        return (0.0, 0.0);
    }
    let yaw = n.y.atan2(n.x);
    let pitch = n.z.atan2((n.x * n.x + n.y * n.y).sqrt());
    (yaw, pitch)
}

/// The cone's location: the view point plus `v · dt`.
#[must_use]
pub fn cone_location(view: UeVec3, v: UeVec3, dt: f32) -> UeVec3 {
    if !v.is_finite() || !dt.is_finite() {
        return view;
    }
    view + v * dt
}

/// The velocity the cone saw this tick. The cone ticks after the pawn's
/// physics and before the gun (G-TM-2), so while the grapple pulls it sees
/// the velocity without the gun's pull increment; the simulation's state is
/// after the gun, so the increment is taken off again (`dt · 10⁷/d` toward
/// the anchor, G-PH-2). No pull happens while boosting or while an instant
/// release is pending. A proximity release in the same tick (velocity
/// halved, attachment gone) is not undone.
#[must_use]
pub fn velocity_seen_by_cone(state: &PlayerState, dt: f32) -> UeVec3 {
    let gun = &state.script.gun;
    let pulled = gun.attached.is_some()
        && gun.instant_release_timer.is_none()
        && state.script.boots.state != BootsStateName::Boosting;
    let d = gun.distance;
    if !pulled || !d.is_finite() || d <= 0.0 || !dt.is_finite() {
        return state.velocity;
    }
    let toward = (gun.grapple_location - state.position).normalize_or_zero();
    state.velocity - toward * (dt * PULL_NUMERATOR / (d / PULL_DISTANCE_SCALE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::grapple_gun::Attachment;

    #[test]
    fn parameter_below_and_above_the_fade_range() {
        assert_eq!(cone_parameter(UeVec3::ZERO), 0.0);
        assert_eq!(cone_parameter(UeVec3::new(1199.0, 0.0, 0.0)), 0.0);
        assert!((cone_parameter(UeVec3::new(1200.0, 0.0, 0.0)) - 0.24).abs() < 1e-6);
        assert!((cone_parameter(UeVec3::new(2000.0, 0.0, 0.0)) - 0.4).abs() < 1e-6);
        assert_eq!(cone_parameter(UeVec3::new(9000.0, 0.0, 0.0)), 1.0);
        assert_eq!(cone_parameter(UeVec3::new(f32::NAN, 0.0, 0.0)), 0.0);
    }

    #[test]
    fn vertical_motion_fades_the_cone() {
        // Straight down: |v̂.z| = 1 → factor 0.
        assert!(cone_parameter(UeVec3::new(0.0, 0.0, -3000.0)).abs() < 1e-6);
        // |v̂.z| = 0.95 → factor 0.5 of the speed term.
        let z = 0.95f32;
        let h = (1.0 - z * z).sqrt();
        let v = UeVec3::new(h, 0.0, -z) * 2500.0;
        assert!((cone_parameter(v) - 0.5 * 0.5).abs() < 1e-4);
        // Just below the threshold: no fade.
        let z = 0.89f32;
        let h = (1.0 - z * z).sqrt();
        let v = UeVec3::new(h, 0.0, z) * 2500.0;
        assert!((cone_parameter(v) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rotation_and_location() {
        let (yaw, pitch) = cone_rotation(UeVec3::new(0.0, 10.0, 0.0));
        assert!((yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-6 && pitch.abs() < 1e-6);
        let (_, pitch) = cone_rotation(UeVec3::new(1.0, 0.0, 1.0));
        assert!((pitch - std::f32::consts::FRAC_PI_4).abs() < 1e-6);
        assert_eq!(cone_rotation(UeVec3::ZERO), (0.0, 0.0));
        let l = cone_location(UeVec3::ONE, UeVec3::new(60.0, 0.0, 0.0), 1.0 / 60.0);
        assert!((l - UeVec3::new(2.0, 1.0, 1.0)).length() < 1e-6);
    }

    #[test]
    fn the_cone_sees_the_velocity_before_the_pull() {
        let mut s = PlayerState::new(UeVec3::ZERO, 0.0);
        s.velocity = UeVec3::new(2000.0, 0.0, 0.0);
        // Not attached: unchanged.
        assert_eq!(velocity_seen_by_cone(&s, 1.0 / 60.0), s.velocity);
        // Attached 1000 uu ahead: the pull added dt · 10⁷/1000 = 166.67 uu/s.
        s.script.gun.attached = Some(Attachment::default());
        s.script.gun.grapple_location = UeVec3::new(1000.0, 0.0, 0.0);
        s.script.gun.distance = 1000.0;
        s.velocity = UeVec3::new(2000.0 + 10_000.0 / 60.0, 0.0, 0.0);
        let seen = velocity_seen_by_cone(&s, 1.0 / 60.0);
        assert!((seen.x - 2000.0).abs() < 1e-2, "{seen}");
        assert!((cone_parameter(seen) - 0.4).abs() < 1e-5);
        // Boosting: no pull to undo.
        s.script.boots.state = BootsStateName::Boosting;
        assert_eq!(velocity_seen_by_cone(&s, 1.0 / 60.0), s.velocity);
    }
}
