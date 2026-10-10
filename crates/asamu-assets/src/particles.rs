//! Runtime particle effects from the user-local output of `asamu-import
//! particles` (`particles/particles.json` and `particles/maps/<Map>.json`):
//! a typed model of the converted systems, the distribution evaluator and a
//! deterministic CPU simulator of the UE3 emitter instance, render-agnostic.
//! The Bevy app turns [`SystemInstance::render`] into camera-facing quads.
//!
//! Behaviour follows the shipped executable where it was read (see
//! `docs/reverse-engineering/PARTICLES.md` for evidence and confidence):
//!
//! - **Distributions** evaluate the decoded distribution object, as the game
//!   does whenever one is set; curves use the engine's `FInterpCurve::Eval`
//!   arithmetic (the same rules `asamu_ue3::matinee` documents); uniform
//!   values are `max + (min − max) · r`; vector locks, mirrors and extremes
//!   follow `UDistributionVectorUniform::GetValue`.
//! - **Random numbers** use the engine's generator shape (the seed times
//!   `0x0BB38435` plus `0x3619636B`, a mantissa in `[1, 2)` minus 1), seeded per system
//!   instance so a run is reproducible; the original's global seed sequence
//!   is not reproduced (it depends on everything else the game draws).
//! - **The component** around the emitters follows the executable as well:
//!   activation initializes the emitter instances again and keeps their
//!   live particles, the system's `Delay` adds to every emitter's delay,
//!   warm-up ticks are 0.032 s unless the system sets a rate, a fixed-time
//!   system advances by its own delta per tick, the LOD level is chosen by
//!   `DetermineLODLevelForLocation`'s scan under the system's LOD method,
//!   an emitter on a disabled LOD level is not ticked, and a system that
//!   completes is deactivated.
//! - **Emitter time**, looping, delay, duration ranges, spawn rate with
//!   leftover fraction, bursts, spawn-time sub-stepping, `PreSpawn` /
//!   `PostSpawn`, the per-frame reset of velocity/size/colour/rotation rate,
//!   the kill of particles whose relative time passed 1, and the order
//!   time setup → kill → spawn → reset → module updates → orbit and
//!   integration follow `FParticleEmitterInstance::Tick` and its helpers
//!   (the order is the executable's: the reset runs *after* the spawn, so a
//!   new particle ages one step in the tick that spawns it and whatever a
//!   spawn module did to the transient size, colour or rotation rate is put
//!   back to the base values before the update modules run).
//!
//! Modules simulated: required, spawn, lifetime, size, size multiply life,
//! size scale, size scale by time, velocity, velocity over lifetime,
//! velocity inherit parent (no parent: no-op), acceleration, colour, colour
//! over life, colour scale over life, rotation, rotation rate, rotation rate
//! multiply life, mesh rotation / rate (as sprite rotation), location,
//! location primitive sphere / cylinder, location emitter (approximated: the
//! emitter origin), sub-UV, orbit, attractor point / line, kill box / height.
//! Collision is skipped (particles pass through the world); beams are
//! straight segments; mesh emitters render their particles as sprites.
//! Everything not listed is ignored and counted in
//! [`SystemDef::unsupported`].
//!
//! Converted data is untrusted input: every count is bounded, missing
//! values take the UE3 zero defaults, and nothing here panics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_core::glam::{Mat3, Mat4, Vec3};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{AssetError, AssetResult};

use crate::files::{MAX_MANIFEST_BYTES, parse_json, read_bounded};
/// JSON value type of the converted files (for callers without a
/// `serde_json` dependency).
pub use serde_json::Value as JsonValue;

/// `format` of `particles/particles.json`.
pub const PARTICLES_FORMAT: &str = "asamu-particles";
/// `version` of `particles/particles.json`.
pub const PARTICLES_VERSION: u32 = 1;
/// `format` of a map's placement file.
pub const PLACEMENTS_FORMAT: &str = "asamu-particle-placements";
/// `version` of a map's placement file.
pub const PLACEMENTS_VERSION: u32 = 1;
/// `MaxParticleVertexMemory` of `[Engine.Engine]` (bytes; the value of
/// the shipped `BaseEngine.ini` and `Mac-ASAMUEngine.ini`, CONFIRMED from
/// the user's install).
pub const MAX_PARTICLE_VERTEX_MEMORY: usize = 131_972;
/// Most live particles of an emitter without sub-UV interpolation: the
/// engine's `MaxParticleSpriteCount = MaxParticleVertexMemory / 272`
/// (four 68-byte sprite vertices per particle; divisor CONFIRMED from the
/// executable). `Spawn` clamps the tick's bursts, then its rate particles,
/// to what is left of it.
pub const MAX_SPRITE_PARTICLES: usize = MAX_PARTICLE_VERTEX_MEMORY / 272;
/// The same for emitters with a sub-UV interpolation method:
/// `MaxParticleSubUVCount = MaxParticleVertexMemory / 368` (CONFIRMED).
pub const MAX_SUBUV_PARTICLES: usize = MAX_PARTICLE_VERTEX_MEMORY / 368;
/// `MaxParticleResize` of the shipped `DefaultEngine.ini` and
/// `Mac-ASAMUEngine.ini`: an emitter's particle storage never grows past it
/// (a hard bound here).
pub const MAX_PARTICLES_PER_EMITTER: usize = 1024;
/// Most particles requested from one emitter in one tick before the
/// engine's clamp (a bound on hostile spawn rates).
pub const MAX_SPAWN_PER_TICK: usize = 1 << 20;
/// Most curve keys kept per curve.
pub const MAX_CURVE_KEYS: usize = 4096;
/// Most emitters per system, LODs per emitter, modules per LOD.
pub const MAX_EMITTERS: usize = 256;
/// Most LOD levels per emitter.
pub const MAX_LODS: usize = 16;
/// Most modules per LOD level.
pub const MAX_MODULES: usize = 256;
/// Largest time step simulated at once (longer steps are split).
pub const MAX_STEP: f32 = 0.1;
/// Longest warm-up simulated (seconds).
pub const MAX_WARMUP: f32 = 30.0;
/// Warm-up tick length when the system sets no `WarmupTickRate` (the
/// constant `UParticleSystemComponent::ActivateSystem` falls back to,
/// CONFIRMED from the executable).
pub const WARMUP_TICK: f32 = 0.032;
/// Most placements read from one map's placement file (the shipped maps
/// have at most 43).
pub const MAX_PLACEMENTS: usize = 4096;
/// Most instance parameters kept per placement.
pub const MAX_INSTANCE_PARAMS: usize = 256;
/// Most particle systems kept from `particles.json` (100 are shipped).
pub const MAX_SYSTEMS: usize = 16_384;
/// `KINDA_SMALL_NUMBER` (the executable's legacy emitter-time threshold).
const KINDA_SMALL: f32 = 1.0e-4;
/// `SMALL_NUMBER` (vector normalization threshold in the modules).
const SMALL: f32 = 1.0e-8;
/// Turns → radians for particle rotation (the executable multiplies by the
/// double 2π, CONFIRMED).
const TURN: f64 = std::f64::consts::TAU;

// ---------------------------------------------------------------------------
// Random numbers
// ---------------------------------------------------------------------------

/// The engine's random stream shape (`appSRand` / `FRandomStream`),
/// seedable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rng(pub u32);

impl Rng {
    /// A stream from a 64-bit seed (folded).
    pub fn new(seed: u64) -> Rng {
        Rng((seed as u32) ^ ((seed >> 32) as u32) ^ 0x2545_F491)
    }

    /// Next value in `[0, 1)`: `seed = seed · 0x0BB38435 + 0x3619636B`, the
    /// low 23 bits as the mantissa of a float in `[1, 2)`, minus its integer
    /// part (CONFIRMED arithmetic of the executable's `appSRand`).
    pub fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(0x0BB3_8435).wrapping_add(0x3619_636B);
        let f = f32::from_bits((self.0 & 0x007F_FFFF) | 0x3F80_0000);
        f - f.trunc()
    }

    /// Advance without using the value (the engine draws and discards in a
    /// few places).
    pub fn skip(&mut self) {
        let _ = self.next_f32();
    }
}

// ---------------------------------------------------------------------------
// Curves and distributions
// ---------------------------------------------------------------------------

/// Curve segment mode (`EInterpCurveMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveMode {
    /// `CIM_Linear`.
    Linear,
    /// `CIM_Constant`.
    Constant,
    /// Every cubic mode (auto, user, break, auto clamped): the stored
    /// tangents are used.
    Cubic,
}

/// One curve key.
#[derive(Debug, Clone, PartialEq)]
pub struct CurveKey {
    /// `InVal`.
    pub t: f32,
    /// `OutVal`.
    pub v: Vec<f32>,
    /// `ArriveTangent`.
    pub arrive: Vec<f32>,
    /// `LeaveTangent`.
    pub leave: Vec<f32>,
    /// Mode of the segment that starts here.
    pub mode: CurveMode,
}

/// A curve of `dim` components.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Curve {
    /// Components per value.
    pub dim: usize,
    /// Keys.
    pub keys: Vec<CurveKey>,
    /// Tangents are not multiplied by the segment span.
    pub broken_tangents: bool,
}

/// Cubic Hermite blend in the executable's operation order
/// (`((h00·p0 + h10·t0) + h11·t1) + h01·p1`).
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

fn comp(v: &[f32], i: usize) -> f32 {
    v.get(i).copied().unwrap_or(0.0)
}

impl Curve {
    /// Value at `t` (`FInterpCurve::Eval`): no keys → zeros; one key or
    /// `t` at/before the first key → the first value; `t` at/after the last
    /// key (or NaN) → the last value; else the segment whose end key is the
    /// first with `in > t`, governed by its start key's mode.
    pub fn eval(&self, t: f32) -> Vec<f32> {
        let dim = self.dim.clamp(1, 6);
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return vec![0.0; dim];
        };
        let value = |k: &CurveKey| (0..dim).map(|i| comp(&k.v, i)).collect::<Vec<f32>>();
        if self.keys.len() < 2 || t <= first.t {
            return value(first);
        }
        if t.partial_cmp(&last.t) != Some(std::cmp::Ordering::Less) {
            return value(last);
        }
        for pair in self.keys.windows(2) {
            let [k0, k1] = pair else { continue };
            if t < k1.t {
                let d = k1.t - k0.t;
                if d.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
                    || k0.mode == CurveMode::Constant
                {
                    return value(k0);
                }
                let a = (t - k0.t) / d;
                return (0..dim)
                    .map(|i| {
                        let (p0, p1) = (comp(&k0.v, i), comp(&k1.v, i));
                        if k0.mode == CurveMode::Linear {
                            a * (p1 - p0) + p0
                        } else {
                            let (t0, t1) = if self.broken_tangents {
                                (comp(&k0.leave, i), comp(&k1.arrive, i))
                            } else {
                                (comp(&k0.leave, i) * d, d * comp(&k1.arrive, i))
                            };
                            hermite(p0, t0, p1, t1, a)
                        }
                    })
                    .collect();
            }
        }
        value(last)
    }
}

/// A distribution as the runtime evaluates it.
#[derive(Debug, Clone, PartialEq)]
pub enum Dist {
    /// No distribution (UE3 returns zero).
    Zero,
    /// Constant.
    Constant {
        /// Value (1 or 3 components).
        value: [f32; 3],
        /// `LockedAxes`.
        lock: u8,
    },
    /// Uniform.
    Uniform {
        /// `Min`.
        min: [f32; 3],
        /// `Max`.
        max: [f32; 3],
        /// `LockedAxes`.
        lock: u8,
        /// `MirrorFlags`.
        mirror: [u8; 3],
        /// `bUseExtremes`.
        extremes: bool,
    },
    /// Constant curve.
    Curve {
        /// The curve.
        curve: Curve,
        /// `LockedAxes`.
        lock: u8,
    },
    /// Uniform curve (`[min.., max..]` per key: 2 components for floats, 6
    /// for vectors).
    UniformCurve {
        /// The curve.
        curve: Curve,
        /// `LockedAxes[2]`.
        lock: [u8; 2],
        /// `MirrorFlags`.
        mirror: [u8; 3],
        /// `bUseExtremes`.
        extremes: bool,
    },
    /// A particle (or sound) parameter read from the component.
    Parameter {
        /// `ParameterName`.
        name: String,
        /// Modes per component.
        modes: [u8; 3],
        /// `MinInput`.
        min_input: [f32; 3],
        /// `MaxInput`.
        max_input: [f32; 3],
        /// `MinOutput`.
        min_output: [f32; 3],
        /// `MaxOutput`.
        max_output: [f32; 3],
        /// `Constant`.
        constant: [f32; 3],
    },
    /// Baked lookup table (no distribution object).
    Lookup {
        /// `Op`.
        op: u8,
        /// `LookupTableChunkSize`.
        chunk: u8,
        /// Table.
        table: Vec<f32>,
        /// `LookupTableTimeScale`.
        time_scale: f32,
        /// `LookupTableStartTime`.
        start_time: f32,
    },
}

/// Instance parameters a component provides (`InstanceParameters`, Kismet
/// `SetParticleSysParam`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceParams {
    /// Scalars by lower-case name.
    pub scalars: BTreeMap<String, f32>,
    /// Vectors (and colours as RGB) by lower-case name.
    pub vectors: BTreeMap<String, [f32; 3]>,
}

fn lock3(mut v: [f32; 3], lock: u8) -> [f32; 3] {
    match lock {
        1 => v[1] = v[0],
        2 => v[2] = v[0],
        3 => v[2] = v[1],
        4 => {
            v[1] = v[0];
            v[2] = v[0];
        }
        _ => {}
    }
    v
}

fn map_param(v: f32, mode: u8, min_in: f32, max_in: f32, min_out: f32, max_out: f32) -> f32 {
    if mode == 2 {
        return v;
    }
    let v = if mode == 1 { v.abs() } else { v };
    let gradient = if max_in <= min_in {
        0.0
    } else {
        (max_out - min_out) / (max_in - min_in)
    };
    let clamped = if v < min_in {
        min_in
    } else if v > max_in {
        max_in
    } else {
        v
    };
    min_out + (clamped - min_in) * gradient
}

/// `FRawDistribution` lookup read (op none), `dim` components.
pub fn lookup_value(
    table: &[f32],
    chunk: u8,
    scale: f32,
    start: f32,
    time: f32,
    dim: usize,
) -> Option<Vec<f32>> {
    let chunk = usize::from(chunk);
    if chunk == 0 || table.len() < chunk.checked_add(2)? || dim > chunk {
        return None;
    }
    let t = (time - start) * scale;
    let t = if t >= 0.0 { t } else { 0.0 };
    let i = t as usize;
    let alpha = t - i as f32;
    let last = table.len() - chunk;
    let a = i.saturating_mul(chunk).saturating_add(2).min(last);
    let b = i
        .saturating_mul(chunk)
        .saturating_add(2)
        .saturating_add(chunk)
        .min(last);
    Some(
        (0..dim)
            .map(|c| {
                let x = comp(table, a + c);
                let y = comp(table, b + c);
                alpha * (y - x) + x
            })
            .collect(),
    )
}

impl Dist {
    /// Float value at `t`.
    pub fn eval_f32(&self, t: f32, rng: &mut Rng, params: &InstanceParams) -> f32 {
        match self {
            Dist::Zero => 0.0,
            Dist::Constant { value, .. } => value[0],
            Dist::Uniform { min, max, .. } => {
                let r = rng.next_f32();
                max[0] + (min[0] - max[0]) * r
            }
            Dist::Curve { curve, .. } => comp(&curve.eval(t), 0),
            Dist::UniformCurve { curve, .. } => {
                let v = curve.eval(t);
                let r = rng.next_f32();
                (comp(&v, 1) - comp(&v, 0)) * r + comp(&v, 0)
            }
            Dist::Parameter {
                name,
                modes,
                min_input,
                max_input,
                min_output,
                max_output,
                constant,
            } => {
                let v = params
                    .scalars
                    .get(&name.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(constant[0]);
                map_param(
                    v,
                    modes[0],
                    min_input[0],
                    max_input[0],
                    min_output[0],
                    max_output[0],
                )
            }
            Dist::Lookup {
                op,
                chunk,
                table,
                time_scale,
                start_time,
            } => match op {
                2 => {
                    let r = rng.next_f32();
                    let lo = lookup_value(table, *chunk, *time_scale, *start_time, t, 2)
                        .unwrap_or_default();
                    (comp(&lo, 1) - comp(&lo, 0)) * r + comp(&lo, 0)
                }
                1 | 3 => lookup_value(table, *chunk, *time_scale, *start_time, t, 1)
                    .map_or(0.0, |v| comp(&v, 0)),
                _ => 0.0,
            },
        }
    }

    /// Vector value at `t` (`extreme`: 0 random, > 0 max, < 0 min).
    pub fn eval_vec(&self, t: f32, rng: &mut Rng, params: &InstanceParams) -> Vec3 {
        let v = match self {
            Dist::Zero => [0.0; 3],
            Dist::Constant { value, lock } => lock3(*value, *lock),
            Dist::Uniform {
                min,
                max,
                lock,
                mirror,
                extremes,
            } => uniform_vector(*min, *max, *lock, *mirror, *extremes, rng),
            Dist::Curve { curve, lock } => {
                let v = curve.eval(t);
                lock3([comp(&v, 0), comp(&v, 1), comp(&v, 2)], *lock)
            }
            Dist::UniformCurve {
                curve,
                lock,
                mirror,
                extremes,
            } => {
                let v = curve.eval(t);
                let hi = lock3([comp(&v, 0), comp(&v, 1), comp(&v, 2)], lock[0]);
                let mut lo = lock3([comp(&v, 3), comp(&v, 4), comp(&v, 5)], lock[1]);
                for i in 0..3 {
                    lo[i] = match mirror[i] {
                        0 => hi[i],
                        2 => -hi[i],
                        _ => lo[i],
                    };
                }
                if *extremes {
                    if rng.next_f32() > 0.5 { hi } else { lo }
                } else {
                    let mut out = [0.0; 3];
                    for i in 0..3 {
                        out[i] = hi[i] + (lo[i] - hi[i]) * rng.next_f32();
                    }
                    out
                }
            }
            Dist::Parameter {
                name,
                modes,
                min_input,
                max_input,
                min_output,
                max_output,
                constant,
            } => {
                let p = params
                    .vectors
                    .get(&name.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(*constant);
                let mut out = [0.0; 3];
                for i in 0..3 {
                    out[i] = map_param(
                        p[i],
                        modes[i],
                        min_input[i],
                        max_input[i],
                        min_output[i],
                        max_output[i],
                    );
                }
                out
            }
            Dist::Lookup {
                op,
                chunk,
                table,
                time_scale,
                start_time,
            } => match op {
                2 => {
                    let e = lookup_value(table, *chunk, *time_scale, *start_time, t, 6)
                        .unwrap_or_default();
                    let mut out = [0.0; 3];
                    for (i, o) in out.iter_mut().enumerate() {
                        let r = rng.next_f32();
                        *o = (comp(&e, i + 3) - comp(&e, i)) * r + comp(&e, i);
                    }
                    out
                }
                1 | 3 => lookup_value(table, *chunk, *time_scale, *start_time, t, 3)
                    .map_or([0.0; 3], |v| [comp(&v, 0), comp(&v, 1), comp(&v, 2)]),
                _ => [0.0; 3],
            },
        };
        Vec3::from_array(v)
    }

    /// True for a distribution that never draws random numbers and does not
    /// depend on time (`Constant` / `Zero`).
    pub fn is_constant(&self) -> bool {
        matches!(self, Dist::Zero | Dist::Constant { .. })
    }
}

/// `UDistributionVectorUniform::GetValue` (CONFIRMED draw order: the
/// extreme choice first when `bUseExtremes`, then one draw per unlocked
/// component X, Y, Z; `value = max + (min' − max) · r` where `min'` is
/// `Min`, `Max` (same) or `−Max` (mirror) per component).
fn uniform_vector(
    min: [f32; 3],
    max: [f32; 3],
    lock: u8,
    mirror: [u8; 3],
    extremes: bool,
    rng: &mut Rng,
) -> [f32; 3] {
    let mut lo = [0.0f32; 3];
    for i in 0..3 {
        lo[i] = match mirror[i] {
            0 => max[i],
            2 => -max[i],
            _ => min[i],
        };
    }
    let pick_min = extremes && rng.next_f32() <= 0.5;
    let pick = |i: usize| if pick_min { lo[i] } else { max[i] };
    let mut draw = |i: usize| max[i] + (lo[i] - max[i]) * rng.next_f32();
    match lock {
        1 => {
            // X = Y, Z own.
            let (x, z) = if extremes {
                (pick(0), pick(2))
            } else {
                (draw(0), draw(2))
            };
            [x, x, z]
        }
        2 => {
            let (x, y) = if extremes {
                (pick(0), pick(1))
            } else {
                (draw(0), draw(1))
            };
            [x, y, x]
        }
        3 => {
            let (x, y) = if extremes {
                (pick(0), pick(1))
            } else {
                (draw(0), draw(1))
            };
            [x, y, y]
        }
        4 => {
            let x = if extremes { pick(0) } else { draw(0) };
            [x, x, x]
        }
        _ => {
            if extremes {
                [pick(0), pick(1), pick(2)]
            } else {
                let x = draw(0);
                let y = draw(1);
                let z = draw(2);
                [x, y, z]
            }
        }
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn get<'a>(m: &'a Value, name: &str) -> Option<&'a Value> {
    let obj = m.as_object()?;
    obj.get(name).or_else(|| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

fn jf32(v: Option<&Value>) -> Option<f32> {
    let f = v?.as_f64()? as f32;
    f.is_finite().then_some(f)
}

fn jbool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        _ => None,
    }
}

fn jstr(v: Option<&Value>) -> Option<&str> {
    v?.as_str()
}

fn jvec(v: Option<&Value>) -> Option<Vec3> {
    let v = v?;
    Some(Vec3::new(
        jf32(get(v, "X")).unwrap_or(0.0),
        jf32(get(v, "Y")).unwrap_or(0.0),
        jf32(get(v, "Z")).unwrap_or(0.0),
    ))
}

fn arr3(v: Option<&Value>) -> [f32; 3] {
    let mut out = [0.0; 3];
    if let Some(Value::Array(a)) = v {
        for (o, x) in out.iter_mut().zip(a) {
            *o = jf32(Some(x)).unwrap_or(0.0);
        }
    }
    out
}

fn u8_of(v: Option<&Value>) -> u8 {
    v.and_then(Value::as_u64)
        .and_then(|x| u8::try_from(x).ok())
        .unwrap_or(0)
}

fn mirror_of(v: Option<&Value>) -> [u8; 3] {
    let mut out = [1u8; 3];
    if let Some(Value::Array(a)) = v {
        for (o, x) in out.iter_mut().zip(a) {
            *o = x.as_u64().and_then(|x| u8::try_from(x).ok()).unwrap_or(1);
        }
    }
    out
}

fn curve_of(v: Option<&Value>) -> Curve {
    let Some(v) = v else {
        return Curve::default();
    };
    let dim = get(v, "dim")
        .and_then(Value::as_u64)
        .map_or(1, |d| usize::try_from(d).unwrap_or(1))
        .clamp(1, 6);
    let floats = |x: Option<&Value>| -> Vec<f32> {
        match x {
            Some(Value::Array(a)) => a
                .iter()
                .take(6)
                .map(|f| jf32(Some(f)).unwrap_or(0.0))
                .collect(),
            _ => Vec::new(),
        }
    };
    let keys = match get(v, "keys") {
        Some(Value::Array(a)) => a
            .iter()
            .take(MAX_CURVE_KEYS)
            .filter_map(|k| {
                let t = jf32(get(k, "t"))?;
                Some(CurveKey {
                    t,
                    v: floats(get(k, "v")),
                    arrive: floats(get(k, "arrive")),
                    leave: floats(get(k, "leave")),
                    mode: match jstr(get(k, "mode")).unwrap_or("linear") {
                        "linear" => CurveMode::Linear,
                        "constant" => CurveMode::Constant,
                        _ => CurveMode::Cubic,
                    },
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    Curve {
        dim,
        keys,
        broken_tangents: jbool(get(v, "broken_tangents")).unwrap_or(false),
    }
}

/// A distribution from its JSON form (`{"dist": ..., "value": {...}}`).
pub fn dist_of(v: Option<&Value>) -> Dist {
    let Some(v) = v else {
        return Dist::Zero;
    };
    let Some(inner) = get(v, "value") else {
        return Dist::Zero;
    };
    let kind = jstr(get(inner, "kind")).unwrap_or("");
    match kind {
        "constant" => Dist::Constant {
            value: arr3(get(inner, "value")),
            lock: u8_of(get(inner, "locked_axes")),
        },
        "uniform" => Dist::Uniform {
            min: arr3(get(inner, "min")),
            max: arr3(get(inner, "max")),
            lock: u8_of(get(inner, "locked_axes")),
            mirror: mirror_of(get(inner, "mirror")),
            extremes: jbool(get(inner, "use_extremes")).unwrap_or(false),
        },
        "constant_curve" => Dist::Curve {
            curve: curve_of(get(inner, "curve")),
            lock: u8_of(get(inner, "locked_axes")),
        },
        "uniform_curve" => {
            let locks = arr3(get(inner, "locked_axes"));
            Dist::UniformCurve {
                curve: curve_of(get(inner, "curve")),
                lock: [locks[0] as u8, locks[1] as u8],
                mirror: mirror_of(get(inner, "mirror")),
                extremes: jbool(get(inner, "use_extremes")).unwrap_or(false),
            }
        }
        "parameter" => {
            let modes = arr3(get(inner, "modes"));
            Dist::Parameter {
                name: jstr(get(inner, "name")).unwrap_or("").to_owned(),
                modes: [modes[0] as u8, modes[1] as u8, modes[2] as u8],
                min_input: arr3(get(inner, "min_input")),
                max_input: arr3(get(inner, "max_input")),
                min_output: arr3(get(inner, "min_output")),
                max_output: arr3(get(inner, "max_output")),
                constant: arr3(get(inner, "constant")),
            }
        }
        "lookup" => Dist::Lookup {
            op: u8_of(get(inner, "op")),
            chunk: u8_of(get(inner, "chunk")),
            table: match get(inner, "table") {
                Some(Value::Array(a)) => a
                    .iter()
                    .take(1 << 16)
                    .map(|f| jf32(Some(f)).unwrap_or(0.0))
                    .collect(),
                _ => Vec::new(),
            },
            time_scale: jf32(get(inner, "time_scale")).unwrap_or(0.0),
            start_time: jf32(get(inner, "start_time")).unwrap_or(0.0),
        },
        _ => Dist::Zero,
    }
}

fn enum_is(v: Option<&Value>, name: &str) -> bool {
    jstr(v).is_some_and(|s| s.eq_ignore_ascii_case(name))
}

// ---------------------------------------------------------------------------
// Typed definitions
// ---------------------------------------------------------------------------

/// How sprites face the camera (`EParticleScreenAlignment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAlignment {
    /// `PSA_Square`: square, camera-facing, size X only.
    Square,
    /// `PSA_Rectangle`: camera-facing, size X by Y.
    Rectangle,
    /// `PSA_Velocity`: stretched along the velocity.
    Velocity,
    /// `PSA_TypeSpecific` (mesh emitters) and anything else: like square.
    TypeSpecific,
}

/// Sub-UV interpolation (`EParticleSubUVInterpMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubUvMethod {
    /// `PSUVIM_None`.
    None,
    /// `PSUVIM_Linear`.
    Linear,
    /// `PSUVIM_Linear_Blend`.
    LinearBlend,
    /// `PSUVIM_Random`.
    Random,
    /// `PSUVIM_Random_Blend`.
    RandomBlend,
}

/// One burst (`ParticleBurst`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burst {
    /// `Count`.
    pub count: i32,
    /// `CountLow` (−1: always `Count`).
    pub count_low: i32,
    /// `Time` (emitter time).
    pub time: f32,
}

/// `ParticleModuleRequired` values.
#[derive(Debug, Clone, PartialEq)]
pub struct RequiredDef {
    /// `Material`.
    pub material: Option<String>,
    /// `bUseLocalSpace`.
    pub local_space: bool,
    /// `bKillOnDeactivate`.
    pub kill_on_deactivate: bool,
    /// `bKillOnCompleted`.
    pub kill_on_completed: bool,
    /// `EmitterDuration`.
    pub duration: f32,
    /// `EmitterDurationLow`.
    pub duration_low: f32,
    /// `bEmitterDurationUseRange`.
    pub duration_use_range: bool,
    /// `bDurationRecalcEachLoop`.
    pub duration_recalc: bool,
    /// `EmitterLoops` (0 = forever).
    pub loops: i32,
    /// `EmitterDelay`.
    pub delay: f32,
    /// `EmitterDelayLow`.
    pub delay_low: f32,
    /// `bEmitterDelayUseRange`.
    pub delay_use_range: bool,
    /// `bDelayFirstLoopOnly`.
    pub delay_first_loop_only: bool,
    /// `bUseLegacyEmitterTime`.
    pub legacy_time: bool,
    /// `ScreenAlignment`.
    pub alignment: ScreenAlignment,
    /// `SubImages_Horizontal`.
    pub sub_h: u32,
    /// `SubImages_Vertical`.
    pub sub_v: u32,
    /// `InterpolationMethod`.
    pub sub_uv: SubUvMethod,
    /// `RandomImageChanges` (random sub-UV methods update the image only
    /// when it is non-zero, CONFIRMED).
    pub random_image_changes: i32,
    /// `RandomImageTime` as `FParticleEmitterInstance::Init` derives it:
    /// `0.99 / (RandomImageChanges + 1)`, or 1 without changes (CONFIRMED;
    /// the stored value is overwritten there).
    pub random_image_time: f32,
    /// `bUseMaxDrawCount` / `MaxDrawCount`.
    pub max_draw_count: Option<u32>,
    /// `SpawnRate` (used without a spawn module).
    pub spawn_rate: Dist,
    /// `BurstList` (used without a spawn module).
    pub bursts: Vec<Burst>,
    /// `EmitterOrigin`.
    pub origin: Vec3,
    /// `SortMode` is set (sorted draw).
    pub sorted: bool,
}

/// `ParticleModuleSpawn` values.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnDef {
    /// `Rate`.
    pub rate: Dist,
    /// `RateScale`.
    pub rate_scale: Dist,
    /// `BurstList`.
    pub bursts: Vec<Burst>,
    /// `bProcessSpawnRate`.
    pub process_rate: bool,
    /// `bProcessBurstList`.
    pub process_bursts: bool,
}

/// Orbit options (`OrbitOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OrbitOptions {
    /// `bProcessDuringSpawn`.
    pub spawn: bool,
    /// `bProcessDuringUpdate`.
    pub update: bool,
    /// `bUseEmitterTime`.
    pub emitter_time: bool,
}

/// A simulated module.
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleDef {
    /// `ParticleModuleLifetime`.
    Lifetime(Dist),
    /// `ParticleModuleSize`.
    Size(Dist),
    /// `ParticleModuleSizeMultiplyLife`.
    SizeMultiplyLife {
        /// `LifeMultiplier`.
        dist: Dist,
        /// `MultiplyX/Y/Z`.
        axes: [bool; 3],
    },
    /// `ParticleModuleSizeScale` (`Size = BaseSize · scale`).
    SizeScale {
        /// `SizeScale`.
        dist: Dist,
    },
    /// `ParticleModuleSizeScaleByTime`.
    SizeScaleByTime {
        /// `SizeScaleByTime`.
        dist: Dist,
        /// `bEnableX/Y/Z`.
        axes: [bool; 3],
    },
    /// `ParticleModuleVelocity`.
    Velocity {
        /// `StartVelocity`.
        start: Dist,
        /// `StartVelocityRadial`.
        radial: Dist,
        /// `bInWorldSpace`.
        world: bool,
        /// `bApplyOwnerScale`.
        owner_scale: bool,
    },
    /// `ParticleModuleVelocityOverLifetime`.
    VelocityOverLifetime {
        /// `VelOverLife`.
        dist: Dist,
        /// `Absolute`.
        absolute: bool,
        /// `bInWorldSpace`.
        world: bool,
        /// `bApplyOwnerScale`.
        owner_scale: bool,
    },
    /// `ParticleModuleAcceleration`.
    Acceleration {
        /// `Acceleration`.
        dist: Dist,
        /// `bAlwaysInWorldSpace`.
        world: bool,
        /// `bApplyOwnerScale`.
        owner_scale: bool,
    },
    /// `ParticleModuleColor`.
    Color {
        /// `StartColor`.
        color: Dist,
        /// `StartAlpha`.
        alpha: Dist,
        /// `bClampAlpha`: kept for reference only. The executable reads it
        /// in the editor's curve display and nowhere in `Spawn` / `Update`
        /// (CONFIRMED), so the simulator does not clamp the alpha.
        clamp: bool,
    },
    /// `ParticleModuleColorOverLife`.
    ColorOverLife {
        /// `ColorOverLife`.
        color: Dist,
        /// `AlphaOverLife`.
        alpha: Dist,
        /// `bClampAlpha` (not applied at run time, see [`ModuleDef::Color`]).
        clamp: bool,
    },
    /// `ParticleModuleColorScaleOverLife`.
    ColorScaleOverLife {
        /// `ColorScaleOverLife`.
        color: Dist,
        /// `AlphaScaleOverLife`.
        alpha: Dist,
        /// `bEmitterTime`.
        emitter_time: bool,
    },
    /// `ParticleModuleRotation` / `MeshRotation` (X used).
    Rotation(Dist, bool),
    /// `ParticleModuleRotationRate` / `MeshRotationRate` (X used).
    RotationRate(Dist, bool),
    /// `ParticleModuleRotationRateMultiplyLife`.
    RotationRateMultiplyLife(Dist),
    /// `ParticleModuleLocation`.
    Location(Dist),
    /// `ParticleModuleLocationPrimitiveSphere` / `Cylinder`.
    Primitive {
        /// Sphere (else cylinder).
        sphere: bool,
        /// Axis flags: +X, +Y, +Z, −X, −Y, −Z.
        axes: [bool; 6],
        /// `SurfaceOnly`.
        surface: bool,
        /// `Velocity`.
        velocity: bool,
        /// `VelocityScale`.
        velocity_scale: Dist,
        /// `StartLocation`.
        start: Dist,
        /// `StartRadius`.
        radius: Dist,
        /// `StartHeight` (cylinder).
        height: Dist,
        /// `HeightAxis` (0 X, 1 Y, 2 Z; cylinder).
        height_axis: usize,
        /// `RadialVelocity` (cylinder).
        radial_velocity: bool,
    },
    /// `ParticleModuleSubUV`.
    SubUv(Dist),
    /// `ParticleModuleOrbit`.
    Orbit {
        /// `ChainMode` (0 add, 1 scale, 2 link).
        chain: u8,
        /// `OffsetAmount`.
        offset: Dist,
        /// `OffsetOptions`.
        offset_opts: OrbitOptions,
        /// `RotationAmount`.
        rotation: Dist,
        /// `RotationOptions`.
        rotation_opts: OrbitOptions,
        /// `RotationRateAmount`.
        rate: Dist,
        /// `RotationRateOptions`.
        rate_opts: OrbitOptions,
    },
    /// `ParticleModuleAttractorPoint`.
    AttractorPoint {
        /// `Position`.
        position: Dist,
        /// `Range`.
        range: Dist,
        /// `Strength`.
        strength: Dist,
        /// `StrengthByDistance`.
        by_distance: bool,
        /// `bAffectBaseVelocity`.
        base_velocity: bool,
        /// `bUseWorldSpacePosition`.
        world_position: bool,
    },
    /// `ParticleModuleAttractorLine`.
    AttractorLine {
        /// `EndPoint0`.
        p0: Vec3,
        /// `EndPoint1`.
        p1: Vec3,
        /// `Range`.
        range: Dist,
        /// `Strength`.
        strength: Dist,
    },
    /// `ParticleModuleKillHeight`.
    KillHeight {
        /// `Height`.
        height: Dist,
        /// `bFloor`.
        floor: bool,
        /// `bAbsolute`.
        absolute: bool,
        /// `bApplyPSysScale`: the height is multiplied by the length of the
        /// component's Z axis.
        apply_scale: bool,
    },
    /// `ParticleModuleKillBox`.
    KillBox {
        /// `LowerLeftCorner`.
        lower: Dist,
        /// `UpperRightCorner`.
        upper: Dist,
        /// `bAbsolute`.
        absolute: bool,
        /// `bKillInside`.
        inside: bool,
        /// `bAxisAlignedAndFixedSize`: the box stays world-aligned (without
        /// it, and without `bAbsolute`, a world-space particle is taken into
        /// the component's frame first).
        axis_aligned: bool,
    },
    /// `ParticleModuleOrientationAxisLock` (render: lock flags).
    AxisLock(String),
    /// `ParticleModuleBeamSource` (`Source`; render only).
    BeamSource(Dist),
    /// `ParticleModuleBeamTarget` (`Target`, `bTargetAbsolute`; render only).
    BeamTarget(Dist, bool),
}

/// A module with its stage flags.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleSlot {
    /// Class name.
    pub class: String,
    /// `bSpawnModule`.
    pub spawn: bool,
    /// `bUpdateModule`.
    pub update: bool,
    /// The simulated behaviour.
    pub def: ModuleDef,
}

/// One LOD level.
#[derive(Debug, Clone, PartialEq)]
pub struct LodDef {
    /// `bEnabled`.
    pub enabled: bool,
    /// Required module.
    pub required: RequiredDef,
    /// Spawn module.
    pub spawn: Option<SpawnDef>,
    /// Enabled modules, in order.
    pub modules: Vec<ModuleSlot>,
    /// Module classes not simulated.
    pub unsupported: Vec<String>,
    /// Type data class (`None`: sprites).
    pub type_data: Option<String>,
}

/// What an emitter renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitterKind {
    /// Sprites.
    Sprite,
    /// Meshes (rendered as sprites).
    Mesh,
    /// Beams (approximated).
    Beam,
    /// Trails (approximated).
    Trail,
    /// Anything else.
    Other,
}

