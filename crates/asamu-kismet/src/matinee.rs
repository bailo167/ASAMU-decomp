//! Matinee playback for the interpreter: the importer's Matinee export
//! (`<converted>/matinee/<map>.matinee.json`, format `asamu-matinee` v1,
//! written by `tools/asamu-import/src/matinee.rs` from
//! `asamu_ue3::matinee::MatineeMap`) deserialized, and the evaluation rules of
//! `docs/reverse-engineering/MATINEE.md`.
//!
//! This crate cannot depend on `asamu-ue3`, so the evaluators are a port of
//! `asamu_ue3::matinee` (curves, rotators, the 16384-entry sine table, move
//! tracks with initial transforms and the winding-preserving world
//! transform, event/director/fade tracks, `SeqAct_Interp` playback) with the
//! same operation order. The importer writes probes (move-track samples
//! computed with `asamu_ue3::matinee`) into the Kismet export; the gated test
//! `matinee_probes_match_the_reference_evaluator` checks this port against
//! them bit for bit.

use std::collections::BTreeMap;
use std::f64::consts::TAU;

use serde::Deserialize;

/// `format` of a Matinee file.
pub const MATINEE_FORMAT: &str = "asamu-matinee";
/// Supported `version`.
pub const MATINEE_VERSION: u32 = 1;

// ================================================================== curves

/// `EInterpCurveMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CurveMode {
    /// `CIM_Linear`.
    #[default]
    Linear,
    /// `CIM_CurveAuto`.
    CurveAuto,
    /// `CIM_Constant`.
    Constant,
    /// `CIM_CurveUser`.
    CurveUser,
    /// `CIM_CurveBreak`.
    CurveBreak,
    /// `CIM_CurveAutoClamped`.
    CurveAutoClamped,
}

/// `EInterpMethodType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterpMethod {
    /// The struct default.
    #[default]
    FixedTangentEvalAndNewAutoTangents,
    /// `IMT_UseFixedTangentEval`.
    FixedTangentEval,
    /// `IMT_UseBrokenTangentEval` (tangents not scaled by the span).
    BrokenTangentEval,
}

/// A curve value: `f32` or `[f32; 3]`.
pub trait CurveValue: Copy + PartialEq + std::fmt::Debug + Default {
    /// Components.
    const DIM: usize;
    /// Component `i` (0 out of range).
    fn get(&self, i: usize) -> f32;
    /// Set component `i`.
    fn set(&mut self, i: usize, v: f32);
}

impl CurveValue for f32 {
    const DIM: usize = 1;
    fn get(&self, i: usize) -> f32 {
        if i == 0 { *self } else { 0.0 }
    }
    fn set(&mut self, i: usize, v: f32) {
        if i == 0 {
            *self = v;
        }
    }
}

impl CurveValue for [f32; 3] {
    const DIM: usize = 3;
    fn get(&self, i: usize) -> f32 {
        self.as_slice().get(i).copied().unwrap_or(0.0)
    }
    fn set(&mut self, i: usize, v: f32) {
        if let Some(c) = self.as_mut_slice().get_mut(i) {
            *c = v;
        }
    }
}

/// One key.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Default)]
pub struct CurvePoint<T> {
    /// `InVal`.
    #[serde(rename = "in", default)]
    pub in_val: f32,
    /// `OutVal`.
    #[serde(rename = "out", default)]
    pub out_val: T,
    /// `ArriveTangent`.
    #[serde(rename = "arrive", default)]
    pub arrive: T,
    /// `LeaveTangent`.
    #[serde(rename = "leave", default)]
    pub leave: T,
    /// `InterpMode`.
    #[serde(default)]
    pub mode: CurveMode,
}

/// A curve.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct InterpCurve<T> {
    /// Keys.
    #[serde(default)]
    pub points: Vec<CurvePoint<T>>,
    /// `InterpMethod`.
    #[serde(default)]
    pub method: InterpMethod,
}

fn lt(a: f32, b: f32) -> bool {
    a.partial_cmp(&b) == Some(std::cmp::Ordering::Less)
}

fn le(a: f32, b: f32) -> bool {
    matches!(
        a.partial_cmp(&b),
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
    )
}

/// Cubic Hermite in the executable's operation order (MATINEE.md).
#[must_use]
pub fn hermite(p0: f32, t0: f32, p1: f32, t1: f32, a: f32) -> f32 {
    let a2 = a * a;
    let a3 = a * a2;
    let two_a3 = a3 + a3;
    let three_a2 = 3.0 * a2;
    let h00 = (two_a3 - three_a2) + 1.0;
    let h10 = (a3 - (a2 + a2)) + a;
    let h11 = a3 - a2;
    let h01 = three_a2 - two_a3;
    ((h00 * p0 + h10 * t0) + h11 * t1) + h01 * p1
}

impl<T: CurveValue> InterpCurve<T> {
    /// Value at `t` (`default` without keys); MATINEE.md "Curve evaluation".
    #[must_use]
    pub fn eval(&self, t: f32, default: T) -> T {
        let n = self.points.len();
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return default;
        };
        if n < 2 || t <= first.in_val {
            return first.out_val;
        }
        if !lt(t, last.in_val) {
            return last.out_val;
        }
        for pair in self.points.windows(2) {
            let [p0, p1] = pair else { continue };
            if t < p1.in_val {
                return self.segment(p0, p1, t);
            }
        }
        last.out_val
    }

    fn segment(&self, p0: &CurvePoint<T>, p1: &CurvePoint<T>, t: f32) -> T {
        let diff = p1.in_val - p0.in_val;
        if !lt(0.0, diff) || p0.mode == CurveMode::Constant {
            return p0.out_val;
        }
        let alpha = (t - p0.in_val) / diff;
        let mut out = p0.out_val;
        if p0.mode == CurveMode::Linear {
            for c in 0..T::DIM {
                let (a, b) = (p0.out_val.get(c), p1.out_val.get(c));
                out.set(c, alpha * (b - a) + a);
            }
            return out;
        }
        let broken = self.method == InterpMethod::BrokenTangentEval;
        for c in 0..T::DIM {
            let (t0, t1) = if broken {
                (p0.leave.get(c), p1.arrive.get(c))
            } else {
                (p0.leave.get(c) * diff, diff * p1.arrive.get(c))
            };
            out.set(
                c,
                hermite(p0.out_val.get(c), t0, p1.out_val.get(c), t1, alpha),
            );
        }
        out
    }
}

