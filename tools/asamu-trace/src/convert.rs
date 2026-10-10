//! Raw recording → canonical trace (`asamu-trace` v1, `source = original`).
//!
//! # Frame alignment
//!
//! Record `R_i` is taken at the start of frame `F_i` (`UWorld::Tick` entry,
//! after the frame's input was dispatched). So `R_i` holds the **state at the
//! end of frame `F_i − 1`** and the **input of frame `F_i`** (held keys,
//! `bPressedJump`); its `WorldInfo.DeltaSeconds` is the length of frame
//! `F_i − 1`. A run of consecutive records `R_0..R_n` becomes samples
//! `0..n`:
//!
//! - sample 0: the state of `R_0` (the initial state), neutral input, time 0;
//! - sample `k ≥ 1` (frame `F_0 + k − 1`): input from the keys of `R_(k−1)`
//!   (edges against `R_(k−2)`), look deltas from the view rotation change
//!   `R_(k−1) → R_k`, state from `R_k`, time `Σ DeltaSeconds(R_1..R_k)`.
//!
//! # Units and fields
//!
//! Rotator units → radians (`((u + 32768) mod 65536) − 32768`, × 2π/65536,
//! rounded to `f32`); look deltas are wrapped differences of the controller's
//! rotation, so they are exact and independent of mouse scaling. Positions
//! (UU), velocities (UU/s) and FOV (degrees) are copied. `grounded` ⇔
//! `Physics == PHYS_Walking (1)`. Attached ⇔ the gun's `bIsGrappling`,
//! anchor = the gun's `vGrappleLocation`: the world hit point, which the gun
//! moves along with the anchor helper only for a target that carries its
//! anchor (GRAPPLE.md G-AT-4, G-AT-7, G-AT-8). The helper's own location
//! (raw `anchor`) is **not** the anchor otherwise: it stays where an earlier
//! moving target left it. Measured on the recordings of 2026-10-10: the
//! gun's own `vDistance` equals the pawn's distance to `vGrappleLocation`
//! within 0.01 uu on 9,594 of 9,601 attached records (the other 7, up to
//! 6.1 uu off, are the first record of a grapple), to the helper within 1 uu
//! on 5,071 only (`tests/real_recordings.rs` prints the counts). Conversions made
//! before this was measured used the helper. Both disagreements are counted
//! in the notes. Without
//! gun data (the pawn's weapon is not a `GrappleGun`) no anchor is known, so
//! the sample is idle even in `PHYS_Flying (4)`; such samples are counted in
//! a `check:` note. `rope_length` is always `null` (the original grapple has
//! no rope). FOV = the camera's POV FOV, else the controller's `FOVAngle`.
//!
//! # Move axes
//!
//! `move_forward` / `move_right` come from the held keys through the game's
//! bindings, or, for a recording made with a gamepad (whose stick is neither
//! a key nor readable from the `PlayerInput` axes at the sample point), from
//! the pawn's `Acceleration` of `R_k` in the axes of `R_(k−1)`'s pawn
//! rotation ([`crate::move_input`], which has the evidence and says where
//! the result is not defined). [`MoveInput::Auto`] decides per run: the keys
//! if any record of the run holds a key bound to a move axis, else the
//! acceleration if the run has any; the notes say which and count the
//! samples of each kind. A keyboard recording therefore keeps its keys.
//!
//! # Script state
//!
//! The `state:` note ([`crate::state`]) holds what the recorder read beyond
//! the sample fields (`GroundSpeed`, `AirControl`, the sprint flag, the
//! grapple budget, ...) for every tick, so that [`crate::replay`] can start
//! from any tick; the `init:` note is the older form for tick 0 only.
//!
//! # Frame lengths
//!
//! The tick of sample `k` has the length `WorldInfo.DeltaSeconds` of `R_k`
//! (the dilated length of the frame that produced `R_k`'s state), an `f32`.
//! `time` is the `f64` sum of those lengths, so the lengths can be read back
//! from the sample times, bit for bit for any realistic recording
//! ([`crate::timestep`]). When every frame of a run has the same length
//! (benchmark mode) the trace gets that fixed `tick_rate`; when the lengths
//! differ (the original without benchmark mode, as on Windows) `tick_rate`
//! is `null` and [`crate::replay`] steps with each sample's own length. No
//! frame length is rejected except one that is not finite. The recorder also
//! stores the tick's own `DeltaSeconds` argument (`dt_arg`, undilated and
//! before the engine's 0.0005..0.4 s clamp); [`raw_timing`] cross-checks it
//! against the next record's `DeltaSeconds`.
//!
//! # Segments
//!
//! A run ends at a frame gap (missed or paused frames), at a frame without a
//! player, when the map, the pawn object or the controller/pawn class
//! changes, and when the world's clock (`WorldInfo.TimeSeconds`) goes back.
//! A world's clock never does; a smaller value is another world: the level
//! was loaded again (a restart from the menu), and the reloaded level can
//! leave its `WorldInfo` and pawn at the addresses the old ones had, so
//! neither the map name nor the pawn number shows it. (A recorder stopped at
//! one breakpoint per frame sees consecutive frame numbers across such a
//! load; the polling recorder drops the load frame anyway.) Runs shorter
//! than two records are dropped. Each run is one trace; replays stop at the
//! end of a trace, never inside a gap.
//!
//! `tools/trace-recorder/asamu_recorder_core.py` (`convert_raw`) implements
//! the same conversion for the recorder; `tests/python_crosscheck.rs` checks
//! that both produce the same traces.

use anyhow::{Result, bail};
use asamu_core::glam::Vec3;
use asamu_core::rotator::{normalize_rotator_axis, rotator_units_to_radians};
use asamu_player::trace::{TRACE_FORMAT, TRACE_SCHEMA_VERSION, TRACE_UNITS, TraceGrappleState};
use asamu_player::{InputFrame, Trace, TraceMeta, TraceSample};
use serde::Serialize;

use crate::bindings::{Actions, KeyMap};
use crate::move_input::{AXES_TABLE_STEP, MoveFrame, horizontal_magnitude, move_direction};
use crate::raw::{RawFile, RawPlayer, RawRecord};
use crate::state::state_note;
use crate::timestep::StepStats;

/// `Physics` value of walking (NATIVE_PHYSICS.md 1.3, CONFIRMED).
pub const PHYS_WALKING: u8 = 1;
/// `Physics` value of flying (the grapple; GRAPPLE.md, CONFIRMED).
pub const PHYS_FLYING: u8 = 4;
/// Prefix of the notes line carrying the initial script state as JSON.
pub const INIT_NOTE_PREFIX: &str = "init: ";

/// One converted run.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// The canonical trace.
    pub trace: Trace,
    /// `GFrameCounter` of the first record.
    pub first_frame: u64,
    /// `GFrameCounter` of the last record.
    pub last_frame: u64,
}

/// Where the move axes of a converted run come from (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveInput {
    /// Per run: the keys if any record holds a key bound to a move axis,
    /// else the acceleration if the run has any, else the keys.
    #[default]
    Auto,
    /// The held keys through the bindings (without bindings: the signs of
    /// the `PlayerInput` axes).
    Keys,
    /// The pawn's acceleration ([`crate::move_input`]).
    Acceleration,
}

/// Options of [`convert`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Overrides the level name taken from the recording.
    pub level: Option<String>,
    /// Source of the move axes.
    pub move_input: MoveInput,
    /// Axes an acceleration is read in.
    pub move_frame: MoveFrame,
}

