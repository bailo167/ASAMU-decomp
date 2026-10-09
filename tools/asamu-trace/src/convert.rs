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
//! anchor = the anchor helper's location (else `vGrappleLocation`). Without
//! gun data (the pawn's weapon is not a `GrappleGun`) no anchor is known, so
//! the sample is idle even in `PHYS_Flying (4)`; such samples are counted in
//! a `check:` note. `rope_length` is always `null` (the original grapple has
//! no rope). FOV = the camera's POV FOV, else the controller's `FOVAngle`.
//!
//! # Segments
//!
//! A run ends at a frame gap (missed or paused frames), at a frame without a
//! player, and when the map, the pawn object or the controller/pawn class
//! changes. Runs shorter than two records are dropped. Each run is one trace;
//! replays stop at the end of a trace, never inside a gap.
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
use crate::raw::{RawFile, RawPlayer, RawRecord};

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

/// Options of [`convert`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// Overrides the level name taken from the recording.
    pub level: Option<String>,
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
}

#[derive(Default)]
struct Counters {
    fov_controller: usize,
    grapple_physics: usize,
    flying_without_gun: usize,
    axis_mismatch: usize,
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
            (g.grappling, Some(g.anchor.unwrap_or(g.grapple_location)))
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
            let (cur, next) = (&run[k - 1], &run[k]);
            let prev = k.checked_sub(2).and_then(|j| run.get(j));
            let dt = delta_seconds(next)?;
            dts.push(dt);
            time += f64::from(dt);
            let (cp, np) = (player(cur)?, player(next)?);
            let input_actions: Actions;
            let (jump_held, key_edge, use_pressed);
            match &keymap {
                Some(m) => {
                    input_actions = m.actions(&cp.keys);
                    let pa = prev.map(player).transpose()?.map(|p| m.actions(&p.keys));
                    if let Some(ax) = &np.axes
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
            let prev_flag = prev
                .map(player)
                .transpose()?
                .is_some_and(|p| p.pressed_jump);
            let flag_edge = cp.pressed_jump && !prev_flag;
            let input = InputFrame {
                move_forward: input_actions.forward as f32,
                move_right: input_actions.right as f32,
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
        if c.grapple_physics > 0 {
            notes.push(format!(
                "check: grapple flag and PHYS_Flying disagree on {} samples",
                c.grapple_physics
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
        let init = InitState::of(player(&run[0])?);
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
        g.anchor = Some(Vec3::new(500.0, 0.0, 0.0));
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
        assert_eq!(x[4].grapple_anchor, Some(Vec3::new(500.0, 0.0, 0.0)));
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
        };
        let segs = convert(&raw(r), &opts).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].trace.meta.level.as_deref(), Some("Override"));
        assert!(convert(&raw(vec![]), &opts).unwrap().is_empty());
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
        // Record 3 claims a grapple although the pawn is falling.
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