// ================================================================== rotations

/// Rotator units per degree (`FRotator::MakeFromEuler`).
pub const DEG_TO_ROT: f32 = 182.044_45;
/// Degrees per rotator unit (`FRotator::Euler`).
pub const ROT_TO_DEG: f32 = 0.005_493_164;

fn trunc_i32(v: f32) -> i32 {
    if v.is_nan() || v >= 2_147_483_648.0 || v < -2_147_483_648.0 {
        i32::MIN
    } else {
        v as i32
    }
}

/// Euler degrees `(roll, pitch, yaw)` to a rotator `[pitch, yaw, roll]`,
/// truncating.
#[must_use]
pub fn rotator_from_euler(e: [f32; 3]) -> [i32; 3] {
    [
        trunc_i32(e[1] * DEG_TO_ROT),
        trunc_i32(e[2] * DEG_TO_ROT),
        trunc_i32(e[0] * DEG_TO_ROT),
    ]
}

/// Rotator to Euler degrees `(roll, pitch, yaw)`.
#[must_use]
pub fn euler_from_rotator(r: [i32; 3]) -> [f32; 3] {
    [
        r[2] as f32 * ROT_TO_DEG,
        r[0] as f32 * ROT_TO_DEG,
        r[1] as f32 * ROT_TO_DEG,
    ]
}

/// One component normalized to `[-32768, 32767]`.
#[must_use]
pub fn normalize_axis(a: i32) -> i32 {
    let low = a & 0xFFFF;
    if low < 0x8000 { low } else { low - 0x10000 }
}

/// `(winding, remainder)` of a rotator.
#[must_use]
pub fn winding_and_remainder(r: [i32; 3]) -> ([i32; 3], [i32; 3]) {
    let rem = r.map(normalize_axis);
    (
        [
            r[0].wrapping_sub(rem[0]),
            r[1].wrapping_sub(rem[1]),
            r[2].wrapping_sub(rem[2]),
        ],
        rem,
    )
}

/// Table sine and cosine of a rotator angle (index `(angle >> 2) & 0x3FFF`).
#[must_use]
pub fn table_sin_cos(angle: i32) -> (f32, f32) {
    let entry = |u: u32| (f64::from((u >> 2) & 0x3FFF) * TAU / 16384.0).sin() as f32;
    let bits = angle as u32;
    (entry(bits), entry(bits.wrapping_add(0x4000)))
}

/// Row-vector 4×4 transform.
pub type Matrix = [[f32; 4]; 4];

/// Identity.
pub const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `FRotationTranslationMatrix` with table trigonometry.
#[must_use]
pub fn rotation_translation_matrix(rot: [i32; 3], origin: [f32; 3]) -> Matrix {
    let (sp, cp) = table_sin_cos(rot[0]);
    let (sy, cy) = table_sin_cos(rot[1]);
    let (sr, cr) = table_sin_cos(rot[2]);
    [
        [cp * cy, cp * sy, sp, 0.0],
        [
            sr * sp * cy - cr * sy,
            sr * sp * sy + cr * cy,
            -sr * cp,
            0.0,
        ],
        [
            -(cr * sp * cy + sr * sy),
            cy * sr - cr * sp * sy,
            cr * cp,
            0.0,
        ],
        [origin[0], origin[1], origin[2], 1.0],
    ]
}

/// `a · b`.
#[must_use]
pub fn matrix_mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0f32; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

/// `p · M`.
#[must_use]
pub fn transform_point(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        *o = p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j];
    }
    out
}

/// `v · M` without translation.
#[must_use]
pub fn transform_vector(m: &Matrix, v: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        *o = v[0] * m[0][j] + v[1] * m[1][j] + v[2] * m[2][j];
    }
    out
}

/// Inverse of a rigid transform.
#[must_use]
pub fn rigid_inverse(m: &Matrix) -> Matrix {
    let mut out = IDENTITY;
    for (i, row) in out.iter_mut().take(3).enumerate() {
        for (j, cell) in row.iter_mut().take(3).enumerate() {
            *cell = m[j][i];
        }
    }
    let t = [m[3][0], m[3][1], m[3][2]];
    let r = transform_vector(&out, t);
    out[3] = [-r[0], -r[1], -r[2], 1.0];
    out
}

/// Rows 0..2 normalized.
#[must_use]
pub fn remove_scaling(m: &Matrix) -> Matrix {
    let mut out = *m;
    for row in out.iter_mut().take(3) {
        let sq = row[2] * row[2] + row[1] * row[1] + row[0] * row[0];
        if -1.0e-8 + sq >= 0.0 {
            let inv = 1.0 / sq.sqrt();
            row[0] *= inv;
            row[1] *= inv;
            row[2] *= inv;
        }
    }
    out
}

/// `FMatrix::Rotator` / `GetCleanedUpRotator`.
#[must_use]
pub fn matrix_rotator(m: &Matrix, clean: bool) -> [i32; 3] {
    let flush = |x: f32| if clean && x.abs() < 1.0e-5 { 0.0 } else { x };
    let to_units = |rad: f32| -> i32 {
        let v = (f64::from(rad * 32768.0) / std::f64::consts::PI) as f32;
        let r = v.round();
        if r.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&r) {
            i32::MIN
        } else {
            r as i32
        }
    };
    let x = m[0];
    let pitch = to_units(flush(x[2]).atan2(flush((x[1] * x[1] + x[0] * x[0]).sqrt())));
    let yaw = to_units(flush(x[1]).atan2(flush(x[0])));
    let (sp, cp) = table_sin_cos(pitch);
    let (sy, cy) = table_sin_cos(yaw);
    let (sr, cr) = table_sin_cos(0);
    let sy_axis = [sp * sr * cy - sy * cr, cr * cy + sp * sr * sy, sr * cp];
    let y = m[1];
    let z = m[2];
    let roll = to_units(
        flush((z[1] * sy_axis[1] + z[0] * sy_axis[0]) - z[2] * sy_axis[2]).atan2(flush(
            (sy_axis[1] * y[1] + sy_axis[0] * y[0]) - sy_axis[2] * y[2],
        )),
    );
    [pitch, yaw, roll]
}