/// An emitter.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterDef {
    /// `EmitterName`.
    pub name: String,
    /// Kind.
    pub kind: EmitterKind,
    /// `LODLevels`.
    pub lods: Vec<LodDef>,
    /// `MediumDetailSpawnRateScale` (applied only below high detail; the
    /// runtime runs at high detail).
    pub medium_detail_scale: f32,
}

/// How a system's LOD level is chosen (`ParticleSystemLODMethod`, in the
/// enum's declaration order; CONFIRMED from `Engine.u`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LodMethod {
    /// `PARTICLESYSTEMLODMETHOD_Automatic`: by distance at activation and
    /// again every `LODDistanceCheckTime`.
    #[default]
    Automatic,
    /// `PARTICLESYSTEMLODMETHOD_DirectSet`: only code sets it; the level
    /// stays where it is.
    DirectSet,
    /// `PARTICLESYSTEMLODMETHOD_ActivateAutomatic`: by distance at
    /// activation only.
    ActivateAutomatic,
}

/// A particle system.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemDef {
    /// Object path.
    pub path: String,
    /// Emitters.
    pub emitters: Vec<EmitterDef>,
    /// `LODDistances`.
    pub lod_distances: Vec<f32>,
    /// `LODDistanceCheckTime`.
    pub lod_check_time: f32,
    /// `LODMethod`.
    pub lod_method: LodMethod,
    /// `WarmupTime`.
    pub warmup_time: f32,
    /// `WarmupTickRate` (0: the engine's [`WARMUP_TICK`]).
    pub warmup_tick_rate: f32,
    /// `SystemUpdateMode` is `EPSUM_FixedTime`: every component tick
    /// advances the system by `UpdateTime_Delta`, whatever the frame time
    /// (CONFIRMED from `UParticleSystemComponent::Tick`).
    pub fixed_time: bool,
    /// `UpdateTime_Delta` (fixed-time systems).
    pub fixed_delta: f32,
    /// `Delay`: added to every emitter's `EmitterDelay` (the component's
    /// `EmitterDelay`, set from it in `InitializeSystem`; CONFIRMED).
    pub delay: f32,
    /// `DelayLow` (with `bUseDelayRange`).
    pub delay_low: f32,
    /// `bUseDelayRange`: the delay is drawn between `DelayLow` and `Delay`
    /// at every initialization.
    pub delay_use_range: bool,
    /// `bSkipSpawnCountCheck`: the component (which copies the flag at
    /// every initialization) spawns without the sprite / sub-UV count limit
    /// (CONFIRMED; no shipped system sets it).
    pub skip_spawn_count_check: bool,
    /// Module classes not simulated (counts).
    pub unsupported: BTreeMap<String, usize>,
}

fn bursts_of(v: Option<&Value>) -> Vec<Burst> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .take(256)
            .map(|b| Burst {
                count: get(b, "Count").and_then(Value::as_i64).unwrap_or(0) as i32,
                count_low: get(b, "CountLow").and_then(Value::as_i64).unwrap_or(0) as i32,
                time: jf32(get(b, "Time")).unwrap_or(0.0),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn params_of(m: &Value) -> &Value {
    get(m, "params").unwrap_or(&Value::Null)
}

fn required_of(m: Option<&Value>) -> RequiredDef {
    let empty = Value::Null;
    let p = m.map_or(&empty, params_of);
    let alignment = match jstr(get(p, "ScreenAlignment")).unwrap_or("PSA_Square") {
        "PSA_Rectangle" => ScreenAlignment::Rectangle,
        "PSA_Velocity" => ScreenAlignment::Velocity,
        "PSA_TypeSpecific" => ScreenAlignment::TypeSpecific,
        _ => ScreenAlignment::Square,
    };
    let sub_uv = match jstr(get(p, "InterpolationMethod")).unwrap_or("PSUVIM_None") {
        "PSUVIM_Linear" => SubUvMethod::Linear,
        "PSUVIM_Linear_Blend" => SubUvMethod::LinearBlend,
        "PSUVIM_Random" => SubUvMethod::Random,
        "PSUVIM_Random_Blend" => SubUvMethod::RandomBlend,
        _ => SubUvMethod::None,
    };
    let count = |n: &str| {
        get(p, n)
            .and_then(Value::as_u64)
            .map_or(1, |x| u32::try_from(x).unwrap_or(1))
            .clamp(1, 64)
    };
    let random_image_changes = get(p, "RandomImageChanges")
        .and_then(Value::as_i64)
        .map_or(0, |x| {
            x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        });
    let random_image_time = if random_image_changes == 0 {
        1.0
    } else {
        let t = 0.99 / (random_image_changes as f32 + 1.0);
        if t.is_finite() { t } else { 1.0 }
    };
    RequiredDef {
        material: jstr(get(p, "Material")).map(str::to_owned),
        local_space: jbool(get(p, "bUseLocalSpace")).unwrap_or(false),
        kill_on_deactivate: jbool(get(p, "bKillOnDeactivate")).unwrap_or(false),
        kill_on_completed: jbool(get(p, "bKillOnCompleted")).unwrap_or(false),
        duration: jf32(get(p, "EmitterDuration")).unwrap_or(0.0),
        duration_low: jf32(get(p, "EmitterDurationLow")).unwrap_or(0.0),
        duration_use_range: jbool(get(p, "bEmitterDurationUseRange")).unwrap_or(false),
        duration_recalc: jbool(get(p, "bDurationRecalcEachLoop")).unwrap_or(false),
        loops: get(p, "EmitterLoops").and_then(Value::as_i64).unwrap_or(0) as i32,
        delay: jf32(get(p, "EmitterDelay")).unwrap_or(0.0),
        delay_low: jf32(get(p, "EmitterDelayLow")).unwrap_or(0.0),
        delay_use_range: jbool(get(p, "bEmitterDelayUseRange")).unwrap_or(false),
        delay_first_loop_only: jbool(get(p, "bDelayFirstLoopOnly")).unwrap_or(false),
        legacy_time: jbool(get(p, "bUseLegacyEmitterTime")).unwrap_or(false),
        alignment,
        sub_h: count("SubImages_Horizontal"),
        sub_v: count("SubImages_Vertical"),
        sub_uv,
        random_image_changes,
        random_image_time,
        max_draw_count: jbool(get(p, "bUseMaxDrawCount"))
            .unwrap_or(false)
            .then(|| {
                get(p, "MaxDrawCount")
                    .and_then(Value::as_u64)
                    .and_then(|x| u32::try_from(x).ok())
            })
            .flatten(),
        spawn_rate: dist_of(get(p, "SpawnRate")),
        bursts: bursts_of(get(p, "BurstList")),
        origin: jvec(get(p, "EmitterOrigin")).unwrap_or(Vec3::ZERO),
        sorted: jstr(get(p, "SortMode")).is_some_and(|s| s != "PSORTMODE_None"),
    }
}

fn orbit_opts(v: Option<&Value>) -> OrbitOptions {
    let Some(v) = v else {
        return OrbitOptions::default();
    };
    OrbitOptions {
        spawn: jbool(get(v, "bProcessDuringSpawn")).unwrap_or(false),
        update: jbool(get(v, "bProcessDuringUpdate")).unwrap_or(false),
        emitter_time: jbool(get(v, "bUseEmitterTime")).unwrap_or(false),
    }
}

fn module_of(class: &str, p: &Value) -> Option<ModuleDef> {
    let d = |n: &str| dist_of(get(p, n));
    let b = |n: &str| jbool(get(p, n)).unwrap_or(false);
    Some(match class {
        "ParticleModuleLifetime" => ModuleDef::Lifetime(d("Lifetime")),
        "ParticleModuleSize" => ModuleDef::Size(d("StartSize")),
        "ParticleModuleSizeMultiplyLife" => ModuleDef::SizeMultiplyLife {
            dist: d("LifeMultiplier"),
            axes: [b("MultiplyX"), b("MultiplyY"), b("MultiplyZ")],
        },
        "ParticleModuleSizeScale" => ModuleDef::SizeScale {
            dist: d("SizeScale"),
        },
        "ParticleModuleSizeScaleByTime" => ModuleDef::SizeScaleByTime {
            dist: d("SizeScaleByTime"),
            axes: [b("bEnableX"), b("bEnableY"), b("bEnableZ")],
        },
        "ParticleModuleVelocity" => ModuleDef::Velocity {
            start: d("StartVelocity"),
            radial: d("StartVelocityRadial"),
            world: b("bInWorldSpace"),
            owner_scale: b("bApplyOwnerScale"),
        },
        "ParticleModuleVelocityOverLifetime" => ModuleDef::VelocityOverLifetime {
            dist: d("VelOverLife"),
            absolute: b("Absolute"),
            world: b("bInWorldSpace"),
            owner_scale: b("bApplyOwnerScale"),
        },
        "ParticleModuleAcceleration" => ModuleDef::Acceleration {
            dist: d("Acceleration"),
            world: b("bAlwaysInWorldSpace"),
            owner_scale: b("bApplyOwnerScale"),
        },
        "ParticleModuleColor" => ModuleDef::Color {
            color: d("StartColor"),
            alpha: d("StartAlpha"),
            clamp: b("bClampAlpha"),
        },
        "ParticleModuleColorOverLife" => ModuleDef::ColorOverLife {
            color: d("ColorOverLife"),
            alpha: d("AlphaOverLife"),
            clamp: b("bClampAlpha"),
        },
        "ParticleModuleColorScaleOverLife" => ModuleDef::ColorScaleOverLife {
            color: d("ColorScaleOverLife"),
            alpha: d("AlphaScaleOverLife"),
            emitter_time: b("bEmitterTime"),
        },
        "ParticleModuleRotation" => ModuleDef::Rotation(d("StartRotation"), false),
        "ParticleModuleMeshRotation" => ModuleDef::Rotation(d("StartRotation"), true),
        "ParticleModuleRotationRate" => ModuleDef::RotationRate(d("StartRotationRate"), false),
        "ParticleModuleMeshRotationRate" => ModuleDef::RotationRate(d("StartRotationRate"), true),
        "ParticleModuleRotationRateMultiplyLife" => {
            ModuleDef::RotationRateMultiplyLife(d("LifeMultiplier"))
        }
        "ParticleModuleLocation" | "ParticleModuleLocationEmitter" => {
            ModuleDef::Location(d("StartLocation"))
        }
        "ParticleModuleLocationPrimitiveSphere" | "ParticleModuleLocationPrimitiveCylinder" => {
            let sphere = class.ends_with("Sphere");
            ModuleDef::Primitive {
                sphere,
                axes: [
                    b("Positive_X"),
                    b("Positive_Y"),
                    b("Positive_Z"),
                    b("Negative_X"),
                    b("Negative_Y"),
                    b("Negative_Z"),
                ],
                surface: b("SurfaceOnly"),
                velocity: b("Velocity"),
                velocity_scale: d("VelocityScale"),
                start: d("StartLocation"),
                radius: d("StartRadius"),
                height: d("StartHeight"),
                height_axis: match jstr(get(p, "HeightAxis")).unwrap_or("PMLPC_HEIGHTAXIS_Z") {
                    "PMLPC_HEIGHTAXIS_X" => 0,
                    "PMLPC_HEIGHTAXIS_Y" => 1,
                    _ => 2,
                },
                radial_velocity: b("RadialVelocity"),
            }
        }
        "ParticleModuleSubUV" => ModuleDef::SubUv(d("SubImageIndex")),
        "ParticleModuleOrbit" => ModuleDef::Orbit {
            chain: match jstr(get(p, "ChainMode")).unwrap_or("EOChainMode_Add") {
                "EOChainMode_Scale" => 1,
                "EOChainMode_Link" => 2,
                _ => 0,
            },
            offset: d("OffsetAmount"),
            offset_opts: orbit_opts(get(p, "OffsetOptions")),
            rotation: d("RotationAmount"),
            rotation_opts: orbit_opts(get(p, "RotationOptions")),
            rate: d("RotationRateAmount"),
            rate_opts: orbit_opts(get(p, "RotationRateOptions")),
        },
        "ParticleModuleAttractorPoint" => ModuleDef::AttractorPoint {
            position: d("Position"),
            range: d("Range"),
            strength: d("Strength"),
            by_distance: b("StrengthByDistance"),
            base_velocity: b("bAffectBaseVelocity"),
            world_position: b("bUseWorldSpacePosition"),
        },
        "ParticleModuleAttractorLine" => ModuleDef::AttractorLine {
            p0: jvec(get(p, "EndPoint0")).unwrap_or(Vec3::ZERO),
            p1: jvec(get(p, "EndPoint1")).unwrap_or(Vec3::ZERO),
            range: d("Range"),
            strength: d("Strength"),
        },
        "ParticleModuleKillHeight" => ModuleDef::KillHeight {
            height: d("Height"),
            floor: b("bFloor"),
            absolute: b("bAbsolute"),
            apply_scale: b("bApplyPSysScale"),
        },
        "ParticleModuleKillBox" => ModuleDef::KillBox {
            lower: d("LowerLeftCorner"),
            upper: d("UpperRightCorner"),
            absolute: b("bAbsolute"),
            inside: b("bKillInside"),
            axis_aligned: b("bAxisAlignedAndFixedSize"),
        },
        "ParticleModuleBeamSource" => ModuleDef::BeamSource(d("Source")),
        "ParticleModuleBeamTarget" => ModuleDef::BeamTarget(d("Target"), b("bTargetAbsolute")),
        "ParticleModuleOrientationAxisLock" => ModuleDef::AxisLock(
            jstr(get(p, "LockAxisFlags"))
                .unwrap_or("EPAL_NONE")
                .to_owned(),
        ),
        _ => return None,
    })
}

/// Modules that are deliberately not simulated but need no note.
const IGNORED: &[&str] = &[
    "ParticleModuleParameterDynamic",
    "ParticleModuleMaterialByParameter",
    "ParticleModuleMeshMaterial",
    "ParticleModuleVelocityInheritParent",
    "ParticleModuleColorByParameter",
    "ParticleModuleBeamNoise",
];

fn lod_of(v: &Value) -> LodDef {
    let required = required_of(get(v, "required").filter(|x| !x.is_null()));
    let spawn = get(v, "spawn").filter(|x| !x.is_null()).map(|m| {
        let p = params_of(m);
        SpawnDef {
            rate: dist_of(get(p, "Rate")),
            rate_scale: dist_of(get(p, "RateScale")),
            bursts: bursts_of(get(p, "BurstList")),
            process_rate: jbool(get(p, "bProcessSpawnRate")).unwrap_or(false),
            process_bursts: jbool(get(p, "bProcessBurstList")).unwrap_or(false),
        }
    });
    let mut modules = Vec::new();
    let mut unsupported = Vec::new();
    if let Some(Value::Array(list)) = get(v, "modules") {
        for m in list.iter().take(MAX_MODULES) {
            let class = jstr(get(m, "class")).unwrap_or("").to_owned();
            if !jbool(get(m, "enabled")).unwrap_or(false) {
                continue;
            }
            match module_of(&class, params_of(m)) {
                Some(def) => modules.push(ModuleSlot {
                    spawn: jbool(get(m, "spawn")).unwrap_or(false),
                    update: jbool(get(m, "update")).unwrap_or(false),
                    class,
                    def,
                }),
                None => {
                    if !IGNORED.contains(&class.as_str()) {
                        unsupported.push(class);
                    }
                }
            }
        }
    }
    LodDef {
        enabled: jbool(get(v, "enabled")).unwrap_or(false),
        required,
        spawn,
        modules,
        unsupported,
        type_data: get(v, "type_data")
            .filter(|x| !x.is_null())
            .and_then(|t| jstr(get(t, "class")))
            .map(str::to_owned),
    }
}

impl SystemDef {
    /// A system from its JSON entry in `particles.json`.
    pub fn from_json(path: &str, v: &Value) -> SystemDef {
        let p = params_of(v);
        let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
        let mut emitters = Vec::new();
        if let Some(Value::Array(list)) = get(v, "emitters") {
            for e in list.iter().take(MAX_EMITTERS) {
                let lods: Vec<LodDef> = match get(e, "lods") {
                    Some(Value::Array(l)) => l.iter().take(MAX_LODS).map(lod_of).collect(),
                    _ => Vec::new(),
                };
                for l in &lods {
                    for u in &l.unsupported {
                        *unsupported.entry(u.clone()).or_default() += 1;
                    }
                }
                let kind = match jstr(get(e, "kind")).unwrap_or("sprite") {
                    "sprite" => EmitterKind::Sprite,
                    "mesh" => EmitterKind::Mesh,
                    "beam" => EmitterKind::Beam,
                    "trail" | "anim_trail" | "ribbon" => EmitterKind::Trail,
                    _ => EmitterKind::Other,
                };
                emitters.push(EmitterDef {
                    name: jstr(get(e, "name")).unwrap_or("").to_owned(),
                    kind,
                    lods,
                    medium_detail_scale: jf32(get(params_of(e), "MediumDetailSpawnRateScale"))
                        .unwrap_or(1.0),
                });
            }
        }
        let lod_distances = match get(p, "LODDistances") {
            Some(Value::Array(a)) => a
                .iter()
                .take(MAX_LODS)
                .map(|x| jf32(Some(x)).unwrap_or(0.0))
                .collect(),
            _ => Vec::new(),
        };
        let lod_method = match jstr(get(p, "LODMethod")).unwrap_or("") {
            "PARTICLESYSTEMLODMETHOD_DirectSet" => LodMethod::DirectSet,
            "PARTICLESYSTEMLODMETHOD_ActivateAutomatic" => LodMethod::ActivateAutomatic,
            _ => LodMethod::Automatic,
        };
        SystemDef {
            path: path.to_owned(),
            emitters,
            lod_distances,
            lod_check_time: jf32(get(p, "LODDistanceCheckTime")).unwrap_or(0.25),
            lod_method,
            warmup_time: jf32(get(p, "WarmupTime"))
                .unwrap_or(0.0)
                .clamp(0.0, MAX_WARMUP),
            warmup_tick_rate: jf32(get(p, "WarmupTickRate")).unwrap_or(0.0),
            fixed_time: enum_is(get(p, "SystemUpdateMode"), "EPSUM_FixedTime"),
            fixed_delta: jf32(get(p, "UpdateTime_Delta")).unwrap_or(1.0 / 60.0),
            delay: jf32(get(p, "Delay")).unwrap_or(0.0),
            delay_low: jf32(get(p, "DelayLow")).unwrap_or(0.0),
            delay_use_range: jbool(get(p, "bUseDelayRange")).unwrap_or(false),
            skip_spawn_count_check: jbool(get(p, "bSkipSpawnCountCheck")).unwrap_or(false),
            unsupported,
        }
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawParticlesFile {
    format: String,
    version: u32,
    systems: BTreeMap<String, Value>,
}

/// Every converted particle system, by lower-case object path.
#[derive(Debug, Clone, Default)]
pub struct ParticleLibrary {
    /// Systems by lower-case path.
    pub systems: BTreeMap<String, std::sync::Arc<SystemDef>>,
}

impl ParticleLibrary {
    /// `particles/particles.json` under the converted root.
    pub fn path(root: &Path) -> PathBuf {
        root.join("particles").join("particles.json")
    }

    /// Parses `particles.json` bytes.
    ///
    /// # Errors
    /// Malformed JSON or a wrong format/version.
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawParticlesFile = parse_json(path, data)?;
        if raw.format != PARTICLES_FORMAT || raw.version != PARTICLES_VERSION {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("{PARTICLES_FORMAT} version {PARTICLES_VERSION}"),
                found: format!("{} version {}", raw.format, raw.version),
            });
        }
        let systems = raw
            .systems
            .iter()
            .take(MAX_SYSTEMS)
            .map(|(k, v)| {
                (
                    k.to_ascii_lowercase(),
                    std::sync::Arc::new(SystemDef::from_json(k, v)),
                )
            })
            .collect();
        Ok(Self { systems })
    }

    /// Loads the library from a converted root; `Ok(None)` when the file is
    /// absent (particles not converted).
    ///
    /// # Errors
    /// A file that exists but cannot be read or parsed.
    pub fn load(root: &Path) -> AssetResult<Option<Self>> {
        let path = Self::path(root);
        if !path.is_file() {
            return Ok(None);
        }
        let data = read_bounded(&path, MAX_MANIFEST_BYTES)?;
        Self::from_json(&path, &data).map(Some)
    }

    /// The system at `path` (case-insensitive).
    pub fn get(&self, path: &str) -> Option<&std::sync::Arc<SystemDef>> {
        self.systems.get(&path.to_ascii_lowercase())
    }
}

/// One placed particle system component (`particles/maps/<Map>.json`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Placement {
    /// Actor object name.
    pub actor: String,
    /// Actor qualified path (Kismet's key).
    pub actor_path: String,
    /// Actor class path.
    #[serde(default)]
    pub actor_class: String,
    /// `ULevel::Actors` index.
    #[serde(default)]
    pub slot: usize,
    /// Actor `bHidden`.
    #[serde(default)]
    pub actor_hidden: bool,
    /// Moved by Matinee.
    #[serde(default)]
    pub moved_by_matinee: bool,
    /// Component world transform (UE3 row-vector matrix).
    pub local_to_world: [[f32; 4]; 4],
    /// `Template`.
    pub template: Option<String>,
    /// `bAutoActivate`.
    #[serde(default)]
    pub auto_activate: bool,
    /// `HiddenGame`.
    #[serde(default)]
    pub hidden_game: bool,
    /// A component-level kill-on-deactivate request. The v868 component
    /// has no such property (the flag lives on each emitter's required
    /// module; script can set it per emitter), so the importer always
    /// writes `false`; kept for script-driven effects.
    #[serde(default)]
    pub kill_on_deactivate: bool,
    /// The same for kill-on-completed (always `false` from the importer).
    #[serde(default)]
    pub kill_on_completed: bool,
    /// `WarmupTime` (0: the template's).
    #[serde(default)]
    pub warmup_time: f32,
    /// `InstanceParameters`.
    #[serde(default)]
    pub instance_parameters: Vec<PlacementParam>,
}

