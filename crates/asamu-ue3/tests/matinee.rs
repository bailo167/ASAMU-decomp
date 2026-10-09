//! Matinee decoding and evaluation against synthetic data written here (no
//! original game data): curve evaluation and automatic tangents with exact
//! expected values, rotator/quaternion helpers, move-track transforms,
//! playback stepping, value-level track decoding, and a synthetic map
//! package (actions, groups, tracks, bindings, a prefab instance, a camera
//! animation) built byte by byte.

#![allow(clippy::unwrap_used, clippy::float_cmp)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use asamu_ue3::kismet::{ClassDefaults, NodeScope, build_graph};
use asamu_ue3::matinee::{
    self, CurveMode, CurvePoint, GroupKind, InterpCurve, InterpMethod, InterpSettings, LinearColor,
    MatineeMap, MoveAxis, MoveAxisTrack, MoveFrame, MoveInstance, MoveRotation, MoveTrack,
    NoGroupActors, Playback, RotMode, TrackData, clamp_float_tangent, decode_curve, decode_track,
    euler_from_rotator, hermite, matrix_rotator, normalize_axis, quat_from_euler,
    rotation_translation_matrix, rotator_from_euler, rotator_from_quat, slerp_quat,
    winding_and_remainder, world_key_transform,
};
use asamu_ue3::schema::{PropertyDef, PropertyType, Schema, StructDef, StructKind};
use asamu_ue3::{ObjRef, Package, Property, Value};
use common::{Export, Import, Synth, W};

// ================================================================== curves

fn fpt(t: f32, v: f32, mode: CurveMode) -> CurvePoint<f32> {
    CurvePoint::new(t, v, mode)
}

fn fcurve(points: Vec<CurvePoint<f32>>) -> InterpCurve<f32> {
    InterpCurve::new(points)
}

#[test]
fn empty_single_and_out_of_range_keys() {
    let empty: InterpCurve<f32> = InterpCurve::default();
    assert_eq!(empty.eval(1.0, 7.5), 7.5);
    assert_eq!(empty.eval_indexed(1.0, 0.0).1, None);
    let one = fcurve(vec![fpt(2.0, 3.0, CurveMode::CurveUser)]);
    assert_eq!(one.eval(-5.0, 0.0), 3.0);
    assert_eq!(one.eval(50.0, 0.0), 3.0);
    let two = fcurve(vec![
        fpt(0.0, 1.0, CurveMode::Linear),
        fpt(2.0, 5.0, CurveMode::Linear),
    ]);
    assert_eq!(two.eval_indexed(-1.0, 0.0), (1.0, Some(0)));
    assert_eq!(two.eval_indexed(0.0, 0.0), (1.0, Some(0)));
    assert_eq!(two.eval_indexed(2.0, 0.0), (5.0, Some(1)));
    assert_eq!(two.eval_indexed(9.0, 0.0), (5.0, Some(1)));
    // NaN time: neither before the first key nor inside: the last key.
    assert_eq!(two.eval_indexed(f32::NAN, 0.0), (5.0, Some(1)));
    assert_eq!(two.in_range(), (0.0, 2.0));
}

#[test]
fn linear_constant_and_cubic_segments() {
    let lin = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::Linear),
        fpt(2.0, 10.0, CurveMode::Linear),
    ]);
    assert_eq!(lin.eval(0.5, 0.0), 2.5);
    assert_eq!(lin.eval_indexed(1.5, 0.0), (7.5, Some(0)));
    let constant = fcurve(vec![
        fpt(0.0, 4.0, CurveMode::Constant),
        fpt(2.0, 10.0, CurveMode::Linear),
    ]);
    assert_eq!(constant.eval(1.999, 0.0), 4.0);
    assert_eq!(constant.eval(2.0, 0.0), 10.0);
    // Cubic with flat tangents: the Hermite basis gives 5 at the midpoint.
    for mode in [
        CurveMode::CurveUser,
        CurveMode::CurveAuto,
        CurveMode::CurveAutoClamped,
        CurveMode::CurveBreak,
    ] {
        let c = fcurve(vec![fpt(0.0, 0.0, mode), fpt(2.0, 10.0, mode)]);
        assert_eq!(c.eval(1.0, 0.0), 5.0, "{mode:?}");
    }
    // The segment's mode is the start key's: a linear end key does not
    // make the segment linear.
    let mixed = fcurve(vec![
        CurvePoint::with_tangents(0.0, 0.0, 0.0, 4.0, CurveMode::CurveBreak),
        fpt(2.0, 10.0, CurveMode::Linear),
    ]);
    // Leave tangent 4 scaled by the span 2: 0.125·8 + 0.5·10 = 6.
    assert_eq!(mixed.eval(1.0, 0.0), 6.0);
    let mut broken = mixed.clone();
    broken.method = InterpMethod::BrokenTangentEval;
    // Unscaled tangent: 0.125·4 + 5 = 5.5.
    assert_eq!(broken.eval(1.0, 0.0), 5.5);
}

#[test]
fn hermite_matches_the_basis_in_documented_order() {
    let (p0, t0, p1, t1) = (1.25f32, -3.5f32, 7.75f32, 0.625f32);
    for i in 0..=16 {
        let a = i as f32 / 16.0;
        let a2 = a * a;
        let a3 = a * a2;
        let expect =
            ((((a3 + a3) - 3.0 * a2) + 1.0) * p0 + ((a3 - (a2 + a2)) + a) * t0 + (a3 - a2) * t1)
                + (3.0 * a2 - (a3 + a3)) * p1;
        assert_eq!(hermite(p0, t0, p1, t1, a).to_bits(), expect.to_bits());
    }
    assert_eq!(hermite(p0, t0, p1, t1, 0.0), p0);
    assert_eq!(hermite(p0, t0, p1, t1, 1.0), p1);
}

#[test]
fn cubic_uses_leave_of_start_and_arrive_of_end() {
    let c = fcurve(vec![
        CurvePoint::with_tangents(0.0, 0.0, 100.0, 2.0, CurveMode::CurveUser),
        CurvePoint::with_tangents(1.0, 1.0, -3.0, 100.0, CurveMode::CurveUser),
    ]);
    let a = 0.25f32;
    let expect = hermite(0.0, 2.0 * 1.0, 1.0, 1.0 * -3.0, a);
    assert_eq!(c.eval(0.25, 0.0).to_bits(), expect.to_bits());
}

#[test]
fn vector_and_color_curves_evaluate_per_component() {
    let v = InterpCurve::new(vec![
        CurvePoint::new(0.0, [0.0, 10.0, -4.0], CurveMode::Linear),
        CurvePoint::new(4.0, [8.0, 10.0, 4.0], CurveMode::Linear),
    ]);
    assert_eq!(v.eval(1.0, [0.0; 3]), [2.0, 10.0, -2.0]);
    let c = InterpCurve::new(vec![
        CurvePoint::new(0.0, LinearColor([0.0, 0.0, 0.0, 1.0]), CurveMode::Linear),
        CurvePoint::new(2.0, LinearColor([1.0, 0.5, 0.0, 0.0]), CurveMode::Linear),
    ]);
    assert_eq!(
        c.eval(1.0, LinearColor([9.0; 4])),
        LinearColor([0.5, 0.25, 0.0, 0.5])
    );
}

#[test]
fn unsorted_keys_follow_the_native_search() {
    // The last key bounds the range even when an earlier key is later.
    let c = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::Linear),
        fpt(2.0, 4.0, CurveMode::Linear),
        fpt(1.0, 6.0, CurveMode::Linear),
    ]);
    assert_eq!(c.eval(1.5, 0.0), 6.0);
    assert_eq!(c.eval(0.5, 0.0), 1.0);
    // A NaN key time makes its segment hold the start value.
    let n = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::Linear),
        fpt(f32::NAN, 5.0, CurveMode::Linear),
        fpt(2.0, 9.0, CurveMode::Linear),
    ]);
    assert_eq!(n.eval(1.0, 0.0), 5.0);
}

// ================================================================== tangents

#[test]
fn auto_tangents_float_divides_vector_multiplies_by_reciprocal() {
    let mut f = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::CurveAuto),
        fpt(1.0, 1.0, CurveMode::CurveAuto),
        fpt(3.0, 5.0, CurveMode::CurveAuto),
    ]);
    f.points[0].arrive_tangent = 9.0;
    f.points[0].leave_tangent = 9.0;
    f.points[2].arrive_tangent = 9.0;
    f.points[2].leave_tangent = 9.0;
    f.auto_set_tangents(0.0);
    // First key: leave zeroed, arrive kept; last key: arrive zeroed.
    assert_eq!(f.points[0].arrive_tangent, 9.0);
    assert_eq!(f.points[0].leave_tangent, 0.0);
    assert_eq!(f.points[2].arrive_tangent, 0.0);
    assert_eq!(f.points[2].leave_tangent, 9.0);
    // ((1 − 0) + (5 − 1)) / 3 in f32.
    assert_eq!(f.points[1].arrive_tangent, 5.0f32 / 3.0);
    assert_eq!(
        f.points[1].leave_tangent.to_bits(),
        1.666_666_6_f32.to_bits()
    );

    let mut v = InterpCurve::new(vec![
        CurvePoint::new(0.0, [0.0; 3], CurveMode::CurveAuto),
        CurvePoint::new(1.0, [1.0, 0.0, 0.0], CurveMode::CurveAuto),
        CurvePoint::new(3.0, [5.0, 0.0, 0.0], CurveMode::CurveAuto),
    ]);
    v.auto_set_tangents(0.0);
    // 5 · (1/3) rounds differently from 5/3.
    assert_eq!(v.points[1].leave_tangent[0], 5.0f32 * (1.0f32 / 3.0));
    assert_eq!(
        v.points[1].leave_tangent[0].to_bits(),
        1.666_666_7_f32.to_bits()
    );
    assert_ne!(
        v.points[1].leave_tangent[0].to_bits(),
        f.points[1].leave_tangent.to_bits()
    );
}

#[test]
fn auto_tangent_tension_methods_and_neighbours() {
    let base = || {
        fcurve(vec![
            fpt(0.0, 0.0, CurveMode::CurveAuto),
            fpt(1.0, 1.0, CurveMode::CurveAuto),
            fpt(2.0, 5.0, CurveMode::CurveAuto),
        ])
    };
    let mut t = base();
    t.auto_set_tangents(0.5);
    assert_eq!(t.points[1].leave_tangent, ((1.0f32 + 4.0) * 0.5) / 2.0);
    let mut old = base();
    old.method = InterpMethod::FixedTangentEval;
    old.auto_set_tangents(0.0);
    assert_eq!(old.points[1].leave_tangent, 2.5);
    // A constant previous key zeroes both tangents; a linear one keeps them.
    let mut c = base();
    c.points[0].mode = CurveMode::Constant;
    c.points[1].arrive_tangent = 7.0;
    c.auto_set_tangents(0.0);
    assert_eq!(
        (c.points[1].arrive_tangent, c.points[1].leave_tangent),
        (0.0, 0.0)
    );
    let mut l = base();
    l.points[0].mode = CurveMode::Linear;
    l.points[1].arrive_tangent = 7.0;
    l.points[1].leave_tangent = 8.0;
    l.auto_set_tangents(0.0);
    assert_eq!(
        (l.points[1].arrive_tangent, l.points[1].leave_tangent),
        (7.0, 8.0)
    );
    // User keys are never touched; a single key loses its leave tangent.
    let mut u = fcurve(vec![CurvePoint::with_tangents(
        0.0,
        1.0,
        3.0,
        4.0,
        CurveMode::CurveUser,
    )]);
    u.auto_set_tangents(0.0);
    assert_eq!(
        (u.points[0].arrive_tangent, u.points[0].leave_tangent),
        (3.0, 0.0)
    );
}