/// Quaternion `(x, y, z, w)`.
pub type Quat = [f32; 4];

/// Identity quaternion.
pub const QUAT_IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];

/// `FQuat(const FMatrix&)`.
#[must_use]
pub fn quat_from_matrix(m: &Matrix) -> Quat {
    let small = |x: f32| !le(1.0e-4, x.abs());
    if (0..3).all(|i| (0..3).all(|j| small(m[i][j]))) {
        return QUAT_IDENTITY;
    }
    let tr = (m[0][0] + m[1][1]) + m[2][2];
    if tr > 0.0 {
        let inv = 1.0 / (tr + 1.0).sqrt();
        let s = inv * 0.5;
        [
            (m[1][2] - m[2][1]) * s,
            (m[2][0] - m[0][2]) * s,
            (m[0][1] - m[1][0]) * s,
            0.5 * (1.0 / inv),
        ]
    } else {
        const NEXT: [usize; 3] = [1, 2, 0];
        let mut i = usize::from(m[0][0] < m[1][1]);
        if !le(m[2][2], m[i][i]) {
            i = 2;
        }
        let j = NEXT[i];
        let k = NEXT[j];
        let inv = 1.0 / (((m[i][i] - m[j][j]) - m[k][k]) + 1.0).sqrt();
        let mut q = [0.0f32; 4];
        q[i] = (1.0 / inv) * 0.5;
        let s = inv * 0.5;
        q[3] = (m[j][k] - m[k][j]) * s;
        q[j] = (m[i][j] + m[j][i]) * s;
        q[k] = (m[i][k] + m[k][i]) * s;
        q
    }
}

/// `FQuat::MakeFromEuler` (via the table rotation matrix).
#[must_use]
pub fn quat_from_euler(e: [f32; 3]) -> Quat {
    quat_from_matrix(&rotation_translation_matrix(
        rotator_from_euler(e),
        [0.0; 3],
    ))
}

/// `SlerpQuat`.
#[must_use]
pub fn slerp_quat(a: Quat, b: Quat, alpha: f32) -> Quat {
    let raw = ((a[0] * b[0] + a[1] * b[1]) + a[2] * b[2]) + a[3] * b[3];
    let cos = if 0.0 <= raw { raw } else { -raw };
    let (s0, s1) = if 0.9999 > cos {
        let c = cos.min(1.0);
        let c = if cos < -1.0 { -1.0 } else { c };
        let omega = c.acos();
        let inv = 1.0 / omega.sin();
        (
            ((1.0 - alpha) * omega).sin() * inv,
            (omega * alpha).sin() * inv,
        )
    } else {
        (1.0 - alpha, alpha)
    };
    let s1 = if 0.0 <= raw { s1 } else { -s1 };
    [
        s1 * b[0] + s0 * a[0],
        s1 * b[1] + s0 * a[1],
        s1 * b[2] + s0 * a[2],
        s1 * b[3] + s0 * a[3],
    ]
}

/// Rotation matrix of a quaternion.
#[must_use]
pub fn quat_matrix(q: Quat) -> Matrix {
    let [x, y, z, w] = q;
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [
        [1.0 - (yy + zz), wz + xy, xz - wy, 0.0],
        [xy - wz, 1.0 - (zz + xx), yz + wx, 0.0],
        [xz + wy, yz - wx, 1.0 - (yy + xx), 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// `FRotator(const FQuat&)`.
#[must_use]
pub fn rotator_from_quat(q: Quat) -> [i32; 3] {
    matrix_rotator(&quat_matrix(q), false)
}

// ================================================================== model (JSON)

/// `EInterpTrackMoveFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MoveFrame {
    /// `IMF_World`.
    #[default]
    World,
    /// `IMF_RelativeToInitial`.
    RelativeToInitial,
}

/// `EInterpTrackMoveRotMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RotMode {
    /// `IMR_Keyframed`.
    #[default]
    Keyframed,
    /// `IMR_LookAtGroup`.
    LookAtGroup,
    /// `IMR_Ignore`.
    Ignore,
}

/// `EInterpMoveAxis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MoveAxis {
    /// X translation.
    #[default]
    TranslationX,
    /// Y translation.
    TranslationY,
    /// Z translation.
    TranslationZ,
    /// Roll.
    RotationX,
    /// Pitch.
    RotationY,
    /// Yaw.
    RotationZ,
}

/// Lookup key.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct LookupKey {
    /// `Time`.
    #[serde(default)]
    pub time: f32,
    /// `GroupName`.
    #[serde(default)]
    pub group: Option<String>,
}

/// Move axis sub-track.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct MoveAxisTrack {
    /// `MoveAxis`.
    #[serde(default)]
    pub axis: MoveAxis,
    /// Curve.
    #[serde(default)]
    pub curve: InterpCurve<f32>,
    /// Lookup keys.
    #[serde(default)]
    pub lookup: Vec<LookupKey>,
}

/// `InterpTrackMove`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct MoveTrack {
    /// `PosTrack`.
    #[serde(default)]
    pub pos: InterpCurve<[f32; 3]>,
    /// `EulerTrack` (degrees roll, pitch, yaw).
    #[serde(default)]
    pub euler: InterpCurve<[f32; 3]>,
    /// Lookup keys.
    #[serde(default)]
    pub lookup: Vec<LookupKey>,
    /// `MoveFrame`.
    #[serde(default)]
    pub move_frame: MoveFrame,
    /// `RotMode`.
    #[serde(default)]
    pub rot_mode: RotMode,
    /// `LookAtGroupName`.
    #[serde(default)]
    pub look_at_group: Option<String>,
    /// `LinCurveTension`.
    #[serde(default)]
    pub lin_curve_tension: f32,
    /// `AngCurveTension`.
    #[serde(default)]
    pub ang_curve_tension: f32,
    /// `bUseQuatInterpolation`.
    #[serde(default)]
    pub use_quat_interpolation: bool,
    /// `bDisableMovement`.
    #[serde(default)]
    pub disable_movement: bool,
    /// `bUseRawActorTMforRelativeToInitial`.
    #[serde(default)]
    pub use_raw_actor_tm: bool,
    /// Axis sub-tracks.
    #[serde(default)]
    pub axes: Vec<MoveAxisTrack>,
}

