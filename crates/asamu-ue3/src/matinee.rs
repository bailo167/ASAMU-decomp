//! Matinee (`SeqAct_Interp` / `InterpData` / `InterpGroup` / `InterpTrack*`)
//! decoding and evaluation for map packages.
//!
//! Everything Matinee stores in a cooked v868 map is ordinary tagged
//! properties (CONFIRMED: every `InterpData`, group and track of the 12
//! shipped maps decodes with no native tail; see
//! `docs/reverse-engineering/MATINEE.md`):
//!
//! ```text
//! SeqAct_Interp  --VariableLinks["Data"]-->  InterpData { InterpLength, InterpGroups[] }
//!                --VariableLinks[<GroupName>]--> SeqVar_Object { ObjValue = actor }
//! InterpGroup    { GroupName, GroupColor, InterpTracks[], bIsFolder, ... }
//! InterpTrack*   { SubTracks[], curves (InterpCurveFloat/Vector/LinearColor), key arrays }
//! InterpCurve*   { Points[] { InVal, OutVal, ArriveTangent, LeaveTangent, InterpMode }, InterpMethod }
//! ```
//!
//! Track and group *instances* (`InterpGroupInst`, `InterpTrackInst*`) are
//! transient: the engine creates them when the action starts (one group
//! instance per bound actor, director instances per player), so no cooked
//! map contains any. [`MatineeAction::bindings`] records the information
//! those instances are built from.
//!
//! Evaluation follows the native code of the shipped executable (read
//! locally, described in our own words in `MATINEE.md`):
//! - [`InterpCurve::eval`]: key search, constant / linear / cubic Hermite
//!   segments, tangent scaling by segment length, with the same floating-point
//!   operation order (CONFIRMED from the disassembly).
//! - [`InterpCurve::auto_set_tangents`]: automatic tangents (`CurveAuto`,
//!   `CurveAutoClamped`) for float and vector curves, [`clamp_float_tangent`].
//! - [`MoveTrack`]: position / rotation at a time (sub-tracks, lookup keys,
//!   Euler or quaternion rotation), the initial transform and the
//!   world-space key transform.
//! - [`Playback`]: the action's play / reverse / loop / stop stepping.
//!
//! The full decoded data is original game data (object names, actor
//! references, sound and animation names): keep exports local.
//! [`MatineeMap::coverage`] reduces a map to publishable counts.
//!
//! Hostile-input discipline: object graphs are walked with visited sets and
//! depth limits, archetype chains are bounded, malformed values become
//! warnings or defaults (never panics), and per-map budgets
//! ([`MAX_MAP_TRACKS`], [`MAX_MAP_GROUPS`], [`MAX_MAP_ITEMS`],
//! [`MAX_MERGED_VALUES`]) stop a small package from multiplying work and
//! output by sharing one group or track among many lists; lookups are
//! hashed so no step is quadratic in the export count.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::TAU;
use std::sync::Arc;

use serde::Serialize;

use crate::kismet::{ClassDefaults, EdgeKind, KismetGraph, NodeScope};
use crate::level::merge_properties;
use crate::object::{decode_object, export_class_path, qualified_path};
use crate::package::{MAX_OUTER_DEPTH, Package};
use crate::property::{ObjRef, Property, Value};
use crate::schema::{Schema, last_component};
use crate::types::PackageIndex;

/// `format` field of the per-map JSON export.
pub const MATINEE_FORMAT: &str = "asamu-matinee";
/// `version` field of the per-map JSON export (bumped on incompatible changes).
pub const MATINEE_VERSION: u32 = 1;
/// Longest archetype chain followed when merging values.
pub const MAX_ARCHETYPE_DEPTH: usize = 16;
/// Deepest sub-track nesting followed (the engine uses one level).
pub const MAX_SUBTRACK_DEPTH: usize = 4;
/// Most groups decoded per `InterpData`.
pub const MAX_GROUPS: usize = 4096;
/// Most tracks decoded per group (sub-tracks included).
pub const MAX_TRACKS: usize = 4096;
/// Most warnings kept per map (the rest are counted).
pub const MAX_WARNINGS: usize = 512;
/// Most notes kept per track or `InterpData` (the last one says more were
/// dropped).
pub const MAX_LOCAL_WARNINGS: usize = 64;
/// Most track decodes per map, sub-tracks included (the shipped maps need
/// 723). A track listed by several groups is decoded once per listing, so
/// this bounds the work a hostile package can multiply.
pub const MAX_MAP_TRACKS: usize = 65_536;
/// Most group decodes per map (the shipped maps need 242).
pub const MAX_MAP_GROUPS: usize = 16_384;
/// Most list items (curve keys, discrete keys, anim sets, bound targets)
/// decoded per map (the shipped maps hold a few thousand).
pub const MAX_MAP_ITEMS: usize = 1 << 22;
/// Most property values copied out of archetypes per map (prefab merges;
/// the shipped maps copy on the order of 10,000, all in AG-StarHaven).
pub const MAX_MERGED_VALUES: usize = 1 << 24;

// ================================================================== curves

/// `EInterpCurveMode` (enumerator order CONFIRMED from `Core.Object`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveMode {
    /// `CIM_Linear` (0).
    Linear,
    /// `CIM_CurveAuto` (1).
    CurveAuto,
    /// `CIM_Constant` (2).
    Constant,
    /// `CIM_CurveUser` (3).
    CurveUser,
    /// `CIM_CurveBreak` (4).
    CurveBreak,
    /// `CIM_CurveAutoClamped` (5).
    CurveAutoClamped,
}

impl CurveMode {
    /// All modes in enumerator order.
    pub const ALL: [CurveMode; 6] = [
        CurveMode::Linear,
        CurveMode::CurveAuto,
        CurveMode::Constant,
        CurveMode::CurveUser,
        CurveMode::CurveBreak,
        CurveMode::CurveAutoClamped,
    ];

    /// Mode with enumerator index `i`.
    pub fn from_index(i: u8) -> Option<CurveMode> {
        Self::ALL.get(usize::from(i)).copied()
    }

    /// Mode from its enumerator name (`CIM_CurveUser`), case-insensitive.
    pub fn from_name(name: &str) -> Option<CurveMode> {
        Self::ALL
            .iter()
            .copied()
            .find(|m| m.enum_name().eq_ignore_ascii_case(name))
    }

    /// Enumerator name.
    pub fn enum_name(self) -> &'static str {
        match self {
            CurveMode::Linear => "CIM_Linear",
            CurveMode::CurveAuto => "CIM_CurveAuto",
            CurveMode::Constant => "CIM_Constant",
            CurveMode::CurveUser => "CIM_CurveUser",
            CurveMode::CurveBreak => "CIM_CurveBreak",
            CurveMode::CurveAutoClamped => "CIM_CurveAutoClamped",
        }
    }

    /// True for the modes whose tangents are computed automatically.
    pub fn is_auto(self) -> bool {
        matches!(self, CurveMode::CurveAuto | CurveMode::CurveAutoClamped)
    }

    /// True for the cubic modes (everything except linear and constant).
    pub fn is_curve(self) -> bool {
        !matches!(self, CurveMode::Linear | CurveMode::Constant)
    }
}

/// `EInterpMethodType` (enumerator order CONFIRMED from `Core.Object`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterpMethod {
    /// `IMT_UseFixedTangentEvalAndNewAutoTangents` (0, the struct default).
    #[default]
    FixedTangentEvalAndNewAutoTangents,
    /// `IMT_UseFixedTangentEval` (1).
    FixedTangentEval,
    /// `IMT_UseBrokenTangentEval` (2): tangents are not scaled by the
    /// segment length.
    BrokenTangentEval,
}

impl InterpMethod {
    /// Method with enumerator index `i`.
    pub fn from_index(i: u8) -> Option<InterpMethod> {
        match i {
            0 => Some(InterpMethod::FixedTangentEvalAndNewAutoTangents),
            1 => Some(InterpMethod::FixedTangentEval),
            2 => Some(InterpMethod::BrokenTangentEval),
            _ => None,
        }
    }

    /// Method from its enumerator name, case-insensitive.
    pub fn from_name(name: &str) -> Option<InterpMethod> {
        [
            "IMT_UseFixedTangentEvalAndNewAutoTangents",
            "IMT_UseFixedTangentEval",
            "IMT_UseBrokenTangentEval",
        ]
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .and_then(|i| u8::try_from(i).ok())
        .and_then(InterpMethod::from_index)
    }
}

/// A value an interpolation curve can hold: a fixed number of `f32`
/// components evaluated independently.
pub trait CurveValue: Copy + PartialEq + std::fmt::Debug {
    /// Number of components.
    const DIM: usize;
    /// All components zero.
    fn zero() -> Self;
    /// Component `i` (0 when out of range).
    fn get(&self, i: usize) -> f32;
    /// Set component `i` (ignored when out of range).
    fn set(&mut self, i: usize, v: f32);
    /// Default of a curve point member that is not stored (the UnrealScript
    /// struct default; `LinearColor` defaults to alpha 1).
    fn point_default() -> Self {
        Self::zero()
    }

    /// Apply `f` to every component.
    fn map(self, mut f: impl FnMut(f32) -> f32) -> Self {
        let mut out = self;
        for i in 0..Self::DIM {
            out.set(i, f(self.get(i)));
        }
        out
    }

    /// Combine two values component by component.
    fn zip(self, other: Self, mut f: impl FnMut(f32, f32) -> f32) -> Self {
        let mut out = self;
        for i in 0..Self::DIM {
            out.set(i, f(self.get(i), other.get(i)));
        }
        out
    }
}

impl CurveValue for f32 {
    const DIM: usize = 1;
    fn zero() -> Self {
        0.0
    }
    fn get(&self, i: usize) -> f32 {
        if i == 0 { *self } else { 0.0 }
    }
    fn set(&mut self, i: usize, v: f32) {
        if i == 0 {
            *self = v;
        }
    }
}

macro_rules! array_curve_value {
    ($n:literal) => {
        impl CurveValue for [f32; $n] {
            const DIM: usize = $n;
            fn zero() -> Self {
                [0.0; $n]
            }
            fn get(&self, i: usize) -> f32 {
                self.as_slice().get(i).copied().unwrap_or(0.0)
            }
            fn set(&mut self, i: usize, v: f32) {
                if let Some(c) = self.as_mut_slice().get_mut(i) {
                    *c = v;
                }
            }
        }
    };
}

array_curve_value!(2);
array_curve_value!(3);

/// `LinearColor` curve value (R, G, B, A).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LinearColor(pub [f32; 4]);

impl CurveValue for LinearColor {
    const DIM: usize = 4;
    fn zero() -> Self {
        LinearColor([0.0; 4])
    }
    fn get(&self, i: usize) -> f32 {
        self.0.get(i).copied().unwrap_or(0.0)
    }
    fn set(&mut self, i: usize, v: f32) {
        if let Some(c) = self.0.get_mut(i) {
            *c = v;
        }
    }
    fn point_default() -> Self {
        // Core.Object.InterpCurvePointLinearColor defaults: (0, 0, 0, 1).
        LinearColor([0.0, 0.0, 0.0, 1.0])
    }
}

/// One key of an interpolation curve (`InterpCurvePoint*`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CurvePoint<T> {
    /// `InVal` (the key time for Matinee curves).
    #[serde(rename = "in")]
    pub in_val: f32,
    /// `OutVal`.
    #[serde(rename = "out")]
    pub out_val: T,
    /// `ArriveTangent`.
    #[serde(rename = "arrive")]
    pub arrive_tangent: T,
    /// `LeaveTangent`.
    #[serde(rename = "leave")]
    pub leave_tangent: T,
    /// `InterpMode` (governs the segment that starts at this key).
    pub mode: CurveMode,
}

impl<T: CurveValue> CurvePoint<T> {
    /// A key with zero tangents.
    pub fn new(in_val: f32, out_val: T, mode: CurveMode) -> Self {
        CurvePoint {
            in_val,
            out_val,
            arrive_tangent: T::zero(),
            leave_tangent: T::zero(),
            mode,
        }
    }

    /// A key with explicit tangents.
    pub fn with_tangents(in_val: f32, out_val: T, arrive: T, leave: T, mode: CurveMode) -> Self {
        CurvePoint {
            in_val,
            out_val,
            arrive_tangent: arrive,
            leave_tangent: leave,
            mode,
        }
    }
}

/// An interpolation curve (`InterpCurveFloat`, `InterpCurveVector`, ...).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterpCurve<T> {
    /// Keys, in stored order (the editor keeps them sorted by `in_val`).
    pub points: Vec<CurvePoint<T>>,
    /// `InterpMethod`.
    pub method: InterpMethod,
}

impl<T> Default for InterpCurve<T> {
    fn default() -> Self {
        InterpCurve {
            points: Vec::new(),
            method: InterpMethod::default(),
        }
    }
}

/// Cubic Hermite blend of one component, in the operation order of the
/// shipped executable (CONFIRMED from the disassembly of
/// `FInterpCurve<float>::Eval`):
/// `((h00·p0 + h10·t0) + h11·t1) + h01·p1` with `a2 = a·a`, `a3 = a·a2`,
/// `h00 = (2a3 − 3a2) + 1`, `h10 = (a3 − 2a2) + a`, `h11 = a3 − a2`,
/// `h01 = 3a2 − 2a3`.
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

/// `max(1e-4, x)` the way the native tangent code computes it: in double
/// precision with `maxsd` semantics (a NaN `x` propagates), then rounded
/// back to `f32`.
fn max_small_sd(x: f32) -> f32 {
    let v = f64::from(x);
    let small = 1.0e-4_f64;
    (if small > v { small } else { v }) as f32
}

/// [`max_small_sd`] with the comparison the float curve code uses
/// (`1e-4 <= x ? x : 1e-4`, so a NaN `x` becomes `1e-4`).
fn max_small_le(x: f32) -> f32 {
    let v = f64::from(x);
    let small = 1.0e-4_f64;
    (if small <= v { v } else { small }) as f32
}

/// `a < b` (false when either is NaN), spelled out so that negations keep
/// the native code's NaN behaviour visible.
fn lt(a: f32, b: f32) -> bool {
    a.partial_cmp(&b) == Some(std::cmp::Ordering::Less)
}

/// `a <= b` (false when either is NaN).
fn le(a: f32, b: f32) -> bool {
    matches!(
        a.partial_cmp(&b),
        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
    )
}

/// The clamped automatic tangent of a key (`FClampFloatTangent`, CONFIRMED
/// from the disassembly; described in `MATINEE.md`).
///
/// A key that is a local extreme of its neighbours gets a flat tangent. In
/// between, the tangent is the average slope `(next − prev) / (tn − tp)`,
/// blended towards the one-sided slope when the key's value lies in the
/// lower or upper third of the neighbours' value range, and clamped so it
/// never overshoots that one-sided slope.
pub fn clamp_float_tangent(
    prev_val: f32,
    prev_time: f32,
    cur_val: f32,
    cur_time: f32,
    next_val: f32,
    next_time: f32,
) -> f32 {
    let cur_to_next = next_val - cur_val;
    let prev_to_cur = cur_val - prev_val;
    // Local maximum or minimum: flat.
    if !lt(prev_to_cur, 0.0) && !lt(0.0, cur_to_next) {
        return 0.0;
    }
    if !lt(0.0, prev_to_cur) && !lt(cur_to_next, 0.0) {
        return 0.0;
    }
    let prev_to_next_time = max_small_sd(next_time - prev_time);
    let prev_to_cur_time = max_small_sd(cur_time - prev_time);
    let cur_to_next_time = max_small_sd(next_time - cur_time);
    let prev_to_next = next_val - prev_val;
    let cur_to_next_slope = cur_to_next / cur_to_next_time;
    let prev_to_cur_slope = prev_to_cur / prev_to_cur_time;
    let prev_to_next_slope = prev_to_next / prev_to_next_time;
    let height_alpha = prev_to_cur / prev_to_next;
    const LOWER: f32 = 0.333;
    const UPPER: f32 = 0.667;
    // `mask ? a : b` with the SSE compare semantics of the native code.
    let pick = |keep_first: bool, a: f32, b: f32| if keep_first { a } else { b };
    let rising = prev_to_next > 0.0;
    let mut out = prev_to_next_slope;
    if LOWER > height_alpha {
        let blend = height_alpha / -LOWER + 1.0;
        let lerped = (prev_to_cur_slope - prev_to_next_slope) * blend + prev_to_next_slope;
        out = if rising {
            pick(prev_to_next_slope <= lerped, prev_to_next_slope, lerped)
        } else {
            pick(lerped <= prev_to_next_slope, prev_to_next_slope, lerped)
        };
    }
    if height_alpha > UPPER {
        let blend = (height_alpha + -UPPER) / LOWER;
        let lerped = (cur_to_next_slope - prev_to_next_slope) * blend + prev_to_next_slope;
        out = if rising {
            pick(out <= lerped, out, lerped)
        } else {
            pick(lerped <= out, out, lerped)
        };
    }
    out
}

/// Curve values whose automatic tangents follow verified native code.
pub trait AutoTangentValue: CurveValue {
    /// True when the non-clamped tangent multiplies by the reciprocal of the
    /// time span instead of dividing (the vector instantiation does, the
    /// float one divides; CONFIRMED from the disassembly).
    const RECIPROCAL_SPAN: bool;
}

impl AutoTangentValue for f32 {
    const RECIPROCAL_SPAN: bool = false;
}

impl AutoTangentValue for [f32; 3] {
    const RECIPROCAL_SPAN: bool = true;
}

impl<T: CurveValue> InterpCurve<T> {
    /// A curve over `points` with the default method.
    pub fn new(points: Vec<CurvePoint<T>>) -> Self {
        InterpCurve {
            points,
            method: InterpMethod::default(),
        }
    }

    /// Value at `t` (`default` when the curve has no keys).
    pub fn eval(&self, t: f32, default: T) -> T {
        self.eval_indexed(t, default).0
    }

