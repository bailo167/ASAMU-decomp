//! Rotator angles as the original uses them (docs/PARITY_FINDINGS.md P7 and
//! P9; ABILITIES.md A-WK-1, A-CM-2, A-CM-8, A-CM-9; GRAPPLE.md G-TG-1).
//!
//! Where the expected values come from:
//!
//! - the index rule, the table's construction, the vector and axes formulas
//!   and the float-to-int conversion were read from the original's native
//!   code (addresses in `asamu_player::sim`), the look update and the pitch
//!   limit from the stock controller script;
//! - the table entries below were computed outside this repository from the
//!   constructor's formula (the correctly rounded sine of the `f32`
//!   argument), not with the code under test;
//! - integer sums and the pitch band are worked out here with plain integer
//!   arithmetic.

mod common;

use asamu_core::rotator::{normalize_rotator_axis, rotator_units_to_radians};
use asamu_player::sim::{
    angle_table_entry, angle_table_index, limit_view_pitch, look_delta_units, rotator_axes_flat,
    rotator_cos, rotator_sin, rotator_vector, view_units,
};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, grapple_gun,
    pawn, step_with,
};
use common::{DT, SplitMix64, flat_world};
use glam::Vec3;

fn original() -> PlayerParams {
    PlayerParams::asamu_original()
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, DT);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

/// The radian value that stands for a rotator angle in the simulation's
/// state: the image of its low 16 bits (−32777 is the same direction as
/// 32759).
fn image(units: i32) -> f32 {
    rotator_units_to_radians(normalize_rotator_axis(units))
}

/// A started pawn standing at the origin of `world`, facing `yaw_units`.
fn standing(params: &PlayerParams, world: &BoxWorld, yaw_units: i32) -> PlayerState {
    let mut s = common::standing(params, world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, params);
    tick(&mut s, &InputFrame::default(), params, world);
    assert!(s.grounded);
    s.yaw = image(yaw_units);
    s
}

/// A look input of whole rotator units, encoded as the trace converter does.
fn look(yaw_units: i32, pitch_units: i32) -> InputFrame {
    InputFrame {
        look_yaw_delta: rotator_units_to_radians(yaw_units),
        look_pitch_delta: rotator_units_to_radians(pitch_units),
        ..InputFrame::default()
    }
}

// Table entries from the constructor's formula, entry i =
// sin(f32(f64(f32(i) · 2) · π / 16384)) rounded to f32 (computed with an
// independent script). 8189 and 12285 are the sine and cosine entries of
// the yaw −32780.
const ENTRY_0: u32 = 0x0000_0000;
const ENTRY_1: u32 = 0x39C9_0FDB;
const ENTRY_2: u32 = 0x3A49_0FDA;
const ENTRY_4095: u32 = 0x3F7F_FFFF;
const ENTRY_4096: u32 = 0x3F80_0000;
const ENTRY_4097: u32 = 0x3F7F_FFFF;
const ENTRY_8189: u32 = 0x3A96_CD0F;
const ENTRY_8191: u32 = 0x39C9_1444;
const ENTRY_8192: u32 = 0xB3BB_BD2E;
const ENTRY_8193: u32 = 0xB9C9_0BBC;
const ENTRY_12285: u32 = 0xBF7F_FFF5;
const ENTRY_12288: u32 = 0xBF80_0000;
const ENTRY_16383: u32 = 0xB9C9_2888;

#[test]
fn the_table_index_is_bits_2_to_15_of_the_angle() {
    // Shift right by 2, keep 14 bits: a floor to a multiple of 4 on the
    // 16-bit circle, also for negative angles (two's complement).
    for (units, index) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 0),
        (4, 1),
        (7, 1),
        (16_384, 4_096),
        (32_767, 8_191),
        (32_768, 8_192),
        (65_535, 16_383),
        (65_536, 0),
        (65_536 + 7, 1),
        (-1, 16_383),
        (-3, 16_383),
        (-4, 16_383),
        (-5, 16_382),
        (-32_768, 8_192),
        (-32_777, 8_189),
        (-32_780, 8_189),
        (-32_776, 8_190),
        (i32::MIN, 0),
        (i32::MAX, 16_383),
    ] {
        assert_eq!(angle_table_index(units), index, "units {units}");
    }
    // Not a truncation towards zero: −1 reads the entry of −4, not of 0.
    assert_ne!(angle_table_index(-1), angle_table_index(0));
}

