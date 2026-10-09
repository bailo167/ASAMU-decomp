//! Unreal units (UU).
//!
//! The simulation (`asamu-player`, `asamu-world`, `asamu-game`) runs entirely in
//! **Unreal units**, the native distance unit of UE3 maps and script defaults.
//! Keeping the simulation in UU means constants recovered later from the
//! original game (script default properties, native code, config, traces) can
//! be used verbatim, and parity traces can be compared without any rescaling.
//!
//! # Presentation scale (a convention, not a recovered fact)
//!
//! [`PRESENTATION_UU_PER_METRE`] (50 UU per metre, i.e. 1 UU = 2 cm) is the
//! scale commonly quoted for UE3-era content. It is used **only** for
//! presentation: rendering in Bevy (whose ecosystem assumes roughly metres) and
//! human-readable HUD/debug output. It has **not** been recovered from *A Story
//! About My Uncle* and nothing in the simulation depends on it. Changing it must
//! never change gameplay.

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// Presentation-only conversion factor: Unreal units per metre.
///
/// **Convention, not a recovered ASAMU fact.** See the module docs.
pub const PRESENTATION_UU_PER_METRE: f32 = 50.0;

/// A distance in Unreal units.
///
/// The simulation mostly works with raw `f32`/`glam::Vec3` values documented
/// as UU; this newtype exists for APIs where mixing UU and metres would be an
/// easy mistake (configuration, HUD output, importer boundaries).
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Uu(pub f32);

impl Uu {
    /// Zero distance.
    pub const ZERO: Self = Self(0.0);

    /// The raw value in Unreal units.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }

    /// Converts to metres using the **presentation convention**
    /// ([`PRESENTATION_UU_PER_METRE`]). Never use this inside the simulation.
    #[must_use]
    pub fn to_presentation_metres(self) -> f32 {
        self.0 / PRESENTATION_UU_PER_METRE
    }

    /// Converts from metres using the **presentation convention**
    /// ([`PRESENTATION_UU_PER_METRE`]). Never use this inside the simulation.
    #[must_use]
    pub fn from_presentation_metres(metres: f32) -> Self {
        Self(metres * PRESENTATION_UU_PER_METRE)
    }
}

impl Add for Uu {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Uu {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for Uu {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl SubAssign for Uu {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl Neg for Uu {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Mul<f32> for Uu {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self(self.0 * rhs)
    }
}

impl Div<f32> for Uu {
    type Output = Self;
    fn div(self, rhs: f32) -> Self {
        Self(self.0 / rhs)
    }
}

impl core::fmt::Display for Uu {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} uu", self.0)
    }
}

/// Converts a speed in UU/s to presentation metres per second (HUD only).
#[must_use]
pub fn uu_per_s_to_presentation_m_per_s(speed_uu_per_s: f32) -> f32 {
    speed_uu_per_s / PRESENTATION_UU_PER_METRE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_round_trip() {
        let d = Uu(123.5);
        let m = d.to_presentation_metres();
        assert_eq!(m, 123.5 / 50.0);
        assert_eq!(Uu::from_presentation_metres(m), d);
    }

    #[test]
    fn arithmetic() {
        let mut a = Uu(10.0) + Uu(5.0) - Uu(2.0);
        a += Uu(1.0);
        a -= Uu(4.0);
        assert_eq!(a, Uu(10.0));
        assert_eq!(-a, Uu(-10.0));
        assert_eq!(a * 2.0, Uu(20.0));
        assert_eq!(a / 2.0, Uu(5.0));
        assert_eq!(a.get(), 10.0);
        assert_eq!(a.to_string(), "10 uu");
    }

    #[test]
    fn serde_is_transparent() {
        let json = serde_json::to_string(&Uu(2.5)).unwrap();
        assert_eq!(json, "2.5");
        let back: Uu = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Uu(2.5));
    }

    #[test]
    fn speed_helper() {
        assert_eq!(uu_per_s_to_presentation_m_per_s(500.0), 10.0);
    }
}
