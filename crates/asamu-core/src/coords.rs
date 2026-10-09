//! Coordinate conventions: UE3 ↔ Bevy.
//!
//! | System | Handedness | Forward | Right | Up | Unit |
//! |---|---|---|---|---|---|
//! | UE3 (simulation, traces, importer) | left-handed | +X | +Y | +Z | UU |
//! | Bevy (rendering only) | right-handed | −Z | +X | +Y | presentation scale |
//!
//! The mapping is `bevy = (ue.y, ue.z, −ue.x) · scale`. Its linear part is an
//! orthogonal matrix with determinant **−1**: it flips handedness, which is
//! exactly what converting a left-handed basis into a right-handed one requires
//! (forward→−Z, right→+X, up→+Y). Because the determinant is −1, cross products
//! change sign: `M a × M b = −M (a × b)`. Code that converts normals or
//! triangle winding must account for that; plain points and directions convert
//! with [`ue_dir_to_bevy`] / [`ue_pos_to_bevy`].
//!
//! The UE3 axis convention itself (left-handed, X forward, Y right, Z up) is
//! standard engine knowledge; that ASAMU's maps and script use it unchanged is
//! **TENTATIVE** until verified against parsed map data and recorded traces.
//!
//! The simulation never uses Bevy coordinates. Conversion happens once, at the
//! render boundary (`apps/asamu`) and, later, in the importer.

use glam::{Mat3, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::det_math::sin_cos;
use crate::units::PRESENTATION_UU_PER_METRE;

/// Uniform scale applied when converting UU positions into Bevy space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldScale {
    /// Bevy world units per Unreal unit.
    pub bevy_units_per_uu: f32,
}

impl WorldScale {
    /// 1 Bevy unit per UU (no scaling).
    pub const IDENTITY: Self = Self {
        bevy_units_per_uu: 1.0,
    };

    /// Bevy units are metres under the **presentation convention**
    /// ([`PRESENTATION_UU_PER_METRE`]); not a recovered ASAMU fact.
    pub const PRESENTATION_METRES: Self = Self {
        bevy_units_per_uu: 1.0 / PRESENTATION_UU_PER_METRE,
    };
}

impl Default for WorldScale {
    fn default() -> Self {
        Self::PRESENTATION_METRES
    }
}

/// The linear UE3 → Bevy basis change (no scale). Columns are the Bevy images
/// of UE +X (forward), +Y (right) and +Z (up). Determinant −1.
#[must_use]
pub fn ue_to_bevy_basis() -> Mat3 {
    Mat3::from_cols(
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    )
}

/// Converts a UE3 direction (or any free vector) to Bevy axes, without scale.
#[must_use]
pub fn ue_dir_to_bevy(v: Vec3) -> Vec3 {
    Vec3::new(v.y, v.z, -v.x)
}

/// Converts a Bevy direction (or any free vector) to UE3 axes, without scale.
/// Exact inverse of [`ue_dir_to_bevy`].
#[must_use]
pub fn bevy_dir_to_ue(v: Vec3) -> Vec3 {
    Vec3::new(-v.z, v.x, v.y)
}

/// Converts a UE3 position in UU to a Bevy position using `scale`.
#[must_use]
pub fn ue_pos_to_bevy(v: Vec3, scale: WorldScale) -> Vec3 {
    ue_dir_to_bevy(v) * scale.bevy_units_per_uu
}

/// Converts a Bevy position to a UE3 position in UU using `scale`.
#[must_use]
pub fn bevy_pos_to_ue(v: Vec3, scale: WorldScale) -> Vec3 {
    bevy_dir_to_ue(v) / scale.bevy_units_per_uu
}

/// Converts non-negative UE3 extents (sizes / half-sizes along each axis) to
/// Bevy extents. Sizes have no sign, so the −X → +Z flip does not apply.
#[must_use]
pub fn ue_extents_to_bevy(extents: Vec3, scale: WorldScale) -> Vec3 {
    Vec3::new(extents.y, extents.z, extents.x) * scale.bevy_units_per_uu
}

/// View direction (unit vector, UE3 axes) for a yaw/pitch pair in radians.
///
/// Uses the UE3 rotator convention: yaw rotates about +Z from +X towards +Y
/// (positive yaw turns right), pitch rotates up from the XY plane (positive
/// pitch looks up): `(cos p · cos y, cos p · sin y, sin p)`. This is the
/// standard engine convention; its use by ASAMU is **TENTATIVE** until
/// confirmed against recorded traces.
///
/// Uses [`crate::det_math::sin_cos`], so the result is bit-identical on every
/// platform (the simulation depends on that).
#[must_use]
pub fn ue_view_direction(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = sin_cos(yaw);
    let (sp, cp) = sin_cos(pitch);
    Vec3::new(cp * cy, cp * sy, sp)
}