/// The initial script state written as the `init:` note (keys sorted).
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct InitState {
    /// `AirControl`.
    #[serde(with = "asamu_core::exact_f32")]
    pub air_control: f32,
    /// Base actor name.
    pub base: Option<String>,
    /// `GroundSpeed`.
    #[serde(with = "asamu_core::exact_f32")]
    pub ground_speed: f32,
    /// `JumpZ`.
    #[serde(with = "asamu_core::exact_f32")]
    pub jump_z: f32,
    /// `iMaxGrapples`.
    pub max_grapples: Option<i32>,
    /// `Physics`.
    pub physics: u8,
    /// Rocket boots enabled.
    pub rocket_boots: Option<bool>,
    /// `bSprinting`.
    pub sprinting: Option<bool>,
    /// `iTimesGrappled`.
    pub times_grappled: Option<i32>,
}

impl InitState {
    fn of(p: &RawPlayer) -> Self {
        Self {
            air_control: p.air_control,
            base: p.base.clone(),
            ground_speed: p.ground_speed,
            jump_z: p.jump_z,
            max_grapples: p.gun.map(|g| g.max_grapples),
            physics: p.physics,
            rocket_boots: p.boots.map(|b| b.enabled),
            sprinting: p.pawn_flags.map(|f| f.sprinting),
            times_grappled: p.gun.map(|g| g.times_grappled),
        }
    }

    /// Finds and parses the `init:` note of a trace, if any.
    #[must_use]
    pub fn from_notes(notes: &[String]) -> Option<Self> {
        notes
            .iter()
            .rev()
            .find_map(|n| n.strip_prefix(INIT_NOTE_PREFIX))
            .and_then(|j| serde_json::from_str(j).ok())
    }
}

/// Rotator units → radians, wrapped to one turn.
#[must_use]
pub fn units_to_radians(units: i32) -> f32 {
    rotator_units_to_radians(normalize_rotator_axis(units))
}

fn delta_radians(to: i32, from: i32) -> f32 {
    units_to_radians(to.wrapping_sub(from))
}

/// Splits records into convertible runs (see the module docs).
#[must_use]
pub fn split_runs(records: &[RawRecord]) -> Vec<&[RawRecord]> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (i, r) in records.iter().enumerate() {
        if !r.usable() {
            if let Some(s) = start.take() {
                runs.push(&records[s..i]);
            }
            continue;
        }
        if let Some(s) = start
            && let Some(prev) = i.checked_sub(1).and_then(|j| records.get(j))
            && !continues(prev, r)
        {
            runs.push(&records[s..i]);
            start = None;
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        runs.push(&records[s..]);
    }
    runs.retain(|r| r.len() >= 2);
    runs
}

fn continues(prev: &RawRecord, r: &RawRecord) -> bool {
    let (Some(pw), Some(w), Some(pp), Some(p)) = (&prev.world, &r.world, &prev.player, &r.player)
    else {
        return false;
    };
    prev.frame.checked_add(1) == Some(r.frame)
        && pw.map == w.map
        && pp.pawn_id == p.pawn_id
        && pp.controller_class == p.controller_class
        && pp.pawn_class == p.pawn_class
        // Not `<`: a clock that is not a number continues nothing.
        && w.time_seconds >= pw.time_seconds
}

#[derive(Default)]
struct Counters {
    fov_controller: usize,
    grapple_physics: usize,
    flying_without_gun: usize,
    axis_mismatch: usize,
    /// Keys mode: samples without a move from the keys whose record has a
    /// horizontal acceleration (and no attached grapple).
    accel_without_key: usize,
    /// Acceleration mode: samples with a derived direction.
    derived: usize,
    /// Of those, the pawn rotation could not be inverted.
    steep: usize,
    /// Of those, the previous record's pawn yaw is not its view yaw.
    yaw_differs: usize,
    /// Acceleration mode: zero acceleration while attached.
    zero_attached: usize,
    /// Zero acceleration within two frames after an attached record.
    zero_release: usize,
    /// Zero acceleration otherwise.
    zero_other: usize,
    /// A non-zero acceleration while attached (not used).
    accel_attached: usize,
    /// Attached samples (the first sample included).
    attached: usize,
    /// Of those, the anchor helper is more than 1 uu from the anchor.
    helper_elsewhere: usize,
    /// Of those, the gun's `vDistance` is more than 1 uu off the pawn's
    /// distance to the anchor.
    anchor_distance: usize,
}

/// Why a run's move axes come from where they do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveSource {
    /// Asked for: keys.
    AskedKeys,
    /// Asked for: acceleration.
    AskedAcceleration,
    /// Auto: this many records hold a move key.
    AutoKeys(usize),
    /// Auto: no move key, but acceleration.
    AutoAcceleration,
    /// Auto: neither a move key nor acceleration.
    AutoNothing,
    /// Auto without bindings: the axes' signs, as before.
    NoBindings,
}

impl MoveSource {
    fn uses_acceleration(self) -> bool {
        matches!(self, Self::AskedAcceleration | Self::AutoAcceleration)
    }
}

/// The grapple holds the pawn (no steering is read then, GRAPPLE.md G-PH-1).
fn is_attached(p: &RawPlayer) -> bool {
    p.physics == PHYS_FLYING || p.gun.is_some_and(|g| g.grappling)
}

