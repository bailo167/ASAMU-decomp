//! Behavioural trace format for parity work.
//!
//! A trace is a sequence of per-tick samples (input + resulting state) that
//! both the original game (recorded later on Windows) and this runtime can
//! produce, so a harness can compare them tick by tick. The on-disk form is
//! **JSON Lines**: line 1 is a [`TraceMeta`] object, every following non-blank
//! line is one [`TraceSample`]. See `docs/PARITY.md` for the field reference.
//!
//! Conventions: UE3 axes, distances in UU, velocities in UU/s, yaw/pitch in
//! radians (UE3 convention), FOV in degrees (horizontal). Sample `tick = k`
//! holds the input applied during tick `k` and the state at the end of it; a
//! recording may start with a sample whose input is neutral and whose state is
//! the initial state. Floats are written exactly (`asamu_core::exact_f32`) so
//! runtime traces round-trip bit-for-bit.

use std::io::{BufRead, Read, Write};

use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::grapple::GrappleState;
use crate::input::InputFrame;
use crate::movement::{MovementModel, PlaceholderMovement};
use crate::params::PlayerParams;
use crate::sim::{PlayerState, step_with};
use crate::world::CollisionWorld;

/// Value of [`TraceMeta::format`].
pub const TRACE_FORMAT: &str = "asamu-trace";
/// Current schema version.
pub const TRACE_SCHEMA_VERSION: u32 = 1;
/// The only supported distance unit.
pub const TRACE_UNITS: &str = "uu";
/// Longest accepted line (bytes, excluding the newline). A sample line is well
/// under 1 KiB; the bound keeps a hostile file from exhausting memory.
pub const MAX_LINE_BYTES: usize = 1 << 20;

/// Who produced a trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceSource {
    /// Recorded from the original game.
    Original,
    /// Produced by this runtime.
    Runtime,
}

/// First line of a trace file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceMeta {
    /// Always [`TRACE_FORMAT`].
    pub format: String,
    /// Schema version ([`TRACE_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Producer.
    pub source: TraceSource,
    /// Original game build (e.g. Steam build id) when known.
    #[serde(default)]
    pub game_build: Option<String>,
    /// Level / map name when known.
    #[serde(default)]
    pub level: Option<String>,
    /// Fixed tick rate in Hz, if the producer ran at one (`None` for
    /// variable-rate recordings).
    #[serde(default, with = "asamu_core::exact_f32::option")]
    pub tick_rate: Option<f32>,
    /// Distance unit; must be [`TRACE_UNITS`].
    pub units: String,
    /// Free-form notes (how it was recorded, caveats).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl TraceMeta {
    /// Metadata for a runtime trace.
    #[must_use]
    pub fn runtime(level: Option<String>, tick_rate: Option<f32>) -> Self {
        Self {
            format: TRACE_FORMAT.to_owned(),
            schema_version: TRACE_SCHEMA_VERSION,
            source: TraceSource::Runtime,
            game_build: None,
            level,
            tick_rate,
            units: TRACE_UNITS.to_owned(),
            notes: vec![
                "asamu runtime; placeholder physics (no original constants yet)".to_owned(),
            ],
        }
    }
}

/// Grapple state as recorded in a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceGrappleState {
    /// Not attached.
    Idle,
    /// Attached (sample carries `grapple_anchor`).
    Attached,
}

/// One tick of a trace.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSample {
    /// Tick index (strictly increasing within a trace).
    pub tick: u64,
    /// Time in seconds at the end of the tick (informational; samples are
    /// aligned by `tick`).
    pub time: f64,
    /// Input applied during the tick.
    pub input: InputFrame,
    /// Collision-centre position after the tick, UU.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub position: Vec3,
    /// Velocity after the tick, UU/s.
    #[serde(with = "asamu_core::exact_f32::vec3")]
    pub velocity: Vec3,
    /// View yaw, radians.
    #[serde(with = "asamu_core::exact_f32")]
    pub yaw: f32,
    /// View pitch, radians.
    #[serde(with = "asamu_core::exact_f32")]
    pub pitch: f32,
    /// Horizontal field of view, degrees.
    #[serde(with = "asamu_core::exact_f32")]
    pub fov: f32,
    /// Grapple state.
    pub grapple_state: TraceGrappleState,
    /// Anchor when attached, UU.
    #[serde(default, with = "asamu_core::exact_f32::option_vec3")]
    pub grapple_anchor: Option<Vec3>,
    /// Rope length when attached and known, UU.
    #[serde(default, with = "asamu_core::exact_f32::option")]
    pub rope_length: Option<f32>,
    /// On a walkable floor.
    pub grounded: bool,
}

impl TraceSample {
    /// Captures a sample from simulation state. The input is stored
    /// [sanitized](InputFrame::sanitized), i.e. exactly as the simulation
    /// consumed it, so replaying a trace reproduces the run.
    #[must_use]
    pub fn capture(
        tick: u64,
        time: f64,
        input: &InputFrame,
        state: &PlayerState,
        fov: f32,
    ) -> Self {
        let (grapple_state, grapple_anchor, rope_length) = match state.grapple {
            GrappleState::Idle => (TraceGrappleState::Idle, None, None),
            GrappleState::Attached {
                anchor,
                rope_length,
            } => (TraceGrappleState::Attached, Some(anchor), Some(rope_length)),
        };
        Self {
            tick,
            time,
            input: input.sanitized(),
            position: state.position,
            velocity: state.velocity,
            yaw: state.yaw,
            pitch: state.pitch,
            fov,
            grapple_state,
            grapple_anchor,
            rope_length,
            grounded: state.grounded,
        }
    }

