//! Frame lengths of a trace: the per-tick `dt` of a variable-rate recording.
//!
//! Without benchmark mode the original runs every frame with its own
//! `DeltaSeconds` (the Windows recordings; `tick_rate: null`). Schema v1 has
//! no per-sample `dt` field and needs none: a sample's `time` is the time at
//! the end of its tick, so the length of the tick of sample `k` is
//! `time[k] − time[k−1]`.
//!
//! # Exactness
//!
//! [`crate::convert`] writes `time` as the `f64` sum of the recorded `f32`
//! `WorldInfo.DeltaSeconds` values. An `f32` of at least 2⁻¹³ s (0.12 ms,
//! i.e. a frame rate below 8192 fps) is a multiple of 2⁻³⁶ s, and so is every
//! partial sum; below 2¹⁷ s (36 h) such a sum fits the 53 bits of an `f64`.
//! Inside those bounds every addition is exact, the difference of two
//! neighbouring times is the recorded value, and rounding it to `f32` gives
//! back the bits the game used ([`FrameLength::of`]; tested). The JSON text
//! round-trips `f64` exactly. Outside the bounds the difference is the
//! nearest `f32` instead of the exact one.
//!
//! # What the simulation does with a frame length
//!
//! [`FrameLength`] names the three cases of `asamu_player::begin_step`: a
//! positive length is stepped as it is; a length above [`MAX_STEP_DT`]
//! (0.25 s) is stepped as `MAX_STEP_DT`, our numerical safety bound; a
//! length that is not positive is a tick in which no time passes (the sample
//! is still written). A replay reports the counts of the last two in its
//! notes.
//!
//! The original clamps every frame's dilated length to 0.0005..0.4 s before
//! any actor ticks (`UWorld::Tick`; CONFIRMED by disassembly in the Mac and
//! the Windows build, see [`crate::convert::raw_timing`]), and that clamped
//! value is what the recorder reads. So a recording of the original has no frame that is not
//! positive, and its frames between 0.25 and 0.4 s (a hitch below 4 fps) are
//! the ones our bound shortens: a known deviation of the replay on exactly
//! those ticks, which is why they are counted.

use asamu_player::{MAX_STEP_DT, TraceSample};
use serde::{Deserialize, Serialize};

/// Two frame lengths differing by more than this fraction of the larger one
/// are different steps ([`same_length`]).
///
/// An analysis threshold, not a game value: it is far above the rounding of
/// one length to `f32` (6e-8) and of a fixed rate's `1 / rate` (2e-8 at
/// 60 Hz), and far below the frame-to-frame variation of a real variable-rate
/// recording.
pub const DT_REL_EPS: f64 = 1e-6;

/// What one tick of a variable-rate replay does with its frame length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameLength {
    /// Stepped with this `dt`, seconds.
    Step(f32),
    /// Longer than [`MAX_STEP_DT`] (or infinite): stepped with `MAX_STEP_DT`.
    Clamped,
    /// Not positive, or not a number: no time passes in this tick.
    NoOp,
}

impl FrameLength {
    /// Classifies a frame length in seconds (a difference of two sample
    /// times). The `f64` is rounded to the nearest `f32` first, so a length
    /// too small for an `f32` is [`FrameLength::NoOp`].
    #[must_use]
    pub fn of(seconds: f64) -> Self {
        let dt = seconds as f32;
        if dt.is_nan() || dt <= 0.0 {
            Self::NoOp
        } else if dt > MAX_STEP_DT {
            Self::Clamped
        } else {
            Self::Step(dt)
        }
    }

    /// The `dt` handed to the simulation (0 for [`FrameLength::NoOp`], which
    /// the simulation treats as "nothing happens").
    #[must_use]
    pub fn dt(self) -> f32 {
        match self {
            Self::Step(dt) => dt,
            Self::Clamped => MAX_STEP_DT,
            Self::NoOp => 0.0,
        }
    }
}

/// `true` when two frame lengths are the same step (see [`DT_REL_EPS`]).
/// Two zero lengths are the same; a length that is not a number never is.
#[must_use]
pub fn same_length(a: f64, b: f64) -> bool {
    (a - b).abs() <= DT_REL_EPS * a.abs().max(b.abs())
}

/// A value that JSON can hold: infinities become the largest finite number,
/// not-a-number becomes 0. (Sample times are finite, but the difference of
/// two huge ones can overflow.)
#[must_use]
pub fn finite(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(-f64::MAX, f64::MAX)
    }
}