/// Event key.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct EventKey {
    /// `Time`.
    #[serde(default)]
    pub time: f32,
    /// `EventName`.
    #[serde(default)]
    pub name: String,
}

/// `InterpTrackEvent`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct EventTrack {
    /// Keys.
    #[serde(default)]
    pub keys: Vec<EventKey>,
    /// `bFireEventsWhenForwards`.
    #[serde(default)]
    pub fire_forwards: bool,
    /// `bFireEventsWhenBackwards`.
    #[serde(default)]
    pub fire_backwards: bool,
    /// `bFireEventsWhenJumpingForwards`.
    #[serde(default)]
    pub fire_jumping_forwards: bool,
}

/// Director cut.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct DirectorCut {
    /// `Time`.
    #[serde(default)]
    pub time: f32,
    /// `TransitionTime`.
    #[serde(default)]
    pub transition_time: f32,
    /// `TargetCamGroup`.
    #[serde(default)]
    pub target_group: Option<String>,
}

/// `InterpTrackDirector`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct DirectorTrack {
    /// Cuts.
    #[serde(default)]
    pub cuts: Vec<DirectorCut>,
}

/// Sound key.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct SoundKey {
    /// `Time`.
    #[serde(default)]
    pub time: f32,
    /// `Volume`.
    #[serde(default)]
    pub volume: f32,
    /// `Pitch`.
    #[serde(default)]
    pub pitch: f32,
    /// `Sound`.
    #[serde(default)]
    pub sound: Option<String>,
}

/// `InterpTrackSound`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct SoundTrack {
    /// Keys.
    #[serde(default)]
    pub keys: Vec<SoundKey>,
    /// `bPlayOnReverse`.
    #[serde(default)]
    pub play_on_reverse: bool,
}

/// `InterpTrackFade`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct FadeTrack {
    /// Curve.
    #[serde(default)]
    pub curve: InterpCurve<f32>,
    /// `bPersistFade`.
    #[serde(default)]
    pub persist_fade: bool,
}

/// A key with an action enumerator (visibility and toggle tracks).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ActionKey {
    /// `Time`.
    #[serde(default)]
    pub time: f32,
    /// Enumerator name.
    #[serde(default)]
    pub action: String,
}

/// `InterpTrackVisibility` / `InterpTrackToggle` (the fields used).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct KeyedTrack {
    /// Keys.
    #[serde(default)]
    pub keys: Vec<ActionKey>,
    /// `bFireEventsWhenForwards`.
    #[serde(default)]
    pub fire_forwards: bool,
    /// `bFireEventsWhenBackwards`.
    #[serde(default)]
    pub fire_backwards: bool,
}

/// Track contents (the classes the runtime plays; others are skipped).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrackData {
    /// Movement.
    Move(MoveTrack),
    /// Kismet events.
    Event(EventTrack),
    /// Camera cuts.
    Director(DirectorTrack),
    /// Sounds.
    Sound(SoundTrack),
    /// Screen fade.
    Fade(FadeTrack),
    /// Visibility.
    Visibility(KeyedTrack),
    /// Toggle.
    Toggle(KeyedTrack),
    /// Anything else (decoded by the importer, not played here).
    #[serde(other)]
    Other,
}

/// One track.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Track {
    /// Qualified class.
    #[serde(default)]
    pub class: String,
    /// `bDisableTrack`.
    #[serde(default)]
    pub disabled: bool,
    /// Contents.
    pub data: TrackData,
}

/// One group.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct InterpGroup {
    /// Role (`group`, `director`, `ai`, `camera`).
    #[serde(default)]
    pub kind: String,
    /// `GroupName`.
    #[serde(default)]
    pub name: String,
    /// `bIsFolder`.
    #[serde(default)]
    pub folder: bool,
    /// Tracks.
    #[serde(default)]
    pub tracks: Vec<Track>,
}

/// One `InterpData`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct InterpData {
    /// Object path.
    #[serde(default)]
    pub path: String,
    /// `InterpLength`.
    #[serde(default)]
    pub length: f32,
    /// Groups.
    #[serde(default)]
    pub groups: Vec<InterpGroup>,
}

/// Effective `SeqAct_Interp` settings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InterpSettings {
    /// `PlayRate`.
    #[serde(default = "one")]
    pub play_rate: f32,
    /// `bLooping`.
    #[serde(default)]
    pub looping: bool,
    /// `bRewindOnPlay`.
    #[serde(default)]
    pub rewind_on_play: bool,
    /// `bNoResetOnRewind`.
    #[serde(default)]
    pub no_reset_on_rewind: bool,
    /// `bRewindIfAlreadyPlaying`.
    #[serde(default)]
    pub rewind_if_already_playing: bool,
    /// `bForceStartPos`.
    #[serde(default)]
    pub force_start_pos: bool,
    /// `ForceStartPosition`.
    #[serde(default)]
    pub force_start_position: f32,
}

fn one() -> f32 {
    1.0
}

impl Default for InterpSettings {
    fn default() -> Self {
        InterpSettings {
            play_rate: 1.0,
            looping: false,
            rewind_on_play: false,
            no_reset_on_rewind: false,
            rewind_if_already_playing: false,
            force_start_pos: false,
            force_start_position: 0.0,
        }
    }
}

/// An object bound to a group.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct BoundTarget {
    /// Variable path.
    #[serde(default)]
    pub variable: String,
    /// Object path.
    #[serde(default)]
    pub object: Option<String>,
    /// Object class.
    #[serde(default)]
    pub object_class: Option<String>,
}

/// A group binding.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct GroupBinding {
    /// Variable link index.
    #[serde(default)]
    pub link: usize,
    /// Label.
    #[serde(default)]
    pub label: String,
    /// Matched group name.
    #[serde(default)]
    pub group: Option<String>,
    /// Bound objects.
    #[serde(default)]
    pub targets: Vec<BoundTarget>,
}

/// One `SeqAct_Interp`.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct MatineeAction {
    /// Object path.
    #[serde(default)]
    pub path: String,
    /// Kismet node id (in the map's own graph).
    #[serde(default)]
    pub node: usize,
    /// Scope.
    #[serde(default)]
    pub scope: String,
    /// `InterpData` path.
    #[serde(default)]
    pub interp_data: Option<String>,
    /// Settings.
    #[serde(default)]
    pub settings: InterpSettings,
    /// Group bindings.
    #[serde(default)]
    pub bindings: Vec<GroupBinding>,
}

