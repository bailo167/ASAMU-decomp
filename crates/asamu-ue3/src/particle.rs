//! Particle systems for UE3 v868: `ParticleSystem`, `ParticleEmitter`
//! (sprite emitters with mesh / beam / trail type data), `ParticleLODLevel`,
//! `ParticleModule*`, the `DistributionFloat*` / `DistributionVector*`
//! objects the modules sample, and `ParticleSystemComponent`s.
//!
//! Every particle class carries **only tagged properties** at v868: the
//! native `Serialize` of the distribution classes and of
//! `UParticleSystemComponent` add no bytes on load (read in the executable,
//! and CONFIRMED by exact consumption of every particle export of the
//! shipped packages; see `docs/reverse-engineering/PARTICLES.md`). The
//! decoder therefore refuses any export whose tagged properties do not end
//! exactly at `SerialSize`.
//!
//! Values are the object's own tagged properties merged over its archetype
//! (or, without one, the merged class defaults): tagged structs merge member
//! by member, everything else is replaced, the same delta rule the engine
//! applies when it loads tags over an archetype.
//!
//! A module property of type `RawDistributionFloat` / `RawDistributionVector`
//! holds both a distribution object reference and a baked lookup table.
//! The shipped game evaluates the **object** whenever one is set (the
//! out-of-line `FRawDistribution*::GetValue` and `GetFastRawDistribution`
//! take the lookup path only without an object, or outside the game; read in
//! the executable). [`RawDistribution`] keeps the decoded object as
//! [`Distribution`] and the table only when no object is set.
//!
//! Hostile input: archetype chains, curve keys, module lists and lookup
//! tables are bounded, unknown classes and missing objects are reported, and
//! nothing here panics. A system may list the same emitter, LOD level or
//! module many times, so the size of a decoded system is not bounded by the
//! size of its package; every system is therefore decoded against a byte
//! budget ([`MAX_SYSTEM_BYTES`] of module and distribution payload, counted
//! once per use) and stops listing modules when it is spent.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::flags;
use crate::material::merge_properties;
use crate::matinee::{CurveMode, CurveValue, InterpCurve, InterpMethod, decode_curve};
use crate::model::{LoadedPackage, PackageSet};
use crate::object::{ObjectError, qualified_path};
use crate::package::Package;
use crate::property::{self, ObjRef, Property, Value, ValueContext};
use crate::reader::Reader;
use crate::schema::{NoSchema, last_component};
use crate::types::PackageIndex;

/// `format` of the importer's particle JSON.
pub const PARTICLES_FORMAT: &str = "asamu-particles";
/// `version` of the importer's particle JSON.
pub const PARTICLES_VERSION: u32 = 1;
/// Longest archetype chain followed.
pub const MAX_ARCHETYPE_DEPTH: usize = 16;
/// Most emitters read from one system.
pub const MAX_EMITTERS: usize = 256;
/// Most LOD levels read from one emitter.
pub const MAX_LOD_LEVELS: usize = 16;
/// Most modules read from one LOD level.
pub const MAX_MODULES: usize = 256;
/// Most keys read from one curve.
pub const MAX_CURVE_KEYS: usize = 4096;
/// Most lookup table entries kept.
pub const MAX_LOOKUP_TABLE: usize = 1 << 16;
/// Deepest nesting followed when converting values.
pub const MAX_PARAM_DEPTH: usize = 16;
/// Most notes kept per decoder.
pub const MAX_NOTES: usize = 512;
/// Payload bytes of emitters, LOD levels, modules and distribution objects
/// one system may decode, each counted every time it is used (the shipped
/// systems total 2.5 MB over all 110; the largest single one is far below
/// 1 MB).
pub const MAX_SYSTEM_BYTES: usize = 32 << 20;
/// Fixed cost added per decoded object against [`MAX_SYSTEM_BYTES`] (the
/// decoded form of an empty object is not free).
const OBJECT_COST: usize = 256;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from particle decoding.
#[derive(Debug, Error)]
pub enum ParticleError {
    /// Prelude or tagged properties failed to decode.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A low-level read failed.
    #[error(transparent)]
    Ue3(#[from] crate::error::Ue3Error),
    /// The export is not a particle object.
    #[error("export {export} ({class}) is not a particle object")]
    NotParticle {
        /// Export index.
        export: usize,
        /// Class path.
        class: String,
    },
    /// Bytes follow the tagged properties of a class that has no native data.
    #[error("{path} ({class}): tagged properties end at {end} of {size} payload bytes")]
    NativeTail {
        /// Object path.
        path: String,
        /// Class path.
        class: String,
        /// Payload offset after the tags.
        end: usize,
        /// `SerialSize`.
        size: usize,
    },
    /// The data is inconsistent.
    #[error("{path}: {detail}")]
    Malformed {
        /// Object path.
        path: String,
        /// What is wrong.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

/// Role of a particle-related class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticleRole {
    /// `Engine.ParticleSystem`.
    System,
    /// `Engine.ParticleEmitter` and subclasses.
    Emitter,
    /// `Engine.ParticleLODLevel`.
    LodLevel,
    /// `Engine.ParticleModule` and subclasses.
    Module,
    /// `DistributionFloat` and subclasses.
    FloatDistribution,
    /// `DistributionVector` and subclasses.
    VectorDistribution,
    /// `Engine.ParticleSystemComponent` and subclasses.
    Component,
}

impl ParticleRole {
    /// Role of the class at `class_path` with super chain `super_chain`
    /// (qualified paths, nearest super first). Without a chain (a package
    /// set that lacks the script packages) the class name alone decides.
    pub fn classify(class_path: &str, super_chain: &[String]) -> Option<ParticleRole> {
        let own = last_component(class_path).to_ascii_lowercase();
        let names: Vec<String> = std::iter::once(own.clone())
            .chain(
                super_chain
                    .iter()
                    .map(|c| last_component(c).to_ascii_lowercase()),
            )
            .collect();
        let has = |n: &str| names.iter().any(|x| x == n);
        if has("particlesystemcomponent") {
            Some(ParticleRole::Component)
        } else if has("particlesystem") {
            Some(ParticleRole::System)
        } else if has("particleemitter") {
            Some(ParticleRole::Emitter)
        } else if has("particlelodlevel") {
            Some(ParticleRole::LodLevel)
        } else if has("particlemodule") {
            Some(ParticleRole::Module)
        } else if has("distributionfloat") {
            Some(ParticleRole::FloatDistribution)
        } else if has("distributionvector") {
            Some(ParticleRole::VectorDistribution)
        } else if super_chain.is_empty() {
            // Name-based fallback (synthetic or partial package sets).
            if own.starts_with("particlemodule") {
                Some(ParticleRole::Module)
            } else if own.starts_with("particle") && own.ends_with("emitter") {
                Some(ParticleRole::Emitter)
            } else if own.starts_with("distributionfloat") {
                Some(ParticleRole::FloatDistribution)
            } else if own.starts_with("distributionvector") {
                Some(ParticleRole::VectorDistribution)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Name used in reports.
    pub fn name(self) -> &'static str {
        match self {
            ParticleRole::System => "system",
            ParticleRole::Emitter => "emitter",
            ParticleRole::LodLevel => "lod_level",
            ParticleRole::Module => "module",
            ParticleRole::FloatDistribution => "float_distribution",
            ParticleRole::VectorDistribution => "vector_distribution",
            ParticleRole::Component => "component",
        }
    }
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One key of a distribution curve (`InterpCurvePoint*`), any dimension.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurveKey {
    /// `InVal`.
    pub t: f32,
    /// `OutVal` components.
    pub v: Vec<f32>,
    /// `ArriveTangent` components.
    pub arrive: Vec<f32>,
    /// `LeaveTangent` components.
    pub leave: Vec<f32>,
    /// `InterpMode` (`linear`, `curve_auto`, `constant`, `curve_user`,
    /// `curve_break`, `curve_auto_clamped`).
    pub mode: &'static str,
}

/// A distribution curve (`InterpCurveFloat`, `InterpCurveVector2D`,
/// `InterpCurveVector`, `InterpCurveTwoVectors`).
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Curve {
    /// Components per value (1, 2, 3 or 6).
    pub dim: usize,
    /// Keys in stored order.
    pub keys: Vec<CurveKey>,
    /// `IMT_UseBrokenTangentEval` (tangents not scaled by the segment span).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub broken_tangents: bool,
    /// `InterpMethod` was stored as a name that is not an enumerator
    /// (`None`; stock UT content saved by an older engine). It evaluates as
    /// the default method (tangents scaled by the span).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy_method: bool,
}

/// What the runtime evaluates for a raw distribution.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Distribution {
    /// `DistributionFloatConstant` / `DistributionVectorConstant`.
    Constant {
        /// `Constant`.
        value: Vec<f32>,
        /// `LockedAxes` (`EDistributionVectorLockFlags` index; vectors only).
        locked_axes: u8,
    },
    /// `DistributionFloatUniform` / `DistributionVectorUniform`.
    Uniform {
        /// `Min`.
        min: Vec<f32>,
        /// `Max`.
        max: Vec<f32>,
        /// `LockedAxes` (vectors).
        locked_axes: u8,
        /// `MirrorFlags[3]` (`EDistributionVectorMirrorFlags` indices; vectors).
        mirror: [u8; 3],
        /// `bUseExtremes` (vectors).
        use_extremes: bool,
    },
    /// `DistributionFloatConstantCurve` / `DistributionVectorConstantCurve`.
    ConstantCurve {
        /// `ConstantCurve`.
        curve: Curve,
        /// `LockedAxes` (vectors).
        locked_axes: u8,
    },
    /// `DistributionFloatUniformCurve` (curve of `[X, Y]`) /
    /// `DistributionVectorUniformCurve` (curve of `[v1.xyz, v2.xyz]`).
    UniformCurve {
        /// `ConstantCurve`.
        curve: Curve,
        /// `LockedAxes[2]` (vectors).
        locked_axes: [u8; 2],
        /// `MirrorFlags[3]` (vectors).
        mirror: [u8; 3],
        /// `bUseExtremes` (vectors).
        use_extremes: bool,
    },
    /// `Distribution*ParameterBase` subclasses (particle or sound
    /// parameters, read from the component at run time).
    Parameter {
        /// `ParameterName`.
        name: String,
        /// `ParamMode` (float) / `ParamModes[3]` (vector): `DPM_Normal`,
        /// `DPM_Abs`, `DPM_Direct` indices.
        modes: Vec<u8>,
        /// `MinInput`.
        min_input: Vec<f32>,
        /// `MaxInput`.
        max_input: Vec<f32>,
        /// `MinOutput`.
        min_output: Vec<f32>,
        /// `MaxOutput`.
        max_output: Vec<f32>,
        /// `Constant` (used when the component has no such parameter).
        constant: Vec<f32>,
        /// Class name (`DistributionFloatParticleParameter`, ...).
        class: String,
    },
    /// No distribution object: the baked `FRawDistribution` lookup table.
    Lookup {
        /// `Op` (`ERawDistributionOperation`: 0 uninitialized, 1 none,
        /// 2 random, 3 extreme, 4 random range).
        op: u8,
        /// `LookupTableNumElements`.
        elements: u8,
        /// `LookupTableChunkSize`.
        chunk: u8,
        /// `LookupTable` (2 range values, then the entries).
        table: Vec<f32>,
        /// `LookupTableTimeScale`.
        time_scale: f32,
        /// `LookupTableStartTime`.
        start_time: f32,
    },
    /// A distribution class this decoder does not model.
    Unsupported {
        /// Class path.
        class: String,
    },
}

/// Summary of a baked lookup table.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Baked {
    /// `Op`.
    pub op: u8,
    /// `LookupTableNumElements`.
    pub elements: u8,
    /// `LookupTableChunkSize`.
    pub chunk: u8,
    /// `LookupTable` length.
    pub len: usize,
    /// `LookupTableTimeScale`.
    pub time_scale: f32,
    /// `LookupTableStartTime`.
    pub start_time: f32,
    /// The table's first two values: the output range `[min, max]` the
    /// engine stored with the samples.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f32; 2]>,
}

/// A `RawDistributionFloat` / `RawDistributionVector` module property.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RawDistribution {
    /// `"float"` or `"vector"`.
    pub dist: &'static str,
    /// Distribution object path (absent: none set, the table is used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// Distribution object class name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// What the runtime evaluates.
    pub value: Distribution,
    /// Baked table summary.
    pub baked: Baked,
    /// The baked table (kept for checks; not serialized).
    #[serde(skip)]
    pub table: Vec<f32>,
}

/// A converted property value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Param {
    /// Bool.
    Bool(bool),
    /// Int or byte.
    Int(i64),
    /// Float.
    Float(f32),
    /// Name, enumerator, string or object path.
    Text(String),
    /// A raw distribution.
    Distribution(Box<RawDistribution>),
    /// Array or static array.
    List(Vec<Param>),
    /// Struct members by name.
    Struct(BTreeMap<String, Param>),
    /// Null object, or a value that could not be decoded.
    Null,
}

impl Param {
    /// Float view (ints and bytes convert).
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Param::Float(f) => Some(*f),
            Param::Int(i) => Some(*i as f32),
            _ => None,
        }
    }

    /// Bool view.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Param::Bool(b) => Some(*b),
            Param::Int(i) => Some(*i != 0),
            _ => None,
        }
    }

    /// Text view.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Param::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Distribution view.
    pub fn as_distribution(&self) -> Option<&RawDistribution> {
        match self {
            Param::Distribution(d) => Some(d),
            _ => None,
        }
    }
}