/// Frame-length statistics of one trace (seconds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepStats {
    /// Ticks measured: samples whose tick directly follows the previous
    /// sample's (a tick after a gap has no length).
    pub steps: usize,
    /// Shortest frame (0 without steps).
    pub min: f64,
    /// Longest frame (0 without steps).
    pub max: f64,
    /// Mean frame length (0 without steps).
    pub mean: f64,
    /// Sum of the measured frame lengths.
    pub duration: f64,
}

impl StepStats {
    /// Statistics of a list of frame lengths.
    #[must_use]
    pub fn of_lengths(lengths: impl IntoIterator<Item = f64>) -> Self {
        let (mut steps, mut min, mut max, mut sum) =
            (0_usize, f64::INFINITY, f64::NEG_INFINITY, 0.0);
        for d in lengths {
            let d = finite(d);
            steps += 1;
            min = min.min(d);
            max = max.max(d);
            sum += d;
        }
        if steps == 0 {
            return Self::default();
        }
        let sum = finite(sum);
        Self {
            steps,
            min,
            max,
            mean: sum / steps as f64,
            duration: sum,
        }
    }

    /// Statistics of the frame lengths of `samples`.
    #[must_use]
    pub fn of_samples(samples: &[TraceSample]) -> Self {
        Self::of_lengths(frame_lengths(samples).map(|(_, d)| d))
    }

    /// `true` when every measured frame has the same length (also without
    /// steps).
    #[must_use]
    pub fn uniform(&self) -> bool {
        same_length(self.min, self.max)
    }

    /// `"0.014200..0.019800 s (mean 0.016700 s)"`, or one value when uniform.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.steps == 0 {
            "no frame lengths".to_owned()
        } else if self.uniform() {
            format!("{:.6} s", self.mean)
        } else {
            format!(
                "{:.6}..{:.6} s (mean {:.6} s)",
                self.min, self.max, self.mean
            )
        }
    }
}

