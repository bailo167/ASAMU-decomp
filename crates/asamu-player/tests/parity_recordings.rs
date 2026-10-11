//! The angle table and the integer view rotation against raw recordings of
//! the original (docs/PARITY_FINDINGS.md P7 and P9; ABILITIES.md A-WK-1,
//! A-CM-8 and A-CM-9).
//!
//! Gated: the tests read the `*.raw.jsonl` recordings (docs/TRACE_CAPTURE.md)
//! in the directory `ASAMU_TRACE_RAW_DIR` names and are skipped without it.
//! Recordings never enter the repository; nothing recorded is in this file.
//!
//! ```sh
//! ASAMU_TRACE_RAW_DIR=<recordings> cargo test -p asamu-player --test parity_recordings -- --nocapture
//! ```
//!
//! The counts in the comments are those of the three recordings of
//! 2026-10-10 (45,237 records); the tests print their own.
//!
//! Where the expectations come from: the recorded values themselves. The
//! only numbers of ours are the bounds on float rounding, derived at
//! [`AXIS_TOLERANCE`], [`LEAN_AXIS_TOLERANCE`] and [`ON_AXIS`].

use std::path::PathBuf;

use asamu_core::rotator::{normalize_rotator_axis, rotator_units_to_radians};
use asamu_player::sim::{self, rotator_axes_flat};
use asamu_player::{InputFrame, PlayerParams, PlayerState};
use glam::Vec3;
use serde_json::Value;

/// `Physics` of a walking pawn and of a falling one (engine enumeration).
const PHYS_WALKING: i64 = 1;
const PHYS_FALLING: i64 = 2;

/// What the tests need of one record.
struct Sample {
    frame: i64,
    map: String,
    physics: i64,
    attached: bool,
    /// Horizontal acceleration, UU/s² (an `f32` in the original).
    acceleration: [f64; 2],
    /// Pitch, yaw, roll in rotator units.
    pawn_rotation: [i32; 3],
    view_rotation: [i32; 3],
}

fn rotation(value: &Value) -> Option<[i32; 3]> {
    let units = |i: usize| i32::try_from(value.get(i)?.as_i64()?).ok();
    Some([units(0)?, units(1)?, units(2)?])
}

/// A record line; `None` for the header and for a record without a player.
fn sample(line: &str) -> Option<Sample> {
    let record: Value = serde_json::from_str(line).ok()?;
    let player = record.get("player")?;
    let acceleration = player.get("acceleration")?;
    Some(Sample {
        frame: record.get("frame")?.as_i64()?,
        map: record.get("world")?.get("map")?.as_str()?.to_owned(),
        physics: player.get("physics")?.as_i64()?,
        attached: player.get("gun")?.get("grappling")?.as_bool()?,
        acceleration: [
            acceleration.get(0)?.as_f64()?,
            acceleration.get(1)?.as_f64()?,
        ],
        pawn_rotation: rotation(player.get("pawn_rotation")?)?,
        view_rotation: rotation(player.get("view_rotation")?)?,
    })
}

/// Every run of consecutive frames in one level, over all recordings: a run
/// ends where a frame is missing, the level changes or a record has no
/// player. `None` (the test is skipped) without recordings.
fn recorded_runs() -> Option<Vec<Vec<Sample>>> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_TRACE_RAW_DIR")?);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".raw.jsonl"))
        })
        .collect();
    paths.sort();
    let mut runs: Vec<Vec<Sample>> = Vec::new();
    for path in &paths {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let mut run: Vec<Sample> = Vec::new();
        for line in text.lines() {
            let next = sample(line);
            let continues = match (run.last(), &next) {
                (Some(before), Some(now)) => now.frame == before.frame + 1 && now.map == before.map,
                _ => false,
            };
            if !continues && !run.is_empty() {
                runs.push(std::mem::take(&mut run));
            }
            run.extend(next);
        }
        if !run.is_empty() {
            runs.push(run);
        }
    }
    (!runs.is_empty()).then_some(runs)
}

