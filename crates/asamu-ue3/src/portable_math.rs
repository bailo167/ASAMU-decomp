//! Transcendental functions that return the same bits on every platform.
//!
//! `f64::sin`, `f32::powf` and the other transcendental methods of the
//! standard library call the C math library of the platform, and those
//! libraries disagree in the last bit: of the 16,384 values
//! `sin(i · 2π / 16384)` in double precision, the macOS library and the
//! Windows C runtime return different results for 847, and neither is the
//! correctly rounded one for all of them (measured, see
//! `docs/reverse-engineering/INVENTORY.md`, "Converting on Windows").
//! Data converted from the same game files must not depend on the system it
//! was converted on, so the decoders whose results reach converted data call
//! the functions here instead. They are the pure-Rust `libm` implementations
//! (IEEE arithmetic only, no platform library), so their results are
//! identical on Windows, Linux and macOS; the tests pin them with hashes
//! over whole tables, which CI evaluates on all three.
//!
//! Only `+ - * /` and `sqrt` are exact in IEEE 754 and safe to use directly.

/// Sine of `x` (radians), the same bits everywhere.
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// Cosine of `x` (radians), the same bits everywhere.
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// Sine of `x` (radians) in single precision, the same bits everywhere:
/// the double-precision sine rounded once.
pub fn sinf(x: f32) -> f32 {
    sin(f64::from(x)) as f32
}

/// Cosine of `x` (radians) in single precision, the same bits everywhere:
/// the double-precision cosine rounded once.
pub fn cosf(x: f32) -> f32 {
    cos(f64::from(x)) as f32
}

/// `x` to the power `y` in single precision, the same bits everywhere.
///
/// Evaluated in double precision and rounded once, which gives the correctly
/// rounded single-precision result unless the exact value lies within about
/// 2⁻⁵³ (relative) of the midpoint of two neighbouring `f32` values.
pub fn powf(x: f32, y: f32) -> f32 {
    libm::pow(f64::from(x), f64::from(y)) as f32
}

