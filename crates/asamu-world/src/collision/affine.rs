//! `f64` affine transforms in the UE3 row-vector convention.
//!
//! The importer writes every placement as a row-vector matrix
//! (`p' = p · M`, row 3 = translation; `docs/reverse-engineering/LEVEL_FORMAT.md`).
//! [`Affine`] keeps the linear part as three rows (the world images of the
//! local X, Y and Z axes) plus the translation, in `f64` so that the exact
//! collision tests see world-space vertices without an extra rounding step.

use glam::{DVec3, Vec3};

/// `p' = p.x · rows[0] + p.y · rows[1] + p.z · rows[2] + translation`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    /// World images of the local X, Y and Z axes.
    pub rows: [DVec3; 3],
    /// World position of the local origin.
    pub translation: DVec3,
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        rows: [DVec3::X, DVec3::Y, DVec3::Z],
        translation: DVec3::ZERO,
    };

    /// A pure translation.
    #[must_use]
    pub fn from_translation(t: DVec3) -> Self {
        Self {
            translation: t,
            ..Self::IDENTITY
        }
    }

    /// From a row-vector 4×4 matrix (`m[3]` = translation; the fourth column
    /// is ignored).
    #[must_use]
    pub fn from_row_matrix(m: &[[f32; 4]; 4]) -> Self {
        let row = |r: &[f32; 4]| DVec3::new(f64::from(r[0]), f64::from(r[1]), f64::from(r[2]));
        Self {
            rows: [row(&m[0]), row(&m[1]), row(&m[2])],
            translation: row(&m[3]),
        }
    }

    /// Back to a row-vector 4×4 matrix (rounded to `f32`).
    #[must_use]
    pub fn to_row_matrix(&self) -> [[f32; 4]; 4] {
        let r = |v: DVec3, w: f32| [v.x as f32, v.y as f32, v.z as f32, w];
        [
            r(self.rows[0], 0.0),
            r(self.rows[1], 0.0),
            r(self.rows[2], 0.0),
            r(self.translation, 1.0),
        ]
    }

    /// Transforms a point.
    #[must_use]
    pub fn point(&self, p: DVec3) -> DVec3 {
        self.rows[0] * p.x + self.rows[1] * p.y + self.rows[2] * p.z + self.translation
    }

    /// Transforms an `f32` point into `f64` world space.
    #[must_use]
    pub fn point_f32(&self, p: Vec3) -> DVec3 {
        self.point(p.as_dvec3())
    }

    /// Transforms a direction (no translation).
    #[must_use]
    pub fn vector(&self, v: DVec3) -> DVec3 {
        self.rows[0] * v.x + self.rows[1] * v.y + self.rows[2] * v.z
    }

    /// Determinant of the linear part (negative = mirroring).
    #[must_use]
    pub fn determinant(&self) -> f64 {
        self.rows[0].dot(self.rows[1].cross(self.rows[2]))
    }

    /// `true` when every number is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.rows.iter().all(|r| r.is_finite()) && self.translation.is_finite()
    }

    /// The inverse transform, or `None` when the linear part is (nearly)
    /// singular relative to its own scale.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        if !self.is_finite() {
            return None;
        }
        let [r0, r1, r2] = self.rows;
        let det = self.determinant();
        let scale = r0.length() * r1.length() * r2.length();
        if !(det.is_finite() && scale > 0.0 && det.abs() > 1.0e-9 * scale) {
            return None;
        }
        // Columns of the inverse are the cross products of the rows over det.
        let c0 = r1.cross(r2) / det;
        let c1 = r2.cross(r0) / det;
        let c2 = r0.cross(r1) / det;
        let rows = [
            DVec3::new(c0.x, c1.x, c2.x),
            DVec3::new(c0.y, c1.y, c2.y),
            DVec3::new(c0.z, c1.z, c2.z),
        ];
        let inv = Self {
            rows,
            translation: DVec3::ZERO,
        };
        let translation = -inv.vector(self.translation);
        let out = Self { rows, translation };
        out.is_finite().then_some(out)
    }

    /// `p · self · other` (apply `self`, then `other`).
    #[must_use]
    pub fn then(&self, other: &Self) -> Self {
        Self {
            rows: [
                other.vector(self.rows[0]),
                other.vector(self.rows[1]),
                other.vector(self.rows[2]),
            ],
            translation: other.point(self.translation),
        }
    }

    /// Exact bounds of the transformed box `[min, max]`.
    #[must_use]
    pub fn transform_aabb(&self, min: DVec3, max: DVec3) -> (DVec3, DVec3) {
        let c = (min + max) * 0.5;
        let e = (max - min) * 0.5;
        let center = self.point(c);
        let ex = self.rows[0].abs() * e.x + self.rows[1].abs() * e.y + self.rows[2].abs() * e.z;
        (center - ex, center + ex)
    }

    /// Half-extents in this transform's output space of a box with
    /// half-extents `e` (the absolute-row rule).
    #[must_use]
    pub fn transform_extent(&self, e: DVec3) -> DVec3 {
        self.rows[0].abs() * e.x + self.rows[1].abs() * e.y + self.rows[2].abs() * e.z
    }
}