fn skipped() {
    eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
}

/// The radian value that stands for a rotator angle in the simulation's
/// state, as the trace converter writes it.
fn image(units: i32) -> f32 {
    rotator_units_to_radians(normalize_rotator_axis(units))
}

/// P9, A-CM-9: the look update of the script layer, fed the recorded change
/// of the view rotation as the trace converter encodes it (the radian image
/// of the difference on the 16-bit circle), reproduces the recorded yaw and
/// pitch on every tick, with nothing left over: the state is carried from
/// the first record of each run to its last and compared bit for bit.
/// The three recordings: 45,223 of 45,223 ticks in 14 runs.
#[test]
fn the_look_update_reproduces_the_recorded_view_on_every_tick() {
    let Some(runs) = recorded_runs() else {
        return skipped();
    };
    let params = PlayerParams::asamu_original();
    let (mut ticks, mut turned, mut at_upper_limit) = (0_usize, 0_usize, 0_usize);
    let (mut lowest_pitch, mut longest) = (0_i32, 0_usize);
    for run in &runs {
        let first = &run[0];
        let mut state = PlayerState::new(Vec3::ZERO, 0.0);
        state.yaw = image(first.view_rotation[1]);
        state.pitch = image(first.view_rotation[0]);
        longest = longest.max(run.len() - 1);
        for (n, pair) in run.windows(2).enumerate() {
            let (before, now) = (&pair[0], &pair[1]);
            let d_yaw = now.view_rotation[1].wrapping_sub(before.view_rotation[1]);
            let d_pitch = now.view_rotation[0].wrapping_sub(before.view_rotation[0]);
            let input = InputFrame {
                look_yaw_delta: image(d_yaw),
                look_pitch_delta: image(d_pitch),
                ..InputFrame::default()
            };
            sim::apply_look(&mut state, &input, &params);
            ticks += 1;
            turned += usize::from(d_yaw != 0 || d_pitch != 0);

            let yaw = normalize_rotator_axis(now.view_rotation[1]);
            let pitch = normalize_rotator_axis(now.view_rotation[0]);
            at_upper_limit += usize::from(pitch == 18_000);
            lowest_pitch = lowest_pitch.min(pitch);
            let at = format!("frame {} ({}), tick {n} of its run", now.frame, now.map);
            assert_eq!(state.yaw_units(), yaw, "yaw at {at}");
            assert_eq!(state.pitch_units(), pitch, "pitch at {at}");
            assert_eq!(state.yaw.to_bits(), image(yaw).to_bits(), "yaw at {at}");
            assert_eq!(
                state.pitch.to_bits(),
                image(pitch).to_bits(),
                "pitch at {at}"
            );
        }
    }
    println!(
        "view rotation: {ticks} of {ticks} ticks reproduced in {} runs (the longest {longest} \
         ticks); the view turns on {turned}; pitch at +18000 on {at_upper_limit}, lowest pitch \
         {lowest_pitch}",
        runs.len()
    );
    assert!(ticks > 0);
}

/// Largest angle, radians, between a recorded acceleration and the axis the
/// stick was held on.
///
/// The original's acceleration is `AccelRate` times the normalised
/// `aForward · X + aStrafe · Y` (A-WK-1), and walking normalises it once
/// more (NATIVE_PHYSICS.md finding 2). With the stick on an axis that is a
/// table axis scaled three times in single precision (by the stick's size,
/// by the normalisation, by the second normalisation; the factor 2048 is a
/// power of two and exact). Scaling a vector changes its direction only
/// through the rounding of its two components, each by at most half an ulp:
/// 2⁻²⁴ of the component. Seen from a vector of length `L` at angle `t`,
/// the two errors turn it by at most `2⁻²⁴ · L · (|cos t · sin t| + |sin t ·
/// cos t|) / L` ≤ 2⁻²⁴ rad. Three scalings: 3 · 2⁻²⁴ = 1.79e-7 rad.
///
/// Measured on the three recordings: the largest angle to the table axis is
/// 8.2e-8 rad (2,349 samples). For comparison, the axis of a yaw truncated
/// towards zero instead of floored is 4 units = 3.8e-4 rad away, the exact
/// angle at least 1 unit = 9.6e-5 rad, and the exact sine of the floored
/// angle up to 2.4e-7 rad (the table's own rounding, A-CM-8).
const AXIS_TOLERANCE: f64 = 3.0 / 16_777_216.0;