#[test]
fn table_entries_are_the_constructors() {
    for (index, bits) in [
        (0_u16, ENTRY_0),
        (1, ENTRY_1),
        (2, ENTRY_2),
        (4_095, ENTRY_4095),
        (4_096, ENTRY_4096),
        (4_097, ENTRY_4097),
        (8_189, ENTRY_8189),
        (8_191, ENTRY_8191),
        (8_192, ENTRY_8192),
        (8_193, ENTRY_8193),
        (12_285, ENTRY_12285),
        (12_288, ENTRY_12288),
        (16_383, ENTRY_16383),
    ] {
        assert_eq!(
            angle_table_entry(index).to_bits(),
            bits,
            "entry {index}: {}",
            angle_table_entry(index)
        );
    }
    // The index wraps like the table's mask.
    assert_eq!(angle_table_entry(16_384 + 2).to_bits(), ENTRY_2);

    // The whole table against the same independent computation: FNV-1a (64
    // bit) over the little-endian bytes of the 16384 entries, and the sum of
    // their bit patterns.
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    let mut sum = 0_u64;
    for index in 0..16_384_u16 {
        let bits = angle_table_entry(index).to_bits();
        sum += u64::from(bits);
        for byte in bits.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    assert_eq!(sum, 34_901_710_796_439);
    assert_eq!(hash, 0x3B7D_ABA5_A055_E607);
}

#[test]
fn sines_and_cosines_come_from_the_one_table() {
    // A cosine is the sine entry a quarter turn on. The f32 argument of the
    // 180° entry is f32(π), so that entry is −8.742278e-8 and not 0: the
    // table's cos 90° and sin 180° are both that value.
    assert_eq!(rotator_sin(0).to_bits(), ENTRY_0);
    assert_eq!(rotator_cos(0).to_bits(), ENTRY_4096);
    assert_eq!(rotator_sin(16_384).to_bits(), ENTRY_4096);
    assert_eq!(rotator_cos(16_384).to_bits(), ENTRY_8192);
    assert_eq!(rotator_sin(32_768).to_bits(), ENTRY_8192);
    assert_eq!(rotator_sin(-32_768).to_bits(), ENTRY_8192);
    assert_eq!(rotator_cos(32_768).to_bits(), ENTRY_12288);
    assert_eq!(rotator_sin(49_152).to_bits(), ENTRY_12288);
    assert_eq!(rotator_cos(49_152).to_bits(), ENTRY_0);
    assert_eq!(rotator_sin(-16_384).to_bits(), ENTRY_12288);
    assert_eq!(f32::from_bits(ENTRY_8192), -8.742_278e-8);
    // Units 1, 2 and 3 above a multiple of 4 read the same entries.
    for low in 1..4 {
        assert_eq!(rotator_sin(4 + low).to_bits(), ENTRY_1);
        assert_eq!(rotator_sin(-32_780 + low).to_bits(), ENTRY_8189);
        assert_eq!(rotator_cos(-32_780 + low).to_bits(), ENTRY_12285);
    }
}

#[test]
fn axes_and_vector_follow_the_native_formulas() {
    // GetAxes with yaw only: X = (cos, sin, 0), Y = (−sin, cos, 0).
    let (x, y) = rotator_axes_flat(-32_777);
    let (sin, cos) = (f32::from_bits(ENTRY_8189), f32::from_bits(ENTRY_12285));
    assert_eq!(x, Vec3::new(cos, sin, 0.0));
    assert_eq!(y, Vec3::new(-sin, cos, 0.0));
    // vector(rotation) = (cos p · cos y, cos p · sin y, sin p) in f32, not
    // renormalised. Pitch 3 reads the entries of pitch 0 (sin 0, cos 1).
    assert_eq!(rotator_vector(3, -32_777), Vec3::new(cos, sin, 0.0));
    // Pitch 16386 reads the entries of 16384: sin = 1, cos = entry 8192.
    let cos_pitch = f32::from_bits(ENTRY_8192);
    assert_eq!(
        rotator_vector(16_386, -32_777),
        Vec3::new(cos_pitch * cos, cos_pitch * sin, 1.0)
    );
}

/// Position and velocity after one tick of walking from rest with the
/// stick at (0.6 forward, 0.8 right), facing `yaw_units`.
fn one_walking_tick(yaw_units: i32) -> (Vec3, Vec3) {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, yaw_units);
    let stick = InputFrame {
        move_forward: 0.6,
        move_right: 0.8,
        ..InputFrame::default()
    };
    tick(&mut s, &stick, &params, &world);
    (s.position, s.velocity)
}