/// A map's Matinee file.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct MatineeFile {
    /// [`MATINEE_FORMAT`].
    pub format: String,
    /// [`MATINEE_VERSION`].
    pub version: u32,
    /// Package.
    #[serde(default)]
    pub package: String,
    /// Actions.
    #[serde(default)]
    pub actions: Vec<MatineeAction>,
    /// Data.
    #[serde(default)]
    pub interp_data: Vec<InterpData>,
}

/// Matinee data indexed for the runtime.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatineeSet {
    /// Actions by Kismet node id (merged-graph ids).
    pub actions: BTreeMap<usize, MatineeAction>,
    /// Data by lower-case path.
    pub data: BTreeMap<String, InterpData>,
}

/// Errors reading a Matinee file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MatineeError {
    /// Invalid JSON.
    #[error("invalid Matinee JSON: {0}")]
    Json(String),
    /// Wrong format or version.
    #[error("unsupported Matinee file ({0})")]
    Format(String),
}

impl MatineeSet {
    /// Parses a Matinee file; level-scope actions only. `node_offset` shifts
    /// the action node ids (for sub-levels merged into a graph).
    ///
    /// # Errors
    /// Invalid JSON or an unsupported format.
    pub fn add_json(&mut self, data: &[u8], node_offset: usize) -> Result<(), MatineeError> {
        let f: MatineeFile =
            serde_json::from_slice(data).map_err(|e| MatineeError::Json(e.to_string()))?;
        if f.format != MATINEE_FORMAT || f.version != MATINEE_VERSION {
            return Err(MatineeError::Format(format!("{} v{}", f.format, f.version)));
        }
        for d in f.interp_data {
            self.data.entry(d.path.to_ascii_lowercase()).or_insert(d);
        }
        for a in f.actions {
            if a.scope != "level" {
                continue;
            }
            self.actions.insert(a.node.saturating_add(node_offset), a);
        }
        Ok(())
    }

    /// The data of action `node`.
    #[must_use]
    pub fn data_of(&self, node: usize) -> Option<(&MatineeAction, &InterpData)> {
        let a = self.actions.get(&node)?;
        let d = self
            .data
            .get(&a.interp_data.as_deref()?.to_ascii_lowercase())?;
        Some((a, d))
    }
}

// ================================================================== move evaluation

/// Per-actor move state (`UInterpTrackInstMove`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveInstance {
    /// `InitialTM` without scale.
    pub initial: Matrix,
    /// Base transform when attached.
    pub base: Option<Matrix>,
}

impl MoveTrack {
    /// True when the track moves its actor (sub-tracks or rotation keys).
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.axes.is_empty() || !self.euler.points.is_empty()
    }

    fn axis_value(&self, k: usize, t: f32) -> f32 {
        // Lookup keys naming a group are not resolved: no shipped key names
        // one (MATINEE.md), so the stored curve applies, as it does in
        // `asamu_ue3::matinee` without group actors.
        self.axes.get(k).map_or(0.0, |a| a.curve.eval(t, 0.0))
    }

    /// Relative position at `t`.
    #[must_use]
    pub fn eval_position(&self, t: f32) -> [f32; 3] {
        if !self.axes.is_empty() {
            return [
                self.axis_value(0, t),
                self.axis_value(1, t),
                self.axis_value(2, t),
            ];
        }
        self.pos.eval(t, [0.0; 3])
    }

    /// Relative Euler rotation at `t`.
    #[must_use]
    pub fn eval_euler(&self, t: f32) -> [f32; 3] {
        if !self.axes.is_empty() {
            return [
                self.axis_value(3, t),
                self.axis_value(4, t),
                self.axis_value(5, t),
            ];
        }
        self.euler.eval(t, [0.0; 3])
    }

    fn key_euler(&self, i: usize) -> [f32; 3] {
        self.euler.points.get(i).map_or([0.0; 3], |p| p.out_val)
    }

    fn quat_rotation(&self, t: f32) -> [i32; 3] {
        let keys = &self.euler.points;
        let n = keys.len();
        let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
            return rotator_from_quat(QUAT_IDENTITY);
        };
        let at = |i: usize| rotator_from_quat(quat_from_euler(self.key_euler(i)));
        if n < 2 || t <= first.in_val {
            return at(0);
        }
        if !lt(t, last.in_val) {
            return at(n - 1);
        }
        let Some(i) = (1..n).find(|&i| keys.get(i).is_some_and(|k| t < k.in_val)) else {
            return at(n - 1);
        };
        let (Some(k0), Some(k1)) = (keys.get(i - 1), keys.get(i)) else {
            return at(n - 1);
        };
        let raw = (t - k0.in_val) / (k1.in_val - k0.in_val);
        let alpha = if 1.0 <= raw { 1.0 } else { raw };
        let alpha = if raw < 0.0 { 0.0 } else { alpha };
        let q = slerp_quat(
            quat_from_euler(self.key_euler(i - 1)),
            quat_from_euler(self.key_euler(i)),
            alpha,
        );
        rotator_from_quat(q)
    }

    /// Relative key transform at `t`.
    #[must_use]
    pub fn key_transform(&self, t: f32) -> ([f32; 3], [i32; 3]) {
        let rot = if self.axes.is_empty() && self.use_quat_interpolation {
            self.quat_rotation(t)
        } else {
            rotator_from_euler(self.eval_euler(t))
        };
        (self.eval_position(t), rot)
    }

    /// `InitialTM` from the actor transform as a matrix.
    #[must_use]
    pub fn initial_from_matrix(&self, actor: &Matrix, position: f32) -> Matrix {
        let initial = if self.use_raw_actor_tm {
            *actor
        } else {
            let (p0, r0) = self.key_transform(position);
            matrix_mul(&rigid_inverse(&rotation_translation_matrix(r0, p0)), actor)
        };
        remove_scaling(&initial)
    }

    /// Instance for an unattached actor (`CalcInitialTransform`).
    #[must_use]
    pub fn instance(&self, location: [f32; 3], rotation: [i32; 3], position: f32) -> MoveInstance {
        MoveInstance {
            initial: self
                .initial_from_matrix(&rotation_translation_matrix(rotation, location), position),
            base: None,
        }
    }

    /// Instance for an actor attached to a base with transform `base`.
    #[must_use]
    pub fn instance_with_base(
        &self,
        location: [f32; 3],
        rotation: [i32; 3],
        base: &Matrix,
        position: f32,
    ) -> MoveInstance {
        let base = remove_scaling(base);
        let actor = rotation_translation_matrix(rotation, location);
        let relative = matrix_mul(&actor, &rigid_inverse(&base));
        MoveInstance {
            initial: self.initial_from_matrix(&relative, position),
            base: Some(base),
        }
    }

    /// Reference frame (`GetMoveRefFrame`).
    #[must_use]
    pub fn ref_frame(&self, inst: &MoveInstance) -> Matrix {
        let base = inst.base.unwrap_or(IDENTITY);
        match self.move_frame {
            MoveFrame::World => base,
            MoveFrame::RelativeToInitial => remove_scaling(&matrix_mul(&inst.initial, &base)),
        }
    }

    /// World location and rotation at `t` (`None` for an inactive track).
    /// The rotation is `None` for `IMR_Ignore` (keep the actor's) and
    /// `IMR_LookAtGroup` (not modelled; no shipped track uses it).
    #[must_use]
    pub fn sample(&self, t: f32, inst: &MoveInstance) -> Option<([f32; 3], Option<[i32; 3]>)> {
        if !self.is_active() {
            return None;
        }
        let (lp, lr) = self.key_transform(t);
        let (location, rot) = world_key_transform(lp, lr, &self.ref_frame(inst));
        let rotation = match self.rot_mode {
            RotMode::Keyframed => Some(rot),
            RotMode::Ignore => None,
            RotMode::LookAtGroup => self.look_at_group.is_none().then_some(rot),
        };
        Some((location, rotation))
    }
}