/// A stick counts as held on an axis when the acceleration is within this
/// angle (0.03°, the trace converter's bound, M5 in `asamu-trace`'s
/// `move_input`) of an axis of the previous yaw floored to 4 units, taken
/// with exact trigonometry. The bound is wider than everything the test
/// tells apart (4 units are 0.022°), so the choice of samples does not
/// depend on the rule under test. A stick held a little off an axis would be
/// counted and would fail the test; the three recordings have none (every
/// sample within this bound is within 8.2e-8 rad).
const ON_AXIS: f64 = 0.03 * core::f64::consts::PI / 180.0;

/// Angle between the horizontal vector `a` and the nearest of ±X, ±Y for
/// the axes `X = (cos, sin)`, `Y = (−sin, cos)`.
fn angle_to_axes(a: [f64; 2], sin: f64, cos: f64) -> f64 {
    let length = a[0].hypot(a[1]) * sin.hypot(cos);
    let from_x = (a[0] * sin - a[1] * cos).abs() / length;
    let from_y = (a[0] * cos + a[1] * sin).abs() / length;
    from_x.min(from_y).min(1.0).asin()
}

/// Sine and cosine of a rotator angle with exact trigonometry.
fn exact(units: i32) -> (f64, f64) {
    (f64::from(units & 0xFFFF) * core::f64::consts::TAU / 65_536.0).sin_cos()
}

/// Sine and cosine of a yaw as the code under test builds the move axes:
/// `X = (cos, sin, 0)`, and `Y` must be `(−sin, cos, 0)`.
fn table(yaw: i32) -> (f64, f64) {
    let (x, y) = rotator_axes_flat(yaw);
    assert_eq!((y.x, y.y, x.z, y.z), (-x.y, x.x, 0.0, 0.0), "yaw {yaw}");
    (f64::from(x.y), f64::from(x.x))
}