/// FNV-1a (64-bit) over `bytes`: the hash the table tests pin results with.
#[cfg(test)]
pub(crate) fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI, TAU};

    #[test]
    fn hash_helper_matches_the_published_vectors() {
        // FNV-1a 64 test vectors: "" and "a".
        assert_eq!(fnv1a64([]), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a64(*b"a"), 0xAF63_DC4C_8601_EC8C);
    }

    #[test]
    fn sine_has_the_same_bits_on_every_platform() {
        assert_eq!(sin(0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(sin(FRAC_PI_2), 1.0);
        assert_eq!(sin(-FRAC_PI_2), -1.0);
        // The double nearest to π is a little below π.
        assert_eq!(sin(PI), 1.224_646_799_147_353_2e-16);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(cos(PI), -1.0);
        // The double nearest to π/2 is a little below π/2.
        assert_eq!(cos(FRAC_PI_2), 6.123_233_995_736_766e-17);
        // All 16,384 arguments of the engine's rotator table. The two hashes
        // are what `libm` 0.2.16 returns (recorded on Windows x86_64): they
        // exist to fail on any system, or after any dependency update, where
        // a single bit of a result differs, because converted data would then
        // differ too.
        let table = (0..16384u32).map(|i| sin(f64::from(i) * TAU / 16384.0));
        assert_eq!(
            fnv1a64(table.flat_map(f64::to_le_bytes)),
            0x7C9D_8A68_5C9D_06C9
        );
        let table = (0..16384u32).map(|i| cos(f64::from(i) * TAU / 16384.0));
        assert_eq!(
            fnv1a64(table.flat_map(f64::to_le_bytes)),
            0xFAE7_AE6C_7DE7_3E9B
        );
    }

    /// The two tables whose platform-dependent last bits made the `levels`
    /// and `lightmaps` conversions differ between macOS and Windows.
    #[test]
    fn decoder_tables_have_the_same_bits_on_every_platform() {
        // Rotator sine table through the level decoder (index = angle / 4).
        let sines = (0..16384i32).map(|i| crate::level::rotator_sin_cos(i * 4).0);
        assert_eq!(
            fnv1a64(sines.flat_map(f64::to_le_bytes)),
            0x7C9D_8A68_5C9D_06C9
        );
        // The cosine is the sine a quarter turn later.
        for i in [0, 4, 8192, 16384, 40_000, -4, -16384] {
            let (_, c) = crate::level::rotator_sin_cos(i);
            let (s, _) = crate::level::rotator_sin_cos(i.wrapping_add(16384));
            assert_eq!(c.to_bits(), s.to_bits(), "angle {i}");
        }
        // Light map sRGB decode for every byte: the hash of the correctly
        // rounded results, computed independently with 70-digit decimal
        // arithmetic (the Windows C runtime returns the same 256 values, the
        // macOS library differs for bytes 71 and 180).
        let linear = (0..=255u8).map(crate::lightmap::srgb_to_linear);
        assert_eq!(
            fnv1a64(linear.flat_map(f32::to_le_bytes)),
            0x341D_A4F8_5895_C67F
        );
        assert_eq!(crate::lightmap::srgb_to_linear(0), 0.0);
        assert_eq!(crate::lightmap::srgb_to_linear(255), 1.0);
    }

    /// Single-precision sine and cosine, and the sine and cosine of whole
    /// degrees the decal frame takes. The hashes were computed independently
    /// (a transcription of the same algorithm into another language, run on
    /// macOS arm64; the 4,096 single-precision sines also equal the correctly
    /// rounded values from 80-digit decimal arithmetic).
    #[test]
    fn single_precision_sine_and_cosine_have_the_same_bits_on_every_platform() {
        assert_eq!(sinf(0.0).to_bits(), 0);
        assert_eq!(cosf(0.0), 1.0);
        let args = || (0..=4096u16).map(|k| f32::from(k) / 64.0);
        assert_eq!(
            fnv1a64(args().map(sinf).flat_map(f32::to_le_bytes)),
            0x224D_88F9_170D_05B6
        );
        assert_eq!(
            fnv1a64(args().map(cosf).flat_map(f32::to_le_bytes)),
            0x0427_39BF_56D7_A7EF
        );
        let degrees = || (0..360u16).map(|d| f64::from(d).to_radians());
        assert_eq!(
            fnv1a64(degrees().flat_map(|a| (sin(a) as f32).to_le_bytes())),
            0xF321_C192_9CC6_C896
        );
        assert_eq!(
            fnv1a64(degrees().flat_map(|a| (cos(a) as f32).to_le_bytes())),
            0x5BF4_FBCF_C4FE_8704
        );
        assert_eq!(sin(30f64.to_radians()) as f32, 0.5);
        assert_eq!(cos(60f64.to_radians()) as f32, 0.5);
        assert_eq!((cos(90f64.to_radians()) as f32).to_bits(), 0x248D_3132);
    }

    /// The colour decode of the material decoder (`(c / 255) ^ 2.2`) for
    /// every byte: the hash of the correctly rounded results, computed
    /// independently with 80-digit decimal arithmetic. The macOS `powf`
    /// returns the neighbouring value for the bytes 79 and 198 (measured), so
    /// with the platform's power this test fails there.
    #[test]
    fn material_colours_have_the_same_bits_on_every_platform() {
        let channel = |b: u8| crate::material::color_to_linear([0, 0, b, 255])[0];
        assert_eq!(
            fnv1a64((0..=255u8).map(channel).flat_map(f32::to_le_bytes)),
            0x5250_950F_A270_F768
        );
        assert_eq!(channel(79).to_bits(), 0x3D9B_7F28);
        assert_eq!(channel(198).to_bits(), 0x3F12_BA8A);
        assert_eq!(channel(0), 0.0);
        assert_eq!(channel(255), 1.0);
    }

    #[test]
    fn single_precision_power_is_correctly_rounded_on_a_grid() {
        assert_eq!(powf(2.0, 10.0), 1024.0);
        assert_eq!(powf(0.25, 0.5), 0.5);
        assert_eq!(powf(1.0, 2.4), 1.0);
        assert_eq!(powf(0.0, 2.4), 0.0);
        // (k / 255)^2.4 for every byte k: the hash of the correctly rounded
        // results, computed independently with 70-digit decimal arithmetic.
        let grid = (0..=255u8).map(|k| powf(f32::from(k) / 255.0, 2.4));
        assert_eq!(
            fnv1a64(grid.flat_map(f32::to_le_bytes)),
            0x579F_C21D_D3B1_BEC3
        );
    }
}