/// `floor(x + 0.5)` as an integer for a note (saturating; mirrored by the
/// Python converter).
fn rounded(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

fn player(r: &RawRecord) -> Result<&RawPlayer> {
    match &r.player {
        Some(p) => Ok(p),
        None => bail!("frame {}: no player", r.frame),
    }
}

fn delta_seconds(r: &RawRecord) -> Result<f32> {
    match &r.world {
        Some(w) => Ok(w.delta_seconds),
        None => bail!("frame {}: no world", r.frame),
    }
}

/// State fields of a sample from one record.
struct State {
    position: Vec3,
    velocity: Vec3,
    yaw: f32,
    pitch: f32,
    fov: f32,
    grapple_state: TraceGrappleState,
    grapple_anchor: Option<Vec3>,
    grounded: bool,
}

fn state(r: &RawRecord, c: &mut Counters) -> Result<State> {
    let p = player(r)?;
    let valid = |f: f32| f > 0.0 && f < 180.0;
    let fov = match p.fov_camera {
        Some(f) if valid(f) => f,
        _ => {
            c.fov_controller += 1;
            if !valid(p.fov_controller) {
                bail!("frame {}: no usable field of view", r.frame);
            }
            p.fov_controller
        }
    };
    let flying = p.physics == PHYS_FLYING;
    // Without gun data there is no anchor, and an attached sample needs one.
    let (attached, anchor) = match &p.gun {
        Some(g) => {
            if g.grappling != flying {
                c.grapple_physics += 1;
            }
            if g.grappling {
                c.attached += 1;
                if g.anchor
                    .is_some_and(|helper| distance_squared(helper, g.grapple_location) > 1.0)
                {
                    c.helper_elsewhere += 1;
                }
                let to_anchor = distance_squared(g.grapple_location, p.location).sqrt();
                if (to_anchor - f64::from(g.distance)).abs() > 1.0 {
                    c.anchor_distance += 1;
                }
            }
            (g.grappling, Some(g.grapple_location))
        }
        None => {
            if flying {
                c.flying_without_gun += 1;
            }
            (false, None)
        }
    };
    Ok(State {
        position: p.location,
        velocity: p.velocity,
        yaw: units_to_radians(p.view_rotation[1]),
        pitch: units_to_radians(p.view_rotation[0]),
        fov,
        grapple_state: if attached {
            TraceGrappleState::Attached
        } else {
            TraceGrappleState::Idle
        },
        grapple_anchor: if attached { anchor } else { None },
        grounded: p.physics == PHYS_WALKING,
    })
}

/// Squared distance of two points in `f64` (as the Python converter
/// computes it).
fn distance_squared(a: Vec3, b: Vec3) -> f64 {
    let (dx, dy, dz) = (
        f64::from(a.x) - f64::from(b.x),
        f64::from(a.y) - f64::from(b.y),
        f64::from(a.z) - f64::from(b.z),
    );
    dx * dx + dy * dy + dz * dz
}

fn sample(tick: u64, time: f64, input: InputFrame, s: State) -> TraceSample {
    TraceSample {
        tick,
        time,
        input,
        position: s.position,
        velocity: s.velocity,
        yaw: s.yaw,
        pitch: s.pitch,
        fov: s.fov,
        grapple_state: s.grapple_state,
        grapple_anchor: s.grapple_anchor,
        rope_length: None,
        grounded: s.grounded,
    }
}

fn sign(v: f32) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// Tick rate in Hz if every frame had the same length (rounded to an integer
/// when within 0.001 Hz of one), else `None`.
#[must_use]
pub fn tick_rate_of(dts: &[f32]) -> Option<f32> {
    let d = *dts.first()?;
    if d <= 0.0 || dts.iter().any(|x| x.to_bits() != d.to_bits()) {
        return None;
    }
    let rate = 1.0 / f64::from(d);
    let r = (rate + 0.5).floor();
    Some(if (rate - r).abs() < 1e-3 {
        r as f32
    } else {
        rate as f32
    })
}

/// Shortest frame the original simulates, seconds: `UWorld::Tick` raises a
/// shorter (dilated) frame length to this before any actor ticks.
/// CONFIRMED in both builds of Steam build 1822049 (the `f32` constant the
/// clamp reads: Mac 0x101718EB4, read at 0x100910BEC; Windows VA 0x226E034,
/// read at VA 0xA358AC — see [`raw_timing`]).
pub const ORIGINAL_MIN_FRAME: f32 = 0.0005;
/// Longest frame the original simulates, seconds: `UWorld::Tick` lowers a
/// longer (dilated) frame length to this. CONFIRMED in both builds (Mac
/// 0x1016E9200, read at 0x100910BE4; Windows VA 0x22340C4, read at VA
/// 0xA358B9).
pub const ORIGINAL_MAX_FRAME: f32 = 0.4;

/// The frame length `UWorld::Tick` gives the actors for a tick argument
/// `dt_arg` under `time_dilation`: the product, clamped to
/// [`ORIGINAL_MIN_FRAME`]..[`ORIGINAL_MAX_FRAME`] (during demo playback, which
/// the recorder never sees, the engine also multiplies by a second
/// dilation).
#[must_use]
pub fn original_frame_length(dt_arg: f32, time_dilation: f32) -> f32 {
    let x = dt_arg * time_dilation;
    if x < ORIGINAL_MIN_FRAME {
        ORIGINAL_MIN_FRAME
    } else {
        x.min(ORIGINAL_MAX_FRAME)
    }
}

/// Frame timing of a raw recording ([`raw_timing`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawTiming {
    /// The frame lengths conversion uses (`WorldInfo.DeltaSeconds` of every
    /// record after the first of its run), seconds.
    pub lengths: StepStats,
    /// Frames with a recorded tick argument: record `R_(k−1)` has `dt_arg`
    /// and `R_k` follows it in the same run.
    pub arg_checked: usize,
    /// Of those, the frames where `DeltaSeconds(R_k)` is exactly
    /// [`original_frame_length`] of `dt_arg(R_(k−1))` and
    /// `TimeDilation(R_(k−1))`.
    pub arg_equal: usize,
    /// Of the equal ones, the frames the clamp changed (shorter than
    /// [`ORIGINAL_MIN_FRAME`] or longer than [`ORIGINAL_MAX_FRAME`]).
    pub arg_clamped: usize,
}

/// Frame lengths of the convertible runs of `raw`, and the cross-check of
/// each frame's tick argument against the `DeltaSeconds` the world shows
/// afterwards.
///
/// The argument (`dt_arg`) is what `UWorld::Tick` was called with; the
/// world's `DeltaSeconds` is what the actors were ticked with. `UWorld::Tick`
/// multiplies the argument by `WorldInfo.TimeDilation`, clamps the result to
/// 0.0005..0.4 s, stores it as `WorldInfo.DeltaSeconds` and adds it to
/// `TimeSeconds` unless paused. CONFIRMED by disassembly in both builds:
///
/// | | Mac (x86_64) | Windows (Win32) |
/// |---|---|---|
/// | the sequence | 0x100910BA7..0x100910C29 in `UWorld::Tick(ELevelTick, float)` | VA 0xA3586B..0xA358F6, in the function that starts at VA 0xA35450 (RVA 0x635450; the start is CONFIRMED: it is the target of the direct call at VA 0x9898BE in `UGameEngine::Tick`, made with `GWorld` in `ecx` right after `UObject::StaticTick`; the only code that reads both constants) |
/// | `TimeDilation` / `TimeSeconds` / `RealTimeSeconds` / `DeltaSeconds` | +0x530 / +0x538 / +0x53C / +0x544 | +0x41C / +0x424 / +0x428 / +0x430 |
///
/// Reproduce (Mac): `objdump -d
/// --disassemble-symbols=__ZN6UWorld4TickE10ELevelTickf <executable>`;
/// (Windows) search `.text` for the one place that reads both `f32`
/// constants within a few instructions and disassemble around it.
/// A recording where argument and `DeltaSeconds` do not agree shows that the
/// step changed in some other way; conversion uses `DeltaSeconds` in any
/// case.
#[must_use]
pub fn raw_timing(raw: &RawFile) -> RawTiming {
    let mut lengths = Vec::new();
    let mut t = RawTiming::default();
    for run in split_runs(&raw.records) {
        for w in run.windows(2) {
            let [cur, next] = w else { continue };
            let (Some(cw), Some(nw)) = (&cur.world, &next.world) else {
                continue;
            };
            lengths.push(f64::from(nw.delta_seconds));
            if let Some(arg) = cur.dt_arg {
                t.arg_checked += 1;
                let product = arg * cw.time_dilation;
                if original_frame_length(arg, cw.time_dilation).to_bits()
                    == nw.delta_seconds.to_bits()
                {
                    t.arg_equal += 1;
                    if product.to_bits() != nw.delta_seconds.to_bits() {
                        t.arg_clamped += 1;
                    }
                }
            }
        }
    }
    t.lengths = StepStats::of_lengths(lengths);
    t
}