#[test]
fn move_axes_of_a_yaw_are_those_of_the_yaw_floored_to_4_units() {
    // Yaw units 1, 2 and 3 above a multiple of 4 move exactly like the
    // multiple itself; so does a negative yaw, whose floor is the multiple
    // below it (towards −∞).
    for (yaw, floored) in [
        (4_001, 4_000),
        (4_002, 4_000),
        (4_003, 4_000),
        (20_001, 20_000),
        (-1, -4),
        (-3, -4),
        (-32_777, -32_780),
        (-32_778, -32_780),
        (-32_779, -32_780),
        (-12_345, -12_348),
    ] {
        assert_eq!(
            one_walking_tick(yaw),
            one_walking_tick(floored),
            "yaw {yaw} moves like {floored}"
        );
    }
    // … and not like the next multiple up (what a truncation towards zero
    // would give for a negative yaw), nor like the multiple 4 below.
    assert_ne!(one_walking_tick(-32_777), one_walking_tick(-32_776));
    assert_ne!(one_walking_tick(-1), one_walking_tick(0));
    assert_ne!(one_walking_tick(4_003), one_walking_tick(4_004));
    assert_ne!(one_walking_tick(4_000), one_walking_tick(3_996));
}

#[test]
fn the_walking_direction_is_the_table_axis() {
    // Forward only, from rest, facing yaw −32777: the first tick's velocity
    // is along X = (cos, sin) of −32780 from the table. Its slope sin/cos is
    // −1.1505212e-3; the exact angle −32777 would give −8.63e-4, the exact
    // sine of −32780 −1.1504861e-3 (the entry's f32 argument is why the
    // table differs from that in the fifth digit).
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, -32_777);
    let forward = InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    };
    tick(&mut s, &forward, &params, &world);
    let (sin, cos) = (
        f64::from(f32::from_bits(ENTRY_8189)),
        f64::from(f32::from_bits(ENTRY_12285)),
    );
    assert!(s.velocity.x < -1.0, "{}", s.velocity);
    let slope = f64::from(s.velocity.y) / f64::from(s.velocity.x);
    assert!(
        (slope - sin / cos).abs() < 2.0e-9,
        "slope {slope} vs {}",
        sin / cos
    );
}

#[test]
fn the_axes_are_those_of_the_yaw_before_the_ticks_look_update() {
    // The controller builds the axes from the pawn's rotation, which is the
    // view of the previous tick, and only then turns the view (A-WK-1). A
    // quarter turn in the same tick as a forward push: the push goes along
    // the old X axis (1, 0) exactly, the yaw becomes 16384 units.
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, 0);
    let turn = InputFrame {
        move_forward: 1.0,
        ..look(16_384, 0)
    };
    tick(&mut s, &turn, &params, &world);
    assert_eq!(s.yaw_units(), 16_384);
    assert_eq!(s.yaw, core::f32::consts::FRAC_PI_2);
    assert!(s.velocity.x > 1.0, "{}", s.velocity);
    assert_eq!(s.velocity.y, 0.0);
    // The next tick uses the new yaw: X = (entry 8192, 1).
    let mut t = standing(&params, &world, 16_384);
    let forward = InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    };
    tick(&mut t, &forward, &params, &world);
    assert!(t.velocity.y > 1.0, "{}", t.velocity);
    let slope = f64::from(t.velocity.x) / f64::from(t.velocity.y);
    assert!((slope - f64::from(f32::from_bits(ENTRY_8192))).abs() < 1.0e-12);
}