/// One instance parameter of a placement.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PlacementParam {
    /// `Name`.
    pub name: String,
    /// `ParamType`.
    pub param_type: String,
    /// `Scalar`.
    #[serde(default)]
    pub scalar: f32,
    /// `Vector`.
    #[serde(default)]
    pub vector: [f32; 3],
    /// `Color` (B, G, R, A bytes).
    #[serde(default)]
    pub color: [u8; 4],
}

#[derive(Deserialize)]
struct RawPlacements {
    format: String,
    version: u32,
    #[serde(default)]
    placements: Vec<Placement>,
}

/// Loads `particles/maps/<map>.json`; `Ok(None)` when absent. The map name
/// must be a plain package name.
///
/// # Errors
/// A file that exists but cannot be read or parsed, or a wrong format.
pub fn load_placements(root: &Path, map: &str) -> AssetResult<Option<Vec<Placement>>> {
    if map.is_empty()
        || !map
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(AssetError::UnsafePath {
            path: map.to_owned(),
            reason: "map names are plain package names",
        });
    }
    let dir = root.join("particles").join("maps");
    let direct = dir.join(format!("{map}.json"));
    let path = if direct.is_file() {
        direct
    } else {
        // Case-insensitive match (maps are opened by names that differ in case).
        let found = std::fs::read_dir(&dir).ok().and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|p| {
                p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map))
                    && p.extension().is_some_and(|x| x == "json")
            })
        });
        match found {
            Some(p) => p,
            None => return Ok(None),
        }
    };
    let data = read_bounded(&path, MAX_MANIFEST_BYTES)?;
    let raw: RawPlacements = parse_json(&path, &data)?;
    if raw.format != PLACEMENTS_FORMAT || raw.version != PLACEMENTS_VERSION {
        return Err(AssetError::Format {
            path,
            expected: format!("{PLACEMENTS_FORMAT} version {PLACEMENTS_VERSION}"),
            found: format!("{} version {}", raw.format, raw.version),
        });
    }
    let mut placements = raw.placements;
    placements.truncate(MAX_PLACEMENTS);
    for p in &mut placements {
        p.instance_parameters.truncate(MAX_INSTANCE_PARAMS);
    }
    Ok(Some(placements))
}

impl Placement {
    /// The component transform as a column-vector matrix (UE space).
    pub fn matrix(&self) -> Mat4 {
        ue_matrix(&self.local_to_world)
    }

    /// Instance parameters for distributions.
    pub fn params(&self) -> InstanceParams {
        let mut out = InstanceParams::default();
        for p in &self.instance_parameters {
            let key = p.name.to_ascii_lowercase();
            match p.param_type.as_str() {
                "PSPT_Scalar" | "PSPT_ScalarRand" => {
                    out.scalars.insert(key, p.scalar);
                }
                "PSPT_Vector" | "PSPT_VectorRand" => {
                    out.vectors.insert(key, p.vector);
                }
                "PSPT_Color" => {
                    let c = |b: u8| f32::from(b) / 255.0;
                    out.vectors
                        .insert(key, [c(p.color[2]), c(p.color[1]), c(p.color[0])]);
                }
                _ => {}
            }
        }
        out
    }
}

/// A UE3 row-vector matrix (`p' = p · M`) as a glam column-vector matrix.
pub fn ue_matrix(m: &[[f32; 4]; 4]) -> Mat4 {
    let finite = m.iter().flatten().all(|x| x.is_finite());
    if finite {
        Mat4::from_cols_array_2d(m)
    } else {
        Mat4::IDENTITY
    }
}

// ---------------------------------------------------------------------------
// Particle materials
// ---------------------------------------------------------------------------

/// Largest colour multiplier kept for a particle material (HDR emissive
/// strengths in the shipped materials stay well below it).
pub const MAX_COLOR_SCALE: f32 = 64.0;

/// How a particle material draws: the displayed texture, its colour
/// multiplier (HDR strength kept, unlike the level materials) and blend.
///
/// Read from `materials/materials.json` (see MATERIALS.md for the schema).
/// Particle materials are mostly unlit additive or translucent graphs of
/// `texture × colour × vertex colour`; the displayed channel is the emissive
/// input when the material is unlit **or** its diffuse input is unconnected
/// (lit particle materials that only feed emissive), the diffuse input
/// otherwise. Ours, an approximation: opacity taken from another texture
/// channel is not represented (the constant factor is kept).
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleMaterial {
    /// Material path.
    pub path: String,
    /// Blend mode.
    pub blend: crate::BlendMode,
    /// Linear colour multiplier of the texture (may exceed 1).
    pub color: [f32; 3],
    /// Constant opacity factor.
    pub opacity: f32,
    /// Displayed texture.
    pub texture: Option<crate::TextureBinding>,
    /// Texture whose channel is the opacity mask (may be the displayed one).
    pub opacity_texture: Option<crate::TextureBinding>,
    /// Channel weights of the opacity mask (`dot(texel, weights)`); all
    /// zero without a mask texture.
    pub opacity_channels: [f32; 4],
    /// Added to the mask (1 without a mask texture, else 0).
    pub opacity_bias: f32,
    /// A converted description was found.
    pub converted: bool,
}

/// `materials/materials.json` as the particle renderer reads it.
#[derive(Debug, Clone, Default)]
pub struct ParticleMaterials {
    entries: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct RawMaterialsFile {
    #[serde(default)]
    materials: BTreeMap<String, Value>,
}

impl ParticleMaterials {
    /// Parses `materials.json` bytes (entries by lower-case path).
    ///
    /// # Errors
    /// Malformed JSON.
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawMaterialsFile = parse_json(path, data)?;
        Ok(Self {
            entries: raw
                .materials
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
        })
    }

    /// Loads `materials/materials.json` under `root`; empty when absent.
    ///
    /// # Errors
    /// A file that exists but cannot be read or parsed.
    pub fn load(root: &Path) -> AssetResult<Self> {
        let path = root.join("materials").join("materials.json");
        if !path.is_file() {
            return Ok(Self::default());
        }
        let data = read_bounded(&path, MAX_MANIFEST_BYTES)?;
        Self::from_json(&path, &data)
    }

    /// The particle material for `path` (a neutral additive-free fallback
    /// from [`crate::materials::fallback_material`] when not converted).
    pub fn get(
        &self,
        path: &str,
        textures: Option<&crate::manifest::TextureManifest>,
    ) -> ParticleMaterial {
        let fallback = || {
            let f = crate::materials::fallback_material(Some(path));
            ParticleMaterial {
                path: path.to_owned(),
                blend: f.blend,
                color: [f.base_color[0], f.base_color[1], f.base_color[2]],
                opacity: f.base_color[3],
                texture: None,
                opacity_texture: None,
                opacity_channels: [0.0; 4],
                opacity_bias: 1.0,
                converted: false,
            }
        };
        let Some(e) = self.entries.get(&path.to_ascii_lowercase()) else {
            return fallback();
        };
        if jstr(get(e, "status")) == Some("fallback") {
            return fallback();
        }
        let blend = match jstr(get(e, "alpha_mode")).unwrap_or("opaque") {
            "mask" => crate::BlendMode::Masked {
                cutoff: jf32(get(e, "alpha_cutoff"))
                    .unwrap_or(1.0 / 3.0)
                    .clamp(0.0, 1.0),
            },
            "blend" | "premultiplied" => crate::BlendMode::Translucent,
            "add" => crate::BlendMode::Additive,
            "modulate" => crate::BlendMode::Modulate,
            _ => crate::BlendMode::Opaque,
        };
        let unlit = jbool(get(e, "unlit")).unwrap_or(false);
        let textured = |c: Option<&Value>| {
            c.and_then(|c| get(c, "texture"))
                .and_then(|t| jstr(get(t, "texture")))
                .is_some()
        };
        let base = get(e, "base_color");
        let emissive = get(e, "emissive").filter(|v| !v.is_null());
        let base_unconnected = jstr(base.and_then(|b| get(b, "source"))) == Some("default");
        let shown = if emissive.is_some()
            && (unlit || base_unconnected || (!textured(base) && textured(emissive)))
        {
            emissive
        } else {
            base
        };
        let value = shown.map_or([1.0; 3], |c| arr3(get(c, "value")));
        let color = value.map(|v| {
            if v.is_finite() {
                v.clamp(0.0, MAX_COLOR_SCALE)
            } else {
                1.0
            }
        });
        let texture = shown
            .and_then(|c| get(c, "texture"))
            .and_then(|t| jstr(get(t, "texture")))
            .and_then(|t| crate::materials::bind_texture(textures, t, false));
        let opacity_input = get(e, "opacity").filter(|v| !v.is_null());
        let opacity = opacity_input
            .map_or(1.0, |o| arr3(get(o, "value"))[0])
            .clamp(0.0, 1.0);
        // The opacity mask: a channel of a texture (often the alpha of the
        // displayed one). A scalar read from several channels takes the
        // first (MATERIALS.md).
        let mask = opacity_input.and_then(|o| get(o, "texture")).and_then(|t| {
            let path = jstr(get(t, "texture"))?;
            let binding = crate::materials::bind_texture(textures, path, true)?;
            let channels = match jstr(get(t, "channels")).unwrap_or("r") {
                "a" => [0.0, 0.0, 0.0, 1.0],
                "g" => [0.0, 1.0, 0.0, 0.0],
                "b" => [0.0, 0.0, 1.0, 0.0],
                _ => [1.0, 0.0, 0.0, 0.0],
            };
            Some((binding, channels))
        });
        let (opacity_texture, opacity_channels, opacity_bias) = match mask {
            Some((b, c)) => (Some(b), c, 0.0),
            None => (None, [0.0; 4], 1.0),
        };
        ParticleMaterial {
            path: path.to_owned(),
            blend,
            color,
            opacity: if opacity.is_finite() { opacity } else { 1.0 },
            texture,
            opacity_texture,
            opacity_channels,
            opacity_bias,
            converted: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------------

/// One particle (`FBaseParticle` plus the module payloads it needs).
#[derive(Debug, Clone, PartialEq)]
pub struct Particle {
    /// `OldLocation`.
    pub old_location: Vec3,
    /// `Location`.
    pub location: Vec3,
    /// `RelativeTime` (0 → 1 over the lifetime).
    pub relative_time: f32,
    /// `OneOverMaxLifetime`.
    pub one_over_max_lifetime: f32,
    /// `BaseVelocity`.
    pub base_velocity: Vec3,
    /// `Velocity`.
    pub velocity: Vec3,
    /// `Rotation` (radians).
    pub rotation: f32,
    /// `BaseRotationRate`.
    pub base_rotation_rate: f32,
    /// `RotationRate`.
    pub rotation_rate: f32,
    /// `BaseSize`.
    pub base_size: Vec3,
    /// `Size`.
    pub size: Vec3,
    /// `Color` (linear RGBA).
    pub color: [f32; 4],
    /// `BaseColor`.
    pub base_color: [f32; 4],
    /// Sub-UV image, next image and blend.
    pub sub_uv: (u32, u32, f32),
    /// Time of the last random sub-UV change.
    pub sub_uv_time: f32,
    /// Acceleration per acceleration module (in module order).
    pub accel: Vec<Vec3>,
    /// Seconds since the spawn per size-scale-by-time module (its payload:
    /// set to the spawn time, then advanced by every update).
    pub scale_time: Vec<f32>,
    /// Orbit state per orbit module.
    pub orbit: Vec<OrbitState>,
    /// Render offset from the orbit chain.
    pub orbit_offset: Vec3,
}

/// Orbit payload (`FOrbitChainModuleInstancePayload`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OrbitState {
    /// `BaseOffset`.
    pub base_offset: Vec3,
    /// `Offset`.
    pub offset: Vec3,
    /// `Rotation` (turns).
    pub rotation: Vec3,
    /// `BaseRotationRate`.
    pub base_rate: Vec3,
    /// `RotationRate`.
    pub rate: Vec3,
}

impl Particle {
    fn new(location: Vec3, accel: usize, orbit: usize, scale_time: usize) -> Particle {
        Particle {
            old_location: location,
            location,
            relative_time: 0.0,
            one_over_max_lifetime: 0.0,
            base_velocity: Vec3::ZERO,
            velocity: Vec3::ZERO,
            rotation: 0.0,
            base_rotation_rate: 0.0,
            rotation_rate: 0.0,
            base_size: Vec3::ZERO,
            size: Vec3::ZERO,
            color: [0.0; 4],
            base_color: [0.0; 4],
            sub_uv: (0, 0, 0.0),
            sub_uv_time: 0.0,
            accel: vec![Vec3::ZERO; accel],
            scale_time: vec![0.0; scale_time],
            orbit: vec![OrbitState::default(); orbit],
            orbit_offset: Vec3::ZERO,
        }
    }
}

/// The component an emitter belongs to, as the modules see it.
#[derive(Debug, Clone, Copy)]
struct Owner<'a> {
    /// Component local-to-world (UE space).
    l2w: Mat4,
    /// Its inverse.
    w2l: Mat4,
    /// Owner scale (axis lengths of the transform).
    scale: Vec3,
    params: &'a InstanceParams,
    /// The component's `bSkipSpawnCountCheck` (copied from the system).
    skip_spawn_count_check: bool,
}

/// One emitter's live state (`FParticleEmitterInstance`).
#[derive(Debug, Clone)]
pub struct EmitterInstance {
    /// Index into the system's emitters.
    pub emitter: usize,
    /// Current LOD index.
    pub lod: usize,
    /// Live particles.
    pub particles: Vec<Particle>,
    /// Leftover spawn fraction.
    pub spawn_fraction: f32,
    /// `SecondsSinceCreation`.
    pub seconds_since_creation: f32,
    /// `EmitterTime`.
    pub emitter_time: f32,
    /// `LoopCount`.
    pub loop_count: i32,
    /// Current `EmitterDuration` (delay included).
    pub duration: f32,
    /// `CurrentDelay`.
    pub delay: f32,
    /// The component's `EmitterDelay` (the system's `Delay`), added to the
    /// emitter's own delay.
    pub component_delay: f32,
    /// Bursts fired this loop.
    pub bursts_fired: Vec<bool>,
    /// Emitter origin this tick / last tick (world).
    pub location: Vec3,
    /// Previous origin.
    pub old_location: Vec3,
    /// `bHaltSpawning`.
    pub halt_spawning: bool,
    /// Kill particles on deactivate (component or required module).
    pub kill_on_deactivate: bool,
    /// Kill particles once completed.
    pub kill_on_completed: bool,
    /// First tick pending.
    pub first_tick: bool,
    rng: Rng,
}

impl EmitterInstance {
    fn new(
        def: &EmitterDef,
        emitter: usize,
        lod: usize,
        seed: u64,
        origin: Vec3,
        component_delay: f32,
    ) -> EmitterInstance {
        let mut e = EmitterInstance {
            emitter,
            lod,
            particles: Vec::new(),
            spawn_fraction: 0.0,
            seconds_since_creation: 0.0,
            emitter_time: 0.0,
            loop_count: 0,
            duration: 0.0,
            delay: 0.0,
            component_delay: 0.0,
            bursts_fired: Vec::new(),
            location: origin,
            old_location: origin,
            halt_spawning: false,
            kill_on_deactivate: false,
            kill_on_completed: false,
            first_tick: true,
            rng: Rng::new(seed),
        };
        e.init(def, origin, component_delay);
        e
    }

    /// `InitParameters` then `Init`, as `InitParticles` calls them on a new
    /// instance and on one that already holds particles (`ActivateSystem` →
    /// `InitializeSystem` on a component that ran before): the duration is
    /// set up (with the loop count as it was), then the spawn fraction, the
    /// time since creation, the loop count and the fired bursts start over,
    /// the origin snaps to the component and the kill flags are taken from
    /// the required module again. **Live particles are kept** (the engine
    /// zeroes the count only when no particle storage exists) and the
    /// emitter time is left alone; legacy-time emitters derive it from the
    /// time since creation on the next tick anyway (all CONFIRMED).
    fn init(&mut self, def: &EmitterDef, origin: Vec3, component_delay: f32) {
        self.component_delay = if component_delay.is_finite() {
            component_delay
        } else {
            0.0
        };
        self.setup_duration(def);
        self.spawn_fraction = 0.0;
        self.seconds_since_creation = 0.0;
        self.loop_count = 0;
        self.bursts_fired.clear();
        self.location = origin;
        self.old_location = origin;
        self.halt_spawning = false;
        self.first_tick = true;
        if let Some(l) = self.lod(def) {
            self.kill_on_deactivate = l.required.kill_on_deactivate;
            self.kill_on_completed = l.required.kill_on_completed;
        }
    }