    fn check_finite(&self) -> Result<(), TraceError> {
        let nf = |field: &'static str| TraceError::NonFinite {
            tick: self.tick,
            field,
        };
        if !self.time.is_finite() {
            return Err(nf("time"));
        }
        if !self.input.is_finite() {
            return Err(nf("input"));
        }
        if !self.position.is_finite() {
            return Err(nf("position"));
        }
        if !self.velocity.is_finite() {
            return Err(nf("velocity"));
        }
        if !self.yaw.is_finite() {
            return Err(nf("yaw"));
        }
        if !self.pitch.is_finite() {
            return Err(nf("pitch"));
        }
        if !self.fov.is_finite() {
            return Err(nf("fov"));
        }
        if self.grapple_anchor.is_some_and(|a| !a.is_finite()) {
            return Err(nf("grapple_anchor"));
        }
        if self.rope_length.is_some_and(|l| !l.is_finite()) {
            return Err(nf("rope_length"));
        }
        Ok(())
    }

    fn check_ranges(&self) -> Result<(), TraceError> {
        let invalid = |field: &'static str, requirement: &'static str| TraceError::InvalidValue {
            tick: self.tick,
            field,
            requirement,
        };
        if !(self.fov > 0.0 && self.fov < 180.0) {
            return Err(invalid("fov", "in (0, 180) degrees"));
        }
        if self.rope_length.is_some_and(|l| l < 0.0) {
            return Err(invalid("rope_length", ">= 0"));
        }
        Ok(())
    }
}

/// Trace read/write/validation errors.
#[derive(Debug, Error)]
pub enum TraceError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// No meta line.
    #[error("trace is empty (missing meta line)")]
    MissingMeta,
    /// The meta line is not a valid [`TraceMeta`].
    #[error("line {line}: invalid meta: {message}")]
    InvalidMeta {
        /// 1-based line number.
        line: usize,
        /// Parser message.
        message: String,
    },
    /// Wrong `format` marker.
    #[error("not an asamu trace (format {0:?}, expected {TRACE_FORMAT:?})")]
    WrongFormat(String),
    /// Unsupported schema version.
    #[error("unsupported schema version {found} (supported: {TRACE_SCHEMA_VERSION})")]
    UnsupportedSchema {
        /// Version found.
        found: u32,
    },
    /// Unsupported unit.
    #[error("unsupported units {0:?} (expected {TRACE_UNITS:?})")]
    UnsupportedUnits(String),
    /// Invalid tick rate in meta.
    #[error("invalid tick_rate {0} (must be finite and > 0)")]
    InvalidTickRate(f32),
    /// A sample line is not a valid [`TraceSample`].
    #[error("line {line}: invalid sample: {message}")]
    InvalidSample {
        /// 1-based line number.
        line: usize,
        /// Parser message.
        message: String,
    },
    /// Non-finite number in a sample.
    #[error("sample at tick {tick}: non-finite value in {field}")]
    NonFinite {
        /// Tick of the sample.
        tick: u64,
        /// Field name.
        field: &'static str,
    },
    /// A finite value outside its allowed range.
    #[error("sample at tick {tick}: {field} must be {requirement}")]
    InvalidValue {
        /// Tick of the sample.
        tick: u64,
        /// Field name.
        field: &'static str,
        /// What the value must satisfy.
        requirement: &'static str,
    },
    /// A line is not valid UTF-8.
    #[error("line {line}: not valid UTF-8")]
    InvalidUtf8 {
        /// 1-based line number.
        line: usize,
    },
    /// A line exceeds [`MAX_LINE_BYTES`].
    #[error("line {line}: longer than {MAX_LINE_BYTES} bytes")]
    LineTooLong {
        /// 1-based line number.
        line: usize,
    },
    /// Ticks must strictly increase.
    #[error("tick {tick} does not follow previous tick {previous}")]
    NonMonotonicTick {
        /// Previous tick.
        previous: u64,
        /// Offending tick.
        tick: u64,
    },
    /// `grapple_state` and `grapple_anchor` disagree.
    #[error("sample at tick {tick}: grapple_state and grapple_anchor disagree")]
    GrappleInconsistent {
        /// Tick of the sample.
        tick: u64,
    },
    /// Serialization failure.
    #[error("serialization failed: {0}")]
    Serialize(String),
}

/// A full trace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    /// Metadata (line 1 on disk).
    pub meta: TraceMeta,
    /// Samples in tick order.
    pub samples: Vec<TraceSample>,
}

impl Trace {
    /// An empty trace with `meta`.
    #[must_use]
    pub fn new(meta: TraceMeta) -> Self {
        Self {
            meta,
            samples: Vec::new(),
        }
    }

    /// Validates metadata and every sample.
    ///
    /// # Errors
    /// The first problem found.
    pub fn validate(&self) -> Result<(), TraceError> {
        validate_meta(&self.meta)?;
        let mut previous: Option<u64> = None;
        for s in &self.samples {
            validate_sample(s, &mut previous)?;
        }
        Ok(())
    }

