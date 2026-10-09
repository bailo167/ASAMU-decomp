//! Fixed-step simulation clock.
//!
//! # The original's tick model is UNKNOWN
//!
//! UE3 advances actors with a **variable** `DeltaTime` per frame (optionally
//! clamped / smoothed by engine settings), so the original game almost
//! certainly does not run gameplay at a fixed rate. Our simulation uses a fixed
//! step because it makes runs deterministic and traces reproducible. The tick
//! rate is therefore a **runtime choice**, not a recovered value, and must be
//! revisited once traces from the original game exist (they record the real
//! per-frame `DeltaTime`, which a replay harness can feed through
//! variable-`dt` steps if parity requires it).
//!
//! The clock only counts ticks. Simulation time is derived as
//! `tick / tick_rate` in `f64`, so it never accumulates floating-point drift.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default simulation tick rate in Hz.
///
/// **Runtime choice, not a recovered value** (see module docs).
pub const DEFAULT_TICK_RATE_HZ: f64 = 60.0;

/// Highest accepted tick rate. A sanity bound, not a gameplay value.
pub const MAX_TICK_RATE_HZ: f64 = 10_000.0;

/// Default cap on ticks run for one [`FixedClock::accumulate`] call, so a long
/// stall (debugger, window drag) cannot trigger a "spiral of death".
pub const DEFAULT_MAX_TICKS_PER_ADVANCE: u32 = 8;

/// Errors from constructing a [`FixedClock`].
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ClockError {
    /// The tick rate was not finite, not positive, or above [`MAX_TICK_RATE_HZ`].
    #[error("invalid tick rate {0} Hz (must be finite, > 0 and <= {MAX_TICK_RATE_HZ})")]
    InvalidTickRate(f64),
}

/// A fixed-step clock: counts simulation ticks at a configurable rate.
///
/// Deserialization re-validates the tick rate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FixedClockRepr")]
pub struct FixedClock {
    tick_rate_hz: f64,
    tick: u64,
    /// Real time not yet consumed by ticks (only used by [`Self::accumulate`]).
    accumulator: f64,
    max_ticks_per_advance: u32,
}

/// Unvalidated serde mirror of [`FixedClock`].
#[derive(Deserialize)]
struct FixedClockRepr {
    tick_rate_hz: f64,
    tick: u64,
    accumulator: f64,
    max_ticks_per_advance: u32,
}

impl TryFrom<FixedClockRepr> for FixedClock {
    type Error = ClockError;

    fn try_from(r: FixedClockRepr) -> Result<Self, ClockError> {
        let mut clock =
            Self::new(r.tick_rate_hz)?.with_max_ticks_per_advance(r.max_ticks_per_advance);
        clock.tick = r.tick;
        clock.accumulator = if r.accumulator.is_finite() && r.accumulator >= 0.0 {
            r.accumulator
        } else {
            0.0
        };
        Ok(clock)
    }
}

impl FixedClock {
    /// Creates a clock at `tick_rate_hz`, starting at tick 0.
    ///
    /// # Errors
    /// [`ClockError::InvalidTickRate`] if the rate is not finite, not positive
    /// or above [`MAX_TICK_RATE_HZ`].
    pub fn new(tick_rate_hz: f64) -> Result<Self, ClockError> {
        if !tick_rate_hz.is_finite() || tick_rate_hz <= 0.0 || tick_rate_hz > MAX_TICK_RATE_HZ {
            return Err(ClockError::InvalidTickRate(tick_rate_hz));
        }
        Ok(Self {
            tick_rate_hz,
            tick: 0,
            accumulator: 0.0,
            max_ticks_per_advance: DEFAULT_MAX_TICKS_PER_ADVANCE,
        })
    }

    /// Sets the cap on ticks returned by one [`Self::accumulate`] call
    /// (minimum 1).
    #[must_use]
    pub fn with_max_ticks_per_advance(mut self, max: u32) -> Self {
        self.max_ticks_per_advance = max.max(1);
        self
    }

    /// Tick rate in Hz.
    #[must_use]
    pub fn tick_rate_hz(&self) -> f64 {
        self.tick_rate_hz
    }

    /// Step length in seconds as `f32` (what the simulation consumes).
    /// Identical on every call, so all ticks use bit-identical `dt`.
    #[must_use]
    pub fn dt(&self) -> f32 {
        (1.0 / self.tick_rate_hz) as f32
    }

    /// Step length in seconds as `f64`.
    #[must_use]
    pub fn dt_f64(&self) -> f64 {
        1.0 / self.tick_rate_hz
    }

