//! UE3 rotators and actor transforms (`docs/reverse-engineering/LEVEL_FORMAT.md`,
//! "Transforms").
//!
//! - `AActor::LocalToWorld`: `world = R(S · (local − PrePivot)) + Location`,
//!   `S = DrawScale · DrawScale3D`, `R` the UE3 rotation matrix of
//!   `(Pitch, Yaw, Roll)` in the row-vector convention (CONFIRMED, native
//!   code).
//! - Sine and cosine of a rotator component come from a 16 384-entry table
//!   indexed by `(angle >> 2) & 0x3FFF`, the cosine a quarter turn later
//!   (index computation CONFIRMED; that the table holds `sin(i·2π/16384)` is
//!   TENTATIVE). The entries are evaluated with the deterministic
//!   `asamu_core::det_math` sine, so the result is bit-identical on every
//!   platform. (The importer evaluates the same formula with the platform
//!   sine in `f64`; the two agree to about 1e-7.)
//!
//! Used for actors that move at run time (falling rocks) and for the
//! checkpoint spawn offset (`spawnPointOffset >> Rotation`); static
//! placements use the matrices the importer wrote.

use core::f32::consts::TAU;

use asamu_core::det_math;
use glam::{DVec3, Vec3};

use crate::collision::Affine;

/// Rotator units per turn.
pub const UNITS_PER_TURN: i32 = 65_536;

/// `(sin, cos)` of a rotator component through the UE3 table lookup.
#[must_use]
pub fn rotator_sin_cos(units: i32) -> (f64, f64) {
    let entry = |u: u32| {
        let i = (u >> 2) & 0x3FFF;
        let angle = i as f32 * (TAU / 16_384.0);
        f64::from(det_math::sin(angle))
    };
    let bits = units as u32;
    (entry(bits), entry(bits.wrapping_add(0x4000)))
}

/// Rows of the UE3 rotation matrix of `(pitch, yaw, roll)` (the world images
/// of the local X, Y and Z axes).
#[must_use]
pub fn rotation_rows(rotation: [i32; 3]) -> [DVec3; 3] {
    let (sp, cp) = rotator_sin_cos(rotation[0]);
    let (sy, cy) = rotator_sin_cos(rotation[1]);
    let (sr, cr) = rotator_sin_cos(rotation[2]);
    [
        DVec3::new(cp * cy, cp * sy, sp),
        DVec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
        DVec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
    ]
}

/// `v >> rotation` (UnrealScript): `v` expressed in the rotated frame, i.e.
/// `v · R`.
#[must_use]
pub fn rotate_vector(v: DVec3, rotation: [i32; 3]) -> DVec3 {
    let r = rotation_rows(rotation);
    r[0] * v.x + r[1] * v.y + r[2] * v.z
}

/// Unit forward vector (local +X) of a rotation.
#[must_use]
pub fn forward(rotation: [i32; 3]) -> DVec3 {
    rotation_rows(rotation)[0]
}

/// `AActor::LocalToWorld` (see the module docs).
#[must_use]
pub fn actor_local_to_world(
    location: Vec3,
    rotation: [i32; 3],
    draw_scale: f32,
    draw_scale3d: Vec3,
    pre_pivot: Vec3,
) -> Affine {
    let s = draw_scale3d.as_dvec3() * f64::from(draw_scale);
    let r = rotation_rows(rotation);
    let rows = [r[0] * s.x, r[1] * s.y, r[2] * s.z];
    let pp = pre_pivot.as_dvec3();
    let translation = location.as_dvec3() - (rows[0] * pp.x + rows[1] * pp.y + rows[2] * pp.z);
    Affine { rows, translation }
}

/// A rotator component as radians (UE3 convention; 65 536 units per turn).
#[must_use]
pub fn units_to_radians(units: i32) -> f32 {
    asamu_core::rotator::rotator_units_to_radians(units)
}

/// UE3 `FRotator::GetNormalized` per component: into `(-32768, 32768]`.
#[must_use]
pub fn normalize_axis(units: i32) -> i32 {
    let a = units & 0xFFFF;
    if a > 32_768 { a - 65_536 } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_are_exact_axis_swaps() {
        let r = rotation_rows([0, 16_384, 0]);
        assert!((r[0] - DVec3::Y).length() < 1e-7, "{r:?}");
        assert!((r[1] - DVec3::NEG_X).length() < 1e-7);
        assert!((r[2] - DVec3::Z).length() < 1e-7);
        let p = rotation_rows([16_384, 0, 0]);
        assert!((p[0] - DVec3::Z).length() < 1e-7);
        // Angles are quantized to 4 units.
        assert_eq!(rotator_sin_cos(1001), rotator_sin_cos(1000));
        assert_eq!(rotator_sin_cos(-4), rotator_sin_cos(65_532));
    }

    #[test]
    fn rows_are_orthonormal_and_compose_with_scale_and_pivot() {
        for rot in [
            [123, 4567, -8901],
            [-16_000, 33_055, 200],
            [2074, 22_788, -897],
        ] {
            let r = rotation_rows(rot);
            for i in 0..3 {
                assert!((r[i].length() - 1.0).abs() < 1e-6);
                for j in 0..i {
                    assert!(r[i].dot(r[j]).abs() < 1e-6);
                }
            }
            assert!(
                (r[0].cross(r[1]) - r[2]).length() < 1e-6,
                "right-handed rows"
            );
        }
        let a = actor_local_to_world(
            Vec3::new(10.0, 20.0, 30.0),
            [0, 16_384, 0],
            2.0,
            Vec3::new(1.0, 1.0, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
        );
        // Local (1,0,0) is the pivot: maps to the location.
        let p = a.point(DVec3::new(1.0, 0.0, 0.0));
        assert!((p - DVec3::new(10.0, 20.0, 30.0)).length() < 1e-6, "{p}");
        // Local +X (scaled by 2) points along world +Y after the yaw.
        let q = a.point(DVec3::new(2.0, 0.0, 0.0));
        assert!((q - DVec3::new(10.0, 22.0, 30.0)).length() < 1e-6, "{q}");
        assert_eq!(normalize_axis(65_536 + 100), 100);
        assert_eq!(normalize_axis(-32_768), 32_768);
        assert_eq!(normalize_axis(40_000), 40_000 - 65_536);
        assert!((rotate_vector(DVec3::X, [0, 32_768, 0]) - DVec3::NEG_X).length() < 1e-6);
        assert!((forward([0, 0, 0]) - DVec3::X).length() < 1e-12);
    }
}