    /// Writes JSON Lines (meta first). Validates before writing.
    ///
    /// # Errors
    /// Validation, serialization or I/O errors.
    pub fn write_jsonl<W: Write>(&self, mut w: W) -> Result<(), TraceError> {
        self.validate()?;
        let ser = |e: serde_json::Error| TraceError::Serialize(e.to_string());
        serde_json::to_writer(&mut w, &self.meta).map_err(ser)?;
        w.write_all(b"\n")?;
        for s in &self.samples {
            serde_json::to_writer(&mut w, s).map_err(ser)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        Ok(())
    }

    /// JSON Lines as a string.
    ///
    /// # Errors
    /// Validation or serialization errors.
    pub fn to_jsonl_string(&self) -> Result<String, TraceError> {
        let mut buf = Vec::new();
        self.write_jsonl(&mut buf)?;
        String::from_utf8(buf).map_err(|e| TraceError::Serialize(e.to_string()))
    }

    /// Reads and validates JSON Lines. Blank lines are ignored; `\n` and
    /// `\r\n` line endings are accepted.
    ///
    /// # Errors
    /// I/O, UTF-8, line-length, parse or validation errors (with 1-based line
    /// numbers where applicable).
    pub fn read_jsonl<R: BufRead>(mut r: R) -> Result<Self, TraceError> {
        let mut meta: Option<TraceMeta> = None;
        let mut samples = Vec::new();
        let mut previous: Option<u64> = None;
        let mut buf = Vec::new();
        let mut number = 0_usize;
        loop {
            buf.clear();
            // +2 so a maximal line plus "\r\n" still fits.
            let limit = u64::try_from(MAX_LINE_BYTES + 2).unwrap_or(u64::MAX);
            let read = (&mut r).take(limit).read_until(b'\n', &mut buf)?;
            if read == 0 {
                break;
            }
            number += 1;
            let mut bytes = buf.as_slice();
            if let Some(rest) = bytes.strip_suffix(b"\n") {
                bytes = rest;
            } else if buf.len() > MAX_LINE_BYTES {
                return Err(TraceError::LineTooLong { line: number });
            }
            if let Some(rest) = bytes.strip_suffix(b"\r") {
                bytes = rest;
            }
            if bytes.len() > MAX_LINE_BYTES {
                return Err(TraceError::LineTooLong { line: number });
            }
            let line =
                std::str::from_utf8(bytes).map_err(|_| TraceError::InvalidUtf8 { line: number })?;
            if line.trim().is_empty() {
                continue;
            }
            match &meta {
                None => {
                    let m: TraceMeta =
                        serde_json::from_str(line).map_err(|e| TraceError::InvalidMeta {
                            line: number,
                            message: e.to_string(),
                        })?;
                    validate_meta(&m)?;
                    meta = Some(m);
                }
                Some(_) => {
                    let s: TraceSample =
                        serde_json::from_str(line).map_err(|e| TraceError::InvalidSample {
                            line: number,
                            message: e.to_string(),
                        })?;
                    validate_sample(&s, &mut previous)?;
                    samples.push(s);
                }
            }
        }
        let meta = meta.ok_or(TraceError::MissingMeta)?;
        Ok(Self { meta, samples })
    }

    /// Parses JSON Lines from a string.
    ///
    /// # Errors
    /// See [`Self::read_jsonl`].
    pub fn from_jsonl_str(s: &str) -> Result<Self, TraceError> {
        Self::read_jsonl(s.as_bytes())
    }
}

fn validate_meta(m: &TraceMeta) -> Result<(), TraceError> {
    if m.format != TRACE_FORMAT {
        return Err(TraceError::WrongFormat(m.format.clone()));
    }
    if m.schema_version != TRACE_SCHEMA_VERSION {
        return Err(TraceError::UnsupportedSchema {
            found: m.schema_version,
        });
    }
    if m.units != TRACE_UNITS {
        return Err(TraceError::UnsupportedUnits(m.units.clone()));
    }
    if let Some(rate) = m.tick_rate
        && !(rate.is_finite() && rate > 0.0)
    {
        return Err(TraceError::InvalidTickRate(rate));
    }
    Ok(())
}

fn validate_sample(s: &TraceSample, previous: &mut Option<u64>) -> Result<(), TraceError> {
    s.check_finite()?;
    s.check_ranges()?;
    if let Some(p) = *previous
        && s.tick <= p
    {
        return Err(TraceError::NonMonotonicTick {
            previous: p,
            tick: s.tick,
        });
    }
    let consistent = match s.grapple_state {
        TraceGrappleState::Idle => s.grapple_anchor.is_none() && s.rope_length.is_none(),
        TraceGrappleState::Attached => s.grapple_anchor.is_some(),
    };
    if !consistent {
        return Err(TraceError::GrappleInconsistent { tick: s.tick });
    }
    *previous = Some(s.tick);
    Ok(())
}

/// Runs the simulation over `inputs` from `initial` and records a runtime
/// trace: one sample for the initial state (tick 0, neutral input) and one per
/// input (ticks 1..=n). Uses the default ([`PlaceholderMovement`]) model.
#[must_use]
pub fn record_run<W: CollisionWorld + ?Sized>(
    meta: TraceMeta,
    initial: &PlayerState,
    inputs: &[InputFrame],
    params: &PlayerParams,
    world: &W,
    dt: f32,
) -> Trace {
    record_run_with(
        &PlaceholderMovement,
        meta,
        initial,
        inputs,
        params,
        world,
        dt,
    )
}

/// [`record_run`] with an explicit locomotion model.
#[must_use]
pub fn record_run_with<M: MovementModel, W: CollisionWorld + ?Sized>(
    model: &M,
    meta: TraceMeta,
    initial: &PlayerState,
    inputs: &[InputFrame],
    params: &PlayerParams,
    world: &W,
    dt: f32,
) -> Trace {
    let fov = params.camera.fov_degrees.value;
    let mut trace = Trace::new(meta);
    let mut state = *initial;
    trace.samples.push(TraceSample::capture(
        0,
        0.0,
        &InputFrame::default(),
        &state,
        fov,
    ));
    for (i, input) in inputs.iter().enumerate() {
        step_with(model, &mut state, input, params, world, dt);
        let tick = i as u64 + 1;
        trace.samples.push(TraceSample::capture(
            tick,
            tick as f64 * f64::from(dt),
            input,
            &state,
            fov,
        ));
    }
    trace
}

/// Which field a divergence was detected in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceField {
    /// Position error (Euclidean distance, UU).
    Position,
    /// Velocity error (Euclidean, UU/s).
    Velocity,
    /// Yaw error (wrapped absolute difference, radians).
    Yaw,
    /// Pitch error (absolute difference, radians).
    Pitch,
    /// FOV error (absolute difference, degrees).
    Fov,
    /// Anchor error (Euclidean distance, UU; both attached).
    GrappleAnchor,
    /// Rope length error (absolute difference, UU; both attached with a
    /// known length).
    RopeLength,
    /// Grapple state differs.
    GrappleState,
    /// Grounded flag differs.
    Grounded,
    /// Inputs differ (the traces are not replays of the same input).
    Input,
}

