//! Serde helpers that make `f32` values round-trip **bit-exactly** through
//! JSON (used by the parity trace format).
//!
//! `serde_json` parses every number as `f64` and narrows to `f32` afterwards;
//! with its default (best-effort) float parser that path is not guaranteed to
//! reproduce the original `f32` bits. These helpers instead write the `f32`
//! widened to `f64` (losslessly), i.e. the shortest decimal that identifies
//! that exact `f64`. On read, any error in the `f64` parse is at most a few
//! `f64` ULPs, which is ~2^28 times smaller than the distance to the nearest
//! `f32` rounding boundary, so narrowing returns the original `f32` exactly.
//!
//! Readers accept any JSON number (hand-written traces may use short decimals);
//! values are parsed as `f64` and rounded to the nearest `f32`. Values that
//! overflow `f32` become infinite and are rejected by trace validation.
//!
//! Use with `#[serde(with = "asamu_core::exact_f32")]` on `f32` fields, or the
//! submodules for `glam::Vec3` and `Option`s.

use serde::{Deserialize, Deserializer, Serializer};

/// Serializes an `f32` as its exact `f64` value.
///
/// # Errors
/// Propagates serializer errors.
pub fn serialize<S: Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(f64::from(*value))
}

/// Deserializes any number as `f64` and narrows to `f32`.
///
/// # Errors
/// Propagates deserializer errors.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    f64::deserialize(deserializer).map(|v| v as f32)
}

/// `glam::Vec3` as a JSON array `[x, y, z]` of exact values.
pub mod vec3 {
    use glam::Vec3;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    /// Propagates serializer errors.
    pub fn serialize<S: Serializer>(v: &Vec3, serializer: S) -> Result<S::Ok, S::Error> {
        [f64::from(v.x), f64::from(v.y), f64::from(v.z)].serialize(serializer)
    }

    /// # Errors
    /// Propagates deserializer errors (including wrong array length).
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec3, D::Error> {
        let [x, y, z] = <[f64; 3]>::deserialize(deserializer)?;
        Ok(Vec3::new(x as f32, y as f32, z as f32))
    }
}

/// `Option<glam::Vec3>` as `null` or `[x, y, z]` of exact values.
pub mod option_vec3 {
    use glam::Vec3;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    /// Propagates serializer errors.
    pub fn serialize<S: Serializer>(v: &Option<Vec3>, serializer: S) -> Result<S::Ok, S::Error> {
        v.map(|v| [f64::from(v.x), f64::from(v.y), f64::from(v.z)])
            .serialize(serializer)
    }

    /// # Errors
    /// Propagates deserializer errors.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec3>, D::Error> {
        let v = Option::<[f64; 3]>::deserialize(deserializer)?;
        Ok(v.map(|[x, y, z]| Vec3::new(x as f32, y as f32, z as f32)))
    }
}

/// `Option<f32>` as `null` or an exact value.
pub mod option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// # Errors
    /// Propagates serializer errors.
    pub fn serialize<S: Serializer>(v: &Option<f32>, serializer: S) -> Result<S::Ok, S::Error> {
        v.map(f64::from).serialize(serializer)
    }

    /// # Errors
    /// Propagates deserializer errors.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<f32>, D::Error> {
        Ok(Option::<f64>::deserialize(deserializer)?.map(|v| v as f32))
    }
}

#[cfg(test)]
mod tests {
    use glam::Vec3;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        #[serde(with = "crate::exact_f32")]
        a: f32,
        #[serde(with = "crate::exact_f32::vec3")]
        v: Vec3,
        #[serde(with = "crate::exact_f32::option_vec3")]
        ov: Option<Vec3>,
        #[serde(with = "crate::exact_f32::option")]
        of: Option<f32>,
    }

    /// Deterministic SplitMix64 for test inputs.
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn random_bit_patterns_round_trip_exactly() {
        let mut s = 0x1234_5678_u64;
        let mut checked = 0;
        while checked < 200_000 {
            let bits = splitmix(&mut s) as u32;
            let x = f32::from_bits(bits);
            if !x.is_finite() {
                continue;
            }
            let json = serde_json::to_string(&Sample {
                a: x,
                v: Vec3::new(x, -x, x * 0.5),
                ov: Some(Vec3::splat(x)),
                of: Some(x),
            })
            .unwrap();
            let back: Sample = serde_json::from_str(&json).unwrap();
            assert_eq!(back.a.to_bits(), x.to_bits(), "{x:e} via {json}");
            assert_eq!(back.v.y.to_bits(), (-x).to_bits());
            assert_eq!(back.v.z.to_bits(), (x * 0.5).to_bits());
            assert_eq!(back.ov.map(|v| v.x.to_bits()), Some(x.to_bits()));
            assert_eq!(back.of.map(f32::to_bits), Some(x.to_bits()));
            checked += 1;
        }
    }

    #[test]
    fn accepts_short_decimals_and_nulls() {
        let back: Sample =
            serde_json::from_str(r#"{"a":0.1,"v":[1,2.5,-3],"ov":null,"of":null}"#).unwrap();
        assert_eq!(back.a, 0.1_f32);
        assert_eq!(back.v, Vec3::new(1.0, 2.5, -3.0));
        assert_eq!(back.ov, None);
        assert_eq!(back.of, None);
        assert!(
            serde_json::from_str::<Sample>(r#"{"a":1,"v":[1,2],"ov":null,"of":null}"#).is_err()
        );
    }
}