/// Converts a raw recording into one trace per usable run.
///
/// # Errors
/// A frame without a usable field of view, or a trace that fails validation.
pub fn convert(raw: &RawFile, opts: &ConvertOptions) -> Result<Vec<Segment>> {
    let h = &raw.header;
    let keymap = (!h.bindings.is_empty()).then(|| KeyMap::from_bindings(&h.bindings));
    let runs = split_runs(&raw.records);
    let total = runs.len();
    let mut out = Vec::with_capacity(total);
    for (si, run) in runs.iter().enumerate() {
        let mut c = Counters::default();
        let players: Vec<&RawPlayer> = run.iter().map(player).collect::<Result<_>>()?;
        // Samples with an input: every one but the first.
        let inputs = run.len() - 1;
        let move_key_records = keymap.as_ref().map_or(0, |m| {
            players
                .iter()
                .take(inputs)
                .filter(|p| m.has_move_key(&p.keys))
                .count()
        });
        let has_acceleration = players
            .iter()
            .skip(1)
            .any(|p| horizontal_magnitude(p.acceleration.x, p.acceleration.y) > 0.0);
        let source = match opts.move_input {
            MoveInput::Keys => MoveSource::AskedKeys,
            MoveInput::Acceleration => MoveSource::AskedAcceleration,
            MoveInput::Auto if keymap.is_none() => MoveSource::NoBindings,
            MoveInput::Auto if move_key_records > 0 => MoveSource::AutoKeys(move_key_records),
            MoveInput::Auto if has_acceleration => MoveSource::AutoAcceleration,
            MoveInput::Auto => MoveSource::AutoNothing,
        };
        // (tick, horizontal magnitude) of every derived sample.
        let mut magnitudes: Vec<(usize, f64)> = Vec::new();
        let mut samples = Vec::with_capacity(run.len());
        samples.push(sample(
            0,
            0.0,
            InputFrame::default(),
            state(&run[0], &mut c)?,
        ));
        let mut time = 0.0_f64;
        let mut dts = Vec::with_capacity(run.len());
        for k in 1..run.len() {
            let next = &run[k];
            let prev = k.checked_sub(2).and_then(|j| run.get(j));
            let dt = delta_seconds(next)?;
            dts.push(dt);
            time += f64::from(dt);
            let (cp, np) = (players[k - 1], players[k]);
            let input_actions: Actions;
            let (jump_held, key_edge, use_pressed);
            match &keymap {
                Some(m) => {
                    input_actions = m.actions(&cp.keys);
                    let pa = prev.map(player).transpose()?.map(|p| m.actions(&p.keys));
                    if !source.uses_acceleration()
                        && let Some(ax) = &np.axes
                        && (sign(ax.base_y) != input_actions.forward
                            || sign(ax.strafe) != input_actions.right)
                    {
                        c.axis_mismatch += 1;
                    }
                    jump_held = input_actions.jump;
                    key_edge = pa.is_some_and(|pa| input_actions.jump && !pa.jump);
                    use_pressed = pa.is_some_and(|pa| input_actions.use_ && !pa.use_);
                }
                None => {
                    let ax = np.axes;
                    input_actions = Actions {
                        forward: ax.map_or(0, |a| sign(a.base_y)),
                        right: ax.map_or(0, |a| sign(a.strafe)),
                        ..Actions::default()
                    };
                    jump_held = false;
                    key_edge = false;
                    use_pressed = false;
                }
            }
            let magnitude = horizontal_magnitude(np.acceleration.x, np.acceleration.y);
            let (move_forward, move_right) = if source.uses_acceleration() {
                let direction = if is_attached(np) {
                    None
                } else {
                    move_direction(
                        cp.pawn_rotation,
                        np.acceleration.x,
                        np.acceleration.y,
                        opts.move_frame,
                    )
                };
                match direction {
                    Some(d) => {
                        c.derived += 1;
                        c.steep += usize::from(d.steep);
                        c.yaw_differs += usize::from(
                            normalize_rotator_axis(cp.pawn_rotation[1])
                                != normalize_rotator_axis(cp.view_rotation[1]),
                        );
                        magnitudes.push((k, magnitude));
                        (d.forward, d.right)
                    }
                    None => {
                        if is_attached(np) {
                            if magnitude > 0.0 {
                                c.accel_attached += 1;
                            } else {
                                c.zero_attached += 1;
                            }
                        } else if is_attached(cp)
                            || prev.map(player).transpose()?.is_some_and(is_attached)
                        {
                            c.zero_release += 1;
                        } else {
                            c.zero_other += 1;
                        }
                        (0.0, 0.0)
                    }
                }
            } else {
                if keymap.is_some()
                    && input_actions.forward == 0
                    && input_actions.right == 0
                    && magnitude > 0.0
                    && !is_attached(np)
                {
                    c.accel_without_key += 1;
                }
                (input_actions.forward as f32, input_actions.right as f32)
            };
            let prev_flag = prev
                .map(player)
                .transpose()?
                .is_some_and(|p| p.pressed_jump);
            let flag_edge = cp.pressed_jump && !prev_flag;
            let input = InputFrame {
                move_forward,
                move_right,
                look_yaw_delta: delta_radians(np.view_rotation[1], cp.view_rotation[1]),
                look_pitch_delta: delta_radians(np.view_rotation[0], cp.view_rotation[0]),
                jump_pressed: key_edge || flag_edge,
                jump_held,
                grapple_held: input_actions.grapple,
                sprint_held: input_actions.sprint,
                power_jump_held: input_actions.power_jump,
                use_pressed,
            };
            samples.push(sample(k as u64, time, input, state(next, &mut c)?));
        }
        let mut notes = vec![format!(
            "raw: {}; sample point: {}",
            h.recorder, h.sample_point
        )];
        if let Some(l) = h.launch_options.as_deref().filter(|s| !s.is_empty()) {
            notes.push(format!("launch options: {l}"));
        }
        if let Some(s) = h.scenario.as_deref().filter(|s| !s.is_empty()) {
            notes.push(format!("scenario: {s}"));
        }
        let (first, last) = (run[0].frame, run[run.len() - 1].frame);
        notes.push(format!(
            "frames {first}..={last} (segment {} of {total})",
            si + 1
        ));
        if keymap.is_some() {
            notes.push(format!(
                "input: key bindings read from the running game ({} bindings)",
                h.bindings.len()
            ));
            if c.axis_mismatch > 0 {
                notes.push(format!(
                    "check: aBaseY/aStrafe sign disagrees with the mapped keys on {} of {} samples",
                    c.axis_mismatch,
                    run.len() - 1
                ));
            }
        } else {
            notes.push(
                "input: no key bindings in the recording; move axes from the signs of \
                 PlayerInput aBaseY/aStrafe, jump from bPressedJump, other actions unknown (false)"
                    .to_owned(),
            );
        }
        move_notes(&mut notes, source, opts.move_frame, &c, &magnitudes, inputs);
        if c.grapple_physics > 0 {
            notes.push(format!(
                "check: grapple flag and PHYS_Flying disagree on {} samples",
                c.grapple_physics
            ));
        }
        if c.helper_elsewhere > 0 {
            notes.push(format!(
                "anchor: the gun's vGrappleLocation; the anchor helper is more than 1 uu from it on \
                 {} of {} attached samples (it carries the anchor of a moving target only)",
                c.helper_elsewhere, c.attached
            ));
        }
        if c.anchor_distance > 0 {
            notes.push(format!(
                "check: the gun's vDistance is more than 1 uu off the pawn's distance to the \
                 anchor on {} of {} attached samples",
                c.anchor_distance, c.attached
            ));
        }
        if c.flying_without_gun > 0 {
            notes.push(format!(
                "check: PHYS_Flying without grapple gun data on {} samples (written as idle: no anchor)",
                c.flying_without_gun
            ));
        }
        if c.fov_controller > 0 {
            notes.push(format!(
                "fov: controller FOVAngle used on {} samples (camera POV FOV unavailable)",
                c.fov_controller
            ));
        }
        notes.extend(h.notes.iter().cloned());
        notes.push(state_note(&players)?);
        let init = InitState::of(players[0]);
        notes.push(format!(
            "{INIT_NOTE_PREFIX}{}",
            serde_json::to_string(&init)?
        ));
        let level = opts
            .level
            .clone()
            .or_else(|| run[0].world.as_ref().and_then(|w| w.map.clone()));
        let trace = Trace {
            meta: TraceMeta {
                format: TRACE_FORMAT.to_owned(),
                schema_version: TRACE_SCHEMA_VERSION,
                source: asamu_player::trace::TraceSource::Original,
                game_build: h.game_build.clone(),
                level,
                tick_rate: tick_rate_of(&dts),
                units: TRACE_UNITS.to_owned(),
                notes,
            },
            samples,
        };
        trace.validate()?;
        out.push(Segment {
            trace,
            first_frame: first,
            last_frame: last,
        });
    }
    Ok(out)
}