/// `ComputeWorldSpaceKeyTransform` (port of `asamu_ue3::matinee`).
#[must_use]
pub fn world_key_transform(pos: [f32; 3], rot: [i32; 3], frame: &Matrix) -> ([f32; 3], [i32; 3]) {
    let (winding, rem) = winding_and_remainder(rot);
    let r = rotation_translation_matrix(rem, [0.0; 3]);
    let mut m = IDENTITY;
    for (i, row) in m.iter_mut().take(3).enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = r[i][0] * frame[0][j]
                + r[i][1] * frame[1][j]
                + r[i][2] * frame[2][j]
                + frame[3][j] * 0.0;
        }
    }
    let location = transform_point(frame, pos);
    let cleaned = matrix_rotator(&m, true).map(normalize_axis);
    let turns = euler_from_rotator(winding).map(|x| x * 0.002_777_777_8);
    let turned = transform_vector(frame, turns);
    let whole = turned.map(|x| {
        let r = x.round();
        let i = if r.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&r) {
            i32::MIN
        } else {
            r as i32
        };
        i as f32 * 360.0
    });
    let extra = rotator_from_euler(whole);
    (
        location,
        [
            cleaned[0].wrapping_add(extra[0]),
            cleaned[1].wrapping_add(extra[1]),
            cleaned[2].wrapping_add(extra[2]),
        ],
    )
}

// ================================================================== other tracks

/// Keys of a keyed track that fire when moving from `last` to `new`
/// (`UInterpTrackEvent::UpdateTrack` window rules, MATINEE.md).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn fired_keys(
    times: &[f32],
    last: f32,
    new: f32,
    length: f32,
    playing_reverse: bool,
    jump: bool,
    fire_forwards: bool,
    fire_backwards: bool,
    fire_jumping_forwards: bool,
) -> Vec<usize> {
    let backwards = playing_reverse || (jump && new < last);
    let allowed = if jump {
        !backwards && fire_jumping_forwards && fire_forwards
    } else if backwards {
        fire_backwards
    } else {
        fire_forwards
    };
    if !allowed {
        return Vec::new();
    }
    let mut out = Vec::new();
    if backwards {
        let lo = if new == 0.0 { new + -1.0e-4 } else { new };
        for (i, t) in times.iter().enumerate() {
            if lo < *t && *t <= last {
                out.push(i);
            }
        }
    } else {
        let hi = if new == length { new + 1.0e-4 } else { new };
        for (i, t) in times.iter().enumerate() {
            if last <= *t && *t < hi {
                out.push(i);
            }
        }
    }
    out
}

impl DirectorTrack {
    /// Cut in effect at `t` (`GetKeyframeIndex`).
    #[must_use]
    pub fn cut_index(&self, t: f32) -> Option<usize> {
        let first = self.cuts.first()?;
        if !lt(first.time, t) {
            return None;
        }
        let mut i = 0usize;
        loop {
            let next = i + 1;
            match self.cuts.get(next) {
                Some(c) if le(c.time, t) => i = next,
                _ => return Some(i),
            }
        }
    }
}

impl FadeTrack {
    /// Fade amount at `t`, clamped to `[0, 1]`.
    #[must_use]
    pub fn amount_at(&self, t: f32) -> f32 {
        let v = self.curve.eval(t, 0.0);
        let m = if le(1.0, v) { 1.0 } else { v };
        if lt(v, 0.0) { 0.0 } else { m }
    }
}

// ================================================================== playback

/// What one [`Playback::step`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StepResult {
    /// Position before.
    pub from: f32,
    /// Position after.
    pub to: f32,
    /// Wrapped (looping).
    pub wrapped: bool,
    /// Relative move tracks re-base (forwards wrap with `bNoResetOnRewind`).
    pub reset_initial_transforms: bool,
    /// Reached the end and stopped.
    pub finished: bool,
}

/// The inputs pulsed in one update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackInputs {
    /// Play.
    pub play: bool,
    /// Reverse.
    pub reverse: bool,
    /// Stop.
    pub stop: bool,
    /// Pause.
    pub pause: bool,
    /// Change Dir.
    pub change_dir: bool,
}

/// A jump requested by Play.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jump {
    /// New position.
    pub to: f32,
    /// Re-base relative move tracks first.
    pub reset_initial_transforms: bool,
}

/// Whole lengths a looping step unwinds one at a time (the engine's loop);
/// beyond that (only with a hostile play rate or frame time) the remainder
/// is taken directly, so one step stays cheap.
const MAX_WRAP_STEPS: u32 = 64;

