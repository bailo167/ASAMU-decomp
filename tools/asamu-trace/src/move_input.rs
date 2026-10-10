//! Movement input from the recorded pawn acceleration.
//!
//! A gamepad's stick is not a key: `PlayerInput.PressedKeys` never holds it,
//! and the polling recorder reads the `PlayerInput` axes after the engine
//! cleared them (every recording of 2026-10-10 has `aBaseY = aStrafe = 0` on
//! every record). What the recording does hold is the pawn's `Acceleration`,
//! which the controller's walking move writes from the two move axes every
//! frame. This module inverts that mapping.
//!
//! # The original's mapping (evidence)
//!
//! | # | Fact | Confidence |
//! |---|---|---|
//! | M1 | In the controller's walking state (pawn walking or falling) the acceleration is `AccelRate × normal(aForward·X + aStrafe·Y)` with the Z part removed before normalising, where `X`, `Y` are the forward and right axes of the **pawn's** rotation as the frame begins (before that frame's rotation update). | CONFIRMED (ABILITIES.md A-WK-1, from the script); the rotation's time is also measured (M4) |
//! | M2 | The stick's magnitude is lost: walking overwrites the acceleration with `AccelRate` along its direction, falling restores the script's value after its per-tick clamp. Recorded horizontal magnitudes while walking or falling, all three recordings: 2048 on 14,851 records, 0 on 20,783, 1.0 on 2 (the unit-length value a landing leaves), nothing else. | CONFIRMED (NATIVE_PHYSICS.md finding 2, §2.1, §4.2, the landing's step 5; measured) |
//! | M3 | While the grapple is attached the controller hands the pawn a zero acceleration, and so does the one controller tick after a release (two records after a release by the gun's own tick, one after a release by the button). | CONFIRMED (GRAPPLE.md G-PH-1) / STRONG (G-RL-7); measured: all 9,601 flying records have zero acceleration; of 75 releases by the button 69 are followed by exactly one zero record and none by fewer; of 43 releases with the button still held 30 by exactly two, one by one, the rest by more |
//! | M4 | The axes are those of the **previous** record's pawn rotation. | measured: with a stick held on an axis (10,051 samples within 0.03° of forward, back, left or right) the direction in the previous record's axes is on the axis to 0.0002° on every sample; in the same record's axes on 7,907 (those where the view does not turn) |
//! | M5 | The axes are built from angles **truncated to 4 rotator units** (a 14-bit angle table): `angle − (angle mod 4)` in 0..65535. | measured on the same 10,051 samples: on the axis to 0.0002° with truncated angles on all of them; with exact angles on 2,105 (a quarter, the yaws that are multiples of 4), the rest off by 1, 2 or 3 units. That the cause is the engine's trigonometry table is STRONG (stock UE3), the effect CONFIRMED |
//! | M6 | Pitch and roll of the pawn rotation take part (the pawn leans while moving, and keeps the pitch it had while flying for the first walking move after a release): `X = (cp·cy, cp·sy, sp)`, `Y = (sr·sp·cy − cr·sy, sr·sp·sy + cr·cy, −sr·cp)`. | STRONG: 73 first moves after a flight start from a kept pitch of 8° or more; in 22 of them these axes and yaw-only axes give directions more than 0.25° apart. In 15 of the 22 the direction in these axes equals the next frame's to 0.2° (median difference 0.00001°); the yaw-only direction does so in 1 (median 1.4°). The lean alone (no pitch) changes the direction on 3,133 of 14,744 samples, by up to 0.56°; of 188 six-sample windows in which that change varies by 0.05° or more, the direction stays within 0.02° in these axes in 14 and in yaw-only axes in none |
//!
//! Reproduce the measurements (the counts are those of the three recordings
//! of 2026-10-10): `ASAMU_TRACE_RAW_DIR=research/local/traces/win cargo test
//! -p asamu-trace --test real_recordings -- --nocapture` prints them and
//! fails if a recording contradicts M2–M5.
//!
//! # What is derived
//!
//! [`move_direction`] solves `f·X_h + r·Y_h ∝ a_h` for the unit vector
//! `(f, r)`: the stick's **direction** after the dead zone, as the original
//! used it. Its magnitude is 1 (M2: the original discards it too).
//!
//! It is not defined, and the converter writes 0, when the acceleration is
//! zero: while the grapple is attached and in the release gap after it (M3:
//! no steering is read there, by either game), and otherwise when the stick
//! is inside its dead zone **or** when something suppressed the move that
//! the record does not show (the move-input lock, ABILITIES.md A-IL-1, or a
//! controller state without a walking move). The converter counts the three
//! cases apart in the trace's notes.
//!
//! [`MoveFrame::Yaw`] is a diagnostic: the direction in the exact yaw-only
//! axes of the previous record. A simulation that builds its axes from the
//! exact yaw then reproduces the recorded acceleration direction, which
//! hides M5 and M6; the default [`MoveFrame::Original`] shows them.
//!
//! `tools/trace-recorder/asamu_recorder_core.py` (`move_direction`)
//! implements the same arithmetic, operation for operation.

