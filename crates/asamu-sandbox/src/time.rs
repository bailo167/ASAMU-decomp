//! The time-control model: freeze, single step, speed.
//!
//! Plain data. The host reconciles its clocks to this model and never the
//! reverse; the simulation's `dt` is never changed (time control changes how
//! often a tick runs, not what a tick computes), and the game's own pause
//! state stays the pause menu's.
//!
//! How a host reads it, once per frame:
//!
//! - [`TimeControl::frozen`]: no tick runs, except that
//! - [`TimeControl::take_step`] returns `true` once for every single step
//!   that was asked for; the host then runs exactly one tick;
//! - [`TimeControl::speed`]: how fast ticks follow each other while not
//!   frozen (1 = normal).
//!
//! The speed steps and limits are ours.

use crate::command::CommandError;

/// The speed factors the slower / faster steps move through. Ours.
pub const SPEED_STEPS: [f32; 7] = [0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

/// Most single steps that can be pending at once (a bound against a hostile
/// step count, not a gameplay value): one minute at the default tick rate.
pub const MAX_PENDING_STEPS: u32 = 3600;

/// The slowest speed factor [`TimeControl::set_speed`] accepts.
pub const MIN_SPEED: f32 = SPEED_STEPS[0];
/// The fastest speed factor [`TimeControl::set_speed`] accepts.
pub const MAX_SPEED: f32 = SPEED_STEPS[SPEED_STEPS.len() - 1];

/// Freeze, pending single steps and the speed factor.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeControl {
    frozen: bool,
    speed: f32,
    pending_steps: u32,
    used: bool,
}

impl Default for TimeControl {
    fn default() -> Self {
        Self {
            frozen: false,
            speed: 1.0,
            pending_steps: 0,
            used: false,
        }
    }
}

impl TimeControl {
    /// The simulation is frozen (no tick runs except single steps).
    #[must_use]
    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// The speed factor (1 = normal).
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Single steps asked for and not yet taken.
    #[must_use]
    pub fn pending_steps(&self) -> u32 {
        self.pending_steps
    }

    /// Freezes or unfreezes. Unfreezing drops the pending single steps.
    pub fn set_frozen(&mut self, frozen: bool) {
        if frozen == self.frozen {
            return;
        }
        self.frozen = frozen;
        if frozen {
            self.used = true;
        } else {
            self.pending_steps = 0;
        }
    }

    /// Asks for `n` single ticks. Freezes first when the simulation is
    /// running (a "frame advance"). At most [`MAX_PENDING_STEPS`] are kept.
    pub fn request_steps(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        self.set_frozen(true);
        self.used = true;
        self.pending_steps = self.pending_steps.saturating_add(n).min(MAX_PENDING_STEPS);
    }

    /// Takes one pending single step; `true` if the host should run exactly
    /// one tick now. Always `false` while not frozen.
    pub fn take_step(&mut self) -> bool {
        if self.frozen && self.pending_steps > 0 {
            self.pending_steps -= 1;
            true
        } else {
            false
        }
    }

    /// The next slower [`SPEED_STEPS`] entry (stays at the slowest).
    pub fn slower(&mut self) {
        let next = SPEED_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < self.speed)
            .unwrap_or(MIN_SPEED);
        self.write_speed(next);
    }

    /// The next faster [`SPEED_STEPS`] entry (stays at the fastest).
    pub fn faster(&mut self) {
        let next = SPEED_STEPS
            .iter()
            .copied()
            .find(|s| *s > self.speed)
            .unwrap_or(MAX_SPEED);
        self.write_speed(next);
    }

    /// Sets the speed factor.
    ///
    /// # Errors
    /// A value that is not finite or outside the [`SPEED_STEPS`] range
    /// ([`MIN_SPEED`] to [`MAX_SPEED`]); the model is unchanged then.
    pub fn set_speed(&mut self, value: f32) -> Result<(), CommandError> {
        if !(value.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(&value)) {
            return Err(CommandError::Refused(format!(
                "the speed factor must be between {MIN_SPEED} and {MAX_SPEED} (got {value})"
            )));
        }
        self.write_speed(value);
        Ok(())
    }

    /// Unfrozen, normal speed, no pending step. Whether time control
    /// [was used](Self::was_used) is history and stays.
    pub fn reset(&mut self) {
        self.frozen = false;
        self.speed = 1.0;
        self.pending_steps = 0;
    }

    /// Unfrozen at normal speed with no pending step.
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.frozen && self.speed == 1.0 && self.pending_steps == 0
    }

    /// Time control was used at some point in this session: the simulation
    /// was frozen, stepped, or run at another speed (a recording made in the
    /// session is tagged with it).
    #[must_use]
    pub fn was_used(&self) -> bool {
        self.used
    }

    fn write_speed(&mut self, value: f32) {
        if value != self.speed {
            self.speed = value;
            self.used = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speed_steps_are_sorted_and_contain_normal_speed() {
        assert!(SPEED_STEPS.windows(2).all(|w| w[0] < w[1]));
        assert!(SPEED_STEPS.contains(&1.0));
        assert_eq!(MIN_SPEED, 0.1);
        assert_eq!(MAX_SPEED, 8.0);
    }

    #[test]
    fn the_default_model_changes_nothing() {
        let mut t = TimeControl::default();
        assert!(t.is_default());
        assert!(!t.was_used());
        assert!(!t.take_step());
        // Writes that change nothing are not "use".
        t.set_frozen(false);
        t.request_steps(0);
        t.set_speed(1.0).unwrap();
        t.reset();
        assert!(t.is_default());
        assert!(!t.was_used());
    }
}