    /// Value at `t` and the index of the key that starts the segment used
    /// (`None` when the curve has no keys).
    ///
    /// Semantics (CONFIRMED from `FInterpCurve<float>::Eval` and
    /// `FInterpCurve<FVector>::Eval`):
    /// - no keys: `default`;
    /// - one key, or `t` at or before the first key: the first key's value;
    /// - `t` at or after the last key (or NaN): the last key's value;
    /// - otherwise the first key `i` with `t < in[i]` closes the segment
    ///   `[i−1, i]`. A zero or negative span, or a `Constant` start key,
    ///   holds the start value; `Linear` interpolates; every other mode is a
    ///   cubic Hermite segment using the start key's leave tangent and the
    ///   end key's arrive tangent, both multiplied by the span unless the
    ///   curve uses [`InterpMethod::BrokenTangentEval`].
    pub fn eval_indexed(&self, t: f32, default: T) -> (T, Option<usize>) {
        let n = self.points.len();
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return (default, None);
        };
        if n < 2 || t <= first.in_val {
            return (first.out_val, Some(0));
        }
        if !lt(t, last.in_val) {
            return (last.out_val, Some(n - 1));
        }
        for (i, pair) in self.points.windows(2).enumerate() {
            let [p0, p1] = pair else { continue };
            if t < p1.in_val {
                return (self.segment(p0, p1, t), Some(i));
            }
        }
        (last.out_val, Some(n - 1))
    }

    /// Value inside the segment `p0 → p1` at `t` (`p0.in ≤ t < p1.in`).
    fn segment(&self, p0: &CurvePoint<T>, p1: &CurvePoint<T>, t: f32) -> T {
        let diff = p1.in_val - p0.in_val;
        if !lt(0.0, diff) || p0.mode == CurveMode::Constant {
            return p0.out_val;
        }
        let alpha = (t - p0.in_val) / diff;
        if p0.mode == CurveMode::Linear {
            return p0.out_val.zip(p1.out_val, |a, b| alpha * (b - a) + a);
        }
        let broken = self.method == InterpMethod::BrokenTangentEval;
        let mut out = p0.out_val;
        for c in 0..T::DIM {
            let (t0, t1) = if broken {
                (p0.leave_tangent.get(c), p1.arrive_tangent.get(c))
            } else {
                (
                    p0.leave_tangent.get(c) * diff,
                    diff * p1.arrive_tangent.get(c),
                )
            };
            out.set(
                c,
                hermite(p0.out_val.get(c), t0, p1.out_val.get(c), t1, alpha),
            );
        }
        out
    }

    /// `(first key time, last key time)`, or `(0, 0)` without keys.
    pub fn in_range(&self) -> (f32, f32) {
        match (self.points.first(), self.points.last()) {
            (Some(a), Some(b)) => (a.in_val, b.in_val),
            _ => (0.0, 0.0),
        }
    }
}

impl<T: AutoTangentValue> InterpCurve<T> {
    /// Recompute the tangents of every automatic key (`CurveAuto`,
    /// `CurveAutoClamped`) the way the editor does before saving
    /// (`FInterpCurve::AutoSetTangents`, CONFIRMED for float and vector
    /// curves). Runtime evaluation only reads stored tangents, so this is
    /// needed only when keys are edited.
    ///
    /// - First key: an automatic key (or the only key) gets a zero leave
    ///   tangent; its arrive tangent is kept.
    /// - Last key: an automatic key gets a zero arrive tangent.
    /// - Inner automatic key between two cubic neighbours: both tangents
    ///   become the same slope. With the default method it is the clamped
    ///   tangent ([`clamp_float_tangent`]) scaled by `1 − tension` for
    ///   `CurveAutoClamped`, else `((v − vp) + (vn − v)) · (1 − tension) /
    ///   max(1e-4, tn − tp)`; with the older methods it is
    ///   `((v − vp) + (vn − v)) · 0.5 · (1 − tension)`.
    /// - Inner automatic key next to a `Constant` key (its own or the
    ///   previous key's mode): both tangents zero. Next to a `Linear` key:
    ///   unchanged.
    pub fn auto_set_tangents(&mut self, tension: f32) {
        let n = self.points.len();
        let one_minus = 1.0 - tension;
        // (1 − tension) · 0.5 (both instantiations; the product commutes).
        let half = one_minus * 0.5;
        for i in 0..n {
            let Some(cur) = self.points.get(i).copied() else {
                continue;
            };
            let mut arrive = cur.arrive_tangent;
            let mut leave = cur.leave_tangent;
            if i == 0 {
                if n < 2 || cur.mode.is_auto() {
                    leave = T::zero();
                }
            } else if i + 1 >= n {
                if cur.mode.is_auto() {
                    arrive = T::zero();
                }
            } else if cur.mode.is_auto() {
                let (Some(prev), Some(next)) = (
                    self.points.get(i - 1).copied(),
                    self.points.get(i + 1).copied(),
                ) else {
                    continue;
                };
                if prev.mode.is_curve() && cur.mode.is_curve() {
                    let tangent = if self.method == InterpMethod::FixedTangentEvalAndNewAutoTangents
                    {
                        if cur.mode == CurveMode::CurveAutoClamped {
                            let mut v = T::zero();
                            for c in 0..T::DIM {
                                v.set(
                                    c,
                                    clamp_float_tangent(
                                        prev.out_val.get(c),
                                        prev.in_val,
                                        cur.out_val.get(c),
                                        cur.in_val,
                                        next.out_val.get(c),
                                        next.in_val,
                                    ) * one_minus,
                                );
                            }
                            v
                        } else if T::RECIPROCAL_SPAN {
                            let inv = 1.0 / max_small_sd(next.in_val - prev.in_val);
                            let mut v = T::zero();
                            for c in 0..T::DIM {
                                let (p, x, q) =
                                    (prev.out_val.get(c), cur.out_val.get(c), next.out_val.get(c));
                                v.set(c, inv * (((q - x) + (x - p)) * one_minus));
                            }
                            v
                        } else {
                            let span = max_small_le(next.in_val - prev.in_val);
                            let mut v = T::zero();
                            for c in 0..T::DIM {
                                let (p, x, q) =
                                    (prev.out_val.get(c), cur.out_val.get(c), next.out_val.get(c));
                                v.set(c, (((x - p) + (q - x)) * one_minus) / span);
                            }
                            v
                        }
                    } else {
                        let mut v = T::zero();
                        for c in 0..T::DIM {
                            let (p, x, q) =
                                (prev.out_val.get(c), cur.out_val.get(c), next.out_val.get(c));
                            v.set(c, ((x - p) + (q - x)) * half);
                        }
                        v
                    };
                    arrive = tangent;
                    leave = tangent;
                } else if prev.mode == CurveMode::Constant || cur.mode == CurveMode::Constant {
                    arrive = T::zero();
                    leave = T::zero();
                }
            }
            if let Some(p) = self.points.get_mut(i) {
                p.arrive_tangent = arrive;
                p.leave_tangent = leave;
            }
        }
    }
}

// ================================================================== rotations

/// Rotator units per degree used by `FRotator::MakeFromEuler`
/// (`65536 / 360` rounded to `f32`; CONFIRMED constant in the executable).
pub const DEG_TO_ROT: f32 = 182.044_45;
/// Degrees per rotator unit used by `FRotator::Euler` (`360 / 65536`).
pub const ROT_TO_DEG: f32 = 0.005_493_164;

/// `FRotator::MakeFromEuler`: Euler degrees `(roll, pitch, yaw)` (Matinee's
/// `EulerTrack` layout, X = roll, Y = pitch, Z = yaw) to a rotator
/// `[pitch, yaw, roll]`, each truncated toward zero (CONFIRMED).
pub fn rotator_from_euler(e: [f32; 3]) -> [i32; 3] {
    let t = |x: f32| -> i32 {
        let v = x * DEG_TO_ROT;
        // `cvttss2si`: truncation; out of range or NaN gives i32::MIN.
        if v.is_nan() || v >= 2_147_483_648.0 || v < -2_147_483_648.0 {
            i32::MIN
        } else {
            v as i32
        }
    };
    [t(e[1]), t(e[2]), t(e[0])]
}

/// `FRotator::Euler`: rotator `[pitch, yaw, roll]` to Euler degrees
/// `(roll, pitch, yaw)` (CONFIRMED).
pub fn euler_from_rotator(r: [i32; 3]) -> [f32; 3] {
    [
        r[2] as f32 * ROT_TO_DEG,
        r[0] as f32 * ROT_TO_DEG,
        r[1] as f32 * ROT_TO_DEG,
    ]
}

/// One rotator component normalized to `[-32768, 32767]`.
pub fn normalize_axis(a: i32) -> i32 {
    let low = a & 0xFFFF;
    if low < 0x8000 { low } else { low - 0x10000 }
}

/// `FRotator::GetWindingAndRemainder`: `(winding, remainder)` with the
/// remainder normalized to `[-32768, 32767]` and `winding = r − remainder`
/// (CONFIRMED).
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

/// Sine and cosine of a rotator angle from the engine's 16384-entry table
/// (index `(angle >> 2) & 0x3FFF`, cosine a quarter turn later; CONFIRMED
/// indexing). Table values are `sin(i · 2π / 16384)` (TENTATIVE: filled at
/// run time; computed here in double precision and rounded to `f32`).
pub fn table_sin_cos(angle: i32) -> (f32, f32) {
    let entry = |u: u32| (f64::from((u >> 2) & 0x3FFF) * TAU / 16384.0).sin() as f32;
    let bits = angle as u32;
    (entry(bits), entry(bits.wrapping_add(0x4000)))
}

/// A row-vector 4×4 transform (`p' = p · M`, row 3 = translation), `f32`.
pub type Matrix = [[f32; 4]; 4];

/// Identity transform.
pub const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `FRotationTranslationMatrix(rotation, origin)` with table trigonometry.
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

/// Product `a · b` (apply `a`, then `b`).
pub fn matrix_mul(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0f32; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

/// Transform a point (`p · M`).
pub fn transform_point(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        *o = p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j];
    }
    out
}

/// Transform a direction (`v · M` without translation).
pub fn transform_vector(m: &Matrix, v: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        *o = v[0] * m[0][j] + v[1] * m[1][j] + v[2] * m[2][j];
    }
    out
}

/// Inverse of a rotation + translation matrix with orthonormal rows
/// (transpose the rotation, rotate back the negated translation).
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

/// Normalize rows 0..2 to unit length, as the move reference frame does
/// (a row whose squared length is below `1e-8` is left alone).
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

/// `FMatrix::Rotator` (`clean == false`) and `GetCleanedUpRotator`
/// (`clean == true`): the rotator of a rotation matrix. Pitch is
/// `atan2(M02, sqrt(M00² + M01²))`, yaw `atan2(M01, M00)`, and roll is
/// measured against the Y axis of the rotation rebuilt from that pitch and
/// yaw (table trigonometry); each angle is scaled by `32768 / π` (the
/// product in `f32`, the division in double precision), rounded with
/// `roundf` and truncated to an integer. The cleaned variant flushes every
/// `atan2` input below `1e-5` in magnitude to zero first. CONFIRMED from the
/// disassembly (`atan2f`/`roundf` are the C library's; Rust's `f32` methods
/// are assumed to agree).
pub fn matrix_rotator(m: &Matrix, clean: bool) -> [i32; 3] {
    let flush = |x: f32| if clean && x.abs() < 1.0e-5 { 0.0 } else { x };
    let to_units = |rad: f32| -> i32 {
        let v = (f64::from(rad * 32768.0) / std::f64::consts::PI) as f32;
        let r = v.round();
        // `cvttss2si`: NaN or out of range gives i32::MIN.
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
    // Roll 0: sine and cosine of zero straight from the table.
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

/// A quaternion `(x, y, z, w)`.
pub type Quat = [f32; 4];

/// The identity quaternion.
pub const QUAT_IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];

/// `FQuat(const FMatrix&)`: quaternion of a rotation matrix (trace method;
/// identity when every rotation element is below `1e-4` in magnitude).
/// CONFIRMED structure from the decompilation.
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

/// `FQuat::MakeFromEuler`: in this build the quaternion of the rotation
/// matrix of `FRotator::MakeFromEuler(e)` (table trigonometry), CONFIRMED.
pub fn quat_from_euler(e: [f32; 3]) -> Quat {
    quat_from_matrix(&rotation_translation_matrix(
        rotator_from_euler(e),
        [0.0; 3],
    ))
}

/// `SlerpQuat` (CONFIRMED from the disassembly): with `c` the dot product
/// (summed x, y, z, w in order) and `|c|` its magnitude, the weights are
/// `1 − α` and `α` when `|c| ≥ 0.9999`, else `sin((1 − α)ω)/sin ω` and
/// `sin(αω)/sin ω` with `ω = acos(min(|c|, 1))`; the second weight is negated
/// when `c < 0` (shortest path). The result is not normalized.
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

/// `FQuatRotationTranslationMatrix(q, 0)`: rotation matrix of a quaternion
/// (row-vector convention; CONFIRMED element formulas).
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

/// `FRotator(const FQuat&)`: `FMatrix::Rotator` of the quaternion's matrix.
pub fn rotator_from_quat(q: Quat) -> [i32; 3] {
    matrix_rotator(&quat_matrix(q), false)
}

/// Rotation facing along `dir` (`FVector::Rotation`; TENTATIVE scaling and
/// rounding, used only by look-at move tracks, which no shipped map uses).
pub fn direction_rotator(dir: [f32; 3]) -> [i32; 3] {
    let units = |rad: f32| -> i32 {
        let v = (rad * 32768.0 / std::f32::consts::PI).round();
        if v.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&v) {
            0
        } else {
            v as i32
        }
    };
    let yaw = units(dir[1].atan2(dir[0]));
    let pitch = units(dir[2].atan2((dir[0] * dir[0] + dir[1] * dir[1]).sqrt()));
    [pitch, yaw, 0]
}

// ================================================================== model

/// `EInterpTrackMoveFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MoveFrame {
    /// `IMF_World` (0, the class default): keys are world (or base) space.
    #[default]
    World,
    /// `IMF_RelativeToInitial` (1): keys are relative to the actor's
    /// transform when the action initialised.
    RelativeToInitial,
}

/// `EInterpTrackMoveRotMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RotMode {
    /// `IMR_Keyframed` (0, the class default).
    #[default]
    Keyframed,
    /// `IMR_LookAtGroup` (1): face the actor of `LookAtGroupName`.
    LookAtGroup,
    /// `IMR_Ignore` (2): leave the rotation alone.
    Ignore,
}

/// `EInterpMoveAxis` of a move sub-track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MoveAxis {
    /// `AXIS_TranslationX` (0).
    #[default]
    TranslationX,
    /// `AXIS_TranslationY` (1).
    TranslationY,
    /// `AXIS_TranslationZ` (2).
    TranslationZ,
    /// `AXIS_RotationX` (3, roll degrees).
    RotationX,
    /// `AXIS_RotationY` (4, pitch degrees).
    RotationY,
    /// `AXIS_RotationZ` (5, yaw degrees).
    RotationZ,
}

impl MoveAxis {
    /// Axis with enumerator index `i`.
    pub fn from_index(i: usize) -> Option<MoveAxis> {
        [
            MoveAxis::TranslationX,
            MoveAxis::TranslationY,
            MoveAxis::TranslationZ,
            MoveAxis::RotationX,
            MoveAxis::RotationY,
            MoveAxis::RotationZ,
        ]
        .get(i)
        .copied()
    }

    /// Enumerator index.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// One key of an `InterpLookupTrack`: when `group` is set, the move key at
/// this index takes its value from that group's actor instead of the curve.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LookupKey {
    /// `Time`.
    pub time: f32,
    /// `GroupName` (`None` = use the curve key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// `InterpTrackMove`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoveTrack {
    /// `PosTrack` (unreal units).
    pub pos: InterpCurve<[f32; 3]>,
    /// `EulerTrack` (degrees: X = roll, Y = pitch, Z = yaw).
    pub euler: InterpCurve<[f32; 3]>,
    /// `LookupTrack`, one entry per key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lookup: Vec<LookupKey>,
    /// `MoveFrame`.
    pub move_frame: MoveFrame,
    /// `RotMode`.
    pub rot_mode: RotMode,
    /// `LookAtGroupName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_at_group: Option<String>,
    /// `LinCurveTension` (lookup-key position tangents).
    pub lin_curve_tension: f32,
    /// `AngCurveTension` (lookup-key rotation tangents).
    pub ang_curve_tension: f32,
    /// `bUseQuatInterpolation`.
    pub use_quat_interpolation: bool,
    /// `bDisableMovement`.
    pub disable_movement: bool,
    /// `bUseRawActorTMforRelativeToInitial`.
    pub use_raw_actor_tm: bool,
    /// Split translation/rotation sub-tracks (`SubTracks`, in stored order).
    /// When present they replace `pos` and `euler`: entries 0..3 drive
    /// X/Y/Z translation and 3..6 roll/pitch/yaw (by position, as the
    /// engine reads them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<MoveAxisTrack>,
}

/// `InterpTrackMoveAxis` (a sub-track of a move track).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoveAxisTrack {
    /// Object path.
    pub path: String,
    /// `MoveAxis`.
    pub axis: MoveAxis,
    /// `FloatTrack`.
    pub curve: InterpCurve<f32>,
    /// `LookupTrack`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lookup: Vec<LookupKey>,
}

/// A curve track bound to a name (property, skeletal control, material or
/// particle parameter, morph node).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NamedCurve<T> {
    /// The name (`PropertyName`, `SkelControlName`, `ParamName`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The curve.
    pub curve: InterpCurve<T>,
}

/// A plain curve track.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurveTrack<T> {
    /// The curve.
    pub curve: InterpCurve<T>,
}

/// `InterpTrackFade`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FadeTrack {
    /// `FloatTrack` (0 = clear, 1 = fully faded).
    pub curve: InterpCurve<f32>,
    /// `bPersistFade`.
    pub persist_fade: bool,
}

/// `InterpTrackFloatMaterialParam` / `InterpTrackVectorMaterialParam`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaterialParamTrack<T> {
    /// `ParamName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// Target materials (`Materials[].TargetMaterial`, then the deprecated
    /// `Material`).
    pub materials: Vec<String>,
    /// The curve.
    pub curve: InterpCurve<T>,
}

/// One `EventTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventKey {
    /// `Time`.
    pub time: f32,
    /// `EventName` (the `SeqAct_Interp` output link of the same name fires).
    pub name: String,
}

/// `InterpTrackEvent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventTrack {
    /// Keys.
    pub keys: Vec<EventKey>,
    /// `bFireEventsWhenForwards`.
    pub fire_forwards: bool,
    /// `bFireEventsWhenBackwards`.
    pub fire_backwards: bool,
    /// `bFireEventsWhenJumpingForwards`.
    pub fire_jumping_forwards: bool,
}

/// One `DirectorTrackCut`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirectorCut {
    /// `Time`.
    pub time: f32,
    /// `TransitionTime` (blend length; 0 = hard cut).
    pub transition_time: f32,
    /// `TargetCamGroup` (the group whose actor becomes the view target;
    /// the director group's own name means the player's camera).
    pub target_group: Option<String>,
    /// `ShotNumber`.
    pub shot: i32,
}

/// `InterpTrackDirector`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirectorTrack {
    /// Cuts.
    pub cuts: Vec<DirectorCut>,
    /// `bSimulateCameraCutsOnClients`.
    pub simulate_cuts_on_clients: bool,
}