/// A particle module.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Module {
    /// Object path.
    pub path: String,
    /// Class name (`ParticleModuleSize`, ...).
    pub class: String,
    /// `bEnabled`.
    pub enabled: bool,
    /// `bSpawnModule`.
    pub spawn: bool,
    /// `bUpdateModule`.
    pub update: bool,
    /// `LODValidity` bit mask.
    pub lod_validity: u8,
    /// Every other effective property (editor-only ones left out).
    pub params: BTreeMap<String, Param>,
}

impl Module {
    /// Parameter `name` (case-insensitive).
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }
}

/// One LOD level of an emitter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LodLevel {
    /// Object path.
    pub path: String,
    /// `Level`.
    pub level: i32,
    /// `bEnabled`.
    pub enabled: bool,
    /// `PeakActiveParticles`.
    pub peak_active_particles: i32,
    /// `RequiredModule`.
    pub required: Option<Module>,
    /// `SpawnModule`.
    pub spawn: Option<Module>,
    /// `TypeDataModule`.
    pub type_data: Option<Module>,
    /// `EventGenerator`.
    pub event_generator: Option<Module>,
    /// `Modules` in order.
    pub modules: Vec<Module>,
}

/// What an emitter renders, from its type data module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterKind {
    /// Camera-facing sprites (no type data).
    Sprite,
    /// `ParticleModuleTypeDataMesh`.
    Mesh,
    /// `ParticleModuleTypeDataBeam2` / `Beam`.
    Beam,
    /// `ParticleModuleTypeDataTrail2` / `Trail`.
    Trail,
    /// `ParticleModuleTypeDataAnimTrail`.
    AnimTrail,
    /// `ParticleModuleTypeDataRibbon`.
    Ribbon,
    /// Any other type data (PhysX, Apex, ...).
    Other,
}

impl EmitterKind {
    /// Kind of a type data module class (`None` = sprites).
    pub fn of_type_data(class: Option<&str>) -> EmitterKind {
        let Some(c) = class else {
            return EmitterKind::Sprite;
        };
        let c = c.to_ascii_lowercase();
        if c.contains("typedatameshphysx") || c.contains("physx") || c.contains("apex") {
            EmitterKind::Other
        } else if c.contains("typedatamesh") {
            EmitterKind::Mesh
        } else if c.contains("typedatabeam") {
            EmitterKind::Beam
        } else if c.contains("typedataanimtrail") {
            EmitterKind::AnimTrail
        } else if c.contains("typedatatrail") {
            EmitterKind::Trail
        } else if c.contains("typedataribbon") {
            EmitterKind::Ribbon
        } else {
            EmitterKind::Other
        }
    }
}

/// A particle emitter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Emitter {
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// `EmitterName` (the object name when not stored).
    pub name: String,
    /// What it renders (from LOD 0's type data).
    pub kind: EmitterKind,
    /// Other effective properties (editor-only ones left out).
    pub params: BTreeMap<String, Param>,
    /// `LODLevels`.
    pub lods: Vec<LodLevel>,
}

/// A particle system.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParticleSystem {
    /// Object path (the key components use in `Template`).
    pub path: String,
    /// Package file stem it was read from.
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Effective properties (editor-only ones left out).
    pub params: BTreeMap<String, Param>,
    /// `Emitters` in order (null entries skipped).
    pub emitters: Vec<Emitter>,
    /// Null or undecodable emitter slots.
    pub skipped_emitters: usize,
    /// Decoding notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// One `InstanceParameters` entry of a component (`ParticleSysParam`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstanceParameter {
    /// `Name`.
    pub name: String,
    /// `ParamType` (`PSPT_*`).
    pub param_type: String,
    /// `Scalar`.
    pub scalar: f32,
    /// `Scalar_Low`.
    pub scalar_low: f32,
    /// `Vector`.
    pub vector: [f32; 3],
    /// `Vector_Low`.
    pub vector_low: [f32; 3],
    /// `Color` (B, G, R, A bytes as stored).
    pub color: [u8; 4],
    /// `Actor`.
    pub actor: Option<String>,
    /// `Material`.
    pub material: Option<String>,
}