/// The step of the original's angle table, rotator units (M5).
pub const AXES_TABLE_STEP: i32 = 4;
/// Below this determinant of the horizontal axes (`cos(pitch)·cos(roll)`:
/// the pawn's forward axis points almost straight up or down) the stick
/// direction cannot be told from the acceleration; the yaw-only axes are
/// used and the sample is counted. A numerical bound, not a property of the
/// game; the largest pitch recorded outside a flight (54.4°) is far from it.
pub const MIN_AXES_DETERMINANT: f64 = 1.0e-3;

const RADIANS_PER_UNIT: f64 = std::f64::consts::TAU / 65536.0;

/// The axes a recorded acceleration is expressed in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoveFrame {
    /// The original's own axes: the previous record's full pawn rotation,
    /// each angle truncated to [`AXES_TABLE_STEP`] units. Gives the stick's
    /// direction.
    #[default]
    Original,
    /// The previous record's exact pawn yaw, no pitch or roll. Gives the
    /// direction a yaw-only simulation needs to reproduce the acceleration.
    Yaw,
}

/// A derived move direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveDirection {
    /// Forward axis, −1..1.
    pub forward: f32,
    /// Right axis, −1..1.
    pub right: f32,
    /// [`MoveFrame::Original`] was asked for but the axes could not be
    /// inverted ([`MIN_AXES_DETERMINANT`]); the yaw-only axes were used.
    pub steep: bool,
}

/// The angle the original's table gives for `units` (M5), radians.
fn table_angle(units: i32) -> f64 {
    let u = units.rem_euclid(65536);
    f64::from(u - u % AXES_TABLE_STEP) * RADIANS_PER_UNIT
}

fn exact_angle(units: i32) -> f64 {
    f64::from(units.rem_euclid(65536)) * RADIANS_PER_UNIT
}

/// Horizontal length of an acceleration, UU/s² (0 for a zero or non-finite
/// one).
#[must_use]
pub fn horizontal_magnitude(accel_x: f32, accel_y: f32) -> f64 {
    let (ax, ay) = (f64::from(accel_x), f64::from(accel_y));
    if !(ax.is_finite() && ay.is_finite()) {
        return 0.0;
    }
    (ax * ax + ay * ay).sqrt()
}