    fn lod<'a>(&self, def: &'a EmitterDef) -> Option<&'a LodDef> {
        def.lods.get(self.lod).or_else(|| def.lods.first())
    }

    /// `SetupEmitterDuration`: delay (range) plus the component's delay,
    /// duration (range) plus delay, minus the delay on the first loop when
    /// only the first loop is delayed (CONFIRMED order of the draws). The
    /// engine sets up every LOD level's duration and keeps the last level's
    /// delay; only the current level is set up here.
    fn setup_duration(&mut self, def: &EmitterDef) {
        let Some(l) = self.lod(def) else { return };
        let r = &l.required;
        let mut delay = r.delay + self.component_delay;
        if r.delay_use_range {
            delay =
                (r.delay - r.delay_low) * self.rng.next_f32() + r.delay_low + self.component_delay;
        }
        self.delay = delay;
        let mut duration = r.duration + delay;
        if r.duration_use_range {
            duration = delay + (r.duration - r.duration_low) * self.rng.next_f32() + r.duration_low;
        }
        if self.loop_count == 1 && r.delay_first_loop_only && (r.loops == 0 || r.loops > 1) {
            duration -= delay;
        }
        self.duration = duration;
    }

    /// Number of bursts in the spawn source.
    fn bursts(l: &LodDef) -> &[Burst] {
        match &l.spawn {
            Some(s) => &s.bursts,
            None => &l.required.bursts,
        }
    }

    /// True once the emitter will not spawn again and has no particles
    /// (`HasCompleted`, CONFIRMED: a finite `EmitterLoops`, the time since
    /// creation at or past `EmitterLoops · EmitterDuration`, and no live
    /// particle).
    pub fn has_completed(&self, def: &EmitterDef) -> bool {
        let Some(l) = self.lod(def) else { return true };
        if l.required.loops == 0 {
            return false;
        }
        l.required.loops as f32 * self.duration <= self.seconds_since_creation
            && self.particles.is_empty()
    }

    /// `Tick_EmitterTimeSetup`; returns the delay to add back after the
    /// module updates.
    fn time_setup(&mut self, dt: f32, def: &EmitterDef, origin: Vec3) -> f32 {
        if self.first_tick {
            self.location = origin;
            self.old_location = origin;
        } else {
            self.old_location = self.location;
            self.location = origin;
        }
        self.seconds_since_creation += dt;
        let Some(l) = self.lod(def) else { return 0.0 };
        let r = &l.required;
        // The delay of this tick is read before the loop handling: a loop
        // that sets the duration up again (and may draw a new delay) takes
        // effect from the next tick (CONFIRMED).
        let current_delay = self.delay;
        let looped;
        if r.legacy_time {
            self.emitter_time = self.seconds_since_creation;
            if self.duration > KINDA_SMALL {
                self.emitter_time = self.seconds_since_creation % self.duration;
                looped = (self.seconds_since_creation - self.duration * self.loop_count as f32)
                    >= self.duration;
            } else {
                looped = false;
            }
        } else {
            self.emitter_time += dt;
            looped = self.duration > 0.0 && self.emitter_time >= self.duration;
        }
        if looped {
            self.loop_count = self.loop_count.saturating_add(1);
            self.bursts_fired.iter_mut().for_each(|b| *b = false);
            if !r.legacy_time {
                self.emitter_time -= self.duration;
            }
            if r.duration_recalc || (r.delay_first_loop_only && self.loop_count == 1) {
                self.setup_duration(def);
            }
        }
        let delay = if r.delay_first_loop_only && self.loop_count > 0 {
            0.0
        } else {
            current_delay
        };
        self.emitter_time -= delay;
        delay
    }

    /// `KillParticles`: relative time past 1 (from the end, swapping like
    /// the engine's index list).
    fn kill_expired(&mut self) {
        let mut i = self.particles.len();
        while i > 0 {
            i -= 1;
            if self.particles.get(i).is_some_and(|p| p.relative_time > 1.0) {
                self.particles.swap_remove(i);
            }
        }
    }

    /// `ResetParticleParameters`.
    fn reset_parameters(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.velocity = p.base_velocity;
            p.size = p.base_size;
            p.rotation_rate = p.base_rotation_rate;
            p.color = p.base_color;
            p.relative_time += p.one_over_max_lifetime * dt;
            for o in &mut p.orbit {
                o.offset = o.base_offset;
                o.rate = o.base_rate;
            }
        }
    }

    /// Spawn rate and bursts for this tick (`Spawn(float)`); returns
    /// `(rate, burst count)`.
    fn spawn_amount(&mut self, def: &EmitterDef, dt: f32, params: &InstanceParams) -> (f32, usize) {
        let Some(l) = self.lod(def) else {
            return (0.0, 0);
        };
        let t = self.emitter_time;
        // The LOD's own spawn module always contributes its rate and bursts
        // (`bProcessSpawnRate` / `bProcessBurstList` only let *additional*
        // spawn modules such as SpawnPerUnit suppress them; those are not
        // simulated). The rate scale is drawn first, as in the executable.
        let (mut rate, process_bursts) = match &l.spawn {
            Some(s) => {
                let scale = s.rate_scale.eval_f32(t, &mut self.rng, params);
                let r = s.rate.eval_f32(t, &mut self.rng, params);
                (r * scale, true)
            }
            None => (
                l.required.spawn_rate.eval_f32(t, &mut self.rng, params),
                true,
            ),
        };
        if rate.is_nan() || rate <= 0.0 {
            rate = 0.0;
        }
        let mut burst = 0usize;
        if process_bursts {
            let bursts = Self::bursts(l);
            if self.bursts_fired.len() != bursts.len() {
                self.bursts_fired = vec![false; bursts.len()];
            }
            for (i, b) in bursts.iter().enumerate() {
                if self.bursts_fired.get(i).copied().unwrap_or(true) || b.time > self.emitter_time {
                    continue;
                }
                let n = if b.count_low < 0 {
                    b.count
                } else {
                    let r = self.rng.next_f32();
                    b.count_low.saturating_add(
                        (b.count.saturating_sub(b.count_low) as f32 * r).round() as i32,
                    )
                };
                burst = burst.saturating_add(usize::try_from(n.max(0)).unwrap_or(0));
                if let Some(f) = self.bursts_fired.get_mut(i) {
                    *f = true;
                }
            }
        }
        let _ = dt;
        (rate, burst.min(MAX_SPAWN_PER_TICK))
    }

    #[allow(clippy::too_many_lines)]
    fn spawn_particle(
        &mut self,
        def: &EmitterDef,
        owner: &Owner<'_>,
        spawn_time: f32,
        interp: f32,
    ) {
        let Some(l) = self.lod(def) else { return };
        if self.particles.len() >= MAX_PARTICLES_PER_EMITTER {
            return;
        }
        let local = l.required.local_space;
        let accel_count = l
            .modules
            .iter()
            .filter(|m| matches!(m.def, ModuleDef::Acceleration { .. }))
            .count();
        let orbit_count = l
            .modules
            .iter()
            .filter(|m| matches!(m.def, ModuleDef::Orbit { .. }))
            .count();
        let scale_count = l
            .modules
            .iter()
            .filter(|m| matches!(m.def, ModuleDef::SizeScaleByTime { .. }))
            .count();
        // PreSpawn: zeroed particle at the emitter origin (world space) or
        // the local origin.
        let start = if local { Vec3::ZERO } else { self.location };
        let mut p = Particle::new(start, accel_count, orbit_count, scale_count);
        let t = self.emitter_time;
        let mut accel_i = 0usize;
        let mut orbit_i = 0usize;
        let mut scale_i = 0usize;
        let params = owner.params;
        for m in &l.modules {
            if !m.spawn {
                match m.def {
                    ModuleDef::Acceleration { .. } => accel_i += 1,
                    ModuleDef::Orbit { .. } => orbit_i += 1,
                    ModuleDef::SizeScaleByTime { .. } => scale_i += 1,
                    _ => {}
                }
                continue;
            }
            match &m.def {
                ModuleDef::Lifetime(d) => {
                    let mut life = d.eval_f32(t, &mut self.rng, params);
                    if p.one_over_max_lifetime > 0.0 {
                        life += 1.0 / p.one_over_max_lifetime;
                    }
                    p.one_over_max_lifetime = if life > 0.0 { 1.0 / life } else { 0.0 };
                    p.relative_time = spawn_time * p.one_over_max_lifetime;
                }
                ModuleDef::Size(d) => {
                    let s = d.eval_vec(t, &mut self.rng, params);
                    p.size += s;
                    p.base_size += s;
                }
                ModuleDef::Velocity {
                    start,
                    radial,
                    world,
                    owner_scale,
                } => {
                    let v = start.eval_vec(t, &mut self.rng, params);
                    let from = if local {
                        p.location
                    } else {
                        p.location - self.location
                    };
                    let len2 = from.length_squared();
                    let dir = if len2 == 1.0 {
                        from
                    } else if len2 >= SMALL {
                        from / len2.sqrt()
                    } else {
                        Vec3::ZERO
                    };
                    let v = if local {
                        if *world {
                            owner.w2l.transform_vector3(v)
                        } else {
                            v
                        }
                    } else if *world {
                        v
                    } else {
                        owner.l2w.transform_vector3(v)
                    };
                    let scale = if *owner_scale { owner.scale } else { Vec3::ONE };
                    let r = radial.eval_f32(t, &mut self.rng, params);
                    let total = dir * r * scale + scale * v;
                    p.velocity += total;
                    p.base_velocity += total;
                }
                ModuleDef::Acceleration {
                    dist,
                    world,
                    owner_scale,
                } => {
                    let mut a = dist.eval_vec(t, &mut self.rng, params);
                    if *owner_scale {
                        a *= owner.scale;
                    }
                    let applied = if *world && local {
                        owner.w2l.transform_vector3(a)
                    } else {
                        a
                    };
                    if let Some(slot) = p.accel.get_mut(accel_i) {
                        *slot = a;
                    }
                    p.velocity += applied * spawn_time;
                    p.base_velocity += applied * spawn_time;
                }
                ModuleDef::Color { color, alpha, .. } => {
                    let c = color.eval_vec(t, &mut self.rng, params);
                    let a = alpha.eval_f32(t, &mut self.rng, params);
                    p.color = [c.x, c.y, c.z, a];
                    p.base_color = p.color;
                }
                ModuleDef::ColorOverLife { color, alpha, .. } => {
                    let c = color.eval_vec(p.relative_time, &mut self.rng, params);
                    let a = alpha.eval_f32(p.relative_time, &mut self.rng, params);
                    p.color = [c.x, c.y, c.z, a];
                    p.base_color = p.color;
                }
                ModuleDef::SizeScaleByTime { .. } => {
                    // The module's payload starts at the spawn time.
                    if let Some(slot) = p.scale_time.get_mut(scale_i) {
                        *slot = spawn_time;
                    }
                }
                ModuleDef::Rotation(d, mesh) => {
                    let r = if *mesh {
                        d.eval_vec(t, &mut self.rng, params).x
                    } else {
                        d.eval_f32(t, &mut self.rng, params)
                    };
                    p.rotation = (f64::from(p.rotation) + f64::from(r) * TURN) as f32;
                }
                ModuleDef::RotationRate(d, mesh) => {
                    let r = if *mesh {
                        d.eval_vec(t, &mut self.rng, params).x
                    } else {
                        d.eval_f32(t, &mut self.rng, params)
                    };
                    let r = (f64::from(r) * TURN) as f32;
                    p.rotation_rate += r;
                    p.base_rotation_rate += r;
                }
                ModuleDef::Location(d) => {
                    let v = d.eval_vec(t, &mut self.rng, params);
                    p.location += if local {
                        v
                    } else {
                        owner.l2w.transform_vector3(v)
                    };
                }
                ModuleDef::Primitive {
                    sphere,
                    axes,
                    surface,
                    velocity,
                    velocity_scale,
                    start,
                    radius,
                    height,
                    height_axis,
                    radial_velocity,
                } => {
                    let center = start.eval_vec(t, &mut self.rng, params);
                    let offset = if *sphere {
                        sphere_offset(axes, *surface, radius, t, &mut self.rng, params)
                    } else {
                        cylinder_offset(
                            axes,
                            *surface,
                            radius,
                            height,
                            *height_axis,
                            t,
                            &mut self.rng,
                            params,
                        )
                    };
                    let pos = center + offset;
                    let world = if local {
                        pos
                    } else {
                        owner.l2w.transform_vector3(pos)
                    };
                    p.location += world;
                    if *velocity {
                        let s = velocity_scale.eval_f32(t, &mut self.rng, params);
                        let dir = if *sphere {
                            // The sphere module pushes along the placed
                            // offset minus the *untransformed* start
                            // location (CONFIRMED quirk; the two agree for a
                            // zero start location or an unrotated, unscaled
                            // component).
                            world - center
                        } else {
                            let mut push = offset;
                            if *radial_velocity {
                                push[(*height_axis).min(2)] = 0.0;
                            }
                            if local {
                                push
                            } else {
                                owner.l2w.transform_vector3(push)
                            }
                        };
                        p.velocity += dir * s;
                        p.base_velocity += dir * s;
                    }
                }
                ModuleDef::SubUv(d) => {
                    let r = &l.required;
                    let (i, n, f) = sub_uv_index(r, d, &mut p, &mut self.rng, params, true);
                    p.sub_uv = (i, n, f);
                }
                ModuleDef::Orbit {
                    offset,
                    offset_opts,
                    rotation,
                    rotation_opts,
                    rate,
                    rate_opts,
                    ..
                } => {
                    let time_of = |o: &OrbitOptions, p: &Particle| {
                        if o.emitter_time { t } else { p.relative_time }
                    };
                    let mut st = OrbitState::default();
                    if offset_opts.spawn {
                        let v = offset.eval_vec(time_of(offset_opts, &p), &mut self.rng, params);
                        st.base_offset += v;
                        st.offset += v;
                    }
                    if rotation_opts.spawn {
                        let v =
                            rotation.eval_vec(time_of(rotation_opts, &p), &mut self.rng, params);
                        st.rotation += v;
                    }
                    if rate_opts.spawn {
                        let v = rate.eval_vec(time_of(rate_opts, &p), &mut self.rng, params);
                        st.base_rate += v;
                        st.rate += v;
                    }
                    if let Some(slot) = p.orbit.get_mut(orbit_i) {
                        *slot = st;
                    }
                }
                _ => {}
            }
            match m.def {
                ModuleDef::Acceleration { .. } => accel_i += 1,
                ModuleDef::Orbit { .. } => orbit_i += 1,
                ModuleDef::SizeScaleByTime { .. } => scale_i += 1,
                _ => {}
            }
        }
        // PostSpawn: interpolate along the emitter's motion (world space),
        // then offset by the spawn time.
        if !local && self.old_location.distance_squared(self.location) > 1.0 {
            p.location += (self.old_location - self.location) * interp;
        }
        p.old_location = p.location;
        p.location += p.velocity * spawn_time;
        self.particles.push(p);
    }

    #[allow(clippy::too_many_lines)]
    fn update_modules(&mut self, def: &EmitterDef, owner: &Owner<'_>, dt: f32) {
        let Some(l) = self.lod(def) else { return };
        let local = l.required.local_space;
        let params = owner.params;
        let et = self.emitter_time;
        let mut accel_i = 0usize;
        let mut orbit_i = 0usize;
        let mut scale_i = 0usize;
        // The component origin (the modules below read it from the
        // component's transform, local-space emitter or not).
        let origin = owner.l2w.transform_point3(Vec3::ZERO);
        for m in &l.modules {
            let is_accel = matches!(m.def, ModuleDef::Acceleration { .. });
            let is_orbit = matches!(m.def, ModuleDef::Orbit { .. });
            let is_scale = matches!(m.def, ModuleDef::SizeScaleByTime { .. });
            // `UParticleModuleSubUV::Update` does nothing for the random
            // methods unless `RandomImageChanges` is set (CONFIRMED).
            let skip = matches!(m.def, ModuleDef::SubUv(_))
                && matches!(
                    l.required.sub_uv,
                    SubUvMethod::Random | SubUvMethod::RandomBlend
                )
                && l.required.random_image_changes == 0;
            if m.update && !skip {
                for p in &mut self.particles {
                    let rt = p.relative_time;
                    match &m.def {
                        ModuleDef::SizeMultiplyLife { dist, axes } => {
                            let v = dist.eval_vec(rt, &mut self.rng, params);
                            if axes[0] {
                                p.size.x *= v.x;
                            }
                            if axes[1] {
                                p.size.y *= v.y;
                            }
                            if axes[2] {
                                p.size.z *= v.z;
                            }
                        }
                        ModuleDef::SizeScale { dist } => {
                            let v = dist.eval_vec(rt, &mut self.rng, params);
                            p.size = p.base_size * v;
                        }
                        ModuleDef::SizeScaleByTime { dist, axes } => {
                            // The payload (seconds since the spawn, starting
                            // at the spawn time) advances by the step, then
                            // the curve is read at it (CONFIRMED).
                            let age = match p.scale_time.get_mut(scale_i) {
                                Some(slot) => {
                                    *slot += dt;
                                    *slot
                                }
                                None => 0.0,
                            };
                            let v = dist.eval_vec(age, &mut self.rng, params);
                            if axes[0] {
                                p.size.x *= v.x;
                            }
                            if axes[1] {
                                p.size.y *= v.y;
                            }
                            if axes[2] {
                                p.size.z *= v.z;
                            }
                        }
                        ModuleDef::VelocityOverLifetime {
                            dist,
                            absolute,
                            world,
                            owner_scale,
                        } => {
                            let mut v = dist.eval_vec(rt, &mut self.rng, params);
                            if !local && !*world {
                                v = owner.l2w.transform_vector3(v);
                            } else if local && *world {
                                v = owner.w2l.transform_vector3(v);
                            }
                            let scale = if *owner_scale { owner.scale } else { Vec3::ONE };
                            if *absolute {
                                p.velocity = v * scale;
                                p.base_velocity = p.velocity;
                            } else {
                                p.velocity *= v * scale;
                            }
                        }
                        ModuleDef::Acceleration { world, .. } => {
                            let a = p.accel.get(accel_i).copied().unwrap_or(Vec3::ZERO);
                            let a = if *world && local {
                                owner.w2l.transform_vector3(a)
                            } else {
                                a
                            };
                            p.velocity += a * dt;
                            p.base_velocity += a * dt;
                        }
                        ModuleDef::ColorOverLife { color, alpha, .. } => {
                            let c = color.eval_vec(rt, &mut self.rng, params);
                            let a = alpha.eval_f32(rt, &mut self.rng, params);
                            p.color = [c.x, c.y, c.z, a];
                        }
                        ModuleDef::ColorScaleOverLife {
                            color,
                            alpha,
                            emitter_time,
                        } => {
                            let tt = if *emitter_time { et } else { rt };
                            let c = color.eval_vec(tt, &mut self.rng, params);
                            let a = alpha.eval_f32(tt, &mut self.rng, params);
                            p.color[0] *= c.x;
                            p.color[1] *= c.y;
                            p.color[2] *= c.z;
                            p.color[3] *= a;
                        }
                        ModuleDef::RotationRateMultiplyLife(d) => {
                            p.rotation_rate *= d.eval_f32(rt, &mut self.rng, params);
                        }
                        ModuleDef::SubUv(d) => {
                            if rt <= 1.0 {
                                let (i, n, f) =
                                    sub_uv_index(&l.required, d, p, &mut self.rng, params, false);
                                p.sub_uv = (i, n, f);
                            }
                        }
                        ModuleDef::Orbit {
                            offset,
                            offset_opts,
                            rotation,
                            rotation_opts,
                            rate,
                            rate_opts,
                            ..
                        } => {
                            let time_of = |o: &OrbitOptions| if o.emitter_time { et } else { rt };
                            let ov = offset_opts.update.then(|| {
                                offset.eval_vec(time_of(offset_opts), &mut self.rng, params)
                            });
                            let rv = rotation_opts.update.then(|| {
                                rotation.eval_vec(time_of(rotation_opts), &mut self.rng, params)
                            });
                            let qv = rate_opts
                                .update
                                .then(|| rate.eval_vec(time_of(rate_opts), &mut self.rng, params));
                            if let Some(st) = p.orbit.get_mut(orbit_i) {
                                if let Some(v) = ov {
                                    st.offset += v;
                                }
                                if let Some(v) = rv {
                                    st.rotation += v;
                                }
                                if let Some(v) = qv {
                                    st.rate += v;
                                }
                            }
                        }
                        ModuleDef::AttractorPoint {
                            position,
                            range,
                            strength,
                            by_distance,
                            base_velocity,
                            world_position,
                        } => {
                            // `UParticleModuleAttractorPoint::Update`
                            // (CONFIRMED): position and range at the emitter
                            // time; a world-space emitter without a world
                            // position transforms the point and scales the
                            // strength by the owner scale's length; the
                            // range is always scaled by that length (√3 for
                            // a unit scale).
                            let mut pos = position.eval_vec(et, &mut self.rng, params);
                            let mut r = range.eval_f32(et, &mut self.rng, params);
                            let transformed = !local && !*world_position;
                            let scale_len = if transformed {
                                pos = owner.l2w.transform_point3(pos);
                                owner.scale.length()
                            } else {
                                Vec3::ONE.length()
                            };
                            r *= scale_len;
                            let to = pos - p.location;
                            let len2 = to.length_squared();
                            let dist = len2.sqrt();
                            if dist <= r {
                                let mut s = if *by_distance {
                                    if r != 0.0 {
                                        strength.eval_f32((r - dist) / r, &mut self.rng, params)
                                    } else {
                                        0.0
                                    }
                                } else {
                                    strength.eval_f32(et, &mut self.rng, params)
                                };
                                if transformed {
                                    s *= scale_len;
                                }
                                let dir = if len2 > SMALL { to / dist } else { to };
                                p.velocity += dir * s * dt;
                                if *base_velocity {
                                    p.base_velocity += dir * s * dt;
                                }
                            }
                        }
                        ModuleDef::AttractorLine {
                            p0,
                            p1,
                            range,
                            strength,
                        } => {
                            attractor_line(
                                p,
                                origin,
                                *p0,
                                *p1,
                                range,
                                strength,
                                &mut self.rng,
                                params,
                                dt,
                            );
                        }
                        _ => {}
                    }
                }
            }
            if is_accel {
                accel_i += 1;
            }
            if is_orbit {
                orbit_i += 1;
            }
            if is_scale {
                scale_i += 1;
            }
        }
        // Kill modules (update stage).
        let kills: Vec<&ModuleDef> = l
            .modules
            .iter()
            .filter(|m| m.update)
            .map(|m| &m.def)
            .filter(|d| matches!(d, ModuleDef::KillHeight { .. } | ModuleDef::KillBox { .. }))
            .collect();
        if !kills.is_empty() {
            let rng = &mut self.rng;
            self.particles.retain(|p| {
                kills.iter().all(|k| match k {
                    // `UParticleModuleKillHeight::Update` (CONFIRMED; no
                    // shipped system uses it): the height at the emitter
                    // time, times the component's Z scale when asked, plus
                    // the component's height unless absolute; a local-space
                    // particle is compared by its rotated, scaled height.
                    ModuleDef::KillHeight {
                        height,
                        floor,
                        absolute,
                        apply_scale,
                    } => {
                        let mut h = height.eval_f32(et, rng, params);
                        if *apply_scale {
                            let z2 = owner.l2w.z_axis.truncate().length_squared();
                            if z2 > SMALL {
                                h *= z2.sqrt();
                            }
                        }
                        if !*absolute {
                            h += origin.z;
                        }
                        let z = if local {
                            owner.l2w.transform_vector3(p.location).z
                        } else {
                            p.location.z
                        };
                        !((z < h && *floor) || (h < z && !*floor))
                    }
                    // `UParticleModuleKillBox::Update` (CONFIRMED; no shipped
                    // system uses it): the corners at the emitter time, plus
                    // the component origin unless absolute; a local-space
                    // particle is rotated and scaled, a world-space one is
                    // taken into the component's frame unless the box is
                    // absolute or axis-aligned; the inside test is strict.
                    ModuleDef::KillBox {
                        lower,
                        upper,
                        absolute,
                        inside,
                        axis_aligned,
                    } => {
                        let mut lo = lower.eval_vec(et, rng, params);
                        let mut hi = upper.eval_vec(et, rng, params);
                        if !*absolute {
                            lo += origin;
                            hi += origin;
                        }
                        let pos = if local {
                            owner.l2w.transform_vector3(p.location)
                        } else if !*absolute && !*axis_aligned {
                            owner.w2l.transform_point3(p.location) + origin
                        } else {
                            p.location
                        };
                        let within = pos.cmpgt(lo).all() && pos.cmplt(hi).all();
                        within != *inside
                    }
                    _ => true,
                })
            });
        }
    }

    /// `UpdateOrbitData`: chain the orbit modules into one render offset.
    fn update_orbits(&mut self, def: &EmitterDef, dt: f32) {
        let Some(l) = self.lod(def) else { return };
        let chains: Vec<u8> = l
            .modules
            .iter()
            .filter_map(|m| match m.def {
                ModuleDef::Orbit { chain, .. } => Some(chain),
                _ => None,
            })
            .collect();
        if chains.is_empty() {
            return;
        }
        for p in &mut self.particles {
            let mut total = Vec3::ZERO;
            let mut acc = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
            let mut rot = Mat3::IDENTITY;
            let mut last: Option<usize> = None;
            for (i, chain) in chains.iter().enumerate() {
                let Some(st) = p.orbit.get(i).copied() else {
                    continue;
                };
                match chain {
                    2 => {
                        if let Some(prev) = last {
                            total += orbit_offset(&mut p.orbit, prev, acc, dt, &mut rot);
                        }
                        acc = (st.offset, st.rotation, st.rate);
                    }
                    1 => {
                        acc.0 *= st.offset;
                        acc.1 *= st.rotation;
                        acc.2 *= st.rate;
                    }
                    _ => {
                        acc.0 += st.offset;
                        acc.1 += st.rotation;
                        acc.2 += st.rate;
                    }
                }
                last = Some(i);
            }
            if let Some(prev) = last {
                total += orbit_offset(&mut p.orbit, prev, acc, dt, &mut rot);
            }
            p.orbit_offset = total;
        }
    }

    /// `UpdateBoundingBox`: integrate location and rotation; the rotation
    /// is wrapped into one turn (`fmod` by 2π as a float, CONFIRMED).
    fn integrate(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.old_location = p.location;
            p.location += p.velocity * dt;
            p.rotation = (p.rotation + p.rotation_rate * dt) % std::f32::consts::TAU;
            if !p.rotation.is_finite() {
                p.rotation = 0.0;
            }
        }
    }

    /// One emitter tick (`FParticleEmitterInstance::Tick`; the order of the
    /// calls is the executable's, CONFIRMED from its vtable: time setup,
    /// `KillParticles`, `Tick_SpawnParticles`, `ResetParticleParameters`,
    /// the module updates, then orbit and `UpdateBoundingBox` when anything
    /// is alive).
    fn tick(&mut self, def: &EmitterDef, owner: &Owner<'_>, dt: f32, suppress_spawning: bool) {
        let origin = owner.l2w.transform_point3(Vec3::ZERO);
        let first = self.seconds_since_creation <= 0.0;
        let delay = self.time_setup(dt, def, origin);
        self.kill_expired();
        // Tick_SpawnParticles.
        if let Some(l) = self.lod(def) {
            let loops = l.required.loops;
            let can_spawn = !suppress_spawning
                && !self.halt_spawning
                && self.emitter_time >= 0.0
                && (loops == 0
                    || self.loop_count < loops
                    || first
                    || self.seconds_since_creation < loops as f32 * self.duration);
            if can_spawn {
                self.spawn(def, owner, dt);
            }
        }
        // The reset covers the particles spawned just now: they age one
        // step in their first tick and their transient values go back to
        // the base values before the update modules see them.
        self.reset_parameters(dt);
        self.update_modules(def, owner, dt);
        if !self.particles.is_empty() {
            self.update_orbits(def, dt);
            self.integrate(dt);
        }
        self.emitter_time += delay;
        self.first_tick = false;
    }

    fn spawn(&mut self, def: &EmitterDef, owner: &Owner<'_>, dt: f32) {
        let (rate, burst) = self.spawn_amount(def, dt, owner.params);
        if burst == 0 && rate <= 0.0 {
            return;
        }
        let old = self.spawn_fraction;
        let new_leftover = old + dt * rate;
        let wanted = (new_leftover.floor().max(0.0) as usize).min(MAX_SPAWN_PER_TICK);
        let increment = if rate > 0.0 { 1.0 / rate } else { 0.0 };
        let start_time = old * increment + dt - increment;
        let active = self.particles.len();
        let (number, burst) = if owner.skip_spawn_count_check {
            // No count limit: only the storage limit applies. The engine
            // grows the storage to the new count plus `trunc(√√count + 1)`
            // and, when that passes `MaxParticleResize`, spawns nothing this
            // tick and keeps the old leftover. (Ours: the engine asks only
            // when its storage is too small; asking every time is at most a
            // few particles stricter. No shipped system takes this path.)
            let new_count = active.saturating_add(wanted).saturating_add(burst);
            let slack = ((new_count as f32).sqrt().sqrt() + 1.0) as usize;
            if new_count.saturating_add(slack) > MAX_PARTICLES_PER_EMITTER {
                return;
            }
            (wanted, burst)
        } else {
            // The engine's per-emitter limit: bursts first, then the rate
            // particles, share what is left of the sprite (or sub-UV) count.
            let max_count = match self.lod(def).map(|l| l.required.sub_uv) {
                Some(SubUvMethod::None) | None => MAX_SPRITE_PARTICLES,
                Some(_) => MAX_SUBUV_PARTICLES,
            };
            if wanted + burst + active > max_count {
                let mut remaining = max_count.saturating_sub(active);
                let burst = burst.min(remaining);
                remaining -= burst;
                (wanted.min(remaining), burst)
            } else {
                (wanted, burst)
            }
        };
        for i in 0..number {
            let spawn_time = start_time - i as f32 * increment;
            let interp = 1.0 - (i + 1) as f32 / number as f32;
            self.spawn_particle(def, owner, spawn_time, interp);
        }
        // The leftover drops the whole count that was due, clamped or not
        // (so it stays below 1; a rate beyond [`MAX_SPAWN_PER_TICK`] per
        // step, which only hostile data asks for, keeps no leftover).
        self.spawn_fraction = new_leftover - new_leftover.floor().max(0.0);
        for _ in 0..burst {
            self.spawn_particle(def, owner, 0.0, 0.0);
        }
        if !(0.0..1.0).contains(&self.spawn_fraction) {
            self.spawn_fraction = 0.0;
        }
    }
}

