//! Deterministic elementary functions for the simulation.
//!
//! # Why
//!
//! `f32::sin`, `f32::cos` and friends call the platform maths library (libm,
//! the MSVC CRT, Apple's libsystem_m). Those are not required to be correctly
//! rounded, so the same input can produce results that differ in the last bit
//! between Windows, Linux and macOS (or between libm versions). One differing
//! bit in a view direction is enough to make two runs of the simulation drift
//! apart, which would break cross-platform trace replay.
//!
//! The functions here are written in plain Rust using only IEEE-754 basic
//! operations (`+`, `-`, `*`, `/`) on `f64`, plus exact operations (`round`,
//! comparisons, `f32 ↔ f64` conversions). Rust evaluates those exactly as
//! written: it never contracts `a * b + c` into a fused multiply-add and never
//! uses extended precision on the supported targets (x86_64 uses SSE2, aarch64
//! uses NEON/VFP scalar ops). The results are therefore **bit-identical on
//! every supported platform**; the golden-value test below pins them.
//!
//! # Accuracy
//!
//! Argument reduction uses a three-part Cody–Waite split of π/2 and the
//! polynomials are Taylor series truncated far below `f64` precision on
//! `|r| ≤ π/4`, so before the final rounding to `f32` the error is a few `f64`
//! ULPs. The `f32` results are within 1 ULP of the true value (in practice
//! correctly rounded) for `|x| ≤ ACCURATE_RANGE`. Larger finite inputs are
//! first reduced with an exact `fmod` by the `f64` value of 2π: the result is
//! deterministic and in `[-1, 1]` but not the true sine (at that magnitude
//! adjacent `f32` values are already more than 0.06 rad apart). The
//! simulation never produces such angles (yaw is wrapped to `[-π, π)`, pitch
//! is clamped).

/// Inputs with `|x|` up to this many radians are reduced accurately
/// (`k · PIO2_1` stays exact while `|k| < 2^20`).
pub const ACCURATE_RANGE: f32 = 1.0e6;

/// π/2 split into three parts (33 + 33 + rest significant bits) so that
/// `k · PIO2_1` and `k · PIO2_2` are exact for `|k| < 2^20`. Their sum equals
/// π/2 to about 1e-31.
const PIO2_1: f64 = f64::from_bits(0x3FF9_21FB_5440_0000);
const PIO2_2: f64 = f64::from_bits(0x3DD0_B461_1A60_0000);
const PIO2_3: f64 = f64::from_bits(0x3BA3_198A_2E00_0000);

// Taylor coefficients ±1/n!. Every n! used here is exactly representable in
// f64 (n ≤ 18 < 2^53), so each constant is the correctly rounded reciprocal.
const S1: f64 = -1.0 / 6.0;
const S2: f64 = 1.0 / 120.0;
const S3: f64 = -1.0 / 5_040.0;
const S4: f64 = 1.0 / 362_880.0;
const S5: f64 = -1.0 / 39_916_800.0;
const S6: f64 = 1.0 / 6_227_020_800.0;
const S7: f64 = -1.0 / 1_307_674_368_000.0;
const S8: f64 = 1.0 / 355_687_428_096_000.0;

const C1: f64 = -1.0 / 2.0;
const C2: f64 = 1.0 / 24.0;
const C3: f64 = -1.0 / 720.0;
const C4: f64 = 1.0 / 40_320.0;
const C5: f64 = -1.0 / 3_628_800.0;
const C6: f64 = 1.0 / 479_001_600.0;
const C7: f64 = -1.0 / 87_178_291_200.0;
const C8: f64 = 1.0 / 20_922_789_888_000.0;
const C9: f64 = -1.0 / 6_402_373_705_728_000.0;

/// `sin(r)` for `|r| ≲ π/4` (truncation error < 1e-19).
fn sin_kernel(r: f64) -> f64 {
    let z = r * r;
    let p = S1 + z * (S2 + z * (S3 + z * (S4 + z * (S5 + z * (S6 + z * (S7 + z * S8))))));
    r + r * z * p
}

/// `cos(r)` for `|r| ≲ π/4` (truncation error < 1e-20).
fn cos_kernel(r: f64) -> f64 {
    let z = r * r;
    let p = C2 + z * (C3 + z * (C4 + z * (C5 + z * (C6 + z * (C7 + z * (C8 + z * C9))))));
    1.0 + z * (C1 + z * p)
}

/// Deterministic `(sin x, cos x)` for an `f32` angle in radians.
///
/// Bit-identical on every platform (see module docs). Non-finite input gives
/// `(NaN, NaN)`; `±0.0` gives `(±0.0, 1.0)`.
#[must_use]
pub fn sin_cos(x: f32) -> (f32, f32) {
    if !x.is_finite() {
        return (f32::NAN, f32::NAN);
    }
    if x == 0.0 {
        // Preserves the sign of zero, like the platform functions.
        return (x, 1.0);
    }
    let mut xd = f64::from(x);
    if xd.abs() > f64::from(ACCURATE_RANGE) {
        // `%` is IEEE fmod: exact, hence identical on every platform.
        xd %= core::f64::consts::TAU;
    }
    let k = (xd * core::f64::consts::FRAC_2_PI).round();
    let r = if k == 0.0 {
        xd
    } else {
        ((xd - k * PIO2_1) - k * PIO2_2) - k * PIO2_3
    };
    let (s, c) = (sin_kernel(r), cos_kernel(r));
    // Quadrant k mod 4 (two's complement `& 3` is the Euclidean remainder;
    // |k| < 2^20 here, so the cast is exact).
    let (s, c) = match (k as i64) & 3 {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    };
    (s as f32, c as f32)
}