/// The walk bob (the sideways sway of the view while walking) after `ticks`
/// ticks of walking forward from rest, facing `yaw_units`.
fn walk_bob_after(yaw_units: i32, ticks: u32) -> Vec3 {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, yaw_units);
    let forward = InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    };
    for _ in 0..ticks {
        tick(&mut s, &forward, &params, &world);
    }
    s.script.walk_bob
}

#[test]
fn the_walk_bob_sways_along_the_table_axis_of_the_floored_yaw() {
    // The bob's sideways part is the pawn's right axis from `GetAxes`
    // (A-CM-4, A-CM-8): Y = (−sin, cos) of the yaw floored to 4 units, table
    // values. Yaw −32777 reads the entries of −32780.
    let (sin, cos) = (
        f64::from(f32::from_bits(ENTRY_8189)),
        f64::from(f32::from_bits(ENTRY_12285)),
    );
    let bob = walk_bob_after(-32_777, 20);
    let sideways = f64::from(bob.x).hypot(f64::from(bob.y));
    assert!(sideways > 0.5, "the pawn sways: {bob}");
    // Parallel to (−sin, cos): the cross product vanishes to the rounding of
    // the two components (each a product of three f32 factors).
    let cross = f64::from(bob.x) * cos + f64::from(bob.y) * sin;
    assert!(
        cross.abs() < 4.0e-7 * sideways,
        "{bob} is not along the table axis"
    );
    // The right axis of the exact angle −32777 is 3 units (2.9e-4 rad) away
    // from that: against it the same cross product is a thousand times
    // larger than the bound above.
    let exact = f64::from(-32_777) * core::f64::consts::TAU / 65_536.0;
    let off = f64::from(bob.x) * exact.cos() + f64::from(bob.y) * exact.sin();
    assert!(off.abs() > 2.0e-4 * sideways, "{off}");

    // Bit for bit the bob of the floored yaw, for units 1 to 3 above a
    // multiple of 4 and for negative yaws (floor towards −∞) …
    for (yaw, floored) in [
        (4_001, 4_000),
        (4_003, 4_000),
        (-1, -4),
        (-32_777, -32_780),
        (-12_345, -12_348),
    ] {
        assert_eq!(
            walk_bob_after(yaw, 20),
            walk_bob_after(floored, 20),
            "yaw {yaw} sways like {floored}"
        );
    }
    // … and not that of the next multiple up, which a truncation towards
    // zero would read for a negative yaw.
    assert_ne!(walk_bob_after(-1, 20), walk_bob_after(0, 20));
    assert_ne!(walk_bob_after(-32_777, 20), walk_bob_after(-32_776, 20));
    assert_ne!(walk_bob_after(4_003, 20), walk_bob_after(4_004, 20));
}

#[test]
fn whole_unit_deltas_survive_the_radian_interface() {
    // Every whole number of units, written as radians the way the trace
    // converter writes it, is read back as that number.
    for units in -70_000..=70_000 {
        assert_eq!(
            look_delta_units(rotator_units_to_radians(units)),
            units,
            "{units}"
        );
    }
}