/// The particle-relevant values of a `ParticleSystemComponent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComponentInfo {
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// `Template`.
    pub template: Option<String>,
    /// `bAutoActivate`.
    pub auto_activate: bool,
    /// A tagged `bKillOnDeactivate`. `Engine.ParticleSystemComponent` has
    /// no such property at v868 (the flag is per emitter, on the required
    /// module), so this is `false` for every shipped component.
    pub kill_on_deactivate: bool,
    /// A tagged `bKillOnCompleted` (see `kill_on_deactivate`).
    pub kill_on_completed: bool,
    /// `bResetOnDetach`.
    pub reset_on_detach: bool,
    /// `HiddenGame`.
    pub hidden_game: bool,
    /// `WarmupTime` as stored. The engine overwrites the component's value
    /// with the template's every time it initializes the system, so the
    /// stored one has no effect in the game.
    pub warmup_time: f32,
    /// `SecondsBeforeInactive`.
    pub seconds_before_inactive: f32,
    /// `EmitterDelay`.
    pub emitter_delay: f32,
    /// `InstanceParameters`.
    pub instance_parameters: Vec<InstanceParameter>,
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// `EDistributionVectorLockFlags` in declaration order.
pub const LOCK_FLAG_NAMES: &[&str] = &[
    "EDVLF_None",
    "EDVLF_XY",
    "EDVLF_XZ",
    "EDVLF_YZ",
    "EDVLF_XYZ",
];
/// `EDistributionVectorMirrorFlags` in declaration order.
pub const MIRROR_FLAG_NAMES: &[&str] = &["EDVMF_Same", "EDVMF_Different", "EDVMF_Mirror"];
/// `EDistributionParamMode` in declaration order.
pub const PARAM_MODE_NAMES: &[&str] = &["DPM_Normal", "DPM_Abs", "DPM_Direct"];

/// Editor-only properties left out of the exported parameter maps.
const EDITOR_ONLY: &[&str] = &[
    "ModuleEditorColor",
    "bEditable",
    "LODDuplicate",
    "b3DDrawMode",
    "bSupported3DDrawMode",
    "bCurvesAsColor",
    "EmitterEditorColor",
    "bCollapsed",
    "bIsSoloing",
    "ThumbnailImage",
    "ThumbnailImageOutOfDate",
    "ThumbnailAngle",
    "ThumbnailDistance",
    "ThumbnailWarmup",
    "CurveEdSetup",
    "PreviewLightRadius",
    "bShouldResetPeakCounts",
    "bHasPhysics",
    "bIsDirty",
    "bCanBeBaked",
    "FloorMesh",
    "FloorPosition",
    "FloorRotation",
    "FloorScale",
    "FloorScale3D",
    "BackgroundColor",
];

/// Properties of modules, LOD levels and emitters that the typed fields
/// already carry.
const TYPED_FIELDS: &[&str] = &[
    "bEnabled",
    "bSpawnModule",
    "bUpdateModule",
    "LODValidity",
    "LODLevels",
    "Emitters",
];

fn excluded(name: &str) -> bool {
    EDITOR_ONLY
        .iter()
        .chain(TYPED_FIELDS)
        .any(|e| e.eq_ignore_ascii_case(name))
}

/// Value of property `name` at static array index `index`
/// (case-insensitive).
pub fn prop_at<'p>(props: &'p [Property], name: &str, index: i32) -> Option<&'p Value> {
    props
        .iter()
        .find(|p| p.array_index == index && p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

/// Value of property `name` (array index 0).
pub fn prop<'p>(props: &'p [Property], name: &str) -> Option<&'p Value> {
    prop_at(props, name, 0)
}

fn fields(v: &Value) -> Option<&[Property]> {
    match v {
        Value::Struct { fields, .. } => Some(fields),
        _ => None,
    }
}

/// Float view of a value.
pub fn value_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f32),
        Value::Byte(b) => Some(f32::from(*b)),
        _ => None,
    }
}

fn value_i32(v: &Value) -> Option<i32> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(i32::from(*b)),
        _ => None,
    }
}

fn value_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Byte(b) => Some(*b != 0),
        Value::Int(i) => Some(*i != 0),
        _ => None,
    }
}

/// Object path of a non-null object value.
pub fn value_object(v: &Value) -> Option<&str> {
    match v {
        Value::Object(o) | Value::Interface(o) if o.index != 0 => Some(o.path.as_str()),
        _ => None,
    }
}

fn value_text(v: &Value) -> Option<&str> {
    match v {
        Value::Name(n) | Value::Enum(n) | Value::Str(n) => Some(n.as_str()),
        _ => None,
    }
}

/// Enumerator index of a stored enum value (`Enum(name)` or a plain byte);
/// `None` when the name is unknown.
pub fn enum_index(v: Option<&Value>, names: &[&str]) -> Option<u8> {
    match v {
        None => Some(0),
        Some(Value::Byte(b)) => Some(*b),
        Some(Value::Int(i)) => u8::try_from(*i).ok(),
        Some(Value::Enum(n) | Value::Name(n)) => names
            .iter()
            .position(|x| x.eq_ignore_ascii_case(n))
            .and_then(|i| u8::try_from(i).ok()),
        Some(_) => None,
    }
}

fn get_f32(props: &[Property], name: &str) -> Option<f32> {
    prop(props, name).and_then(value_f32)
}

fn get_i32(props: &[Property], name: &str) -> Option<i32> {
    prop(props, name).and_then(value_i32)
}

fn get_bool(props: &[Property], name: &str) -> Option<bool> {
    prop(props, name).and_then(value_bool)
}

fn get_object<'p>(props: &'p [Property], name: &str) -> Option<&'p str> {
    prop(props, name).and_then(value_object)
}

fn get_text<'p>(props: &'p [Property], name: &str) -> Option<&'p str> {
    prop(props, name).and_then(value_text)
}

/// `Vector` struct value as `[X, Y, Z]` (missing members 0).
pub fn value_vec3(v: &Value) -> Option<[f32; 3]> {
    let f = fields(v)?;
    let c = |n: &str| get_f32(f, n).unwrap_or(0.0);
    Some([c("X"), c("Y"), c("Z")])
}

fn get_vec3(props: &[Property], name: &str) -> Option<[f32; 3]> {
    prop(props, name).and_then(value_vec3)
}

fn value_color_bytes(v: &Value) -> Option<[u8; 4]> {
    let f = fields(v)?;
    let b = |n: &str| match prop(f, n) {
        Some(Value::Byte(x)) => *x,
        Some(Value::Int(x)) => u8::try_from(*x).unwrap_or(0),
        _ => 0,
    };
    Some([b("B"), b("G"), b("R"), b("A")])
}

fn items(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

fn curve_mode_name(m: CurveMode) -> &'static str {
    match m {
        CurveMode::Linear => "linear",
        CurveMode::CurveAuto => "curve_auto",
        CurveMode::Constant => "constant",
        CurveMode::CurveUser => "curve_user",
        CurveMode::CurveBreak => "curve_break",
        CurveMode::CurveAutoClamped => "curve_auto_clamped",
    }
}

/// `TwoVectors` curve value (`v1.xyz`, `v2.xyz`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoVectors(pub [f32; 6]);

impl CurveValue for TwoVectors {
    const DIM: usize = 6;
    fn zero() -> Self {
        TwoVectors([0.0; 6])
    }
    fn get(&self, i: usize) -> f32 {
        self.0.get(i).copied().unwrap_or(0.0)
    }
    fn set(&mut self, i: usize, v: f32) {
        if let Some(c) = self.0.get_mut(i) {
            *c = v;
        }
    }
}

fn conv_f32(v: &Value) -> Option<f32> {
    value_f32(v)
}

fn conv_vec2(v: &Value) -> Option<[f32; 2]> {
    let f = fields(v)?;
    Some([
        get_f32(f, "X").unwrap_or(0.0),
        get_f32(f, "Y").unwrap_or(0.0),
    ])
}

fn conv_vec3(v: &Value) -> Option<[f32; 3]> {
    value_vec3(v)
}

