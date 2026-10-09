//! UE3 rotator units.
//!
//! UE3 stores rotations (`Rotator`: Pitch, Yaw, Roll) as 32-bit integers where
//! one full turn is 65536 units, so 16384 = 90°. This is an **engine
//! convention** (well documented for UE3 script and native code), not an ASAMU
//! gameplay constant. Script and package data will express angles this way;
//! the simulation uses radians.

use core::f64::consts::TAU;

use serde::{Deserialize, Serialize};

/// Rotator units per full turn (UE3 engine convention).
pub const ROTATOR_UNITS_PER_TURN: i32 = 65536;

const UNITS_PER_TURN_F64: f64 = ROTATOR_UNITS_PER_TURN as f64;

/// Converts rotator units to radians. No wrapping is applied.
///
/// Computed in `f64` so large unwound values (e.g. accumulated yaw) keep their
/// precision until the final rounding to `f32`.
#[must_use]
pub fn rotator_units_to_radians(units: i32) -> f32 {
    (f64::from(units) * (TAU / UNITS_PER_TURN_F64)) as f32
}

/// Converts radians to the nearest rotator unit. No wrapping is applied.
///
/// Out-of-range inputs saturate at `i32::MIN`/`i32::MAX`; `NaN` maps to 0
/// (Rust's saturating float→int cast semantics).
#[must_use]
pub fn radians_to_rotator_units(radians: f32) -> i32 {
    (f64::from(radians) * (UNITS_PER_TURN_F64 / TAU)).round() as i32
}

/// Wraps a rotator axis into `[-32768, 32767]`, matching UE3's
/// `NormalizeAxis` semantics (keep the low 16 bits, interpret as signed).
#[must_use]
pub fn normalize_rotator_axis(units: i32) -> i32 {
    // The low 16 bits reinterpreted as a signed 16-bit value.
    i32::from(units as u16 as i16)
}

/// Wraps an angle in radians into `[-π, π)`.
#[must_use]
pub fn wrap_radians(radians: f32) -> f32 {
    use core::f32::consts::{PI, TAU as TAU32};
    let wrapped = (radians + PI).rem_euclid(TAU32) - PI;
    // rem_euclid can return exactly TAU for tiny negative inputs due to
    // rounding; fold that back into range.
    if wrapped >= PI {
        wrapped - TAU32
    } else {
        wrapped
    }
}

/// A UE3 rotator (integer units, 65536 per turn). Field order matches UE3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rotator {
    /// Rotation about the right axis; positive looks up.
    pub pitch: i32,
    /// Rotation about +Z; positive turns from +X towards +Y (right).
    pub yaw: i32,
    /// Rotation about the forward axis.
    pub roll: i32,
}

impl Rotator {
    /// Creates a rotator from integer units.
    #[must_use]
    pub const fn new(pitch: i32, yaw: i32, roll: i32) -> Self {
        Self { pitch, yaw, roll }
    }

    /// `(pitch, yaw, roll)` in radians (unwrapped).
    #[must_use]
    pub fn to_radians(self) -> (f32, f32, f32) {
        (
            rotator_units_to_radians(self.pitch),
            rotator_units_to_radians(self.yaw),
            rotator_units_to_radians(self.roll),
        )
    }

    /// Builds a rotator from `(pitch, yaw, roll)` radians (rounded, unwrapped).
    #[must_use]
    pub fn from_radians(pitch: f32, yaw: f32, roll: f32) -> Self {
        Self {
            pitch: radians_to_rotator_units(pitch),
            yaw: radians_to_rotator_units(yaw),
            roll: radians_to_rotator_units(roll),
        }
    }

    /// Each axis wrapped into `[-32768, 32767]`.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            pitch: normalize_rotator_axis(self.pitch),
            yaw: normalize_rotator_axis(self.yaw),
            roll: normalize_rotator_axis(self.roll),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::{FRAC_PI_2, PI};

    #[test]
    fn quarter_turns() {
        assert_eq!(rotator_units_to_radians(0), 0.0);
        assert_eq!(rotator_units_to_radians(16384), FRAC_PI_2);
        assert_eq!(rotator_units_to_radians(32768), PI);
        assert_eq!(rotator_units_to_radians(-16384), -FRAC_PI_2);
        assert_eq!(radians_to_rotator_units(FRAC_PI_2), 16384);
        assert_eq!(radians_to_rotator_units(-PI), -32768);
        assert_eq!(
            radians_to_rotator_units(core::f32::consts::TAU),
            ROTATOR_UNITS_PER_TURN
        );
    }

    #[test]
    fn integer_round_trip_is_exact_over_several_turns() {
        // Every unit in ±4 turns survives units → radians → units.
        for units in (-4 * 65536)..=(4 * 65536) {
            assert_eq!(
                radians_to_rotator_units(rotator_units_to_radians(units)),
                units,
                "units {units}"
            );
        }
    }

    #[test]
    fn radians_round_trip_within_half_unit() {
        let half_unit = core::f32::consts::TAU / 65536.0 / 2.0;
        let mut r = -10.0_f32;
        while r < 10.0 {
            let back = rotator_units_to_radians(radians_to_rotator_units(r));
            assert!((back - r).abs() <= half_unit * 1.0001, "{r} -> {back}");
            r += 0.0137;
        }
    }

    #[test]
    fn saturation_and_nan() {
        assert_eq!(radians_to_rotator_units(f32::NAN), 0);
        assert_eq!(radians_to_rotator_units(f32::INFINITY), i32::MAX);
        assert_eq!(radians_to_rotator_units(f32::NEG_INFINITY), i32::MIN);
    }

    #[test]
    fn normalize_axis_matches_ue3_semantics() {
        assert_eq!(normalize_rotator_axis(0), 0);
        assert_eq!(normalize_rotator_axis(32767), 32767);
        assert_eq!(normalize_rotator_axis(32768), -32768);
        assert_eq!(normalize_rotator_axis(65536), 0);
        assert_eq!(normalize_rotator_axis(65536 + 100), 100);
        assert_eq!(normalize_rotator_axis(-1), -1);
        assert_eq!(normalize_rotator_axis(-32769), 32767);
        assert_eq!(normalize_rotator_axis(i32::MIN), 0);
        assert_eq!(normalize_rotator_axis(i32::MAX), -1);
        let r = Rotator::new(70000, -40000, 16384).normalized();
        assert_eq!(r, Rotator::new(70000 - 65536, -40000 + 65536, 16384));
    }

    #[test]
    fn rotator_radians_round_trip() {
        let r = Rotator::new(-2048, 49152, 100);
        let (p, y, ro) = r.to_radians();
        assert_eq!(Rotator::from_radians(p, y, ro), r);
    }

    #[test]
    fn wrap_radians_range() {
        let mut r = -50.0_f32;
        while r < 50.0 {
            let w = wrap_radians(r);
            assert!((-PI..PI).contains(&w), "{r} -> {w}");
            // Same angle modulo a turn.
            let d = (w - r) / core::f32::consts::TAU;
            assert!((d - d.round()).abs() < 1e-4, "{r} -> {w}");
            r += 0.173;
        }
        assert_eq!(wrap_radians(PI), -PI);
        assert_eq!(wrap_radians(0.0), 0.0);
        assert!((-PI..PI).contains(&wrap_radians(-1e-9)));
    }
}