#[test]
fn clamped_tangents_flatten_extremes_and_blend_near_neighbours() {
    // Local maximum and minimum: flat.
    assert_eq!(clamp_float_tangent(0.0, 0.0, 5.0, 1.0, 0.0, 2.0), 0.0);
    assert_eq!(clamp_float_tangent(5.0, 0.0, 0.0, 1.0, 5.0, 2.0), 0.0);
    // Plateau counts as an extreme.
    assert_eq!(clamp_float_tangent(1.0, 0.0, 1.0, 1.0, 3.0, 2.0), 0.0);
    // Middle third: the average slope.
    assert_eq!(clamp_float_tangent(0.0, 0.0, 1.0, 1.0, 2.0, 2.0), 1.0);
    // Lower third, rising: blended towards the previous slope, never above
    // the average slope.
    let blend = 0.05f32 / -0.333 + 1.0;
    let expect = (0.1f32 - 1.0) * blend + 1.0;
    let got = clamp_float_tangent(0.0, 0.0, 0.1, 1.0, 2.0, 2.0);
    assert_eq!(got.to_bits(), expect.to_bits());
    assert!(got < 1.0);
    // Upper third, falling: blended towards the next slope, never below the
    // average slope.
    let got = clamp_float_tangent(2.0, 0.0, 0.1, 1.0, 0.0, 2.0);
    let alpha = (0.1f32 - 2.0) / (0.0 - 2.0);
    let blend = (alpha + -0.667) / 0.333;
    let expect = ((-0.1f32) - (-1.0)) * blend + (-1.0);
    assert_eq!(got.to_bits(), expect.max(-1.0).to_bits());
    // Coincident times use the 1e-4 floor instead of dividing by zero.
    assert!(clamp_float_tangent(0.0, 1.0, 1.0, 1.0, 2.0, 1.0).is_finite());

    let mut c = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::CurveAutoClamped),
        fpt(1.0, 0.1, CurveMode::CurveAutoClamped),
        fpt(2.0, 2.0, CurveMode::CurveAutoClamped),
    ]);
    c.auto_set_tangents(0.25);
    let expect = clamp_float_tangent(0.0, 0.0, 0.1, 1.0, 2.0, 2.0) * 0.75;
    assert_eq!(c.points[1].arrive_tangent.to_bits(), expect.to_bits());
}

// ================================================================== rotations

#[test]
fn euler_rotator_conversions_truncate_and_scale_exactly() {
    assert_eq!(
        rotator_from_euler([90.0, 45.0, -30.0]),
        [8192, -5461, 16384]
    );
    assert_eq!(rotator_from_euler([0.0, 0.0, 720.0]), [0, 131_072, 0]);
    assert_eq!(rotator_from_euler([f32::NAN, 0.0, 0.0])[2], i32::MIN);
    assert_eq!(
        euler_from_rotator([16384, -8192, 32768]),
        [180.0, 90.0, -45.0]
    );
    assert_eq!(normalize_axis(70000), 4464);
    assert_eq!(normalize_axis(-70000), -4464);
    assert_eq!(normalize_axis(32768), -32768);
    assert_eq!(normalize_axis(32767), 32767);
    let (w, r) = winding_and_remainder([70000, -70000, 32768]);
    assert_eq!(r, [4464, -4464, -32768]);
    assert_eq!(w, [65536, -65536, 65536]);
}

#[test]
fn matrix_rotator_round_trips_within_one_unit() {
    for &rot in &[
        [0, 0, 0],
        [0, 16384, 0],
        [4000, -12000, 3000],
        [-8000, 30000, -20000],
        [12000, 100, 32000],
    ] {
        let m = rotation_translation_matrix(rot, [1.0, 2.0, 3.0]);
        for clean in [false, true] {
            let back = matrix_rotator(&m, clean).map(normalize_axis);
            for k in 0..3 {
                let d = normalize_axis(back[k].wrapping_sub(rot[k])).abs();
                assert!(d <= 1, "{rot:?} -> {back:?} (clean {clean})");
            }
        }
    }
}

#[test]
fn quaternion_helpers_agree_with_rotators() {
    for e in [[0.0, 0.0, 0.0], [10.0, 20.0, 30.0], [-45.0, 60.0, 170.0]] {
        let r = rotator_from_quat(quat_from_euler(e));
        let expect = rotator_from_euler(e);
        for k in 0..3 {
            let d = normalize_axis(r[k].wrapping_sub(expect[k])).abs();
            // The matrix goes through the 16384-entry table: 4-unit steps.
            assert!(d <= 4, "{e:?}: {r:?} vs {expect:?}");
        }
    }
    let a = quat_from_euler([0.0, 0.0, 0.0]);
    let b = quat_from_euler([0.0, 0.0, 90.0]);
    let mid = rotator_from_quat(slerp_quat(a, b, 0.5));
    assert!((mid[1] - 8192).abs() <= 2, "{mid:?}");
    // The opposite sign of the same rotation takes the same (short) path.
    let nb = b.map(|c| -c);
    let mid2 = rotator_from_quat(slerp_quat(a, nb, 0.5));
    assert!((mid2[1] - 8192).abs() <= 2, "{mid2:?}");
    // Nearly identical quaternions blend linearly.
    let q = slerp_quat(a, a, 0.3);
    assert!((q[3] - 1.0).abs() < 1e-6);
}

// ================================================================== move tracks

fn move_track(frame: MoveFrame) -> MoveTrack {
    MoveTrack {
        pos: InterpCurve::new(vec![
            CurvePoint::new(0.0, [0.0; 3], CurveMode::Linear),
            CurvePoint::new(1.0, [100.0, 0.0, 0.0], CurveMode::Linear),
        ]),
        euler: InterpCurve::new(vec![
            CurvePoint::new(0.0, [0.0; 3], CurveMode::Linear),
            CurvePoint::new(1.0, [0.0, 0.0, 90.0], CurveMode::Linear),
        ]),
        lookup: Vec::new(),
        move_frame: frame,
        rot_mode: RotMode::Keyframed,
        look_at_group: None,
        lin_curve_tension: 0.0,
        ang_curve_tension: 0.0,
        use_quat_interpolation: false,
        disable_movement: false,
        use_raw_actor_tm: false,
        axes: Vec::new(),
    }
}

fn close3(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() <= tol)
}

#[test]
fn position_and_rotation_follow_the_curves() {
    let t = move_track(MoveFrame::World);
    let a = NoGroupActors;
    assert_eq!(t.eval_position(0.5, &a), t.pos.eval(0.5, [0.0; 3]));
    assert_eq!(t.eval_euler(0.5, &a), [0.0, 0.0, 45.0]);
    let (p, r) = t.key_transform(1.0, &a);
    assert_eq!(p, [100.0, 0.0, 0.0]);
    assert_eq!(r, [0, 16384, 0]);
    // World frame, unattached: the keys are the world transform.
    let inst = MoveInstance::new(&t, [5.0, 5.0, 5.0], [0, 0, 0], 0.0, &a);
    let s = t.sample(1.0, &inst, &a).unwrap();
    assert!(close3(s.location, [100.0, 0.0, 0.0], 1e-4));
    assert_eq!(s.rotation, MoveRotation::Set([0, 16384, 0]));
}

#[test]
fn relative_tracks_start_at_the_actor_and_move_in_its_frame() {
    let mut t = move_track(MoveFrame::RelativeToInitial);
    let a = NoGroupActors;
    let loc = [100.0, 200.0, 300.0];
    let rot = [0, 16384, 0];
    let inst = MoveInstance::new(&t, loc, rot, 0.0, &a);
    let s0 = t.sample(0.0, &inst, &a).unwrap();
    assert!(close3(s0.location, loc, 1e-3), "{:?}", s0.location);
    assert_eq!(s0.rotation, MoveRotation::Set(rot));
    // +100 along the actor's X axis, which faces world +Y after a 90° yaw.
    let s1 = t.sample(1.0, &inst, &a).unwrap();
    assert!(
        close3(s1.location, [100.0, 300.0, 300.0], 1e-3),
        "{:?}",
        s1.location
    );
    // 90° + 90° = 180°, reported normalized as -32768.
    assert_eq!(s1.rotation, MoveRotation::Set([0, -32768, 0]));
    // A track whose first key is not at the origin still starts at the
    // actor (the first key is factored out of the initial transform).
    t.pos.points[0].out_val = [50.0, 0.0, 0.0];
    t.pos.points[1].out_val = [150.0, 0.0, 0.0];
    let inst = MoveInstance::new(&t, loc, rot, 0.0, &a);
    let s0 = t.sample(0.0, &inst, &a).unwrap();
    assert!(close3(s0.location, loc, 1e-3), "{:?}", s0.location);
    let s1 = t.sample(1.0, &inst, &a).unwrap();
    assert!(
        close3(s1.location, [100.0, 300.0, 300.0], 1e-3),
        "{:?}",
        s1.location
    );
    // With the raw actor transform the keys are offsets from the actor.
    t.use_raw_actor_tm = true;
    let inst = MoveInstance::new(&t, loc, rot, 0.0, &a);
    let s0 = t.sample(0.0, &inst, &a).unwrap();
    assert!(
        close3(s0.location, [100.0, 250.0, 300.0], 1e-3),
        "{:?}",
        s0.location
    );
}

#[test]
fn whole_turns_survive_the_frame_change() {
    let mut t = move_track(MoveFrame::World);
    t.euler.points[1].out_val = [0.0, 0.0, 720.0];
    let inst = MoveInstance::new(&t, [0.0; 3], [0, 0, 0], 0.0, &NoGroupActors);
    let s = t.sample(1.0, &inst, &NoGroupActors).unwrap();
    assert_eq!(s.rotation, MoveRotation::Set([0, 131_072, 0]));
    let s = t.sample(0.75, &inst, &NoGroupActors).unwrap();
    // 540° = one whole turn plus a remainder of -180° (normalized).
    assert_eq!(s.rotation, MoveRotation::Set([0, 98304, 0]));
    let (p, r) = world_key_transform(
        [1.0, 2.0, 3.0],
        [0, 65536 + 100, 0],
        &rotation_translation_matrix([0, 0, 0], [10.0, 0.0, 0.0]),
    );
    assert_eq!(p, [11.0, 2.0, 3.0]);
    assert_eq!(r, [0, 65636, 0]);
}

#[test]
fn split_axes_replace_the_vector_curves() {
    let mut t = move_track(MoveFrame::World);
    t.axes = (0..6)
        .map(|k| MoveAxisTrack {
            path: format!("Axis{k}"),
            axis: MoveAxis::from_index(k).unwrap(),
            curve: InterpCurve::new(vec![CurvePoint::new(
                0.0,
                (k as f32 + 1.0) * 10.0,
                CurveMode::Constant,
            )]),
            lookup: Vec::new(),
        })
        .collect();
    let a = NoGroupActors;
    assert_eq!(t.eval_position(0.3, &a), [10.0, 20.0, 30.0]);
    assert_eq!(t.eval_euler(0.3, &a), [40.0, 50.0, 60.0]);
    // Sub-tracks win over the quaternion path.
    t.use_quat_interpolation = true;
    assert_eq!(
        t.key_transform(0.3, &a).1,
        rotator_from_euler([40.0, 50.0, 60.0])
    );
}

#[test]
fn quaternion_interpolation_slerps_between_keys() {
    let mut t = move_track(MoveFrame::World);
    t.use_quat_interpolation = true;
    let a = NoGroupActors;
    let r = t.key_transform(0.5, &a).1;
    assert!((r[1] - 8192).abs() <= 2, "{r:?}");
    assert_eq!(t.key_transform(-1.0, &a).1, [0, 0, 0]);
    let end = t.key_transform(2.0, &a).1;
    assert!((end[1] - 16384).abs() <= 1, "{end:?}");
    // Keys modes are ignored: a constant key still slerps.
    t.euler.points[0].mode = CurveMode::Constant;
    let r = t.key_transform(0.5, &a).1;
    assert!((r[1] - 8192).abs() <= 2, "{r:?}");
}

#[test]
fn rotation_modes_and_inactive_tracks() {
    let mut t = move_track(MoveFrame::World);
    let a = NoGroupActors;
    let inst = MoveInstance::new(&t, [0.0; 3], [0, 0, 0], 0.0, &a);
    t.rot_mode = RotMode::Ignore;
    assert_eq!(
        t.sample(0.5, &inst, &a).unwrap().rotation,
        MoveRotation::Keep
    );
    t.rot_mode = RotMode::LookAtGroup;
    t.look_at_group = Some("Target".to_owned());
    assert_eq!(
        t.sample(0.5, &inst, &a).unwrap().rotation,
        MoveRotation::LookAt("Target".to_owned())
    );
    // No rotation keys and no sub-tracks: the engine leaves the actor alone.
    t.euler.points.clear();
    assert!(!t.is_active());
    assert!(t.sample(0.5, &inst, &a).is_none());
}

struct Actors;

impl matinee::GroupActors for Actors {
    fn location(&self, group: &str) -> Option<[f32; 3]> {
        (group == "Anchor").then_some([7.0, 8.0, 9.0])
    }
    fn rotation(&self, group: &str) -> Option<[i32; 3]> {
        (group == "Anchor").then_some([0, 16384, 0])
    }
}