/// The notes about the move axes of one run (`total` samples with input).
fn move_notes(
    notes: &mut Vec<String>,
    source: MoveSource,
    frame: MoveFrame,
    c: &Counters,
    magnitudes: &[(usize, f64)],
    total: usize,
) {
    let why = match source {
        MoveSource::NoBindings => return,
        MoveSource::AskedKeys => "--move-input keys".to_owned(),
        MoveSource::AskedAcceleration => "--move-input acceleration".to_owned(),
        MoveSource::AutoKeys(n) => format!("auto: a move key is held on {n} records"),
        MoveSource::AutoAcceleration => "auto: no move key in this run".to_owned(),
        MoveSource::AutoNothing => "auto: no move key and no acceleration in this run".to_owned(),
    };
    if !source.uses_acceleration() {
        notes.push(format!("move input: the mapped move keys ({why})"));
        if c.accel_without_key > 0 {
            notes.push(format!(
                "check: acceleration without a mapped move key on {} of {total} samples",
                c.accel_without_key
            ));
        }
        return;
    }
    let axes = match frame {
        MoveFrame::Original => format!(
            "in the axes of the previous record's pawn rotation, each angle truncated to \
             {AXES_TABLE_STEP} rotator units"
        ),
        MoveFrame::Yaw => "in the previous record's exact pawn yaw, without pitch and roll \
                           (--move-frame yaw: reproduces the acceleration's direction, not the \
                           stick's)"
            .to_owned(),
    };
    notes.push(format!(
        "move input: derived from the pawn's Acceleration ({why}): its horizontal direction \
         {axes}; magnitude 1 (the stick's is not recorded); {} of {total} samples",
        c.derived
    ));
    notes.push(format!(
        "move input: 0 on {} samples with zero acceleration: {} while the grapple is attached \
         (no steering is read then), {} within two frames after an attached record (the \
         controller's release gap), {} others (no deflection beyond the dead zone, or a move \
         suppressed in a way the record does not show)",
        c.zero_attached + c.zero_release + c.zero_other,
        c.zero_attached,
        c.zero_release,
        c.zero_other
    ));
    if c.accel_attached > 0 {
        notes.push(format!(
            "check: acceleration while the grapple is attached on {} samples (not used)",
            c.accel_attached
        ));
    }
    let largest = magnitudes.iter().map(|m| m.1).fold(0.0_f64, f64::max);
    let below: Vec<usize> = magnitudes
        .iter()
        .filter(|m| m.1 < 0.5 * largest)
        .map(|m| m.0)
        .collect();
    if let Some(first) = below.first() {
        notes.push(format!(
            "check: acceleration below half the run's largest ({} uu/s^2) on {} derived samples \
             (direction used; first at tick {first})",
            rounded(largest),
            below.len()
        ));
    }
    if c.steep > 0 {
        notes.push(format!(
            "check: pawn rotation too steep to invert on {} samples (yaw-only axes used)",
            c.steep
        ));
    }
    if c.yaw_differs > 0 {
        notes.push(format!(
            "check: pawn yaw differs from the view yaw on {} derived samples",
            c.yaw_differs
        ));
    }
}