/// Error statistics for one continuous field over matched ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldStats {
    /// Number of matched samples contributing.
    pub count: usize,
    /// Maximum error.
    pub max: f64,
    /// Tick at which `max` occurred (first occurrence).
    pub max_tick: Option<u64>,
    /// Mean error.
    pub mean: f64,
    /// Root-mean-square error.
    pub rms: f64,
    #[serde(skip)]
    sum: f64,
    #[serde(skip)]
    sum_sq: f64,
}

impl FieldStats {
    fn add(&mut self, tick: u64, err: f64) {
        self.count += 1;
        self.sum += err;
        self.sum_sq += err * err;
        if self.max_tick.is_none() || err > self.max {
            self.max = err;
            self.max_tick = Some(tick);
        }
    }

    fn finish(&mut self) {
        if self.count > 0 {
            let n = self.count as f64;
            self.mean = self.sum / n;
            self.rms = (self.sum_sq / n).sqrt();
        }
    }
}

/// Per-field tolerances used to find the first divergence. Defaults are all
/// zero (any difference counts): tolerances are an analysis choice to be made
/// per study, not a property of the game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompareTolerances {
    /// UU.
    pub position: f64,
    /// UU/s.
    pub velocity: f64,
    /// Radians (yaw and pitch).
    pub angle: f64,
    /// Degrees.
    pub fov: f64,
    /// UU (anchor position and rope length).
    pub anchor: f64,
}

/// First tick at which a field exceeded its tolerance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    /// Tick.
    pub tick: u64,
    /// Field.
    pub field: TraceField,
    /// Error value (1.0 for boolean / categorical mismatches).
    pub error: f64,
}

/// Result of [`compare`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TraceDiff {
    /// Ticks present in both traces.
    pub matched: usize,
    /// Ticks only in `a`.
    pub only_in_a: usize,
    /// Ticks only in `b`.
    pub only_in_b: usize,
    /// Position error stats (UU).
    pub position: FieldStats,
    /// Velocity error stats (UU/s).
    pub velocity: FieldStats,
    /// Yaw error stats (radians, wrapped).
    pub yaw: FieldStats,
    /// Pitch error stats (radians).
    pub pitch: FieldStats,
    /// FOV error stats (degrees).
    pub fov: FieldStats,
    /// Anchor error stats (UU) over ticks where both are attached.
    pub grapple_anchor: FieldStats,
    /// Rope length error stats (UU) over ticks where both are attached and
    /// both record a rope length.
    pub rope_length: FieldStats,
    /// Ticks where the grapple state differs.
    pub grapple_state_mismatches: usize,
    /// Ticks where `grounded` differs.
    pub grounded_mismatches: usize,
    /// Ticks where the inputs differ.
    pub input_mismatches: usize,
    /// First divergence above tolerance (any field), if any.
    pub first_divergence: Option<Divergence>,
}

impl TraceDiff {
    /// `true` if every matched tick agrees exactly and no tick is unmatched.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.only_in_a == 0
            && self.only_in_b == 0
            && self.position.max == 0.0
            && self.velocity.max == 0.0
            && self.yaw.max == 0.0
            && self.pitch.max == 0.0
            && self.fov.max == 0.0
            && self.grapple_anchor.max == 0.0
            && self.rope_length.max == 0.0
            && self.grapple_state_mismatches == 0
            && self.grounded_mismatches == 0
            && self.input_mismatches == 0
    }
}

fn vec_err(a: Vec3, b: Vec3) -> f64 {
    let d = a.as_dvec3() - b.as_dvec3();
    d.length()
}

fn angle_err(a: f32, b: f32) -> f64 {
    let d = (f64::from(a) - f64::from(b)).rem_euclid(core::f64::consts::TAU);
    d.min(core::f64::consts::TAU - d)
}

/// Compares two traces with zero tolerances (any difference is a divergence).
#[must_use]
pub fn compare(a: &Trace, b: &Trace) -> TraceDiff {
    compare_with(a, b, &CompareTolerances::default())
}