/// `(tick, frame length)` of every sample that directly follows the previous
/// sample (`tick = previous tick + 1`): the length is the difference of the
/// two sample times, in seconds.
pub fn frame_lengths(samples: &[TraceSample]) -> impl Iterator<Item = (u64, f64)> + '_ {
    samples.windows(2).filter_map(|w| match w {
        [a, b] if a.tick.checked_add(1) == Some(b.tick) => Some((b.tick, b.time - a.time)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_core::glam::Vec3;
    use asamu_player::InputFrame;
    use asamu_player::trace::TraceGrappleState;

    fn sample(tick: u64, time: f64) -> TraceSample {
        TraceSample {
            tick,
            time,
            input: InputFrame::default(),
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            fov: 90.0,
            grapple_state: TraceGrappleState::Idle,
            grapple_anchor: None,
            rope_length: None,
            grounded: true,
        }
    }

    /// Deterministic pseudo-random numbers.
    struct Lcg(u64);

    impl Lcg {
        fn unit(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 40) as f32) / (1u64 << 24) as f32
        }
    }

    #[test]
    fn classification() {
        let d60 = 1.0_f32 / 60.0;
        assert_eq!(FrameLength::of(f64::from(d60)), FrameLength::Step(d60));
        assert_eq!(FrameLength::of(f64::from(d60)).dt(), d60);
        assert_eq!(
            FrameLength::of(f64::from(MAX_STEP_DT)),
            FrameLength::Step(MAX_STEP_DT)
        );
        for long in [0.2500001, 0.4, 1.0, 1e30, 1e300, f64::INFINITY] {
            assert_eq!(FrameLength::of(long), FrameLength::Clamped, "{long}");
            assert_eq!(FrameLength::of(long).dt(), MAX_STEP_DT);
        }
        // Not positive, not a number, or below the smallest f32.
        for none in [
            0.0,
            -0.0,
            -1.0 / 60.0,
            -1e300,
            f64::NEG_INFINITY,
            f64::NAN,
            1e-60,
        ] {
            assert_eq!(FrameLength::of(none), FrameLength::NoOp, "{none}");
            assert_eq!(FrameLength::of(none).dt(), 0.0);
        }
        // Tiny but representable lengths are steps.
        assert_eq!(FrameLength::of(1e-6), FrameLength::Step(1e-6));
        assert!(matches!(FrameLength::of(1e-40), FrameLength::Step(d) if d > 0.0));
    }

    /// The f64 sum of f32 frame lengths gives every length back bit for bit
    /// (the module docs' bounds), also through the JSON text.
    #[test]
    fn time_differences_recover_the_recorded_f32_lengths() {
        let mut r = Lcg(7);
        // 0.5 ms .. 0.4 s (stock UE3's frame clamp range), with clusters
        // around common frame rates.
        let lengths: Vec<f32> = (0..200_000)
            .map(|i| match i % 4 {
                0 => 0.0005 + r.unit() * 0.3995,
                1 => 1.0 / 60.0 + (r.unit() - 0.5) * 0.004,
                2 => 1.0 / 144.0 + (r.unit() - 0.5) * 0.001,
                _ => 1.0 / 30.0 + (r.unit() - 0.5) * 0.01,
            })
            .collect();
        let mut time = 0.0_f64;
        let mut samples = vec![sample(0, 0.0)];
        for (i, d) in lengths.iter().enumerate() {
            time += f64::from(*d);
            samples.push(sample(i as u64 + 1, time));
        }
        assert!(time > 10_000.0, "hours of frames: {time}");
        let mut n = 0;
        for ((tick, d), want) in frame_lengths(&samples).zip(&lengths) {
            assert_eq!((d as f32).to_bits(), want.to_bits(), "tick {tick}");
            // The same through the JSON number.
            let text = serde_json::to_string(&samples[tick as usize].time).unwrap();
            let back: f64 = serde_json::from_str(&text).unwrap();
            assert_eq!(back.to_bits(), samples[tick as usize].time.to_bits());
            n += 1;
        }
        assert_eq!(n, lengths.len());
        // A runtime trace's times are tick / rate: their differences round
        // to the clock's f32 step.
        for rate in [24.0_f64, 30.0, 50.0, 60.0, 62.0, 75.0, 120.0, 144.0, 240.0] {
            let step = (1.0 / rate) as f32;
            for k in 0..100_000_u64 {
                let d = (k + 1) as f64 / rate - k as f64 / rate;
                assert_eq!((d as f32).to_bits(), step.to_bits(), "{rate} Hz tick {k}");
            }
        }
    }

    #[test]
    fn statistics_and_gaps() {
        let s = vec![
            sample(10, 5.0),
            sample(11, 5.25),
            sample(12, 5.75),
            // Gap: tick 13 is missing, so tick 14 has no length.
            sample(14, 9.0),
            sample(15, 9.25),
        ];
        let lengths: Vec<(u64, f64)> = frame_lengths(&s).collect();
        assert_eq!(lengths, [(11, 0.25), (12, 0.5), (15, 0.25)]);
        let st = StepStats::of_samples(&s);
        assert_eq!(
            st,
            StepStats {
                steps: 3,
                min: 0.25,
                max: 0.5,
                mean: 1.0 / 3.0,
                duration: 1.0
            }
        );
        assert!(!st.uniform());
        assert_eq!(st.describe(), "0.250000..0.500000 s (mean 0.333333 s)");
        let u = StepStats::of_lengths([0.02, 0.02 + 1e-10]);
        assert!(u.uniform());
        assert_eq!(u.describe(), "0.020000 s");
        let none = StepStats::of_samples(&s[..1]);
        assert_eq!(none, StepStats::default());
        assert!(none.uniform());
        assert_eq!(none.describe(), "no frame lengths");
        assert_eq!(frame_lengths(&[]).count(), 0);
        // u64::MAX has no successor.
        assert_eq!(
            frame_lengths(&[sample(u64::MAX, 0.0), sample(0, 1.0)]).count(),
            0
        );
    }

    #[test]
    fn same_length_and_finite() {
        let d = f64::from(1.0_f32 / 60.0);
        assert!(same_length(d, 1.0 / 60.0), "f32 rounding of 1/60");
        assert!(same_length(0.0, 0.0));
        assert!(!same_length(d, d * 1.00001));
        assert!(!same_length(0.0, 1e-9));
        assert!(!same_length(f64::NAN, f64::NAN));
        assert!(!same_length(f64::INFINITY, f64::INFINITY));
        assert_eq!(finite(f64::INFINITY), f64::MAX);
        assert_eq!(finite(f64::NEG_INFINITY), -f64::MAX);
        assert_eq!(finite(f64::NAN), 0.0);
        assert_eq!(finite(1.5), 1.5);
        // Overflowing differences stay serialisable.
        let st = StepStats::of_samples(&[sample(0, -1e308), sample(1, 1e308), sample(2, -1e308)]);
        assert_eq!((st.min, st.max), (-f64::MAX, f64::MAX));
        let text = serde_json::to_string(&st).unwrap();
        assert_eq!(serde_json::from_str::<StepStats>(&text).unwrap(), st);
    }
}