/// One `SoundTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SoundKey {
    /// `Time`.
    pub time: f32,
    /// `Volume` (struct default 1).
    pub volume: f32,
    /// `Pitch` (struct default 1).
    pub pitch: f32,
    /// `Sound` (a `SoundCue` path).
    pub sound: Option<String>,
}

/// `InterpTrackSound`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SoundTrack {
    /// Keys.
    pub keys: Vec<SoundKey>,
    /// `VectorTrack` (inherited from `InterpTrackVectorBase`).
    pub curve: InterpCurve<[f32; 3]>,
    /// `bPlayOnReverse`.
    pub play_on_reverse: bool,
    /// `bContinueSoundOnMatineeEnd`.
    pub continue_on_end: bool,
    /// `bSuppressSubtitles`.
    pub suppress_subtitles: bool,
    /// `bTreatAsDialogue`.
    pub treat_as_dialogue: bool,
}

/// One `AnimControlTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimKey {
    /// `StartTime`.
    pub start_time: f32,
    /// `AnimSeqName`.
    pub sequence: Option<String>,
    /// `AnimStartOffset`.
    pub start_offset: f32,
    /// `AnimEndOffset`.
    pub end_offset: f32,
    /// `AnimPlayRate` (struct default 0).
    pub play_rate: f32,
    /// `bLooping`.
    pub looping: bool,
    /// `bReverse`.
    pub reverse: bool,
}

/// `InterpTrackAnimControl`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimControlTrack {
    /// `SlotName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// `AnimSets`.
    pub anim_sets: Vec<String>,
    /// `AnimSeqs`.
    pub keys: Vec<AnimKey>,
    /// `FloatTrack` (slot weight).
    pub weight: InterpCurve<f32>,
    /// `bEnableRootMotion`.
    pub root_motion: bool,
    /// `bSkipAnimNotifiers`.
    pub skip_notifiers: bool,
}

/// A key holding a time and an enumerator (`ToggleTrackKey`,
/// `HeadTrackingKey`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionKey {
    /// `Time`.
    pub time: f32,
    /// The enumerator name (`ETTA_On`, `EHTA_EnableHeadTracking`, ...).
    pub action: String,
}

/// `InterpTrackToggle`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToggleTrack {
    /// `ToggleTrack` keys (`ETTA_Off`, `ETTA_On`, `ETTA_Toggle`, `ETTA_Trigger`).
    pub keys: Vec<ActionKey>,
    /// `bActivateSystemEachUpdate`.
    pub activate_each_update: bool,
    /// `bActivateWithJustAttachedFlag`.
    pub activate_with_just_attached: bool,
    /// `bFireEventsWhenForwards`.
    pub fire_forwards: bool,
    /// `bFireEventsWhenBackwards`.
    pub fire_backwards: bool,
    /// `bFireEventsWhenJumpingForwards`.
    pub fire_jumping_forwards: bool,
}

/// One `VisibilityTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisibilityKey {
    /// `Time`.
    pub time: f32,
    /// `Action` (`EVTA_Hide`, `EVTA_Show`, `EVTA_Toggle`).
    pub action: String,
    /// `ActiveCondition` (`EVTC_Always`, gore conditions).
    pub condition: String,
}

/// `InterpTrackVisibility`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisibilityTrack {
    /// Keys.
    pub keys: Vec<VisibilityKey>,
    /// `bFireEventsWhenForwards`.
    pub fire_forwards: bool,
    /// `bFireEventsWhenBackwards`.
    pub fire_backwards: bool,
    /// `bFireEventsWhenJumpingForwards`.
    pub fire_jumping_forwards: bool,
}

/// One `ParticleReplayTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParticleReplayKey {
    /// `Time`.
    pub time: f32,
    /// `Duration`.
    pub duration: f32,
    /// `ClipIDNumber`.
    pub clip: i32,
}

/// `InterpTrackParticleReplay`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParticleReplayTrack {
    /// `TrackKeys`.
    pub keys: Vec<ParticleReplayKey>,
}

/// One `NotifyTrackKey`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotifyKey {
    /// `Time`.
    pub time: f32,
    /// `Notify` (an `AnimNotify` object).
    pub notify: Option<String>,
}

/// `InterpTrackNotify`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotifyTrack {
    /// `ParentNodeName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_node: Option<String>,
    /// `NotifyTrack` keys.
    pub keys: Vec<NotifyKey>,
}

/// `InterpTrackHeadTracking`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeadTrackingTrack {
    /// `HeadTrackingTrack` keys.
    pub keys: Vec<ActionKey>,
    /// `TrackControllerName`.
    pub controllers: Vec<String>,
}

/// A track class this decoder does not model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnknownTrack {
    /// Names of the effective properties (values are not interpreted).
    pub properties: Vec<String>,
}

/// Decoded track contents, by track class.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrackData {
    /// `InterpTrackMove`.
    Move(MoveTrack),
    /// `InterpTrackMoveAxis` (only outside a move track).
    MoveAxis(MoveAxisTrack),
    /// `InterpTrackEvent`.
    Event(EventTrack),
    /// `InterpTrackDirector`.
    Director(DirectorTrack),
    /// `InterpTrackSound`.
    Sound(SoundTrack),
    /// `InterpTrackFloatProp` (`name` = `PropertyName`).
    FloatProperty(NamedCurve<f32>),
    /// `InterpTrackVectorProp`.
    VectorProperty(NamedCurve<[f32; 3]>),
    /// `InterpTrackColorProp` (a vector curve, X/Y/Z = R/G/B).
    ColorProperty(NamedCurve<[f32; 3]>),
    /// `InterpTrackLinearColorProp`.
    LinearColorProperty(NamedCurve<LinearColor>),
    /// `InterpTrackFade`.
    Fade(FadeTrack),
    /// `InterpTrackSlomo`.
    Slomo(CurveTrack<f32>),
    /// `InterpTrackColorScale`.
    ColorScale(CurveTrack<[f32; 3]>),
    /// `InterpTrackAudioMaster`.
    AudioMaster(CurveTrack<[f32; 3]>),
    /// `InterpTrackSkelControlStrength` (`name` = `SkelControlName`).
    SkelControlStrength(NamedCurve<f32>),
    /// `InterpTrackSkelControlScale`.
    SkelControlScale(NamedCurve<f32>),
    /// `InterpTrackFloatMaterialParam`.
    FloatMaterialParam(MaterialParamTrack<f32>),
    /// `InterpTrackVectorMaterialParam`.
    VectorMaterialParam(MaterialParamTrack<[f32; 3]>),
    /// `InterpTrackFloatParticleParam` (`name` = `ParamName`).
    FloatParticleParam(NamedCurve<f32>),
    /// `InterpTrackMorphWeight` (`name` = `MorphNodeName`).
    MorphWeight(NamedCurve<f32>),
    /// `InterpTrackAnimControl`.
    AnimControl(AnimControlTrack),
    /// `InterpTrackToggle`.
    Toggle(ToggleTrack),
    /// `InterpTrackVisibility`.
    Visibility(VisibilityTrack),
    /// `InterpTrackParticleReplay`.
    ParticleReplay(ParticleReplayTrack),
    /// `InterpTrackNotify`.
    Notify(NotifyTrack),
    /// `InterpTrackHeadTracking`.
    HeadTracking(HeadTrackingTrack),
    /// Another subclass of `InterpTrackFloatBase`.
    FloatBase(CurveTrack<f32>),
    /// Another subclass of `InterpTrackVectorBase`.
    VectorBase(CurveTrack<[f32; 3]>),
    /// Another subclass of `InterpTrackLinearColorBase`.
    LinearColorBase(CurveTrack<LinearColor>),
    /// Not modelled.
    Unknown(UnknownTrack),
}

impl TrackData {
    /// Snake-case variant name (the JSON `type`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            TrackData::Move(_) => "move",
            TrackData::MoveAxis(_) => "move_axis",
            TrackData::Event(_) => "event",
            TrackData::Director(_) => "director",
            TrackData::Sound(_) => "sound",
            TrackData::FloatProperty(_) => "float_property",
            TrackData::VectorProperty(_) => "vector_property",
            TrackData::ColorProperty(_) => "color_property",
            TrackData::LinearColorProperty(_) => "linear_color_property",
            TrackData::Fade(_) => "fade",
            TrackData::Slomo(_) => "slomo",
            TrackData::ColorScale(_) => "color_scale",
            TrackData::AudioMaster(_) => "audio_master",
            TrackData::SkelControlStrength(_) => "skel_control_strength",
            TrackData::SkelControlScale(_) => "skel_control_scale",
            TrackData::FloatMaterialParam(_) => "float_material_param",
            TrackData::VectorMaterialParam(_) => "vector_material_param",
            TrackData::FloatParticleParam(_) => "float_particle_param",
            TrackData::MorphWeight(_) => "morph_weight",
            TrackData::AnimControl(_) => "anim_control",
            TrackData::Toggle(_) => "toggle",
            TrackData::Visibility(_) => "visibility",
            TrackData::ParticleReplay(_) => "particle_replay",
            TrackData::Notify(_) => "notify",
            TrackData::HeadTracking(_) => "head_tracking",
            TrackData::FloatBase(_) => "float_base",
            TrackData::VectorBase(_) => "vector_base",
            TrackData::LinearColorBase(_) => "linear_color_base",
            TrackData::Unknown(_) => "unknown",
        }
    }

    /// `(curve keys, discrete keys)` held by the track itself (not by its
    /// axis sub-tracks).
    pub fn key_counts(&self) -> (usize, usize) {
        match self {
            // Axis sub-tracks are tracks of their own and count there.
            TrackData::Move(m) => (m.pos.points.len() + m.euler.points.len(), 0),
            TrackData::MoveAxis(a) => (a.curve.points.len(), 0),
            TrackData::Event(e) => (0, e.keys.len()),
            TrackData::Director(d) => (0, d.cuts.len()),
            TrackData::Sound(s) => (s.curve.points.len(), s.keys.len()),
            TrackData::FloatProperty(c)
            | TrackData::SkelControlStrength(c)
            | TrackData::SkelControlScale(c)
            | TrackData::FloatParticleParam(c)
            | TrackData::MorphWeight(c) => (c.curve.points.len(), 0),
            TrackData::VectorProperty(c) | TrackData::ColorProperty(c) => (c.curve.points.len(), 0),
            TrackData::LinearColorProperty(c) => (c.curve.points.len(), 0),
            TrackData::Fade(f) => (f.curve.points.len(), 0),
            TrackData::Slomo(c) | TrackData::FloatBase(c) => (c.curve.points.len(), 0),
            TrackData::ColorScale(c) | TrackData::AudioMaster(c) | TrackData::VectorBase(c) => {
                (c.curve.points.len(), 0)
            }
            TrackData::LinearColorBase(c) => (c.curve.points.len(), 0),
            TrackData::FloatMaterialParam(m) => (m.curve.points.len(), 0),
            TrackData::VectorMaterialParam(m) => (m.curve.points.len(), 0),
            TrackData::AnimControl(a) => (a.weight.points.len(), a.keys.len()),
            TrackData::Toggle(t) => (0, t.keys.len()),
            TrackData::Visibility(v) => (0, v.keys.len()),
            TrackData::ParticleReplay(p) => (0, p.keys.len()),
            TrackData::Notify(n) => (0, n.keys.len()),
            TrackData::HeadTracking(h) => (0, h.keys.len()),
            TrackData::Unknown(_) => (0, 0),
        }
    }
}

/// One track (`InterpTrack` subclass instance).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    /// Object path.
    pub path: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified class path.
    pub class: String,
    /// `TrackTitle`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `bDisableTrack`.
    pub disabled: bool,
    /// `ActiveCondition` when not `ETAC_Always`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_condition: Option<String>,
    /// Contents.
    pub data: TrackData,
    /// Sub-tracks not folded into the contents (see [`MoveTrack::axes`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sub_tracks: Vec<Track>,
    /// Decoder notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Group role, from the class hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    /// `InterpGroup`: drives the actors bound to its name.
    Group,
    /// `InterpGroupDirector`: camera cuts, fades, slomo, audio master; bound
    /// to the players at run time.
    Director,
    /// `InterpGroupAI`: drives a pawn.
    Ai,
    /// `InterpGroupCamera`: camera-animation group.
    Camera,
}

impl GroupKind {
    /// Lower-case name.
    pub fn name(self) -> &'static str {
        match self {
            GroupKind::Group => "group",
            GroupKind::Director => "director",
            GroupKind::Ai => "ai",
            GroupKind::Camera => "camera",
        }
    }
}

/// `InterpGroupAI` settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AiGroupInfo {
    /// `StageMarkGroup`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_mark_group: Option<String>,
    /// `SnapToRootBoneLocationWhenFinished`.
    pub snap_to_root_bone: bool,
    /// `bNoEncroachmentCheck`.
    pub no_encroachment_check: bool,
    /// `bDisableWorldCollision`.
    pub disable_world_collision: bool,
}

/// One group of an `InterpData`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterpGroup {
    /// Object path.
    pub path: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified class path.
    pub class: String,
    /// Role.
    pub kind: GroupKind,
    /// `GroupName` (the `SeqAct_Interp` variable link with this label binds
    /// the group's actors).
    pub name: String,
    /// `GroupColor` (R, G, B, A; editor only).
    pub color: [u8; 4],
    /// `bIsFolder` (an editor folder: no instance, no actors).
    pub folder: bool,
    /// `bIsParented` (shown under a folder in the editor).
    pub parented: bool,
    /// `GroupAnimSets`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub anim_sets: Vec<String>,
    /// `InterpGroupAI` settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiGroupInfo>,
    /// Tracks, in `InterpTracks` order.
    pub tracks: Vec<Track>,
}

/// One `InterpData` (Matinee sequence data).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterpDataInfo {
    /// Object path.
    pub path: String,
    /// Export index.
    pub export_index: usize,
    /// Where the object lives (level tree, prefab archetype, detached).
    pub scope: NodeScope,
    /// `InterpLength` in seconds.
    pub length: f32,
    /// `PathBuildTime`.
    pub path_build_time: f32,
    /// `EdSectionStart`, `EdSectionEnd` (editor loop section).
    pub ed_section: [f32; 2],
    /// `bShouldBakeAndPrune`.
    pub bake_and_prune: bool,
    /// Groups, in `InterpGroups` order.
    pub groups: Vec<InterpGroup>,
    /// `SeqAct_Interp` actions linked to this data.
    pub used_by: Vec<String>,
    /// Decoder notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A `CameraAnim` asset: a single camera group played on a player's camera
/// (camera shakes and canned camera moves, started by `SeqAct_PlayCameraAnim`
/// and script rather than by `SeqAct_Interp`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CameraAnimInfo {
    /// Object path.
    pub path: String,
    /// Export index.
    pub export_index: usize,
    /// `AnimLength` in seconds.
    pub length: f32,
    /// `BaseFOV`.
    pub base_fov: f32,
    /// `CameraInterpGroup`, decoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<InterpGroup>,
}

/// Effective `SeqAct_Interp` settings (own value, else archetype, else class
/// default, else zero).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InterpSettings {
    /// `PlayRate` (class default 1).
    pub play_rate: f32,
    /// `bLooping`.
    pub looping: bool,
    /// `bRewindOnPlay`.
    pub rewind_on_play: bool,
    /// `bNoResetOnRewind`.
    pub no_reset_on_rewind: bool,
    /// `bRewindIfAlreadyPlaying`.
    pub rewind_if_already_playing: bool,
    /// `bForceStartPos`.
    pub force_start_pos: bool,
    /// `ForceStartPosition`.
    pub force_start_position: f32,
    /// `bClientSideOnly`.
    pub client_side_only: bool,
    /// `bSkipUpdateIfNotVisible`.
    pub skip_update_if_not_visible: bool,
    /// `bIsSkippable`.
    pub is_skippable: bool,
    /// `bDisableRadioFilter`.
    pub disable_radio_filter: bool,
    /// `bInterpForPathBuilding`.
    pub interp_for_path_building: bool,
    /// The camera-transition flags that are set
    /// (`bAutoStopWhenDirectorChanges`,
    /// `bLockOutgoingCameraOnFirstTransition`,
    /// `bSkipFirstTransitionIfNotFromCamera`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub camera_flags: Vec<String>,
    /// `PreferredSplitScreenNum`.
    pub preferred_split_screen: i32,
    /// `ConstantCameraAnim`.
    pub constant_camera_anim: i32,
    /// `ConstantCameraAnimRate` (class default 4).
    pub constant_camera_anim_rate: f32,
    /// Settings stored on the object itself (the designer's choices).
    pub stored: Vec<String>,
}

impl Default for InterpSettings {
    /// The `SeqAct_Interp` class defaults (CONFIRMED from
    /// `Engine.Default__SeqAct_Interp`: `PlayRate` 1,
    /// `ConstantCameraAnimRate` 4, everything else zero).
    fn default() -> Self {
        InterpSettings {
            play_rate: 1.0,
            looping: false,
            rewind_on_play: false,
            no_reset_on_rewind: false,
            rewind_if_already_playing: false,
            force_start_pos: false,
            force_start_position: 0.0,
            client_side_only: false,
            skip_update_if_not_visible: false,
            is_skippable: false,
            disable_radio_filter: false,
            interp_for_path_building: false,
            camera_flags: Vec::new(),
            preferred_split_screen: 0,
            constant_camera_anim: 0,
            constant_camera_anim_rate: 4.0,
            stored: Vec::new(),
        }
    }
}

/// An object reached through a variable link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoundTarget {
    /// The variable object.
    pub variable: String,
    /// Its class (short name).
    pub variable_class: String,
    /// `FindVarName` when reached through a `SeqVar_Named`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named: Option<String>,
    /// The object the variable holds (actor path), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// That object's class (short name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_class: Option<String>,
}

/// One group variable link of a `SeqAct_Interp`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupBinding {
    /// Variable link index.
    pub link: usize,
    /// Link label (`LinkDesc`, equal to a group name).
    pub label: String,
    /// The matching group of the linked `InterpData` (case-insensitive name
    /// match), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// That group's role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_kind: Option<GroupKind>,
    /// Objects linked (one group instance per actor at run time).
    pub targets: Vec<BoundTarget>,
}

/// A variable link that feeds a property of the action (`PropertyName`
/// set, e.g. a `SeqVar_Float` driving `PlayRate`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PropertyLink {
    /// Variable link index.
    pub link: usize,
    /// Link label.
    pub label: String,
    /// The property fed.
    pub property: String,
    /// Linked variables and their current values.
    pub variables: Vec<LinkedValue>,
}

