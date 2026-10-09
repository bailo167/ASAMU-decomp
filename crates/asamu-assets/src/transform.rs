//! UE3 scene transforms → render (Bevy) transforms.
//!
//! The scene JSON stores every actor and component placement as a UE3
//! `FMatrix` in **row-vector** convention (`p' = p · M`, row 3 = translation;
//! `LEVEL_FORMAT.md`, CONFIRMED). Converted meshes are glTF files whose
//! vertices are UE3 local coordinates mapped with the `asamu-core` basis
//! change `B: (x, y, z) → (y, z, −x)` and multiplied by the mesh manifest's
//! `scale` (glTF units per UU; 1 by default).
//!
//! For a glTF vertex `g`, the UE3 local point is `Bᵀ g / mesh_scale`, the UE3
//! world point is `M_ue · local` and the render point is `s · B · world`
//! (`s` = [`WorldScale::bevy_units_per_uu`]). Hence the render matrix of an
//! instance is
//!
//! ```text
//! R = S(s) · B · M_ue · Bᵀ · S(1 / mesh_scale)
//! ```
//!
//! `B · M · Bᵀ` is a similarity transform by an orthogonal matrix, so the
//! instance keeps the handedness of `M_ue`: a mirroring UE3 transform
//! (negative determinant; 48 of the 1,076 static mesh components of
//! AG-Workshop) stays mirroring, and the renderer must not cull its back
//! faces with the normal rule (see [`RenderTransform::mirrored`]). The
//! handedness flip of `B` itself is already baked into the glTF files
//! (`MESHES.md`).
//!
//! The simulation never sees any of this: it keeps UE3 coordinates in UU.

use asamu_core::WorldScale;
use asamu_core::coords::ue_to_bevy_basis;
use asamu_core::glam::{Mat3, Mat4, Quat, Vec3, Vec4};

/// A UE3 row-vector matrix as stored in the scene JSON.
pub type UeRowMatrix = [[f32; 4]; 4];

/// Converts a UE3 row-vector matrix to a column-vector [`Mat4`] in UE3
/// space (`p' = M · p`): column `i` of the result is row `i` of the input.
#[must_use]
pub fn ue_row_matrix_to_mat4(m: &UeRowMatrix) -> Mat4 {
    Mat4::from_cols(
        Vec4::from_array(m[0]),
        Vec4::from_array(m[1]),
        Vec4::from_array(m[2]),
        Vec4::from_array(m[3]),
    )
}

/// The UE3 → render basis change as a 4×4 matrix (no translation, no scale).
#[must_use]
pub fn basis4() -> Mat4 {
    Mat4::from_mat3(ue_to_bevy_basis())
}

/// The render matrix of a mesh instance (see the module docs).
#[must_use]
pub fn instance_matrix(ue_local_to_world: &Mat4, mesh_scale: f32, scale: WorldScale) -> Mat4 {
    let b = basis4();
    let inv_mesh = if mesh_scale.is_finite() && mesh_scale != 0.0 {
        1.0 / mesh_scale
    } else {
        1.0
    };
    Mat4::from_scale(Vec3::splat(scale.bevy_units_per_uu))
        * b
        * *ue_local_to_world
        * b.transpose()
        * Mat4::from_scale(Vec3::splat(inv_mesh))
}

/// A decomposed render transform (glam types of `asamu-core`; the app copies
/// the components into its own math types).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderTransform {
    /// Translation in render units.
    pub translation: Vec3,
    /// Rotation.
    pub rotation: Quat,
    /// Scale (one component negative when [`Self::mirrored`]).
    pub scale: Vec3,
    /// The linear part has a negative determinant (mirroring transform):
    /// front faces wind the other way.
    pub mirrored: bool,
    /// Largest |cos| between two transformed axes. A UE3 component transform
    /// composed with a non-uniformly scaled, rotated parent can shear, which
    /// a translation/rotation/scale transform cannot express; this measures
    /// how much is lost: below 2e-7 in 11 of the 12 shipped maps; in
    /// AG-StarHaven 6 of 7,353 draws shear (up to 0.017), placing their axes
    /// within 0.9 % (`tests/converted_real_data.rs`).
    pub shear: f32,
}