/// P7, A-CM-8 and A-WK-1: with the stick held on an axis, the recorded
/// acceleration of a walking or falling frame lies on the move axes our
/// controller builds from the **previous** record's pawn yaw: the yaw
/// floored to 4 rotator units (towards −∞ for a negative yaw), sine and
/// cosine from the engine's table.
///
/// Samples: consecutive records, the second walking or falling and not
/// attached, horizontal acceleration 2048, the previous pawn rotation read
/// by the table as yaw only (pitch and roll below 4 units: the simulation
/// has no pawn pitch or roll), stick on an axis ([`ON_AXIS`]).
///
/// The three recordings: 2,349 samples (2,217 walking, 132 falling), all
/// within [`AXIS_TOLERANCE`] of the table axes (largest angle 8.2e-8 rad).
/// Of the 1,112 whose previous yaw is negative and no multiple of 4, none
/// fits the yaw truncated towards zero; of the 1,933 whose previous yaw is
/// no multiple of 4, none fits the exact angle. The exact sine of the
/// floored angle is beyond the tolerance on 73 and beyond 1e-7 rad on 330;
/// the same record's yaw fits 1,845 (those whose view did not turn).
#[test]
fn recorded_accelerations_lie_on_the_table_axes_of_the_floored_previous_yaw() {
    let Some(runs) = recorded_runs() else {
        return skipped();
    };
    let (mut samples, mut walking, mut misses, mut largest) = (0_usize, 0_usize, 0_usize, 0.0_f64);
    let (mut negative_odd, mut odd, mut same_record) = (0_usize, 0_usize, 0_usize);
    let (mut exact_sine_beyond, mut exact_sine_beyond_1e7) = (0_usize, 0_usize);
    for pair in runs.iter().flat_map(|run| run.windows(2)) {
        let (before, now) = (&pair[0], &pair[1]);
        let [pitch, yaw, roll] = before.pawn_rotation;
        let a = now.acceleration;
        if !(now.physics == PHYS_WALKING || now.physics == PHYS_FALLING)
            || now.attached
            || (a[0].hypot(a[1]) - 2048.0).abs() >= 0.01
            || (pitch & 0xFFFC) != 0
            || (roll & 0xFFFC) != 0
        {
            continue;
        }
        // The yaw floored to 4 units on the 16-bit circle, written out here.
        let floored = yaw & 0xFFFC;
        let (sin, cos) = exact(floored);
        if angle_to_axes(a, sin, cos) >= ON_AXIS {
            continue;
        }
        samples += 1;
        walking += usize::from(now.physics == PHYS_WALKING);

        // The rule under test.
        let (sin, cos) = table(yaw);
        let angle = angle_to_axes(a, sin, cos);
        largest = largest.max(angle);
        misses += usize::from(angle > AXIS_TOLERANCE);

        // What the rule is not. A yaw that is no multiple of 4 tells the
        // floor from the exact angle, a negative one also from a truncation
        // towards zero (Rust's integer division truncates towards zero).
        if yaw % 4 != 0 {
            odd += 1;
            let (sin, cos) = exact(yaw);
            assert!(
                angle_to_axes(a, sin, cos) > AXIS_TOLERANCE,
                "frame {}: the exact yaw {yaw} fits",
                now.frame
            );
            if yaw < 0 {
                negative_odd += 1;
                let (sin, cos) = table((yaw / 4) * 4);
                assert!(
                    angle_to_axes(a, sin, cos) > AXIS_TOLERANCE,
                    "frame {}: the yaw {yaw} truncated towards zero fits",
                    now.frame
                );
            }
        }
        // The same record's yaw (after that frame's look update) instead of
        // the previous record's.
        let (sin, cos) = table(now.pawn_rotation[1]);
        same_record += usize::from(angle_to_axes(a, sin, cos) <= AXIS_TOLERANCE);
        // The table's entries against the exact sine of the same angle.
        let (sin, cos) = exact(floored);
        let angle = angle_to_axes(a, sin, cos);
        exact_sine_beyond += usize::from(angle > AXIS_TOLERANCE);
        exact_sine_beyond_1e7 += usize::from(angle > 1.0e-7);
    }
    println!(
        "move axes: {samples} on-axis samples ({walking} walking, {} falling); beyond {:.3e} rad of \
         the table axes of the floored previous yaw: {misses} (largest angle {largest:.3e} rad)",
        samples - walking,
        AXIS_TOLERANCE
    );
    println!(
        "  previous yaw no multiple of 4: {odd} (the exact angle fits none); of them negative: \
         {negative_odd} (the yaw truncated towards zero fits none)"
    );
    println!(
        "  the same record's yaw fits {same_record} of {samples}; the exact sine of the floored \
         angle is beyond the tolerance on {exact_sine_beyond} and beyond 1e-7 rad on \
         {exact_sine_beyond_1e7}"
    );
    assert!(samples > 0, "no on-axis sample in the recordings");
    assert_eq!(misses, 0, "of {samples} samples");
}

/// [`AXIS_TOLERANCE`] with one more single-precision scaling: under a lean
/// roll the right axis the original builds is `(−cos r · sin y, cos r ·
/// cos y, −sin r)`, the flat axis scaled by the roll's cosine before the
/// three scalings counted there. 4 · 2⁻²⁴ = 2.4e-7 rad.
const LEAN_AXIS_TOLERANCE: f64 = 4.0 / 16_777_216.0;