fn conv_two_vectors(v: &Value) -> Option<TwoVectors> {
    let f = fields(v)?;
    let a = get_vec3(f, "v1").unwrap_or([0.0; 3]);
    let b = get_vec3(f, "v2").unwrap_or([0.0; 3]);
    Some(TwoVectors([a[0], a[1], a[2], b[0], b[1], b[2]]))
}

fn to_curve<T: CurveValue>(c: &InterpCurve<T>) -> Curve {
    let comps = |x: &T| (0..T::DIM).map(|i| x.get(i)).collect::<Vec<f32>>();
    Curve {
        dim: T::DIM,
        keys: c
            .points
            .iter()
            .take(MAX_CURVE_KEYS)
            .map(|p| CurveKey {
                t: p.in_val,
                v: comps(&p.out_val),
                arrive: comps(&p.arrive_tangent),
                leave: comps(&p.leave_tangent),
                mode: curve_mode_name(p.mode),
            })
            .collect(),
        broken_tangents: c.method == InterpMethod::BrokenTangentEval,
        legacy_method: false,
    }
}

/// Decode an `InterpCurve*` value of `dim` components (1, 2, 3 or 6).
///
/// An `InterpMethod` stored as the name `None` (seen in stock UT content)
/// is not an enumerator; it reads as the enum's first value, the default,
/// without a note.
pub fn decode_distribution_curve(v: Option<&Value>, dim: usize, notes: &mut Vec<String>) -> Curve {
    let mut local = Vec::new();
    let mut c = match dim {
        1 => to_curve(&decode_curve(v, conv_f32, &mut local, "curve")),
        2 => to_curve(&decode_curve(v, conv_vec2, &mut local, "curve")),
        3 => to_curve(&decode_curve(v, conv_vec3, &mut local, "curve")),
        _ => to_curve(&decode_curve(v, conv_two_vectors, &mut local, "curve")),
    };
    let legacy = "unknown InterpMethod Enum(\"None\")";
    c.legacy_method = local.iter().any(|n| n.contains(legacy));
    for n in local.into_iter().filter(|n| !n.contains(legacy)) {
        push_note(notes, n);
    }
    c
}