/// Decomposes a render matrix. Returns `None` when the matrix is not finite
/// or its linear part is degenerate (zero scale on some axis).
#[must_use]
pub fn decompose(m: &Mat4) -> Option<RenderTransform> {
    if !m.is_finite() {
        return None;
    }
    let lin = Mat3::from_mat4(*m);
    let det = lin.determinant();
    if !det.is_finite() || det.abs() <= f32::MIN_POSITIVE {
        return None;
    }
    let axes = [lin.x_axis, lin.y_axis, lin.z_axis];
    let lens = axes.map(Vec3::length);
    if lens.iter().any(|l| !l.is_finite() || *l <= 1e-12) {
        return None;
    }
    let mut shear = 0.0_f32;
    for (i, j) in [(0, 1), (0, 2), (1, 2)] {
        let c = (axes[i].dot(axes[j]) / (lens[i] * lens[j])).abs();
        shear = shear.max(c);
    }
    let (scale, rotation, translation) = m.to_scale_rotation_translation();
    if !(scale.is_finite() && rotation.is_finite() && translation.is_finite()) {
        return None;
    }
    Some(RenderTransform {
        translation,
        rotation: rotation.normalize(),
        scale,
        mirrored: det < 0.0,
        shear,
    })
}

/// UE3 location (UU) and direction of a placed light from its world matrix:
/// row 3 is the location and row 0 (the local +X axis) the direction a UE3
/// light points along (`LightComponent::GetDirection`; UE3 convention,
/// TENTATIVE for ASAMU).
#[must_use]
pub fn light_location_direction(m: &UeRowMatrix) -> (Vec3, Vec3) {
    let location = Vec3::new(m[3][0], m[3][1], m[3][2]);
    let dir = Vec3::new(m[0][0], m[0][1], m[0][2]);
    let dir = dir.try_normalize().unwrap_or(Vec3::X);
    (location, dir)
}