/// `SeqAct_Interp` playback state (port of `asamu_ue3::matinee::Playback`).
#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    /// `Position`.
    pub position: f32,
    /// `bIsPlaying`.
    pub playing: bool,
    /// `bPaused`.
    pub paused: bool,
    /// `bReversePlayback`.
    pub reverse: bool,
    /// `InterpLength`.
    pub length: f32,
    /// Settings.
    pub settings: InterpSettings,
}

impl Playback {
    /// Stopped at 0.
    #[must_use]
    pub fn new(length: f32, settings: InterpSettings) -> Playback {
        Playback {
            position: 0.0,
            playing: false,
            paused: false,
            reverse: false,
            length,
            settings,
        }
    }

    /// Play input.
    pub fn play(&mut self) -> Option<Jump> {
        let s = &self.settings;
        let jump = if s.force_start_pos && !self.playing {
            Some(Jump {
                to: s.force_start_position,
                reset_initial_transforms: false,
            })
        } else if s.rewind_on_play && (!self.playing || s.rewind_if_already_playing) {
            Some(Jump {
                to: 0.0,
                reset_initial_transforms: s.no_reset_on_rewind,
            })
        } else {
            None
        };
        if let Some(j) = jump {
            self.position = j.to;
        }
        self.playing = true;
        self.paused = false;
        self.reverse = false;
        jump
    }

    /// True when `inputs` (re)initialise a stopped action.
    #[must_use]
    pub fn needs_init(&self, inputs: PlaybackInputs) -> bool {
        !self.playing && (inputs.play || inputs.reverse || inputs.change_dir)
    }

    /// One input per update, engine precedence.
    pub fn apply_inputs(&mut self, inputs: PlaybackInputs) -> Option<Jump> {
        if self.playing && inputs.pause {
            self.paused = !self.paused;
            None
        } else if inputs.play {
            self.play()
        } else if inputs.reverse {
            self.playing = true;
            self.paused = false;
            self.reverse = true;
            None
        } else if inputs.stop {
            self.stop();
            None
        } else if inputs.change_dir {
            self.playing = true;
            self.paused = false;
            self.reverse = !self.reverse;
            None
        } else {
            None
        }
    }

    /// Stop.
    pub fn stop(&mut self) {
        self.playing = false;
        self.paused = false;
    }