/// A variable linked to a property link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedValue {
    /// The variable object.
    pub variable: String,
    /// Its class (short name).
    pub variable_class: String,
    /// Its value (`FloatValue`, `IntValue`, ...), if stored or defaulted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// One `SeqAct_Interp` (Matinee action).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatineeAction {
    /// Object path.
    pub path: String,
    /// Export index.
    pub export_index: usize,
    /// Kismet node id (see the Kismet graph export).
    pub node: usize,
    /// Qualified class path.
    pub class: String,
    /// Where the action lives.
    pub scope: NodeScope,
    /// Parent sequence path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_sequence: Option<String>,
    /// `ObjComment`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Linked `InterpData` path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interp_data: Option<String>,
    /// Effective settings.
    pub settings: InterpSettings,
    /// Input link labels (`Play`, `Reverse`, `Stop`, `Pause`, `Change Dir`).
    pub inputs: Vec<String>,
    /// Output link labels (`Completed`, `Reversed`, then one per event name).
    pub outputs: Vec<String>,
    /// Group bindings (variable links other than the data link and the
    /// property links).
    pub bindings: Vec<GroupBinding>,
    /// Property links.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub property_links: Vec<PropertyLink>,
}

/// Per-track-class counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TrackClassCoverage {
    /// Tracks of this class reached from an `InterpData`.
    pub count: usize,
    /// Of those, decoded into a modelled variant.
    pub decoded: usize,
    /// Curve keys held.
    pub curve_keys: usize,
    /// Discrete keys held (events, cuts, sounds, anim keys, ...).
    pub discrete_keys: usize,
}

/// Publishable per-map statistics (counts and class names only).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MatineeCoverage {
    /// `SeqAct_Interp` actions (all scopes).
    pub actions: usize,
    /// Actions in the level tree.
    pub level_actions: usize,
    /// Actions with a linked `InterpData`.
    pub actions_with_data: usize,
    /// `InterpData` exports (all scopes).
    pub interp_data: usize,
    /// `InterpData` in the level tree.
    pub level_interp_data: usize,
    /// `InterpData` used by no action.
    pub unused_interp_data: usize,
    /// `CameraAnim` assets.
    pub camera_anims: usize,
    /// Groups by class (short name).
    pub groups: BTreeMap<String, usize>,
    /// Folder groups.
    pub folder_groups: usize,
    /// Tracks by class (short name), sub-tracks included.
    pub tracks: BTreeMap<String, TrackClassCoverage>,
    /// Tracks reached.
    pub tracks_total: usize,
    /// Tracks decoded into a modelled variant.
    pub tracks_decoded: usize,
    /// Tracks of unmodelled classes.
    pub tracks_unknown: usize,
    /// Disabled tracks.
    pub tracks_disabled: usize,
    /// `InterpTrack` exports not reached from any `InterpData`.
    pub orphan_tracks: usize,
    /// `InterpGroup` exports not reached from any `InterpData`.
    pub orphan_groups: usize,
    /// Track or group instances (`*Inst` classes) stored in the package.
    pub stored_instances: usize,
    /// Curve keys by `InterpMode`.
    pub curve_modes: BTreeMap<String, usize>,
    /// Curves by `InterpMethod` (curves with keys only).
    pub curve_methods: BTreeMap<String, usize>,
    /// Move tracks by `MoveFrame`.
    pub move_frames: BTreeMap<String, usize>,
    /// Move tracks by `RotMode`.
    pub rot_modes: BTreeMap<String, usize>,
    /// Move tracks with `bUseQuatInterpolation`.
    pub quat_interpolation: usize,
    /// Move tracks with `bUseRawActorTMforRelativeToInitial`.
    pub raw_actor_tm: usize,
    /// Move tracks using split axis sub-tracks.
    pub split_move_tracks: usize,
    /// Axis sub-tracks whose `MoveAxis` differs from their position.
    pub axis_order_mismatches: usize,
    /// Lookup keys that name a group.
    pub lookup_group_keys: usize,
    /// Move tracks whose lookup track length differs from the key count.
    pub lookup_length_mismatches: usize,
    /// Group bindings (variable links) of all actions.
    pub bindings: usize,
    /// Property links of all actions.
    pub property_links: usize,
    /// Bindings whose label matches no group of the linked data.
    pub bindings_without_group: usize,
    /// Bound objects (actors) by class (short name).
    pub bound_classes: BTreeMap<String, usize>,
    /// Non-folder, non-director groups of used data with no bound object
    /// in some action that uses the data.
    pub unbound_groups: usize,
    /// Objects whose values were merged from an export archetype.
    pub archetype_merges: usize,
    /// Inherited references that could not be mapped onto the instance.
    pub unremapped_refs: usize,
    /// Objects that failed to decode.
    pub decode_failures: usize,
    /// Warnings recorded.
    pub warnings: usize,
}

/// All Matinee data of one map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatineeMap {
    /// Always [`MATINEE_FORMAT`].
    pub format: String,
    /// Always [`MATINEE_VERSION`].
    pub version: u32,
    /// Package name (file stem).
    pub package: String,
    /// `SeqAct_Interp` actions, in export order.
    pub actions: Vec<MatineeAction>,
    /// `InterpData` objects, in export order.
    pub interp_data: Vec<InterpDataInfo>,
    /// `CameraAnim` assets, in export order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub camera_anims: Vec<CameraAnimInfo>,
    /// Statistics.
    pub coverage: MatineeCoverage,
    /// Group and track exports reached from no `InterpData` (editor
    /// leftovers), by path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub orphans: Vec<String>,
    /// Notes.
    pub warnings: Vec<String>,
}

impl MatineeMap {
    /// The `InterpData` with path `path`.
    pub fn data(&self, path: &str) -> Option<&InterpDataInfo> {
        self.interp_data
            .iter()
            .find(|d| d.path.eq_ignore_ascii_case(path))
    }
}

// ================================================================== values

/// Push a decoder note, keeping at most [`MAX_LOCAL_WARNINGS`] (the last
/// kept note says that more were dropped).
fn note(w: &mut Vec<String>, msg: String) {
    match w.len().cmp(&MAX_LOCAL_WARNINGS.saturating_sub(1)) {
        std::cmp::Ordering::Less => w.push(msg),
        std::cmp::Ordering::Equal => w.push("further notes not kept".to_owned()),
        std::cmp::Ordering::Greater => {}
    }
}

/// Number of values in `v`, nested values included (depth-limited).
fn value_size(v: &Value) -> usize {
    fn count(v: &Value, depth: usize) -> usize {
        if depth > crate::property::MAX_VALUE_DEPTH {
            return 1;
        }
        let inner = match v {
            Value::Array(list) => list
                .iter()
                .map(|x| count(x, depth + 1))
                .fold(0usize, usize::saturating_add),
            Value::Struct { fields, .. } => fields
                .iter()
                .map(|f| count(&f.value, depth + 1))
                .fold(0usize, usize::saturating_add),
            _ => 0,
        };
        inner.saturating_add(1)
    }
    count(v, 0)
}

/// Number of values in `props`, nested values included (depth-limited).
fn value_count(props: &[Property]) -> usize {
    props
        .iter()
        .map(|p| value_size(&p.value))
        .fold(0usize, usize::saturating_add)
}

fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Value> {
    props
        .iter()
        .find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

fn member<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct { fields, .. } => prop(fields, name),
        _ => None,
    }
}

fn as_f32(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f32),
        Value::Byte(b) => Some(f32::from(*b)),
        _ => None,
    }
}

fn as_i32(v: Option<&Value>) -> Option<i32> {
    match v? {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(i32::from(*b)),
        _ => None,
    }
}

fn as_bool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Byte(b) => Some(*b != 0),
        _ => None,
    }
}

fn as_name(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Name(s) | Value::Enum(s) if !s.eq_ignore_ascii_case("None") && !s.is_empty() => {
            Some(s.clone())
        }
        _ => None,
    }
}

fn as_string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Str(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn as_obj(v: Option<&Value>) -> Option<&ObjRef> {
    match v? {
        Value::Object(o) | Value::Interface(o) if o.index != 0 => Some(o),
        _ => None,
    }
}

fn items(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

fn obj_paths(v: Option<&Value>) -> Vec<String> {
    items(v)
        .iter()
        .filter_map(|x| as_obj(Some(x)).map(|o| o.path.clone()))
        .collect()
}

/// Enumerator index of an enum value stored as a name or as a byte.
fn enum_index(v: Option<&Value>, names: &[&str]) -> Option<usize> {
    match v? {
        Value::Enum(s) | Value::Name(s) => names.iter().position(|n| n.eq_ignore_ascii_case(s)),
        Value::Byte(b) => Some(usize::from(*b)),
        Value::Int(i) => usize::try_from(*i).ok(),
        _ => None,
    }
}

/// Enumerator name of an enum value (falls back to `names[0]` when absent,
/// the zero value, and to the raw number when out of range).
fn enum_name(v: Option<&Value>, names: &[&str]) -> String {
    match v {
        None => names.first().copied().unwrap_or("").to_owned(),
        Some(Value::Enum(s) | Value::Name(s)) => s.clone(),
        Some(other) => match enum_index(Some(other), names) {
            Some(i) => names
                .get(i)
                .map(|s| (*s).to_owned())
                .unwrap_or_else(|| i.to_string()),
            None => names.first().copied().unwrap_or("").to_owned(),
        },
    }
}

fn conv_f32(v: &Value) -> Option<f32> {
    as_f32(Some(v))
}

fn conv_vec3(v: &Value) -> Option<[f32; 3]> {
    if !matches!(v, Value::Struct { .. }) {
        return None;
    }
    let c = |n: &str| as_f32(member(v, n)).unwrap_or(0.0);
    Some([c("X"), c("Y"), c("Z")])
}

fn conv_color(v: &Value) -> Option<LinearColor> {
    if !matches!(v, Value::Struct { .. }) {
        return None;
    }
    let c = |n: &str, d: f32| as_f32(member(v, n)).unwrap_or(d);
    Some(LinearColor([
        c("R", 0.0),
        c("G", 0.0),
        c("B", 0.0),
        c("A", 1.0),
    ]))
}

const CURVE_MODE_NAMES: &[&str] = &[
    "CIM_Linear",
    "CIM_CurveAuto",
    "CIM_Constant",
    "CIM_CurveUser",
    "CIM_CurveBreak",
    "CIM_CurveAutoClamped",
];

const INTERP_METHOD_NAMES: &[&str] = &[
    "IMT_UseFixedTangentEvalAndNewAutoTangents",
    "IMT_UseFixedTangentEval",
    "IMT_UseBrokenTangentEval",
];

/// Decode an `InterpCurve*` struct value. Members that are not stored take
/// the UnrealScript struct defaults (zero values, `CIM_Linear`,
/// `IMT_UseFixedTangentEvalAndNewAutoTangents`; `LinearColor` alpha 1). An
/// unknown curve mode decodes as `CurveUser` (the native evaluator treats
/// every mode other than linear and constant as cubic).
pub fn decode_curve<T: CurveValue>(
    v: Option<&Value>,
    conv: fn(&Value) -> Option<T>,
    warnings: &mut Vec<String>,
    what: &str,
) -> InterpCurve<T> {
    let Some(v) = v else {
        return InterpCurve::default();
    };
    let mut points = Vec::new();
    for (i, p) in items(member(v, "Points")).iter().enumerate() {
        if !matches!(p, Value::Struct { .. }) {
            note(warnings, format!("{what}: point {i} is not a struct"));
            continue;
        }
        let get = |n: &str| member(p, n).and_then(conv).unwrap_or_else(T::point_default);
        let mode = match member(p, "InterpMode") {
            None => CurveMode::Linear,
            Some(m) => match enum_index(Some(m), CURVE_MODE_NAMES)
                .and_then(|i| u8::try_from(i).ok())
                .and_then(CurveMode::from_index)
            {
                Some(mode) => mode,
                None => {
                    note(
                        warnings,
                        format!("{what}: point {i} has unknown InterpMode {m:?}"),
                    );
                    CurveMode::CurveUser
                }
            },
        };
        points.push(CurvePoint {
            in_val: as_f32(member(p, "InVal")).unwrap_or(0.0),
            out_val: get("OutVal"),
            arrive_tangent: get("ArriveTangent"),
            leave_tangent: get("LeaveTangent"),
            mode,
        });
    }
    let method = match member(v, "InterpMethod") {
        None => InterpMethod::default(),
        Some(m) => match enum_index(Some(m), INTERP_METHOD_NAMES)
            .and_then(|i| u8::try_from(i).ok())
            .and_then(InterpMethod::from_index)
        {
            Some(x) => x,
            None => {
                note(warnings, format!("{what}: unknown InterpMethod {m:?}"));
                InterpMethod::default()
            }
        },
    };
    InterpCurve { points, method }
}

fn decode_lookup(v: Option<&Value>) -> Vec<LookupKey> {
    let Some(v) = v else {
        return Vec::new();
    };
    items(member(v, "Points"))
        .iter()
        .map(|p| LookupKey {
            time: as_f32(member(p, "Time")).unwrap_or(0.0),
            group: as_name(member(p, "GroupName")),
        })
        .collect()
}

// ================================================================== track decoding

/// What a class chain decodes as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackClass {
    Move,
    MoveAxis,
    Event,
    Director,
    Sound,
    FloatProp,
    VectorProp,
    ColorProp,
    LinearColorProp,
    Fade,
    Slomo,
    ColorScale,
    AudioMaster,
    SkelControlStrength,
    SkelControlScale,
    FloatMaterialParam,
    VectorMaterialParam,
    FloatParticleParam,
    MorphWeight,
    AnimControl,
    Toggle,
    Visibility,
    ParticleReplay,
    Notify,
    HeadTracking,
    FloatBase,
    VectorBase,
    LinearColorBase,
}

const TRACK_CLASSES: &[(&str, TrackClass)] = &[
    ("interptrackmove", TrackClass::Move),
    ("interptrackmoveaxis", TrackClass::MoveAxis),
    ("interptrackevent", TrackClass::Event),
    ("interptrackdirector", TrackClass::Director),
    ("interptracksound", TrackClass::Sound),
    ("interptrackfloatprop", TrackClass::FloatProp),
    ("interptrackvectorprop", TrackClass::VectorProp),
    ("interptrackcolorprop", TrackClass::ColorProp),
    ("interptracklinearcolorprop", TrackClass::LinearColorProp),
    ("interptrackfade", TrackClass::Fade),
    ("interptrackslomo", TrackClass::Slomo),
    ("interptrackcolorscale", TrackClass::ColorScale),
    ("interptrackaudiomaster", TrackClass::AudioMaster),
    (
        "interptrackskelcontrolstrength",
        TrackClass::SkelControlStrength,
    ),
    ("interptrackskelcontrolscale", TrackClass::SkelControlScale),
    (
        "interptrackfloatmaterialparam",
        TrackClass::FloatMaterialParam,
    ),
    (
        "interptrackvectormaterialparam",
        TrackClass::VectorMaterialParam,
    ),
    (
        "interptrackfloatparticleparam",
        TrackClass::FloatParticleParam,
    ),
    ("interptrackmorphweight", TrackClass::MorphWeight),
    ("interptrackanimcontrol", TrackClass::AnimControl),
    ("interptracktoggle", TrackClass::Toggle),
    ("interptrackvisibility", TrackClass::Visibility),
    ("interptrackparticlereplay", TrackClass::ParticleReplay),
    ("interptracknotify", TrackClass::Notify),
    ("interptrackheadtracking", TrackClass::HeadTracking),
    ("interptrackfloatbase", TrackClass::FloatBase),
    ("interptrackvectorbase", TrackClass::VectorBase),
    ("interptracklinearcolorbase", TrackClass::LinearColorBase),
];

fn track_class(chain: &[String]) -> Option<TrackClass> {
    chain.iter().find_map(|c| {
        TRACK_CLASSES
            .iter()
            .find(|(n, _)| c.eq_ignore_ascii_case(n))
            .map(|(_, k)| *k)
    })
}

fn group_kind(chain: &[String]) -> Option<GroupKind> {
    for c in chain {
        match c.to_ascii_lowercase().as_str() {
            "interpgroupdirector" => return Some(GroupKind::Director),
            "interpgroupai" => return Some(GroupKind::Ai),
            "interpgroupcamera" => return Some(GroupKind::Camera),
            "interpgroup" => return Some(GroupKind::Group),
            _ => {}
        }
    }
    None
}

fn has_class(chain: &[String], name: &str) -> bool {
    chain.iter().any(|c| c.eq_ignore_ascii_case(name))
}

const MOVE_FRAME_NAMES: &[&str] = &["IMF_World", "IMF_RelativeToInitial"];
const ROT_MODE_NAMES: &[&str] = &["IMR_Keyframed", "IMR_LookAtGroup", "IMR_Ignore"];
const MOVE_AXIS_NAMES: &[&str] = &[
    "AXIS_TranslationX",
    "AXIS_TranslationY",
    "AXIS_TranslationZ",
    "AXIS_RotationX",
    "AXIS_RotationY",
    "AXIS_RotationZ",
];
const ACTIVE_CONDITION_NAMES: &[&str] = &["ETAC_Always", "ETAC_GoreEnabled", "ETAC_GoreDisabled"];
const TOGGLE_ACTION_NAMES: &[&str] = &["ETTA_Off", "ETTA_On", "ETTA_Toggle", "ETTA_Trigger"];
const VISIBILITY_ACTION_NAMES: &[&str] = &["EVTA_Hide", "EVTA_Show", "EVTA_Toggle"];
const VISIBILITY_CONDITION_NAMES: &[&str] =
    &["EVTC_Always", "EVTC_GoreEnabled", "EVTC_GoreDisabled"];
const HEAD_TRACKING_ACTION_NAMES: &[&str] =
    &["EHTA_DisableHeadTracking", "EHTA_EnableHeadTracking"];

fn decode_move_axis(path: &str, props: &[Property], w: &mut Vec<String>) -> MoveAxisTrack {
    let axis_idx = enum_index(prop(props, "MoveAxis"), MOVE_AXIS_NAMES).unwrap_or(0);
    let axis = MoveAxis::from_index(axis_idx).unwrap_or_else(|| {
        note(w, format!("{path}: unknown MoveAxis {axis_idx}"));
        MoveAxis::TranslationX
    });
    MoveAxisTrack {
        path: path.to_owned(),
        axis,
        curve: decode_curve(prop(props, "FloatTrack"), conv_f32, w, "FloatTrack"),
        lookup: decode_lookup(prop(props, "LookupTrack")),
    }
}

/// Decode the contents of a track from its effective properties.
fn decode_track_data(
    class: Option<TrackClass>,
    path: &str,
    props: &[Property],
    w: &mut Vec<String>,
) -> TrackData {
    let flag = |n: &str| as_bool(prop(props, n)).unwrap_or(false);
    let fcurve = |n: &str, w: &mut Vec<String>| decode_curve(prop(props, n), conv_f32, w, n);
    let vcurve = |n: &str, w: &mut Vec<String>| decode_curve(prop(props, n), conv_vec3, w, n);
    let ccurve = |n: &str, w: &mut Vec<String>| decode_curve(prop(props, n), conv_color, w, n);
    let named_f = |name: &str, w: &mut Vec<String>| NamedCurve {
        name: as_name(prop(props, name)),
        curve: fcurve("FloatTrack", w),
    };
    let Some(class) = class else {
        return TrackData::Unknown(UnknownTrack {
            properties: props.iter().map(|p| p.name.clone()).collect(),
        });
    };
    match class {
        TrackClass::Move => TrackData::Move(MoveTrack {
            pos: vcurve("PosTrack", w),
            euler: vcurve("EulerTrack", w),
            lookup: decode_lookup(prop(props, "LookupTrack")),
            move_frame: match enum_index(prop(props, "MoveFrame"), MOVE_FRAME_NAMES) {
                Some(1) => MoveFrame::RelativeToInitial,
                Some(0) | None => MoveFrame::World,
                Some(other) => {
                    note(w, format!("{path}: unknown MoveFrame {other}"));
                    MoveFrame::World
                }
            },
            rot_mode: match enum_index(prop(props, "RotMode"), ROT_MODE_NAMES) {
                Some(1) => RotMode::LookAtGroup,
                Some(2) => RotMode::Ignore,
                Some(0) | None => RotMode::Keyframed,
                Some(other) => {
                    note(w, format!("{path}: unknown RotMode {other}"));
                    RotMode::Keyframed
                }
            },
            look_at_group: as_name(prop(props, "LookAtGroupName")),
            lin_curve_tension: as_f32(prop(props, "LinCurveTension")).unwrap_or(0.0),
            ang_curve_tension: as_f32(prop(props, "AngCurveTension")).unwrap_or(0.0),
            use_quat_interpolation: flag("bUseQuatInterpolation"),
            disable_movement: flag("bDisableMovement"),
            use_raw_actor_tm: flag("bUseRawActorTMforRelativeToInitial"),
            axes: Vec::new(),
        }),
        TrackClass::MoveAxis => TrackData::MoveAxis(decode_move_axis(path, props, w)),
        TrackClass::Event => TrackData::Event(EventTrack {
            keys: items(prop(props, "EventTrack"))
                .iter()
                .map(|k| EventKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    name: as_name(member(k, "EventName")).unwrap_or_else(|| "None".to_owned()),
                })
                .collect(),
            fire_forwards: flag("bFireEventsWhenForwards"),
            fire_backwards: flag("bFireEventsWhenBackwards"),
            fire_jumping_forwards: flag("bFireEventsWhenJumpingForwards"),
        }),
        TrackClass::Director => TrackData::Director(DirectorTrack {
            cuts: items(prop(props, "CutTrack"))
                .iter()
                .map(|k| DirectorCut {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    transition_time: as_f32(member(k, "TransitionTime")).unwrap_or(0.0),
                    target_group: as_name(member(k, "TargetCamGroup")),
                    shot: as_i32(member(k, "ShotNumber")).unwrap_or(0),
                })
                .collect(),
            simulate_cuts_on_clients: flag("bSimulateCameraCutsOnClients"),
        }),
        TrackClass::Sound => TrackData::Sound(SoundTrack {
            keys: items(prop(props, "Sounds"))
                .iter()
                .map(|k| SoundKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    volume: as_f32(member(k, "Volume")).unwrap_or(1.0),
                    pitch: as_f32(member(k, "Pitch")).unwrap_or(1.0),
                    sound: as_obj(member(k, "Sound")).map(|o| o.path.clone()),
                })
                .collect(),
            curve: vcurve("VectorTrack", w),
            play_on_reverse: flag("bPlayOnReverse"),
            continue_on_end: flag("bContinueSoundOnMatineeEnd"),
            suppress_subtitles: flag("bSuppressSubtitles"),
            treat_as_dialogue: flag("bTreatAsDialogue"),
        }),
        TrackClass::FloatProp => TrackData::FloatProperty(named_f("PropertyName", w)),
        TrackClass::VectorProp => TrackData::VectorProperty(NamedCurve {
            name: as_name(prop(props, "PropertyName")),
            curve: vcurve("VectorTrack", w),
        }),
        TrackClass::ColorProp => TrackData::ColorProperty(NamedCurve {
            name: as_name(prop(props, "PropertyName")),
            curve: vcurve("VectorTrack", w),
        }),
        TrackClass::LinearColorProp => TrackData::LinearColorProperty(NamedCurve {
            name: as_name(prop(props, "PropertyName")),
            curve: ccurve("LinearColorTrack", w),
        }),
        TrackClass::Fade => TrackData::Fade(FadeTrack {
            curve: fcurve("FloatTrack", w),
            persist_fade: flag("bPersistFade"),
        }),
        TrackClass::Slomo => TrackData::Slomo(CurveTrack {
            curve: fcurve("FloatTrack", w),
        }),
        TrackClass::ColorScale => TrackData::ColorScale(CurveTrack {
            curve: vcurve("VectorTrack", w),
        }),
        TrackClass::AudioMaster => TrackData::AudioMaster(CurveTrack {
            curve: vcurve("VectorTrack", w),
        }),
        TrackClass::SkelControlStrength => {
            TrackData::SkelControlStrength(named_f("SkelControlName", w))
        }
        TrackClass::SkelControlScale => TrackData::SkelControlScale(named_f("SkelControlName", w)),
        TrackClass::FloatMaterialParam | TrackClass::VectorMaterialParam => {
            let mut materials: Vec<String> = items(prop(props, "Materials"))
                .iter()
                .filter_map(|m| as_obj(member(m, "TargetMaterial")).map(|o| o.path.clone()))
                .collect();
            if let Some(o) = as_obj(prop(props, "Material")) {
                materials.push(o.path.clone());
            }
            let param = as_name(prop(props, "ParamName"));
            if class == TrackClass::FloatMaterialParam {
                TrackData::FloatMaterialParam(MaterialParamTrack {
                    param,
                    materials,
                    curve: fcurve("FloatTrack", w),
                })
            } else {
                TrackData::VectorMaterialParam(MaterialParamTrack {
                    param,
                    materials,
                    curve: vcurve("VectorTrack", w),
                })
            }
        }
        TrackClass::FloatParticleParam => TrackData::FloatParticleParam(named_f("ParamName", w)),
        TrackClass::MorphWeight => TrackData::MorphWeight(named_f("MorphNodeName", w)),
        TrackClass::AnimControl => TrackData::AnimControl(AnimControlTrack {
            slot: as_name(prop(props, "SlotName")),
            anim_sets: obj_paths(prop(props, "AnimSets")),
            keys: items(prop(props, "AnimSeqs"))
                .iter()
                .map(|k| AnimKey {
                    start_time: as_f32(member(k, "StartTime")).unwrap_or(0.0),
                    sequence: as_name(member(k, "AnimSeqName")),
                    start_offset: as_f32(member(k, "AnimStartOffset")).unwrap_or(0.0),
                    end_offset: as_f32(member(k, "AnimEndOffset")).unwrap_or(0.0),
                    play_rate: as_f32(member(k, "AnimPlayRate")).unwrap_or(0.0),
                    looping: as_bool(member(k, "bLooping")).unwrap_or(false),
                    reverse: as_bool(member(k, "bReverse")).unwrap_or(false),
                })
                .collect(),
            weight: fcurve("FloatTrack", w),
            root_motion: flag("bEnableRootMotion"),
            skip_notifiers: flag("bSkipAnimNotifiers"),
        }),
        TrackClass::Toggle => TrackData::Toggle(ToggleTrack {
            keys: items(prop(props, "ToggleTrack"))
                .iter()
                .map(|k| ActionKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    action: enum_name(member(k, "ToggleAction"), TOGGLE_ACTION_NAMES),
                })
                .collect(),
            activate_each_update: flag("bActivateSystemEachUpdate"),
            activate_with_just_attached: flag("bActivateWithJustAttachedFlag"),
            fire_forwards: flag("bFireEventsWhenForwards"),
            fire_backwards: flag("bFireEventsWhenBackwards"),
            fire_jumping_forwards: flag("bFireEventsWhenJumpingForwards"),
        }),
        TrackClass::Visibility => TrackData::Visibility(VisibilityTrack {
            keys: items(prop(props, "VisibilityTrack"))
                .iter()
                .map(|k| VisibilityKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    action: enum_name(member(k, "Action"), VISIBILITY_ACTION_NAMES),
                    condition: enum_name(member(k, "ActiveCondition"), VISIBILITY_CONDITION_NAMES),
                })
                .collect(),
            fire_forwards: flag("bFireEventsWhenForwards"),
            fire_backwards: flag("bFireEventsWhenBackwards"),
            fire_jumping_forwards: flag("bFireEventsWhenJumpingForwards"),
        }),
        TrackClass::ParticleReplay => TrackData::ParticleReplay(ParticleReplayTrack {
            keys: items(prop(props, "TrackKeys"))
                .iter()
                .map(|k| ParticleReplayKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    duration: as_f32(member(k, "Duration")).unwrap_or(0.0),
                    clip: as_i32(member(k, "ClipIDNumber")).unwrap_or(0),
                })
                .collect(),
        }),
        TrackClass::Notify => TrackData::Notify(NotifyTrack {
            parent_node: as_name(prop(props, "ParentNodeName")),
            keys: items(prop(props, "NotifyTrack"))
                .iter()
                .map(|k| NotifyKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    notify: as_obj(member(k, "Notify")).map(|o| o.path.clone()),
                })
                .collect(),
        }),
        TrackClass::HeadTracking => TrackData::HeadTracking(HeadTrackingTrack {
            keys: items(prop(props, "HeadTrackingTrack"))
                .iter()
                .map(|k| ActionKey {
                    time: as_f32(member(k, "Time")).unwrap_or(0.0),
                    action: enum_name(member(k, "Action"), HEAD_TRACKING_ACTION_NAMES),
                })
                .collect(),
            controllers: items(prop(props, "TrackControllerName"))
                .iter()
                .filter_map(|v| as_name(Some(v)))
                .collect(),
        }),
        TrackClass::FloatBase => TrackData::FloatBase(CurveTrack {
            curve: fcurve("FloatTrack", w),
        }),
        TrackClass::VectorBase => TrackData::VectorBase(CurveTrack {
            curve: vcurve("VectorTrack", w),
        }),
        TrackClass::LinearColorBase => TrackData::LinearColorBase(CurveTrack {
            curve: ccurve("LinearColorTrack", w),
        }),
    }
}