/// P7, A-WK-1: the pawn leans while it moves (a roll of up to 2,046 units in
/// these samples), and the simulation has no pawn roll. That costs nothing
/// while the stick is on an axis: with the pawn's pitch read as 0 the
/// original's forward axis is `(cos y, sin y, 0)` whatever the roll, and its
/// right axis, once the script has dropped the Z part and normalised, points
/// along `(−sin y, cos y)`. So every such sample must lie on the yaw-only
/// table axes [`sim::rotator_axes_flat`] gives for the previous record's
/// yaw. That is four times the sample of the test above, which keeps to
/// rotations without a roll.
///
/// Samples: as above, but with any roll (pitch below 4 units as before: a
/// pitch kept from a flight turns the right axis under a roll, which the
/// simulation does not model).
///
/// The three recordings: 9,977 samples (6,797 walking, 3,180 falling; 7,628
/// with a roll of 4 units or more), all within [`LEAN_AXIS_TOLERANCE`]
/// (largest angle 9.6e-8 rad). Of the 1,406 whose previous yaw is negative
/// and no multiple of 4, none fits the yaw truncated towards zero; of the
/// 7,887 whose previous yaw is no multiple of 4, none fits the exact angle.
#[test]
fn a_lean_roll_leaves_on_axis_accelerations_on_the_table_axes_of_the_yaw() {
    let Some(runs) = recorded_runs() else {
        return skipped();
    };
    let (mut samples, mut walking, mut leaning, mut largest_roll) =
        (0_usize, 0_usize, 0_usize, 0_i32);
    let (mut misses, mut largest) = (0_usize, 0.0_f64);
    let (mut odd, mut exact_fits) = (0_usize, 0_usize);
    let (mut negative_odd, mut truncated_fits) = (0_usize, 0_usize);
    for pair in runs.iter().flat_map(|run| run.windows(2)) {
        let (before, now) = (&pair[0], &pair[1]);
        let [pitch, yaw, roll] = before.pawn_rotation;
        let a = now.acceleration;
        if !(now.physics == PHYS_WALKING || now.physics == PHYS_FALLING)
            || now.attached
            || (a[0].hypot(a[1]) - 2048.0).abs() >= 0.01
            || (pitch & 0xFFFC) != 0
        {
            continue;
        }
        let (sin, cos) = exact(yaw & 0xFFFC);
        if angle_to_axes(a, sin, cos) >= ON_AXIS {
            continue;
        }
        samples += 1;
        walking += usize::from(now.physics == PHYS_WALKING);
        leaning += usize::from((roll & 0xFFFC) != 0);
        largest_roll = largest_roll.max(normalize_rotator_axis(roll).abs());

        let (sin, cos) = table(yaw);
        let angle = angle_to_axes(a, sin, cos);
        largest = largest.max(angle);
        misses += usize::from(angle > LEAN_AXIS_TOLERANCE);

        if yaw % 4 != 0 {
            odd += 1;
            let (sin, cos) = exact(yaw);
            exact_fits += usize::from(angle_to_axes(a, sin, cos) <= LEAN_AXIS_TOLERANCE);
            if yaw < 0 {
                negative_odd += 1;
                // Rust's integer division truncates towards zero.
                let (sin, cos) = table((yaw / 4) * 4);
                truncated_fits += usize::from(angle_to_axes(a, sin, cos) <= LEAN_AXIS_TOLERANCE);
            }
        }
    }
    println!(
        "move axes under a lean roll: {samples} on-axis samples ({walking} walking, {} falling; \
         {leaning} with a roll, the largest {largest_roll} units); beyond {:.3e} rad of the table \
         axes of the floored previous yaw: {misses} (largest angle {largest:.3e} rad)",
        samples - walking,
        LEAN_AXIS_TOLERANCE
    );
    println!(
        "  previous yaw no multiple of 4: {odd} (the exact angle fits {exact_fits}); of them \
         negative: {negative_odd} (the yaw truncated towards zero fits {truncated_fits})"
    );
    assert!(samples > 0, "no on-axis sample in the recordings");
    assert_eq!(misses, 0, "of {samples} samples");
    assert_eq!(exact_fits, 0, "of {odd} samples");
    assert_eq!(truncated_fits, 0, "of {negative_odd} samples");
}
