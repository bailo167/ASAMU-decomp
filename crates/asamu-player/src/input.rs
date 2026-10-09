//! Per-tick player input.
//!
//! [`InputFrame`] holds **abstract actions**, not keys: the app maps devices
//! to them. The original's keyboard mapping (`DefaultInput.ini`,
//! GAMEPLAY_LEADS.md "Input bindings") is: W/S/A/D → the move axes, Space →
//! jump (press: `Jump`, release: `ReleaseJump`; also `RocketBoostKeyDown`),
//! left shift → sprint (`StartSprinting` / `StopSprinting`), left mouse →
//! fire (the grapple), right mouse → power jump (`PowerJumpKeyDown` /
//! `PowerJumpKeyUp`), E or Enter → `use`.

use serde::{Deserialize, Serialize};

/// Input for one simulation tick.
///
/// This is the *logical* input the simulation consumes, independent of devices.
/// The app (or a trace replayer) produces one frame per tick. All values are
/// sanitized by the simulation ([`InputFrame::sanitized`]): non-finite numbers
/// become 0 and axes are clamped to `[-1, 1]`, so arbitrary input can never
/// inject NaN into the state.
///
/// Floats use exact JSON encoding (see `asamu_core::exact_f32`) so traces
/// round-trip bit-exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFrame {
    /// Forward/back axis in `[-1, 1]` (+1 = forward).
    #[serde(with = "asamu_core::exact_f32")]
    pub move_forward: f32,
    /// Strafe axis in `[-1, 1]` (+1 = right).
    #[serde(with = "asamu_core::exact_f32")]
    pub move_right: f32,
    /// Yaw change this tick in radians (UE3 convention: + turns right).
    #[serde(with = "asamu_core::exact_f32")]
    pub look_yaw_delta: f32,
    /// Pitch change this tick in radians (+ looks up).
    #[serde(with = "asamu_core::exact_f32")]
    pub look_pitch_delta: f32,
    /// Jump was *pressed* (edge) since the previous tick.
    pub jump_pressed: bool,
    /// Jump button is currently held (level). Unused by the placeholder model;
    /// recorded so traces can capture variable-height jumps if the original
    /// turns out to have them.
    #[serde(default)]
    pub jump_held: bool,
    /// Grapple button is currently held (level). The simulation detects the
    /// press edge itself, so the frame does not need a separate "pressed" flag.
    pub grapple_held: bool,
    /// Sprint button held (level; the original's `StartSprinting` on press,
    /// `StopSprinting` on release — edges detected by the simulation).
    #[serde(default)]
    pub sprint_held: bool,
    /// Power-jump button held (level; `PowerJumpKeyDown` / `PowerJumpKeyUp`
    /// edges detected by the simulation). In story mode it zooms.
    #[serde(default)]
    pub power_jump_held: bool,
    /// `use` was pressed (edge) since the previous tick (only does something
    /// in story mode).
    #[serde(default)]
    pub use_pressed: bool,
}

impl InputFrame {
    /// A copy with non-finite values zeroed and axes clamped to `[-1, 1]`.
    #[must_use]
    pub fn sanitized(&self) -> Self {
        fn axis(v: f32) -> f32 {
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        }
        fn finite(v: f32) -> f32 {
            if v.is_finite() { v } else { 0.0 }
        }
        Self {
            move_forward: axis(self.move_forward),
            move_right: axis(self.move_right),
            look_yaw_delta: finite(self.look_yaw_delta),
            look_pitch_delta: finite(self.look_pitch_delta),
            jump_pressed: self.jump_pressed,
            jump_held: self.jump_held,
            grapple_held: self.grapple_held,
            sprint_held: self.sprint_held,
            power_jump_held: self.power_jump_held,
            use_pressed: self.use_pressed,
        }
    }

    /// `true` if every float field is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.move_forward.is_finite()
            && self.move_right.is_finite()
            && self.look_yaw_delta.is_finite()
            && self.look_pitch_delta.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_and_zeroes() {
        let f = InputFrame {
            move_forward: 3.0,
            move_right: f32::NAN,
            look_yaw_delta: f32::INFINITY,
            look_pitch_delta: -0.5,
            jump_pressed: true,
            jump_held: true,
            grapple_held: true,
            sprint_held: true,
            power_jump_held: true,
            use_pressed: true,
        };
        assert!(!f.is_finite());
        let s = f.sanitized();
        assert!(s.is_finite());
        assert_eq!(s.move_forward, 1.0);
        assert_eq!(s.move_right, 0.0);
        assert_eq!(s.look_yaw_delta, 0.0);
        assert_eq!(s.look_pitch_delta, -0.5);
        assert!(s.jump_pressed && s.jump_held && s.grapple_held);
        assert!(s.sprint_held && s.power_jump_held && s.use_pressed);
    }

    #[test]
    fn serde_round_trip_and_strictness() {
        let f = InputFrame {
            move_forward: 0.1,
            move_right: -1.0,
            look_yaw_delta: 0.003,
            look_pitch_delta: -0.002,
            jump_pressed: false,
            jump_held: false,
            grapple_held: true,
            sprint_held: true,
            power_jump_held: false,
            use_pressed: true,
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: InputFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
        // Frames written before the action fields existed still parse.
        let old = r#"{"move_forward":0.0,"move_right":0.0,"look_yaw_delta":0.0,"look_pitch_delta":0.0,"jump_pressed":false,"grapple_held":false}"#;
        let back: InputFrame = serde_json::from_str(old).unwrap();
        assert_eq!(back, InputFrame::default());
        assert!(serde_json::from_str::<InputFrame>(&json.replace("}", r#","bogus":1}"#)).is_err());
    }
}