/// Decode a track from its class chain (lower-case short names, nearest
/// first) and effective properties, without sub-tracks. Public for tests
/// and tools that already hold decoded properties.
pub fn decode_track(path: &str, class: &str, chain: &[String], props: &[Property]) -> Track {
    let mut warnings = Vec::new();
    let data = decode_track_data(track_class(chain), path, props, &mut warnings);
    let condition = enum_name(prop(props, "ActiveCondition"), ACTIVE_CONDITION_NAMES);
    Track {
        path: path.to_owned(),
        export_index: 0,
        class: class.to_owned(),
        title: as_string(prop(props, "TrackTitle")),
        disabled: as_bool(prop(props, "bDisableTrack")).unwrap_or(false),
        active_condition: (!condition.eq_ignore_ascii_case("ETAC_Always")).then_some(condition),
        data,
        sub_tracks: Vec::new(),
        warnings,
    }
}

// ================================================================== extraction

struct Resolver<'a> {
    pkg: &'a Package,
    own_name: &'a str,
    schema: &'a dyn Schema,
    defaults: &'a dyn ClassDefaults,
    effective: HashMap<usize, Arc<Vec<Property>>>,
    in_progress: HashSet<usize>,
    class_defaults: HashMap<String, Arc<Vec<Property>>>,
    chains: HashMap<String, Arc<Vec<String>>>,
    /// Exports that are some export's archetype.
    archetypes: HashSet<usize>,
    /// `(archetype, instance root)` → the first export (in export order)
    /// with that archetype inside that prefab instance tree.
    in_instance: HashMap<(usize, usize), usize>,
    /// Cached [`value_count`] of effective property lists of archetypes.
    value_sizes: HashMap<usize, usize>,
    warnings: Vec<String>,
    dropped: usize,
    archetype_merges: usize,
    unremapped: usize,
    decode_failures: usize,
    /// Track decodes left ([`MAX_MAP_TRACKS`]).
    tracks_left: usize,
    /// Group decodes left ([`MAX_MAP_GROUPS`]).
    groups_left: usize,
    /// List items left ([`MAX_MAP_ITEMS`]).
    items_left: usize,
    /// Archetype values left to copy ([`MAX_MERGED_VALUES`]).
    values_left: usize,
    /// Budgets already reported as exhausted.
    exhausted: HashSet<&'static str>,
}

impl<'a> Resolver<'a> {
    fn new(
        pkg: &'a Package,
        own_name: &'a str,
        schema: &'a dyn Schema,
        defaults: &'a dyn ClassDefaults,
    ) -> Self {
        let mut r = Resolver {
            pkg,
            own_name,
            schema,
            defaults,
            effective: HashMap::new(),
            in_progress: HashSet::new(),
            class_defaults: HashMap::new(),
            chains: HashMap::new(),
            archetypes: HashSet::new(),
            in_instance: HashMap::new(),
            value_sizes: HashMap::new(),
            warnings: Vec::new(),
            dropped: 0,
            archetype_merges: 0,
            unremapped: 0,
            decode_failures: 0,
            tracks_left: MAX_MAP_TRACKS,
            groups_left: MAX_MAP_GROUPS,
            items_left: MAX_MAP_ITEMS,
            values_left: MAX_MERGED_VALUES,
            exhausted: HashSet::new(),
        };
        // An export with archetype `a` lies inside the instance tree rooted
        // at `root` (the topmost export with an archetype above some object)
        // exactly when its own instance root is `root`, so one pass over the
        // export table answers every remap lookup.
        for (i, e) in pkg.exports.iter().enumerate() {
            if let Some(a) = e.archetype_index.export_index() {
                r.archetypes.insert(a);
                if let Some(root) = r.instance_root(i) {
                    r.in_instance.entry((a, root)).or_insert(i);
                }
            }
        }
        r
    }

    /// Report a per-map budget as exhausted (once per budget).
    fn exhaust(&mut self, what: &'static str, limit: usize) {
        if self.exhausted.insert(what) {
            self.warn(format!(
                "more than {limit} {what} in this map: the rest are skipped"
            ));
        }
    }

    /// Take `n` list items from the per-map budget; false (and a warning)
    /// when they do not fit.
    fn take_items(&mut self, n: usize) -> bool {
        if n > self.items_left {
            self.items_left = 0;
            self.exhaust("list items", MAX_MAP_ITEMS);
            return false;
        }
        self.items_left -= n;
        true
    }