/// Deterministic `sin x` (see [`sin_cos`]).
#[must_use]
pub fn sin(x: f32) -> f32 {
    sin_cos(x).0
}

/// Deterministic `cos x` (see [`sin_cos`]).
#[must_use]
pub fn cos(x: f32) -> f32 {
    sin_cos(x).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

    /// Distance in ULPs between two finite `f32`s of the same sign region.
    fn ulps(a: f32, b: f32) -> u32 {
        fn key(x: f32) -> i64 {
            let bits = i64::from(x.to_bits());
            if bits & 0x8000_0000 != 0 {
                -(bits & 0x7FFF_FFFF)
            } else {
                bits
            }
        }
        u32::try_from((key(a) - key(b)).unsigned_abs()).unwrap_or(u32::MAX)
    }

    /// Reference value computed in f64 by the platform library, rounded to
    /// f32 (accurate to well under an f32 ULP for these magnitudes).
    fn reference(x: f32) -> (f32, f32) {
        let (s, c) = f64::from(x).sin_cos();
        (s as f32, c as f32)
    }

    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn within_one_ulp_of_reference() {
        let mut state = 42_u64;
        let mut worst = 0;
        let mut check = |x: f32| {
            let (s, c) = sin_cos(x);
            let (rs, rc) = reference(x);
            let (es, ec) = (ulps(s, rs), ulps(c, rc));
            assert!(
                es <= 1 && ec <= 1,
                "x = {x:e}: ({s:e}, {c:e}) vs ({rs:e}, {rc:e})"
            );
            worst = worst.max(es).max(ec);
        };
        // Uniform over the simulation's range and well beyond.
        for _ in 0..400_000 {
            let u = (splitmix(&mut state) >> 40) as f32 / (1u64 << 24) as f32;
            check((u * 2.0 - 1.0) * 1.0e4);
        }
        // Dense around quadrant boundaries, where reduction matters most.
        for k in -2000..=2000 {
            let centre = k as f32 * FRAC_PI_2;
            let mut x = centre;
            for _ in 0..8 {
                check(x);
                x = f32::from_bits(x.to_bits() + 1);
            }
        }
        // Tiny magnitudes: every power of two from the smallest subnormal up.
        for bits in (0..23).map(|i| 1_u32 << i).chain((1..127).map(|e| e << 23)) {
            let x = f32::from_bits(bits);
            check(x);
            check(-x);
        }
        assert!(worst <= 1);
    }

    #[test]
    fn exact_special_values() {
        assert_eq!(sin_cos(0.0), (0.0, 1.0));
        let (s, c) = sin_cos(-0.0);
        assert!(s == 0.0 && s.is_sign_negative() && c == 1.0);
        assert!(sin(f32::NAN).is_nan() && cos(f32::INFINITY).is_nan());
        assert!(sin(f32::NEG_INFINITY).is_nan());
        assert_eq!(sin(FRAC_PI_2), 1.0);
        assert_eq!(cos(PI), -1.0);
        assert_eq!(sin(-FRAC_PI_2), -1.0);
        assert!((sin(FRAC_PI_4) - cos(FRAC_PI_4)).abs() <= f32::EPSILON);
        // Odd/even symmetry holds exactly.
        let mut x = -20.0_f32;
        while x < 20.0 {
            assert_eq!(sin(-x).to_bits(), (-sin(x)).to_bits(), "{x}");
            assert_eq!(cos(-x).to_bits(), cos(x).to_bits(), "{x}");
            x += 0.0731;
        }
    }

    #[test]
    fn bounded_and_deterministic_far_outside_range() {
        for x in [ACCURATE_RANGE * 10.0, 1.0e20, -3.0e38, f32::MAX, f32::MIN] {
            let (s, c) = sin_cos(x);
            assert!(
                (-1.0..=1.0).contains(&s) && (-1.0..=1.0).contains(&c),
                "{x}"
            );
            assert_eq!(sin_cos(x), (s, c));
        }
    }

    /// Golden bit patterns. These must be identical on every platform and
    /// build; a mismatch means a non-IEEE or contracted operation crept in.
    #[test]
    fn golden_bits_are_platform_independent() {
        let cases: [(f32, u32, u32); 8] = [
            (0.1, 0x3DCC_7577, 0x3F7E_B898),
            (1.0, 0x3F57_6AA4, 0x3F0A_5140),
            (-2.5, 0xBF19_3578, 0xBF4D_17BF),
            (3.0, 0x3E10_81C3, 0xBF7D_7026),
            (PI, 0xB3BB_BD2E, 0xBF80_0000),
            (TAU, 0x343B_BD2E, 0x3F80_0000),
            (-1234.5678, 0xBD9F_EAA5, 0xBF7F_37E7),
            (FRAC_PI_4, 0x3F35_04F3, 0x3F35_04F3),
        ];
        let mut report = String::new();
        let mut ok = true;
        for (x, s_bits, c_bits) in cases {
            let (s, c) = sin_cos(x);
            report.push_str(&format!(
                "({x:?}, 0x{:08X}, 0x{:08X}),\n",
                s.to_bits(),
                c.to_bits()
            ));
            ok &= s.to_bits() == s_bits && c.to_bits() == c_bits;
        }
        assert!(ok, "golden values changed; actual:\n{report}");
    }
}