#[cfg(test)]
mod tests {
    use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};

    use super::*;

    const EPS: f32 = 1e-4;

    /// The UE3 row-vector matrix of a yaw rotation (radians, about +Z from +X
    /// towards +Y), per-axis scale `s` and translation `t`, from the closed
    /// form in LEVEL_FORMAT.md with pitch = roll = 0.
    fn ue_yaw_matrix(yaw: f32, s: [f32; 3], t: [f32; 3]) -> UeRowMatrix {
        let (sy, cy) = yaw.sin_cos();
        [
            [cy * s[0], sy * s[0], 0.0, 0.0],
            [-sy * s[1], cy * s[1], 0.0, 0.0],
            [0.0, 0.0, s[2], 0.0],
            [t[0], t[1], t[2], 1.0],
        ]
    }

    fn ue_transform_point(m: &UeRowMatrix, p: Vec3) -> Vec3 {
        let r = [p.x, p.y, p.z, 1.0];
        let mut out = [0.0f32; 3];
        for (j, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|i| r[i] * m[i][j]).sum();
        }
        Vec3::from_array(out)
    }

    fn gltf_of(p: Vec3, mesh_scale: f32) -> Vec3 {
        // What the importer writes: (y, z, -x) * scale.
        ue_dir_to_bevy(p) * mesh_scale
    }

    #[test]
    fn row_matrix_conversion_transposes() {
        let m = ue_yaw_matrix(0.7, [2.0, 3.0, 4.0], [10.0, -20.0, 30.0]);
        let g = ue_row_matrix_to_mat4(&m);
        for p in [Vec3::ZERO, Vec3::X, Vec3::new(1.0, -2.0, 0.5)] {
            let a = g.transform_point3(p);
            let b = ue_transform_point(&m, p);
            assert!((a - b).abs().max_element() < EPS, "{a} vs {b}");
        }
    }

    #[test]
    fn instance_matrix_places_gltf_vertices_like_the_simulation_would() {
        let scales = [WorldScale::IDENTITY, WorldScale::PRESENTATION_METRES];
        let cases = [
            ue_yaw_matrix(0.0, [1.0; 3], [0.0; 3]),
            ue_yaw_matrix(1.2, [1.31, 1.31, 1.31], [258.0, -168.0, 72.0]),
            ue_yaw_matrix(-2.5, [2.0, 0.5, 3.0], [-1000.0, 40.0, -7.0]),
            // Mirroring (negative X scale).
            ue_yaw_matrix(0.4, [-1.0, 1.0, 1.0], [5.0, 6.0, 7.0]),
        ];
        for scale in scales {
            for mesh_scale in [1.0, 0.02] {
                for m in &cases {
                    let r = instance_matrix(&ue_row_matrix_to_mat4(m), mesh_scale, scale);
                    for local in [Vec3::ZERO, Vec3::new(10.0, -3.0, 2.0), Vec3::Z * 50.0] {
                        let render = r.transform_point3(gltf_of(local, mesh_scale));
                        let expected = ue_pos_to_bevy(ue_transform_point(m, local), scale);
                        let tol = EPS * expected.abs().max_element().max(1.0);
                        assert!(
                            (render - expected).abs().max_element() <= tol,
                            "{render} vs {expected}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn decomposition_round_trips_and_flags_mirroring() {
        let m = ue_yaw_matrix(0.9, [1.5, 1.5, 1.5], [100.0, 200.0, 300.0]);
        let r = instance_matrix(
            &ue_row_matrix_to_mat4(&m),
            1.0,
            WorldScale::PRESENTATION_METRES,
        );
        let t = decompose(&r).unwrap();
        assert!(!t.mirrored);
        assert!(t.shear < 1e-5);
        let back = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        assert!(back.abs_diff_eq(r, 1e-4));

        let mirrored = ue_yaw_matrix(0.9, [-1.0, 2.0, 2.0], [1.0, 2.0, 3.0]);
        let r = instance_matrix(&ue_row_matrix_to_mat4(&mirrored), 1.0, WorldScale::IDENTITY);
        let t = decompose(&r).unwrap();
        assert!(t.mirrored);
        let back = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        assert!(back.abs_diff_eq(r, 1e-4), "{back} vs {r}");
    }

    #[test]
    fn degenerate_and_non_finite_matrices_are_refused() {
        let zero = ue_yaw_matrix(0.0, [0.0, 1.0, 1.0], [0.0; 3]);
        assert!(decompose(&ue_row_matrix_to_mat4(&zero)).is_none());
        let mut nan = ue_yaw_matrix(0.0, [1.0; 3], [0.0; 3]);
        nan[3][0] = f32::NAN;
        assert!(decompose(&ue_row_matrix_to_mat4(&nan)).is_none());
        let mut inf = ue_yaw_matrix(0.0, [1.0; 3], [0.0; 3]);
        inf[1][1] = f32::INFINITY;
        assert!(decompose(&ue_row_matrix_to_mat4(&inf)).is_none());
    }

    #[test]
    fn shear_is_measured() {
        let sheared: UeRowMatrix = [
            [1.0, 0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let r = instance_matrix(&ue_row_matrix_to_mat4(&sheared), 1.0, WorldScale::IDENTITY);
        let t = decompose(&r).unwrap();
        assert!((t.shear - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    }

    #[test]
    fn light_direction_is_the_local_x_axis() {
        let m = ue_yaw_matrix(core::f32::consts::FRAC_PI_2, [3.0; 3], [1.0, 2.0, 3.0]);
        let (loc, dir) = light_location_direction(&m);
        assert_eq!(loc, Vec3::new(1.0, 2.0, 3.0));
        assert!((dir - Vec3::Y).abs().max_element() < EPS, "{dir}");
        let degenerate = [[0.0; 4]; 4];
        assert_eq!(light_location_direction(&degenerate).1, Vec3::X);
    }
}