    fn warn(&mut self, msg: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(msg);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    fn path(&self, i: usize) -> String {
        PackageIndex::from_export(i)
            .and_then(|idx| qualified_path(self.pkg, Some(self.own_name), idx).ok())
            .unwrap_or_else(|| format!("export {i}"))
    }

    fn class_path(&self, i: usize) -> String {
        export_class_path(self.pkg, Some(self.own_name), i).unwrap_or_default()
    }

    /// Class chain (lower-case short names, nearest first); the class's own
    /// short name when the schema does not know it.
    fn chain(&mut self, class_path: &str) -> Arc<Vec<String>> {
        let key = class_path.to_ascii_lowercase();
        if let Some(c) = self.chains.get(&key) {
            return c.clone();
        }
        let mut chain = self.schema.class_chain(class_path);
        if chain.is_empty() {
            chain.push(last_component(class_path).to_ascii_lowercase());
        }
        let chain = Arc::new(chain);
        self.chains.insert(key, chain.clone());
        chain
    }

    fn class_defaults(&mut self, class_path: &str) -> Arc<Vec<Property>> {
        let key = class_path.to_ascii_lowercase();
        if let Some(d) = self.class_defaults.get(&key) {
            return d.clone();
        }
        let d = Arc::new(self.defaults.class_defaults(class_path));
        self.class_defaults.insert(key, d.clone());
        d
    }

    fn own_props(&mut self, i: usize) -> Vec<Property> {
        match decode_object(self.pkg, Some(self.own_name), i, self.schema) {
            Ok(d) => {
                if let Some(w) = d.warnings.first() {
                    let msg = format!(
                        "{}: {} decoder note(s), first: {w}",
                        d.path,
                        d.warnings.len()
                    );
                    self.warn(msg);
                }
                d.properties
            }
            Err(e) => {
                self.decode_failures = self.decode_failures.saturating_add(1);
                let msg = format!("{}: decode failed: {e}", self.path(i));
                self.warn(msg);
                Vec::new()
            }
        }
    }

    /// Topmost export in the outer chain of `i` (itself included) that has
    /// an export archetype: the root of the prefab instance `i` belongs to.
    fn instance_root(&self, i: usize) -> Option<usize> {
        let mut cur = Some(i);
        let mut root = None;
        let mut steps = 0usize;
        while let Some(c) = cur {
            if steps > MAX_OUTER_DEPTH {
                break;
            }
            steps += 1;
            let Some(e) = self.pkg.exports.get(c) else {
                break;
            };
            if e.archetype_index.export_index().is_some() {
                root = Some(c);
            }
            cur = e.outer_index.export_index();
        }
        root
    }

    /// Point references to archetype objects at the matching objects of the
    /// instance tree rooted at `root` (found through the export table's
    /// archetype field).
    fn remap_value(&mut self, v: &mut Value, root: usize, depth: usize) {
        if depth > crate::property::MAX_VALUE_DEPTH {
            return;
        }
        match v {
            Value::Object(o) | Value::Interface(o) => {
                let Some(r) = PackageIndex(o.index).export_index() else {
                    return;
                };
                let hit = self
                    .archetypes
                    .contains(&r)
                    .then(|| self.in_instance.get(&(r, root)).copied());
                match hit {
                    Some(Some(c)) => {
                        if let Some(idx) = PackageIndex::from_export(c) {
                            o.index = idx.0;
                            o.path = self.path(c);
                        }
                    }
                    Some(None) => {
                        self.unremapped = self.unremapped.saturating_add(1);
                        let msg = format!(
                            "{}: inherited reference {} has no counterpart in the instance",
                            self.path(root),
                            o.path
                        );
                        self.warn(msg);
                    }
                    None => {}
                }
            }
            Value::Array(list) => {
                for x in list.iter_mut() {
                    self.remap_value(x, root, depth + 1);
                }
            }
            Value::Struct { fields, .. } => {
                for f in fields.iter_mut() {
                    self.remap_value(&mut f.value, root, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Effective properties of export `i`: own values over the (remapped)
    /// archetype's effective values, or over the class defaults when the
    /// archetype is not an export of this package.
    fn effective(&mut self, i: usize) -> Arc<Vec<Property>> {
        self.effective_depth(i, 0)
    }

    fn effective_depth(&mut self, i: usize, depth: usize) -> Arc<Vec<Property>> {
        if let Some(e) = self.effective.get(&i) {
            return e.clone();
        }
        self.in_progress.insert(i);
        let class = self.class_path(i);
        let own = self.own_props(i);
        let arch = self
            .pkg
            .exports
            .get(i)
            .map(|e| e.archetype_index)
            .unwrap_or_default();
        let mut base: Vec<Property> = match arch.export_index() {
            Some(a) if depth < MAX_ARCHETYPE_DEPTH && !self.in_progress.contains(&a) => {
                let b = self.effective_depth(a, depth + 1);
                let size = match self.value_sizes.get(&a) {
                    Some(&n) => n,
                    None => {
                        let n = value_count(&b);
                        self.value_sizes.insert(a, n);
                        n
                    }
                };
                if size > self.values_left {
                    // Hostile fan-out of one large archetype: stop copying.
                    self.values_left = 0;
                    self.exhaust("archetype values", MAX_MERGED_VALUES);
                    self.in_progress.remove(&i);
                    let mut base = (*self.class_defaults(&class)).clone();
                    merge_properties(&mut base, &own);
                    let arc = Arc::new(base);
                    self.effective.insert(i, arc.clone());
                    return arc;
                }
                self.values_left -= size;
                self.archetype_merges = self.archetype_merges.saturating_add(1);
                let mut b = (*b).clone();
                if let Some(root) = self.instance_root(i) {
                    // Values the object stores itself replace the inherited
                    // ones, so only the inherited survivors are remapped.
                    let own_keys: HashSet<(String, i32)> = own
                        .iter()
                        .filter(|p| !matches!(p.value, Value::Struct { binary: false, .. }))
                        .map(|p| (p.name.to_ascii_lowercase(), p.array_index))
                        .collect();
                    for p in b.iter_mut() {
                        if !own_keys.contains(&(p.name.to_ascii_lowercase(), p.array_index)) {
                            self.remap_value(&mut p.value, root, 0);
                        }
                    }
                }
                b
            }
            Some(_) => {
                let msg = format!("{}: archetype chain too deep or cyclic", self.path(i));
                self.warn(msg);
                (*self.class_defaults(&class)).clone()
            }
            None => (*self.class_defaults(&class)).clone(),
        };
        merge_properties(&mut base, &own);
        self.in_progress.remove(&i);
        let arc = Arc::new(base);
        self.effective.insert(i, arc.clone());
        arc
    }

    fn export_of(&self, o: &ObjRef) -> Option<usize> {
        PackageIndex(o.index)
            .export_index()
            .filter(|&i| i < self.pkg.exports.len())
    }

    fn object_class(&self, o: &ObjRef) -> Option<String> {
        let idx = PackageIndex(o.index);
        if idx.is_null() {
            return None;
        }
        self.pkg.class_name(idx).ok()
    }
}

/// Per-`InterpData` decoding state.
struct DataWalk<'r> {
    reached_groups: &'r mut HashSet<usize>,
    reached_tracks: &'r mut HashSet<usize>,
    stack: HashSet<usize>,
    budget: usize,
}

fn decode_track_tree(
    r: &mut Resolver<'_>,
    t: usize,
    depth: usize,
    walk: &mut DataWalk<'_>,
) -> Option<Track> {
    if walk.budget == 0 {
        return None;
    }
    if r.tracks_left == 0 {
        r.exhaust("track decodes", MAX_MAP_TRACKS);
        return None;
    }
    if walk.stack.contains(&t) {
        let msg = format!("{}: track reached through itself", r.path(t));
        r.warn(msg);
        return None;
    }
    walk.budget -= 1;
    r.tracks_left -= 1;
    let props = r.effective(t);
    let class = r.class_path(t);
    let chain = r.chain(&class);
    let path = r.path(t);
    let mut track = decode_track(&path, &class, &chain, &props);
    track.export_index = t;
    let (curve_keys, discrete_keys) = track.data.key_counts();
    let extra = match &track.data {
        TrackData::Unknown(u) => u.properties.len(),
        TrackData::AnimControl(a) => a.anim_sets.len(),
        TrackData::FloatMaterialParam(m) => m.materials.len(),
        TrackData::VectorMaterialParam(m) => m.materials.len(),
        TrackData::HeadTracking(h) => h.controllers.len(),
        TrackData::Move(m) => m.lookup.len(),
        TrackData::MoveAxis(a) => a.lookup.len(),
        _ => 0,
    };
    if !r.take_items(
        curve_keys
            .saturating_add(discrete_keys)
            .saturating_add(extra)
            .saturating_add(1),
    ) {
        return None;
    }
    walk.reached_tracks.insert(t);
    walk.stack.insert(t);
    let subs: Vec<ObjRef> = items(prop(&props, "SubTracks"))
        .iter()
        .filter_map(|v| as_obj(Some(v)).cloned())
        .collect();
    if !subs.is_empty() && depth >= MAX_SUBTRACK_DEPTH {
        note(
            &mut track.warnings,
            format!("sub-tracks deeper than {MAX_SUBTRACK_DEPTH} ignored"),
        );
    } else {
        for o in &subs {
            match r.export_of(o) {
                Some(s) => {
                    if let Some(st) = decode_track_tree(r, s, depth + 1, walk) {
                        track.sub_tracks.push(st);
                    }
                }
                None => note(
                    &mut track.warnings,
                    format!("sub-track {} is not an export", o.path),
                ),
            }
        }
    }
    if let TrackData::Move(m) = &mut track.data {
        let mut rest = Vec::new();
        for st in std::mem::take(&mut track.sub_tracks) {
            match st.data {
                TrackData::MoveAxis(a) => m.axes.push(a),
                _ => rest.push(st),
            }
        }
        track.sub_tracks = rest;
    }
    walk.stack.remove(&t);
    Some(track)
}

fn color_of(v: Option<&Value>) -> [u8; 4] {
    let Some(v) = v else {
        return [0; 4];
    };
    let c = |n: &str| {
        as_i32(member(v, n))
            .and_then(|x| u8::try_from(x).ok())
            .unwrap_or(0)
    };
    [c("R"), c("G"), c("B"), c("A")]
}

fn decode_group(r: &mut Resolver<'_>, g: usize, walk: &mut DataWalk<'_>) -> Option<InterpGroup> {
    if r.groups_left == 0 {
        r.exhaust("group decodes", MAX_MAP_GROUPS);
        return None;
    }
    r.groups_left -= 1;
    walk.reached_groups.insert(g);
    let props = r.effective(g);
    let anim_set_count = items(prop(&props, "GroupAnimSets")).len();
    if !r.take_items(anim_set_count) {
        return None;
    }
    let class = r.class_path(g);
    let chain = r.chain(&class);
    let kind = group_kind(&chain).unwrap_or(GroupKind::Group);
    let flag = |n: &str| as_bool(prop(&props, n)).unwrap_or(false);
    let ai = (kind == GroupKind::Ai).then(|| AiGroupInfo {
        stage_mark_group: as_name(prop(&props, "StageMarkGroup")),
        snap_to_root_bone: flag("SnapToRootBoneLocationWhenFinished"),
        no_encroachment_check: flag("bNoEncroachmentCheck"),
        disable_world_collision: flag("bDisableWorldCollision"),
    });
    let mut group = InterpGroup {
        path: r.path(g),
        export_index: g,
        class: class.clone(),
        kind,
        name: as_name(prop(&props, "GroupName")).unwrap_or_else(|| "None".to_owned()),
        color: color_of(prop(&props, "GroupColor")),
        folder: flag("bIsFolder"),
        parented: flag("bIsParented"),
        anim_sets: obj_paths(prop(&props, "GroupAnimSets")),
        ai,
        tracks: Vec::new(),
    };
    let refs: Vec<ObjRef> = items(prop(&props, "InterpTracks"))
        .iter()
        .filter_map(|v| as_obj(Some(v)).cloned())
        .collect();
    walk.budget = MAX_TRACKS;
    walk.stack.clear();
    for o in refs {
        let Some(t) = r.export_of(&o) else {
            let msg = format!("{}: track {} is not an export", group.path, o.path);
            r.warn(msg);
            continue;
        };
        match decode_track_tree(r, t, 0, walk) {
            Some(track) => group.tracks.push(track),
            None if walk.budget == 0 => {
                let msg = format!("{}: more than {MAX_TRACKS} tracks", group.path);
                r.warn(msg);
                break;
            }
            None => {}
        }
    }
    Some(group)
}

fn decode_data(
    r: &mut Resolver<'_>,
    d: usize,
    scope: NodeScope,
    reached_groups: &mut HashSet<usize>,
    reached_tracks: &mut HashSet<usize>,
) -> InterpDataInfo {
    let props = r.effective(d);
    let mut info = InterpDataInfo {
        path: r.path(d),
        export_index: d,
        scope,
        length: as_f32(prop(&props, "InterpLength")).unwrap_or(0.0),
        path_build_time: as_f32(prop(&props, "PathBuildTime")).unwrap_or(0.0),
        ed_section: [
            as_f32(prop(&props, "EdSectionStart")).unwrap_or(0.0),
            as_f32(prop(&props, "EdSectionEnd")).unwrap_or(0.0),
        ],
        bake_and_prune: as_bool(prop(&props, "bShouldBakeAndPrune")).unwrap_or(false),
        groups: Vec::new(),
        used_by: Vec::new(),
        warnings: Vec::new(),
    };
    let refs: Vec<ObjRef> = items(prop(&props, "InterpGroups"))
        .iter()
        .filter_map(|v| as_obj(Some(v)).cloned())
        .collect();
    if refs.len() > MAX_GROUPS {
        note(
            &mut info.warnings,
            format!("{} groups, only {MAX_GROUPS} decoded", refs.len()),
        );
    }
    let mut walk = DataWalk {
        reached_groups,
        reached_tracks,
        stack: HashSet::new(),
        budget: MAX_TRACKS,
    };
    let mut seen = HashSet::new();
    for o in refs.iter().take(MAX_GROUPS) {
        let Some(g) = r.export_of(o) else {
            note(
                &mut info.warnings,
                format!("group {} is not an export", o.path),
            );
            continue;
        };
        if !seen.insert(g) {
            note(&mut info.warnings, format!("group {} listed twice", o.path));
            continue;
        }
        let class = r.class_path(g);
        let chain = r.chain(&class);
        if group_kind(&chain).is_none() {
            note(
                &mut info.warnings,
                format!("{} is a {class}, not an InterpGroup", o.path),
            );
            continue;
        }
        if let Some(group) = decode_group(r, g, &mut walk) {
            info.groups.push(group);
        }
    }
    info
}

const SETTINGS: &[&str] = &[
    "PlayRate",
    "bLooping",
    "bRewindOnPlay",
    "bNoResetOnRewind",
    "bRewindIfAlreadyPlaying",
    "bForceStartPos",
    "ForceStartPosition",
    "bClientSideOnly",
    "bSkipUpdateIfNotVisible",
    "bIsSkippable",
    "bDisableRadioFilter",
    "bInterpForPathBuilding",
    "bAutoStopWhenDirectorChanges",
    "bLockOutgoingCameraOnFirstTransition",
    "bSkipFirstTransitionIfNotFromCamera",
    "PreferredSplitScreenNum",
    "ConstantCameraAnim",
    "ConstantCameraAnimRate",
];

fn settings_of(node: &crate::kismet::KismetNode) -> InterpSettings {
    let get = |n: &str| node.param(n).map(|p| &p.value);
    let flag = |n: &str| as_bool(get(n)).unwrap_or(false);
    let d = InterpSettings::default();
    InterpSettings {
        play_rate: as_f32(get("PlayRate")).unwrap_or(d.play_rate),
        looping: flag("bLooping"),
        rewind_on_play: flag("bRewindOnPlay"),
        no_reset_on_rewind: flag("bNoResetOnRewind"),
        rewind_if_already_playing: flag("bRewindIfAlreadyPlaying"),
        force_start_pos: flag("bForceStartPos"),
        force_start_position: as_f32(get("ForceStartPosition")).unwrap_or(0.0),
        client_side_only: flag("bClientSideOnly"),
        skip_update_if_not_visible: flag("bSkipUpdateIfNotVisible"),
        is_skippable: flag("bIsSkippable"),
        disable_radio_filter: flag("bDisableRadioFilter"),
        interp_for_path_building: flag("bInterpForPathBuilding"),
        camera_flags: [
            "bAutoStopWhenDirectorChanges",
            "bLockOutgoingCameraOnFirstTransition",
            "bSkipFirstTransitionIfNotFromCamera",
        ]
        .iter()
        .filter(|n| flag(n))
        .map(|n| (*n).to_owned())
        .collect(),
        preferred_split_screen: as_i32(get("PreferredSplitScreenNum")).unwrap_or(0),
        constant_camera_anim: as_i32(get("ConstantCameraAnim")).unwrap_or(0),
        constant_camera_anim_rate: as_f32(get("ConstantCameraAnimRate"))
            .unwrap_or(d.constant_camera_anim_rate),
        stored: node
            .params
            .iter()
            .filter(|p| p.origin == crate::kismet::Origin::Own)
            .filter(|p| SETTINGS.iter().any(|s| s.eq_ignore_ascii_case(&p.name)))
            .map(|p| p.name.clone())
            .collect(),
    }
}

fn value_targets(
    r: &mut Resolver<'_>,
    node: &crate::kismet::KismetNode,
    named: Option<String>,
    out: &mut Vec<BoundTarget>,
) {
    let value = node.variable.as_ref().and_then(|v| v.value.as_ref());
    let mut objs: Vec<&ObjRef> = Vec::new();
    match value {
        Some(Value::Object(o) | Value::Interface(o)) if o.index != 0 => objs.push(o),
        Some(Value::Array(list)) => objs.extend(list.iter().filter_map(|x| as_obj(Some(x)))),
        _ => {}
    }
    let variable_class = node.class_name().to_owned();
    if !r.take_items(objs.len().max(1)) {
        return;
    }
    if objs.is_empty() {
        out.push(BoundTarget {
            variable: node.path.clone(),
            variable_class,
            named,
            object: None,
            object_class: None,
        });
        return;
    }
    for o in objs {
        out.push(BoundTarget {
            variable: node.path.clone(),
            variable_class: variable_class.clone(),
            named: named.clone(),
            object: Some(o.path.clone()),
            object_class: r.object_class(o),
        });
    }
}

/// Extract the Matinee data of a package opened in `set` (builds its Kismet
/// graph first).
pub fn extract_for(set: &crate::model::PackageSet, lp: &crate::model::LoadedPackage) -> MatineeMap {
    let graph = crate::kismet::build_graph_for(set, lp);
    extract(&lp.package, &lp.name, set, set, &graph)
}

/// Extract the Matinee data of `pkg` (own name `own_name`), using `graph`
/// (its Kismet graph) for the actions, their settings and their variable
/// links.
pub fn extract(
    pkg: &Package,
    own_name: &str,
    schema: &dyn Schema,
    defaults: &dyn ClassDefaults,
    graph: &KismetGraph,
) -> MatineeMap {
    let mut r = Resolver::new(pkg, own_name, schema, defaults);
    let scope_of: HashMap<usize, NodeScope> = graph
        .nodes
        .iter()
        .map(|n| (n.export_index, n.scope))
        .collect();

    // Census of Matinee exports.
    let mut data_exports = Vec::new();
    let mut anim_exports = Vec::new();
    let mut group_exports = Vec::new();
    let mut track_exports = Vec::new();
    let mut stored_instances = 0usize;
    for i in 0..pkg.exports.len() {
        let class = r.class_path(i);
        let chain = r.chain(&class);
        if has_class(&chain, "interpgroupinst") || has_class(&chain, "interptrackinst") {
            stored_instances += 1;
        } else if has_class(&chain, "interpdata") {
            data_exports.push(i);
        } else if has_class(&chain, "cameraanim") {
            anim_exports.push(i);
        } else if group_kind(&chain).is_some() {
            group_exports.push(i);
        } else if has_class(&chain, "interptrack") || track_class(&chain).is_some() {
            track_exports.push(i);
        }
    }

    let mut reached_groups = HashSet::new();
    let mut reached_tracks = HashSet::new();
    let mut interp_data: Vec<InterpDataInfo> = Vec::with_capacity(data_exports.len());
    for &d in &data_exports {
        let scope = scope_of.get(&d).copied().unwrap_or(NodeScope::Detached);
        interp_data.push(decode_data(
            &mut r,
            d,
            scope,
            &mut reached_groups,
            &mut reached_tracks,
        ));
    }

    let mut camera_anims = Vec::new();
    for &a in &anim_exports {
        let props = r.effective(a);
        let mut info = CameraAnimInfo {
            path: r.path(a),
            export_index: a,
            length: as_f32(prop(&props, "AnimLength")).unwrap_or(0.0),
            base_fov: as_f32(prop(&props, "BaseFOV")).unwrap_or(0.0),
            group: None,
        };
        if let Some(g) = as_obj(prop(&props, "CameraInterpGroup")).and_then(|o| r.export_of(o)) {
            let class = r.class_path(g);
            let chain = r.chain(&class);
            if group_kind(&chain).is_some() && !reached_groups.contains(&g) {
                let mut walk = DataWalk {
                    reached_groups: &mut reached_groups,
                    reached_tracks: &mut reached_tracks,
                    stack: HashSet::new(),
                    budget: MAX_TRACKS,
                };
                info.group = decode_group(&mut r, g, &mut walk);
            }
        }
        camera_anims.push(info);
    }

    // Lookups by lower-case path / group name (first match wins, as a
    // linear search would), so hostile packages cannot make them quadratic.
    let data_by_path = path_index(&interp_data);
    let groups_by_name: Vec<HashMap<String, usize>> = interp_data
        .iter()
        .map(|d| {
            let mut m = HashMap::new();
            for (k, g) in d.groups.iter().enumerate() {
                m.entry(g.name.to_ascii_lowercase()).or_insert(k);
            }
            m
        })
        .collect();

    // Actions, from the Kismet graph.
    let mut out_edges: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, e) in graph.edges.iter().enumerate() {
        out_edges.entry(e.from).or_default().push(k);
    }
    let mut actions = Vec::new();
    for node in &graph.nodes {
        let chain = r.chain(&node.class);
        if !has_class(&chain, "seqact_interp") {
            continue;
        }
        let edges: Vec<&crate::kismet::KismetEdge> = out_edges
            .get(&node.id)
            .map(|v| v.iter().filter_map(|&k| graph.edges.get(k)).collect())
            .unwrap_or_default();
        let data_path = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Matinee)
            .find_map(|e| graph.node(e.to).map(|n| n.path.clone()));
        let data_idx = data_path
            .as_ref()
            .and_then(|p| data_by_path.get(&p.to_ascii_lowercase()).copied());
        let data = data_idx.and_then(|k| interp_data.get(k));
        let data_ports: HashSet<usize> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Matinee)
            .filter_map(|e| e.from_port)
            .collect();
        // Variable edges by link, in edge order (one pass, not one per link).
        let mut by_port: HashMap<usize, Vec<&crate::kismet::KismetEdge>> = HashMap::new();
        for e in edges.iter().filter(|e| e.kind == EdgeKind::Variable) {
            if let Some(port) = e.from_port {
                by_port.entry(port).or_default().push(e);
            }
        }
        let mut bindings = Vec::new();
        let mut property_links = Vec::new();
        for (port, var) in node.variables.iter().enumerate() {
            if data_ports.contains(&port)
                || var
                    .expected_type
                    .as_deref()
                    .is_some_and(|t| last_component(t).eq_ignore_ascii_case("InterpData"))
            {
                continue;
            }
            if let Some(property) = var
                .property_name
                .as_ref()
                .filter(|p| !p.eq_ignore_ascii_case("None"))
            {
                let mut variables = Vec::new();
                for v in by_port
                    .get(&port)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| graph.node(e.to))
                {
                    let value = v.variable.as_ref().and_then(|x| x.value.as_ref());
                    if !r.take_items(value.map_or(1, value_size)) {
                        break;
                    }
                    variables.push(LinkedValue {
                        variable: v.path.clone(),
                        variable_class: v.class_name().to_owned(),
                        value: value.cloned(),
                    });
                }
                property_links.push(PropertyLink {
                    link: port,
                    label: var.desc.clone(),
                    property: property.clone(),
                    variables,
                });
                continue;
            }
            let mut targets = Vec::new();
            for e in by_port.get(&port).into_iter().flatten() {
                if r.items_left == 0 {
                    r.exhaust("list items", MAX_MAP_ITEMS);
                    break;
                }
                let Some(v) = graph.node(e.to) else { continue };
                let vchain = r.chain(&v.class);
                if has_class(&vchain, "seqvar_named") {
                    let name = v.variable.as_ref().and_then(|x| x.find_var_name.clone());
                    let named: Vec<usize> = out_edges
                        .get(&v.id)
                        .map(|ks| {
                            ks.iter()
                                .filter_map(|&k| graph.edges.get(k))
                                .filter(|ne| ne.kind == EdgeKind::NamedVariable)
                                .map(|ne| ne.to)
                                .collect()
                        })
                        .unwrap_or_default();
                    for t in named {
                        if r.items_left == 0 {
                            break;
                        }
                        if let Some(tn) = graph.node(t) {
                            value_targets(&mut r, tn, name.clone(), &mut targets);
                        }
                    }
                } else {
                    value_targets(&mut r, v, None, &mut targets);
                }
            }
            let group = data.zip(data_idx).and_then(|(d, k)| {
                groups_by_name
                    .get(k)
                    .and_then(|m| m.get(&var.desc.to_ascii_lowercase()))
                    .and_then(|&gi| d.groups.get(gi))
            });
            bindings.push(GroupBinding {
                link: port,
                label: var.desc.clone(),
                group: group.map(|g| g.name.clone()),
                group_kind: group.map(|g| g.kind),
                targets,
            });
        }
        actions.push(MatineeAction {
            path: node.path.clone(),
            export_index: node.export_index,
            node: node.id,
            class: node.class.clone(),
            scope: node.scope,
            parent_sequence: node
                .parent
                .and_then(|p| graph.node(p))
                .map(|p| p.path.clone()),
            comment: node.comment.clone(),
            interp_data: data_path,
            settings: settings_of(node),
            inputs: node.inputs.iter().map(|p| p.desc.clone()).collect(),
            outputs: node.outputs.iter().map(|p| p.desc.clone()).collect(),
            bindings,
            property_links,
        });
    }
    for a in &actions {
        if let Some(p) = &a.interp_data
            && let Some(&k) = data_by_path.get(&p.to_ascii_lowercase())
            && let Some(d) = interp_data.get_mut(k)
        {
            d.used_by.push(a.path.clone());
        }
    }

    let mut coverage = coverage_of(&actions, &interp_data);
    coverage.camera_anims = camera_anims.len();
    for g in camera_anims.iter().filter_map(|c| c.group.as_ref()) {
        bump(&mut coverage.groups, last_component(&g.class));
        for t in &g.tracks {
            count_track(t, &mut coverage);
        }
    }
    let orphan_groups: Vec<usize> = group_exports
        .iter()
        .copied()
        .filter(|g| !reached_groups.contains(g))
        .collect();
    let orphan_tracks: Vec<usize> = track_exports
        .iter()
        .copied()
        .filter(|t| !reached_tracks.contains(t))
        .collect();
    coverage.orphan_groups = orphan_groups.len();
    coverage.orphan_tracks = orphan_tracks.len();
    let orphans: Vec<String> = orphan_groups
        .iter()
        .chain(orphan_tracks.iter())
        .map(|&i| r.path(i))
        .collect();
    coverage.stored_instances = stored_instances;
    coverage.archetype_merges = r.archetype_merges;
    coverage.unremapped_refs = r.unremapped;
    coverage.decode_failures = r.decode_failures;
    let mut warnings = std::mem::take(&mut r.warnings);
    if r.dropped > 0 {
        warnings.push(format!("{} further warnings not kept", r.dropped));
    }
    let track_warnings: usize = interp_data
        .iter()
        .map(|d| {
            d.warnings.len()
                + d.groups
                    .iter()
                    .flat_map(|g| g.tracks.iter())
                    .map(|t| t.warnings.len())
                    .sum::<usize>()
        })
        .sum();
    coverage.warnings = warnings.len() + r.dropped + track_warnings;
    MatineeMap {
        format: MATINEE_FORMAT.to_owned(),
        version: MATINEE_VERSION,
        package: own_name.to_owned(),
        actions,
        interp_data,
        camera_anims,
        coverage,
        orphans,
        warnings,
    }
}

fn bump(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_owned()).or_insert(0) += 1;
}