/// The unit move direction `(forward, right)` that gives the horizontal
/// acceleration `(accel_x, accel_y)` in the axes of `rotation` (pitch, yaw,
/// roll in rotator units: the pawn rotation of the **previous** record).
/// `None` when the acceleration is zero or not finite.
#[must_use]
pub fn move_direction(
    rotation: [i32; 3],
    accel_x: f32,
    accel_y: f32,
    frame: MoveFrame,
) -> Option<MoveDirection> {
    let (ax, ay) = (f64::from(accel_x), f64::from(accel_y));
    if !(ax.is_finite() && ay.is_finite()) || (ax == 0.0 && ay == 0.0) {
        return None;
    }
    let (mut f, mut r, mut steep) = (0.0_f64, 0.0_f64, false);
    let mut solved = false;
    if frame == MoveFrame::Original {
        let p = table_angle(rotation[0]);
        let y = table_angle(rotation[1]);
        let ro = table_angle(rotation[2]);
        let (cp, sp) = (p.cos(), p.sin());
        let (cy, sy) = (y.cos(), y.sin());
        let (cr, sr) = (ro.cos(), ro.sin());
        let (xx, xy) = (cp * cy, cp * sy);
        let (yx, yy) = (sr * sp * cy - cr * sy, sr * sp * sy + cr * cy);
        let det = xx * yy - xy * yx;
        if det.abs() >= MIN_AXES_DETERMINANT {
            f = (ax * yy - ay * yx) / det;
            r = (xx * ay - xy * ax) / det;
            solved = true;
        } else {
            steep = true;
            f = ax * cy + ay * sy;
            r = ay * cy - ax * sy;
            solved = true;
        }
    }
    if !solved {
        let y = exact_angle(rotation[1]);
        let (cy, sy) = (y.cos(), y.sin());
        f = ax * cy + ay * sy;
        r = ay * cy - ax * sy;
    }
    let n = (f * f + r * r).sqrt();
    if !(n > 0.0 && n.is_finite()) {
        return None;
    }
    Some(MoveDirection {
        forward: (f / n) as f32,
        right: (r / n) as f32,
        steep,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCEL_RATE: f64 = 2048.0;

    /// The original's mapping (M1, M5, M6): the horizontal acceleration for
    /// a stick `(forward, right)` and a pawn rotation.
    fn original_acceleration(rotation: [i32; 3], forward: f64, right: f64) -> (f32, f32) {
        let [p, y, r] = rotation.map(table_angle);
        let x = (p.cos() * y.cos(), p.cos() * y.sin());
        let yv = (
            r.sin() * p.sin() * y.cos() - r.cos() * y.sin(),
            r.sin() * p.sin() * y.sin() + r.cos() * y.cos(),
        );
        let (wx, wy) = (forward * x.0 + right * yv.0, forward * x.1 + right * yv.1);
        let n = (wx * wx + wy * wy).sqrt();
        ((ACCEL_RATE * wx / n) as f32, (ACCEL_RATE * wy / n) as f32)
    }

    fn angle_deg(d: MoveDirection) -> f64 {
        f64::from(d.right).atan2(f64::from(d.forward)).to_degrees()
    }

    #[test]
    fn inverts_the_original_mapping() {
        for rotation in [
            [0, 0, 0],
            [0, 16384, 0],
            [0, -34894, 0],
            [0, 24422, 280],
            [4334, 12345, -1869],
            [-9008, 65535, 334],
            [65536 + 3319, -7, 65536 - 9],
        ] {
            for (f, r) in [
                (1.0, 0.0),
                (-1.0, 0.0),
                (0.0, 1.0),
                (0.0, -1.0),
                (0.6, 0.8),
                (-0.28, 0.96),
                (0.9, -0.1),
            ] {
                let n = f64::hypot(f, r);
                let (ax, ay) = original_acceleration(rotation, f, r);
                let d = move_direction(rotation, ax, ay, MoveFrame::Original).unwrap();
                assert!(!d.steep);
                assert!(
                    (f64::from(d.forward) - f / n).abs() < 2e-6
                        && (f64::from(d.right) - r / n).abs() < 2e-6,
                    "{rotation:?} ({f}, {r}) → {d:?}"
                );
                let len = f64::hypot(f64::from(d.forward), f64::from(d.right));
                assert!((len - 1.0).abs() < 1e-6);
            }
        }
    }

    /// M5: a stick held straight forward gives exactly "forward" in the
    /// truncated axes whatever the yaw's low two bits are; exact angles are
    /// off by up to three units.
    #[test]
    fn angles_are_truncated_to_the_table_step() {
        let unit = 360.0 / 65536.0;
        for low in 0..4 {
            let rotation = [0, 30640 + low, 0];
            let (ax, ay) = original_acceleration(rotation, 1.0, 0.0);
            let d = move_direction(rotation, ax, ay, MoveFrame::Original).unwrap();
            assert!(angle_deg(d).abs() < 1e-4, "{low}: {d:?}");
            let y = move_direction(rotation, ax, ay, MoveFrame::Yaw).unwrap();
            assert!(
                (angle_deg(y) + f64::from(low) * unit).abs() < 1e-4,
                "{low}: {}",
                angle_deg(y)
            );
        }
        // Negative and wrapped angles truncate towards −∞ on the 16-bit
        // circle: −2 is 65534, which the table reads as 65532.
        assert_eq!(table_angle(-2), table_angle(65532));
        assert_eq!(table_angle(65536 + 7), table_angle(4));
        assert_eq!(table_angle(i32::MIN), table_angle(0));
        assert_eq!(exact_angle(-1), exact_angle(65535));
    }

    /// M6: the pitch a pawn keeps from flying foreshortens its forward axis,
    /// so a diagonal stick gives an acceleration further to the side; with
    /// roll the right axis leans into the forward direction.
    #[test]
    fn pitch_and_roll_take_part() {
        let rotation = [4334, 0, -1869];
        let (ax, ay) = original_acceleration(rotation, 0.0, -1.0);
        let full = move_direction(rotation, ax, ay, MoveFrame::Original).unwrap();
        assert!((angle_deg(full) + 90.0).abs() < 1e-3);
        let yaw = move_direction(rotation, ax, ay, MoveFrame::Yaw).unwrap();
        // sin(roll)·sin(pitch) / cos(roll) = tan of 4.19°.
        assert!(
            (angle_deg(yaw) + 90.0 - 4.19).abs() < 0.02,
            "{}",
            angle_deg(yaw)
        );
    }

    #[test]
    fn zero_steep_and_hostile_values() {
        assert_eq!(move_direction([0; 3], 0.0, 0.0, MoveFrame::Original), None);
        assert_eq!(move_direction([0; 3], -0.0, 0.0, MoveFrame::Yaw), None);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(move_direction([0; 3], bad, 1.0, MoveFrame::Original), None);
            assert_eq!(move_direction([0; 3], 1.0, bad, MoveFrame::Yaw), None);
            assert_eq!(horizontal_magnitude(bad, 1.0), 0.0);
        }
        // Looking straight up: the forward axis has no horizontal part.
        let d = move_direction([16384, 0, 0], 0.0, 2048.0, MoveFrame::Original).unwrap();
        assert!(d.steep);
        assert_eq!((d.forward, d.right), (0.0, 1.0));
        // Extreme magnitudes and angles stay finite and of unit length.
        for (ax, ay) in [
            (f32::MAX, f32::MAX),
            (f32::MIN_POSITIVE, 0.0),
            (-1e-40, 1e-40),
        ] {
            for rotation in [[i32::MAX, i32::MIN, i32::MAX], [0, 1, 2]] {
                for frame in [MoveFrame::Original, MoveFrame::Yaw] {
                    let d = move_direction(rotation, ax, ay, frame).unwrap();
                    let len = f64::hypot(f64::from(d.forward), f64::from(d.right));
                    assert!((len - 1.0).abs() < 1e-6, "{ax} {ay} {rotation:?}: {d:?}");
                }
            }
        }
        assert_eq!(horizontal_magnitude(3.0, -4.0), 5.0);
    }
}