    /// Number of ticks completed so far.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Simulation time in seconds after the completed ticks (`tick / rate`).
    #[must_use]
    pub fn time_seconds(&self) -> f64 {
        self.tick as f64 / self.tick_rate_hz
    }

    /// Simulation time in seconds at an arbitrary tick index.
    #[must_use]
    pub fn time_at_tick(&self, tick: u64) -> f64 {
        tick as f64 / self.tick_rate_hz
    }

    /// Records one completed tick and returns the new tick count.
    pub fn advance_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    /// Adds real elapsed time and returns how many ticks are now due (at most
    /// the configured cap; excess time beyond the cap is dropped). The caller
    /// runs that many simulation steps, calling [`Self::advance_tick`] after
    /// each. Non-finite or negative input is ignored.
    pub fn accumulate(&mut self, real_seconds: f64) -> u32 {
        if real_seconds.is_finite() && real_seconds > 0.0 {
            self.accumulator += real_seconds;
        }
        let dt = self.dt_f64();
        let mut due = 0_u32;
        while self.accumulator >= dt && due < self.max_ticks_per_advance {
            self.accumulator -= dt;
            due += 1;
        }
        if due == self.max_ticks_per_advance && self.accumulator >= dt {
            // Drop the backlog rather than trying to catch up.
            self.accumulator = 0.0;
        }
        due
    }

    /// Fraction of a tick of real time accumulated but not yet simulated, in
    /// `[0, 1)`; useful for render interpolation.
    #[must_use]
    pub fn overstep_fraction(&self) -> f64 {
        (self.accumulator / self.dt_f64()).clamp(0.0, 1.0)
    }

    /// Resets to tick 0 with an empty accumulator (rate unchanged).
    pub fn reset(&mut self) {
        self.tick = 0;
        self.accumulator = 0.0;
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self {
            tick_rate_hz: DEFAULT_TICK_RATE_HZ,
            tick: 0,
            accumulator: 0.0,
            max_ticks_per_advance: DEFAULT_MAX_TICKS_PER_ADVANCE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_rates() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, MAX_TICK_RATE_HZ * 2.0] {
            assert!(FixedClock::new(bad).is_err(), "{bad}");
        }
        assert!(FixedClock::new(1.0).is_ok());
        assert!(FixedClock::new(MAX_TICK_RATE_HZ).is_ok());
    }

    #[test]
    fn default_matches_constant() {
        let c = FixedClock::default();
        assert_eq!(c.tick_rate_hz(), DEFAULT_TICK_RATE_HZ);
        assert_eq!(c, FixedClock::new(DEFAULT_TICK_RATE_HZ).unwrap());
    }

    #[test]
    fn dt_and_time() {
        let mut c = FixedClock::new(120.0).unwrap();
        assert_eq!(c.dt(), (1.0_f64 / 120.0) as f32);
        assert_eq!(c.dt(), c.dt(), "dt is stable");
        for _ in 0..240 {
            c.advance_tick();
        }
        assert_eq!(c.tick(), 240);
        assert_eq!(c.time_seconds(), 2.0);
        assert_eq!(c.time_at_tick(60), 0.5);
        c.reset();
        assert_eq!(c.tick(), 0);
    }

    #[test]
    fn accumulate_counts_due_ticks_and_caps_backlog() {
        let mut c = FixedClock::new(100.0).unwrap();
        assert_eq!(c.accumulate(0.005), 0);
        assert!((c.overstep_fraction() - 0.5).abs() < 1e-9);
        assert_eq!(c.accumulate(0.006), 1);
        assert_eq!(c.accumulate(-1.0), 0);
        assert_eq!(c.accumulate(f64::NAN), 0);
        // A 10 s stall yields at most the cap, and the backlog is dropped.
        assert_eq!(c.accumulate(10.0), DEFAULT_MAX_TICKS_PER_ADVANCE);
        assert_eq!(c.accumulate(0.0), 0);
        let mut c = FixedClock::new(100.0)
            .unwrap()
            .with_max_ticks_per_advance(0);
        assert_eq!(c.accumulate(1.0), 1, "cap is at least 1");
    }

    #[test]
    fn serde_round_trip() {
        let mut c = FixedClock::new(75.0).unwrap();
        c.advance_tick();
        let json = serde_json::to_string(&c).unwrap();
        let back: FixedClock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        let bad = json.replace("75.0", "0.0");
        assert!(serde_json::from_str::<FixedClock>(&bad).is_err());
    }
}