#[test]
fn a_fraction_of_a_unit_is_dropped_towards_zero() {
    // The original assigns the float turn to an integer rotator: the
    // float-to-int conversion truncates.
    let radians = |units: f64| (units * core::f64::consts::TAU / 65_536.0) as f32;
    for (units, whole) in [
        (2.9, 2),
        (-2.9, -2),
        (0.999, 0),
        (-0.999, 0),
        (2.5, 2),
        (-2.5, -2),
        (1_000.25, 1_000),
        (-1_000.75, -1_000),
        (0.0, 0),
    ] {
        assert_eq!(look_delta_units(radians(units)), whole, "{units}");
    }
    assert_eq!(look_delta_units(f32::NAN), 0);
    assert_eq!(look_delta_units(f32::INFINITY), 0);
    assert_eq!(look_delta_units(f32::NEG_INFINITY), 0);
}

/// The stock pitch limit, written out from the rule: reduce to 0..=65535; a
/// value strictly between `max` and `65535 − max` goes to `max` below half a
/// turn and to `65535 − max` otherwise; read as a signed 16-bit value.
fn limited_pitch(pitch: i64, max: i64) -> i64 {
    let p = pitch.rem_euclid(65_536);
    let p = if p > max && p < 65_535 - max {
        if p < 32_768 { max } else { 65_535 - max }
    } else {
        p
    };
    if p >= 32_768 { p - 65_536 } else { p }
}

#[test]
fn the_pitch_limit_is_the_stock_band() {
    // ViewPitchMax 18000: the upper end is +18000 (measured: 50 recorded
    // frames at exactly 18000, none above). The lower end is 65535 − 18000 =
    // 47535 on the 16-bit circle, which is −18001.
    for (pitch, limited) in [
        (0, 0),
        (17_999, 17_999),
        (18_000, 18_000),
        (18_001, 18_000),
        (32_767, 18_000),
        (32_768, -18_001),
        (47_534, -18_001),
        (47_535, -18_001),
        (47_536, -18_000),
        (-18_000, -18_000),
        (-18_001, -18_001),
        (-18_002, -18_001),
        (-1, -1),
        (65_536 + 5, 5),
    ] {
        assert_eq!(limit_view_pitch(pitch, 18_000.0), limited, "{pitch}");
        assert_eq!(limited_pitch(i64::from(pitch), 18_000), i64::from(limited));
    }

    // Through the tick: three looks up of 7000 units reach the limit, the
    // radian value is the image of 18000 (98.876953125°).
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, 0);
    for expected in [7_000, 14_000, 18_000, 18_000] {
        tick(&mut s, &look(0, 7_000), &params, &world);
        assert_eq!(s.pitch_units(), expected);
    }
    assert_eq!(s.pitch, rotator_units_to_radians(18_000));
    assert_eq!(s.pitch, (18_000.0_f32 * 360.0 / 65_536.0).to_radians());
    // Down again in steps of 9000: 9000, 0, −9000, −18000, then the lower
    // end one unit further.
    for expected in [9_000, 0, -9_000, -18_000, -18_001, -18_001] {
        tick(&mut s, &look(0, -9_000), &params, &world);
        assert_eq!(s.pitch_units(), expected);
    }
}