    /// `StepInterp`.
    pub fn step(&mut self, dt: f32) -> StepResult {
        let from = self.position;
        let mut out = StepResult {
            from,
            to: from,
            wrapped: false,
            reset_initial_transforms: false,
            finished: false,
        };
        if !self.playing || self.paused {
            return out;
        }
        let len = self.length;
        let rate = self.settings.play_rate;
        if self.reverse {
            let mut p = from - dt * rate;
            if !le(0.0, p) {
                if self.settings.looping && len > 0.0 {
                    out.wrapped = true;
                    let mut guard = 0u32;
                    while p < 0.0 && guard < MAX_WRAP_STEPS {
                        p += len;
                        guard += 1;
                    }
                    if p < 0.0 {
                        // Only a hostile rate or frame time gets here.
                        p = p.rem_euclid(len);
                    }
                } else {
                    p = 0.0;
                    out.finished = true;
                }
            }
            out.to = p;
        } else {
            let mut p = from + dt * rate;
            if !le(p, len) {
                if self.settings.looping && len > 0.0 {
                    out.wrapped = true;
                    out.reset_initial_transforms = self.settings.no_reset_on_rewind;
                    let mut guard = 0u32;
                    while len < p && guard < MAX_WRAP_STEPS {
                        p -= len;
                        guard += 1;
                    }
                    if len < p {
                        // Only a hostile rate or frame time gets here.
                        p = p.rem_euclid(len);
                    }
                } else {
                    p = len;
                    out.finished = true;
                }
            }
            out.to = p;
        }
        self.position = out.to;
        if out.finished {
            self.stop();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: f32, v: f32, a: f32, l: f32, mode: CurveMode) -> CurvePoint<f32> {
        CurvePoint {
            in_val: t,
            out_val: v,
            arrive: a,
            leave: l,
            mode,
        }
    }

    #[test]
    fn curves_follow_the_engine_rules() {
        let c = InterpCurve {
            points: vec![
                pt(0.0, 0.0, 0.0, 0.0, CurveMode::Linear),
                pt(2.0, 10.0, 0.0, 0.0, CurveMode::CurveUser),
                pt(4.0, 0.0, 0.0, 0.0, CurveMode::Constant),
            ],
            method: InterpMethod::default(),
        };
        assert_eq!(c.eval(-1.0, 9.0), 0.0);
        assert_eq!(c.eval(1.0, 9.0), 5.0);
        // Hermite with zero tangents at the midpoint: half way.
        assert_eq!(c.eval(3.0, 9.0), 5.0);
        assert_eq!(c.eval(5.0, 9.0), 0.0);
        assert_eq!(c.eval(f32::NAN, 9.0), 0.0);
        let empty: InterpCurve<f32> = InterpCurve::default();
        assert_eq!(empty.eval(1.0, 9.0), 9.0);
        // Hand-computed cubic: p0=0, p1=1, tangents 1 → h10*d + h11*d + h01.
        let c = InterpCurve {
            points: vec![
                pt(0.0, 0.0, 1.0, 1.0, CurveMode::CurveUser),
                pt(1.0, 1.0, 1.0, 1.0, CurveMode::CurveUser),
            ],
            method: InterpMethod::default(),
        };
        assert!((c.eval(0.25, 0.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn rotators_and_matrices_round_trip() {
        assert_eq!(rotator_from_euler([0.0, 90.0, -45.0]), [16384, -8192, 0]);
        assert_eq!(normalize_axis(65536 + 100), 100);
        assert_eq!(normalize_axis(40000), 40000 - 65536);
        let (w, r) = winding_and_remainder([0, 65536 * 2 + 5, 0]);
        assert_eq!(w, [0, 131072, 0]);
        assert_eq!(r, [0, 5, 0]);
        let m = rotation_translation_matrix([0, 16384, 0], [1.0, 2.0, 3.0]);
        let p = transform_point(&m, [1.0, 0.0, 0.0]);
        assert!((p[0] - 1.0).abs() < 1e-5 && (p[1] - 3.0).abs() < 1e-5);
        assert_eq!(matrix_rotator(&m, true), [0, 16384, 0]);
        let inv = rigid_inverse(&m);
        let back = transform_point(&inv, p);
        assert!((back[0] - 1.0).abs() < 1e-4 && back[1].abs() < 1e-4);
        let q = quat_from_euler([0.0, 0.0, 90.0]);
        assert_eq!(rotator_from_quat(q), [0, 16384, 0]);
        let mid = slerp_quat(QUAT_IDENTITY, q, 0.5);
        assert_eq!(rotator_from_quat(mid), [0, 8192, 0]);
    }

    #[test]
    fn relative_move_track_starts_at_the_actor() {
        let track = MoveTrack {
            pos: InterpCurve {
                points: vec![
                    CurvePoint {
                        in_val: 0.0,
                        out_val: [0.0; 3],
                        arrive: [0.0; 3],
                        leave: [0.0; 3],
                        mode: CurveMode::Linear,
                    },
                    CurvePoint {
                        in_val: 1.0,
                        out_val: [0.0, 0.0, 100.0],
                        arrive: [0.0; 3],
                        leave: [0.0; 3],
                        mode: CurveMode::Linear,
                    },
                ],
                method: InterpMethod::default(),
            },
            euler: InterpCurve {
                points: vec![CurvePoint::default()],
                method: InterpMethod::default(),
            },
            move_frame: MoveFrame::RelativeToInitial,
            ..MoveTrack::default()
        };
        let inst = track.instance([10.0, 20.0, 30.0], [0, 16384, 0], 0.0);
        let (l0, r0) = track.sample(0.0, &inst).unwrap();
        assert!((l0[0] - 10.0).abs() < 1e-3 && (l0[2] - 30.0).abs() < 1e-3);
        assert_eq!(r0, Some([0, 16384, 0]));
        let (l1, _) = track.sample(1.0, &inst).unwrap();
        assert!((l1[2] - 130.0).abs() < 1e-3);
        // Restarting at the end position keeps the actor where it is.
        let inst2 = track.instance(l1, [0, 16384, 0], 1.0);
        let (l2, _) = track.sample(1.0, &inst2).unwrap();
        assert!((l2[2] - 130.0).abs() < 1e-3);
    }

    #[test]
    fn keyed_windows_and_playback() {
        let times = [0.0, 1.0, 2.0];
        assert_eq!(
            fired_keys(&times, 0.0, 1.5, 2.0, false, false, true, false, false),
            vec![0, 1]
        );
        assert_eq!(
            fired_keys(&times, 1.5, 2.0, 2.0, false, false, true, false, false),
            vec![2]
        );
        assert!(fired_keys(&times, 2.0, 0.5, 2.0, true, false, true, false, false).is_empty());
        assert_eq!(
            fired_keys(&times, 2.0, 0.0, 2.0, true, false, true, true, false),
            vec![0, 1, 2]
        );
        let mut p = Playback::new(
            1.0,
            InterpSettings {
                looping: true,
                ..InterpSettings::default()
            },
        );
        assert!(p.needs_init(PlaybackInputs {
            play: true,
            ..PlaybackInputs::default()
        }));
        p.apply_inputs(PlaybackInputs {
            play: true,
            ..PlaybackInputs::default()
        });
        let s = p.step(1.5);
        assert!(s.wrapped && (s.to - 0.5).abs() < 1e-6);
        p.settings.looping = false;
        let s = p.step(1.0);
        assert!(s.finished && s.to == 1.0 && !p.playing);
        let d = DirectorTrack {
            cuts: vec![
                DirectorCut {
                    time: 1.0,
                    ..DirectorCut::default()
                },
                DirectorCut {
                    time: 2.0,
                    ..DirectorCut::default()
                },
            ],
        };
        assert_eq!(d.cut_index(1.0), None);
        assert_eq!(d.cut_index(1.5), Some(0));
        assert_eq!(d.cut_index(2.0), Some(1));
        let f = FadeTrack {
            curve: InterpCurve {
                points: vec![pt(0.0, 2.0, 0.0, 0.0, CurveMode::Linear)],
                method: InterpMethod::default(),
            },
            persist_fade: false,
        };
        assert_eq!(f.amount_at(0.0), 1.0);
    }

    #[test]
    fn matinee_json_parses_and_indexes() {
        let json = serde_json::json!({
            "format": MATINEE_FORMAT, "version": MATINEE_VERSION, "package": "T",
            "actions": [
                {"path": "T.A", "node": 4, "scope": "level", "interp_data": "T.D",
                 "settings": {"play_rate": 2.0, "looping": true},
                 "bindings": [{"link": 1, "label": "Lift", "group": "Lift",
                               "targets": [{"variable": "T.V", "object": "T.TheWorld.PersistentLevel.Mover"}]}]},
                {"path": "T.P", "node": 9, "scope": "prefab", "interp_data": "T.D"}
            ],
            "interp_data": [{"path": "T.D", "length": 3.0, "groups": [
                {"kind": "group", "name": "Lift", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                      "pos": {"points": [{"in": 0.0, "out": [0.0, 0.0, 0.0], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"}]},
                      "euler": {"points": []}}},
                    {"class": "Engine.InterpTrackAnimControl", "data": {"type": "anim_control", "keys": []}},
                    {"class": "Engine.InterpTrackEvent", "data": {"type": "event", "keys": [{"time": 1.0, "name": "Open"}], "fire_forwards": true}}
                ]}
            ]}]
        });
        let mut set = MatineeSet::default();
        set.add_json(&serde_json::to_vec(&json).unwrap(), 10)
            .unwrap();
        let (a, d) = set.data_of(14).unwrap();
        assert_eq!(a.settings.play_rate, 2.0);
        assert!(a.settings.looping);
        assert_eq!(d.length, 3.0);
        let tracks = &d.groups[0].tracks;
        assert!(matches!(tracks[0].data, TrackData::Move(_)));
        assert!(matches!(tracks[1].data, TrackData::Other));
        assert!(matches!(tracks[2].data, TrackData::Event(_)));
        assert!(set.data_of(19).is_none(), "prefab actions are skipped");
        assert!(
            set.add_json(b"{\"format\":\"x\",\"version\":1}", 0)
                .is_err()
        );
    }
}