/// Horizontal forward unit vector (UE3 axes) for a yaw in radians
/// (deterministic across platforms, like [`ue_view_direction`]).
#[must_use]
pub fn ue_forward_flat(yaw: f32) -> Vec3 {
    let (s, c) = sin_cos(yaw);
    Vec3::new(c, s, 0.0)
}

/// Horizontal right unit vector (UE3 axes) for a yaw in radians
/// (deterministic across platforms, like [`ue_view_direction`]).
#[must_use]
pub fn ue_right_flat(yaw: f32) -> Vec3 {
    let (s, c) = sin_cos(yaw);
    Vec3::new(-s, c, 0.0)
}

/// Bevy camera rotation for a UE3 yaw/pitch (radians).
///
/// A Bevy camera looks along its local −Z. The returned rotation maps −Z onto
/// `ue_dir_to_bevy(ue_view_direction(yaw, pitch))`, with no roll. UE3 positive
/// yaw turns right, which is a *negative* rotation about Bevy's +Y; positive
/// pitch looks up in both systems. Render-side only (uses glam's platform
/// trigonometry; never feed it back into the simulation).
#[must_use]
pub fn ue_view_to_bevy_rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_rotation_y(-yaw) * Quat::from_rotation_x(pitch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-6;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() <= EPS
    }

    #[test]
    fn axes_map_as_documented() {
        assert_eq!(ue_dir_to_bevy(Vec3::X), Vec3::NEG_Z, "forward");
        assert_eq!(ue_dir_to_bevy(Vec3::Y), Vec3::X, "right");
        assert_eq!(ue_dir_to_bevy(Vec3::Z), Vec3::Y, "up");
    }

    #[test]
    fn basis_matrix_matches_function_and_flips_handedness() {
        let m = ue_to_bevy_basis();
        assert_eq!(m.determinant(), -1.0);
        // Orthogonal: M^T M = I.
        assert!((m.transpose() * m).abs_diff_eq(Mat3::IDENTITY, 0.0));
        for v in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::new(1.5, -2.25, 3.0),
            Vec3::new(-7.0, 0.5, -0.125),
        ] {
            assert_eq!(m * v, ue_dir_to_bevy(v));
        }
    }

    #[test]
    fn cross_products_change_sign() {
        // Orientation-reversing orthogonal map: M a × M b = −M (a × b).
        let pairs = [
            (Vec3::X, Vec3::Y),
            (Vec3::new(1.0, 2.0, 3.0), Vec3::new(-4.0, 0.5, 2.0)),
            (Vec3::new(0.3, -0.7, 0.2), Vec3::new(5.0, 1.0, -1.0)),
        ];
        for (a, b) in pairs {
            let lhs = ue_dir_to_bevy(a).cross(ue_dir_to_bevy(b));
            let rhs = -ue_dir_to_bevy(a.cross(b));
            assert!(close(lhs, rhs), "{lhs} vs {rhs}");
        }
        // Concretely: in Bevy (right-handed) right × up = back (+Z), i.e. the
        // opposite of the converted UE forward.
        let right = ue_dir_to_bevy(Vec3::Y);
        let up = ue_dir_to_bevy(Vec3::Z);
        assert_eq!(right.cross(up), -ue_dir_to_bevy(Vec3::X));
    }

    #[test]
    fn round_trips() {
        let scales = [
            WorldScale::IDENTITY,
            WorldScale::PRESENTATION_METRES,
            WorldScale {
                bevy_units_per_uu: 0.01,
            },
        ];
        let points = [
            Vec3::ZERO,
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-1024.0, 512.25, -64.5),
            Vec3::new(1.0e5, -3.0e4, 2.0e3),
        ];
        for p in points {
            assert_eq!(bevy_dir_to_ue(ue_dir_to_bevy(p)), p);
            assert_eq!(ue_dir_to_bevy(bevy_dir_to_ue(p)), p);
            for s in scales {
                let back = bevy_pos_to_ue(ue_pos_to_bevy(p, s), s);
                let tol = p.abs().max_element().max(1.0) * 1e-6;
                assert!((back - p).abs().max_element() <= tol, "{p} -> {back}");
            }
        }
    }

    #[test]
    fn presentation_scale_is_fifty_uu_per_metre() {
        let p = ue_pos_to_bevy(
            Vec3::new(100.0, 50.0, 25.0),
            WorldScale::PRESENTATION_METRES,
        );
        assert!(close(p, Vec3::new(1.0, 0.5, -2.0)));
    }

    #[test]
    fn extents_are_unsigned() {
        let e = ue_extents_to_bevy(Vec3::new(10.0, 20.0, 30.0), WorldScale::IDENTITY);
        assert_eq!(e, Vec3::new(20.0, 30.0, 10.0));
    }

    #[test]
    fn view_direction_convention() {
        use core::f32::consts::{FRAC_PI_2, FRAC_PI_4};
        assert!(close(ue_view_direction(0.0, 0.0), Vec3::X));
        assert!(
            close(ue_view_direction(FRAC_PI_2, 0.0), Vec3::Y),
            "yaw +90° faces right"
        );
        assert!(
            close(ue_view_direction(0.0, FRAC_PI_2), Vec3::Z),
            "pitch +90° faces up"
        );
        let d = ue_view_direction(0.3, -0.4);
        assert!((d.length() - 1.0).abs() < EPS);
        assert!(close(
            ue_forward_flat(FRAC_PI_4),
            Vec3::new(1.0, 1.0, 0.0).normalize()
        ));
        assert!(close(ue_right_flat(0.0), Vec3::Y));
        assert!(close(ue_right_flat(FRAC_PI_2), Vec3::NEG_X));
    }

    #[test]
    fn bevy_camera_rotation_looks_along_converted_view_direction() {
        let samples = [
            (0.0, 0.0),
            (1.0, 0.0),
            (-2.0, 0.5),
            (3.0, -1.2),
            (0.25, 1.5),
            (-0.75, -0.3),
        ];
        for (yaw, pitch) in samples {
            let q = ue_view_to_bevy_rotation(yaw, pitch);
            let forward = q * Vec3::NEG_Z;
            let expected = ue_dir_to_bevy(ue_view_direction(yaw, pitch));
            assert!(
                close(forward, expected),
                "yaw {yaw} pitch {pitch}: {forward} vs {expected}"
            );
            // No roll: the camera's right vector stays horizontal and is the
            // converted UE right vector; the camera's up is never upside down.
            let right = q * Vec3::X;
            assert!(right.y.abs() <= EPS);
            assert!(close(right, ue_dir_to_bevy(ue_right_flat(yaw))), "{right}");
            assert!((q * Vec3::Y).y > 0.0);
        }
    }

    #[test]
    fn converted_view_basis_is_bevys_right_handed_camera_basis() {
        // The Bevy images of UE forward/right/up must form Bevy's camera
        // basis, in which right × up = back (−forward).
        for yaw in [0.0_f32, 0.7, -2.1, 3.1] {
            let f = ue_dir_to_bevy(ue_forward_flat(yaw));
            let r = ue_dir_to_bevy(ue_right_flat(yaw));
            let u = ue_dir_to_bevy(Vec3::Z);
            assert!(close(r.cross(u), -f), "yaw {yaw}");
            assert!((f.dot(r)).abs() <= EPS && (f.length() - 1.0).abs() <= EPS);
        }
    }

    #[test]
    fn rotator_quarter_turns_map_to_ue_axes() {
        use crate::rotator::Rotator;
        let cases = [
            (Rotator::new(0, 0, 0), Vec3::X),
            (Rotator::new(0, 16384, 0), Vec3::Y),
            (Rotator::new(0, -16384, 0), Vec3::NEG_Y),
            (Rotator::new(0, 32768, 0), Vec3::NEG_X),
            (Rotator::new(16384, 0, 0), Vec3::Z),
            (Rotator::new(-16384, 12345, 0), Vec3::NEG_Z),
        ];
        for (rot, expected) in cases {
            let (pitch, yaw, _) = rot.to_radians();
            let d = ue_view_direction(yaw, pitch);
            assert!(close(d, expected), "{rot:?}: {d}");
        }
        // Wrapped and unwrapped rotators give the same direction.
        let r = Rotator::new(1000, 70000, 0);
        let (p1, y1, _) = r.to_radians();
        let (p2, y2, _) = r.normalized().to_radians();
        assert!(close(ue_view_direction(y1, p1), ue_view_direction(y2, p2)));
    }

    #[test]
    fn view_direction_is_bit_identical_to_det_math_formula() {
        // Pins that the simulation-facing helpers use the deterministic
        // trigonometry (not the platform's).
        for (yaw, pitch) in [(0.3_f32, -0.2_f32), (-3.0, 1.2), (2.0, 0.0)] {
            let (sy, cy) = crate::det_math::sin_cos(yaw);
            let (sp, cp) = crate::det_math::sin_cos(pitch);
            assert_eq!(
                ue_view_direction(yaw, pitch),
                Vec3::new(cp * cy, cp * sy, sp)
            );
            assert_eq!(ue_forward_flat(yaw), Vec3::new(cy, sy, 0.0));
            assert_eq!(ue_right_flat(yaw), Vec3::new(-sy, cy, 0.0));
        }
    }
}