#[test]
fn lookup_keys_take_the_named_actor() {
    let mut t = move_track(MoveFrame::World);
    t.lookup = vec![
        matinee::LookupKey {
            time: 0.0,
            group: None,
        },
        matinee::LookupKey {
            time: 1.0,
            group: Some("Anchor".to_owned()),
        },
    ];
    assert_eq!(t.eval_position(1.0, &Actors), [7.0, 8.0, 9.0]);
    assert_eq!(t.eval_position(0.5, &Actors), [3.5, 4.0, 4.5]);
    assert_eq!(t.eval_euler(1.0, &Actors), [0.0, 0.0, 90.0]);
    // Unresolved groups fall back to the stored keys.
    assert_eq!(t.eval_position(1.0, &NoGroupActors), [100.0, 0.0, 0.0]);
}

// ================================================================== playback

fn settings() -> InterpSettings {
    InterpSettings::default()
}

#[test]
fn playback_advances_finishes_and_loops() {
    let mut p = Playback::new(4.0, settings());
    // Not playing: nothing moves.
    assert_eq!(p.step(1.0).to, 0.0);
    assert!(p.play().is_none());
    let r = p.step(1.5);
    assert_eq!((r.from, r.to, r.finished), (0.0, 1.5, false));
    let r = p.step(3.0);
    assert_eq!((r.to, r.finished), (4.0, true));
    assert!(!p.playing);

    let mut s = settings();
    s.looping = true;
    s.play_rate = 2.0;
    let mut p = Playback::new(4.0, s);
    p.play();
    let r = p.step(4.5); // 9 seconds of sequence time: two wraps.
    assert!(r.wrapped && !r.finished);
    assert_eq!(r.to, 1.0);
    assert!(p.playing);

    // Reverse play wraps the other way, or stops at 0.
    p.play_reverse();
    let r = p.step(1.0);
    assert_eq!(r.to, 3.0);
    assert!(r.wrapped);
    let mut q = Playback::new(4.0, settings());
    q.position = 1.0;
    q.play_reverse();
    let r = q.step(2.0);
    assert_eq!((r.to, r.finished), (0.0, true));
}

#[test]
fn playback_inputs_follow_the_action_flags() {
    let mut s = settings();
    s.rewind_on_play = true;
    s.no_reset_on_rewind = true;
    let mut p = Playback::new(4.0, s.clone());
    p.position = 3.0;
    let j = p.play().unwrap();
    assert_eq!((j.to, j.reset_initial_transforms), (0.0, true));
    p.step(1.0);
    // Already playing: no rewind unless bRewindIfAlreadyPlaying.
    assert!(p.play().is_none());
    assert_eq!(p.position, 1.0);
    let mut s2 = s.clone();
    s2.rewind_if_already_playing = true;
    let mut p2 = Playback::new(4.0, s2);
    p2.play();
    p2.step(1.0);
    assert_eq!(p2.play().unwrap().to, 0.0);

    let mut f = settings();
    f.force_start_pos = true;
    f.force_start_position = 2.5;
    let mut p = Playback::new(4.0, f);
    assert_eq!(p.play().unwrap().to, 2.5);
    assert_eq!(p.position, 2.5);

    let mut p = Playback::new(4.0, settings());
    p.play();
    p.pause();
    assert_eq!(p.step(1.0).to, 0.0);
    p.pause();
    assert_eq!(p.step(1.0).to, 1.0);
    p.change_direction();
    assert!(p.reverse && p.playing);
    p.stop();
    assert!(!p.playing && !p.paused);
    assert_eq!(p.position, 1.0);

    // Looping with bNoResetOnRewind flags the wrap.
    let mut l = settings();
    l.looping = true;
    l.no_reset_on_rewind = true;
    let mut p = Playback::new(1.0, l);
    p.play();
    assert!(p.step(1.5).reset_initial_transforms);
    // A zero-length looping sequence cannot spin forever.
    let mut z = settings();
    z.looping = true;
    let mut p = Playback::new(0.0, z);
    p.play();
    assert!(p.step(1.0).finished);
}

// ================================================================== value decoding