fn curve_stats<T>(c: &InterpCurve<T>, cov: &mut MatineeCoverage) {
    if c.points.is_empty() {
        return;
    }
    let method = match c.method {
        InterpMethod::FixedTangentEvalAndNewAutoTangents => {
            "IMT_UseFixedTangentEvalAndNewAutoTangents"
        }
        InterpMethod::FixedTangentEval => "IMT_UseFixedTangentEval",
        InterpMethod::BrokenTangentEval => "IMT_UseBrokenTangentEval",
    };
    bump(&mut cov.curve_methods, method);
    for p in &c.points {
        bump(&mut cov.curve_modes, p.mode.enum_name());
    }
}

fn track_curves(t: &TrackData, cov: &mut MatineeCoverage) {
    match t {
        TrackData::Move(m) => {
            curve_stats(&m.pos, cov);
            curve_stats(&m.euler, cov);
            for a in &m.axes {
                curve_stats(&a.curve, cov);
            }
        }
        TrackData::MoveAxis(a) => curve_stats(&a.curve, cov),
        TrackData::Sound(s) => curve_stats(&s.curve, cov),
        TrackData::FloatProperty(c)
        | TrackData::SkelControlStrength(c)
        | TrackData::SkelControlScale(c)
        | TrackData::FloatParticleParam(c)
        | TrackData::MorphWeight(c) => curve_stats(&c.curve, cov),
        TrackData::VectorProperty(c) | TrackData::ColorProperty(c) => curve_stats(&c.curve, cov),
        TrackData::LinearColorProperty(c) => curve_stats(&c.curve, cov),
        TrackData::Fade(f) => curve_stats(&f.curve, cov),
        TrackData::Slomo(c) | TrackData::FloatBase(c) => curve_stats(&c.curve, cov),
        TrackData::ColorScale(c) | TrackData::AudioMaster(c) | TrackData::VectorBase(c) => {
            curve_stats(&c.curve, cov)
        }
        TrackData::LinearColorBase(c) => curve_stats(&c.curve, cov),
        TrackData::FloatMaterialParam(m) => curve_stats(&m.curve, cov),
        TrackData::VectorMaterialParam(m) => curve_stats(&m.curve, cov),
        TrackData::AnimControl(a) => curve_stats(&a.weight, cov),
        _ => {}
    }
}

fn count_track(t: &Track, cov: &mut MatineeCoverage) {
    let short = last_component(&t.class).to_owned();
    let decoded = !matches!(t.data, TrackData::Unknown(_));
    let (ck, dk) = t.data.key_counts();
    {
        let e = cov.tracks.entry(short).or_default();
        e.count += 1;
        e.decoded += usize::from(decoded);
        e.curve_keys += ck;
        e.discrete_keys += dk;
    }
    cov.tracks_total += 1;
    cov.tracks_decoded += usize::from(decoded);
    cov.tracks_unknown += usize::from(!decoded);
    cov.tracks_disabled += usize::from(t.disabled);
    track_curves(&t.data, cov);
    if let TrackData::Move(m) = &t.data {
        bump(
            &mut cov.move_frames,
            match m.move_frame {
                MoveFrame::World => "IMF_World",
                MoveFrame::RelativeToInitial => "IMF_RelativeToInitial",
            },
        );
        bump(
            &mut cov.rot_modes,
            match m.rot_mode {
                RotMode::Keyframed => "IMR_Keyframed",
                RotMode::LookAtGroup => "IMR_LookAtGroup",
                RotMode::Ignore => "IMR_Ignore",
            },
        );
        cov.quat_interpolation += usize::from(m.use_quat_interpolation);
        cov.raw_actor_tm += usize::from(m.use_raw_actor_tm);
        cov.lookup_group_keys += m.lookup.iter().filter(|k| k.group.is_some()).count();
        if !m.axes.is_empty() {
            cov.split_move_tracks += 1;
            for (i, a) in m.axes.iter().enumerate() {
                // Sub-tracks are counted as tracks of their own class too.
                let e = cov
                    .tracks
                    .entry("InterpTrackMoveAxis".to_owned())
                    .or_default();
                e.count += 1;
                e.decoded += 1;
                e.curve_keys += a.curve.points.len();
                cov.tracks_total += 1;
                cov.tracks_decoded += 1;
                cov.axis_order_mismatches += usize::from(a.axis.index() != i);
                cov.lookup_group_keys += a.lookup.iter().filter(|k| k.group.is_some()).count();
                cov.lookup_length_mismatches +=
                    usize::from(!a.lookup.is_empty() && a.lookup.len() != a.curve.points.len());
            }
        } else if !m.lookup.is_empty() && m.lookup.len() != m.pos.points.len() {
            cov.lookup_length_mismatches += 1;
        }
    }
    for st in &t.sub_tracks {
        count_track(st, cov);
    }
}

/// Lower-case path → index of the first `InterpData` with that path.
fn path_index(datas: &[InterpDataInfo]) -> HashMap<String, usize> {
    let mut m = HashMap::with_capacity(datas.len());
    for (k, d) in datas.iter().enumerate() {
        m.entry(d.path.to_ascii_lowercase()).or_insert(k);
    }
    m
}

fn coverage_of(actions: &[MatineeAction], datas: &[InterpDataInfo]) -> MatineeCoverage {
    let by_path = path_index(datas);
    let mut cov = MatineeCoverage {
        actions: actions.len(),
        level_actions: actions
            .iter()
            .filter(|a| a.scope == NodeScope::Level)
            .count(),
        actions_with_data: actions.iter().filter(|a| a.interp_data.is_some()).count(),
        interp_data: datas.len(),
        level_interp_data: datas.iter().filter(|d| d.scope == NodeScope::Level).count(),
        unused_interp_data: datas.iter().filter(|d| d.used_by.is_empty()).count(),
        ..MatineeCoverage::default()
    };
    for d in datas {
        for g in &d.groups {
            bump(&mut cov.groups, last_component(&g.class));
            cov.folder_groups += usize::from(g.folder);
            for t in &g.tracks {
                count_track(t, &mut cov);
            }
        }
    }
    for a in actions {
        cov.property_links += a.property_links.len();
        for b in &a.bindings {
            cov.bindings += 1;
            cov.bindings_without_group += usize::from(b.group.is_none());
            for t in &b.targets {
                if let Some(c) = &t.object_class {
                    bump(&mut cov.bound_classes, c);
                }
            }
        }
        let Some(d) = a
            .interp_data
            .as_ref()
            .and_then(|p| by_path.get(&p.to_ascii_lowercase()))
            .and_then(|&k| datas.get(k))
        else {
            continue;
        };
        let bound: HashSet<String> = a
            .bindings
            .iter()
            .filter(|b| b.targets.iter().any(|t| t.object.is_some()))
            .filter_map(|b| b.group.as_deref().map(str::to_ascii_lowercase))
            .collect();
        for g in &d.groups {
            if g.folder || g.kind == GroupKind::Director {
                continue;
            }
            cov.unbound_groups += usize::from(!bound.contains(&g.name.to_ascii_lowercase()));
        }
    }
    cov
}

// ================================================================== move evaluation

/// The actors of other groups, for lookup keys and look-at rotation.
pub trait GroupActors {
    /// World location of the actor of group `group`.
    fn location(&self, group: &str) -> Option<[f32; 3]>;
    /// Rotation of the actor of group `group`.
    fn rotation(&self, group: &str) -> Option<[i32; 3]>;
}

/// No other actors (lookup keys fall back to their stored values).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGroupActors;

impl GroupActors for NoGroupActors {
    fn location(&self, _group: &str) -> Option<[f32; 3]> {
        None
    }
    fn rotation(&self, _group: &str) -> Option<[i32; 3]> {
        None
    }
}

/// The curve with lookup keys replaced by the named groups' actor values,
/// or `None` when no key resolves (then the stored curve applies as is).
///
/// A resolved key takes the actor's value; its time for neighbouring
/// tangents is the lookup key's `Time`. Inner resolved keys get a
/// Catmull-Rom style tangent from their neighbours scaled by
/// `1 − tension` (divided by the neighbours' time span with the default
/// method, halved otherwise); first and last resolved keys get zero
/// tangents. Structure STRONG (decompiled `GetKeyframePosition`), exact
/// operation order TENTATIVE; no shipped map uses lookup keys.
fn lookup_curve<T: CurveValue>(
    curve: &InterpCurve<T>,
    lookup: &[LookupKey],
    value_of: impl Fn(&str) -> Option<T>,
    tension: f32,
) -> Option<InterpCurve<T>> {
    let n = curve.points.len();
    let resolved: Vec<Option<T>> = (0..n)
        .map(|i| {
            lookup
                .get(i)
                .and_then(|k| k.group.as_deref())
                .and_then(&value_of)
        })
        .collect();
    if resolved.iter().all(Option::is_none) {
        return None;
    }
    let mut c = curve.clone();
    for (p, r) in c.points.iter_mut().zip(&resolved) {
        if let Some(v) = r {
            p.out_val = *v;
        }
    }
    let time_of = |k: usize| -> f32 {
        match (resolved.get(k), lookup.get(k), curve.points.get(k)) {
            (Some(Some(_)), Some(l), _) => l.time,
            (_, _, Some(p)) => p.in_val,
            _ => 0.0,
        }
    };
    let one_minus = 1.0 - tension;
    for i in 0..n {
        if resolved.get(i).is_none_or(Option::is_none) {
            continue;
        }
        let tangent = if i == 0 || i + 1 >= n {
            T::zero()
        } else {
            let (Some(prev), Some(cur), Some(next)) =
                (c.points.get(i - 1), c.points.get(i), c.points.get(i + 1))
            else {
                continue;
            };
            let scale = if curve.method == InterpMethod::FixedTangentEvalAndNewAutoTangents {
                (1.0 / max_small_le(time_of(i + 1) - time_of(i - 1))) * one_minus
            } else {
                one_minus * 0.5
            };
            let mut v = T::zero();
            for k in 0..T::DIM {
                let (p, x, q) = (prev.out_val.get(k), cur.out_val.get(k), next.out_val.get(k));
                v.set(k, scale * ((x - p) + (q - x)));
            }
            v
        };
        if let Some(p) = c.points.get_mut(i) {
            p.arrive_tangent = tangent;
            p.leave_tangent = tangent;
        }
    }
    Some(c)
}

impl MoveAxisTrack {
    /// Value at `t` (`UInterpTrackMoveAxis::EvalValueAtTime`: the curve, 0
    /// without keys; lookup keys take the named group's actor location or
    /// Euler rotation component).
    pub fn eval(&self, t: f32, actors: &dyn GroupActors, tension: f32) -> f32 {
        let axis = self.axis.index();
        let value_of = |g: &str| -> Option<f32> {
            if axis < 3 {
                actors
                    .location(g)
                    .and_then(|l| l.as_slice().get(axis).copied())
            } else {
                actors
                    .rotation(g)
                    .map(euler_from_rotator)
                    .and_then(|e| e.as_slice().get(axis - 3).copied())
            }
        };
        match lookup_curve(&self.curve, &self.lookup, value_of, tension) {
            Some(c) => c.eval(t, 0.0),
            None => self.curve.eval(t, 0.0),
        }
    }
}

/// What a move track does to its actor's rotation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum MoveRotation {
    /// Set this rotation.
    Set([i32; 3]),
    /// Keep the actor's current rotation (`IMR_Ignore`).
    Keep,
    /// Face the actor of this group (`IMR_LookAtGroup`; see
    /// [`direction_rotator`]).
    LookAt(String),
}

/// A move track's output at one time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoveSample {
    /// World location.
    pub location: [f32; 3],
    /// Rotation.
    pub rotation: MoveRotation,
}

/// Per-actor state of a move track (`UInterpTrackInstMove`).
#[derive(Debug, Clone, PartialEq)]
pub struct MoveInstance {
    /// `InitialTM`: the reference transform of `IMF_RelativeToInitial`
    /// tracks, without scale.
    pub initial: Matrix,
    /// The transform of the actor's base, when it is attached to one.
    pub base: Option<Matrix>,
}