#[test]
fn yaw_and_pitch_are_exact_after_10000_integer_deltas_of_mixed_sign() {
    // The view angles are integers: after any number of whole-unit deltas
    // the yaw is the sum of the deltas on the 16-bit circle and the pitch
    // is the limited sum, with no rounding left behind. Checked on every
    // tick against plain integer arithmetic.
    let params = original();
    let world = flat_world();
    let start_yaw = -32_777;
    let mut s = standing(&params, &world, start_yaw);
    let mut rng = SplitMix64(0x5EED_0009);
    let (mut yaw, mut pitch) = (i64::from(start_yaw), 0_i64);
    let (mut signs, mut at_upper, mut at_lower) = ([0_u32; 2], 0_u32, 0_u32);
    for n in 0..10_000 {
        // Yaw deltas of both signs with a drift to the right, so the yaw
        // goes round several times.
        let d_yaw = (rng.next_u64() % 1_401) as i32 - 700 + 25;
        // Pitch deltas drift upwards in the first half and downwards in the
        // second, so both limits are reached.
        let bias = if n < 5_000 { 40 } else { -40 };
        let d_pitch = (rng.next_u64() % 801) as i32 - 400 + bias;
        signs[usize::from(d_yaw < 0)] += 1;
        tick(&mut s, &look(d_yaw, d_pitch), &params, &world);

        yaw += i64::from(d_yaw);
        let wrapped = (yaw + 32_768).rem_euclid(65_536) - 32_768;
        pitch = limited_pitch(pitch + i64::from(d_pitch), 18_000);
        at_upper += u32::from(pitch == 18_000);
        at_lower += u32::from(pitch == -18_001);

        assert_eq!(i64::from(s.yaw_units()), wrapped, "tick {n}");
        assert_eq!(i64::from(s.pitch_units()), pitch, "tick {n}");
        assert_eq!(
            s.yaw.to_bits(),
            rotator_units_to_radians(wrapped as i32).to_bits(),
            "tick {n}"
        );
        assert_eq!(
            s.pitch.to_bits(),
            rotator_units_to_radians(pitch as i32).to_bits(),
            "tick {n}"
        );
    }
    assert!(signs[0] > 4_000 && signs[1] > 4_000, "{signs:?}");
    assert!(at_upper > 100 && at_lower > 100, "{at_upper} {at_lower}");
    assert!(yaw.abs() > 65_536, "the yaw went round: {yaw}");
}

#[test]
fn a_view_that_does_not_change_is_left_bit_for_bit() {
    // A view written from outside (a teleport, a replay start) is taken as
    // its nearest unit, and a tick without a look delta does not rewrite it.
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world, 0);
    s.yaw = 0.75;
    s.pitch = -0.25;
    assert_eq!(view_units(0.75), 7_823);
    assert_eq!(view_units(-0.25), -2_608);
    tick(&mut s, &InputFrame::default(), &params, &world);
    assert_eq!((s.yaw, s.pitch), (0.75, -0.25));
    // One unit of look moves it to the image of the next unit.
    tick(&mut s, &look(1, -1), &params, &world);
    assert_eq!(s.yaw, rotator_units_to_radians(7_824));
    assert_eq!(s.pitch, rotator_units_to_radians(-2_609));
}

#[test]
fn the_aim_is_the_native_vector_of_the_cached_view() {
    // G-TG-1 with A-CM-8: the fire trace runs along vector(rotation) of the
    // camera's cached view, each angle floored to 4 units by the table.
    let params = original();
    // A wall whose face is at x = −1000, facing +X.
    let world = BoxWorld::new().with_box(
        Vec3::new(-1_100.0, -5_000.0, -5_000.0),
        Vec3::new(-1_000.0, 5_000.0, 8_000.0),
        true,
    );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1_000.0), image(-32_777));
    s.pitch = image(3);
    pawn::start(&mut s, &params);
    grapple_gun::set_max_grapples(&mut s, 3);
    let (sin, cos) = (f32::from_bits(ENTRY_8189), f32::from_bits(ENTRY_12285));
    assert_eq!(s.native_view_direction(), Vec3::new(cos, sin, 0.0));

    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &fire, &params, &world);
    assert_eq!(s.aim_direction(), Vec3::new(cos, sin, 0.0));
    let anchor = e.gun.attached.expect("attached").anchor;
    // The ray leaves the eye (38 above the centre) level, so the anchor is
    // at the eye's height and, at the wall about 1000 uu away, sideways by
    // sin / |cos| = 1.15052e-3 of the distance: 1.15 uu. With the exact
    // angle of −32777 units it would be tan(9 units) = 8.6286e-4: 0.86 uu.
    let slope = f64::from(sin) / f64::from(cos).abs();
    assert!((slope - 1.150_52e-3).abs() < 1.0e-8);
    assert!((anchor.x + 1_000.0).abs() < 5.0, "{anchor}");
    assert!(
        (f64::from(anchor.y) / f64::from(-anchor.x) - slope).abs() < 1.0e-6,
        "anchor {anchor} vs slope {slope}"
    );
    assert!((anchor.z - 1_038.0).abs() < 1.0e-3, "{anchor}");
}