/// `CalculateOrbitOffset` (CONFIRMED): integrate the accumulated rotation
/// and store it in the link's last payload; when any component of it is at
/// least 1e-4 turns, turn the rotation vector by the chain's matrix so far
/// (`TransformNormal`), take it as Euler angles (turns × 360 degrees, roll
/// X, pitch Y, yaw Z), append that rotation to the chain's matrix (the
/// earlier links apply first: row-vector `Accumulated · Rot`) and rotate the
/// accumulated offset by the result; otherwise the offset is returned as it
/// is. The engine builds the rotation from a rotator through its sine
/// table (65536 steps a turn); exact sines are used here.
fn orbit_offset(
    orbit: &mut [OrbitState],
    at: usize,
    acc: (Vec3, Vec3, Vec3),
    dt: f32,
    rot: &mut Mat3,
) -> Vec3 {
    let rotation = acc.1 + acc.2 * dt;
    if let Some(st) = orbit.get_mut(at) {
        st.rotation = rotation;
    }
    if rotation.abs().max_element() >= 1.0e-4 {
        // `rot` holds the engine's row-vector matrix transposed, so
        // `rot · v` is the engine's `v · M` and appending a rotation on the
        // engine's right multiplies on the left here.
        let euler = (*rot * rotation) * std::f32::consts::TAU;
        *rot = ue_rotation(euler) * *rot;
        *rot * acc.0
    } else {
        acc.0
    }
}

/// UE3 rotation matrix of Euler angles in radians (`x` roll, `y` pitch, `z`
/// yaw), axes as columns: the same rows as `FRotationMatrix` (the level
/// importer's `rotation_rows`, CONFIRMED from `AActor::LocalToWorld`).
pub fn ue_rotation(euler: Vec3) -> Mat3 {
    let (sr, cr) = euler.x.sin_cos();
    let (sp, cp) = euler.y.sin_cos();
    let (sy, cy) = euler.z.sin_cos();
    Mat3::from_cols(
        Vec3::new(cp * cy, cp * sy, sp),
        Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
        Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
    )
}

fn unit_direction(axes: &[bool; 6], rng: &mut Rng) -> Vec3 {
    let r = [rng.next_f32(), rng.next_f32(), rng.next_f32()];
    let mut d = [0.0f32; 3];
    for i in 0..3 {
        let (pos, neg) = (axes[i], axes[i + 3]);
        d[i] = if pos && neg {
            r[i] + r[i] - 1.0
        } else if pos {
            r[i]
        } else if neg {
            -r[i]
        } else {
            0.0
        };
    }
    Vec3::from_array(d)
}

fn sphere_offset(
    axes: &[bool; 6],
    surface: bool,
    radius: &Dist,
    t: f32,
    rng: &mut Rng,
    params: &InstanceParams,
) -> Vec3 {
    let mut dir = unit_direction(axes, rng);
    let len2 = dir.length_squared();
    let n = if len2 > SMALL { dir / len2.sqrt() } else { dir };
    if surface && len2 > SMALL {
        dir /= len2.sqrt();
    }
    let r = radius.eval_f32(t, rng, params);
    let v = dir * r;
    let clamp = |x: f32, nx: f32, on: bool| {
        if !on {
            return 0.0;
        }
        let lim = nx.abs() * r;
        x.clamp(-lim.abs(), lim.abs())
    };
    Vec3::new(
        clamp(v.x, n.x, axes[0] || axes[3]),
        clamp(v.y, n.y, axes[1] || axes[4]),
        clamp(v.z, n.z, axes[2] || axes[5]),
    )
}

/// `UParticleModuleLocationPrimitiveCylinder::SpawnEx` (CONFIRMED shape):
/// directions are drawn until the radial part lies inside the unit disc
/// (at most 50 draws); the height component is `dir · StartHeight / 2`, the
/// radial part `dir · StartRadius`, pushed out to the radius for
/// `SurfaceOnly` unless the point lies on a cap.
#[allow(clippy::too_many_arguments)]
fn cylinder_offset(
    axes: &[bool; 6],
    surface: bool,
    radius: &Dist,
    height: &Dist,
    height_axis: usize,
    t: f32,
    rng: &mut Rng,
    params: &InstanceParams,
) -> Vec3 {
    let height_axis = height_axis.min(2);
    let (a, b) = match height_axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let r = radius.eval_f32(t, rng, params);
    let h = height.eval_f32(t, rng, params);
    let mut dir = unit_direction(axes, rng);
    let mut tries = 0;
    while tries < 50 {
        let (x, y) = (dir[a] * r, dir[b] * r);
        if x * x + y * y <= r * r {
            break;
        }
        dir = unit_direction(axes, rng);
        tries += 1;
    }
    let half = h * 0.5;
    let mut out = Vec3::ZERO;
    out[height_axis] = dir[height_axis] * half;
    out[a] = dir[a] * r;
    out[b] = dir[b] * r;
    if surface && (out[height_axis].abs() - half).abs() >= SMALL {
        let len = (dir[a] * dir[a] + dir[b] * dir[b]).sqrt();
        if len > 0.0 {
            out[a] = dir[a] / len * r;
            out[b] = dir[b] / len * r;
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn attractor_line(
    p: &mut Particle,
    origin: Vec3,
    p0: Vec3,
    p1: Vec3,
    range: &Dist,
    strength: &Dist,
    rng: &mut Rng,
    params: &InstanceParams,
    dt: f32,
) {
    let line = p1 - p0;
    let len2 = line.length_squared();
    if len2 <= 0.0 {
        return;
    }
    let rel = p.location - origin;
    let proj = line * ((rel - p0).dot(line) / len2);
    let pick = |a: f32, d: f32| if d != 0.0 { (a - 0.0) / d } else { 0.0 };
    // Line parameter of the projection (first non-zero axis, as the native
    // code picks it).
    let tx = pick(proj.x - p0.x, line.x);
    let ty = pick(proj.y - p0.y, line.y);
    let tz = pick(proj.z - p0.z, line.z);
    let param = if tx != 0.0 {
        tx
    } else if ty != 0.0 {
        ty
    } else {
        tz
    };
    if !(0.0..=1.0).contains(&param) {
        return;
    }
    let r = range.eval_f32(param, rng, params);
    if r <= 0.0 {
        return;
    }
    let perp = rel - proj;
    let dist = perp.length();
    if dist <= r {
        let s = strength.eval_f32((r - dist) / r, rng, params);
        // Velocity gains (perp × line) · strength · dt (the native module's
        // swirl around the line; CONFIRMED arithmetic).
        p.velocity += perp.cross(line) * s * dt;
    }
}

/// `UParticleModuleSubUV::DetermineImageIndex` for the linear and random
/// methods; returns (image, next image, blend).
fn sub_uv_index(
    r: &RequiredDef,
    d: &Dist,
    p: &mut Particle,
    rng: &mut Rng,
    params: &InstanceParams,
    spawning: bool,
) -> (u32, u32, f32) {
    let total = r.sub_h.saturating_mul(r.sub_v).max(1);
    match r.sub_uv {
        SubUvMethod::None => (0, 0, 0.0),
        SubUvMethod::Linear | SubUvMethod::LinearBlend => {
            let v = d.eval_f32(p.relative_time, rng, params);
            let idx = if v.is_nan() || v < 0.0 {
                0
            } else {
                (v as u32).min(total - 1)
            };
            let blend = if r.sub_uv == SubUvMethod::LinearBlend {
                (v - idx as f32).abs()
            } else {
                0.0
            };
            let next = if idx + 1 >= total { 0 } else { idx + 1 };
            (idx, next, blend)
        }
        SubUvMethod::Random | SubUvMethod::RandomBlend => {
            let change = spawning
                || r.random_image_time == 0.0
                || p.relative_time - p.sub_uv_time > r.random_image_time;
            if change {
                let f = rng.next_f32();
                let idx = ((total as f32 * f) as u32).min(total - 1);
                p.sub_uv_time = p.relative_time;
                let blend = if r.sub_uv == SubUvMethod::RandomBlend {
                    f
                } else {
                    0.0
                };
                let next = if idx + 1 >= total { 0 } else { idx + 1 };
                (idx, next, blend)
            } else {
                p.sub_uv
            }
        }
    }
}

/// What a system instance is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    /// Not started (or deactivated and drained).
    Inactive,
    /// Spawning.
    Active,
    /// Deactivated: no new particles, live ones finish.
    Deactivating,
}

/// One running particle system component.
#[derive(Debug, Clone)]
pub struct SystemInstance {
    /// The template.
    pub def: std::sync::Arc<SystemDef>,
    /// Component transform (UE space, column-vector).
    pub transform: Mat4,
    /// Instance parameters.
    pub params: InstanceParams,
    /// Emitter instances.
    pub emitters: Vec<EmitterInstance>,
    /// Activity.
    pub state: ActivityState,
    /// Current LOD.
    pub lod: usize,
    /// Seconds since the last LOD check.
    pub lod_timer: f32,
    /// The activation's LOD choice is still due (it needs the viewer, which
    /// the next [`SystemInstance::tick`] with one supplies).
    pub lod_pending: bool,
    /// Kill every emitter's particles on deactivation (script can ask for
    /// it per emitter; the v868 component itself has no such property).
    pub kill_on_deactivate: bool,
    /// Beam source point set by script (`SetBeamSourcePoint`; UE world).
    pub beam_source: Option<Vec3>,
    /// Beam target point set by script (`SetBeamTargetPoint`; UE world).
    pub beam_target: Option<Vec3>,
    seed: u64,
    generation: u64,
}

/// Most warm-up ticks of one activation (a bound on a hostile
/// `WarmupTickRate`; 30 s at the engine's default tick are 938).
const MAX_WARMUP_STEPS: usize = 1024;

impl SystemInstance {
    /// A new, inactive instance.
    pub fn new(
        def: std::sync::Arc<SystemDef>,
        transform: Mat4,
        params: InstanceParams,
        seed: u64,
    ) -> SystemInstance {
        SystemInstance {
            def,
            transform,
            params,
            emitters: Vec::new(),
            state: ActivityState::Inactive,
            lod: 0,
            lod_timer: 0.0,
            lod_pending: false,
            kill_on_deactivate: false,
            beam_source: None,
            beam_target: None,
            seed,
            generation: 0,
        }
    }

    /// Back to the state of a new instance (no emitter instances, LOD 0,
    /// the first activation's random sequence): a restarted level replays
    /// the same particles.
    pub fn reset(&mut self) {
        self.emitters.clear();
        self.state = ActivityState::Inactive;
        self.lod = 0;
        self.lod_timer = 0.0;
        self.lod_pending = false;
        self.beam_source = None;
        self.beam_target = None;
        self.generation = 0;
    }

    fn owner(&self) -> Owner<'_> {
        let l2w = self.transform;
        let w2l = if l2w.determinant().abs() > 1e-12 {
            l2w.inverse()
        } else {
            Mat4::IDENTITY
        };
        let scale = Vec3::new(
            l2w.x_axis.truncate().length(),
            l2w.y_axis.truncate().length(),
            l2w.z_axis.truncate().length(),
        );
        Owner {
            l2w,
            w2l,
            scale,
            params: &self.params,
            skip_spawn_count_check: self.def.skip_spawn_count_check,
        }
    }

    /// `ActivateSystem` in the game (CONFIRMED from the executable):
    ///
    /// - spawning is no longer suppressed;
    /// - `InitializeSystem` runs. The component's emitter delay becomes the
    ///   system's `Delay` (drawn between `DelayLow` and `Delay` with
    ///   `bUseDelayRange`) and every emitter instance is initialized: a new
    ///   one is created, an existing one starts its time, loops and bursts
    ///   over and **keeps its live particles**. (The engine skips this only
    ///   for the first activation of an auto-activating component, whose
    ///   instances were initialized when it was attached.) Activating a
    ///   system that is still running therefore restarts its emitters'
    ///   clocks without clearing what is on screen;
    /// - the LOD level is chosen by distance unless the method is
    ///   `DirectSet` (done by the next tick that knows the viewer);
    /// - the warm-up runs whenever the system has a `WarmupTime` (the
    ///   component's own value is overwritten from the system on every
    ///   initialization): ticks of `WarmupTickRate`, capped at the warm-up
    ///   time, or [`WARMUP_TICK`] without one, until the time is covered.
    pub fn activate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let origin = self.transform.transform_point3(Vec3::ZERO);
        let base = self
            .seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(self.generation.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let def = self.def.clone();
        let component_delay = if def.delay_use_range {
            let r = Rng::new(base ^ 0x5EED_DE1A_5EED_DE1A).next_f32();
            def.delay_low + (def.delay - def.delay_low) * r
        } else {
            def.delay
        };
        if !self.emitters.is_empty() && self.emitters.len() == def.emitters.len() {
            for (inst, e) in self.emitters.iter_mut().zip(&def.emitters) {
                inst.lod = self.lod.min(e.lods.len().saturating_sub(1));
                inst.init(e, origin, component_delay);
            }
        } else {
            self.emitters = def
                .emitters
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let lod = self.lod.min(e.lods.len().saturating_sub(1));
                    EmitterInstance::new(
                        e,
                        i,
                        lod,
                        base.wrapping_add(i as u64 * 0x1000_0001),
                        origin,
                        component_delay,
                    )
                })
                .collect();
        }
        self.state = ActivityState::Active;
        self.lod_pending = def.lod_method != LodMethod::DirectSet;
        let warmup = def.warmup_time.clamp(0.0, MAX_WARMUP);
        if warmup > 0.0 {
            let rate = def.warmup_tick_rate;
            let step = if rate > 0.0 {
                rate.min(warmup)
            } else {
                WARMUP_TICK
            };
            let mut t = 0.0;
            let mut n = 0;
            // At least one tick, as the engine's loop runs.
            loop {
                self.step(step);
                t += step;
                n += 1;
                if t.partial_cmp(&warmup) != Some(std::cmp::Ordering::Less) || n >= MAX_WARMUP_STEPS
                {
                    break;
                }
            }
        }
    }

    /// `DeactivateSystem` (CONFIRMED): spawning is suppressed; an emitter
    /// whose `bKillOnDeactivate` is set (from its required module, or by
    /// script) loses its particles at once, the others let theirs finish.
    pub fn deactivate(&mut self) {
        if self.state == ActivityState::Inactive {
            return;
        }
        self.state = ActivityState::Deactivating;
        for e in &mut self.emitters {
            if self.kill_on_deactivate || e.kill_on_deactivate {
                e.particles.clear();
            }
        }
    }

    /// Toggle as Kismet's `SeqAct_Toggle` does on an `Emitter` (`Toggle`
    /// input: on when not currently spawning).
    pub fn toggle(&mut self) {
        if self.state == ActivityState::Active {
            self.deactivate();
        } else {
            self.activate();
        }
    }

    /// Live particles over all emitters.
    pub fn particle_count(&self) -> usize {
        self.emitters.iter().map(|e| e.particles.len()).sum()
    }

    /// True when nothing is live and nothing will spawn.
    pub fn is_idle(&self) -> bool {
        match self.state {
            ActivityState::Inactive => true,
            ActivityState::Deactivating => self.particle_count() == 0,
            ActivityState::Active => self.has_completed(),
        }
    }

    /// `UParticleSystemComponent::HasCompleted` (CONFIRMED from the
    /// executable), per emitter instance:
    ///
    /// - current LOD level disabled: ignored when its loops are finite; an
    ///   endless one holds the system open until it is deactivated;
    /// - enabled, finite loops: done when the instance has completed, or,
    ///   once the system is deactivated, when it has no particles left;
    /// - enabled, endless: done only after a deactivation, with no
    ///   particles left.
    ///
    /// A system without emitter instances has completed.
    pub fn has_completed(&self) -> bool {
        let deactivated = self.state != ActivityState::Active;
        self.emitters.iter().all(|inst| {
            let Some(def) = self.def.emitters.get(inst.emitter) else {
                return true;
            };
            let Some(l) = inst.lod(def) else { return true };
            let finite = l.required.loops > 0;
            if !l.enabled {
                finite || deactivated
            } else if deactivated {
                inst.particles.is_empty()
            } else {
                finite && inst.has_completed(def)
            }
        })
    }

    /// LOD for a viewer at `distance` UU
    /// (`UParticleSystemComponent::DetermineLODLevelForLocation`, CONFIRMED
    /// from the executable): 0 with fewer than two `LODDistances`; else the
    /// entry before the first one, from index 1 on, that is greater than
    /// the distance, or the last entry when none is. Entry 0 is never
    /// compared. (The engine takes the nearest local player's view point.)
    pub fn lod_for_distance(&self, distance: f32) -> usize {
        let d = &self.def.lod_distances;
        if d.len() < 2 {
            return 0;
        }
        d.iter()
            .enumerate()
            .skip(1)
            .find(|(_, x)| **x > distance)
            .map_or(d.len(), |(i, _)| i)
            - 1
    }

    /// Advance by `dt` seconds with the viewer at `viewer` (UE space; `None`
    /// keeps the LOD).
    ///
    /// - LOD: an activation's choice is made on the first tick that knows
    ///   the viewer; an `Automatic` system chooses again whenever more than
    ///   `LODDistanceCheckTime` has passed (CONFIRMED).
    /// - A fixed-time system advances by its `UpdateTime_Delta` once per
    ///   call, whatever `dt` is (the engine does so once per frame,
    ///   CONFIRMED); the others advance by `dt`, split into steps of at most
    ///   [`MAX_STEP`].
    pub fn tick(&mut self, dt: f32, viewer: Option<Vec3>) {
        if !dt.is_finite() || dt <= 0.0 || self.state == ActivityState::Inactive {
            return;
        }
        if let Some(v) = viewer {
            let automatic = self.def.lod_method == LodMethod::Automatic;
            if automatic {
                self.lod_timer += dt;
            }
            let due = automatic && self.lod_timer > self.def.lod_check_time;
            if self.lod_pending || due {
                self.lod_pending = false;
                if due {
                    self.lod_timer = 0.0;
                }
                let origin = self.transform.transform_point3(Vec3::ZERO);
                let lod = self.lod_for_distance(origin.distance(v));
                self.set_lod(lod);
            }
        }
        if self.def.fixed_time {
            // (Ours: the delta is capped at [`MAX_STEP`]; the shipped
            // fixed-time system stores 1/30 s.)
            let delta = self.def.fixed_delta;
            if delta.is_finite() && delta > 0.0 {
                self.step(delta.min(MAX_STEP));
            }
            return;
        }
        let mut left = dt;
        let mut n = 0;
        while left > 0.0 && n < 64 {
            let step = left.min(MAX_STEP);
            self.step(step);
            left -= step;
            n += 1;
        }
    }

    fn set_lod(&mut self, lod: usize) {
        if lod == self.lod {
            return;
        }
        self.lod = lod;
        for (inst, def) in self.emitters.iter_mut().zip(&self.def.emitters) {
            if def.lods.is_empty() {
                continue;
            }
            inst.lod = lod.min(def.lods.len() - 1);
        }
    }

    fn step(&mut self, dt: f32) {
        let suppress = self.state != ActivityState::Active;
        let def = self.def.clone();
        let owner_data = self.owner();
        let params = self.params.clone();
        let owner = Owner {
            params: &params,
            ..owner_data
        };
        for inst in &mut self.emitters {
            let Some(edef) = def.emitters.get(inst.emitter) else {
                continue;
            };
            // The component ticks an emitter only while its current LOD
            // level is enabled (CONFIRMED): a disabled level freezes the
            // emitter, clock and particles.
            if !inst.lod(edef).is_some_and(|l| l.enabled) {
                continue;
            }
            inst.tick(edef, &owner, dt, suppress);
        }
        // A system that completes on its own is deactivated by the
        // component's tick (spawning stays suppressed until the next
        // activation; the `Emitter` actor hears of it and is no longer
        // "currently active"; CONFIRMED).
        if self.state == ActivityState::Active && self.has_completed() {
            self.deactivate();
        }
        if self.state == ActivityState::Deactivating && self.particle_count() == 0 {
            self.state = ActivityState::Inactive;
        }
    }

    /// Render data of every emitter (world-space sprites, UE units).
    pub fn render(&self) -> Vec<EmitterRender> {
        let mut out = Vec::new();
        let l2w = self.transform;
        for inst in &self.emitters {
            let Some(def) = self.def.emitters.get(inst.emitter) else {
                continue;
            };
            let Some(l) = inst.lod(def) else { continue };
            if !l.enabled || inst.particles.is_empty() {
                continue;
            }
            let r = &l.required;
            let scale = Vec3::new(
                l2w.x_axis.truncate().length(),
                l2w.y_axis.truncate().length(),
                l2w.z_axis.truncate().length(),
            );
            let axis_lock = l.modules.iter().find_map(|m| match &m.def {
                ModuleDef::AxisLock(s) => Some(s.clone()),
                _ => None,
            });
            let limit = r.max_draw_count.map_or(usize::MAX, |m| m as usize);
            if def.kind == EmitterKind::Beam {
                // Source: the script's point, else the emitter origin plus
                // the source module's value; target: the script's point,
                // else the target module's value (absolute or relative).
                let origin = l2w.transform_point3(Vec3::ZERO);
                let mut rng = Rng(0);
                let t = inst.emitter_time;
                let source = self.beam_source.unwrap_or_else(|| {
                    l.modules
                        .iter()
                        .find_map(|m| match &m.def {
                            ModuleDef::BeamSource(d) => Some(
                                origin
                                    + l2w.transform_vector3(d.eval_vec(t, &mut rng, &self.params)),
                            ),
                            _ => None,
                        })
                        .unwrap_or(origin)
                });
                let target = self.beam_target.or_else(|| {
                    l.modules.iter().find_map(|m| match &m.def {
                        ModuleDef::BeamTarget(d, absolute) => {
                            let v = d.eval_vec(t, &mut rng, &self.params);
                            Some(if *absolute {
                                v
                            } else {
                                l2w.transform_point3(v)
                            })
                        }
                        _ => None,
                    })
                });
                let Some(target) = target else { continue };
                let beams = inst
                    .particles
                    .iter()
                    .take(limit)
                    .map(|p| BeamSegment {
                        start: source,
                        end: target,
                        width: p.size.x * scale.x,
                        color: p.color,
                    })
                    .collect();
                out.push(EmitterRender {
                    emitter: inst.emitter,
                    kind: def.kind,
                    material: r.material.clone(),
                    alignment: r.alignment,
                    sub_images: [r.sub_h, r.sub_v],
                    sub_uv: r.sub_uv,
                    axis_lock: None,
                    sorted: r.sorted,
                    sprites: Vec::new(),
                    beams,
                });
                continue;
            }
            let sprites = inst
                .particles
                .iter()
                .take(limit)
                .map(|p| {
                    let mut pos = p.location;
                    let mut vel = p.velocity;
                    let mut offset = p.orbit_offset;
                    if r.local_space {
                        pos = l2w.transform_point3(pos);
                        vel = l2w.transform_vector3(vel);
                        offset = l2w.transform_vector3(offset);
                    } else {
                        offset = l2w.transform_vector3(offset);
                    }
                    Sprite {
                        position: pos + offset,
                        size: [p.size.x * scale.x, p.size.y * scale.y],
                        rotation: p.rotation,
                        color: p.color,
                        velocity: vel,
                        sub_image: p.sub_uv.0,
                        next_image: p.sub_uv.1,
                        blend: p.sub_uv.2,
                    }
                })
                .collect();
            out.push(EmitterRender {
                emitter: inst.emitter,
                kind: def.kind,
                material: r.material.clone(),
                alignment: r.alignment,
                sub_images: [r.sub_h, r.sub_v],
                sub_uv: r.sub_uv,
                axis_lock,
                sorted: r.sorted,
                sprites,
                beams: Vec::new(),
            });
        }
        out
    }
}

/// One sprite to draw (world space, UE units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// Centre.
    pub position: Vec3,
    /// Width and height (`Size.X`, `Size.Y` times the component scale).
    pub size: [f32; 2],
    /// Rotation about the view axis (radians).
    pub rotation: f32,
    /// Linear RGBA.
    pub color: [f32; 4],
    /// Velocity (for velocity-aligned sprites).
    pub velocity: Vec3,
    /// Sub-image.
    pub sub_image: u32,
    /// Next sub-image (blend target).
    pub next_image: u32,
    /// Blend towards the next sub-image.
    pub blend: f32,
}

/// The sprites of one emitter.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitterRender {
    /// Emitter index.
    pub emitter: usize,
    /// Kind.
    pub kind: EmitterKind,
    /// Material path.
    pub material: Option<String>,
    /// Alignment.
    pub alignment: ScreenAlignment,
    /// Sub-images (horizontal, vertical).
    pub sub_images: [u32; 2],
    /// Sub-UV method.
    pub sub_uv: SubUvMethod,
    /// `LockAxisFlags` of an orientation-lock module.
    pub axis_lock: Option<String>,
    /// Sorted draw requested.
    pub sorted: bool,
    /// Sprites.
    pub sprites: Vec<Sprite>,
    /// Beam segments (beam emitters; their particles are not sprites).
    pub beams: Vec<BeamSegment>,
}