fn fprop(name: &str, value: Value) -> Property {
    Property {
        name: name.to_owned(),
        type_name: String::new(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    }
}

fn vstruct(name: &str, fields: Vec<Property>) -> Value {
    Value::Struct {
        name: name.to_owned(),
        binary: false,
        fields,
    }
}

fn vvec(x: f32, y: f32, z: f32) -> Value {
    Value::Struct {
        name: "Vector".to_owned(),
        binary: true,
        fields: vec![
            fprop("X", Value::Float(x)),
            fprop("Y", Value::Float(y)),
            fprop("Z", Value::Float(z)),
        ],
    }
}

#[test]
fn curves_decode_with_struct_defaults() {
    let curve = vstruct(
        "InterpCurveVector",
        vec![fprop(
            "Points",
            Value::Array(vec![
                vstruct(
                    "InterpCurvePointVector",
                    vec![
                        fprop("InVal", Value::Float(1.0)),
                        fprop("OutVal", vvec(1.0, 2.0, 3.0)),
                        fprop("InterpMode", Value::Enum("CIM_CurveUser".to_owned())),
                    ],
                ),
                // Mode stored as a plain byte; tangents missing.
                vstruct(
                    "InterpCurvePointVector",
                    vec![
                        fprop("InVal", Value::Float(2.0)),
                        fprop("InterpMode", Value::Byte(2)),
                    ],
                ),
                // Unknown mode name: cubic, with a warning.
                vstruct(
                    "InterpCurvePointVector",
                    vec![fprop("InterpMode", Value::Enum("CIM_Bogus".to_owned()))],
                ),
                Value::Int(3),
            ]),
        )],
    );
    let mut w = Vec::new();
    let c = decode_curve(
        Some(&curve),
        |v| match v {
            Value::Struct { fields, .. } => {
                let g = |n: &str| {
                    fields
                        .iter()
                        .find(|p| p.name == n)
                        .and_then(|p| match p.value {
                            Value::Float(f) => Some(f),
                            _ => None,
                        })
                        .unwrap_or(0.0)
                };
                Some([g("X"), g("Y"), g("Z")])
            }
            _ => None,
        },
        &mut w,
        "PosTrack",
    );
    assert_eq!(c.points.len(), 3);
    assert_eq!(c.points[0].out_val, [1.0, 2.0, 3.0]);
    assert_eq!(c.points[0].mode, CurveMode::CurveUser);
    assert_eq!(c.points[1].mode, CurveMode::Constant);
    assert_eq!(c.points[1].arrive_tangent, [0.0; 3]);
    assert_eq!(c.points[2].mode, CurveMode::CurveUser);
    assert_eq!(c.method, InterpMethod::FixedTangentEvalAndNewAutoTangents);
    assert_eq!(w.len(), 2, "{w:?}");
    // Absent curve: empty.
    assert!(
        decode_curve::<f32>(None, |_| None, &mut w, "x")
            .points
            .is_empty()
    );
}

fn chain(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn tracks_decode_by_class_chain() {
    let fl = |t: f32, v: f32| {
        vstruct(
            "InterpCurvePointFloat",
            vec![
                fprop("InVal", Value::Float(t)),
                fprop("OutVal", Value::Float(v)),
                fprop("InterpMode", Value::Enum("CIM_Linear".to_owned())),
            ],
        )
    };
    let float_track = vstruct(
        "InterpCurveFloat",
        vec![fprop(
            "Points",
            Value::Array(vec![fl(0.0, 0.0), fl(2.0, 1.0)]),
        )],
    );
    let t = decode_track(
        "P.Fade",
        "Engine.InterpTrackFade",
        &chain(&[
            "interptrackfade",
            "interptrackfloatbase",
            "interptrack",
            "object",
        ]),
        &[
            fprop("FloatTrack", float_track.clone()),
            fprop("bPersistFade", Value::Bool(true)),
            fprop("bDisableTrack", Value::Bool(true)),
            fprop(
                "ActiveCondition",
                Value::Enum("ETAC_GoreEnabled".to_owned()),
            ),
        ],
    );
    assert!(t.disabled);
    assert_eq!(t.active_condition.as_deref(), Some("ETAC_GoreEnabled"));
    match &t.data {
        TrackData::Fade(f) => {
            assert!(f.persist_fade);
            assert_eq!(f.curve.eval(1.0, 0.0), 0.5);
        }
        other => panic!("{other:?}"),
    }
    // A custom subclass of a float track base decodes as its base.
    let t = decode_track(
        "P.X",
        "asamu.MyTrack",
        &chain(&["mytrack", "interptrackfloatbase", "interptrack", "object"]),
        &[fprop("FloatTrack", float_track)],
    );
    assert!(matches!(t.data, TrackData::FloatBase(_)));
    // Events with keys, flags from the properties given.
    let t = decode_track(
        "P.E",
        "Engine.InterpTrackEvent",
        &chain(&["interptrackevent", "interptrack", "object"]),
        &[
            fprop(
                "EventTrack",
                Value::Array(vec![vstruct(
                    "EventTrackKey",
                    vec![
                        fprop("Time", Value::Float(1.5)),
                        fprop("EventName", Value::Name("Boom".to_owned())),
                    ],
                )]),
            ),
            fprop("bFireEventsWhenForwards", Value::Bool(true)),
        ],
    );
    match &t.data {
        TrackData::Event(e) => {
            assert_eq!(e.keys.len(), 1);
            assert_eq!(e.keys[0].name, "Boom");
            assert!(e.fire_forwards && !e.fire_backwards);
        }
        other => panic!("{other:?}"),
    }
    // Toggle actions stored as bytes resolve to enumerator names.
    let t = decode_track(
        "P.T",
        "Engine.InterpTrackToggle",
        &chain(&["interptracktoggle", "interptrack", "object"]),
        &[fprop(
            "ToggleTrack",
            Value::Array(vec![vstruct(
                "ToggleTrackKey",
                vec![
                    fprop("Time", Value::Float(0.5)),
                    fprop("ToggleAction", Value::Byte(3)),
                ],
            )]),
        )],
    );
    match &t.data {
        TrackData::Toggle(k) => assert_eq!(k.keys[0].action, "ETTA_Trigger"),
        other => panic!("{other:?}"),
    }
    // Sound keys default volume and pitch to 1.
    let t = decode_track(
        "P.S",
        "Engine.InterpTrackSound",
        &chain(&[
            "interptracksound",
            "interptrackvectorbase",
            "interptrack",
            "object",
        ]),
        &[fprop(
            "Sounds",
            Value::Array(vec![vstruct(
                "SoundTrackKey",
                vec![
                    fprop("Time", Value::Float(2.0)),
                    fprop(
                        "Sound",
                        Value::Object(ObjRef {
                            index: -3,
                            path: "Pkg.Cue".to_owned(),
                        }),
                    ),
                ],
            )]),
        )],
    );
    match &t.data {
        TrackData::Sound(s) => {
            assert_eq!((s.keys[0].volume, s.keys[0].pitch), (1.0, 1.0));
            assert_eq!(s.keys[0].sound.as_deref(), Some("Pkg.Cue"));
        }
        other => panic!("{other:?}"),
    }
    // Move track: enum values, defaults for what is not stored.
    let t = decode_track(
        "P.M",
        "Engine.InterpTrackMove",
        &chain(&["interptrackmove", "interptrack", "object"]),
        &[
            fprop("MoveFrame", Value::Enum("IMF_RelativeToInitial".to_owned())),
            fprop("RotMode", Value::Byte(2)),
            fprop("bUseQuatInterpolation", Value::Bool(true)),
        ],
    );
    match &t.data {
        TrackData::Move(m) => {
            assert_eq!(m.move_frame, MoveFrame::RelativeToInitial);
            assert_eq!(m.rot_mode, RotMode::Ignore);
            assert!(m.use_quat_interpolation);
            assert!(m.pos.points.is_empty() && !m.is_active());
        }
        other => panic!("{other:?}"),
    }
    // Unknown classes keep their property names.
    let t = decode_track(
        "P.U",
        "Mod.Weird",
        &chain(&["weird", "object"]),
        &[fprop("Foo", Value::Int(1))],
    );
    match &t.data {
        TrackData::Unknown(u) => assert_eq!(u.properties, vec!["Foo".to_owned()]),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.data.kind_name(), "unknown");
}

// ================================================================== synthetic map

#[derive(Clone, Debug)]
enum V {
    Int(i32),
    Float(f32),
    Bool(bool),
    Str(&'static str),
    Name(&'static str),
    /// Enum-typed byte: (enum name, enumerator).
    Enum(&'static str, &'static str),
    /// Package index (export i => i + 1, import j => -(j + 1)).
    Obj(i32),
    Arr(Vec<V>),
    /// Tagged struct.
    Struct(&'static str, Vec<(&'static str, V)>),
    /// Binary `Vector`.
    Vec3(f32, f32, f32),
    /// Binary `Rotator`.
    Rot(i32, i32, i32),
}

#[derive(Default)]
struct Names(Vec<String>);

impl Names {
    fn idx(&mut self, s: &str) -> i32 {
        if let Some(i) = self.0.iter().position(|x| x == s) {
            return i as i32;
        }
        self.0.push(s.to_owned());
        (self.0.len() - 1) as i32
    }
    fn fname(&mut self, w: &mut W, s: &str) {
        let i = self.idx(s);
        w.i32(i);
        w.i32(0);
    }
}

fn type_name(v: &V) -> &'static str {
    match v {
        V::Int(_) => "IntProperty",
        V::Float(_) => "FloatProperty",
        V::Bool(_) => "BoolProperty",
        V::Str(_) => "StrProperty",
        V::Name(_) => "NameProperty",
        V::Enum(..) => "ByteProperty",
        V::Obj(_) => "ObjectProperty",
        V::Arr(_) => "ArrayProperty",
        V::Struct(..) | V::Vec3(..) | V::Rot(..) => "StructProperty",
    }
}

fn item(n: &mut Names, w: &mut W, v: &V) {
    match v {
        V::Int(i) => w.i32(*i),
        V::Float(f) => w.bytes(&f.to_le_bytes()),
        V::Bool(b) => w.bytes(&[u8::from(*b)]),
        V::Str(s) => w.fstring(s),
        V::Name(s) => n.fname(w, s),
        V::Enum(_, e) => n.fname(w, e),
        V::Obj(i) => w.i32(*i),
        V::Arr(items) => {
            w.i32(items.len() as i32);
            for i in items {
                item(n, w, i);
            }
        }
        V::Struct(_, fields) => tagged(n, w, fields),
        V::Vec3(x, y, z) => {
            for c in [x, y, z] {
                w.bytes(&c.to_le_bytes());
            }
        }
        V::Rot(p, y, r) => {
            for c in [p, y, r] {
                w.i32(*c);
            }
        }
    }
}

fn tag(n: &mut Names, w: &mut W, name: &str, v: &V) {
    let mut body = W::default();
    if !matches!(v, V::Bool(_)) {
        item(n, &mut body, v);
    }
    n.fname(w, name);
    n.fname(w, type_name(v));
    w.i32(body.len() as i32);
    w.i32(0);
    match v {
        V::Struct(s, _) => n.fname(w, s),
        V::Vec3(..) => n.fname(w, "Vector"),
        V::Rot(..) => n.fname(w, "Rotator"),
        V::Bool(b) => w.bytes(&[u8::from(*b)]),
        V::Enum(e, _) => n.fname(w, e),
        _ => {}
    }
    w.bytes(&body.0);
}

fn tagged(n: &mut Names, w: &mut W, fields: &[(&str, V)]) {
    for (name, v) in fields {
        tag(n, w, name, v);
    }
    n.fname(w, "None");
}

/// Package index of 0-based export `i`.
fn e(i: usize) -> i32 {
    i as i32 + 1
}

#[derive(Clone)]
struct ExportSpec {
    class: &'static str,
    outer: i32,
    name: &'static str,
    archetype: i32,
    props: Vec<(&'static str, V)>,
}

fn x(
    class: &'static str,
    outer: usize,
    name: &'static str,
    props: Vec<(&'static str, V)>,
) -> ExportSpec {
    ExportSpec {
        class,
        outer: if outer == TOP { 0 } else { e(outer) },
        name,
        archetype: 0,
        props,
    }
}

const TOP: usize = usize::MAX;

fn input(desc: &'static str) -> V {
    V::Struct("SeqOpInputLink", vec![("LinkDesc", V::Str(desc))])
}

fn output(desc: &'static str) -> V {
    V::Struct(
        "SeqOpOutputLink",
        vec![("Links", V::Arr(vec![])), ("LinkDesc", V::Str(desc))],
    )
}

fn var_link(desc: &'static str, prop: &'static str, vars: &[usize]) -> V {
    V::Struct(
        "SeqVarLink",
        vec![
            (
                "LinkedVariables",
                V::Arr(vars.iter().map(|&i| V::Obj(e(i))).collect()),
            ),
            ("LinkDesc", V::Str(desc)),
            ("PropertyName", V::Name(prop)),
        ],
    )
}

fn parent(i: usize) -> (&'static str, V) {
    ("ParentSequence", V::Obj(e(i)))
}

fn objs(list: &[usize]) -> V {
    V::Arr(list.iter().map(|&i| V::Obj(e(i))).collect())
}

fn vpoint(t: f32, v: (f32, f32, f32), mode: &'static str) -> V {
    V::Struct(
        "InterpCurvePointVector",
        vec![
            ("InVal", V::Float(t)),
            ("OutVal", V::Vec3(v.0, v.1, v.2)),
            ("ArriveTangent", V::Vec3(0.0, 0.0, 0.0)),
            ("LeaveTangent", V::Vec3(0.0, 0.0, 0.0)),
            ("InterpMode", V::Enum("EInterpCurveMode", mode)),
        ],
    )
}

fn fpoint(t: f32, v: f32, mode: &'static str) -> V {
    V::Struct(
        "InterpCurvePointFloat",
        vec![
            ("InVal", V::Float(t)),
            ("OutVal", V::Float(v)),
            ("ArriveTangent", V::Float(0.0)),
            ("LeaveTangent", V::Float(0.0)),
            ("InterpMode", V::Enum("EInterpCurveMode", mode)),
        ],
    )
}

fn vcurve(points: Vec<V>) -> V {
    V::Struct("InterpCurveVector", vec![("Points", V::Arr(points))])
}

fn fcurve_v(points: Vec<V>) -> V {
    V::Struct("InterpCurveFloat", vec![("Points", V::Arr(points))])
}

mod ex {
    pub const WORLD: usize = 0;
    pub const LEVEL: usize = 1;
    pub const MAIN: usize = 2;
    pub const INTERP: usize = 3;
    pub const DATA: usize = 4;
    pub const VAR_OBJ: usize = 5;
    pub const VAR_FLOAT: usize = 6;
    pub const GROUP: usize = 7;
    pub const DIRECTOR: usize = 8;
    pub const MOVE: usize = 9;
    pub const EVENT: usize = 10;
    pub const ACTOR: usize = 11;
    pub const NAMED: usize = 12;
    pub const VAR_TARGET: usize = 13;
    pub const GROUP2: usize = 14;
    pub const DIR_TRACK: usize = 15;
    pub const FADE: usize = 16;
    pub const MOVE2: usize = 17;
    pub const AXIS0: usize = 18;
    // 18..=23 axis sub-tracks
    pub const ORPHAN: usize = 24;
    pub const PREFAB_PKG: usize = 25;
    pub const PREFAB: usize = 26;
    pub const ARCH_SEQ: usize = 27;
    pub const ARCH_INTERP: usize = 28;
    pub const ARCH_DATA: usize = 29;
    pub const ARCH_GROUP: usize = 30;
    pub const ARCH_TRACK: usize = 31;
    pub const INST_SEQ: usize = 32;
    pub const INST_INTERP: usize = 33;
    pub const INST_DATA: usize = 34;
    pub const INST_GROUP: usize = 35;
    pub const INST_TRACK: usize = 36;
    pub const CAM_ANIM: usize = 37;
    pub const CAM_GROUP: usize = 38;
    pub const CAM_TRACK: usize = 39;
}

fn spec() -> Vec<ExportSpec> {
    use ex::*;
    let axis_names = [
        "AXIS_TranslationX",
        "AXIS_TranslationY",
        "AXIS_TranslationZ",
        "AXIS_RotationX",
        "AXIS_RotationY",
        "AXIS_RotationZ",
    ];
    let axis_names_static: Vec<&'static str> = axis_names.to_vec();
    let mut v = vec![
        x("Engine.World", TOP, "TheWorld", vec![]),
        x("Engine.Level", WORLD, "PersistentLevel", vec![]),
        x(
            "Engine.Sequence",
            LEVEL,
            "Main_Sequence",
            vec![(
                "SequenceObjects",
                objs(&[
                    INTERP, DATA, VAR_OBJ, VAR_FLOAT, NAMED, VAR_TARGET, INST_SEQ,
                ]),
            )],
        ),
        x(
            "Engine.SeqAct_Interp",
            MAIN,
            "SeqAct_Interp_0",
            vec![
                (
                    "InputLinks",
                    V::Arr(vec![
                        input("Play"),
                        input("Reverse"),
                        input("Stop"),
                        input("Pause"),
                        input("Change Dir"),
                    ]),
                ),
                (
                    "OutputLinks",
                    V::Arr(vec![
                        output("Completed"),
                        output("Reversed"),
                        output("Boom"),
                    ]),
                ),
                (
                    "VariableLinks",
                    V::Arr(vec![
                        var_link("Data", "None", &[DATA]),
                        var_link("Mover", "None", &[VAR_OBJ]),
                        var_link("PlayRate", "PlayRate", &[VAR_FLOAT]),
                        var_link("Named", "None", &[NAMED]),
                        var_link("Nobody", "None", &[]),
                    ]),
                ),
                ("PlayRate", V::Float(0.5)),
                ("bLooping", V::Bool(true)),
                ("ObjComment", V::Str("lift")),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.InterpData",
            MAIN,
            "InterpData_0",
            vec![
                ("InterpLength", V::Float(4.0)),
                ("InterpGroups", objs(&[GROUP, DIRECTOR, GROUP2])),
                ("EdSectionEnd", V::Float(4.0)),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.SeqVar_Object",
            MAIN,
            "SeqVar_Object_0",
            vec![("ObjValue", V::Obj(e(ACTOR))), parent(MAIN)],
        ),
        x(
            "Engine.SeqVar_Float",
            MAIN,
            "SeqVar_Float_0",
            vec![("FloatValue", V::Float(0.25)), parent(MAIN)],
        ),
        x(
            "Engine.InterpGroup",
            DATA,
            "InterpGroup_0",
            vec![
                ("InterpTracks", objs(&[MOVE, EVENT])),
                ("GroupName", V::Name("Mover")),
                (
                    "GroupColor",
                    V::Struct(
                        "Color",
                        vec![
                            ("B", V::Int(1)),
                            ("G", V::Int(2)),
                            ("R", V::Int(3)),
                            ("A", V::Int(255)),
                        ],
                    ),
                ),
            ],
        ),
        x(
            "Engine.InterpGroupDirector",
            DATA,
            "InterpGroupDirector_0",
            vec![("InterpTracks", objs(&[DIR_TRACK, FADE]))],
        ),
        x(
            "Engine.InterpTrackMove",
            GROUP,
            "InterpTrackMove_0",
            vec![
                (
                    "PosTrack",
                    vcurve(vec![
                        vpoint(0.0, (0.0, 0.0, 0.0), "CIM_Linear"),
                        vpoint(4.0, (400.0, 0.0, 0.0), "CIM_Linear"),
                    ]),
                ),
                (
                    "EulerTrack",
                    vcurve(vec![
                        vpoint(0.0, (0.0, 0.0, 0.0), "CIM_Linear"),
                        vpoint(4.0, (0.0, 0.0, 90.0), "CIM_Linear"),
                    ]),
                ),
                (
                    "LookupTrack",
                    V::Struct(
                        "InterpLookupTrack",
                        vec![(
                            "Points",
                            V::Arr(vec![
                                V::Struct(
                                    "InterpLookupPoint",
                                    vec![("GroupName", V::Name("None")), ("Time", V::Float(0.0))],
                                ),
                                V::Struct(
                                    "InterpLookupPoint",
                                    vec![("GroupName", V::Name("None")), ("Time", V::Float(4.0))],
                                ),
                            ]),
                        )],
                    ),
                ),
                (
                    "MoveFrame",
                    V::Enum("EInterpTrackMoveFrame", "IMF_RelativeToInitial"),
                ),
            ],
        ),
        x(
            "Engine.InterpTrackEvent",
            GROUP,
            "InterpTrackEvent_0",
            vec![(
                "EventTrack",
                V::Arr(vec![V::Struct(
                    "EventTrackKey",
                    vec![("Time", V::Float(1.5)), ("EventName", V::Name("Boom"))],
                )]),
            )],
        ),
        x(
            "Engine.InterpActor",
            LEVEL,
            "InterpActor_0",
            vec![
                ("Location", V::Vec3(100.0, 200.0, 300.0)),
                ("Rotation", V::Rot(0, 16384, 0)),
            ],
        ),
        x(
            "Engine.SeqVar_Named",
            MAIN,
            "SeqVar_Named_0",
            vec![("FindVarName", V::Name("Target")), parent(MAIN)],
        ),
        x(
            "Engine.SeqVar_Object",
            MAIN,
            "SeqVar_Object_1",
            vec![
                ("ObjValue", V::Obj(e(ACTOR))),
                ("VarName", V::Name("Target")),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.InterpGroup",
            DATA,
            "InterpGroup_1",
            vec![
                ("InterpTracks", objs(&[MOVE2])),
                ("GroupName", V::Name("Named")),
            ],
        ),
        x(
            "Engine.InterpTrackDirector",
            DIRECTOR,
            "InterpTrackDirector_0",
            vec![(
                "CutTrack",
                V::Arr(vec![V::Struct(
                    "DirectorTrackCut",
                    vec![
                        ("Time", V::Float(0.5)),
                        ("TargetCamGroup", V::Name("Mover")),
                        ("ShotNumber", V::Int(10)),
                    ],
                )]),
            )],
        ),
        x(
            "Engine.InterpTrackFade",
            DIRECTOR,
            "InterpTrackFade_0",
            vec![
                (
                    "FloatTrack",
                    fcurve_v(vec![
                        fpoint(0.0, 0.0, "CIM_CurveAutoClamped"),
                        fpoint(1.0, 1.0, "CIM_CurveAutoClamped"),
                    ]),
                ),
                ("bPersistFade", V::Bool(true)),
            ],
        ),
        x(
            "Engine.InterpTrackMove",
            GROUP2,
            "InterpTrackMove_1",
            vec![(
                "SubTracks",
                objs(&[AXIS0, AXIS0 + 1, AXIS0 + 2, AXIS0 + 3, AXIS0 + 4, AXIS0 + 5]),
            )],
        ),
    ];
    for (k, axis) in axis_names_static.into_iter().enumerate() {
        let name: &'static str = Box::leak(format!("InterpTrackMoveAxis_{k}").into_boxed_str());
        let value = (k as f32 + 1.0) * 10.0;
        v.push(x(
            "Engine.InterpTrackMoveAxis",
            MOVE2,
            name,
            vec![
                ("MoveAxis", V::Enum("EInterpMoveAxis", axis)),
                (
                    "FloatTrack",
                    fcurve_v(vec![fpoint(0.0, value, "CIM_Constant")]),
                ),
            ],
        ));
    }
    v.extend([
        x(
            "Engine.InterpTrackToggle",
            GROUP2,
            "InterpTrackToggle_9",
            vec![],
        ),
        x("Core.Package", TOP, "PrefabPkg", vec![]),
        x("Engine.Prefab", PREFAB_PKG, "MyPrefab", vec![]),
        x(
            "Engine.PrefabSequence",
            PREFAB,
            "PrefabSequence_0",
            vec![("SequenceObjects", objs(&[ARCH_INTERP, ARCH_DATA]))],
        ),
        x(
            "Engine.SeqAct_Interp",
            ARCH_SEQ,
            "SeqAct_Interp_9",
            vec![
                ("InputLinks", V::Arr(vec![input("Play")])),
                (
                    "VariableLinks",
                    V::Arr(vec![
                        var_link("Data", "None", &[ARCH_DATA]),
                        var_link("Arch", "None", &[]),
                    ]),
                ),
                ("bRewindOnPlay", V::Bool(true)),
                parent(ARCH_SEQ),
            ],
        ),
        x(
            "Engine.InterpData",
            ARCH_SEQ,
            "InterpData_9",
            vec![
                ("InterpLength", V::Float(2.0)),
                ("InterpGroups", objs(&[ARCH_GROUP])),
                parent(ARCH_SEQ),
            ],
        ),
        x(
            "Engine.InterpGroup",
            ARCH_DATA,
            "InterpGroup_9",
            vec![
                ("InterpTracks", objs(&[ARCH_TRACK])),
                ("GroupName", V::Name("Arch")),
            ],
        ),
        x(
            "Engine.InterpTrackFloatProp",
            ARCH_GROUP,
            "InterpTrackFloatProp_9",
            vec![
                ("PropertyName", V::Name("Brightness")),
                (
                    "FloatTrack",
                    fcurve_v(vec![
                        fpoint(0.0, 1.0, "CIM_Linear"),
                        fpoint(2.0, 3.0, "CIM_Linear"),
                    ]),
                ),
            ],
        ),
        ExportSpec {
            archetype: e(ARCH_SEQ),
            ..x(
                "Engine.PrefabSequence",
                MAIN,
                "Inst_Seq",
                vec![
                    ("SequenceObjects", objs(&[INST_INTERP, INST_DATA])),
                    parent(MAIN),
                ],
            )
        },
        ExportSpec {
            archetype: e(ARCH_INTERP),
            ..x(
                "Engine.SeqAct_Interp",
                INST_SEQ,
                "SeqAct_Interp_1",
                vec![parent(INST_SEQ)],
            )
        },
        ExportSpec {
            archetype: e(ARCH_DATA),
            ..x(
                "Engine.InterpData",
                INST_SEQ,
                "InterpData_1",
                vec![parent(INST_SEQ)],
            )
        },
        ExportSpec {
            archetype: e(ARCH_GROUP),
            ..x("Engine.InterpGroup", INST_DATA, "InterpGroup_9", vec![])
        },
        ExportSpec {
            archetype: e(ARCH_TRACK),
            ..x(
                "Engine.InterpTrackFloatProp",
                INST_GROUP,
                "InterpTrackFloatProp_9",
                vec![],
            )
        },
        x(
            "Engine.CameraAnim",
            PREFAB_PKG,
            "CamAnim",
            vec![
                ("CameraInterpGroup", V::Obj(e(CAM_GROUP))),
                ("AnimLength", V::Float(1.5)),
            ],
        ),
        x(
            "Engine.InterpGroupCamera",
            CAM_ANIM,
            "InterpGroupCamera_0",
            vec![("InterpTracks", objs(&[CAM_TRACK]))],
        ),
        x(
            "Engine.InterpTrackMove",
            CAM_GROUP,
            "InterpTrackMove_7",
            vec![(
                "PosTrack",
                vcurve(vec![vpoint(0.0, (1.0, 2.0, 3.0), "CIM_CurveUser")]),
            )],
        ),
    ]);
    assert_eq!(v.len(), CAM_TRACK + 1);
    assert_eq!(v[ORPHAN].class, "Engine.InterpTrackToggle");
    v
}

fn build_bytes(spec: &[ExportSpec]) -> Vec<u8> {
    let mut n = Names::default();
    for s in ["None", "Core", "Package", "Class"] {
        n.idx(s);
    }
    let mut imports: Vec<Import> = Vec::new();
    let mut packages: HashMap<String, i32> = HashMap::new();
    let mut classes: HashMap<String, i32> = HashMap::new();
    let mut import_of = |n: &mut Names, imports: &mut Vec<Import>, class: &str| -> i32 {
        if let Some(&i) = classes.get(class) {
            return i;
        }
        let (pkg, cls) = class.split_once('.').unwrap();
        let pkg_idx = match packages.get(pkg) {
            Some(&p) => p,
            None => {
                imports.push(Import {
                    class_package: n.idx("Core"),
                    class_name: n.idx("Package"),
                    outer: 0,
                    name: n.idx(pkg),
                    number: 0,
                });
                let p = -(imports.len() as i32);
                packages.insert(pkg.to_owned(), p);
                p
            }
        };
        imports.push(Import {
            class_package: n.idx("Core"),
            class_name: n.idx("Class"),
            outer: pkg_idx,
            name: n.idx(cls),
            number: 0,
        });
        let i = -(imports.len() as i32);
        classes.insert(class.to_owned(), i);
        i
    };
    let mut exports = Vec::new();
    for (i, s) in spec.iter().enumerate() {
        let class = import_of(&mut n, &mut imports, s.class);
        let mut w = W::default();
        w.i32(i as i32); // NetIndex
        tagged(&mut n, &mut w, &s.props);
        let name = n.idx(s.name);
        exports.push(Export {
            class,
            super_: 0,
            outer: s.outer,
            name,
            number: 0,
            archetype: s.archetype,
            object_flags: 0x0007_0004_0000_0000,
            payload: w.0,
            export_flags: 0,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
    }
    let mut synth = Synth::sample();
    synth.names = n.0.iter().map(|s| (s.clone(), 0u64)).collect();
    synth.imports = imports;
    synth.exports = exports;
    synth.package_flags = 0x0002_0008;
    synth.texture_allocations = Vec::new();
    synth.additional_packages = Vec::new();
    synth.build().0
}

// ------------------------------------------------------------------ schema

struct TestSchema {
    defs: HashMap<String, Arc<PropertyDef>>,
    structs: HashMap<String, Arc<StructDef>>,
    interp_settings: Vec<Arc<PropertyDef>>,
}

fn def(owner: &str, name: &str, ty: PropertyType) -> PropertyDef {
    PropertyDef {
        name: name.to_owned(),
        path: format!("{owner}.{name}"),
        array_dim: 1,
        flags: 0,
        category: "None".to_owned(),
        array_enum: None,
        rep_offset: None,
        ty,
    }
}

fn array_of(owner: &str, name: &str, inner: PropertyType) -> PropertyType {
    PropertyType::Array {
        inner: Box::new(def(owner, name, inner)),
    }
}

fn obj(class: &str) -> PropertyType {
    PropertyType::Object {
        class: class.to_owned(),
    }
}

fn strukt(path: &str) -> PropertyType {
    PropertyType::Struct {
        struct_path: path.to_owned(),
    }
}

fn byte_enum(path: &str) -> PropertyType {
    PropertyType::Byte {
        enum_path: Some(path.to_owned()),
    }
}

const OP: &str = "Engine.SequenceOp";
const CORE: &str = "Core.Object";

impl TestSchema {
    fn new() -> TestSchema {
        let class = PropertyType::Class {
            class: "Core.Class".into(),
            meta_class: "Engine.SequenceObject".into(),
        };
        let all = vec![
            // Kismet.
            def(
                OP,
                "InputLinks",
                array_of(OP, "InputLinks", strukt("Engine.SequenceOp.SeqOpInputLink")),
            ),
            def(
                OP,
                "OutputLinks",
                array_of(
                    OP,
                    "OutputLinks",
                    strukt("Engine.SequenceOp.SeqOpOutputLink"),
                ),
            ),
            def(
                OP,
                "VariableLinks",
                array_of(OP, "VariableLinks", strukt("Engine.SequenceOp.SeqVarLink")),
            ),
            def(
                "Engine.Sequence",
                "SequenceObjects",
                array_of(
                    "Engine.Sequence",
                    "SequenceObjects",
                    obj("Engine.SequenceObject"),
                ),
            ),
            def(
                "Engine.SequenceObject",
                "ParentSequence",
                obj("Engine.Sequence"),
            ),
            def("Engine.SequenceObject", "ObjComment", PropertyType::Str),
            def("Engine.SequenceVariable", "VarName", PropertyType::Name),
            def(OP, "LinkDesc", PropertyType::Str),
            def(OP, "ActivateDelay", PropertyType::Float),
            def(OP, "bDisabled", PropertyType::Bool),
            def(OP, "LinkedOp", obj("Engine.SequenceOp")),
            def(OP, "InputLinkIdx", PropertyType::Int),
            def(
                OP,
                "Links",
                array_of(
                    OP,
                    "Links",
                    strukt("Engine.SequenceOp.SeqOpOutputInputLink"),
                ),
            ),
            def(OP, "ExpectedType", class),
            def(
                OP,
                "LinkedVariables",
                array_of(OP, "LinkedVariables", obj("Engine.SequenceVariable")),
            ),
            def(OP, "PropertyName", PropertyType::Name),
            def("Engine.SeqVar_Object", "ObjValue", obj("Core.Object")),
            def("Engine.SeqVar_Float", "FloatValue", PropertyType::Float),
            def("Engine.SeqVar_Named", "FindVarName", PropertyType::Name),
            // Matinee.
            def("Engine.InterpData", "InterpLength", PropertyType::Float),
            def("Engine.InterpData", "EdSectionEnd", PropertyType::Float),
            def(
                "Engine.InterpData",
                "InterpGroups",
                array_of(
                    "Engine.InterpData",
                    "InterpGroups",
                    obj("Engine.InterpGroup"),
                ),
            ),
            def(
                "Engine.InterpGroup",
                "InterpTracks",
                array_of(
                    "Engine.InterpGroup",
                    "InterpTracks",
                    obj("Engine.InterpTrack"),
                ),
            ),
            def("Engine.InterpGroup", "GroupName", PropertyType::Name),
            def(
                "Engine.InterpGroup",
                "GroupColor",
                strukt("Core.Object.Color"),
            ),
            def(
                "Engine.InterpTrack",
                "SubTracks",
                array_of("Engine.InterpTrack", "SubTracks", obj("Engine.InterpTrack")),
            ),
            def(
                "Engine.InterpTrackMove",
                "PosTrack",
                strukt("Core.Object.InterpCurveVector"),
            ),
            def(
                "Engine.InterpTrackMove",
                "EulerTrack",
                strukt("Core.Object.InterpCurveVector"),
            ),
            def(
                "Engine.InterpTrackMove",
                "LookupTrack",
                strukt("Engine.InterpTrackMove.InterpLookupTrack"),
            ),
            def(
                "Engine.InterpTrackMove",
                "MoveFrame",
                byte_enum("Engine.InterpTrackMove.EInterpTrackMoveFrame"),
            ),
            def(
                "Engine.InterpTrackMoveAxis",
                "MoveAxis",
                byte_enum("Engine.InterpTrackMoveAxis.EInterpMoveAxis"),
            ),
            def(
                "Engine.InterpTrackFloatBase",
                "FloatTrack",
                strukt("Core.Object.InterpCurveFloat"),
            ),
            def("Engine.InterpTrackFade", "bPersistFade", PropertyType::Bool),
            def(
                "Engine.InterpTrackEvent",
                "EventTrack",
                array_of(
                    "Engine.InterpTrackEvent",
                    "EventTrack",
                    strukt("Engine.InterpTrackEvent.EventTrackKey"),
                ),
            ),
            def(
                "Engine.InterpTrackDirector",
                "CutTrack",
                array_of(
                    "Engine.InterpTrackDirector",
                    "CutTrack",
                    strukt("Engine.InterpTrackDirector.DirectorTrackCut"),
                ),
            ),
            def(
                "Engine.CameraAnim",
                "CameraInterpGroup",
                obj("Engine.InterpGroupCamera"),
            ),
            def("Engine.CameraAnim", "AnimLength", PropertyType::Float),
            def("Engine.Actor", "Location", strukt("Core.Object.Vector")),
            def("Engine.Actor", "Rotation", strukt("Core.Object.Rotator")),
            // Struct members.
            def(CORE, "InVal", PropertyType::Float),
            def(
                CORE,
                "InterpMode",
                byte_enum("Core.Object.EInterpCurveMode"),
            ),
            def(CORE, "Time", PropertyType::Float),
            def(CORE, "EventName", PropertyType::Name),
            def(CORE, "TargetCamGroup", PropertyType::Name),
            def(CORE, "ShotNumber", PropertyType::Int),
            def(CORE, "B", PropertyType::Int),
            def(CORE, "G", PropertyType::Int),
            def(CORE, "R", PropertyType::Int),
            def(CORE, "A", PropertyType::Int),
        ];
        let settings_owner = "Engine.SeqAct_Interp";
        let interp_settings = vec![
            Arc::new(def(settings_owner, "PlayRate", PropertyType::Float)),
            Arc::new(def(settings_owner, "bLooping", PropertyType::Bool)),
            Arc::new(def(settings_owner, "bRewindOnPlay", PropertyType::Bool)),
            Arc::new(def(
                settings_owner,
                "ForceStartPosition",
                PropertyType::Float,
            )),
            Arc::new(def(settings_owner, "bForceStartPos", PropertyType::Bool)),
        ];
        let mut defs: HashMap<String, Arc<PropertyDef>> = HashMap::new();
        for d in all {
            defs.insert(d.name.to_ascii_lowercase(), Arc::new(d));
        }
        for d in &interp_settings {
            defs.insert(d.name.to_ascii_lowercase(), d.clone());
        }
        let mk = |path: &str| -> (String, Arc<StructDef>) {
            let name = path.rsplit('.').next().unwrap().to_owned();
            (
                path.to_ascii_lowercase(),
                Arc::new(StructDef {
                    path: path.to_owned(),
                    name,
                    kind: StructKind::ScriptStruct,
                    super_path: None,
                    struct_flags: 0,
                    properties: Vec::new(),
                }),
            )
        };
        let structs = [
            "Engine.SequenceOp.SeqOpInputLink",
            "Engine.SequenceOp.SeqOpOutputLink",
            "Engine.SequenceOp.SeqOpOutputInputLink",
            "Engine.SequenceOp.SeqVarLink",
            "Core.Object.InterpCurveVector",
            "Core.Object.InterpCurveFloat",
            "Core.Object.InterpCurvePointVector",
            "Core.Object.InterpCurvePointFloat",
            "Engine.InterpTrackMove.InterpLookupTrack",
            "Engine.InterpTrackMove.InterpLookupPoint",
            "Engine.InterpTrackEvent.EventTrackKey",
            "Engine.InterpTrackDirector.DirectorTrackCut",
            "Core.Object.Color",
        ]
        .into_iter()
        .map(mk)
        .collect();
        TestSchema {
            defs,
            structs,
            interp_settings,
        }
    }
}

fn chain_of(short: &str) -> Vec<&'static str> {
    let op = ["sequenceop", "sequenceobject", "object"];
    let var = ["sequencevariable", "sequenceobject", "object"];
    let with = |own: &[&'static str], tail: &[&'static str]| -> Vec<&'static str> {
        own.iter().chain(tail).copied().collect()
    };
    match short {
        "sequence" => with(&["sequence"], &op),
        "prefabsequence" => with(&["prefabsequence", "sequence"], &op),
        "seqact_interp" => with(&["seqact_interp", "seqact_latent", "sequenceaction"], &op),
        "interpdata" => with(&["interpdata"], &var),
        "seqvar_object" => with(&["seqvar_object"], &var),
        "seqvar_float" => with(&["seqvar_float"], &var),
        "seqvar_named" => with(&["seqvar_named"], &var),
        "interpgroup" => vec!["interpgroup", "object"],
        "interpgroupdirector" => vec!["interpgroupdirector", "interpgroup", "object"],
        "interpgroupcamera" => vec!["interpgroupcamera", "interpgroup", "object"],
        "interptrackmove" => vec!["interptrackmove", "interptrack", "object"],
        "interptrackmoveaxis" => vec![
            "interptrackmoveaxis",
            "interptrackfloatbase",
            "interptrack",
            "object",
        ],
        "interptrackevent" => vec!["interptrackevent", "interptrack", "object"],
        "interptrackdirector" => vec!["interptrackdirector", "interptrack", "object"],
        "interptracktoggle" => vec!["interptracktoggle", "interptrack", "object"],
        "interptrackfade" => vec![
            "interptrackfade",
            "interptrackfloatbase",
            "interptrack",
            "object",
        ],
        "interptrackfloatprop" => vec![
            "interptrackfloatprop",
            "interptrackfloatbase",
            "interptrack",
            "object",
        ],
        "cameraanim" => vec!["cameraanim", "object"],
        "interpactor" => vec!["interpactor", "dynamicsmactor", "actor", "object"],
        _ => Vec::new(),
    }
}

impl Schema for TestSchema {
    fn struct_def(&self, path: &str) -> Option<Arc<StructDef>> {
        self.structs.get(&path.to_ascii_lowercase()).cloned()
    }
    fn struct_by_name(&self, name: &str) -> Option<Arc<StructDef>> {
        self.structs
            .values()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
    }
    fn find_property(&self, owner: &str, name: &str) -> Option<Arc<PropertyDef>> {
        let o = owner.to_ascii_lowercase();
        let n = name.to_ascii_lowercase();
        let point = |p: &str| Some(Arc::new(def(owner, name, array_of(owner, name, strukt(p)))));
        if n == "points" {
            if o.ends_with("interpcurvevector") {
                return point("Core.Object.InterpCurvePointVector");
            }
            if o.ends_with("interpcurvefloat") {
                return point("Core.Object.InterpCurvePointFloat");
            }
            if o.ends_with("interplookuptrack") {
                return point("Engine.InterpTrackMove.InterpLookupPoint");
            }
        }
        if matches!(n.as_str(), "outval" | "arrivetangent" | "leavetangent") {
            if o.ends_with("interpcurvepointvector") {
                return Some(Arc::new(def(owner, name, strukt("Core.Object.Vector"))));
            }
            if o.ends_with("interpcurvepointfloat") {
                return Some(Arc::new(def(owner, name, PropertyType::Float)));
            }
        }
        if n == "groupname" && o.ends_with("interplookuppoint") {
            return Some(Arc::new(def(owner, name, PropertyType::Name)));
        }
        self.defs.get(&n).cloned()
    }
    fn property_link(&self, owner: &str) -> Vec<Arc<PropertyDef>> {
        if owner.eq_ignore_ascii_case("Engine.SeqAct_Interp") {
            return self.interp_settings.clone();
        }
        Vec::new()
    }
    fn enum_names(&self, _path: &str) -> Option<Arc<Vec<String>>> {
        None
    }
    fn class_chain(&self, class_path: &str) -> Vec<String> {
        let short = class_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let c = chain_of(&short);
        if c.is_empty() {
            return vec![short];
        }
        c.into_iter().map(str::to_owned).collect()
    }
}

/// Class defaults: `PlayRate` 1, event tracks fire forwards and backwards,
/// groups are named `InterpGroup`.
struct TestDefaults;

fn dprop(name: &str, type_name: &str, value: Value) -> Property {
    Property {
        name: name.into(),
        type_name: type_name.into(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    }
}

impl ClassDefaults for TestDefaults {
    fn class_defaults(&self, class_path: &str) -> Vec<Property> {
        match class_path.rsplit('.').next().unwrap_or("") {
            "SeqAct_Interp" => vec![dprop("PlayRate", "FloatProperty", Value::Float(1.0))],
            "InterpTrackEvent" => vec![
                dprop("bFireEventsWhenForwards", "BoolProperty", Value::Bool(true)),
                dprop(
                    "bFireEventsWhenBackwards",
                    "BoolProperty",
                    Value::Bool(true),
                ),
            ],
            "InterpGroup" | "InterpGroupDirector" | "InterpGroupCamera" => {
                vec![dprop(
                    "GroupName",
                    "NameProperty",
                    Value::Name("InterpGroup".into()),
                )]
            }
            _ => Vec::new(),
        }
    }
}

fn extract_spec(spec: &[ExportSpec]) -> MatineeMap {
    let pkg = Package::from_bytes(build_bytes(spec)).unwrap();
    let schema = TestSchema::new();
    let graph = build_graph(&pkg, "AG-Test", &schema, &TestDefaults);
    matinee::extract(&pkg, "AG-Test", &schema, &TestDefaults, &graph)
}

#[test]
fn synthetic_map_actions_settings_and_bindings() {
    let m = extract_spec(&spec());
    assert_eq!(m.format, matinee::MATINEE_FORMAT);
    assert_eq!(m.package, "AG-Test");
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.actions.len(), 3);
    let a = m
        .actions
        .iter()
        .find(|a| a.path.ends_with("Main_Sequence.SeqAct_Interp_0"))
        .unwrap();
    assert_eq!(a.scope, NodeScope::Level);
    assert_eq!(a.comment.as_deref(), Some("lift"));
    assert!(a.interp_data.as_deref().unwrap().ends_with("InterpData_0"));
    assert_eq!(a.settings.play_rate, 0.5);
    assert!(a.settings.looping);
    assert!(!a.settings.rewind_on_play);
    assert_eq!(a.settings.stored, vec!["PlayRate", "bLooping"]);
    assert_eq!(a.inputs.len(), 5);
    assert_eq!(a.outputs, vec!["Completed", "Reversed", "Boom"]);
    // Data link excluded; the PlayRate link is a property link.
    let labels: Vec<&str> = a.bindings.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, vec!["Mover", "Named", "Nobody"]);
    assert_eq!(a.property_links.len(), 1);
    assert_eq!(a.property_links[0].property, "PlayRate");
    assert_eq!(
        a.property_links[0].variables[0].value,
        Some(Value::Float(0.25))
    );
    let mover = &a.bindings[0];
    assert_eq!(mover.group.as_deref(), Some("Mover"));
    assert_eq!(mover.group_kind, Some(GroupKind::Group));
    assert_eq!(mover.targets.len(), 1);
    assert!(
        mover.targets[0]
            .object
            .as_deref()
            .unwrap()
            .ends_with("InterpActor_0")
    );
    assert_eq!(
        mover.targets[0].object_class.as_deref(),
        Some("InterpActor")
    );
    // Through a named variable.
    let named = &a.bindings[1];
    assert_eq!(named.group.as_deref(), Some("Named"));
    assert_eq!(named.targets[0].named.as_deref(), Some("Target"));
    assert!(
        named.targets[0]
            .object
            .as_deref()
            .unwrap()
            .ends_with("InterpActor_0")
    );
    // A label without a group.
    assert!(a.bindings[2].group.is_none() && a.bindings[2].targets.is_empty());

    let c = &m.coverage;
    assert_eq!(c.actions, 3);
    assert_eq!(c.level_actions, 2);
    assert_eq!(c.actions_with_data, 3);
    assert_eq!(c.property_links, 1);
    assert_eq!(c.bindings_without_group, 1);
    assert_eq!(c.bound_classes.get("InterpActor"), Some(&2));
    // The prefab instance's group "Arch" has a link but no actor.
    assert_eq!(c.unbound_groups, 2);
}

#[test]
fn synthetic_map_groups_tracks_and_defaults() {
    let m = extract_spec(&spec());
    let d = m
        .interp_data
        .iter()
        .find(|d| d.path.ends_with("Main_Sequence.InterpData_0"))
        .unwrap();
    assert_eq!(d.length, 4.0);
    assert_eq!(d.ed_section, [0.0, 4.0]);
    assert_eq!(d.scope, NodeScope::Level);
    assert_eq!(d.used_by.len(), 1);
    let kinds: Vec<GroupKind> = d.groups.iter().map(|g| g.kind).collect();
    assert_eq!(
        kinds,
        vec![GroupKind::Group, GroupKind::Director, GroupKind::Group]
    );
    let g = &d.groups[0];
    assert_eq!(g.name, "Mover");
    assert_eq!(g.color, [3, 2, 1, 255]);
    // Director group: the class default name.
    assert_eq!(d.groups[1].name, "InterpGroup");
    match &g.tracks[0].data {
        TrackData::Move(mv) => {
            assert_eq!(mv.move_frame, MoveFrame::RelativeToInitial);
            assert_eq!(mv.pos.points.len(), 2);
            assert_eq!(mv.pos.eval(1.0, [0.0; 3]), [100.0, 0.0, 0.0]);
            assert_eq!(mv.lookup.len(), 2);
            assert!(mv.lookup.iter().all(|k| k.group.is_none()));
            // At time 0 the actor stays where it was placed.
            let inst = MoveInstance::new(
                mv,
                [100.0, 200.0, 300.0],
                [0, 16384, 0],
                0.0,
                &NoGroupActors,
            );
            let s = mv.sample(0.0, &inst, &NoGroupActors).unwrap();
            assert!(close3(s.location, [100.0, 200.0, 300.0], 1e-3));
        }
        other => panic!("{other:?}"),
    }
    match &g.tracks[1].data {
        TrackData::Event(ev) => {
            assert_eq!(ev.keys[0].name, "Boom");
            assert_eq!(ev.keys[0].time, 1.5);
            // Flags from the class defaults.
            assert!(ev.fire_forwards && ev.fire_backwards && !ev.fire_jumping_forwards);
        }
        other => panic!("{other:?}"),
    }
    match &d.groups[1].tracks[0].data {
        TrackData::Director(dt) => {
            assert_eq!(dt.cuts[0].target_group.as_deref(), Some("Mover"));
            assert_eq!(dt.cuts[0].shot, 10);
        }
        other => panic!("{other:?}"),
    }
    match &d.groups[1].tracks[1].data {
        TrackData::Fade(f) => {
            assert!(f.persist_fade);
            assert_eq!(f.curve.points[1].mode, CurveMode::CurveAutoClamped);
        }
        other => panic!("{other:?}"),
    }
    // Split move track: six axis sub-tracks folded into the move track.
    let t = &d.groups[2].tracks[0];
    assert!(t.sub_tracks.is_empty());
    match &t.data {
        TrackData::Move(mv) => {
            assert_eq!(mv.axes.len(), 6);
            assert_eq!(mv.axes[4].axis, MoveAxis::RotationY);
            assert_eq!(mv.eval_position(0.0, &NoGroupActors), [10.0, 20.0, 30.0]);
            assert_eq!(mv.eval_euler(0.0, &NoGroupActors), [40.0, 50.0, 60.0]);
        }
        other => panic!("{other:?}"),
    }
    let c = &m.coverage;
    assert_eq!(c.groups.get("InterpGroup"), Some(&4));
    assert_eq!(c.groups.get("InterpGroupDirector"), Some(&1));
    assert_eq!(c.groups.get("InterpGroupCamera"), Some(&1));
    assert_eq!(
        c.tracks.get("InterpTrackMoveAxis").map(|t| t.count),
        Some(6)
    );
    assert_eq!(c.split_move_tracks, 1);
    assert_eq!(c.axis_order_mismatches, 0);
    assert_eq!(c.move_frames.get("IMF_RelativeToInitial"), Some(&1));
    assert_eq!(c.tracks_unknown, 0);
    assert_eq!(c.orphan_tracks, 1);
    assert_eq!(m.orphans.len(), 1);
    assert!(m.orphans[0].ends_with("InterpTrackToggle_9"));
    assert_eq!(c.stored_instances, 0);
    assert_eq!(c.decode_failures, 0);
}

#[test]
fn synthetic_prefab_instance_inherits_and_remaps() {
    let m = extract_spec(&spec());
    let inst = m
        .interp_data
        .iter()
        .find(|d| d.path.ends_with("Inst_Seq.InterpData_1"))
        .unwrap();
    assert_eq!(inst.scope, NodeScope::Level);
    // Inherited from the archetype.
    assert_eq!(inst.length, 2.0);
    assert_eq!(inst.groups.len(), 1);
    let g = &inst.groups[0];
    // The inherited reference points at the instance's own group and track.
    assert!(g.path.contains("Inst_Seq.InterpData_1."), "{}", g.path);
    assert_eq!(g.name, "Arch");
    let t = &g.tracks[0];
    assert!(t.path.contains("Inst_Seq."), "{}", t.path);
    match &t.data {
        TrackData::FloatProperty(fp) => {
            assert_eq!(fp.name.as_deref(), Some("Brightness"));
            assert_eq!(fp.curve.eval(1.0, 0.0), 2.0);
        }
        other => panic!("{other:?}"),
    }
    let a = m
        .actions
        .iter()
        .find(|a| a.path.ends_with("Inst_Seq.SeqAct_Interp_1"))
        .unwrap();
    assert_eq!(a.interp_data.as_deref(), Some(inst.path.as_str()));
    assert!(a.settings.rewind_on_play);
    assert!(a.settings.stored.is_empty(), "inherited, not stored");
    let arch = m
        .interp_data
        .iter()
        .find(|d| d.path.ends_with("PrefabSequence_0.InterpData_9"))
        .unwrap();
    assert_eq!(arch.scope, NodeScope::Prefab);
    // InterpData_1, its group and its track.
    assert_eq!(m.coverage.archetype_merges, 3);
    assert_eq!(m.coverage.unremapped_refs, 0);
    assert_eq!(m.coverage.level_interp_data, 2);
}

#[test]
fn synthetic_camera_anim() {
    let m = extract_spec(&spec());
    assert_eq!(m.camera_anims.len(), 1);
    let c = &m.camera_anims[0];
    assert_eq!(c.length, 1.5);
    let g = c.group.as_ref().unwrap();
    assert_eq!(g.kind, GroupKind::Camera);
    match &g.tracks[0].data {
        TrackData::Move(mv) => assert_eq!(mv.pos.eval(5.0, [0.0; 3]), [1.0, 2.0, 3.0]),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.coverage.camera_anims, 1);
}

// ================================================================== hostile

#[test]
fn cycles_and_bad_references_are_bounded() {
    use ex::*;
    let mut s = spec();
    // A track that lists itself (and its group) as sub-tracks.
    s[MOVE2].props = vec![("SubTracks", objs(&[MOVE2, GROUP2, AXIS0]))];
    // A group list with a non-group, a duplicate, an import and null.
    s[DATA].props = vec![
        ("InterpLength", V::Float(4.0)),
        (
            "InterpGroups",
            V::Arr(vec![
                V::Obj(e(GROUP)),
                V::Obj(e(GROUP)),
                V::Obj(e(MOVE)),
                V::Obj(-1),
                V::Obj(0),
                V::Obj(e(GROUP2)),
            ]),
        ),
        parent(MAIN),
    ];
    // An archetype cycle.
    s[INST_TRACK].archetype = e(INST_TRACK);
    s[ARCH_TRACK].archetype = e(INST_TRACK);
    let m = extract_spec(&s);
    let d = m
        .interp_data
        .iter()
        .find(|d| d.path.ends_with("Main_Sequence.InterpData_0"))
        .unwrap();
    assert_eq!(d.groups.len(), 2);
    assert!(d.warnings.len() >= 3, "{:?}", d.warnings);
    let g2 = &d.groups[1];
    match &g2.tracks[0].data {
        TrackData::Move(mv) => assert_eq!(mv.axes.len(), 1),
        other => panic!("{other:?}"),
    }
    // The group listed as a sub-track decodes as an unknown track.
    assert_eq!(g2.tracks[0].sub_tracks.len(), 1);
    assert!(!m.warnings.is_empty());
}

#[test]
fn truncated_and_corrupted_packages_never_panic() {
    let bytes = build_bytes(&spec());
    let schema = TestSchema::new();
    for cut in (0..bytes.len()).step_by(bytes.len() / 97 + 1) {
        let mut b = bytes.clone();
        b.truncate(cut);
        if let Ok(pkg) = Package::from_bytes(b) {
            let graph = build_graph(&pkg, "AG-Test", &schema, &TestDefaults);
            let _ = matinee::extract(&pkg, "AG-Test", &schema, &TestDefaults, &graph);
        }
    }
    let mut seed = 0x9E37_79B9u32;
    for _ in 0..64 {
        let mut b = bytes.clone();
        for _ in 0..8 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let i = (seed as usize) % b.len();
            b[i] = (seed >> 24) as u8;
        }
        if let Ok(pkg) = Package::from_bytes(b) {
            let graph = build_graph(&pkg, "AG-Test", &schema, &TestDefaults);
            let m = matinee::extract(&pkg, "AG-Test", &schema, &TestDefaults, &graph);
            assert!(m.warnings.len() <= matinee::MAX_WARNINGS + 1);
        }
    }
}

// ================================================================== other tracks

#[test]
fn event_keys_fire_by_direction_and_window() {
    let ev = matinee::EventTrack {
        keys: vec![
            matinee::EventKey {
                time: 0.0,
                name: "Start".into(),
            },
            matinee::EventKey {
                time: 1.0,
                name: "Mid".into(),
            },
            matinee::EventKey {
                time: 4.0,
                name: "End".into(),
            },
        ],
        fire_forwards: true,
        fire_backwards: true,
        fire_jumping_forwards: false,
    };
    // Forwards: last <= time < new; the end key fires on reaching the end.
    assert_eq!(ev.fired_keys(0.0, 1.0, 4.0, false, false), vec![0]);
    assert_eq!(ev.fired_keys(1.0, 4.0, 4.0, false, false), vec![1, 2]);
    // Backwards: new < time <= last; the start key fires on reaching 0.
    assert_eq!(ev.fired_keys(4.0, 1.0, 4.0, true, false), vec![2]);
    assert_eq!(ev.fired_keys(1.0, 0.0, 4.0, true, false), vec![0, 1]);
    // Jumps fire only forwards and only when allowed.
    assert!(ev.fired_keys(0.0, 4.0, 4.0, false, true).is_empty());
    let mut jf = ev.clone();
    jf.fire_jumping_forwards = true;
    assert_eq!(jf.fired_keys(0.5, 4.0, 4.0, false, true), vec![1, 2]);
    assert!(jf.fired_keys(4.0, 0.5, 4.0, false, true).is_empty());
    let mut fwd_only = ev.clone();
    fwd_only.fire_backwards = false;
    assert!(fwd_only.fired_keys(4.0, 0.0, 4.0, true, false).is_empty());
}

#[test]
fn director_cuts_fade_and_slomo() {
    let cut = |time: f32, group: &str| matinee::DirectorCut {
        time,
        transition_time: time / 10.0,
        target_group: Some(group.to_owned()),
        shot: 0,
    };
    let d = matinee::DirectorTrack {
        cuts: vec![cut(1.0, "CamA"), cut(2.0, "CamB")],
        simulate_cuts_on_clients: true,
    };
    assert_eq!(d.cut_index(0.5), None);
    // The first cut needs t strictly after it; later cuts apply at their time.
    assert_eq!(d.cut_index(1.0), None);
    assert_eq!(d.cut_index(1.5), Some(0));
    assert_eq!(d.cut_index(2.0), Some(1));
    assert_eq!(
        d.viewed_group(0.0, "DirGroup"),
        ("DirGroup".to_owned(), 0.0, 0.0)
    );
    assert_eq!(
        d.viewed_group(3.0, "DirGroup"),
        ("CamB".to_owned(), 2.0, 0.2)
    );

    let fade = matinee::FadeTrack {
        curve: fcurve(vec![
            fpt(0.0, -1.0, CurveMode::Linear),
            fpt(2.0, 3.0, CurveMode::Linear),
        ]),
        persist_fade: false,
    };
    assert_eq!(fade.amount_at(0.0), 0.0);
    assert_eq!(fade.amount_at(0.75), 0.5);
    assert_eq!(fade.amount_at(2.0), 1.0);
    let slomo = fcurve(vec![
        fpt(0.0, 0.0, CurveMode::Linear),
        fpt(1.0, 2.0, CurveMode::Linear),
    ]);
    assert_eq!(matinee::slomo_factor(&slomo, 0.0), 0.1);
    assert_eq!(matinee::slomo_factor(&slomo, 0.5), 1.0);
}

// ================================================================== verification additions

/// Hand-computed reference values (exact dyadic fractions, so the `f32`
/// results must match bit for bit). Segment `k0 → k1` with `k0 = (t 0,
/// value 1, leave tangent 2)` and `k1 = (t 2, value 5, arrive tangent -1)`.
/// At `t = 0.5` (`a = 1/4`): `h00 = 27/32`, `h10 = 9/64`, `h11 = -3/64`,
/// `h01 = 5/32`; the tangents scaled by the span 2 are `4` and `-2`, so the
/// cubic value is `27/32 + 36/64 + 6/64 + 25/32 = 2.28125`; without span
/// scaling (`IMT_UseBrokenTangentEval`) it is `27/32 + 18/64 + 3/64 + 25/32
/// = 1.953125`. At `t = 1.5` (`a = 3/4`): `h00 = 5/32`, `h10 = 3/64`,
/// `h11 = -9/64`, `h01 = 27/32`, giving `4.84375` (scaled) and `4.609375`
/// (unscaled).
#[test]
fn every_mode_matches_hand_computed_values() {
    let curve = |mode: CurveMode, method: InterpMethod| InterpCurve {
        points: vec![
            CurvePoint::with_tangents(0.0, 1.0f32, 0.0, 2.0, mode),
            CurvePoint::with_tangents(2.0, 5.0f32, -1.0, 0.0, CurveMode::Linear),
        ],
        method,
    };
    let default = InterpMethod::FixedTangentEvalAndNewAutoTangents;
    // Linear: 1 + a·4; Constant: the start value.
    assert_eq!(curve(CurveMode::Linear, default).eval(0.5, 0.0), 2.0);
    assert_eq!(curve(CurveMode::Linear, default).eval(1.5, 0.0), 4.0);
    assert_eq!(curve(CurveMode::Constant, default).eval(0.5, 0.0), 1.0);
    assert_eq!(curve(CurveMode::Constant, default).eval(1.999, 0.0), 1.0);
    // Every cubic mode uses the stored tangents identically.
    for mode in [
        CurveMode::CurveAuto,
        CurveMode::CurveUser,
        CurveMode::CurveBreak,
        CurveMode::CurveAutoClamped,
    ] {
        for method in [default, InterpMethod::FixedTangentEval] {
            let c = curve(mode, method);
            assert_eq!(c.eval(0.5, 0.0), 2.28125, "{mode:?} {method:?}");
            assert_eq!(c.eval(1.5, 0.0), 4.84375, "{mode:?} {method:?}");
        }
        let b = curve(mode, InterpMethod::BrokenTangentEval);
        assert_eq!(b.eval(0.5, 0.0), 1.953125, "{mode:?}");
        assert_eq!(b.eval(1.5, 0.0), 4.609375, "{mode:?}");
    }
    // Vector curves: the same per component. Component 1 has flat
    // tangents (`27/32·(-1) + 5/32·3 = -0.375`), component 2 has tangents 8
    // and 4 scaled to 16 and 8 (`9/64·16 − 3/64·8 = 1.875`).
    let v = InterpCurve::new(vec![
        CurvePoint::with_tangents(
            0.0,
            [1.0, -1.0, 0.0],
            [0.0; 3],
            [2.0, 0.0, 8.0],
            CurveMode::CurveUser,
        ),
        CurvePoint::with_tangents(
            2.0,
            [5.0, 3.0, 0.0],
            [-1.0, 0.0, 4.0],
            [0.0; 3],
            CurveMode::Linear,
        ),
    ]);
    assert_eq!(v.eval(0.5, [0.0; 3]), [2.28125, -0.375, 1.875]);
}

/// Hand-computed automatic tangents for keys `(0, 0)`, `(1, 1)`, `(2, 4)`.
/// `CurveAuto`: `((1 − 0) + (4 − 1)) / (2 − 0) = 2`. `CurveAutoClamped`:
/// not an extreme; the average slope is 2, the key sits at height `1/4`
/// of the neighbours' range (below 0.333), so the tangent is blended
/// towards the previous slope 1 with weight `1 − 0.25/0.333 ≈ 0.249249`,
/// giving `≈ 1.750751`, which stays below the average slope. The first key
/// gets a zero leave tangent, so at `t = 0.5` the curve is
/// `0.5 − 0.125·T1`: `0.25` (auto) and `≈ 0.281156` (clamped).
#[test]
fn auto_tangents_match_hand_computed_values() {
    let keys = |mode: CurveMode| {
        fcurve(vec![
            fpt(0.0, 0.0, mode),
            fpt(1.0, 1.0, mode),
            fpt(2.0, 4.0, mode),
        ])
    };
    let mut auto = keys(CurveMode::CurveAuto);
    auto.auto_set_tangents(0.0);
    assert_eq!(auto.points[1].arrive_tangent, 2.0);
    assert_eq!(auto.points[1].leave_tangent, 2.0);
    assert_eq!(auto.eval(0.5, 0.0), 0.25);
    let mut clamped = keys(CurveMode::CurveAutoClamped);
    clamped.auto_set_tangents(0.0);
    let t = clamped.points[1].leave_tangent;
    assert!((t - 1.750_751).abs() < 1e-6, "{t}");
    assert_eq!(clamped.points[1].arrive_tangent, t);
    assert!((clamped.eval(0.5, 0.0) - 0.281_156).abs() < 1e-6);
    // Tension 0.5 halves both.
    let mut half = keys(CurveMode::CurveAutoClamped);
    half.auto_set_tangents(0.5);
    assert_eq!(half.points[1].leave_tangent, t * 0.5);
}

#[test]
fn reverse_loop_wrap_fires_no_events() {
    let ev = matinee::EventTrack {
        keys: vec![
            matinee::EventKey {
                time: 0.0,
                name: "Start".into(),
            },
            matinee::EventKey {
                time: 2.0,
                name: "Mid".into(),
            },
        ],
        fire_forwards: true,
        fire_backwards: true,
        fire_jumping_forwards: true,
    };
    // The wrap of a reverse loop is a jump from 0 to the end while playing
    // in reverse: the engine treats it as backwards, so nothing fires.
    assert!(ev.fired_keys(0.0, 4.0, 4.0, true, true).is_empty());
    // The same jump while stopped is a forwards jump.
    assert_eq!(ev.fired_keys(0.0, 4.0, 4.0, false, true), vec![0, 1]);
    // A normal reverse update still fires backwards.
    assert_eq!(ev.fired_keys(4.0, 1.0, 4.0, true, false), vec![1]);
}

#[test]
fn restarting_from_where_it_stopped_keeps_the_path() {
    let mut t = move_track(MoveFrame::RelativeToInitial);
    for p in &mut t.euler.points {
        p.out_val = [0.0; 3];
    }
    let a = NoGroupActors;
    // A lift placed at the bottom, initialised at position 0, rises 100.
    let first = MoveInstance::new(&t, [0.0; 3], [0, 0, 0], 0.0, &a);
    let top = t.sample(1.0, &first, &a).unwrap().location;
    assert!(close3(top, [100.0, 0.0, 0.0], 1e-3), "{top:?}");
    // Restarted by Reverse at position 1 (the engine re-initialises with the
    // action's current position): it goes back down the same path.
    let again = MoveInstance::new(&t, top, [0, 0, 0], 1.0, &a);
    let s1 = t.sample(1.0, &again, &a).unwrap().location;
    let s0 = t.sample(0.0, &again, &a).unwrap().location;
    assert!(close3(s1, top, 1e-3), "{s1:?}");
    assert!(close3(s0, [0.0; 3], 1e-3), "{s0:?}");
    // A time-0 instance would have moved the whole path up by 100.
    let rebased = MoveInstance::new(&t, top, [0, 0, 0], 0.0, &a);
    let wrong = t.sample(0.0, &rebased, &a).unwrap().location;
    assert!(close3(wrong, top, 1e-3), "{wrong:?}");
}

#[test]
fn inputs_follow_the_engine_precedence() {
    use matinee::PlaybackInputs as I;
    let mut p = Playback::new(4.0, settings());
    // Stopped: Pause alone does nothing and does not start the action.
    let pause = I {
        pause: true,
        ..I::default()
    };
    assert!(!p.needs_init(pause));
    p.apply_inputs(pause);
    assert!(!p.playing && !p.paused);
    // Stopped with Pause and Reverse: Reverse starts it.
    let pr = I {
        pause: true,
        reverse: true,
        ..I::default()
    };
    assert!(p.needs_init(pr));
    p.apply_inputs(pr);
    assert!(p.playing && p.reverse && !p.paused);
    // Playing: Pause wins over Play.
    let pp = I {
        pause: true,
        play: true,
        ..I::default()
    };
    assert!(!p.needs_init(pp));
    p.apply_inputs(pp);
    assert!(p.paused && p.reverse);
    // Stop wins over Change Dir.
    p.apply_inputs(I {
        stop: true,
        change_dir: true,
        ..I::default()
    });
    assert!(!p.playing && !p.paused);
    // Change Dir alone starts a stopped action in the other direction.
    p.apply_inputs(I {
        change_dir: true,
        ..I::default()
    });
    assert!(p.playing && !p.reverse);
}

#[test]
fn nan_steps_count_as_past_the_end() {
    let mut p = Playback::new(4.0, settings());
    p.play();
    let r = p.step(f32::NAN);
    assert_eq!((r.to, r.finished), (4.0, true));
    assert!(!p.playing);
    let mut q = Playback::new(4.0, settings());
    q.position = 2.0;
    q.play_reverse();
    let r = q.step(f32::NAN);
    assert_eq!((r.to, r.finished), (0.0, true));
    let mut l = settings();
    l.looping = true;
    let mut w = Playback::new(4.0, l);
    w.play();
    let r = w.step(f32::NAN);
    assert!(r.wrapped && !r.finished && r.to.is_nan());
}

#[test]
fn track_notes_are_capped() {
    let points = Value::Array((0..1000).map(Value::Int).collect());
    let curve = Value::Struct {
        name: "InterpCurveFloat".into(),
        binary: false,
        fields: vec![fprop("Points", points)],
    };
    let mut w = Vec::new();
    let conv: fn(&Value) -> Option<f32> = |v| match v {
        Value::Float(f) => Some(*f),
        _ => None,
    };
    let c = decode_curve(Some(&curve), conv, &mut w, "FloatTrack");
    assert!(c.points.is_empty());
    assert_eq!(w.len(), matinee::MAX_LOCAL_WARNINGS);
    assert_eq!(w.last().map(String::as_str), Some("further notes not kept"));
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Many `InterpData` sharing one group that lists one track thousands of
/// times: without a per-map budget the work (and the output) would grow as
/// the product of the three counts.
#[test]
fn shared_groups_and_tracks_hit_the_map_budget() {
    let points: Vec<V> = (0..8)
        .map(|k| fpoint(k as f32, k as f32, "CIM_Linear"))
        .collect();
    let mut s = vec![
        x(
            "Engine.InterpTrackFloatProp",
            TOP,
            "InterpTrackFloatProp_0",
            vec![("FloatTrack", fcurve_v(points))],
        ),
        x(
            "Engine.InterpGroup",
            TOP,
            "InterpGroup_0",
            vec![("InterpTracks", objs(&[0; 4096]))],
        ),
    ];
    for k in 0..20 {
        s.push(x(
            "Engine.InterpData",
            TOP,
            leak(format!("InterpData_{k}")),
            vec![("InterpGroups", objs(&[1]))],
        ));
    }
    let m = extract_spec(&s);
    assert_eq!(m.interp_data.len(), 20);
    assert_eq!(m.coverage.tracks_total, matinee::MAX_MAP_TRACKS);
    assert!(
        m.warnings.iter().any(|w| w.contains("track decodes")),
        "{:?}",
        m.warnings
    );
    // Later data still decode their groups, but without tracks.
    assert!(m.interp_data[19].groups[0].tracks.is_empty());
}