/// Rounds an `f64` box outward to `f32` (so the `f32` box contains it).
#[must_use]
pub fn round_out(min: DVec3, max: DVec3) -> (Vec3, Vec3) {
    let down = |v: f64| {
        let f = v as f32;
        if f64::from(f) > v { f.next_down() } else { f }
    };
    let up = |v: f64| {
        let f = v as f32;
        if f64::from(f) < v { f.next_up() } else { f }
    };
    (
        Vec3::new(down(min.x), down(min.y), down(min.z)),
        Vec3::new(up(max.x), up(max.y), up(max.z)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Affine {
        Affine {
            rows: [
                DVec3::new(0.0, 2.0, 0.0),
                DVec3::new(-3.0, 0.0, 0.0),
                DVec3::new(0.0, 0.5, 1.5),
            ],
            translation: DVec3::new(10.0, -4.0, 7.0),
        }
    }

    #[test]
    fn inverse_round_trips() {
        let a = sample();
        let inv = a.inverse().unwrap();
        for p in [
            DVec3::ZERO,
            DVec3::new(1.0, 2.0, 3.0),
            DVec3::new(-7.5, 0.25, 100.0),
        ] {
            let back = inv.point(a.point(p));
            assert!((back - p).length() < 1e-9, "{back} vs {p}");
        }
        let id = a.then(&inv);
        for (r, e) in id.rows.iter().zip([DVec3::X, DVec3::Y, DVec3::Z]) {
            assert!((*r - e).length() < 1e-12);
        }
        assert!(id.translation.length() < 1e-9);
    }

    #[test]
    fn singular_and_non_finite_have_no_inverse() {
        let mut a = sample();
        a.rows[2] = a.rows[0] * 2.0;
        assert!(a.inverse().is_none());
        let mut b = sample();
        b.translation.x = f64::NAN;
        assert!(b.inverse().is_none());
    }

    #[test]
    fn row_matrix_round_trip_and_aabb() {
        let m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, -2.0, 0.0, 0.0],
            [5.0, 6.0, 7.0, 1.0],
        ];
        let a = Affine::from_row_matrix(&m);
        assert_eq!(a.to_row_matrix(), m);
        // Local (1, 0, 0) -> (6, 6, 7); local (0, 1, 0) -> (5, 6, 8); local (0,0,1) -> (5, 4, 7).
        assert_eq!(a.point(DVec3::X), DVec3::new(6.0, 6.0, 7.0));
        assert_eq!(a.point(DVec3::Y), DVec3::new(5.0, 6.0, 8.0));
        let (lo, hi) = a.transform_aabb(DVec3::splat(-1.0), DVec3::splat(1.0));
        assert_eq!(lo, DVec3::new(4.0, 4.0, 6.0));
        assert_eq!(hi, DVec3::new(6.0, 8.0, 8.0));
        let (flo, fhi) = round_out(DVec3::splat(0.1), DVec3::splat(0.1));
        assert!(f64::from(flo.x) <= 0.1 && f64::from(fhi.x) >= 0.1);
    }
}