/// One beam of a beam emitter, approximated as a straight segment (ours:
/// no noise, no tangents, no taper).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamSegment {
    /// Start (UE world).
    pub start: Vec3,
    /// End (UE world).
    pub end: Vec3,
    /// Width (`Size.X` times the component scale).
    pub width: f32,
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// UV rectangle `[u0, v0, u1, v1]` of sub-image `index` in an `h × v` grid
/// (row-major, the engine's `index % h`, `index / h`).
pub fn sub_image_rect(index: u32, grid: [u32; 2]) -> [f32; 4] {
    let h = grid[0].max(1);
    let v = grid[1].max(1);
    let index = index.min(h.saturating_mul(v) - 1);
    let (col, row) = (index % h, index / h);
    let (w, hh) = (1.0 / h as f32, 1.0 / v as f32);
    [
        col as f32 * w,
        row as f32 * hh,
        (col + 1) as f32 * w,
        (row + 1) as f32 * hh,
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(t: f32, v: &[f32], mode: CurveMode) -> CurveKey {
        CurveKey {
            t,
            v: v.to_vec(),
            arrive: vec![0.0; v.len()],
            leave: vec![0.0; v.len()],
            mode,
        }
    }

    fn none() -> InstanceParams {
        InstanceParams::default()
    }

    #[test]
    fn rng_matches_the_engine_generator() {
        // seed 0: 0 * a + c = 0x3619636B; mantissa 0x19636B → 1.19834… − 1.
        let mut r = Rng(0);
        let v = r.next_f32();
        assert_eq!(r.0, 0x3619_636B);
        assert_eq!(v, f32::from_bits(0x3F99_636B) - 1.0);
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            let x = a.next_f32();
            assert_eq!(x, b.next_f32());
            assert!((0.0..1.0).contains(&x));
        }
        assert_ne!(Rng::new(1).next_f32(), Rng::new(2).next_f32());
    }

    #[test]
    fn curve_evaluation_matches_hand_values() {
        let lin = Curve {
            dim: 1,
            keys: vec![
                key(0.0, &[1.0], CurveMode::Linear),
                key(1.0, &[0.0], CurveMode::Linear),
            ],
            broken_tangents: false,
        };
        assert_eq!(lin.eval(0.25), vec![0.75]);
        assert_eq!(lin.eval(-1.0), vec![1.0]);
        assert_eq!(lin.eval(2.0), vec![0.0]);
        assert_eq!(lin.eval(f32::NAN), vec![0.0], "NaN reads the last key");
        let constant = Curve {
            dim: 1,
            keys: vec![
                key(0.0, &[3.0], CurveMode::Constant),
                key(1.0, &[5.0], CurveMode::Linear),
            ],
            broken_tangents: false,
        };
        assert_eq!(constant.eval(0.99), vec![3.0]);
        assert_eq!(constant.eval(1.0), vec![5.0]);
        // Cubic: p0 = 0, p1 = 1, leave0 = 2, arrive1 = 0 over span 2 (t 0..2):
        // T0 = 2·2 = 4, T1 = 0; at a = 0.5: h00 = .5, h10 = .125, h11 = −.125, h01 = .5
        // → 0·.5 + 4·.125 + 0 + 1·.5 = 1.0.
        let mut k0 = key(0.0, &[0.0], CurveMode::Cubic);
        k0.leave = vec![2.0];
        let k1 = key(2.0, &[1.0], CurveMode::Cubic);
        let cubic = Curve {
            dim: 1,
            keys: vec![k0, k1],
            broken_tangents: false,
        };
        assert_eq!(cubic.eval(1.0), vec![1.0]);
        // Broken tangents are not scaled by the span: T0 = 2 → 0.25 + 0.5.
        let broken = Curve {
            broken_tangents: true,
            ..cubic.clone()
        };
        assert_eq!(broken.eval(1.0), vec![0.75]);
        // Vector curve, per component.
        let v = Curve {
            dim: 3,
            keys: vec![
                key(0.0, &[0.0, 10.0, -2.0], CurveMode::Linear),
                key(2.0, &[2.0, 0.0, 2.0], CurveMode::Linear),
            ],
            broken_tangents: false,
        };
        assert_eq!(v.eval(0.5), vec![0.5, 7.5, -1.0]);
        // Zero-length segment holds the start key.
        let zero = Curve {
            dim: 1,
            keys: vec![
                key(0.0, &[1.0], CurveMode::Linear),
                key(0.5, &[2.0], CurveMode::Linear),
                key(0.5, &[9.0], CurveMode::Linear),
                key(1.0, &[3.0], CurveMode::Linear),
            ],
            broken_tangents: false,
        };
        assert_eq!(zero.eval(0.75), vec![6.0]);
        assert_eq!(Curve::default().eval(0.3), vec![0.0]);
    }

    #[test]
    fn hermite_operation_order() {
        assert_eq!(hermite(0.0, 4.0, 1.0, 0.0, 0.5), 1.0);
        assert_eq!(hermite(2.0, 0.0, 6.0, 0.0, 0.0), 2.0);
        assert_eq!(hermite(2.0, 0.0, 6.0, 0.0, 1.0), 6.0);
    }

    #[test]
    fn distributions_follow_the_native_formulas() {
        let p = none();
        // Uniform float: max + (min − max) · r with the stream's r.
        let mut rng = Rng(7);
        let mut probe = rng;
        let r = probe.next_f32();
        let d = Dist::Uniform {
            min: [2.0, 0.0, 0.0],
            max: [10.0, 0.0, 0.0],
            lock: 0,
            mirror: [1; 3],
            extremes: false,
        };
        assert_eq!(d.eval_f32(0.0, &mut rng, &p), 10.0 + (2.0 - 10.0) * r);
        // Constant vector with XY lock.
        let c = Dist::Constant {
            value: [1.0, 2.0, 3.0],
            lock: 1,
        };
        assert_eq!(c.eval_vec(0.0, &mut rng, &p), Vec3::new(1.0, 1.0, 3.0));
        let c4 = Dist::Constant {
            value: [1.0, 2.0, 3.0],
            lock: 4,
        };
        assert_eq!(c4.eval_vec(0.0, &mut rng, &p), Vec3::splat(1.0));
        // Uniform vector: mirror Y (min' = −max), lock XYZ draws once.
        let mut a = Rng(99);
        let mut b = a;
        let u = Dist::Uniform {
            min: [0.0, 0.0, 0.0],
            max: [4.0, 4.0, 4.0],
            lock: 0,
            mirror: [1, 2, 0],
            extremes: false,
        };
        let v = u.eval_vec(0.0, &mut a, &p);
        let (r1, r2, _r3) = (b.next_f32(), b.next_f32(), b.next_f32());
        assert_eq!(v.x, 4.0 + (0.0 - 4.0) * r1);
        assert_eq!(v.y, 4.0 + (-4.0 - 4.0) * r2);
        assert_eq!(v.z, 4.0, "Same: min = max");
        let mut c = Rng(5);
        let mut d = c;
        let locked = Dist::Uniform {
            min: [0.0; 3],
            max: [1.0, 9.0, 9.0],
            lock: 4,
            mirror: [1; 3],
            extremes: false,
        };
        let v = locked.eval_vec(0.0, &mut c, &p);
        let r = d.next_f32();
        assert_eq!(v, Vec3::splat(1.0 - r));
        assert_eq!(c, d, "a locked XYZ uniform draws one number");
        // Extremes pick min or max.
        let ext = Dist::Uniform {
            min: [1.0; 3],
            max: [5.0; 3],
            lock: 0,
            mirror: [1; 3],
            extremes: true,
        };
        for s in 0..50 {
            let v = ext.eval_vec(0.0, &mut Rng(s), &p);
            assert!(v == Vec3::splat(1.0) || v == Vec3::splat(5.0), "{v}");
        }
        // Parameter mapping: normal clamps and maps, abs, direct.
        let mut params = InstanceParams::default();
        params.scalars.insert("speed".into(), -3.0);
        let param = |modes: u8, max_input: f32| Dist::Parameter {
            name: "Speed".into(),
            modes: [modes; 3],
            min_input: [0.0; 3],
            max_input: [max_input; 3],
            min_output: [100.0; 3],
            max_output: [200.0; 3],
            constant: [0.0; 3],
        };
        let normal = param(0, 10.0);
        assert_eq!(
            normal.eval_f32(0.0, &mut rng, &params),
            100.0,
            "clamped to MinInput"
        );
        assert_eq!(param(1, 10.0).eval_f32(0.0, &mut rng, &params), 130.0);
        assert_eq!(param(2, 10.0).eval_f32(0.0, &mut rng, &params), -3.0);
        assert_eq!(
            normal.eval_f32(0.0, &mut rng, &none()),
            100.0,
            "constant 0 maps to 100"
        );
        assert_eq!(
            param(0, 0.0).eval_f32(0.0, &mut rng, &params),
            100.0,
            "zero gradient"
        );
        // Lookup (no object): op none.
        let l = Dist::Lookup {
            op: 1,
            chunk: 1,
            table: vec![0.0, 10.0, 0.0, 10.0],
            time_scale: 1.0,
            start_time: 0.0,
        };
        assert_eq!(l.eval_f32(0.5, &mut rng, &p), 5.0);
        assert_eq!(Dist::Zero.eval_f32(1.0, &mut rng, &p), 0.0);
    }

    #[test]
    fn json_distributions_parse() {
        let v = json!({"dist": "vector", "value": {"kind": "uniform", "min": [1, 2, 3], "max": [4, 5, 6], "locked_axes": 2, "mirror": [1, 2, 0], "use_extremes": true}});
        assert_eq!(
            dist_of(Some(&v)),
            Dist::Uniform {
                min: [1.0, 2.0, 3.0],
                max: [4.0, 5.0, 6.0],
                lock: 2,
                mirror: [1, 2, 0],
                extremes: true
            }
        );
        let c = json!({"dist": "float", "value": {"kind": "constant_curve", "locked_axes": 0, "curve": {"dim": 1, "keys": [{"t": 0, "v": [1], "arrive": [0], "leave": [0], "mode": "linear"}, {"t": 1, "v": [3], "arrive": [0], "leave": [0], "mode": "curve_auto"}]}}});
        let d = dist_of(Some(&c));
        assert_eq!(d.eval_f32(0.5, &mut Rng(0), &none()), 2.0);
        assert_eq!(dist_of(None), Dist::Zero);
        assert_eq!(
            dist_of(Some(&json!({"value": {"kind": "bogus"}}))),
            Dist::Zero
        );
        assert_eq!(dist_of(Some(&json!(17))), Dist::Zero);
    }

    fn dist_json(kind: &str, v: serde_json::Value) -> serde_json::Value {
        let mut inner = v;
        inner["kind"] = json!(kind);
        json!({"dist": "float", "value": inner})
    }

    /// A one-emitter system: rate, lifetime, size, velocity, colour over life.
    fn system_json(
        rate: f32,
        life: f32,
        bursts: serde_json::Value,
        loops: i32,
    ) -> serde_json::Value {
        json!({
            "path": "Test.PS",
            "params": {"LODDistances": [0.0, 1000.0]},
            "emitters": [{
                "name": "E",
                "kind": "sprite",
                "params": {},
                "lods": [{
                    "level": 0,
                    "enabled": true,
                    "required": {"class": "ParticleModuleRequired", "enabled": true, "params": {
                        "Material": "Fx.M_Spark",
                        "EmitterDuration": 1.0,
                        "EmitterLoops": loops,
                        "bUseLegacyEmitterTime": false,
                        "SubImages_Horizontal": 2,
                        "SubImages_Vertical": 2,
                        "InterpolationMethod": "PSUVIM_Linear"
                    }},
                    "spawn": {"class": "ParticleModuleSpawn", "enabled": true, "params": {
                        "Rate": dist_json("constant", json!({"value": [rate], "locked_axes": 0})),
                        "RateScale": dist_json("constant", json!({"value": [1.0], "locked_axes": 0})),
                        "BurstList": bursts,
                        "bProcessSpawnRate": true,
                        "bProcessBurstList": true
                    }},
                    "type_data": null,
                    "modules": [
                        {"class": "ParticleModuleLifetime", "enabled": true, "spawn": true, "update": false, "params": {
                            "Lifetime": dist_json("constant", json!({"value": [life], "locked_axes": 0}))}},
                        {"class": "ParticleModuleSize", "enabled": true, "spawn": true, "update": false, "params": {
                            "StartSize": {"dist": "vector", "value": {"kind": "uniform", "min": [10, 10, 10], "max": [20, 20, 20], "locked_axes": 4, "mirror": [1, 1, 1], "use_extremes": false}}}},
                        {"class": "ParticleModuleVelocity", "enabled": true, "spawn": true, "update": false, "params": {
                            "StartVelocity": {"dist": "vector", "value": {"kind": "uniform", "min": [-50, -50, 100], "max": [50, 50, 200], "locked_axes": 0, "mirror": [1, 1, 1], "use_extremes": false}},
                            "StartVelocityRadial": dist_json("constant", json!({"value": [0.0], "locked_axes": 0}))}},
                        {"class": "ParticleModuleColorOverLife", "enabled": true, "spawn": true, "update": true, "params": {
                            "ColorOverLife": {"dist": "vector", "value": {"kind": "constant", "value": [1, 0.5, 0.25], "locked_axes": 0}},
                            "AlphaOverLife": dist_json("constant_curve", json!({"locked_axes": 0, "curve": {"dim": 1, "keys": [
                                {"t": 0, "v": [1], "arrive": [0], "leave": [0], "mode": "linear"},
                                {"t": 1, "v": [0], "arrive": [0], "leave": [0], "mode": "linear"}]}})),
                            "bClampAlpha": true}},
                        {"class": "ParticleModuleSubUV", "enabled": true, "spawn": true, "update": true, "params": {
                            "SubImageIndex": dist_json("constant_curve", json!({"locked_axes": 0, "curve": {"dim": 1, "keys": [
                                {"t": 0, "v": [0], "arrive": [0], "leave": [0], "mode": "linear"},
                                {"t": 1, "v": [4], "arrive": [0], "leave": [0], "mode": "linear"}]}}))}},
                        {"class": "ParticleModuleCollision", "enabled": true, "spawn": true, "update": true, "params": {}}
                    ]
                }]
            }]
        })
    }

    fn instance(v: &serde_json::Value, seed: u64) -> SystemInstance {
        let def = std::sync::Arc::new(SystemDef::from_json("Test.PS", v));
        SystemInstance::new(def, Mat4::IDENTITY, InstanceParams::default(), seed)
    }

    fn run(s: &mut SystemInstance, seconds: f32, dt: f32) {
        let n = (seconds / dt).round() as usize;
        for _ in 0..n {
            s.tick(dt, None);
        }
    }

    #[test]
    fn spawn_rate_with_leftover_fraction() {
        // 30/s at 60 Hz: one particle every other tick; lifetime 10 s.
        let mut s = instance(&system_json(30.0, 10.0, json!([]), 0), 1);
        s.activate();
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 30);
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 60);
        assert_eq!(s.def.unsupported.get("ParticleModuleCollision"), Some(&1));
    }

    #[test]
    fn bursts_fire_once_per_loop_and_lifetime_kills() {
        let bursts = json!([{"Count": 5, "CountLow": -1, "Time": 0.0}, {"Count": 3, "CountLow": -1, "Time": 0.5}]);
        let mut s = instance(&system_json(0.0, 0.25, bursts, 0), 2);
        s.activate();
        s.tick(1.0 / 60.0, None);
        assert_eq!(s.particle_count(), 5, "first burst at t = 0");
        run(&mut s, 0.3, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 0, "lifetime 0.25 s");
        run(&mut s, 0.25, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 3, "second burst at t = 0.5");
        // Next loop (duration 1 s) fires both again.
        run(&mut s, 0.5, 1.0 / 60.0);
        assert!(s.particle_count() >= 5, "{}", s.particle_count());
    }

    /// The engine's sprite and sub-UV count limits (485 and 358 with the
    /// shipped vertex memory) clamp spawning; bursts are served first.
    #[test]
    fn spawn_is_clamped_to_the_engine_particle_limits() {
        assert_eq!((MAX_SPRITE_PARTICLES, MAX_SUBUV_PARTICLES), (485, 358));
        // The fixture uses a linear sub-UV method: 358.
        let mut s = instance(&system_json(5000.0, 100.0, json!([]), 0), 1);
        s.activate();
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 358);
        assert!(
            s.emitters[0].spawn_fraction < 1.0,
            "the leftover does not pile up"
        );
        // Without sub-UV: 485; a burst takes the room before the rate does.
        let mut v = system_json(
            6000.0,
            100.0,
            json!([{"Count": 480, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        v["emitters"][0]["lods"][0]["required"]["params"]["InterpolationMethod"] =
            json!("PSUVIM_None");
        let mut s = instance(&v, 1);
        s.activate();
        s.tick(1.0 / 60.0, None);
        assert_eq!(
            s.particle_count(),
            485,
            "480 burst particles and 5 of the 100 due"
        );
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 485);
    }

    #[test]
    fn finite_loops_complete() {
        let mut s = instance(&system_json(20.0, 0.2, json!([]), 1), 3);
        s.activate();
        run(&mut s, 0.5, 1.0 / 60.0);
        assert!(s.particle_count() > 0);
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 0);
        assert!(s.is_idle(), "one loop of 1 s, then the particles die");
        assert_eq!(
            s.state,
            ActivityState::Inactive,
            "a system that completes is deactivated by its own tick"
        );
        // The next activation runs the loop again.
        s.activate();
        run(&mut s, 0.5, 1.0 / 60.0);
        assert!(s.particle_count() > 0);
        assert_eq!(s.state, ActivityState::Active);
    }

    /// The component's completion rule per emitter: endless emitters hold a
    /// system open, finite ones on a disabled LOD level are ignored, and a
    /// deactivated system is done when its particles are gone.
    #[test]
    fn completion_follows_the_component_rule() {
        let two = |loops_a: i32, loops_b: i32, b_enabled: bool| {
            let mut v = bare_system(30.0, 0.2, json!([]), vec![]);
            let mut second = v["emitters"][0].clone();
            v["emitters"][0]["lods"][0]["required"]["params"]["EmitterLoops"] = json!(loops_a);
            second["lods"][0]["required"]["params"]["EmitterLoops"] = json!(loops_b);
            second["lods"][0]["enabled"] = json!(b_enabled);
            v["emitters"].as_array_mut().unwrap().push(second);
            let mut s = instance(&v, 2);
            s.activate();
            for _ in 0..150 {
                s.tick(DT, None);
            }
            s
        };
        // Both finite: done after one loop of 1 s plus the 0.2 s lifetime.
        assert_eq!(two(1, 1, true).state, ActivityState::Inactive);
        // One endless emitter keeps the system active.
        let s = two(1, 0, true);
        assert_eq!(s.state, ActivityState::Active);
        assert!(!s.has_completed());
        // A finite emitter on a disabled LOD level is not waited for...
        assert_eq!(two(1, 3, false).state, ActivityState::Inactive);
        // ...an endless one is, until the system is deactivated.
        let mut s = two(1, 0, false);
        assert_eq!(s.state, ActivityState::Active);
        s.deactivate();
        s.tick(DT, None);
        assert_eq!(s.state, ActivityState::Inactive);
        // No emitters at all: completed at once.
        let mut empty = instance(&json!({"emitters": []}), 1);
        empty.activate();
        assert!(empty.has_completed());
        empty.tick(DT, None);
        assert_eq!(empty.state, ActivityState::Inactive);
    }

    #[test]
    fn deactivate_stops_spawning_and_toggle_restarts() {
        let mut s = instance(&system_json(60.0, 0.5, json!([]), 0), 4);
        s.activate();
        run(&mut s, 0.5, 1.0 / 60.0);
        let live = s.particle_count();
        assert!(live > 20);
        s.deactivate();
        assert_eq!(s.state, ActivityState::Deactivating);
        run(&mut s, 1.0, 1.0 / 60.0);
        assert_eq!(s.particle_count(), 0);
        assert_eq!(s.state, ActivityState::Inactive);
        assert!(s.is_idle());
        s.toggle();
        assert_eq!(s.state, ActivityState::Active);
        run(&mut s, 0.25, 1.0 / 60.0);
        assert!(s.particle_count() > 0);
        s.kill_on_deactivate = true;
        s.deactivate();
        assert_eq!(s.particle_count(), 0, "bKillOnDeactivate clears at once");
    }

    #[test]
    fn simulation_is_deterministic_per_seed() {
        let v = system_json(
            40.0,
            1.5,
            json!([{"Count": 4, "CountLow": 1, "Time": 0.0}]),
            0,
        );
        let mut a = instance(&v, 77);
        let mut b = instance(&v, 77);
        let mut c = instance(&v, 78);
        for s in [&mut a, &mut b, &mut c] {
            s.activate();
            run(s, 3.0, 1.0 / 60.0);
        }
        let ra = a.render();
        let rb = b.render();
        assert_eq!(ra, rb, "same seed, same particles");
        assert_ne!(ra, c.render(), "another seed differs");
        let pa = &a.emitters[0].particles;
        assert!(!pa.is_empty());
        for p in pa {
            assert!(p.location.is_finite() && p.velocity.is_finite());
            assert!((10.0..=20.0).contains(&p.size.x));
            assert_eq!(p.size.x, p.size.y, "XYZ locked size");
            assert!(p.velocity.z >= 100.0 && p.velocity.z <= 200.0);
            assert!((0.0..=1.0).contains(&p.color[3]));
            assert_eq!(p.color[0], 1.0);
        }
        // A fresh activation of the same instance draws a new sequence.
        let before = a.render();
        a.activate();
        run(&mut a, 3.0, 1.0 / 60.0);
        assert_ne!(before, a.render());
    }

    #[test]
    fn colour_over_life_and_sub_uv_follow_relative_time() {
        let mut s = instance(
            &system_json(
                0.0,
                1.0,
                json!([{"Count": 1, "CountLow": -1, "Time": 0.0}]),
                0,
            ),
            5,
        );
        s.activate();
        run(&mut s, 0.45, 1.0 / 60.0);
        let p = &s.emitters[0].particles[0];
        // The burst particle spawned on the first tick ages in every tick,
        // the spawning one included: 27 ticks → relative time 27/60 = 0.45,
        // alpha 1 − rt, sub image trunc(4 · 0.45) = 1.
        assert!((p.relative_time - 0.45).abs() < 1e-4, "{}", p.relative_time);
        assert!((p.color[3] - (1.0 - p.relative_time)).abs() < 1e-5);
        assert_eq!(p.sub_uv.0, (4.0 * p.relative_time) as u32);
        assert_eq!(p.sub_uv.0, 1);
        let r = s.render();
        assert_eq!(r[0].sub_images, [2, 2]);
        assert_eq!(r[0].material.as_deref(), Some("Fx.M_Spark"));
        assert_eq!(sub_image_rect(2, [2, 2]), [0.0, 0.5, 0.5, 1.0]);
        assert_eq!(sub_image_rect(99, [2, 2]), [0.5, 0.5, 1.0, 1.0], "clamped");
    }

    #[test]
    fn transform_moves_world_space_particles() {
        let v = system_json(
            0.0,
            5.0,
            json!([{"Count": 3, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        let def = std::sync::Arc::new(SystemDef::from_json("Test.PS", &v));
        let t = Mat4::from_translation(Vec3::new(1000.0, -500.0, 250.0));
        let mut s = SystemInstance::new(def, t, InstanceParams::default(), 9);
        s.activate();
        s.tick(1.0 / 60.0, None);
        for p in &s.emitters[0].particles {
            assert!((p.location.x - 1000.0).abs() < 10.0);
            assert!((p.location.y + 500.0).abs() < 10.0);
        }
    }

    #[test]
    fn ue_rotation_matches_the_engine_axes() {
        // Yaw a quarter turn: +X (forward) turns to +Y (right).
        let m = ue_rotation(Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2));
        assert!((m * Vec3::X - Vec3::Y).length() < 1e-6);
        // Pitch a quarter turn: forward points up.
        let m = ue_rotation(Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0));
        assert!((m * Vec3::X - Vec3::Z).length() < 1e-6);
        assert!(
            (ue_rotation(Vec3::ZERO) * Vec3::new(1.0, 2.0, 3.0) - Vec3::new(1.0, 2.0, 3.0))
                .length()
                < 1e-6
        );
    }

    #[test]
    fn orbit_offsets_turn_with_their_rotation_rate() {
        let mut v = system_json(
            0.0,
            100.0,
            json!([{"Count": 1, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        let vec = |x: f32, y: f32, z: f32| json!({"dist": "vector", "value": {"kind": "constant", "value": [x, y, z], "locked_axes": 0}});
        let orbit = json!({"class": "ParticleModuleOrbit", "enabled": true, "spawn": true, "update": true, "params": {
            "ChainMode": "EOChainMode_Add",
            "OffsetAmount": vec(100.0, 0.0, 0.0),
            "OffsetOptions": {"bProcessDuringSpawn": true},
            "RotationAmount": vec(0.0, 0.0, 0.0),
            "RotationOptions": {"bProcessDuringSpawn": true},
            // A quarter turn per second about Z (yaw).
            "RotationRateAmount": vec(0.0, 0.0, 0.25),
            "RotationRateOptions": {"bProcessDuringSpawn": true}}});
        let modules = v["emitters"][0]["lods"][0]["modules"]
            .as_array_mut()
            .unwrap();
        modules.retain(|m| m["class"] != "ParticleModuleVelocity");
        modules.push(orbit);
        let mut s = instance(&v, 3);
        s.activate();
        run(&mut s, 1.0, 1.0 / 60.0);
        let p = &s.emitters[0].particles[0];
        // After one second: a quarter turn, the offset points along +Y.
        assert!(
            (p.orbit_offset - Vec3::new(0.0, 100.0, 0.0)).length() < 1.0,
            "{}",
            p.orbit_offset
        );
        assert!((p.orbit[0].rotation.z - 0.25).abs() < 1e-3);
        let r = s.render();
        assert!((r[0].sprites[0].position - Vec3::new(0.0, 100.0, 0.0)).length() < 1.0);
    }

    #[test]
    fn attractor_point_pulls_within_its_scaled_range() {
        let mut v = system_json(
            0.0,
            100.0,
            json!([{"Count": 1, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        let c = |x: f32| dist_json("constant", json!({"value": [x], "locked_axes": 0}));
        let attract = |range: f32| {
            json!({"class": "ParticleModuleAttractorPoint", "enabled": true, "spawn": false, "update": true, "params": {
            "Position": {"dist": "vector", "value": {"kind": "constant", "value": [100, 0, 0], "locked_axes": 0}},
            "Range": c(range), "Strength": c(60.0), "bAffectBaseVelocity": true}})
        };
        let run_with = |range: f32| {
            let mut v = v.clone();
            let modules = v["emitters"][0]["lods"][0]["modules"]
                .as_array_mut()
                .unwrap();
            modules.retain(|m| m["class"] != "ParticleModuleVelocity");
            modules.push(attract(range));
            let mut s = instance(&v, 3);
            s.activate();
            s.tick(1.0 / 60.0, None);
            s.emitters[0].particles[0].base_velocity
        };
        // Range 60 · √3 ≈ 103.9 reaches the particle 100 away; 50 · √3 does not.
        let pulled = run_with(60.0);
        assert!(
            (pulled.x - 60.0 * 3f32.sqrt() / 60.0).abs() < 1e-4,
            "{pulled}"
        );
        assert_eq!(run_with(50.0), Vec3::ZERO);
        let _ = &mut v;
    }

    #[test]
    fn beams_run_from_source_to_target() {
        let mut v = system_json(
            0.0,
            5.0,
            json!([{"Count": 2, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        v["emitters"][0]["kind"] = json!("beam");
        let target = json!({"class": "ParticleModuleBeamTarget", "enabled": true, "spawn": true, "update": true, "params": {
            "Target": {"dist": "vector", "value": {"kind": "constant", "value": [500, 0, 0], "locked_axes": 0}},
            "bTargetAbsolute": false}});
        v["emitters"][0]["lods"][0]["modules"]
            .as_array_mut()
            .unwrap()
            .push(target);
        let def = std::sync::Arc::new(SystemDef::from_json("Test.Beam", &v));
        let at = Mat4::from_translation(Vec3::new(100.0, 200.0, 300.0));
        let mut s = SystemInstance::new(def, at, InstanceParams::default(), 1);
        s.activate();
        s.tick(1.0 / 60.0, None);
        let r = s.render();
        assert_eq!(r.len(), 1);
        assert!(r[0].sprites.is_empty());
        assert_eq!(r[0].beams.len(), 2);
        assert_eq!(r[0].beams[0].start, Vec3::new(100.0, 200.0, 300.0));
        assert_eq!(
            r[0].beams[0].end,
            Vec3::new(600.0, 200.0, 300.0),
            "relative target"
        );
        // Script points override both ends.
        s.beam_source = Some(Vec3::ZERO);
        s.beam_target = Some(Vec3::new(0.0, 0.0, 50.0));
        let r = s.render();
        assert_eq!(
            (r[0].beams[1].start, r[0].beams[1].end),
            (Vec3::ZERO, Vec3::new(0.0, 0.0, 50.0))
        );
    }

    #[test]
    fn lod_by_distance() {
        let mut s = instance(&system_json(10.0, 1.0, json!([]), 0), 6);
        assert_eq!(s.lod_for_distance(0.0), 0);
        assert_eq!(s.lod_for_distance(999.0), 0);
        assert_eq!(
            s.lod_for_distance(1000.0),
            1,
            "an entry equal to the distance is passed"
        );
        assert_eq!(s.lod_for_distance(1500.0), 1);
        s.activate();
        assert!(s.lod_pending, "the activation's choice waits for a viewer");
        // Only one LOD exists: the emitter stays on it.
        s.tick(1.0 / 60.0, Some(Vec3::new(5000.0, 0.0, 0.0)));
        assert_eq!(s.lod, 1, "chosen on the first tick that knows the viewer");
        assert_eq!(s.emitters[0].lod, 0);
        // Automatic: chosen again once more than LODDistanceCheckTime (the
        // 0.25 s default) has passed, not before.
        s.tick(0.2, Some(Vec3::ZERO));
        assert_eq!(s.lod, 1);
        s.tick(0.1, Some(Vec3::ZERO));
        assert_eq!(s.lod, 0);
    }

    /// The engine's rule compares from entry 1 on and stops at the first
    /// entry beyond the distance; entry 0 and anything after that entry are
    /// never looked at.
    #[test]
    fn lod_rule_follows_the_native_scan() {
        let lods = |d: serde_json::Value| {
            let mut v = system_json(1.0, 1.0, json!([]), 0);
            v["params"]["LODDistances"] = d;
            instance(&v, 0)
        };
        let s = lods(json!([500.0, 1000.0, 2500.0]));
        assert_eq!(s.lod_for_distance(100.0), 0, "entry 0 is not compared");
        assert_eq!(s.lod_for_distance(999.0), 0);
        assert_eq!(s.lod_for_distance(2499.0), 1);
        assert_eq!(s.lod_for_distance(2500.0), 2);
        assert_eq!(s.lod_for_distance(1.0e9), 2);
        // Not ascending: the scan stops at the first greater entry.
        let s = lods(json!([0.0, 2500.0, 1000.0]));
        assert_eq!(s.lod_for_distance(1500.0), 0);
        assert_eq!(s.lod_for_distance(3000.0), 2);
        // One entry or none: always 0.
        assert_eq!(lods(json!([0.0])).lod_for_distance(1.0e6), 0);
        assert_eq!(lods(json!([])).lod_for_distance(1.0e6), 0);
        assert_eq!(lods(json!([0.0, 10.0])).lod_for_distance(f32::NAN), 1);
    }

    /// `DirectSet` systems never change level by distance;
    /// `ActivateAutomatic` ones choose once per activation.
    #[test]
    fn lod_methods() {
        let with = |method: &str| {
            let mut v = system_json(1.0, 1.0, json!([]), 0);
            v["params"]["LODMethod"] = json!(method);
            instance(&v, 0)
        };
        let far = Some(Vec3::new(5000.0, 0.0, 0.0));
        let mut direct = with("PARTICLESYSTEMLODMETHOD_DirectSet");
        assert_eq!(direct.def.lod_method, LodMethod::DirectSet);
        direct.activate();
        for _ in 0..60 {
            direct.tick(1.0 / 60.0, far);
        }
        assert_eq!(direct.lod, 0);
        let mut once = with("PARTICLESYSTEMLODMETHOD_ActivateAutomatic");
        once.activate();
        once.tick(1.0 / 60.0, far);
        assert_eq!(once.lod, 1);
        for _ in 0..60 {
            once.tick(1.0 / 60.0, Some(Vec3::ZERO));
        }
        assert_eq!(once.lod, 1, "kept until the next activation");
        once.activate();
        once.tick(1.0 / 60.0, Some(Vec3::ZERO));
        assert_eq!(once.lod, 0);
        assert_eq!(
            with("PARTICLESYSTEMLODMETHOD_Automatic").def.lod_method,
            LodMethod::Automatic
        );
    }

    #[test]
    fn hostile_systems_stay_bounded() {
        // Huge rate, negative lifetime, NaN-free garbage: bounded and finite.
        let mut v = system_json(
            1.0e9,
            -1.0,
            json!([{"Count": 2_000_000_000, "CountLow": -1, "Time": 0.0}]),
            0,
        );
        v["params"]["WarmupTime"] = json!(1.0e9);
        let mut s = instance(&v, 10);
        s.activate();
        run(&mut s, 1.0, 1.0 / 30.0);
        assert!(s.particle_count() <= MAX_PARTICLES_PER_EMITTER);
        // Extreme burst counts do not overflow.
        let wild = system_json(
            0.0,
            1.0,
            json!([{"Count": i32::MIN, "CountLow": i32::MAX, "Time": 0.0}, {"Count": i32::MAX, "CountLow": 0, "Time": 0.0}]),
            0,
        );
        let mut w = instance(&wild, 11);
        w.activate();
        w.tick(1.0 / 60.0, None);
        assert!(w.particle_count() <= MAX_SUBUV_PARTICLES);
        // Malformed JSON shapes build empty but valid systems.
        let junk = SystemDef::from_json(
            "x",
            &json!({"emitters": [17, {"lods": "no"}], "params": [1, 2]}),
        );
        let mut j = SystemInstance::new(
            std::sync::Arc::new(junk),
            Mat4::IDENTITY,
            InstanceParams::default(),
            0,
        );
        j.activate();
        j.tick(1.0, Some(Vec3::ZERO));
        assert_eq!(j.particle_count(), 0);
        j.tick(f32::NAN, None);
        j.tick(-1.0, None);
        // Non-finite transforms fall back to identity.
        assert_eq!(ue_matrix(&[[f32::NAN; 4]; 4]), Mat4::IDENTITY);
    }

    // ------------------------------------------------------------------
    // Hand-checked behaviour of the emitter tick (verification pass)
    // ------------------------------------------------------------------

    fn cf(x: f32) -> serde_json::Value {
        json!({"dist": "float", "value": {"kind": "constant", "value": [x], "locked_axes": 0}})
    }

    fn cv(x: f32, y: f32, z: f32) -> serde_json::Value {
        json!({"dist": "vector", "value": {"kind": "constant", "value": [x, y, z], "locked_axes": 0}})
    }

    fn module(
        class: &str,
        spawn: bool,
        update: bool,
        params: serde_json::Value,
    ) -> serde_json::Value {
        json!({"class": class, "enabled": true, "spawn": spawn, "update": update, "params": params})
    }

    /// One sprite emitter (new emitter time, duration 1 s, endless) with a
    /// constant lifetime and the given modules.
    fn bare_system(
        rate: f32,
        life: f32,
        bursts: serde_json::Value,
        modules: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        let mut all = vec![module(
            "ParticleModuleLifetime",
            true,
            false,
            json!({"Lifetime": cf(life)}),
        )];
        all.extend(modules);
        json!({
            "path": "Test.Bare",
            "params": {},
            "emitters": [{"name": "E", "kind": "sprite", "params": {}, "lods": [{
                "level": 0, "enabled": true,
                "required": {"class": "ParticleModuleRequired", "enabled": true, "params": {
                    "EmitterDuration": 1.0, "EmitterLoops": 0, "bUseLegacyEmitterTime": false}},
                "spawn": {"class": "ParticleModuleSpawn", "enabled": true, "params": {
                    "Rate": cf(rate), "RateScale": cf(1.0), "BurstList": bursts}},
                "type_data": null,
                "modules": all
            }]}]
        })
    }

    fn one_burst(count: i32) -> serde_json::Value {
        json!([{"Count": count, "CountLow": -1, "Time": 0.0}])
    }

    const DT: f32 = 1.0 / 60.0;

    /// The executable's order: kill, spawn, reset, update, integrate. With
    /// 90 particles a second at 60 Hz, a lifetime of 1 s and a velocity of
    /// 180 UU/s up, by hand:
    ///
    /// tick 1: leftover 0 + 1.5 → one particle, increment 1/90, spawn time
    /// 1/60 − 1/90 = 1/180. Relative time 1/180 at spawn; `PostSpawn` moves
    /// it 180 · 1/180 = 1; the reset ages it by 1/60 (→ 4/180); the
    /// integration moves it 3 more (→ 4). Leftover 0.5.
    /// tick 2: leftover 0.5 + 1.5 → two particles with spawn times 2/180
    /// and 0: heights 2 + 3 and 0 + 3, relative times 5/180 and 3/180; the
    /// first particle is at 7 and 7/180.
    #[test]
    fn spawn_then_reset_orders_the_first_tick() {
        let v = bare_system(
            90.0,
            1.0,
            json!([]),
            vec![module(
                "ParticleModuleVelocity",
                true,
                false,
                json!({"StartVelocity": cv(0.0, 0.0, 180.0), "StartVelocityRadial": cf(0.0), "bInWorldSpace": true}),
            )],
        );
        let mut s = instance(&v, 1);
        s.activate();
        s.tick(DT, None);
        let e = &s.emitters[0];
        assert_eq!(e.particles.len(), 1);
        assert!((e.particles[0].relative_time - 4.0 / 180.0).abs() < 1e-6);
        assert!((e.particles[0].location.z - 4.0).abs() < 1e-4);
        assert!(
            (e.particles[0].old_location.z - 1.0).abs() < 1e-4,
            "where PostSpawn left it"
        );
        assert!((e.spawn_fraction - 0.5).abs() < 1e-5);
        s.tick(DT, None);
        let e = &s.emitters[0];
        let z: Vec<f32> = e.particles.iter().map(|p| p.location.z).collect();
        let rt: Vec<f32> = e
            .particles
            .iter()
            .map(|p| p.relative_time * 180.0)
            .collect();
        assert_eq!(z.len(), 3);
        for (got, want) in z.iter().zip([7.0, 5.0, 3.0]) {
            assert!((got - want).abs() < 1e-4, "{z:?}");
        }
        for (got, want) in rt.iter().zip([7.0, 5.0, 3.0]) {
            assert!((got - want).abs() < 1e-4, "{rt:?}");
        }
        assert!(e.spawn_fraction < 1e-5);
    }

    /// A particle dies in the tick after its relative time passed 1: with a
    /// 0.1 s lifetime at 60 Hz a burst particle is at 6/6 after six ticks
    /// (not past 1, alive in tick 7 whatever the rounding of 6 · 1/6), at
    /// 7/6 after seven, and is gone in the eighth.
    #[test]
    fn lifetime_counts_the_spawning_tick() {
        let mut s = instance(&bare_system(0.0, 0.1, one_burst(1), vec![]), 1);
        s.activate();
        for _ in 0..5 {
            s.tick(DT, None);
        }
        assert_eq!(s.particle_count(), 1);
        assert!((s.emitters[0].particles[0].relative_time - 5.0 / 6.0).abs() < 1e-5);
        s.tick(DT, None);
        s.tick(DT, None);
        assert_eq!(
            s.particle_count(),
            1,
            "relative time 7/6 is past 1 but the kill is next tick"
        );
        s.tick(DT, None);
        assert_eq!(s.particle_count(), 0);
    }

    /// The reset after the spawn puts transient values back before the
    /// update modules run, so a life multiplier counts once in the spawning
    /// tick, and base values survive.
    #[test]
    fn life_multipliers_apply_once_per_tick() {
        let v = bare_system(
            0.0,
            10.0,
            one_burst(1),
            vec![
                module(
                    "ParticleModuleSize",
                    true,
                    false,
                    json!({"StartSize": cv(10.0, 10.0, 10.0)}),
                ),
                module(
                    "ParticleModuleSizeMultiplyLife",
                    true,
                    true,
                    json!({"LifeMultiplier": cv(0.5, 0.25, 1.0), "MultiplyX": true, "MultiplyY": true, "MultiplyZ": false}),
                ),
                module(
                    "ParticleModuleColor",
                    true,
                    false,
                    json!({"StartColor": cv(1.0, 0.8, 0.6), "StartAlpha": cf(1.0)}),
                ),
                module(
                    "ParticleModuleColorScaleOverLife",
                    true,
                    true,
                    json!({"ColorScaleOverLife": cv(0.5, 0.5, 0.5), "AlphaScaleOverLife": cf(0.25)}),
                ),
                module(
                    "ParticleModuleRotationRate",
                    true,
                    false,
                    json!({"StartRotationRate": cf(1.0)}),
                ),
                module(
                    "ParticleModuleRotationRateMultiplyLife",
                    true,
                    true,
                    json!({"LifeMultiplier": cf(0.5)}),
                ),
            ],
        );
        let mut s = instance(&v, 1);
        s.activate();
        for tick in 0..3 {
            s.tick(DT, None);
            let p = &s.emitters[0].particles[0];
            assert_eq!(p.base_size, Vec3::splat(10.0));
            assert_eq!(p.size, Vec3::new(5.0, 2.5, 10.0), "tick {tick}");
            assert_eq!(p.color, [0.5, 0.4, 0.3, 0.25], "tick {tick}");
            let turn = std::f32::consts::TAU;
            assert!((p.base_rotation_rate - turn).abs() < 1e-5);
            assert!((p.rotation_rate - 0.5 * turn).abs() < 1e-5);
        }
    }

    /// `bClampAlpha` is an editor display flag: `Spawn` / `Update` store the
    /// alpha as evaluated.
    #[test]
    fn alpha_is_not_clamped_at_run_time() {
        let v = bare_system(
            0.0,
            10.0,
            one_burst(2),
            vec![module(
                "ParticleModuleColor",
                true,
                false,
                json!({"StartColor": cv(1.0, 1.0, 1.0), "StartAlpha": cf(5.0), "bClampAlpha": true}),
            )],
        );
        let mut s = instance(&v, 1);
        s.activate();
        s.tick(DT, None);
        assert_eq!(s.emitters[0].particles[0].color[3], 5.0);
        let over = bare_system(
            0.0,
            10.0,
            one_burst(1),
            vec![module(
                "ParticleModuleColorOverLife",
                true,
                true,
                json!({"ColorOverLife": cv(1.0, 1.0, 1.0), "AlphaOverLife": cf(-2.0), "bClampAlpha": true}),
            )],
        );
        let mut s = instance(&over, 1);
        s.activate();
        s.tick(DT, None);
        s.tick(DT, None);
        assert_eq!(s.emitters[0].particles[0].color[3], -2.0);
    }

    /// Rotation integrates the rate and is wrapped into one turn: at 0.75
    /// turns a second it reads 0.5 turns after two seconds.
    #[test]
    fn rotation_wraps_into_one_turn() {
        let v = bare_system(
            0.0,
            100.0,
            one_burst(1),
            vec![module(
                "ParticleModuleRotationRate",
                true,
                false,
                json!({"StartRotationRate": cf(0.75)}),
            )],
        );
        let mut s = instance(&v, 1);
        s.activate();
        let mut max = 0.0f32;
        for _ in 0..120 {
            s.tick(DT, None);
            max = max.max(s.emitters[0].particles[0].rotation);
        }
        assert!(max < std::f32::consts::TAU, "{max}");
        let r = s.emitters[0].particles[0].rotation;
        assert!((r - std::f32::consts::PI).abs() < 1e-2, "{r}");
    }

    /// Two linked orbit modules. The second link's rotation vector is first
    /// turned by the first link's matrix, read as Euler angles, and appended
    /// after the first rotation; the offsets add up. Expected values from an
    /// independent row-vector computation (first link: 100 along X, an
    /// eighth of a turn of yaw; second link: 10 along Z, rotation
    /// (0.25, 0.1, 0) turns): (70.7107, 70.7107, 0) + (−7.8593, 6.1819,
    /// 0.1241).
    #[test]
    fn linked_orbit_chains_compose_like_the_engine() {
        let spawn = json!({"bProcessDuringSpawn": true});
        let orbit = |offset: serde_json::Value, rotation: serde_json::Value| {
            module(
                "ParticleModuleOrbit",
                true,
                true,
                json!({"ChainMode": "EOChainMode_Link",
                    "OffsetAmount": offset, "OffsetOptions": spawn,
                    "RotationAmount": rotation, "RotationOptions": spawn,
                    "RotationRateAmount": cv(0.0, 0.0, 0.0), "RotationRateOptions": spawn}),
            )
        };
        let v = bare_system(
            0.0,
            100.0,
            one_burst(1),
            vec![
                orbit(cv(100.0, 0.0, 0.0), cv(0.0, 0.0, 0.125)),
                orbit(cv(0.0, 0.0, 10.0), cv(0.25, 0.1, 0.0)),
            ],
        );
        let mut s = instance(&v, 1);
        s.activate();
        let want = Vec3::new(62.8513, 76.8925, 0.1241);
        for _ in 0..3 {
            s.tick(DT, None);
            let got = s.emitters[0].particles[0].orbit_offset;
            assert!((got - want).length() < 1e-3, "{got}");
        }
        // The same chain with `Add`: values accumulate into one link.
        let add = |offset: serde_json::Value| {
            module(
                "ParticleModuleOrbit",
                true,
                true,
                json!({"ChainMode": "EOChainMode_Add",
                    "OffsetAmount": offset, "OffsetOptions": spawn,
                    "RotationAmount": cv(0.0, 0.0, 0.125), "RotationOptions": spawn,
                    "RotationRateAmount": cv(0.0, 0.0, 0.0), "RotationRateOptions": spawn}),
            )
        };
        let v = bare_system(
            0.0,
            100.0,
            one_burst(1),
            vec![add(cv(60.0, 0.0, 0.0)), add(cv(40.0, 0.0, 0.0))],
        );
        let mut s = instance(&v, 1);
        s.activate();
        s.tick(DT, None);
        // Offsets add to 100 along X, rotations to a quarter turn of yaw.
        let got = s.emitters[0].particles[0].orbit_offset;
        assert!((got - Vec3::new(0.0, 100.0, 0.0)).length() < 1e-3, "{got}");
    }

    /// `SizeScaleByTime` keeps its own clock per particle: the spawn time,
    /// plus every update's step. Curve 1 → 3 over two seconds, X only: after
    /// one tick 10 · (1 + 1/60), after sixty 10 · 2, whatever the lifetime.
    #[test]
    fn size_scale_by_time_reads_seconds_since_spawn() {
        let curve = json!({"dist": "vector", "value": {"kind": "constant_curve", "locked_axes": 0, "curve": {"dim": 3, "keys": [
            {"t": 0, "v": [1, 1, 1], "arrive": [0, 0, 0], "leave": [0, 0, 0], "mode": "linear"},
            {"t": 2, "v": [3, 3, 3], "arrive": [0, 0, 0], "leave": [0, 0, 0], "mode": "linear"}]}}});
        for life in [5.0, 500.0] {
            let v = bare_system(
                0.0,
                life,
                one_burst(1),
                vec![
                    module(
                        "ParticleModuleSize",
                        true,
                        false,
                        json!({"StartSize": cv(10.0, 10.0, 10.0)}),
                    ),
                    module(
                        "ParticleModuleSizeScaleByTime",
                        true,
                        true,
                        json!({"SizeScaleByTime": curve, "bEnableX": true, "bEnableY": false, "bEnableZ": false}),
                    ),
                ],
            );
            let mut s = instance(&v, 1);
            s.activate();
            s.tick(DT, None);
            let p = &s.emitters[0].particles[0];
            assert!((p.size.x - 10.0 * (1.0 + DT)).abs() < 1e-4, "{}", p.size.x);
            assert_eq!(p.size.y, 10.0);
            for _ in 1..60 {
                s.tick(DT, None);
            }
            let p = &s.emitters[0].particles[0];
            assert!((p.size.x - 20.0).abs() < 1e-3, "{}", p.size.x);
            assert!((p.scale_time[0] - 1.0).abs() < 1e-4);
        }
    }

    /// The system's `Delay` is the component's emitter delay: added to each
    /// emitter's own delay and to its duration, not a pause before ticking.
    #[test]
    fn system_delay_adds_to_the_emitter_delay() {
        let mut v = bare_system(60.0, 10.0, json!([]), vec![]);
        v["params"]["Delay"] = json!(0.51);
        let mut s = instance(&v, 1);
        s.activate();
        assert!((s.emitters[0].delay - 0.51).abs() < 1e-6);
        assert!((s.emitters[0].duration - 1.51).abs() < 1e-6);
        for _ in 0..30 {
            s.tick(DT, None);
        }
        assert_eq!(s.particle_count(), 0, "emitter time 0.5 − 0.51 is negative");
        assert!(
            (s.emitters[0].seconds_since_creation - 0.5).abs() < 1e-4,
            "the emitter ticks during the delay"
        );
        s.tick(DT, None);
        assert_eq!(s.particle_count(), 1);
        // A range is drawn per initialization, reproducibly.
        v["params"]["Delay"] = json!(1.0);
        v["params"]["DelayLow"] = json!(0.5);
        v["params"]["bUseDelayRange"] = json!(true);
        let (mut a, mut b) = (instance(&v, 9), instance(&v, 9));
        a.activate();
        b.activate();
        let d = a.emitters[0].delay;
        assert!((0.5..=1.0).contains(&d), "{d}");
        assert_eq!(d, b.emitters[0].delay);
    }

    /// A fixed-time system advances by its own delta once per tick call.
    #[test]
    fn fixed_time_systems_step_their_own_delta() {
        let mut v = bare_system(30.0, 100.0, json!([]), vec![]);
        v["params"]["SystemUpdateMode"] = json!("EPSUM_FixedTime");
        v["params"]["UpdateTime_Delta"] = json!(1.0 / 30.0);
        let mut s = instance(&v, 1);
        s.activate();
        for _ in 0..60 {
            s.tick(DT, None);
        }
        assert!((s.emitters[0].seconds_since_creation - 2.0).abs() < 1e-3);
        assert_eq!(
            s.particle_count(),
            60,
            "30 a second over two simulated seconds"
        );
        s.tick(0.5, None);
        assert!((s.emitters[0].seconds_since_creation - (2.0 + 1.0 / 30.0)).abs() < 1e-3);
    }

    /// `ActivateSystem` on a system that ran before initializes the emitter
    /// instances again: clock, loops and bursts start over, the particles on
    /// screen stay. `reset` drops everything and replays the first run.
    #[test]
    fn reactivation_keeps_live_particles_and_restarts_the_clock() {
        let v = bare_system(60.0, 10.0, one_burst(5), vec![]);
        let mut s = instance(&v, 3);
        s.activate();
        for _ in 0..30 {
            s.tick(DT, None);
        }
        assert_eq!(s.particle_count(), 35);
        let first_run = s.render();
        s.deactivate();
        s.tick(DT, None);
        assert_eq!(s.particle_count(), 35, "no spawning while deactivated");
        s.activate();
        assert_eq!(s.state, ActivityState::Active);
        assert_eq!(
            s.particle_count(),
            35,
            "live particles survive the activation"
        );
        assert_eq!(s.emitters[0].seconds_since_creation, 0.0);
        assert_eq!(s.emitters[0].loop_count, 0);
        s.tick(DT, None);
        assert_eq!(s.particle_count(), 35 + 5 + 1, "the burst fires again");
        // Activating a running system does the same.
        s.activate();
        s.tick(DT, None);
        assert_eq!(s.particle_count(), 41 + 5 + 1);
        // A reset replays the first run exactly.
        s.reset();
        assert_eq!((s.particle_count(), s.state), (0, ActivityState::Inactive));
        s.activate();
        for _ in 0..30 {
            s.tick(DT, None);
        }
        assert_eq!(s.render(), first_run);
    }

    /// A system with `bSkipSpawnCountCheck` is not held to the sprite count;
    /// the storage limit still applies (a tick that would pass it spawns
    /// nothing and keeps its leftover).
    #[test]
    fn spawn_count_check_can_be_skipped_by_the_system() {
        let mut v = bare_system(6000.0, 100.0, json!([]), vec![]);
        let mut held = instance(&v, 1);
        held.activate();
        v["params"]["bSkipSpawnCountCheck"] = json!(true);
        let mut free = instance(&v, 1);
        assert!(free.def.skip_spawn_count_check);
        free.activate();
        for _ in 0..60 {
            held.tick(DT, None);
            free.tick(DT, None);
        }
        assert_eq!(held.particle_count(), MAX_SPRITE_PARTICLES);
        // 100 a tick: ten ticks fit (1000 + trunc(√√1000 + 1) = 1006), the
        // eleventh would need 1100.
        assert_eq!(free.particle_count(), 1000);
        assert!(free.particle_count() <= MAX_PARTICLES_PER_EMITTER);
    }

    #[test]
    fn kill_height_and_kill_box_follow_the_native_tests() {
        let fall = |kill: serde_json::Value, at: Mat4| {
            let v = bare_system(
                0.0,
                100.0,
                one_burst(1),
                vec![
                    module(
                        "ParticleModuleVelocity",
                        true,
                        false,
                        json!({"StartVelocity": cv(0.0, 0.0, -600.0), "StartVelocityRadial": cf(0.0), "bInWorldSpace": true}),
                    ),
                    kill,
                ],
            );
            let def = std::sync::Arc::new(SystemDef::from_json("Test.Kill", &v));
            let mut s = SystemInstance::new(def, at, InstanceParams::default(), 1);
            s.activate();
            let mut alive = Vec::new();
            for _ in 0..3 {
                s.tick(DT, None);
                alive.push(s.particle_count());
            }
            alive
        };
        let height = |floor: bool, absolute: bool| {
            module(
                "ParticleModuleKillHeight",
                false,
                true,
                json!({"Height": cf(0.0), "bFloor": floor, "bAbsolute": absolute}),
            )
        };
        let up = Mat4::from_translation(Vec3::new(0.0, 0.0, 1000.0));
        // Relative floor at the component's height: the particle is at it in
        // the first update (not below), 10 UU below in the second.
        assert_eq!(fall(height(true, false), up), vec![1, 0, 0]);
        // Absolute floor at 0: far above.
        assert_eq!(fall(height(true, true), up), vec![1, 1, 1]);
        // A ceiling kills what is above it: the particle falls away from it.
        assert_eq!(fall(height(false, false), up), vec![1, 1, 1]);
        assert_eq!(fall(height(false, true), up), vec![0, 0, 0]);
        let cube = |inside: bool| {
            module(
                "ParticleModuleKillBox",
                false,
                true,
                json!({"LowerLeftCorner": cv(-5.0, -5.0, -5.0), "UpperRightCorner": cv(5.0, 5.0, 5.0), "bKillInside": inside}),
            )
        };
        // Inside the box (which follows the component) in the first update.
        assert_eq!(fall(cube(true), up), vec![0, 0, 0]);
        assert_eq!(
            fall(cube(false), up),
            vec![1, 0, 0],
            "killed once it has left"
        );
    }

    /// Random sub-images are drawn at spawn and stay unless
    /// `RandomImageChanges` is set; with it the time between changes is
    /// `0.99 / (changes + 1)` of the lifetime.
    #[test]
    fn random_sub_images_change_only_when_asked() {
        let build = |changes: Option<i32>| {
            let mut v = bare_system(
                0.0,
                1.0,
                one_burst(24),
                vec![module(
                    "ParticleModuleSubUV",
                    true,
                    true,
                    json!({"SubImageIndex": cf(0.0)}),
                )],
            );
            let r = &mut v["emitters"][0]["lods"][0]["required"]["params"];
            r["InterpolationMethod"] = json!("PSUVIM_Random");
            r["SubImages_Horizontal"] = json!(4);
            r["SubImages_Vertical"] = json!(4);
            if let Some(c) = changes {
                r["RandomImageChanges"] = json!(c);
            }
            instance(&v, 5)
        };
        let images = |s: &SystemInstance| -> Vec<u32> {
            s.emitters[0].particles.iter().map(|p| p.sub_uv.0).collect()
        };
        let mut fixed = build(None);
        assert_eq!(
            fixed.def.emitters[0].lods[0].required.random_image_time,
            1.0
        );
        fixed.activate();
        fixed.tick(DT, None);
        let at_spawn = images(&fixed);
        assert!(at_spawn.iter().all(|i| *i < 16));
        assert!(
            at_spawn.iter().any(|i| *i != at_spawn[0]),
            "24 draws of 16 images"
        );
        for _ in 0..40 {
            fixed.tick(DT, None);
        }
        assert_eq!(images(&fixed), at_spawn);
        let mut changing = build(Some(4));
        let t = changing.def.emitters[0].lods[0].required.random_image_time;
        assert!((t - 0.99 / 5.0).abs() < 1e-6);
        changing.activate();
        changing.tick(DT, None);
        let at_spawn = images(&changing);
        for _ in 0..8 {
            changing.tick(DT, None);
        }
        assert_eq!(
            images(&changing),
            at_spawn,
            "9/60 of the lifetime is within 0.198"
        );
        for _ in 0..8 {
            changing.tick(DT, None);
        }
        assert_ne!(images(&changing), at_spawn, "17/60 is past it");
    }

    /// An emitter whose current LOD level is disabled is not ticked.
    #[test]
    fn disabled_lod_levels_freeze_their_emitter() {
        let mut v = bare_system(60.0, 1.0, json!([]), vec![]);
        v["emitters"][0]["lods"][0]["enabled"] = json!(false);
        let mut s = instance(&v, 1);
        s.activate();
        for _ in 0..30 {
            s.tick(DT, None);
        }
        assert_eq!(s.particle_count(), 0);
        assert_eq!(s.emitters[0].seconds_since_creation, 0.0);
        assert!(s.render().is_empty());
    }

    /// Two instances with one seed stay identical through every tick of a
    /// mixed run (uneven steps, deactivation, re-activation, a moved
    /// component, a LOD change); another seed differs.
    #[test]
    fn simulation_replays_exactly_step_by_step() {
        let v = system_json(
            75.0,
            0.8,
            json!([{"Count": 6, "CountLow": 2, "Time": 0.25}]),
            0,
        );
        let mut a = instance(&v, 1234);
        let mut b = instance(&v, 1234);
        let mut c = instance(&v, 1235);
        let mut differs = false;
        for s in [&mut a, &mut b, &mut c] {
            s.activate();
        }
        for step in 0..400u32 {
            let dt = [DT, 0.011, 0.033, 0.25][(step % 4) as usize];
            let viewer = Some(Vec3::new(step as f32 * 10.0, 0.0, 0.0));
            for s in [&mut a, &mut b, &mut c] {
                if step == 120 {
                    s.deactivate();
                }
                if step == 150 || step == 300 {
                    s.activate();
                }
                if step == 200 {
                    s.transform = Mat4::from_translation(Vec3::new(50.0, -20.0, 5.0));
                }
                s.tick(dt, viewer);
            }
            assert_eq!(a.render(), b.render(), "step {step}");
            assert_eq!(a.particle_count(), b.particle_count());
            assert_eq!(a.emitters[0].spawn_fraction, b.emitters[0].spawn_fraction);
            assert_eq!(a.emitters[0].emitter_time, b.emitters[0].emitter_time);
            assert_eq!((a.lod, a.state), (b.lod, b.state));
            differs |= a.render() != c.render();
            for p in &a.emitters[0].particles {
                assert!(p.location.is_finite() && p.relative_time.is_finite());
                assert!(p.rotation.abs() < std::f32::consts::TAU);
            }
            assert!(a.particle_count() <= MAX_SUBUV_PARTICLES);
        }
        assert!(differs);
    }

    /// Engine-exact curve arithmetic by hand, beyond the linear cases: a
    /// cubic segment with both tangents, per component, over a span of 0.5.
    ///
    /// Keys at t = 1 (value 2, leave tangent 4) and t = 1.5 (value 3, arrive
    /// tangent −2); at t = 1.125, a = 0.25: h00 = 0.84375, h10 = 0.140625,
    /// h11 = −0.046875, h01 = 0.15625; tangents scaled by the span: 2 and
    /// −1 → 0.84375 · 2 + 0.140625 · 2 + (−0.046875)(−1) + 0.15625 · 3 =
    /// 2.484375. Unscaled (broken) tangents: 1.6875 + 0.5625 + 0.09375 +
    /// 0.46875 = 2.8125.
    #[test]
    fn cubic_curves_match_hand_values() {
        let mut k0 = key(1.0, &[2.0, -2.0], CurveMode::Cubic);
        k0.leave = vec![4.0, -4.0];
        let mut k1 = key(1.5, &[3.0, -3.0], CurveMode::Cubic);
        k1.arrive = vec![-2.0, 2.0];
        let curve = Curve {
            dim: 2,
            keys: vec![k0, k1],
            broken_tangents: false,
        };
        assert_eq!(curve.eval(1.125), vec![2.484_375, -2.484_375]);
        let broken = Curve {
            broken_tangents: true,
            ..curve.clone()
        };
        assert_eq!(broken.eval(1.125), vec![2.8125, -2.8125]);
        assert_eq!(curve.eval(1.0), vec![2.0, -2.0], "at the first key");
        assert_eq!(curve.eval(1.5), vec![3.0, -3.0], "at the last key");
        // A float uniform curve draws between the two components.
        let d = Dist::UniformCurve {
            curve,
            lock: [0; 2],
            mirror: [1; 3],
            extremes: false,
        };
        let mut rng = Rng(11);
        let mut probe = rng;
        let r = probe.next_f32();
        let v = d.eval_f32(1.125, &mut rng, &none());
        assert_eq!(v, (-2.484_375 - 2.484_375) * r + 2.484_375);
        assert_eq!(rng, probe, "one draw");
    }

    /// The generator against values computed by hand from the recurrence
    /// (seed · 0x0BB38435 + 0x3619636B, low 23 bits as a mantissa).
    #[test]
    fn rng_sequence_by_hand() {
        let mut r = Rng(1);
        // 1 · 0x0BB38435 + 0x3619636B = 0x41CCE7A0.
        let a = r.next_f32();
        assert_eq!(r.0, 0x41CC_E7A0);
        assert_eq!(a, f32::from_bits(0x3F80_0000 | 0x004C_E7A0) - 1.0);
        // 0x41CCE7A0 · 0x0BB38435 + 0x3619636B mod 2³² = 0x51D3D78B.
        let b = r.next_f32();
        assert_eq!(r.0, 0x51D3_D78B);
        assert_eq!(b, f32::from_bits(0x3F80_0000 | 0x0053_D78B) - 1.0);
        assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b));
    }

    #[test]
    fn counts_from_files_are_bounded() {
        let r = sub_image_rect(7, [u32::MAX, u32::MAX]);
        assert!(
            r.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)),
            "no overflow: {r:?}"
        );
        let tmp =
            std::env::temp_dir().join(format!("asamu-particles-bounds-{}", std::process::id()));
        let maps = tmp.join("particles").join("maps");
        std::fs::create_dir_all(&maps).unwrap();
        let one = json!({"actor": "E", "actor_path": "M.TheWorld.PersistentLevel.E",
            "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]], "template": "t",
            "instance_parameters": (0..MAX_INSTANCE_PARAMS + 10).map(|i| json!({"name": format!("p{i}"), "param_type": "PSPT_Scalar", "scalar": 1.0})).collect::<Vec<_>>()});
        let many: Vec<_> = (0..MAX_PLACEMENTS + 5).map(|_| one.clone()).collect();
        let file = json!({"format": PLACEMENTS_FORMAT, "version": PLACEMENTS_VERSION, "placements": many[..3]});
        std::fs::write(maps.join("Small.json"), serde_json::to_vec(&file).unwrap()).unwrap();
        let p = load_placements(&tmp, "Small").unwrap().unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].instance_parameters.len(), MAX_INSTANCE_PARAMS);
        let lean = json!({"actor": "E", "actor_path": "M.E",
            "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]], "template": "t"});
        let file = json!({"format": PLACEMENTS_FORMAT, "version": PLACEMENTS_VERSION,
            "placements": (0..MAX_PLACEMENTS + 5).map(|_| lean.clone()).collect::<Vec<_>>()});
        std::fs::write(maps.join("Big.json"), serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(
            load_placements(&tmp, "Big").unwrap().unwrap().len(),
            MAX_PLACEMENTS
        );
        let _ = std::fs::remove_dir_all(&tmp);
        // A hostile warm-up tick rate cannot spin: the tick count is capped.
        let mut v = bare_system(10.0, 1.0, json!([]), vec![]);
        v["params"]["WarmupTime"] = json!(30.0);
        v["params"]["WarmupTickRate"] = json!(1.0e-9);
        let mut s = instance(&v, 1);
        s.activate();
        assert!(s.emitters[0].seconds_since_creation < 1.0e-3);
        // The engine's default warm-up tick: 5 s take ceil(5 / 0.032) = 157 ticks.
        v["params"]["WarmupTime"] = json!(5.0);
        v["params"]["WarmupTickRate"] = json!(0.0);
        let mut s = instance(&v, 1);
        s.activate();
        assert!((s.emitters[0].seconds_since_creation - 157.0 * WARMUP_TICK).abs() < 1e-3);
    }

    #[test]
    fn library_and_placements_parse() {
        let tmp = std::env::temp_dir().join(format!("asamu-particles-test-{}", std::process::id()));
        let maps = tmp.join("particles").join("maps");
        std::fs::create_dir_all(&maps).unwrap();
        let lib = json!({"format": PARTICLES_FORMAT, "version": 1, "systems": {"Test.PS": system_json(5.0, 1.0, json!([]), 0)}});
        std::fs::write(
            ParticleLibrary::path(&tmp),
            serde_json::to_vec(&lib).unwrap(),
        )
        .unwrap();
        let place = json!({"format": PLACEMENTS_FORMAT, "version": 1, "map": "AG-Test", "placements": [{
            "actor": "Emitter_0", "actor_path": "AG-Test.TheWorld.PersistentLevel.Emitter_0",
            "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[10,20,30,1]],
            "template": "test.ps", "auto_activate": true,
            "instance_parameters": [{"name": "Tint", "param_type": "PSPT_Color", "color": [0, 128, 255, 255]}]
        }]});
        std::fs::write(
            maps.join("AG-Test.json"),
            serde_json::to_vec(&place).unwrap(),
        )
        .unwrap();
        let l = ParticleLibrary::load(&tmp).unwrap().unwrap();
        assert!(l.get("TEST.ps").is_some());
        let p = load_placements(&tmp, "ag-test").unwrap().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            p[0].matrix().transform_point3(Vec3::ZERO),
            Vec3::new(10.0, 20.0, 30.0)
        );
        let params = p[0].params();
        assert_eq!(params.vectors.get("tint"), Some(&[1.0, 128.0 / 255.0, 0.0]));
        assert!(load_placements(&tmp, "Missing").unwrap().is_none());
        assert!(load_placements(&tmp, "../etc").is_err());
        // Wrong format.
        std::fs::write(maps.join("Bad.json"), br#"{"format":"x","version":1}"#).unwrap();
        assert!(load_placements(&tmp, "Bad").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(ParticleLibrary::load(&tmp).unwrap().is_none());
    }

    #[test]
    fn particle_materials_bind_the_opacity_mask_channel() {
        use crate::manifest::{TextureEntry, TextureManifest};
        let entry = |file: &str| TextureEntry {
            package: "Fx".to_owned(),
            class: "Texture2D".to_owned(),
            file: file.to_owned(),
            format: "PF_DXT1".to_owned(),
            size: [64, 64],
            mips: 1,
            cube: false,
            srgb: Some(true),
            address: None,
            lod_group: None,
            compression_settings: None,
        };
        let mut t = BTreeMap::new();
        t.insert("Fx.T_Smoke".to_owned(), entry("Fx/T_Smoke.dds"));
        t.insert("Fx.T_Mask".to_owned(), entry("Fx/T_Mask.dds"));
        let textures = TextureManifest::from_entries(1, t);
        let file = json!({"materials": {
            "Fx.M_Mist": {"status": "approximated", "alpha_mode": "blend", "unlit": true,
                "base_color": {"source": "default", "value": [0, 0, 0, 1]},
                "emissive": {"value": [0.5, 0.5, 0.5, 1], "texture": {"texture": "Fx.T_Smoke"}},
                "opacity": {"value": [0.25, 0.25, 0.25, 0.25], "texture": {"texture": "Fx.T_Mask", "channels": "r"}}},
            "Fx.M_Dust": {"status": "approximated", "alpha_mode": "add", "unlit": true,
                "base_color": {"source": "default", "value": [0, 0, 0, 1]},
                "emissive": {"value": [1, 1, 1, 1], "texture": {"texture": "Fx.T_Smoke"}},
                "opacity": {"value": [1, 1, 1, 1], "texture": {"texture": "Fx.T_Smoke", "channels": "a"}}},
            "Fx.M_Gone": {"status": "approximated", "alpha_mode": "blend", "unlit": true,
                "base_color": {"source": "default", "value": [0, 0, 0, 1]},
                "emissive": {"value": [1, 1, 1, 1], "texture": {"texture": "Fx.T_Smoke"}},
                "opacity": {"value": [1, 1, 1, 1], "texture": {"texture": "Fx.T_NotConverted", "channels": "g"}}}
        }});
        let m =
            ParticleMaterials::from_json(Path::new("m.json"), &serde_json::to_vec(&file).unwrap())
                .unwrap();
        let mist = m.get("Fx.M_Mist", Some(&textures));
        assert_eq!(
            mist.texture.as_ref().map(|t| t.file.as_str()),
            Some("Fx/T_Smoke.dds")
        );
        assert_eq!(
            mist.opacity_texture.as_ref().map(|t| t.file.as_str()),
            Some("Fx/T_Mask.dds")
        );
        assert_eq!(
            mist.opacity_texture.as_ref().map(|t| t.srgb),
            Some(false),
            "masks sample linearly"
        );
        assert_eq!(
            (mist.opacity_channels, mist.opacity_bias, mist.opacity),
            ([1.0, 0.0, 0.0, 0.0], 0.0, 0.25)
        );
        let dust = m.get("Fx.M_Dust", Some(&textures));
        assert_eq!(
            dust.opacity_channels,
            [0.0, 0.0, 0.0, 1.0],
            "alpha of the displayed texture"
        );
        let gone = m.get("Fx.M_Gone", Some(&textures));
        assert_eq!(
            (gone.opacity_texture.is_none(), gone.opacity_bias),
            (true, 1.0),
            "an unconverted mask is left out"
        );
    }

    #[test]
    fn particle_materials_keep_hdr_strength_and_pick_the_shown_channel() {
        let file = json!({"format": "asamu-materials", "version": 1, "materials": {
            "Fx.M_Snow": {"status": "approximated", "alpha_mode": "add", "unlit": true,
                "base_color": {"source": "default", "value": [0, 0, 0, 1]},
                "emissive": {"source": "expression", "value": [10, 10, 10, 1], "texture": {"texture": "Fx.T_Snow"}},
                "opacity": {"source": "expression", "value": [0.4, 0.4, 0.4, 0.4], "texture": {"texture": "Fx.T_Snow"}}},
            "Fx.M_LitDust": {"status": "approximated", "alpha_mode": "blend", "unlit": false,
                "base_color": {"source": "default", "value": [0, 0, 0, 1]},
                "emissive": {"source": "expression", "value": [1, 0.5, 0.25, 1], "texture": {"texture": "Fx.T_Dust"}}},
            "Fx.M_Rock": {"status": "approximated", "alpha_mode": "opaque", "unlit": false,
                "base_color": {"source": "expression", "value": [0.5, 0.5, 0.5, 1], "texture": {"texture": "Fx.T_Rock"}},
                "emissive": {"source": "constant", "value": [900, 0, 0, 1]}},
            "Fx.M_Broken": {"status": "fallback", "alpha_mode": "add", "base_color": {"value": [1, 1, 1, 1]}}
        }});
        let m =
            ParticleMaterials::from_json(Path::new("m.json"), &serde_json::to_vec(&file).unwrap())
                .unwrap();
        let snow = m.get("fx.m_snow", None);
        assert_eq!(snow.blend, crate::BlendMode::Additive);
        assert_eq!(snow.color, [10.0; 3], "HDR emissive strength kept");
        assert_eq!(snow.opacity, 0.4);
        assert!(
            snow.converted && snow.texture.is_none(),
            "no texture manifest: unbound"
        );
        let dust = m.get("Fx.M_LitDust", None);
        assert_eq!(dust.blend, crate::BlendMode::Translucent);
        assert_eq!(
            dust.color,
            [1.0, 0.5, 0.25],
            "lit material with only emissive connected"
        );
        assert_eq!(dust.opacity, 1.0);
        let rock = m.get("Fx.M_Rock", None);
        assert_eq!(rock.color, [0.5; 3], "a textured diffuse input is shown");
        assert_eq!(rock.blend, crate::BlendMode::Opaque);
        assert!(!m.get("Fx.M_Broken", None).converted);
        assert!(!m.get("Fx.Missing", None).converted);
        assert!(ParticleMaterials::from_json(Path::new("m.json"), b"[1]").is_err());
    }

    /// Real data (skips without `ASAMU_CONVERTED_DIR` holding both the
    /// Kismet graphs and the particle placements): every `Emitter` actor a
    /// level script references is a placement under the same path, so the
    /// app's Kismet hook finds it.
    #[test]
    fn kismet_emitters_are_placements() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let Ok(rd) = std::fs::read_dir(root.join("kismet")) else {
            eprintln!("SKIP: no kismet/ under ASAMU_CONVERTED_DIR");
            return;
        };
        let mut referenced = 0usize;
        let mut maps = 0usize;
        for f in rd.flatten().map(|e| e.path()) {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let Some(map) = name.strip_suffix(".kismet.json") else {
                continue;
            };
            let Ok(Some(placements)) = load_placements(&root, map) else {
                continue;
            };
            maps += 1;
            let graph: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
            for a in graph["actors"].as_array().into_iter().flatten() {
                let class = a["class"].as_str().unwrap_or("");
                if !class.ends_with(".Emitter") {
                    continue;
                }
                let path = a["path"].as_str().unwrap();
                referenced += 1;
                assert!(
                    placements
                        .iter()
                        .any(|p| p.actor_path.eq_ignore_ascii_case(path)),
                    "{map}: {path} is not a placement"
                );
            }
        }
        println!("{referenced} Kismet-referenced emitters in {maps} maps are placements");
        if maps == 0 {
            eprintln!("SKIP: no map with both Kismet and particle placements");
        }
    }

    /// Real data (skips without `ASAMU_CONVERTED_DIR`): every placement of
    /// every map runs for ten seconds at 60 Hz the way the app runs it
    /// (auto-activated ones only, LOD 0); everything stays finite and
    /// within the engine's per-emitter limits, and a second run is
    /// identical.
    #[test]
    fn converted_placements_simulate() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let Ok(Some(lib)) = ParticleLibrary::load(&root) else {
            eprintln!("SKIP: no particles/particles.json under ASAMU_CONVERTED_DIR");
            return;
        };
        let Ok(rd) = std::fs::read_dir(root.join("particles").join("maps")) else {
            eprintln!("SKIP: no particles/maps under ASAMU_CONVERTED_DIR");
            return;
        };
        let mut maps: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        maps.sort();
        let (mut placements, mut auto) = (0usize, 0usize);
        for map in &maps {
            let list = load_placements(&root, map).unwrap().unwrap();
            let run = |seed_base: u64| {
                let mut live = 0usize;
                let mut active = 0usize;
                let mut state = Vec::new();
                for (i, p) in list.iter().enumerate() {
                    let def = lib.get(p.template.as_deref().unwrap()).unwrap();
                    let mut s = SystemInstance::new(
                        def.clone(),
                        p.matrix(),
                        p.params(),
                        seed_base + i as u64,
                    );
                    if p.auto_activate {
                        s.activate();
                        active += 1;
                    }
                    for _ in 0..600 {
                        s.tick(1.0 / 60.0, None);
                    }
                    for (inst, e) in s.emitters.iter().zip(&def.emitters) {
                        let sub_uv = inst.lod(e).map(|l| l.required.sub_uv);
                        let cap = if def.skip_spawn_count_check {
                            MAX_PARTICLES_PER_EMITTER
                        } else if matches!(sub_uv, Some(SubUvMethod::None) | None) {
                            MAX_SPRITE_PARTICLES
                        } else {
                            MAX_SUBUV_PARTICLES
                        };
                        assert!(inst.particles.len() <= cap, "{map}: {}", def.path);
                    }
                    for r in s.render() {
                        for sp in &r.sprites {
                            assert!(sp.position.is_finite(), "{map}: {}", def.path);
                            assert!(sp.size.iter().all(|x| x.is_finite()));
                            assert!(sp.color.iter().all(|x| x.is_finite()));
                            assert!(sp.rotation.is_finite());
                        }
                    }
                    live += s.particle_count();
                    state.push(s.render());
                }
                (active, live, state)
            };
            let (active, live, state) = run(1000);
            println!(
                "{map:20} {:3} placements, {active:3} auto-activated, {live:5} live particles after 10 s",
                list.len()
            );
            assert!(run(1000).2 == state, "{map}: a second run differs");
            placements += list.len();
            auto += active;
        }
        println!("{placements} placements, {auto} auto-activated");
        assert!(placements > 0);
    }

    /// Real data (skips without `ASAMU_CONVERTED_DIR`): every converted
    /// system builds and simulates finitely; constant and curve
    /// distributions stay within the range the engine baked with them.
    #[test]
    fn converted_systems_simulate() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let path = ParticleLibrary::path(&root);
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("SKIP: no particles/particles.json under ASAMU_CONVERTED_DIR");
            return;
        };
        let lib = ParticleLibrary::from_json(&path, &data).unwrap();
        assert!(!lib.systems.is_empty());
        // Range check against the baked headers.
        let raw: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let mut checked = 0usize;
        let mut outside = Vec::new();
        fn walk(v: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
            match v {
                serde_json::Value::Object(m) => {
                    if m.contains_key("dist") && m.contains_key("baked") {
                        out.push(v.clone());
                    }
                    m.values().for_each(|x| walk(x, out));
                }
                serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut dists = Vec::new();
        walk(&raw["systems"], &mut dists);
        for d in &dists {
            let kind = d["value"]["kind"].as_str().unwrap_or("");
            let legacy = d["value"]["curve"]["legacy_method"]
                .as_bool()
                .unwrap_or(false);
            if !matches!(kind, "constant" | "constant_curve") || legacy {
                continue;
            }
            let Some(range) = d["baked"]["range"].as_array() else {
                continue;
            };
            let lo = range[0].as_f64().unwrap() as f32;
            let hi = range[1].as_f64().unwrap() as f32;
            let dist = dist_of(Some(d));
            let vector = d["dist"] == "vector";
            let curve_range = match &dist {
                Dist::Curve { curve, .. } => curve
                    .keys
                    .first()
                    .map(|a| a.t)
                    .zip(curve.keys.last().map(|b| b.t)),
                _ => Some((0.0, 0.0)),
            };
            let Some((t0, t1)) = curve_range else {
                continue;
            };
            let mut rng = Rng(0);
            for i in 0..=20 {
                let t = t0 + (t1 - t0) * i as f32 / 20.0;
                let vals: Vec<f32> = if vector {
                    dist.eval_vec(t, &mut rng, &none()).to_array().to_vec()
                } else {
                    vec![dist.eval_f32(t, &mut rng, &none())]
                };
                let tol = |x: f32| 1e-3 * x.abs().max(1.0);
                // The engine's range is over every component of the sampled
                // values; a cubic between samples may overshoot slightly.
                for v in vals {
                    if v < lo - tol(lo) * 50.0 || v > hi + tol(hi) * 50.0 {
                        outside.push(format!("{v} not in [{lo}, {hi}] at {t}: {}", d["object"]));
                    }
                }
            }
            checked += 1;
        }
        println!(
            "{checked} constant/curve distributions range-checked, {} outside",
            outside.len()
        );
        for o in outside.iter().take(10) {
            println!("  {o}");
        }
        assert!(checked > 1000);
        assert!(
            outside.len() * 100 < checked,
            "{} of {checked} outside",
            outside.len()
        );
        // Simulate every system for 3 s at 60 Hz.
        let mut total = 0usize;
        for (i, def) in lib.systems.values().enumerate() {
            let mut s = SystemInstance::new(
                def.clone(),
                Mat4::IDENTITY,
                InstanceParams::default(),
                i as u64,
            );
            s.activate();
            for _ in 0..180 {
                s.tick(1.0 / 60.0, Some(Vec3::new(500.0, 0.0, 0.0)));
            }
            for r in s.render() {
                for sp in &r.sprites {
                    assert!(sp.position.is_finite(), "{}", def.path);
                    assert!(sp.size.iter().all(|x| x.is_finite()), "{}", def.path);
                    assert!(sp.color.iter().all(|x| x.is_finite()), "{}", def.path);
                }
                total += r.sprites.len();
            }
        }
        println!(
            "{} systems simulated, {total} sprites after 3 s",
            lib.systems.len()
        );
        assert!(total > 0);
    }
}