/// Compares two traces sample-by-sample, aligned by `tick`.
///
/// Both traces are expected to have strictly increasing ticks (guaranteed for
/// traces that passed [`Trace::validate`]); samples are matched with a linear
/// merge, so the result does not depend on any hash ordering.
#[must_use]
pub fn compare_with(a: &Trace, b: &Trace, tol: &CompareTolerances) -> TraceDiff {
    fn note(diff: &mut TraceDiff, tick: u64, field: TraceField, err: f64, limit: f64) {
        // NaN or negative tolerances behave as zero (strict); +inf ignores
        // the field for divergence purposes.
        let limit = limit.max(0.0);
        if err > limit && diff.first_divergence.is_none() {
            diff.first_divergence = Some(Divergence {
                tick,
                field,
                error: err,
            });
        }
    }
    let mut diff = TraceDiff::default();
    let (mut i, mut j) = (0, 0);
    while i < a.samples.len() && j < b.samples.len() {
        let (sa, sb) = (&a.samples[i], &b.samples[j]);
        if sa.tick < sb.tick {
            diff.only_in_a += 1;
            i += 1;
            continue;
        }
        if sb.tick < sa.tick {
            diff.only_in_b += 1;
            j += 1;
            continue;
        }
        let tick = sa.tick;
        diff.matched += 1;

        let e = vec_err(sa.position, sb.position);
        diff.position.add(tick, e);
        note(&mut diff, tick, TraceField::Position, e, tol.position);

        let e = vec_err(sa.velocity, sb.velocity);
        diff.velocity.add(tick, e);
        note(&mut diff, tick, TraceField::Velocity, e, tol.velocity);

        let e = angle_err(sa.yaw, sb.yaw);
        diff.yaw.add(tick, e);
        note(&mut diff, tick, TraceField::Yaw, e, tol.angle);

        let e = (f64::from(sa.pitch) - f64::from(sb.pitch)).abs();
        diff.pitch.add(tick, e);
        note(&mut diff, tick, TraceField::Pitch, e, tol.angle);

        let e = (f64::from(sa.fov) - f64::from(sb.fov)).abs();
        diff.fov.add(tick, e);
        note(&mut diff, tick, TraceField::Fov, e, tol.fov);

        if sa.grapple_state != sb.grapple_state {
            diff.grapple_state_mismatches += 1;
            note(&mut diff, tick, TraceField::GrappleState, 1.0, 0.0);
        } else {
            if let (Some(pa), Some(pb)) = (sa.grapple_anchor, sb.grapple_anchor) {
                let e = vec_err(pa, pb);
                diff.grapple_anchor.add(tick, e);
                note(&mut diff, tick, TraceField::GrappleAnchor, e, tol.anchor);
            }
            if let (Some(la), Some(lb)) = (sa.rope_length, sb.rope_length) {
                let e = (f64::from(la) - f64::from(lb)).abs();
                diff.rope_length.add(tick, e);
                note(&mut diff, tick, TraceField::RopeLength, e, tol.anchor);
            }
        }

        if sa.grounded != sb.grounded {
            diff.grounded_mismatches += 1;
            note(&mut diff, tick, TraceField::Grounded, 1.0, 0.0);
        }
        if sa.input != sb.input {
            diff.input_mismatches += 1;
            note(&mut diff, tick, TraceField::Input, 1.0, 0.0);
        }
        i += 1;
        j += 1;
    }
    diff.only_in_a += a.samples.len() - i;
    diff.only_in_b += b.samples.len() - j;
    for stats in [
        &mut diff.position,
        &mut diff.velocity,
        &mut diff.yaw,
        &mut diff.pitch,
        &mut diff.fov,
        &mut diff.grapple_anchor,
        &mut diff.rope_length,
    ] {
        stats.finish();
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(tick: u64) -> TraceSample {
        TraceSample {
            tick,
            time: tick as f64 / 60.0,
            input: InputFrame::default(),
            position: Vec3::new(tick as f32, 0.0, 45.05),
            velocity: Vec3::new(1.0, 2.0, 3.0),
            yaw: 0.1,
            pitch: -0.2,
            fov: 90.0,
            grapple_state: TraceGrappleState::Idle,
            grapple_anchor: None,
            rope_length: None,
            grounded: true,
        }
    }

    fn trace(n: u64) -> Trace {
        Trace {
            meta: TraceMeta::runtime(Some("test".into()), Some(60.0)),
            samples: (0..n).map(sample).collect(),
        }
    }

    #[test]
    fn round_trip_and_layout() {
        let t = trace(5);
        let s = t.to_jsonl_string().unwrap();
        assert_eq!(s.lines().count(), 6);
        assert!(
            s.lines()
                .next()
                .unwrap()
                .contains(r#""format":"asamu-trace""#)
        );
        let back = Trace::from_jsonl_str(&s).unwrap();
        assert_eq!(back, t);
        // Blank lines are tolerated.
        let spaced = s.replace('\n', "\n\n");
        assert_eq!(Trace::from_jsonl_str(&spaced).unwrap(), t);
    }

    #[test]
    fn validation_errors() {
        assert!(matches!(
            Trace::from_jsonl_str(""),
            Err(TraceError::MissingMeta)
        ));
        assert!(matches!(
            Trace::from_jsonl_str("{}\n"),
            Err(TraceError::InvalidMeta { line: 1, .. })
        ));
        let good = trace(3).to_jsonl_string().unwrap();
        let meta_line = good.lines().next().unwrap().to_owned();

        let wrong_units = good.replacen(r#""units":"uu""#, r#""units":"m""#, 1);
        assert!(matches!(
            Trace::from_jsonl_str(&wrong_units),
            Err(TraceError::UnsupportedUnits(u)) if u == "m"
        ));
        let wrong_version = good.replacen(r#""schema_version":1"#, r#""schema_version":99"#, 1);
        assert!(matches!(
            Trace::from_jsonl_str(&wrong_version),
            Err(TraceError::UnsupportedSchema { found: 99 })
        ));
        let wrong_format = good.replacen("asamu-trace", "other", 1);
        assert!(matches!(
            Trace::from_jsonl_str(&wrong_format),
            Err(TraceError::WrongFormat(_))
        ));

        let mut lines: Vec<String> = good.lines().map(str::to_owned).collect();
        lines.swap(1, 2);
        assert!(matches!(
            Trace::from_jsonl_str(&lines.join("\n")),
            Err(TraceError::NonMonotonicTick {
                previous: 1,
                tick: 0
            })
        ));

        let bad_sample = format!("{meta_line}\n{{\"tick\":0}}\n");
        assert!(matches!(
            Trace::from_jsonl_str(&bad_sample),
            Err(TraceError::InvalidSample { line: 2, .. })
        ));

        let mut t = trace(2);
        t.samples[1].grapple_state = TraceGrappleState::Attached;
        assert!(matches!(
            t.validate(),
            Err(TraceError::GrappleInconsistent { tick: 1 })
        ));

        let mut t = trace(2);
        t.samples[0].velocity.x = f32::NAN;
        assert!(matches!(
            t.to_jsonl_string(),
            Err(TraceError::NonFinite {
                tick: 0,
                field: "velocity"
            })
        ));

        let mut t = trace(1);
        t.meta.tick_rate = Some(0.0);
        assert!(matches!(t.validate(), Err(TraceError::InvalidTickRate(_))));
    }

    #[test]
    fn compare_identical_is_exact() {
        let t = trace(10);
        let d = compare(&t, &t);
        assert!(d.is_exact());
        assert_eq!(d.matched, 10);
        assert_eq!(d.first_divergence, None);
        assert_eq!(d.position.count, 10);
        assert_eq!(d.position.rms, 0.0);
    }

    #[test]
    fn compare_reports_errors_and_first_divergence() {
        let a = trace(10);
        let mut b = trace(12);
        b.samples.remove(0); // tick 0 only in a
        for s in &mut b.samples {
            if s.tick >= 4 {
                s.position.x += 3.0;
                s.position.y += 4.0; // error 5
            }
        }
        b.samples[6].grounded = false; // tick 7
        let d = compare(&a, &b);
        assert_eq!(d.matched, 9);
        assert_eq!(d.only_in_a, 1);
        assert_eq!(d.only_in_b, 2);
        assert_eq!(d.position.max, 5.0);
        assert_eq!(d.position.max_tick, Some(4));
        assert!((d.position.mean - 5.0 * 6.0 / 9.0).abs() < 1e-9);
        assert!((d.position.rms - (25.0 * 6.0 / 9.0_f64).sqrt()).abs() < 1e-9);
        assert_eq!(d.grounded_mismatches, 1);
        assert_eq!(
            d.first_divergence,
            Some(Divergence {
                tick: 4,
                field: TraceField::Position,
                error: 5.0
            })
        );
        // With a generous tolerance the first divergence is the grounded flip.
        let tol = CompareTolerances {
            position: 10.0,
            ..CompareTolerances::default()
        };
        let d = compare_with(&a, &b, &tol);
        assert_eq!(
            d.first_divergence.map(|x| (x.tick, x.field)),
            Some((7, TraceField::Grounded))
        );
        assert!(!d.is_exact());
    }

    #[test]
    fn yaw_error_wraps() {
        assert!(angle_err(3.1, -3.1) < 0.09);
        assert_eq!(angle_err(1.0, 1.0), 0.0);
        // Exact on constructed values (computed in f64 from the f32 inputs).
        let (a, b) = (3.0_f32, -3.0_f32);
        let expected = core::f64::consts::TAU - (f64::from(a) - f64::from(b));
        assert!((angle_err(a, b) - expected).abs() < 1e-12);
        assert!((angle_err(b, a) - expected).abs() < 1e-12);
        assert!((angle_err(0.25, -0.5) - 0.75).abs() < 1e-12);
        // Never more than π.
        let mut x = -10.0_f32;
        while x < 10.0 {
            assert!(angle_err(x, 0.0) <= core::f64::consts::PI + 1e-12);
            x += 0.37;
        }
    }

    /// Meta line + the given sample lines, joined with `\n`.
    fn file(samples: &[String]) -> String {
        let meta = serde_json::to_string(&TraceMeta::runtime(None, Some(60.0))).unwrap();
        let mut out = meta;
        for s in samples {
            out.push('\n');
            out.push_str(s);
        }
        out.push('\n');
        out
    }

    fn sample_line(tick: u64) -> String {
        serde_json::to_string(&sample(tick)).unwrap()
    }

    #[test]
    fn malformed_lines_are_rejected_with_line_numbers() {
        let good = sample_line(1);
        let cases: Vec<(String, &str)> = vec![
            ("not json at all".to_owned(), "garbage"),
            (good[..good.len() / 2].to_owned(), "truncated"),
            (format!("{good} trailing"), "trailing garbage"),
            (format!("{good}{good}"), "two objects on one line"),
            (good.replacen("{", r#"{"tick":5,"#, 1), "duplicate key"),
            (good.replacen("{", r#"{"extra":1,"#, 1), "unknown field"),
            (
                good.replacen(
                    r#""grapple_held":false"#,
                    r#""grapple_held":false,"x":0"#,
                    1,
                ),
                "unknown input field",
            ),
            (
                good.replacen(r#""tick":1"#, r#""tick":"1""#, 1),
                "wrong type",
            ),
            (
                good.replacen(r#""tick":1"#, r#""tick":-1"#, 1),
                "negative tick",
            ),
            (good.replacen(r#""fov":90.0,"#, "", 1), "missing field"),
            (
                good.replacen("\"yaw\":", "\"yaw\":NaN,\"_\":", 1),
                "NaN literal",
            ),
            (
                good.replacen(
                    r#""grapple_state":"idle""#,
                    r#""grapple_state":"swinging""#,
                    1,
                ),
                "unknown enum variant",
            ),
            (
                good.replacen("\"position\":[", "\"position\":[1.0,", 1),
                "vec3 with 4 elements",
            ),
            ("[]".to_owned(), "array instead of object"),
        ];
        for (line, what) in cases {
            assert_ne!(line, good, "{what}: the corruption did not apply");
            let text = file(&[sample_line(0), line]);
            match Trace::from_jsonl_str(&text) {
                Err(TraceError::InvalidSample { line: 3, message }) => {
                    assert!(!message.is_empty(), "{what}");
                }
                other => panic!("{what}: expected InvalidSample on line 3, got {other:?}"),
            }
        }
        // A second meta line where a sample is expected.
        let meta = file(&[]).trim_end().to_owned();
        assert!(matches!(
            Trace::from_jsonl_str(&format!("{meta}\n{meta}\n")),
            Err(TraceError::InvalidSample { line: 2, .. })
        ));
    }

    #[test]
    fn meta_schema_checks() {
        let base = file(&[sample_line(0)]);
        for (from, to) in [
            (r#""schema_version":1"#, r#""schema_version":0"#),
            (r#""schema_version":1"#, r#""schema_version":2"#),
        ] {
            let text = base.replacen(from, to, 1);
            assert_ne!(text, base);
            assert!(
                matches!(
                    Trace::from_jsonl_str(&text),
                    Err(TraceError::UnsupportedSchema { .. })
                ),
                "{to}"
            );
        }
        for (from, to, what) in [
            (
                r#""schema_version":1"#,
                r#""schema_version":-1"#,
                "negative",
            ),
            (
                r#""schema_version":1"#,
                r#""schema_version":1.5"#,
                "fractional",
            ),
            (r#""schema_version":1,"#, "", "missing"),
            (
                r#""source":"runtime""#,
                r#""source":"emulator""#,
                "bad source",
            ),
            (r#""format""#, r#""format":"x","format""#, "duplicate key"),
        ] {
            let text = base.replacen(from, to, 1);
            assert_ne!(text, base, "{what}: the corruption did not apply");
            assert!(
                matches!(
                    Trace::from_jsonl_str(&text),
                    Err(TraceError::InvalidMeta { line: 1, .. })
                ),
                "{what}: {:?}",
                Trace::from_jsonl_str(&text)
            );
        }
        let text = base.replacen(r#""tick_rate":60.0"#, r#""tick_rate":-60.0"#, 1);
        assert_ne!(text, base);
        assert!(matches!(
            Trace::from_jsonl_str(&text),
            Err(TraceError::InvalidTickRate(_))
        ));
        // Leading blank lines before the meta are fine; the line number of a
        // later error still counts them.
        let text = format!("\n\n{}", file(&["oops".to_owned()]));
        assert!(matches!(
            Trace::from_jsonl_str(&text),
            Err(TraceError::InvalidSample { line: 4, .. })
        ));
    }

    #[test]
    fn crlf_utf8_and_line_length() {
        let text = file(&[sample_line(0), sample_line(1)]);
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(
            Trace::from_jsonl_str(&crlf).unwrap(),
            Trace::from_jsonl_str(&text).unwrap()
        );
        // No trailing newline on the last line.
        let trimmed = text.trim_end();
        assert_eq!(Trace::from_jsonl_str(trimmed).unwrap().samples.len(), 2);

        let mut bytes = file(&[sample_line(0)]).into_bytes();
        bytes.extend_from_slice(b"{\"tick\":\xff}\n");
        assert!(matches!(
            Trace::read_jsonl(bytes.as_slice()),
            Err(TraceError::InvalidUtf8 { line: 3 })
        ));

        let long = format!(
            "{}\n{}\n",
            file(&[]).trim_end(),
            " ".repeat(MAX_LINE_BYTES + 1)
        );
        assert!(matches!(
            Trace::from_jsonl_str(&long),
            Err(TraceError::LineTooLong { line: 2 })
        ));
        // Exactly at the limit (blank) is accepted.
        let at_limit = format!(
            "{}\n{}\r\n",
            file(&[]).trim_end(),
            " ".repeat(MAX_LINE_BYTES)
        );
        assert!(Trace::from_jsonl_str(&at_limit).unwrap().samples.is_empty());
    }

    #[test]
    fn value_checks() {
        let mut t = trace(2);
        t.samples[1].yaw = f32::INFINITY;
        assert!(matches!(
            t.validate(),
            Err(TraceError::NonFinite {
                tick: 1,
                field: "yaw"
            })
        ));
        // A JSON number that overflows f32 becomes infinite and is rejected.
        let line = sample_line(0).replacen(r#""fov":90.0"#, r#""fov":1e39"#, 1);
        assert_ne!(line, sample_line(0));
        let text = file(&[line]);
        assert!(matches!(
            Trace::from_jsonl_str(&text),
            Err(TraceError::NonFinite { field: "fov", .. })
        ));
        let mut t = trace(1);
        t.samples[0].fov = 0.0;
        assert!(matches!(
            t.validate(),
            Err(TraceError::InvalidValue { field: "fov", .. })
        ));
        let mut t = trace(1);
        t.samples[0].grapple_state = TraceGrappleState::Attached;
        t.samples[0].grapple_anchor = Some(Vec3::ONE);
        t.samples[0].rope_length = Some(-1.0);
        assert!(matches!(
            t.validate(),
            Err(TraceError::InvalidValue {
                field: "rope_length",
                ..
            })
        ));
        // Idle with a rope length, attached without an anchor.
        let mut t = trace(1);
        t.samples[0].rope_length = Some(10.0);
        assert!(matches!(
            t.validate(),
            Err(TraceError::GrappleInconsistent { tick: 0 })
        ));
        let mut t = trace(1);
        t.samples[0].grapple_state = TraceGrappleState::Attached;
        assert!(matches!(
            t.validate(),
            Err(TraceError::GrappleInconsistent { tick: 0 })
        ));
        // Duplicate ticks are not strictly increasing.
        let text = file(&[sample_line(3), sample_line(3)]);
        assert!(matches!(
            Trace::from_jsonl_str(&text),
            Err(TraceError::NonMonotonicTick {
                previous: 3,
                tick: 3
            })
        ));
    }

    fn attached(mut s: TraceSample, anchor: Vec3, rope: Option<f32>) -> TraceSample {
        s.grapple_state = TraceGrappleState::Attached;
        s.grapple_anchor = Some(anchor);
        s.rope_length = rope;
        s
    }

    #[test]
    fn metrics_on_constructed_examples() {
        // a has ticks 0, 2, 4; b has ticks 1..=5.
        let a = Trace {
            meta: TraceMeta::runtime(None, Some(60.0)),
            samples: [0, 2, 4].map(sample).to_vec(),
        };
        let mut b = Trace {
            meta: TraceMeta::runtime(None, Some(60.0)),
            samples: (1..=5).map(sample).collect(),
        };
        // Tick 2: velocity off by (3, 4, 0) -> 5; tick 4: off by 0.
        b.samples[1].velocity += Vec3::new(3.0, 4.0, 0.0);
        // Tick 4: pitch off by 0.5, fov off by 2.
        b.samples[3].pitch += 0.5;
        b.samples[3].fov += 2.0;
        let d = compare(&a, &b);
        assert_eq!((d.matched, d.only_in_a, d.only_in_b), (2, 1, 3));
        assert_eq!(d.velocity.count, 2);
        assert_eq!(d.velocity.max, 5.0);
        assert_eq!(d.velocity.max_tick, Some(2));
        assert_eq!(d.velocity.mean, 2.5);
        assert!((d.velocity.rms - 12.5_f64.sqrt()).abs() < 1e-12);
        assert_eq!(d.position.max, 0.0);
        assert_eq!(d.position.max_tick, Some(2), "ties keep the first tick");
        let pitch_err = (f64::from(-0.2_f32 + 0.5) - f64::from(-0.2_f32)).abs();
        assert_eq!(d.pitch.max, pitch_err);
        assert_eq!(d.pitch.max_tick, Some(4));
        assert_eq!(d.fov.max, 2.0);
        assert_eq!(
            d.first_divergence,
            Some(Divergence {
                tick: 2,
                field: TraceField::Velocity,
                error: 5.0
            })
        );
        // Within a tick, fields are checked in a fixed order (position first).
        let mut c = a.clone();
        c.samples[1].position.z += 1.0;
        c.samples[1].velocity.z += 1.0;
        assert_eq!(
            compare(&a, &c).first_divergence.map(|x| x.field),
            Some(TraceField::Position)
        );
        // Negative / NaN tolerances act as zero; +inf ignores a field.
        let tol = CompareTolerances {
            position: f64::NAN,
            velocity: -1.0,
            angle: f64::INFINITY,
            fov: f64::INFINITY,
            anchor: 0.0,
        };
        assert_eq!(compare_with(&a, &a, &tol).first_divergence, None);
        assert_eq!(
            compare_with(&a, &c, &tol).first_divergence.map(|x| x.field),
            Some(TraceField::Position)
        );
    }

    #[test]
    fn grapple_metrics() {
        let base = trace(4);
        let mut a = base.clone();
        let mut b = base.clone();
        a.samples[1] = attached(a.samples[1], Vec3::new(0.0, 0.0, 100.0), Some(50.0));
        b.samples[1] = attached(b.samples[1], Vec3::new(0.0, 3.0, 104.0), Some(47.5));
        a.samples[2] = attached(a.samples[2], Vec3::ZERO, Some(10.0));
        b.samples[2] = attached(b.samples[2], Vec3::ZERO, None);
        // Tick 3: state mismatch (anchor/rope not compared).
        a.samples[3] = attached(a.samples[3], Vec3::ZERO, Some(1.0));
        b.samples[3].input.grapple_held = true;
        let d = compare(&a, &b);
        assert_eq!(d.grapple_anchor.count, 2);
        assert_eq!(d.grapple_anchor.max, 5.0);
        assert_eq!(d.grapple_anchor.max_tick, Some(1));
        assert_eq!(d.rope_length.count, 1, "only where both record a length");
        assert_eq!(d.rope_length.max, 2.5);
        assert_eq!(d.grapple_state_mismatches, 1);
        assert_eq!(d.input_mismatches, 1);
        assert_eq!(
            d.first_divergence.map(|x| (x.tick, x.field)),
            Some((1, TraceField::GrappleAnchor))
        );
        let tol = CompareTolerances {
            anchor: 10.0,
            ..CompareTolerances::default()
        };
        assert_eq!(
            compare_with(&a, &b, &tol)
                .first_divergence
                .map(|x| (x.tick, x.field)),
            Some((3, TraceField::GrappleState))
        );
        assert!(!d.is_exact());
        // Empty traces compare as exact with no matches.
        let e = Trace::new(TraceMeta::runtime(None, None));
        let d = compare(&e, &e);
        assert!(d.is_exact() && d.matched == 0 && d.position.count == 0);
    }
}