/// Output file names for `count` segments of `raw_name` (mirrors the Python
/// recorder): `<stem>.trace.jsonl`, or `<stem>.seg<i>.trace.jsonl`.
#[must_use]
pub fn output_names(raw_name: &str, count: usize) -> Vec<String> {
    let stem = raw_name
        .strip_suffix(".raw.jsonl")
        .or_else(|| raw_name.rsplit_once('.').map(|(s, _)| s))
        .unwrap_or(raw_name);
    if count == 1 {
        vec![format!("{stem}.trace.jsonl")]
    } else {
        (0..count)
            .map(|i| format!("{stem}.seg{i}.trace.jsonl"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::tests::{header, player, record};
    use crate::raw::{RawAxes, RawRecord};

    fn raw(records: Vec<RawRecord>) -> RawFile {
        RawFile {
            header: header(),
            records,
        }
    }

    fn scripted() -> Vec<RawRecord> {
        let mut r = vec![
            record(100, player(0.0, 0, &[])),
            record(101, player(0.0, 0, &["W"])),
            record(102, player(7.25, 182, &["W", "SpaceBar"])),
            record(103, player(20.5, 65536 + 364, &["LeftMouseButton", "E"])),
            record(104, player(40.0, 364, &["LeftMouseButton"])),
        ];
        let p3 = r[3].player.as_mut().unwrap();
        p3.physics = 2;
        let p4 = r[4].player.as_mut().unwrap();
        p4.physics = PHYS_FLYING;
        p4.view_rotation[0] = -1000;
        let g = p4.gun.as_mut().unwrap();
        g.grappling = true;
        // The helper is where an earlier moving target left it; the anchor
        // is the gun's own hit location.
        g.anchor = Some(Vec3::new(-7000.0, 12.0, 3.0));
        g.grapple_location = Vec3::new(500.0, 0.0, 0.0);
        g.distance = (Vec3::new(500.0, 0.0, 0.0) - p4.location).length();
        r
    }

    #[test]
    fn converts_scripted_frames() {
        let segs = convert(&raw(scripted()), &ConvertOptions::default()).unwrap();
        assert_eq!(segs.len(), 1);
        let s = &segs[0];
        assert_eq!((s.first_frame, s.last_frame), (100, 104));
        let t = &s.trace;
        assert_eq!(t.meta.source, asamu_player::trace::TraceSource::Original);
        assert_eq!(t.meta.level.as_deref(), Some("AG-Workshop"));
        assert_eq!(t.meta.tick_rate, Some(60.0));
        assert_eq!(t.meta.game_build.as_deref(), Some("steam-1822049-mac"));
        let ticks: Vec<u64> = t.samples.iter().map(|s| s.tick).collect();
        assert_eq!(ticks, [0, 1, 2, 3, 4]);
        let x = &t.samples;
        assert_eq!(x[0].input, InputFrame::default());
        assert_eq!(x[1].input.move_forward, 0.0, "frame 100 had no keys");
        assert_eq!(x[2].input.move_forward, 1.0, "frame 101: W");
        assert!(x[3].input.jump_pressed && x[3].input.jump_held);
        assert_eq!(x[3].input.look_yaw_delta, units_to_radians(182));
        assert!(x[4].input.grapple_held && x[4].input.use_pressed);
        assert!(!x[4].input.jump_pressed);
        assert_eq!(
            x[4].input.look_yaw_delta, 0.0,
            "65536 + 364 → 364 is no turn"
        );
        assert!(x[2].grounded && !x[3].grounded);
        assert_eq!(x[4].grapple_state, TraceGrappleState::Attached);
        // The anchor is the gun's hit location, not the helper's (which the
        // scripted record has 7,500 uu away); the gun's distance fits it.
        assert_eq!(x[4].grapple_anchor, Some(Vec3::new(500.0, 0.0, 0.0)));
        assert!(
            t.meta.notes.iter().any(|n| n
                == "anchor: the gun's vGrappleLocation; the anchor helper is more than 1 uu \
                    from it on 1 of 1 attached samples (it carries the anchor of a moving \
                    target only)"),
            "{:#?}",
            t.meta.notes
        );
        assert!(!t.meta.notes.iter().any(|n| n.contains("vDistance")));
        assert_eq!(x[4].rope_length, None);
        assert_eq!(x[4].pitch, units_to_radians(-1000));
        assert_eq!(x[2].position.x, 7.25);
        assert_eq!(x[1].time, f64::from(1.0_f32 / 60.0));
        let init = InitState::from_notes(&t.meta.notes).unwrap();
        assert_eq!(init.max_grapples, Some(2));
        assert_eq!(init.rocket_boots, Some(true));
        assert_eq!(init.physics, 1);
        // Round trip through the canonical format is exact.
        let text = t.to_jsonl_string().unwrap();
        assert_eq!(&Trace::from_jsonl_str(&text).unwrap(), t);
    }

    #[test]
    fn segments_at_gaps_pauses_and_changes() {
        let mut r = scripted();
        r[3].frame += 5;
        r[4].frame += 5;
        let segs = convert(&raw(r.clone()), &ConvertOptions::default()).unwrap();
        assert_eq!(
            segs.iter()
                .map(|s| s.trace.samples.len())
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert!(
            segs[1]
                .trace
                .meta
                .notes
                .iter()
                .any(|n| n == "frames 108..=109 (segment 2 of 2)")
        );

        let mut r = scripted();
        r[2].world.as_mut().unwrap().paused = true;
        let segs = convert(&raw(r), &ConvertOptions::default()).unwrap();
        assert_eq!(
            segs.iter()
                .map(|s| (s.first_frame, s.trace.samples.len()))
                .collect::<Vec<_>>(),
            [(100, 2), (103, 2)],
            "the paused frame splits the run"
        );

        let mut r = scripted();
        r[3].player.as_mut().unwrap().pawn_id = 1;
        r[2].world.as_mut().unwrap().map = Some("AG-Cave".into());
        let segs = convert(&raw(r), &ConvertOptions::default()).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].trace.meta.level.as_deref(), Some("AG-Workshop"));

        let mut r = scripted();
        r[1].player = None;
        let opts = ConvertOptions {
            level: Some("Override".into()),
            ..ConvertOptions::default()
        };
        let segs = convert(&raw(r), &opts).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].trace.meta.level.as_deref(), Some("Override"));
        assert!(convert(&raw(vec![]), &opts).unwrap().is_empty());
    }

    /// The level is loaded again between two consecutive frames and
    /// everything lands at the old addresses: same map, same pawn number,
    /// consecutive frame numbers. Only the world's clock shows it.
    #[test]
    fn a_world_clock_that_goes_back_ends_the_run() {
        let lens = |r: Vec<RawRecord>| -> Vec<(u64, usize)> {
            convert(&raw(r), &ConvertOptions::default())
                .unwrap()
                .iter()
                .map(|s| (s.first_frame, s.trace.samples.len()))
                .collect()
        };
        let mut r = scripted();
        for (i, rec) in r.iter_mut().enumerate() {
            let w = rec.world.as_mut().unwrap();
            w.time_seconds = if i < 3 { 50.0 } else { 0.25 } + i as f32 / 60.0;
        }
        assert_eq!(lens(r.clone()), [(100, 3), (103, 2)]);
        // Frame lengths are not taken across the reload either.
        let t = raw_timing(&raw(r.clone()));
        assert_eq!((t.lengths.steps, t.arg_checked), (3, 3));
        // A clock that stands still (a pause that is not the Pauser's, zero
        // dilation) or runs on continues the run.
        for rec in &mut r {
            rec.world.as_mut().unwrap().time_seconds = 50.0;
        }
        assert_eq!(lens(r.clone()), [(100, 5)]);
        // By the smallest step back it ends.
        r[4].world.as_mut().unwrap().time_seconds = f32::from_bits(50.0_f32.to_bits() - 1);
        assert_eq!(lens(r), [(100, 4)]);
    }

    #[test]
    fn fov_axes_and_fallbacks() {
        let mut r = scripted();
        r[1].player.as_mut().unwrap().fov_camera = None;
        r[2].player.as_mut().unwrap().axes = Some(RawAxes {
            base_y: 0.0,
            strafe: 0.0,
            forward: 0.0,
            turn: 0.0,
            look_up: 0.0,
            mouse_x: 0.0,
            mouse_y: 0.0,
        });
        let t = &convert(&raw(r.clone()), &ConvertOptions::default()).unwrap()[0].trace;
        assert!(
            t.meta
                .notes
                .iter()
                .any(|n| n.starts_with("fov: controller FOVAngle used on 1"))
        );
        assert!(
            t.meta
                .notes
                .iter()
                .any(|n| n.starts_with("check: aBaseY/aStrafe sign disagrees"))
        );

        r[1].player.as_mut().unwrap().fov_controller = 0.0;
        assert!(convert(&raw(r.clone()), &ConvertOptions::default()).is_err());

        // Without bindings: axes from aBaseY of the next record, jump from the flag.
        let mut r = scripted();
        r[2].player.as_mut().unwrap().axes = Some(RawAxes {
            base_y: 1200.0,
            strafe: -3.0,
            forward: 0.0,
            turn: 0.0,
            look_up: 0.0,
            mouse_x: 0.0,
            mouse_y: 0.0,
        });
        r[1].player.as_mut().unwrap().pressed_jump = true;
        r[2].player.as_mut().unwrap().pressed_jump = true;
        let mut f = raw(r);
        f.header.bindings.clear();
        let t = &convert(&f, &ConvertOptions::default()).unwrap()[0].trace;
        assert_eq!(
            (
                t.samples[2].input.move_forward,
                t.samples[2].input.move_right
            ),
            (1.0, -1.0)
        );
        assert!(t.samples[2].input.jump_pressed, "flag rose at frame 101");
        assert!(
            !t.samples[3].input.jump_pressed,
            "flag still set: no new press"
        );
        assert!(!t.samples[2].input.jump_held && !t.samples[4].input.grapple_held);
        assert!(
            t.meta
                .notes
                .iter()
                .any(|n| n.starts_with("input: no key bindings"))
        );
    }

    #[test]
    fn grapple_state_without_gun_and_edges() {
        let mut r = scripted();
        // Record 4 is flying and grappling; without gun data it is idle.
        r[4].player.as_mut().unwrap().gun = None;
        // Record 3 claims a grapple although the pawn is falling (and its
        // gun's distance does not fit its hit location).
        r[3].player
            .as_mut()
            .unwrap()
            .gun
            .as_mut()
            .unwrap()
            .grappling = true;
        // bPressedJump already set at the first record: a press in frame 100.
        r[0].player.as_mut().unwrap().pressed_jump = true;
        let segs = convert(&raw(r.clone()), &ConvertOptions::default()).unwrap();
        let t = &segs[0].trace;
        assert_eq!(t.samples[4].grapple_state, TraceGrappleState::Idle);
        assert_eq!(t.samples[4].grapple_anchor, None);
        assert_eq!(t.samples[3].grapple_state, TraceGrappleState::Attached);
        let notes = &t.meta.notes;
        assert!(
            notes.iter().any(|n| n
                == "check: PHYS_Flying without grapple gun data on 1 samples (written as idle: no anchor)"),
            "{notes:#?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "check: grapple flag and PHYS_Flying disagree on 1 samples"),
            "{notes:#?}"
        );
        assert!(
            notes.iter().any(|n| n
                == "check: the gun's vDistance is more than 1 uu off the pawn's distance to the \
                    anchor on 1 of 1 attached samples"),
            "{notes:#?}"
        );
        assert!(!notes.iter().any(|n| n.starts_with("anchor:")));
        assert!(
            t.samples[1].input.jump_pressed,
            "a set flag in the first record is a press (no earlier record)"
        );
        assert!(!t.samples[1].input.jump_held, "no jump key held");

        // A controller class change splits on both sides; record 2 alone is
        // too short to be a trace.
        r[2].player.as_mut().unwrap().controller_class = Some("Other".into());
        let segs = convert(&raw(r.clone()), &ConvertOptions::default()).unwrap();
        assert_eq!(
            segs.iter()
                .map(|s| (s.first_frame, s.last_frame))
                .collect::<Vec<_>>(),
            [(100, 101), (103, 104)]
        );

        // Time is the f64 sum of the f32 frame lengths; mixed lengths give
        // no tick rate.
        let mut r = scripted();
        r[2].world.as_mut().unwrap().delta_seconds = 0.02;
        let t = &convert(&raw(r), &ConvertOptions::default()).unwrap()[0].trace;
        let d = f64::from(1.0_f32 / 60.0);
        assert_eq!(t.samples[2].time, d + f64::from(0.02_f32));
        assert_eq!(t.samples[4].time, d + f64::from(0.02_f32) + d + d);
        assert_eq!(t.meta.tick_rate, None);
    }

    #[test]
    fn hostile_values_are_errors_not_panics() {
        for (field, v) in [
            ("delta", f32::NAN),
            ("delta", f32::INFINITY),
            ("x", f32::INFINITY),
        ] {
            let mut r = scripted();
            match field {
                "delta" => r[2].world.as_mut().unwrap().delta_seconds = v,
                _ => r[2].player.as_mut().unwrap().location.x = v,
            }
            assert!(
                convert(&raw(r), &ConvertOptions::default()).is_err(),
                "{field} {v}"
            );
        }
        // Extreme frame numbers and rotations convert without overflow.
        let mut r = scripted();
        for (i, rec) in r.iter_mut().enumerate() {
            rec.frame = u64::MAX - 4 + i as u64;
            rec.player.as_mut().unwrap().view_rotation = [i32::MIN, i32::MAX, 0];
        }
        let segs = convert(&raw(r.clone()), &ConvertOptions::default()).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].last_frame, u64::MAX);
        // u64::MAX followed by 0 is a gap, not a wrap.
        r[3].frame = 0;
        r[4].frame = 1;
        let segs = convert(&raw(r), &ConvertOptions::default()).unwrap();
        assert_eq!(segs.len(), 2);
    }

    /// A recording made with a gamepad: pad buttons under their own names,
    /// the stick only in the pawn's acceleration.
    fn gamepad() -> RawFile {
        let mut h = header();
        for (name, command) in [
            (
                "GBA_MoveForward_Gamepad",
                "Axis aBaseY Speed=1.0 DeadZone=0.4",
            ),
            ("XboxTypeS_LeftY", "GBA_MoveForward_Gamepad"),
            ("XboxTypeS_A", "GBA_ReleaseableJump | RocketBoostKeyDown"),
            ("XboxTypeS_RightTrigger", "GBA_Fire"),
            ("XboxTypeS_LeftShoulder", "GBA_Sprint"),
        ] {
            h.bindings.push(crate::raw::RawBinding {
                name: name.into(),
                command: command.into(),
            });
        }
        // Yaw 16386 reads as 16384 in the original's angle table: forward is
        // +Y, right is −X.
        let mut r = vec![
            record(200, player(0.0, 0, &[])),
            record(201, player(0.0, 16386, &["XboxTypeS_LeftShoulder"])),
            record(202, player(1.0, 16386, &["XboxTypeS_RightTrigger"])),
            record(203, player(2.0, 16386, &["XboxTypeS_RightTrigger"])),
            record(204, player(3.0, 16386, &[])),
            record(205, player(4.0, 16386, &[])),
            record(206, player(5.0, 16386, &["XboxTypeS_A"])),
            record(207, player(6.0, 0, &[])),
            record(208, player(7.0, 0, &["XboxTypeS_RightTrigger"])),
            record(209, player(8.0, 0, &[])),
        ];
        let accel = |i: usize, x: f32, y: f32, r: &mut Vec<RawRecord>| {
            r[i].player.as_mut().unwrap().acceleration = Vec3::new(x, y, 0.0);
        };
        // Sample 1: stick forward at yaw 0. Sample 2: stick (0.6, 0.8) in the
        // axes of record 1.
        accel(1, 2048.0, 0.0, &mut r);
        accel(2, 2048.0 * -0.8, 2048.0 * 0.6, &mut r);
        // Sample 3: attached. Samples 4, 5: the release gap. Sample 6: no
        // deflection.
        let p3 = r[3].player.as_mut().unwrap();
        p3.physics = PHYS_FLYING;
        p3.gun.as_mut().unwrap().grappling = true;
        p3.gun.as_mut().unwrap().anchor = Some(Vec3::new(900.0, 0.0, 50.0));
        for i in [4, 5, 6] {
            r[i].player.as_mut().unwrap().physics = 2;
        }
        // Sample 7: the unit-length acceleration a landing leaves, in the
        // axes of record 6 (forward = +Y).
        accel(7, 0.0, -1.0, &mut r);
        // Sample 8: stick right at yaw 0. Sample 9: attached, with an
        // acceleration that must not be read.
        accel(8, 0.0, 2048.0, &mut r);
        accel(9, 5.0, 5.0, &mut r);
        let p9 = r[9].player.as_mut().unwrap();
        p9.physics = PHYS_FLYING;
        p9.gun.as_mut().unwrap().grappling = true;
        RawFile {
            header: h,
            records: r,
        }
    }

    fn moves(t: &Trace) -> Vec<(f32, f32)> {
        t.samples
            .iter()
            .map(|s| (s.input.move_forward, s.input.move_right))
            .collect()
    }

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn gamepad_runs_take_the_move_axes_from_the_acceleration() {
        let t = &convert(&gamepad(), &ConvertOptions::default()).unwrap()[0].trace;
        let m = moves(t);
        assert_eq!(m[0], (0.0, 0.0));
        assert_eq!(m[1], (1.0, 0.0));
        assert!(close(m[2], (0.6, 0.8)), "{:?}", m[2]);
        for (k, got) in m.iter().enumerate().take(7).skip(3) {
            assert_eq!(*got, (0.0, 0.0), "sample {k}");
        }
        assert!(close(m[7], (-1.0, 0.0)), "{:?}", m[7]);
        assert!(close(m[8], (0.0, 1.0)), "{:?}", m[8]);
        assert_eq!(m[9], (0.0, 0.0));
        // The buttons still come from the keys, one frame ahead of the state.
        assert!(t.samples[2].input.sprint_held && !t.samples[3].input.sprint_held);
        assert!(t.samples[3].input.grapple_held && t.samples[4].input.grapple_held);
        assert!(t.samples[7].input.jump_pressed && t.samples[7].input.jump_held);
        let notes = &t.meta.notes;
        let has = |text: &str| notes.iter().any(|n| n == text);
        assert!(
            has(
                "move input: derived from the pawn's Acceleration (auto: no move key in this \
                 run): its horizontal direction in the axes of the previous record's pawn \
                 rotation, each angle truncated to 4 rotator units; magnitude 1 (the stick's is \
                 not recorded); 4 of 9 samples"
            ),
            "{notes:#?}"
        );
        assert!(
            has(
                "move input: 0 on 4 samples with zero acceleration: 1 while the grapple is \
                 attached (no steering is read then), 2 within two frames after an attached \
                 record (the controller's release gap), 1 others (no deflection beyond the dead \
                 zone, or a move suppressed in a way the record does not show)"
            ),
            "{notes:#?}"
        );
        assert!(
            has("check: acceleration while the grapple is attached on 1 samples (not used)"),
            "{notes:#?}"
        );
        assert!(
            has(
                "check: acceleration below half the run's largest (2048 uu/s^2) on 1 derived \
                 samples (direction used; first at tick 7)"
            ),
            "{notes:#?}"
        );
        assert!(!notes.iter().any(|n| n.contains("aBaseY/aStrafe sign")));
        // The state timeline is there, and the older init: note after it.
        let timeline = crate::state::StateTimeline::from_notes(notes)
            .unwrap()
            .unwrap();
        assert_eq!(timeline.at(3).unwrap().unwrap().physics, Some(PHYS_FLYING));
        assert_eq!(timeline.at(6).unwrap().unwrap().grappling, Some(false));
        assert!(notes.last().unwrap().starts_with(INIT_NOTE_PREFIX));
        t.validate().unwrap();

        // The exact-yaw axes read the same acceleration two units further
        // round (16386 instead of 16384), and say so.
        let yaw = ConvertOptions {
            move_frame: MoveFrame::Yaw,
            ..ConvertOptions::default()
        };
        let y = &convert(&gamepad(), &yaw).unwrap()[0].trace;
        let two_units = f64::from(units_to_radians(2));
        let (f, r) = moves(y)[2];
        let turned = f64::from(r).atan2(f64::from(f)) - 0.8_f64.atan2(0.6);
        assert!((turned + two_units).abs() < 1e-6, "{turned}");
        assert_eq!(moves(y)[1], (1.0, 0.0));
        assert!(
            y.meta
                .notes
                .iter()
                .any(|n| n.contains("exact pawn yaw") && n.contains("--move-frame yaw")),
        );

        // Asked for keys: nothing moves, and the acceleration is reported.
        let keys = ConvertOptions {
            move_input: MoveInput::Keys,
            ..ConvertOptions::default()
        };
        let k = &convert(&gamepad(), &keys).unwrap()[0].trace;
        assert!(moves(k).iter().all(|m| *m == (0.0, 0.0)));
        let notes = &k.meta.notes;
        assert!(
            notes
                .iter()
                .any(|n| n == "move input: the mapped move keys (--move-input keys)"),
            "{notes:#?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n == "check: acceleration without a mapped move key on 4 of 9 samples"),
            "{notes:#?}"
        );
    }

    #[test]
    fn keyboard_runs_keep_their_keys() {
        // The scripted run holds W; give every record an acceleration that
        // points somewhere else.
        let mut r = scripted();
        for rec in &mut r {
            rec.player.as_mut().unwrap().acceleration = Vec3::new(0.0, -2048.0, 0.0);
        }
        let file = raw(r);
        let t = &convert(&file, &ConvertOptions::default()).unwrap()[0].trace;
        assert_eq!(
            moves(t),
            [(0.0, 0.0), (0.0, 0.0), (1.0, 0.0), (1.0, 0.0), (0.0, 0.0)]
        );
        let notes = &t.meta.notes;
        assert!(
            notes
                .iter()
                .any(|n| n
                    == "move input: the mapped move keys (auto: a move key is held on 2 records)"),
            "{notes:#?}"
        );
        // Samples 1 (no key yet) and 4 would move by the acceleration; sample
        // 4's record is attached and not counted.
        assert!(
            notes
                .iter()
                .any(|n| n == "check: acceleration without a mapped move key on 1 of 4 samples"),
            "{notes:#?}"
        );
        // Opposite keys cancel but are still keys.
        let mut r = scripted();
        for rec in &mut r {
            let p = rec.player.as_mut().unwrap();
            p.keys = vec!["W".into(), "S".into()];
            p.acceleration = Vec3::new(2048.0, 0.0, 0.0);
        }
        let t = &convert(&raw(r), &ConvertOptions::default()).unwrap()[0].trace;
        assert!(moves(t).iter().all(|m| *m == (0.0, 0.0)));
        // Asked for the acceleration: the keys are ignored for the move axes
        // (yaw 0, then 182 units: +X is forward, then almost).
        let asked = ConvertOptions {
            move_input: MoveInput::Acceleration,
            ..ConvertOptions::default()
        };
        let a = &convert(&file, &asked).unwrap()[0].trace;
        let m = moves(a);
        assert!(close(m[1], (0.0, -1.0)), "{:?}", m[1]);
        assert!(m[3].0 < 0.0 && m[3].1 < -0.99, "{:?}", m[3]);
        assert_eq!(m[4], (0.0, 0.0), "attached");
        assert!(a.meta.notes.iter().any(|n| n.starts_with(
            "move input: derived from the pawn's Acceleration \
                                        (--move-input acceleration)"
        )),);
        // Nothing held and nothing accelerating: the keys, and the note says
        // why. Without bindings the older fallback stays as it was.
        let mut still = scripted();
        for rec in &mut still {
            rec.player.as_mut().unwrap().keys.clear();
        }
        let t = &convert(&raw(still.clone()), &ConvertOptions::default()).unwrap()[0].trace;
        assert!(t.meta.notes.iter().any(|n| n
            == "move input: the mapped move keys (auto: no move key and no acceleration in \
                this run)"));
        let mut f = raw(still);
        f.header.bindings.clear();
        let t = &convert(&f, &ConvertOptions::default()).unwrap()[0].trace;
        assert!(!t.meta.notes.iter().any(|n| n.starts_with("move input:")));
    }

    #[test]
    fn hostile_accelerations_and_rotations() {
        let mut f = gamepad();
        let p = f.records[1].player.as_mut().unwrap();
        p.acceleration = Vec3::new(f32::MAX, -f32::MAX, f32::MAX);
        f.records[0].player.as_mut().unwrap().pawn_rotation = [i32::MIN, i32::MAX, i32::MIN];
        // Straight up: the forward axis has no horizontal part.
        f.records[7].player.as_mut().unwrap().pawn_rotation = [16384, 0, 0];
        let t = &convert(&f, &ConvertOptions::default()).unwrap()[0].trace;
        for s in &t.samples {
            let len = f64::from(s.input.move_forward).hypot(f64::from(s.input.move_right));
            assert!(
                len == 0.0 || (len - 1.0).abs() < 1e-6,
                "tick {}: {len}",
                s.tick
            );
        }
        assert!(
            t.meta.notes.iter().any(|n| n
                == "check: pawn rotation too steep to invert on 1 samples (yaw-only axes \
                        used)"),
            "{:#?}",
            t.meta.notes
        );
        // Record 0's pawn yaw is not its view yaw; make record 1's differ too.
        let differs = |t: &Trace, n: usize| {
            t.meta.notes.iter().any(|x| {
                *x == format!("check: pawn yaw differs from the view yaw on {n} derived samples")
            })
        };
        assert!(differs(t, 1), "{:#?}", t.meta.notes);
        f.records[1].player.as_mut().unwrap().view_rotation[1] += 7;
        let t = &convert(&f, &ConvertOptions::default()).unwrap()[0].trace;
        assert!(differs(t, 2), "{:#?}", t.meta.notes);
    }

    #[test]
    fn tick_rate_rules() {
        let d60 = 1.0_f32 / 60.0;
        assert_eq!(tick_rate_of(&[d60, d60]), Some(60.0));
        assert_eq!(
            tick_rate_of(&[0.03]),
            Some((1.0 / f64::from(0.03_f32)) as f32)
        );
        assert_eq!(tick_rate_of(&[d60, 0.02]), None);
        assert_eq!(tick_rate_of(&[]), None);
        assert_eq!(tick_rate_of(&[0.0]), None);
    }

    #[test]
    fn names() {
        assert_eq!(output_names("a.raw.jsonl", 1), ["a.trace.jsonl"]);
        assert_eq!(
            output_names("dir.x/a.jsonl", 2),
            ["dir.x/a.seg0.trace.jsonl", "dir.x/a.seg1.trace.jsonl"]
        );
        assert_eq!(output_names("plain", 1), ["plain.trace.jsonl"]);
    }

    #[test]
    fn rotator_wrapping() {
        assert_eq!(units_to_radians(65536 + 16384), units_to_radians(16384));
        assert!(units_to_radians(32768) < 0.0, "half a turn maps to −π");
        assert_eq!(delta_radians(i32::MIN, i32::MAX), units_to_radians(1));
    }
}