impl MoveInstance {
    /// Instance for an unattached actor placed at `location` / `rotation`
    /// (`CalcInitialTransform`).
    ///
    /// `position` is the time whose key transform is factored out of the
    /// initial transform (CONFIRMED from the disassembly):
    /// - when the action initialises (`InitInterp` → `InitTrackInst`, which
    ///   runs every time a stopped action is started by Play, Reverse or
    ///   Change Dir), it is the action's current `Position`, so an action
    ///   restarted from where it stopped (a lift sent back down with Reverse)
    ///   continues from the actor's current placement along the same path;
    /// - when relative tracks are re-based for `bNoResetOnRewind` (Play's
    ///   rewind, the forwards loop wrap), it is 0.
    pub fn new(
        track: &MoveTrack,
        location: [f32; 3],
        rotation: [i32; 3],
        position: f32,
        actors: &dyn GroupActors,
    ) -> MoveInstance {
        MoveInstance {
            initial: track.initial_transform(location, rotation, position, actors),
            base: None,
        }
    }

    /// Instance for an actor attached to a base whose transform is `base`
    /// (the base actor's rotation and location as a matrix; TENTATIVE: the
    /// engine's `GetBaseMatrix` is assumed to carry no scale). The actor's
    /// world placement is first expressed relative to the base, so keys
    /// (`IMF_World`) and the initial transform (`IMF_RelativeToInitial`)
    /// follow the base. `position` as in [`MoveInstance::new`].
    pub fn with_base(
        track: &MoveTrack,
        location: [f32; 3],
        rotation: [i32; 3],
        base: &Matrix,
        position: f32,
        actors: &dyn GroupActors,
    ) -> MoveInstance {
        let base = remove_scaling(base);
        let actor = rotation_translation_matrix(rotation, location);
        let relative = matrix_mul(&actor, &rigid_inverse(&base));
        MoveInstance {
            initial: track.initial_from_matrix(&relative, position, actors),
            base: Some(base),
        }
    }
}

impl MoveTrack {
    /// True when the track moves its actor at all: the engine skips move
    /// tracks with neither sub-tracks nor rotation keys
    /// (`GetLocationAtTime`, CONFIRMED).
    pub fn is_active(&self) -> bool {
        !self.axes.is_empty() || !self.euler.points.is_empty()
    }

    fn axis_value(&self, k: usize, t: f32, actors: &dyn GroupActors) -> f32 {
        let tension = if k < 3 {
            self.lin_curve_tension
        } else {
            self.ang_curve_tension
        };
        self.axes.get(k).map_or(0.0, |a| a.eval(t, actors, tension))
    }

    /// Relative position at `t` (`EvalPositionAtTime`): sub-tracks 0..3
    /// when split, else `PosTrack` with lookup keys resolved (zero without
    /// keys). Identical, operation for operation, to [`InterpCurve::eval`]
    /// for curves without lookup keys (CONFIRMED from the disassembly).
    pub fn eval_position(&self, t: f32, actors: &dyn GroupActors) -> [f32; 3] {
        if !self.axes.is_empty() {
            return [
                self.axis_value(0, t, actors),
                self.axis_value(1, t, actors),
                self.axis_value(2, t, actors),
            ];
        }
        match lookup_curve(
            &self.pos,
            &self.lookup,
            |g| actors.location(g),
            self.lin_curve_tension,
        ) {
            Some(c) => c.eval(t, [0.0; 3]),
            None => self.pos.eval(t, [0.0; 3]),
        }
    }

    /// Relative Euler rotation (degrees: roll, pitch, yaw) at `t`
    /// (`EvalRotationAtTime`).
    pub fn eval_euler(&self, t: f32, actors: &dyn GroupActors) -> [f32; 3] {
        if !self.axes.is_empty() {
            return [
                self.axis_value(3, t, actors),
                self.axis_value(4, t, actors),
                self.axis_value(5, t, actors),
            ];
        }
        match lookup_curve(
            &self.euler,
            &self.lookup,
            |g| actors.rotation(g).map(euler_from_rotator),
            self.ang_curve_tension,
        ) {
            Some(c) => c.eval(t, [0.0; 3]),
            None => self.euler.eval(t, [0.0; 3]),
        }
    }

    fn key_euler(&self, i: usize, actors: &dyn GroupActors) -> [f32; 3] {
        self.lookup
            .get(i)
            .and_then(|k| k.group.as_deref())
            .and_then(|g| actors.rotation(g))
            .map(euler_from_rotator)
            .or_else(|| self.euler.points.get(i).map(|p| p.out_val))
            .unwrap_or([0.0; 3])
    }

    /// Quaternion rotation path of `GetKeyTransformAtTime` (CONFIRMED):
    /// slerp between the quaternions of the two Euler keys around `t` (alpha
    /// clamped to `[0, 1]`, key modes ignored); the first or last key
    /// outside the key range; identity without keys.
    fn quat_rotation(&self, t: f32, actors: &dyn GroupActors) -> [i32; 3] {
        let keys = &self.euler.points;
        let n = keys.len();
        let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
            return rotator_from_quat(QUAT_IDENTITY);
        };
        let at = |i: usize| rotator_from_quat(quat_from_euler(self.key_euler(i, actors)));
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
            quat_from_euler(self.key_euler(i - 1, actors)),
            quat_from_euler(self.key_euler(i, actors)),
            alpha,
        );
        rotator_from_quat(q)
    }

    /// Relative key transform at `t` (`GetKeyTransformAtTime`): position
    /// and rotator. The rotation is `MakeFromEuler` of the Euler curve
    /// (truncating) unless the track uses quaternion interpolation without
    /// sub-tracks.
    pub fn key_transform(&self, t: f32, actors: &dyn GroupActors) -> ([f32; 3], [i32; 3]) {
        let rot = if self.axes.is_empty() && self.use_quat_interpolation {
            self.quat_rotation(t, actors)
        } else {
            rotator_from_euler(self.eval_euler(t, actors))
        };
        (self.eval_position(t, actors), rot)
    }

    /// `InitialTM` for an unattached, non-AI actor at `location` /
    /// `rotation` (`CalcInitialTransform`, STRONG structure): the actor
    /// transform, preceded by the inverse of the track's key transform at
    /// `position` unless `bUseRawActorTMforRelativeToInitial`, with scale
    /// removed. Relative tracks therefore leave the actor where it stands at
    /// `position` (see [`MoveInstance::new`] for which time the engine
    /// passes). The engine inverts with the general `FMatrix::Inverse` and
    /// sums matrix products from the last column down; we use the rigid
    /// inverse and natural order, which can differ in the last bits
    /// (TENTATIVE: never by a whole rotator unit after rounding).
    pub fn initial_transform(
        &self,
        location: [f32; 3],
        rotation: [i32; 3],
        position: f32,
        actors: &dyn GroupActors,
    ) -> Matrix {
        self.initial_from_matrix(
            &rotation_translation_matrix(rotation, location),
            position,
            actors,
        )
    }

    /// [`MoveTrack::initial_transform`] from the actor's transform as a
    /// matrix (relative to its base when attached).
    pub fn initial_from_matrix(
        &self,
        actor: &Matrix,
        position: f32,
        actors: &dyn GroupActors,
    ) -> Matrix {
        let initial = if self.use_raw_actor_tm {
            *actor
        } else {
            let (p0, r0) = self.key_transform(position, actors);
            matrix_mul(&rigid_inverse(&rotation_translation_matrix(r0, p0)), actor)
        };
        remove_scaling(&initial)
    }

    /// The reference frame keys are relative to (`GetMoveRefFrame`,
    /// CONFIRMED structure): the base transform (identity when unattached)
    /// for `IMF_World`, `InitialTM · base` without scale for
    /// `IMF_RelativeToInitial`.
    pub fn ref_frame(&self, inst: &MoveInstance) -> Matrix {
        let base = inst.base.unwrap_or(IDENTITY);
        match self.move_frame {
            MoveFrame::World => base,
            MoveFrame::RelativeToInitial => remove_scaling(&matrix_mul(&inst.initial, &base)),
        }
    }

    /// World location and rotation of the actor at `t`
    /// (`GetLocationAtTime`), or `None` for an inactive track
    /// ([`MoveTrack::is_active`]).
    pub fn sample(
        &self,
        t: f32,
        inst: &MoveInstance,
        actors: &dyn GroupActors,
    ) -> Option<MoveSample> {
        if !self.is_active() {
            return None;
        }
        let (lp, lr) = self.key_transform(t, actors);
        let (location, rot) = world_key_transform(lp, lr, &self.ref_frame(inst));
        let rotation = match self.rot_mode {
            RotMode::Keyframed => MoveRotation::Set(rot),
            RotMode::Ignore => MoveRotation::Keep,
            RotMode::LookAtGroup => match &self.look_at_group {
                Some(g) => MoveRotation::LookAt(g.clone()),
                None => MoveRotation::Set(rot),
            },
        };
        Some(MoveSample { location, rotation })
    }
}

/// `ComputeWorldSpaceKeyTransform` (structure CONFIRMED, exact operation
/// order TENTATIVE): the position is transformed by the reference frame;
/// the rotation is split into whole turns (winding) and a remainder in
/// `[-180°, 180°)`; the remainder's matrix is transformed by the frame and
/// converted back with [`matrix_rotator`] (cleaned, normalized); the
/// winding, as Euler degrees divided by 360, is rotated by the frame,
/// rounded to whole turns, scaled back by 360 and added with
/// `MakeFromEuler`. This keeps multi-turn spins (rotors, wheels) intact
/// through the frame change.
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

impl EventTrack {
    /// Keys that fire when the sequence moves from `last` to `new`
    /// (`UInterpTrackEvent::UpdateTrack`, STRONG from the decompilation and
    /// disassembly).
    ///
    /// `playing_reverse` is "the action is playing and playing in reverse"
    /// (`bIsPlaying && bReversePlayback`). The update runs backwards when
    /// `playing_reverse`, or for a jump to an earlier position while the
    /// action is not playing. (The engine treats a jump to an earlier
    /// position while playing forwards as a forwards update with an empty
    /// window, which fires nothing, exactly as a disallowed backwards jump
    /// does; so `playing_reverse || (jump && new < last)` gives the same
    /// keys.) In particular the wrap of a reverse-looping action (a jump from
    /// 0 to the end while playing in reverse) fires nothing. Forwards, keys
    /// with `last <= time < new` fire (the window is widened by `1e-4` when
    /// `new` is exactly `length`, so a key at the very end fires); backwards,
    /// keys with `new < time <= last` (widened by `1e-4` when `new` is 0).
    /// Normal updates obey `bFireEventsWhenForwards` /
    /// `bFireEventsWhenBackwards`; jumps fire only forwards, and only with
    /// both `bFireEventsWhenJumpingForwards` and `bFireEventsWhenForwards`.
    pub fn fired_keys(
        &self,
        last: f32,
        new: f32,
        length: f32,
        playing_reverse: bool,
        jump: bool,
    ) -> Vec<usize> {
        let backwards = playing_reverse || (jump && new < last);
        let allowed = if jump {
            !backwards && self.fire_jumping_forwards && self.fire_forwards
        } else if backwards {
            self.fire_backwards
        } else {
            self.fire_forwards
        };
        if !allowed {
            return Vec::new();
        }
        let mut out = Vec::new();
        if backwards {
            let lo = if new == 0.0 { new + -1.0e-4 } else { new };
            for (i, k) in self.keys.iter().enumerate() {
                if lo < k.time && k.time <= last {
                    out.push(i);
                }
            }
        } else {
            let hi = if new == length { new + 1.0e-4 } else { new };
            for (i, k) in self.keys.iter().enumerate() {
                if last <= k.time && k.time < hi {
                    out.push(i);
                }
            }
        }
        out
    }
}

impl DirectorTrack {
    /// Index of the cut in effect at `t` (`GetKeyframeIndex`, CONFIRMED):
    /// none before (or exactly at) the first cut's time; otherwise the last
    /// cut whose time is at or before `t`, scanning in stored order.
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

    /// The group whose actor is viewed at `t`, the cut's time and its
    /// transition time (`GetViewedGroupName`, CONFIRMED): the director
    /// group's own name (meaning the player's own camera) with zero times
    /// before the first cut.
    pub fn viewed_group(&self, t: f32, director_group: &str) -> (String, f32, f32) {
        match self.cut_index(t).and_then(|i| self.cuts.get(i)) {
            Some(c) => (
                c.target_group.clone().unwrap_or_else(|| "None".to_owned()),
                c.time,
                c.transition_time,
            ),
            None => (director_group.to_owned(), 0.0, 0.0),
        }
    }
}

impl FadeTrack {
    /// Screen fade at `t`, clamped to `[0, 1]` (`GetFadeAmountAtTime`,
    /// CONFIRMED; 0 without keys).
    pub fn amount_at(&self, t: f32) -> f32 {
        let v = self.curve.eval(t, 0.0);
        let m = if le(1.0, v) { 1.0 } else { v };
        if lt(v, 0.0) { 0.0 } else { m }
    }
}

/// Global time dilation of a slomo track at `t`, never below 0.1
/// (`GetSlomoFactorAtTime`, CONFIRMED constant).
pub fn slomo_factor(curve: &InterpCurve<f32>, t: f32) -> f32 {
    let v = curve.eval(t, 0.0);
    if le(v, 0.1) { 0.1 } else { v }
}

// ================================================================== playback

/// What one [`Playback::step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StepResult {
    /// Position before the step.
    pub from: f32,
    /// Position after the step.
    pub to: f32,
    /// The sequence wrapped around (looping): tracks were updated to the
    /// end (or start) first, then jumped to the other end.
    pub wrapped: bool,
    /// Relative move tracks must recompute their initial transforms at the
    /// wrap (`bNoResetOnRewind`), so motion accumulates loop after loop.
    pub reset_initial_transforms: bool,
    /// Playback reached the end (or the start when reversed) and stopped;
    /// the `Completed` (or `Reversed`) output fires.
    pub finished: bool,
}

/// The `SeqAct_Interp` inputs pulsed in one Kismet update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PlaybackInputs {
    /// `Play` (input 0).
    pub play: bool,
    /// `Reverse` (input 1).
    pub reverse: bool,
    /// `Stop` (input 2).
    pub stop: bool,
    /// `Pause` (input 3).
    pub pause: bool,
    /// `Change Dir` (input 4).
    pub change_dir: bool,
}

/// A position jump requested by [`Playback::play`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Jump {
    /// New position (tracks jump without firing events in between).
    pub to: f32,
    /// Relative move tracks recompute their initial transforms first.
    pub reset_initial_transforms: bool,
}

/// `SeqAct_Interp` playback state (`Play`, `Reverse`, `Stop`, `Pause`,
/// `Change Dir` inputs and `StepInterp`; STRONG from the decompiled native
/// functions, see `MATINEE.md`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Playback {
    /// `Position` in seconds.
    pub position: f32,
    /// `bIsPlaying`.
    pub playing: bool,
    /// `bPaused`.
    pub paused: bool,
    /// `bReversePlayback`.
    pub reverse: bool,
    /// `InterpData.InterpLength`.
    pub length: f32,
    /// Settings.
    pub settings: InterpSettings,
}

impl Playback {
    /// Stopped at position 0.
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

    /// `Play` input: jump to `ForceStartPosition` when `bForceStartPos` and
    /// not playing; else rewind to 0 when `bRewindOnPlay` and (not playing
    /// or `bRewindIfAlreadyPlaying`). Then play forwards, unpaused.
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

    /// `Reverse` input: play backwards, unpaused.
    pub fn play_reverse(&mut self) {
        self.playing = true;
        self.paused = false;
        self.reverse = true;
    }

    /// `Change Dir` input: play, unpaused, in the other direction.
    pub fn change_direction(&mut self) {
        self.playing = true;
        self.paused = false;
        self.reverse = !self.reverse;
    }

    /// `Pause` input: toggle the pause of a playing sequence; ignored while
    /// stopped (STRONG: `UpdateOp` flips the paused flag only while playing).
    pub fn pause(&mut self) {
        if self.playing {
            self.paused = !self.paused;
        }
    }

    /// True when pulsing `inputs` (re)initialises the action
    /// (`USeqAct_Interp::Activated`, STRONG): a stopped action is started
    /// only by Play, Reverse or Change Dir, and starting it rebuilds the
    /// group and track instances first, so the caller recomputes every
    /// [`MoveInstance`] at the current [`Playback::position`] before calling
    /// [`Playback::apply_inputs`]. This assumes the stopped action was
    /// deactivated, which the engine does at its first update without
    /// playing; an input in the very update after Stop still reaches the
    /// active action through `UpdateOp` and rebuilds nothing.
    pub fn needs_init(&self, inputs: PlaybackInputs) -> bool {
        !self.playing && (inputs.play || inputs.reverse || inputs.change_dir)
    }

    /// Apply the inputs pulsed in one Kismet update, one of them at most,
    /// with the engine's precedence (`USeqAct_Interp::UpdateOp`, STRONG):
    /// Pause while playing, then Play, Reverse, Stop and Change Dir. Returns
    /// the jump requested by Play, if any.
    pub fn apply_inputs(&mut self, inputs: PlaybackInputs) -> Option<Jump> {
        if self.playing && inputs.pause {
            self.pause();
            None
        } else if inputs.play {
            self.play()
        } else if inputs.reverse {
            self.play_reverse();
            None
        } else if inputs.stop {
            self.stop();
            None
        } else if inputs.change_dir {
            self.change_direction();
            None
        } else {
            None
        }
    }

    /// `Stop` input (or the end of a non-looping play): not playing, not
    /// paused; the position is kept.
    pub fn stop(&mut self) {
        self.playing = false;
        self.paused = false;
    }

    /// Advance by `dt` seconds (`StepInterp`, STRONG): only while playing
    /// and not paused. The position moves by `dt · PlayRate` in the play
    /// direction. Past an end, a looping sequence updates to that end, jumps
    /// to the other end and wraps by whole lengths (only the forwards wrap
    /// re-bases relative initial transforms, and only with
    /// `bNoResetOnRewind`); a non-looping one clamps to the end and stops.
    /// Safety deviations: a looping sequence whose length is not positive
    /// clamps and stops instead of looping forever, and one step wraps at
    /// most a million lengths.
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
        // The comparisons are the native ones (`0 <= p`, `p <= len`), so a
        // NaN position counts as past the end, as in the engine.
        if self.reverse {
            let mut p = from - dt * rate;
            if !le(0.0, p) {
                if self.settings.looping && len > 0.0 {
                    out.wrapped = true;
                    let mut guard = 0u32;
                    while p < 0.0 && guard < 1_000_000 {
                        p += len;
                        guard += 1;
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
                    while len < p && guard < 1_000_000 {
                        p -= len;
                        guard += 1;
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