/// The curve back as a matinee curve of `T` (for evaluation in checks).
pub fn curve_as<T: CurveValue>(c: &Curve) -> InterpCurve<T> {
    let val = |v: &[f32]| {
        let mut x = T::zero();
        for (i, c) in v.iter().enumerate() {
            x.set(i, *c);
        }
        x
    };
    let mode = |m: &str| match m {
        "curve_auto" => CurveMode::CurveAuto,
        "constant" => CurveMode::Constant,
        "curve_user" => CurveMode::CurveUser,
        "curve_break" => CurveMode::CurveBreak,
        "curve_auto_clamped" => CurveMode::CurveAutoClamped,
        _ => CurveMode::Linear,
    };
    InterpCurve {
        points: c
            .keys
            .iter()
            .map(|k| crate::matinee::CurvePoint {
                in_val: k.t,
                out_val: val(&k.v),
                arrive_tangent: val(&k.arrive),
                leave_tangent: val(&k.leave),
                mode: mode(k.mode),
            })
            .collect(),
        method: if c.broken_tangents {
            InterpMethod::BrokenTangentEval
        } else {
            InterpMethod::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Raw arrays without a schema
// ---------------------------------------------------------------------------

/// Array properties whose elements are floats.
const FLOAT_ARRAYS: &[&str] = &["LookupTable", "LODDistances"];
/// Array properties whose elements are object references.
const OBJECT_ARRAYS: &[&str] = &[
    "Emitters",
    "LODLevels",
    "Modules",
    "SpawnModules",
    "SpawningModules",
    "UpdateModules",
    "ConvertedModules",
];

/// Tag header bytes before an `ArrayProperty` value: name, type, size,
/// array index.
const ARRAY_TAG_HEADER: usize = 24;

/// Re-read arrays whose element type was unknown at decode time (a package
/// set without the script classes): float and object arrays by name, other
/// arrays as back-to-back tagged structs when that consumes them exactly.
/// Offsets of nested tagged struct members are payload offsets, so nested
/// arrays are recovered too.
fn recover_arrays(
    props: &mut [Property],
    payload: &[u8],
    pkg: &Package,
    own_name: Option<&str>,
    depth: usize,
) {
    if depth > MAX_PARAM_DEPTH {
        return;
    }
    for p in props.iter_mut() {
        match &mut p.value {
            Value::RawArray { count, bytes } => {
                let (count, bytes) = (*count, *bytes);
                if let Some(v) =
                    recover_one(&p.name, p.offset, count, bytes, payload, pkg, own_name)
                {
                    p.value = v;
                }
            }
            Value::Struct {
                binary: false,
                fields,
                ..
            } => recover_arrays(fields, payload, pkg, own_name, depth + 1),
            Value::Array(list) => {
                for item in list.iter_mut() {
                    if let Value::Struct {
                        binary: false,
                        fields,
                        ..
                    } = item
                    {
                        recover_arrays(fields, payload, pkg, own_name, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

fn recover_one(
    name: &str,
    tag_offset: usize,
    count: usize,
    bytes: usize,
    payload: &[u8],
    pkg: &Package,
    own_name: Option<&str>,
) -> Option<Value> {
    let start = tag_offset.checked_add(ARRAY_TAG_HEADER)?.checked_add(4)?;
    let end = start.checked_add(bytes)?;
    let data = payload.get(start..end)?;
    let is = |list: &[&str]| list.iter().any(|n| n.eq_ignore_ascii_case(name));
    if is(FLOAT_ARRAYS) && count.checked_mul(4) == Some(bytes) {
        let mut r = Reader::new(data);
        let mut out = Vec::with_capacity(count.min(property::MAX_PREALLOC));
        for _ in 0..count {
            out.push(Value::Float(r.read_f32().ok()?));
        }
        return Some(Value::Array(out));
    }
    if is(OBJECT_ARRAYS) && count.checked_mul(4) == Some(bytes) {
        let mut r = Reader::new(data);
        let mut out = Vec::with_capacity(count.min(property::MAX_PREALLOC));
        for _ in 0..count {
            let idx = r.read_package_index().ok()?;
            let path = if idx.is_null() {
                String::new()
            } else {
                qualified_path(pkg, own_name, idx).ok()?
            };
            out.push(Value::Object(ObjRef { index: idx.0, path }));
        }
        return Some(Value::Array(out));
    }
    // Back-to-back tagged structs (curve points, bursts, parameters).
    let schema = NoSchema;
    let mut ctx = ValueContext::new(pkg, own_name, &schema);
    ctx.set_work_budget(property::work_budget_for(bytes));
    let mut r = Reader::at(&payload[..end], start).ok()?;
    let mut out = Vec::with_capacity(count.min(property::MAX_PREALLOC));
    for _ in 0..count {
        let mut fields = property::read_tagged(&mut r, &mut ctx, None, 1).ok()?;
        recover_arrays(&mut fields, payload, pkg, own_name, 1);
        out.push(Value::Struct {
            name: String::new(),
            binary: false,
            fields,
        });
    }
    (r.position() == end).then_some(Value::Array(out))
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// A particle export with its effective values.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Package file stem.
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Qualified class path.
    pub class_path: String,
    /// Role.
    pub role: ParticleRole,
    /// `RF_ClassDefaultObject` is set.
    pub is_default_object: bool,
    /// Archetype path (null: the class defaults).
    pub archetype: Option<String>,
    /// Tagged properties as stored.
    pub own: Vec<Property>,
    /// Effective values (own over archetype or class defaults).
    pub props: Arc<Vec<Property>>,
    /// `SerialSize` (the tags consume it exactly).
    pub payload_size: usize,
    /// Decoding notes of the tagged properties.
    pub warnings: Vec<String>,
}

impl Resolved {
    /// Class name (last path component).
    pub fn class_name(&self) -> &str {
        last_component(&self.class_path)
    }

    /// Object name (last path component).
    pub fn object_name(&self) -> &str {
        last_component(&self.path)
    }
}

type Key = (String, usize);

/// Decodes particle exports of a [`PackageSet`], caching class information
/// and effective values.
pub struct ParticleDecoder<'a> {
    set: &'a PackageSet,
    roles: RefCell<HashMap<String, Option<ParticleRole>>>,
    defaults: RefCell<HashMap<String, Arc<Vec<Property>>>>,
    resolved: RefCell<HashMap<Key, Arc<Resolved>>>,
    failed: RefCell<HashMap<Key, String>>,
    notes: RefCell<Vec<String>>,
    /// What is left of [`MAX_SYSTEM_BYTES`] for the system being decoded.
    budget: Cell<usize>,
}

impl<'a> ParticleDecoder<'a> {
    /// Decoder over `set`.
    pub fn new(set: &'a PackageSet) -> ParticleDecoder<'a> {
        ParticleDecoder {
            set,
            roles: RefCell::default(),
            defaults: RefCell::default(),
            resolved: RefCell::default(),
            failed: RefCell::default(),
            notes: RefCell::default(),
            budget: Cell::new(MAX_SYSTEM_BYTES),
        }
    }

    /// Take the decoded size of `r` from the current system's budget.
    fn charge(&self, r: &Resolved) -> Result<(), ParticleError> {
        let cost = r.payload_size.saturating_add(OBJECT_COST);
        match self.budget.get().checked_sub(cost) {
            Some(left) => {
                self.budget.set(left);
                Ok(())
            }
            None => {
                self.budget.set(0);
                Err(ParticleError::Malformed {
                    path: r.path.clone(),
                    detail: format!(
                        "the system's decode budget of {MAX_SYSTEM_BYTES} bytes is spent"
                    ),
                })
            }
        }
    }

    /// The package set.
    pub fn set(&self) -> &'a PackageSet {
        self.set
    }

    /// Notes collected while decoding (bounded by [`MAX_NOTES`]).
    pub fn notes(&self) -> Vec<String> {
        self.notes.borrow().clone()
    }

    fn note(&self, s: String) {
        let mut n = self.notes.borrow_mut();
        if n.len() < MAX_NOTES {
            n.push(s);
        }
    }

    /// Role of the class at `class_path`.
    pub fn role_of(&self, class_path: &str) -> Option<ParticleRole> {
        let key = class_path.to_ascii_lowercase();
        if let Some(r) = self.roles.borrow().get(&key) {
            return *r;
        }
        let r = ParticleRole::classify(class_path, &self.set.super_chain(class_path));
        self.roles.borrow_mut().insert(key, r);
        r
    }

    /// Class path and role of export `index`, if it is a particle object.
    pub fn export_role(&self, lp: &LoadedPackage, index: usize) -> Option<(String, ParticleRole)> {
        let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index).ok()?;
        let role = self.role_of(&class)?;
        Some((class, role))
    }

    /// Merged class defaults of `class_path`.
    pub fn class_defaults(&self, class_path: &str) -> Arc<Vec<Property>> {
        let key = class_path.to_ascii_lowercase();
        if let Some(d) = self.defaults.borrow().get(&key) {
            return d.clone();
        }
        let props: Vec<Property> = match self.set.inherited_defaults(class_path) {
            Ok(d) => d
                .values
                .into_iter()
                .map(|v| Property {
                    name: v.name,
                    type_name: v.type_name,
                    array_index: v.array_index,
                    size: 0,
                    struct_name: None,
                    enum_name: None,
                    value: v.value,
                    offset: 0,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let props = Arc::new(props);
        self.defaults.borrow_mut().insert(key, props.clone());
        props
    }

    /// Decode export `index` of `lp` strictly (tags must end at
    /// `SerialSize`) and merge it over its archetype or class defaults.
    pub fn resolve(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<Arc<Resolved>, ParticleError> {
        self.resolve_depth(lp, index, 0)
    }

    /// [`Self::resolve`] by qualified object path.
    pub fn resolve_path(&self, path: &str) -> Option<Result<Arc<Resolved>, ParticleError>> {
        let (lp, i) = self.set.locate(path)?;
        Some(self.resolve(&lp, i))
    }

    fn resolve_depth(
        &self,
        lp: &LoadedPackage,
        index: usize,
        depth: usize,
    ) -> Result<Arc<Resolved>, ParticleError> {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(r) = self.resolved.borrow().get(&key) {
            return Ok(r.clone());
        }
        if let Some(e) = self.failed.borrow().get(&key) {
            return Err(ParticleError::Malformed {
                path: format!("{}#{index}", lp.name),
                detail: e.clone(),
            });
        }
        let out = self.resolve_uncached(lp, index, depth);
        match &out {
            Ok(r) => {
                self.resolved.borrow_mut().insert(key, r.clone());
            }
            Err(e) => {
                self.failed.borrow_mut().insert(key, e.to_string());
            }
        }
        out
    }

    fn resolve_uncached(
        &self,
        lp: &LoadedPackage,
        index: usize,
        depth: usize,
    ) -> Result<Arc<Resolved>, ParticleError> {
        let Some((class_path, role)) = self.export_role(lp, index) else {
            let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
                .unwrap_or_default();
            return Err(ParticleError::NotParticle {
                export: index,
                class,
            });
        };
        let entry = lp.package.export(index)?;
        let is_default_object = entry.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0;
        let archetype_index = entry.archetype_index;
        let obj = self.set.decode(lp, index)?;
        if obj.properties_end != obj.payload_size {
            return Err(ParticleError::NativeTail {
                path: obj.path,
                class: class_path,
                end: obj.properties_end,
                size: obj.payload_size,
            });
        }
        let payload = lp.package.export_data(index)?;
        let mut own = obj.properties;
        recover_arrays(&mut own, payload, &lp.package, Some(&lp.name), 0);

        let archetype = if archetype_index.is_null() {
            None
        } else {
            lp.ref_path(archetype_index)?
        };
        let mut base: Option<Arc<Vec<Property>>> = None;
        if let Some(apath) = &archetype {
            if depth >= MAX_ARCHETYPE_DEPTH {
                self.note(format!(
                    "{}: archetype chain longer than {MAX_ARCHETYPE_DEPTH}",
                    obj.path
                ));
            } else if let Some((alp, ai)) = self.set.locate(apath) {
                match self.resolve_depth(&alp, ai, depth + 1) {
                    Ok(a) => base = Some(a.props.clone()),
                    Err(e) => self.note(format!("{}: archetype {apath}: {e}", obj.path)),
                }
            } else {
                self.note(format!("{}: archetype {apath} not found", obj.path));
            }
        }
        let base = base.unwrap_or_else(|| self.class_defaults(&class_path));
        let props = if own.is_empty() {
            base
        } else {
            let mut merged = (*base).clone();
            merge_properties(&mut merged, &own);
            Arc::new(merged)
        };
        Ok(Arc::new(Resolved {
            package: lp.name.clone(),
            export_index: index,
            path: obj.path,
            class_path,
            role,
            is_default_object,
            archetype,
            own,
            props,
            payload_size: obj.payload_size,
            warnings: obj.warnings,
        }))
    }

    fn resolve_role(
        &self,
        path: &str,
        want: &[ParticleRole],
    ) -> Result<Arc<Resolved>, ParticleError> {
        let r = self
            .resolve_path(path)
            .ok_or_else(|| ObjectError::NotFound(path.to_owned()))??;
        if want.contains(&r.role) {
            Ok(r)
        } else {
            Err(ParticleError::Malformed {
                path: path.to_owned(),
                detail: format!(
                    "is a {} ({}), expected {:?}",
                    r.role.name(),
                    r.class_path,
                    want
                ),
            })
        }
    }

    // ------------------------------------------------------------ values

    /// Convert an effective property list to parameters (editor-only and
    /// typed fields left out).
    pub fn params(&self, props: &[Property], notes: &mut Vec<String>) -> BTreeMap<String, Param> {
        let mut out: BTreeMap<String, Param> = BTreeMap::new();
        let mut arrays: BTreeMap<String, BTreeMap<i32, Param>> = BTreeMap::new();
        let static_arrays: Vec<String> = props
            .iter()
            .filter(|p| p.array_index > 0)
            .map(|p| p.name.to_ascii_lowercase())
            .collect();
        for p in props {
            if excluded(&p.name) {
                continue;
            }
            let v = self.param(p, notes, 0);
            if static_arrays.contains(&p.name.to_ascii_lowercase()) {
                arrays
                    .entry(p.name.clone())
                    .or_default()
                    .insert(p.array_index, v);
            } else {
                out.insert(p.name.clone(), v);
            }
        }
        for (name, elems) in arrays {
            let len = elems
                .keys()
                .max()
                .map_or(0, |m| usize::try_from(*m).unwrap_or(0).saturating_add(1))
                .min(256);
            let list = (0..len)
                .map(|i| {
                    i32::try_from(i)
                        .ok()
                        .and_then(|i| elems.get(&i).cloned())
                        .unwrap_or(Param::Null)
                })
                .collect();
            out.insert(name, Param::List(list));
        }
        out
    }

    fn param(&self, p: &Property, notes: &mut Vec<String>, depth: usize) -> Param {
        self.value_param(&p.value, p.struct_name.as_deref(), notes, depth)
    }

    fn value_param(
        &self,
        v: &Value,
        struct_name: Option<&str>,
        notes: &mut Vec<String>,
        depth: usize,
    ) -> Param {
        if depth > MAX_PARAM_DEPTH {
            return Param::Null;
        }
        match v {
            Value::Bool(b) => Param::Bool(*b),
            Value::Int(i) => Param::Int(i64::from(*i)),
            Value::Byte(b) => Param::Int(i64::from(*b)),
            Value::Float(f) => Param::Float(*f),
            Value::Enum(s) | Value::Name(s) | Value::Str(s) => Param::Text(s.clone()),
            Value::Object(o) | Value::Interface(o) => {
                if o.index == 0 {
                    Param::Null
                } else {
                    Param::Text(o.path.clone())
                }
            }
            Value::Delegate { function, .. } => Param::Text(function.clone()),
            Value::Array(list) => Param::List(
                list.iter()
                    .take(MAX_LOOKUP_TABLE)
                    .map(|x| self.value_param(x, None, notes, depth + 1))
                    .collect(),
            ),
            Value::Struct { name, fields, .. } => {
                let sname = if name.is_empty() {
                    struct_name.unwrap_or("")
                } else {
                    name.as_str()
                };
                if sname.eq_ignore_ascii_case("RawDistributionFloat") {
                    Param::Distribution(Box::new(self.raw_distribution(fields, false, notes)))
                } else if sname.eq_ignore_ascii_case("RawDistributionVector") {
                    Param::Distribution(Box::new(self.raw_distribution(fields, true, notes)))
                } else {
                    let mut m = BTreeMap::new();
                    for f in fields {
                        let key = if f.array_index > 0 {
                            format!("{}[{}]", f.name, f.array_index)
                        } else {
                            f.name.clone()
                        };
                        m.insert(key, self.param(f, notes, depth + 1));
                    }
                    Param::Struct(m)
                }
            }
            Value::RawArray { .. } | Value::Raw { .. } => Param::Null,
        }
    }

    /// Decode a `RawDistributionFloat` (`vector` false) or
    /// `RawDistributionVector` struct.
    pub fn raw_distribution(
        &self,
        fields: &[Property],
        vector: bool,
        notes: &mut Vec<String>,
    ) -> RawDistribution {
        let byte = |n: &str| match prop(fields, n) {
            Some(Value::Byte(b)) => *b,
            Some(Value::Int(i)) => u8::try_from(*i).unwrap_or(0),
            _ => 0,
        };
        let table: Vec<f32> = items(prop(fields, "LookupTable"))
            .iter()
            .take(MAX_LOOKUP_TABLE)
            .filter_map(value_f32)
            .collect();
        let baked = Baked {
            op: byte("Op"),
            elements: byte("LookupTableNumElements"),
            chunk: byte("LookupTableChunkSize"),
            len: table.len(),
            time_scale: get_f32(fields, "LookupTableTimeScale").unwrap_or(0.0),
            start_time: get_f32(fields, "LookupTableStartTime").unwrap_or(0.0),
            range: match table.as_slice() {
                [a, b, ..] => Some([*a, *b]),
                _ => None,
            },
        };
        let object = get_object(fields, "Distribution").map(str::to_owned);
        let (class, value) = match &object {
            Some(path) => match self.resolve_path(path) {
                Some(Ok(r)) => {
                    let want = if vector {
                        ParticleRole::VectorDistribution
                    } else {
                        ParticleRole::FloatDistribution
                    };
                    let d = if let Err(e) = self.charge(&r) {
                        push_note(notes, e.to_string());
                        Distribution::Unsupported {
                            class: r.class_path.clone(),
                        }
                    } else if r.role == want {
                        self.distribution(&r, notes)
                    } else {
                        push_note(
                            notes,
                            format!(
                                "{path}: a {} where a distribution was expected",
                                r.role.name()
                            ),
                        );
                        Distribution::Unsupported {
                            class: r.class_path.clone(),
                        }
                    };
                    (Some(r.class_name().to_owned()), d)
                }
                Some(Err(e)) => {
                    push_note(notes, format!("{path}: {e}"));
                    (None, lookup(&baked, &table))
                }
                None => {
                    push_note(
                        notes,
                        format!("{path}: distribution object not found; using the baked table"),
                    );
                    (None, lookup(&baked, &table))
                }
            },
            None => (None, lookup(&baked, &table)),
        };
        RawDistribution {
            dist: if vector { "vector" } else { "float" },
            object,
            class,
            value,
            baked,
            table,
        }
    }

    /// Decode a resolved distribution object.
    pub fn distribution(&self, r: &Resolved, notes: &mut Vec<String>) -> Distribution {
        let chain: Vec<String> = std::iter::once(r.class_path.clone())
            .chain(self.set.super_chain(&r.class_path))
            .map(|c| last_component(&c).to_ascii_lowercase())
            .collect();
        let has = |n: &str| chain.iter().any(|c| c == n);
        let own = chain.first().cloned().unwrap_or_default();
        let is = |n: &str| has(n) || (chain.len() == 1 && own == n);
        let p = r.props.as_slice();
        let vector = r.role == ParticleRole::VectorDistribution;
        let v1 = |name: &str| vec![get_f32(p, name).unwrap_or(0.0)];
        let v3 = |name: &str| get_vec3(p, name).unwrap_or([0.0; 3]).to_vec();
        let lock =
            |name: &str, i: i32| enum_index(prop_at(p, name, i), LOCK_FLAG_NAMES).unwrap_or(0);
        let mirror = || {
            let mut m = [1u8; 3];
            for (i, slot) in m.iter_mut().enumerate() {
                let idx = i32::try_from(i).unwrap_or(0);
                // Unstored entries take the class default (merged into props).
                *slot = enum_index(prop_at(p, "MirrorFlags", idx), MIRROR_FLAG_NAMES).unwrap_or(1);
            }
            m
        };
        if !vector {
            if is("distributionfloatparameterbase") || own.ends_with("parameter") {
                return Distribution::Parameter {
                    name: get_text(p, "ParameterName").unwrap_or("None").to_owned(),
                    modes: vec![enum_index(prop(p, "ParamMode"), PARAM_MODE_NAMES).unwrap_or(0)],
                    min_input: v1("MinInput"),
                    max_input: v1("MaxInput"),
                    min_output: v1("MinOutput"),
                    max_output: v1("MaxOutput"),
                    constant: v1("Constant"),
                    class: r.class_name().to_owned(),
                };
            }
            if is("distributionfloatconstantcurve") {
                return Distribution::ConstantCurve {
                    curve: decode_distribution_curve(prop(p, "ConstantCurve"), 1, notes),
                    locked_axes: 0,
                };
            }
            if is("distributionfloatuniformcurve") {
                return Distribution::UniformCurve {
                    curve: decode_distribution_curve(prop(p, "ConstantCurve"), 2, notes),
                    locked_axes: [0; 2],
                    mirror: [1; 3],
                    use_extremes: false,
                };
            }
            if is("distributionfloatuniform") {
                return Distribution::Uniform {
                    min: v1("Min"),
                    max: v1("Max"),
                    locked_axes: 0,
                    mirror: [1; 3],
                    use_extremes: false,
                };
            }
            if is("distributionfloatconstant") {
                return Distribution::Constant {
                    value: v1("Constant"),
                    locked_axes: 0,
                };
            }
        } else {
            if is("distributionvectorparameterbase") || own.ends_with("parameter") {
                let modes = (0..3)
                    .map(|i| enum_index(prop_at(p, "ParamModes", i), PARAM_MODE_NAMES).unwrap_or(0))
                    .collect();
                return Distribution::Parameter {
                    name: get_text(p, "ParameterName").unwrap_or("None").to_owned(),
                    modes,
                    min_input: v3("MinInput"),
                    max_input: v3("MaxInput"),
                    min_output: v3("MinOutput"),
                    max_output: v3("MaxOutput"),
                    constant: v3("Constant"),
                    class: r.class_name().to_owned(),
                };
            }
            if is("distributionvectorconstantcurve") {
                return Distribution::ConstantCurve {
                    curve: decode_distribution_curve(prop(p, "ConstantCurve"), 3, notes),
                    locked_axes: lock("LockedAxes", 0),
                };
            }
            if is("distributionvectoruniformcurve") {
                return Distribution::UniformCurve {
                    curve: decode_distribution_curve(prop(p, "ConstantCurve"), 6, notes),
                    locked_axes: [lock("LockedAxes", 0), lock("LockedAxes", 1)],
                    mirror: mirror(),
                    use_extremes: get_bool(p, "bUseExtremes").unwrap_or(false),
                };
            }
            if is("distributionvectoruniform") {
                return Distribution::Uniform {
                    min: v3("Min"),
                    max: v3("Max"),
                    locked_axes: lock("LockedAxes", 0),
                    mirror: mirror(),
                    use_extremes: get_bool(p, "bUseExtremes").unwrap_or(false),
                };
            }
            if is("distributionvectorconstant") {
                return Distribution::Constant {
                    value: v3("Constant"),
                    locked_axes: lock("LockedAxes", 0),
                };
            }
        }
        push_note(
            notes,
            format!(
                "{}: unsupported distribution class {}",
                r.path, r.class_path
            ),
        );
        Distribution::Unsupported {
            class: r.class_path.clone(),
        }
    }

    // ------------------------------------------------------------ structure

    /// Decode a module by path.
    pub fn module(&self, path: &str, notes: &mut Vec<String>) -> Result<Module, ParticleError> {
        let r = self.resolve_role(path, &[ParticleRole::Module])?;
        self.charge(&r)?;
        let p = r.props.as_slice();
        Ok(Module {
            path: r.path.clone(),
            class: r.class_name().to_owned(),
            enabled: get_bool(p, "bEnabled").unwrap_or(false),
            spawn: get_bool(p, "bSpawnModule").unwrap_or(false),
            update: get_bool(p, "bUpdateModule").unwrap_or(false),
            lod_validity: get_i32(p, "LODValidity")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(0),
            params: self.params(p, notes),
        })
    }

    fn optional_module(
        &self,
        props: &[Property],
        name: &str,
        owner: &str,
        notes: &mut Vec<String>,
    ) -> Option<Module> {
        let path = get_object(props, name)?;
        match self.module(path, notes) {
            Ok(m) => Some(m),
            Err(e) => {
                push_note(notes, format!("{owner}.{name}: {e}"));
                None
            }
        }
    }

    /// Decode a LOD level by path.
    pub fn lod_level(
        &self,
        path: &str,
        notes: &mut Vec<String>,
    ) -> Result<LodLevel, ParticleError> {
        let r = self.resolve_role(path, &[ParticleRole::LodLevel])?;
        self.charge(&r)?;
        let p = r.props.as_slice();
        let mut modules = Vec::new();
        for (i, m) in items(prop(p, "Modules")).iter().enumerate() {
            if self.budget.get() == 0 {
                push_note(notes, format!("{}: decode budget spent", r.path));
                break;
            }
            if i >= MAX_MODULES {
                push_note(
                    notes,
                    format!("{}: more than {MAX_MODULES} modules", r.path),
                );
                break;
            }
            let Some(mp) = value_object(m) else {
                push_note(notes, format!("{}: module slot {i} is null", r.path));
                continue;
            };
            match self.module(mp, notes) {
                Ok(m) => modules.push(m),
                Err(e) => push_note(notes, format!("{}: module {mp}: {e}", r.path)),
            }
        }
        Ok(LodLevel {
            path: r.path.clone(),
            level: get_i32(p, "Level").unwrap_or(0),
            enabled: get_bool(p, "bEnabled").unwrap_or(false),
            peak_active_particles: get_i32(p, "PeakActiveParticles").unwrap_or(0),
            required: self.optional_module(p, "RequiredModule", &r.path, notes),
            spawn: self.optional_module(p, "SpawnModule", &r.path, notes),
            type_data: self.optional_module(p, "TypeDataModule", &r.path, notes),
            event_generator: self.optional_module(p, "EventGenerator", &r.path, notes),
            modules,
        })
    }

    /// Decode an emitter by path.
    pub fn emitter(&self, path: &str, notes: &mut Vec<String>) -> Result<Emitter, ParticleError> {
        let r = self.resolve_role(path, &[ParticleRole::Emitter])?;
        self.charge(&r)?;
        let p = r.props.as_slice();
        let mut lods = Vec::new();
        for (i, l) in items(prop(p, "LODLevels")).iter().enumerate() {
            if self.budget.get() == 0 {
                push_note(notes, format!("{}: decode budget spent", r.path));
                break;
            }
            if i >= MAX_LOD_LEVELS {
                push_note(
                    notes,
                    format!("{}: more than {MAX_LOD_LEVELS} LOD levels", r.path),
                );
                break;
            }
            let Some(lpath) = value_object(l) else {
                push_note(notes, format!("{}: LOD slot {i} is null", r.path));
                continue;
            };
            match self.lod_level(lpath, notes) {
                Ok(l) => lods.push(l),
                Err(e) => push_note(notes, format!("{}: LOD {lpath}: {e}", r.path)),
            }
        }
        let kind = EmitterKind::of_type_data(
            lods.first()
                .and_then(|l| l.type_data.as_ref())
                .map(|m| m.class.as_str()),
        );
        let name = get_text(p, "EmitterName")
            .filter(|n| !n.eq_ignore_ascii_case("None"))
            .unwrap_or_else(|| r.object_name())
            .to_owned();
        Ok(Emitter {
            path: r.path.clone(),
            class: r.class_name().to_owned(),
            name,
            kind,
            params: self.params(p, notes),
            lods,
        })
    }

    /// Decode the particle system at export `index` of `lp`.
    pub fn system(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<ParticleSystem, ParticleError> {
        let r = self.resolve(lp, index)?;
        if r.role != ParticleRole::System {
            return Err(ParticleError::Malformed {
                path: r.path.clone(),
                detail: format!("is a {}, not a particle system", r.role.name()),
            });
        }
        let p = r.props.as_slice();
        // Each system gets the whole budget.
        self.budget.set(MAX_SYSTEM_BYTES);
        let mut notes = Vec::new();
        let mut emitters = Vec::new();
        let mut skipped = 0usize;
        for (i, e) in items(prop(p, "Emitters")).iter().enumerate() {
            if self.budget.get() == 0 {
                push_note(&mut notes, "decode budget spent".to_owned());
                break;
            }
            if i >= MAX_EMITTERS {
                push_note(&mut notes, format!("more than {MAX_EMITTERS} emitters"));
                break;
            }
            let Some(epath) = value_object(e) else {
                skipped += 1;
                continue;
            };
            match self.emitter(epath, &mut notes) {
                Ok(e) => emitters.push(e),
                Err(err) => {
                    skipped += 1;
                    push_note(&mut notes, format!("emitter {epath}: {err}"));
                }
            }
        }
        let params = self.params(p, &mut notes);
        notes.truncate(MAX_NOTES);
        Ok(ParticleSystem {
            path: r.path.clone(),
            package: r.package.clone(),
            export_index: index,
            params,
            emitters,
            skipped_emitters: skipped,
            notes,
        })
    }

    /// The particle values of the component at export `index` of `lp`.
    pub fn component(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<ComponentInfo, ParticleError> {
        let r = self.resolve(lp, index)?;
        if r.role != ParticleRole::Component {
            return Err(ParticleError::Malformed {
                path: r.path.clone(),
                detail: format!("is a {}, not a particle system component", r.role.name()),
            });
        }
        Ok(component_info(&r))
    }
}

/// Add a note unless the list is already long (twice [`MAX_NOTES`]; the
/// system keeps the first [`MAX_NOTES`]).
fn push_note(notes: &mut Vec<String>, note: String) {
    if notes.len() < 2 * MAX_NOTES {
        notes.push(note);
    }
}

fn lookup(baked: &Baked, table: &[f32]) -> Distribution {
    Distribution::Lookup {
        op: baked.op,
        elements: baked.elements,
        chunk: baked.chunk,
        table: table.to_vec(),
        time_scale: baked.time_scale,
        start_time: baked.start_time,
    }
}

/// The particle values of a resolved component.
pub fn component_info(r: &Resolved) -> ComponentInfo {
    let p = r.props.as_slice();
    let mut params = Vec::new();
    for v in items(prop(p, "InstanceParameters")) {
        let Some(f) = fields(v) else { continue };
        params.push(InstanceParameter {
            name: get_text(f, "Name").unwrap_or("None").to_owned(),
            param_type: get_text(f, "ParamType").unwrap_or("PSPT_None").to_owned(),
            scalar: get_f32(f, "Scalar").unwrap_or(0.0),
            scalar_low: get_f32(f, "Scalar_Low").unwrap_or(0.0),
            vector: get_vec3(f, "Vector").unwrap_or([0.0; 3]),
            vector_low: get_vec3(f, "Vector_Low").unwrap_or([0.0; 3]),
            color: prop(f, "Color")
                .and_then(value_color_bytes)
                .unwrap_or([0; 4]),
            actor: get_object(f, "Actor").map(str::to_owned),
            material: get_object(f, "Material").map(str::to_owned),
        });
    }
    ComponentInfo {
        path: r.path.clone(),
        class: r.class_name().to_owned(),
        template: get_object(p, "Template").map(str::to_owned),
        auto_activate: get_bool(p, "bAutoActivate").unwrap_or(false),
        kill_on_deactivate: get_bool(p, "bKillOnDeactivate").unwrap_or(false),
        kill_on_completed: get_bool(p, "bKillOnCompleted").unwrap_or(false),
        reset_on_detach: get_bool(p, "bResetOnDetach").unwrap_or(false),
        hidden_game: get_bool(p, "HiddenGame").unwrap_or(false),
        warmup_time: get_f32(p, "WarmupTime").unwrap_or(0.0),
        seconds_before_inactive: get_f32(p, "SecondsBeforeInactive").unwrap_or(0.0),
        emitter_delay: get_f32(p, "EmitterDelay").unwrap_or(0.0),
        instance_parameters: params,
    }
}

// ---------------------------------------------------------------------------
// Lookup table evaluation (for checks)
// ---------------------------------------------------------------------------

/// Value of a baked lookup table at `time` for `Op` none (1), the way
/// `FRawDistribution::GetValue1` / `GetValue3` read it (CONFIRMED from the
/// executable): `t = max((time − start) · scale, 0)`, entry `i = trunc(t)`,
/// the two entries at `min(i·chunk + 2, len − chunk)` and
/// `min(i·chunk + 2 + chunk, len − chunk)` lerped by `t − i`; `dim`
/// components per entry. `None` for an empty or inconsistent table.
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
    // `appTrunc` of a huge value saturates here instead of wrapping.
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
                let x = table.get(a + c).copied().unwrap_or(0.0);
                let y = table.get(b + c).copied().unwrap_or(0.0);
                alpha * (y - x) + x
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Census
// ---------------------------------------------------------------------------

/// Per-class counts of the census.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ClassCount {
    /// Role.
    pub role: Option<ParticleRole>,
    /// Exports (class default objects included).
    pub exports: usize,
    /// Of which class default objects.
    pub default_objects: usize,
    /// Tagged properties end exactly at `SerialSize`.
    pub exact: usize,
    /// Failed to decode or carried native bytes.
    pub failed: usize,
}

/// Coverage of every particle export of a package set.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParticleCensus {
    /// Packages scanned.
    pub packages: usize,
    /// Per class name.
    pub classes: BTreeMap<String, ClassCount>,
    /// Particle exports per package.
    pub per_package: BTreeMap<String, usize>,
    /// Particle systems (not class default objects) decoded completely.
    pub systems: usize,
    /// Systems that failed.
    pub systems_failed: usize,
    /// Emitters, LOD levels and modules reached through the systems.
    pub emitters: usize,
    /// LOD levels.
    pub lod_levels: usize,
    /// Modules (counted per LOD level that lists them).
    pub modules: usize,
    /// Raw distributions reached.
    pub raw_distributions: usize,
    /// Of which without a distribution object (table used).
    pub raw_without_object: usize,
    /// Distribution kinds reached.
    pub distribution_kinds: BTreeMap<String, usize>,
    /// Module classes reached through the systems.
    pub module_classes: BTreeMap<String, usize>,
    /// Emitter kinds.
    pub emitter_kinds: BTreeMap<String, usize>,
    /// Bytes of particle export payloads (all consumed by tagged properties).
    pub payload_bytes: u64,
    /// Failures (bounded).
    pub failures: Vec<String>,
    /// Decoding notes of the systems (bounded).
    pub notes: Vec<String>,
}

fn walk_params(p: &BTreeMap<String, Param>, census: &mut ParticleCensus) {
    for v in p.values() {
        walk_param(v, census, 0);
    }
}

fn walk_param(v: &Param, census: &mut ParticleCensus, depth: usize) {
    if depth > MAX_PARAM_DEPTH {
        return;
    }
    match v {
        Param::Distribution(d) => {
            census.raw_distributions += 1;
            if d.object.is_none() {
                census.raw_without_object += 1;
            }
            let kind = match &d.value {
                Distribution::Constant { .. } => "constant",
                Distribution::Uniform { .. } => "uniform",
                Distribution::ConstantCurve { .. } => "constant_curve",
                Distribution::UniformCurve { .. } => "uniform_curve",
                Distribution::Parameter { .. } => "parameter",
                Distribution::Lookup { .. } => "lookup",
                Distribution::Unsupported { .. } => "unsupported",
            };
            *census
                .distribution_kinds
                .entry(format!("{}:{kind}", d.dist))
                .or_default() += 1;
        }
        Param::List(l) => {
            for x in l {
                walk_param(x, census, depth + 1);
            }
        }
        Param::Struct(m) => {
            for x in m.values() {
                walk_param(x, census, depth + 1);
            }
        }
        _ => {}
    }
}

fn walk_module(m: &Module, census: &mut ParticleCensus) {
    census.modules += 1;
    *census.module_classes.entry(m.class.clone()).or_default() += 1;
    walk_params(&m.params, census);
}

impl ParticleDecoder<'_> {
    /// Decode every particle export of `packages` strictly, then every
    /// particle system completely.
    pub fn census(&self, packages: &[Arc<LoadedPackage>]) -> ParticleCensus {
        let mut c = ParticleCensus {
            packages: packages.len(),
            ..ParticleCensus::default()
        };
        let push_fail = |c: &mut ParticleCensus, s: String| {
            if c.failures.len() < MAX_NOTES {
                c.failures.push(s);
            }
        };
        for lp in packages {
            for i in 0..lp.package.exports.len() {
                let Some((class, role)) = self.export_role(lp, i) else {
                    continue;
                };
                let name = last_component(&class).to_owned();
                let is_cdo = lp
                    .package
                    .export(i)
                    .is_ok_and(|e| e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0);
                {
                    let entry = c.classes.entry(name.clone()).or_default();
                    entry.role = Some(role);
                    entry.exports += 1;
                    if is_cdo {
                        entry.default_objects += 1;
                    }
                }
                *c.per_package.entry(lp.name.clone()).or_default() += 1;
                match self.resolve(lp, i) {
                    Ok(r) => {
                        c.payload_bytes = c
                            .payload_bytes
                            .saturating_add(u64::try_from(r.payload_size).unwrap_or(0));
                        if let Some(e) = c.classes.get_mut(&name) {
                            e.exact += 1;
                        }
                    }
                    Err(e) => {
                        if let Some(x) = c.classes.get_mut(&name) {
                            x.failed += 1;
                        }
                        push_fail(&mut c, format!("{}#{i}: {e}", lp.name));
                    }
                }
            }
        }
        for lp in packages {
            for i in 0..lp.package.exports.len() {
                let Some((_, ParticleRole::System)) = self.export_role(lp, i) else {
                    continue;
                };
                if lp
                    .package
                    .export(i)
                    .is_ok_and(|e| e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0)
                {
                    continue;
                }
                match self.system(lp, i) {
                    Ok(s) => {
                        c.systems += 1;
                        walk_params(&s.params, &mut c);
                        for e in &s.emitters {
                            c.emitters += 1;
                            *c.emitter_kinds
                                .entry(format!("{:?}", e.kind).to_ascii_lowercase())
                                .or_default() += 1;
                            walk_params(&e.params, &mut c);
                            for l in &e.lods {
                                c.lod_levels += 1;
                                for m in l
                                    .required
                                    .iter()
                                    .chain(&l.spawn)
                                    .chain(&l.type_data)
                                    .chain(&l.event_generator)
                                    .chain(&l.modules)
                                {
                                    walk_module(m, &mut c);
                                }
                            }
                        }
                        for n in s.notes {
                            if c.notes.len() < MAX_NOTES {
                                c.notes.push(format!("{}: {n}", s.path));
                            }
                        }
                    }
                    Err(e) => {
                        c.systems_failed += 1;
                        push_fail(&mut c, format!("{}#{i}: {e}", lp.name));
                    }
                }
            }
        }
        c
    }
}

/// Export indices of every particle system (not class default objects) in
/// `pkg`, by class name only (`ParticleSystem`).
pub fn system_exports(pkg: &Package) -> Vec<usize> {
    (0..pkg.exports.len())
        .filter(|&i| {
            pkg.export_class_name(i)
                .is_ok_and(|c| c.eq_ignore_ascii_case("ParticleSystem"))
                && pkg
                    .export(i)
                    .is_ok_and(|e| e.object_flags & flags::object::CLASS_DEFAULT_OBJECT == 0)
        })
        .collect()
}

/// Package index of an export (for callers holding indices).
pub fn export_ref(i: usize) -> PackageIndex {
    PackageIndex::from_export(i).unwrap_or_default()
}
