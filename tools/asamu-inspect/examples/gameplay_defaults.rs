//! Gameplay defaults with provenance, from the user's own install.
//!
//! Reads class default objects (inheritance resolved), component templates
//! (archetype chain resolved) and the shipped `.ini` hierarchy, computes the
//! native field layout of each class from its script property layout (Mac
//! x86_64 by default, Win32 with `--target win32`), and writes sanitized JSON
//! (names, types, numeric values, offsets) to an output directory.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/wf2-defaults cargo run --release -p asamu-inspect \
//!     --example gameplay_defaults -- --out docs/reverse-engineering/data/defaults \
//!     [--native-sizes research/local/defaults/gpsc.asm]   # objdump text, see DEFAULTS.md
//!     [--ablate <rule>]       # report what one layout rule decides; writes nothing
//!     [--layout <Class>]...   # print the own-field layout of classes; writes nothing
//!     [--class <Class>]...    # report only these classes
//!
//! # Win32 layout: validates the Win32 rules against the Win32 binary data and
//! # writes native_layout_win32.json (into --win32-data) and the Windows
//! # recorder layout. Run from the repository root.
//! cargo run --release -p asamu-inspect --example gameplay_defaults -- --target win32 \
//!     [--win32-data docs/reverse-engineering/data/win32]
//!     [--recorder-layout tools/trace-recorder/layout_win_x86.json]
//!     [--recorder-template tools/trace-recorder/layout_mac_x86_64.json]
//!     [--check]               # regenerate and compare with the files; writes nothing
//!     [--ablate <rule>]...    # report what the rules decide; writes nothing
//!     [--layout <Class>]...   # print the Win32 own-field layout; writes nothing
//! ```
//!
//! Method and results: `docs/reverse-engineering/DEFAULTS.md`.
//!
//! The install is located through `ASAMU_ORIGINAL_DIR` or the default macOS
//! Steam library. The cooked script packages are byte-identical in the Mac
//! and Windows installs (13 files, equal SHA-256), so the Win32 mode reads
//! the same packages. Nothing is written inside the install. No script
//! source, payload bytes or long strings are emitted: string values longer
//! than [`MAX_STR`] characters, and every localized string, are replaced by
//! their length. The output is deterministic (sorted maps, declaration
//! order).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::flags;
use asamu_ue3::model::PackageSet;
use asamu_ue3::{Property, PropertyDef, PropertyType, Schema, Value};
use serde_json::{Map, Value as Json, json};

/// Longest string value emitted verbatim.
const MAX_STR: usize = 48;
/// Longest config text value emitted verbatim.
const MAX_RAW: usize = 120;
/// Longest array emitted element by element.
const MAX_ARRAY: usize = 24;

/// The build whose native layout is computed. The script property layout is
/// the same on both (the cooked script packages are byte-identical); the
/// C++ ABI differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Mac x86_64, Clang, Itanium C++ ABI (the default).
    MacX8664,
    /// Windows x86, Visual C++ 2010, UE3 headers under `#pragma pack(4)`.
    Win32,
}

impl Target {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "mac" | "mac-x86_64" => Some(Target::MacX8664),
            "win32" | "win-x86" => Some(Target::Win32),
            _ => None,
        }
    }

    /// Rules that `--ablate` can switch off for this target.
    fn ablations(self) -> &'static [&'static str] {
        match self {
            Target::MacX8664 => ABLATIONS,
            Target::Win32 => WIN32_ABLATIONS,
        }
    }
}

// ------------------------------------------------------------------ layout

/// Computed native layout of one property.
#[derive(Debug, Clone)]
struct Field {
    owner: String,
    name: String,
    offset: u64,
    size: u64,
    /// Bit within the 32-bit word at `offset` (bool properties only).
    bit: Option<u32>,
    array_dim: u32,
    kind: &'static str,
    /// Struct type (struct properties only).
    struct_path: Option<String>,
}

/// Computed native layout of a struct or class.
#[derive(Debug, Clone)]
struct Layout {
    /// End of the last property (unpadded).
    end: u64,
    /// Alignment.
    align: u64,
    /// Padded size (`end` aligned to `align`).
    size: u64,
    fields: Vec<Field>,
}

fn align_up(v: u64, a: u64) -> u64 {
    if a <= 1 {
        return v;
    }
    v.div_ceil(a).saturating_mul(a)
}

/// Layout engine: UE3 property linking rules for one little-endian build
/// ([`Target`]).
struct Layouter<'a> {
    set: &'a PackageSet,
    target: Target,
    cache: HashMap<String, Arc<Layout>>,
    depth: usize,
    /// Rules switched off (`--ablate`), to measure what each rule decides.
    ablate: Vec<String>,
}

/// Layout rules that `--ablate` can switch off (Mac x86_64).
const ABLATIONS: &[&str] = &[
    "padded-class-base",
    "no-simd-align",
    "no-color-align",
    "ptr32",
    "no-bool-merge",
    "no-align16-matrix",
    "no-align16-plane",
    "no-align16-quat",
    "no-align16-vector4",
    "no-align16-shvector",
    "no-align16-shvectorrgb",
];

/// Script structs whose native type is declared with a 16-byte alignment
/// (lower-case names). `no-simd-align` drops the rule for all of them,
/// `no-align16-<name>` for one.
const ALIGN16: &[&str] = &[
    "matrix",
    "plane",
    "quat",
    "vector4",
    "shvector",
    "shvectorrgb",
];

/// Layout rules that `--ablate` can switch off (Win32). Each name says what
/// the ablated run does instead of the rule.
const WIN32_ABLATIONS: &[&str] = &[
    "ptr64",
    "no-bool-merge",
    "padded-class-base",
    "pack4-class-base",
    "no-simd-align",
    "no-align16-matrix",
    "no-align16-plane",
    "no-align16-quat",
    "no-align16-vector4",
    "no-align16-shvector",
    "no-align16-shvectorrgb",
    "no-color-align",
    "wide-align-8",
];

/// `#pragma pack` value the Win32 engine headers are compiled under: the
/// natural alignment of a member is capped at 4 bytes.
const WIN32_PACK: u64 = 4;

impl<'a> Layouter<'a> {
    fn new(set: &'a PackageSet) -> Self {
        Self::for_target(set, Target::MacX8664)
    }

    fn for_target(set: &'a PackageSet, target: Target) -> Self {
        Layouter {
            set,
            target,
            cache: HashMap::new(),
            depth: 0,
            ablate: Vec::new(),
        }
    }

    fn off(&self, rule: &str) -> bool {
        self.ablate.iter().any(|a| a == rule)
    }

    fn ptr(&self) -> u64 {
        match self.target {
            Target::MacX8664 if self.off("ptr32") => 4,
            Target::MacX8664 => 8,
            Target::Win32 if self.off("ptr64") => 8,
            Target::Win32 => 4,
        }
    }

    /// Alignment of the 8-byte native mirrors `QWord` and `Double`: 8 on the
    /// Mac build; 4 on Win32, where the engine headers are compiled under
    /// `#pragma pack(4)`.
    fn wide_align(&self) -> u64 {
        match self.target {
            Target::MacX8664 => 8,
            Target::Win32 if self.off("wide-align-8") => 8,
            Target::Win32 => 4,
        }
    }

    /// Where the members of a struct or class begin after its parent `b`.
    ///
    /// On both builds a class starts at its parent's unpadded end and a
    /// script struct after its padded parent (the engine links properties
    /// with the same code everywhere).
    ///
    /// - Mac (Itanium C++ ABI): the compiler reuses a non-POD base's tail
    ///   padding, so the C++ classes agree.
    /// - Win32 (Visual C++): the compiler starts a derived class at the
    ///   parent's *non-virtual size*, its end rounded up to its alignment
    ///   capped by the `#pragma pack(4)` in force; a 16-byte alignment pads
    ///   only the parent's own `sizeof`. For every class of this game that
    ///   is the same offset as the unpadded end (`pack4-class-base`).
    fn start_after_parent(&self, b: &Layout, is_struct: bool) -> u64 {
        if is_struct || self.off("padded-class-base") {
            b.size
        } else if self.target == Target::Win32 && self.off("pack4-class-base") {
            align_up(b.end, b.align.min(WIN32_PACK))
        } else {
            b.end
        }
    }

    /// Element size and alignment of a property type.
    fn element(&mut self, ty: &PropertyType) -> Result<(u64, u64)> {
        Ok(match ty {
            PropertyType::Byte { .. } => (1, 1),
            PropertyType::Int | PropertyType::Float | PropertyType::Bool => (4, 4),
            PropertyType::Name => (8, 4),
            PropertyType::Str | PropertyType::Array { .. } => (self.ptr() + 8, self.ptr()),
            PropertyType::Object { .. }
            | PropertyType::Class { .. }
            | PropertyType::Component { .. } => (self.ptr(), self.ptr()),
            PropertyType::Interface { .. } => (2 * self.ptr(), self.ptr()),
            PropertyType::Delegate { .. } => (self.ptr() + 8, self.ptr()),
            PropertyType::Struct { struct_path } => {
                let l = self.layout(struct_path)?;
                (l.size, l.align)
            }
            // A native TMap; Core.Object.Map_Mirror declares its layout.
            PropertyType::Map { .. } => {
                let l = self.layout("Core.Object.Map_Mirror")?;
                (l.size, l.align)
            }
        })
    }

    /// Layout of the struct or class at `path`.
    fn layout(&mut self, path: &str) -> Result<Arc<Layout>> {
        let key = path.to_ascii_lowercase();
        if let Some(l) = self.cache.get(&key) {
            return Ok(l.clone());
        }
        if self.depth > 64 {
            bail!("layout nesting too deep at {path}");
        }
        let def = self
            .set
            .struct_def(path)
            .with_context(|| format!("no struct definition for {path}"))?;
        let short = def.name.to_ascii_lowercase();
        // Native-mirror structs whose script declaration is narrower than the
        // native type on a 64-bit build.
        if def.kind == asamu_ue3::schema::StructKind::ScriptStruct
            && matches!(short.as_str(), "pointer" | "qword" | "double")
        {
            let (n, a) = if short == "pointer" {
                (self.ptr(), self.ptr())
            } else {
                (8, self.wide_align())
            };
            let l = Arc::new(Layout {
                end: n,
                align: a,
                size: n,
                fields: Vec::new(),
            });
            self.cache.insert(key, l.clone());
            return Ok(l);
        }
        self.depth += 1;
        let base = def.super_path.as_ref().map(|s| self.layout(s));
        self.depth -= 1;
        let (mut end, mut align, mut fields) = match base {
            Some(b) => {
                let b = b?;
                let is_struct = def.kind == asamu_ue3::schema::StructKind::ScriptStruct;
                let start = self.start_after_parent(&b, is_struct);
                (start, b.align, b.fields.clone())
            }
            None => (0, 1, Vec::new()),
        };
        let mut prev_bool: Option<(u64, u32)> = None;
        for p in &def.properties {
            let dim = u32::try_from(p.array_dim.max(1)).unwrap_or(1);
            let (esize, ealign) = self
                .element(&p.ty)
                .with_context(|| format!("{}.{}", def.path, p.name))?;
            let (offset, bit) = if matches!(p.ty, PropertyType::Bool) && dim == 1 {
                match prev_bool {
                    Some((off, b)) if b < 31 && !self.off("no-bool-merge") => (off, b + 1),
                    _ => (align_up(end, 4), 0),
                }
            } else {
                (align_up(end, ealign), u32::MAX)
            };
            let bit = (bit != u32::MAX).then_some(bit);
            prev_bool = bit.map(|b| (offset, b));
            let size = esize.saturating_mul(u64::from(dim));
            end = end.max(offset.saturating_add(size));
            align = align.max(ealign);
            fields.push(Field {
                owner: def.path.clone(),
                name: p.name.clone(),
                offset,
                size,
                bit,
                array_dim: dim,
                kind: p.ty.class_name(),
                struct_path: match &p.ty {
                    PropertyType::Struct { struct_path } => Some(struct_path.clone()),
                    _ => None,
                },
            });
        }
        // Native alignment of math structs (UE3 declares these with a
        // 16-byte alignment for SIMD; FColor is a union with a DWORD).
        let special = match short.as_str() {
            name if ALIGN16.contains(&name)
                && !self.off("no-simd-align")
                && !self.off(&format!("no-align16-{name}")) =>
            {
                16
            }
            "color" if !self.off("no-color-align") => 4,
            _ => 1,
        };
        if def.kind == asamu_ue3::schema::StructKind::ScriptStruct {
            align = align.max(special);
        }
        let l = Arc::new(Layout {
            end,
            align,
            size: align_up(end, align),
            fields,
        });
        self.cache.insert(key, l.clone());
        Ok(l)
    }
}

// ------------------------------------------------------------- native table

/// A field offset observed in native code (`docs/reverse-engineering/NATIVE_PHYSICS.md` §7).
struct NativeRef {
    class: &'static str,
    offset: u64,
    /// Bit of the 64-bit word at `offset`, as written in NATIVE_PHYSICS.md.
    bit64: Option<u32>,
    hypothesis: &'static str,
    evidence_conf: &'static str,
}

const fn nr(
    class: &'static str,
    offset: u64,
    bit64: Option<u32>,
    hypothesis: &'static str,
    evidence_conf: &'static str,
) -> NativeRef {
    NativeRef {
        class,
        offset,
        bit64,
        hypothesis,
        evidence_conf,
    }
}

const NATIVE: &[NativeRef] = &[
    nr("Engine.Actor", 0x080, None, "Location", "STRONG"),
    nr("Engine.Actor", 0x08C, None, "Rotation", "STRONG"),
    nr("Engine.Actor", 0x0C0, None, "Physics", "CONFIRMED"),
    nr("Engine.Actor", 0x0C1, None, "RemoteRole", "TENTATIVE"),
    nr("Engine.Actor", 0x0C2, None, "Role", "STRONG"),
    nr("Engine.Actor", 0x0D0, None, "Base", "STRONG"),
    nr("Engine.Actor", 0x0E8, Some(0), "bStatic", "TENTATIVE"),
    nr("Engine.Actor", 0x0E8, Some(3), "bDeleteMe", "STRONG"),
    nr("Engine.Actor", 0x0E8, Some(4), "bTicked", "STRONG"),
    nr(
        "Engine.Actor",
        0x0E8,
        Some(7),
        "bWorldGeometry",
        "TENTATIVE",
    ),
    nr("Engine.Actor", 0x0E8, Some(17), "bCanStepUpOn", "TENTATIVE"),
    nr("Engine.Actor", 0x0E8, Some(59), "", "UNKNOWN"),
    nr("Engine.Actor", 0x0F0, Some(1), "", "UNKNOWN"),
    nr("Engine.Actor", 0x0F0, Some(12), "bJustTeleported", "STRONG"),
    nr("Engine.Actor", 0x118, None, "WorldInfo", "STRONG"),
    nr("Engine.Actor", 0x120, None, "LifeSpan", "STRONG"),
    nr("Engine.Actor", 0x188, None, "PhysicsVolume", "STRONG"),
    nr("Engine.Actor", 0x190, None, "Velocity", "CONFIRMED"),
    nr("Engine.Actor", 0x19C, None, "Acceleration", "CONFIRMED"),
    nr("Engine.Actor", 0x1D8, None, "RelativeLocation", "TENTATIVE"),
    nr("Engine.Actor", 0x1F0, None, "CollisionComponent", "STRONG"),
    nr("Engine.Actor", 0x1FC, None, "RotationRate", "STRONG"),
    nr("Engine.Actor", 0x208, None, "PendingTouch", "STRONG"),
    nr("Engine.Pawn", 0x250, None, "MaxStepHeight", "STRONG"),
    nr("Engine.Pawn", 0x254, None, "MaxJumpHeight", "TENTATIVE"),
    nr("Engine.Pawn", 0x258, None, "WalkableFloorZ", "STRONG"),
    nr("Engine.Pawn", 0x25C, None, "LedgeCheckThreshold", "STRONG"),
    nr("Engine.Pawn", 0x260, None, "PartialLedgeMoveDir", "STRONG"),
    nr("Engine.Pawn", 0x270, None, "Controller", "CONFIRMED"),
    nr("Engine.Pawn", 0x298, Some(2), "bIsWalking", "STRONG"),
    nr("Engine.Pawn", 0x298, Some(3), "bWantsToCrouch", "STRONG"),
    nr("Engine.Pawn", 0x298, Some(4), "bIsCrouched", "STRONG"),
    nr("Engine.Pawn", 0x298, Some(5), "bTryToUncrouch", "TENTATIVE"),
    nr("Engine.Pawn", 0x298, Some(6), "bCanCrouch", "TENTATIVE"),
    nr("Engine.Pawn", 0x298, Some(7), "bCrawler", "TENTATIVE"),
    nr("Engine.Pawn", 0x298, Some(10), "bCanJump", "TENTATIVE"),
    nr("Engine.Pawn", 0x298, Some(16), "bAvoidLedges", "TENTATIVE"),
    nr(
        "Engine.Pawn",
        0x298,
        Some(18),
        "bAllowLedgeOverhang",
        "TENTATIVE",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(19),
        "bPartiallyOverLedge",
        "STRONG",
    ),
    nr("Engine.Pawn", 0x298, Some(20), "bSimulateGravity", "STRONG"),
    nr(
        "Engine.Pawn",
        0x298,
        Some(22),
        "bCanWalkOffLedges",
        "TENTATIVE",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(25),
        "bDirectHitWall",
        "TENTATIVE",
    ),
    nr("Engine.Pawn", 0x298, Some(27), "bForceFloorCheck", "STRONG"),
    nr("Engine.Pawn", 0x298, Some(41), "", "UNKNOWN"),
    nr(
        "Engine.Pawn",
        0x298,
        Some(49),
        "bRunPhysicsWithNoController",
        "STRONG",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(50),
        "bForceMaxAccel",
        "TENTATIVE",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(51),
        "bLimitFallAccel",
        "TENTATIVE",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(53),
        "bForceRMVelocity",
        "TENTATIVE",
    ),
    nr(
        "Engine.Pawn",
        0x298,
        Some(54),
        "bForceRegularVelocity",
        "TENTATIVE",
    ),
    nr("Engine.Pawn", 0x298, Some(59), "", "UNKNOWN"),
    nr("Engine.Pawn", 0x2A0, None, "WalkingPhysics", "STRONG"),
    nr("Engine.Pawn", 0x2A8, None, "UncrouchTime", "STRONG"),
    nr("Engine.Pawn", 0x2AC, None, "CrouchHeight", "STRONG"),
    nr("Engine.Pawn", 0x2B0, None, "CrouchRadius", "STRONG"),
    nr("Engine.Pawn", 0x2D0, None, "DesiredSpeed", "STRONG"),
    nr("Engine.Pawn", 0x2E8, None, "AvgPhysicsTime", "TENTATIVE"),
    nr("Engine.Pawn", 0x2F0, None, "Buoyancy", "STRONG"),
    nr("Engine.Pawn", 0x33C, None, "GroundSpeed", "STRONG"),
    nr("Engine.Pawn", 0x340, None, "WaterSpeed", "STRONG"),
    nr("Engine.Pawn", 0x344, None, "AirSpeed", "STRONG"),
    nr("Engine.Pawn", 0x348, None, "LadderSpeed", "TENTATIVE"),
    nr("Engine.Pawn", 0x34C, None, "AccelRate", "STRONG"),
    nr("Engine.Pawn", 0x350, None, "JumpZ", "STRONG"),
    nr("Engine.Pawn", 0x35C, None, "AirControl", "STRONG"),
    nr("Engine.Pawn", 0x360, None, "WalkingPct", "STRONG"),
    nr(
        "Engine.Pawn",
        0x364,
        None,
        "MovementSpeedModifier",
        "STRONG",
    ),
    nr("Engine.Pawn", 0x368, None, "CrouchedPct", "STRONG"),
    nr("Engine.Pawn", 0x374, None, "BaseEyeHeight", "STRONG"),
    nr("Engine.Pawn", 0x378, None, "EyeHeight", "STRONG"),
    nr("Engine.Pawn", 0x37C, None, "Floor", "STRONG"),
    nr("Engine.Pawn", 0x398, None, "Health", "STRONG"),
    nr("Engine.Pawn", 0x3B0, None, "RMVelocity", "TENTATIVE"),
    nr("Engine.Pawn", 0x470, None, "Mesh", "STRONG"),
    nr("Engine.Pawn", 0x478, None, "CylinderComponent", "STRONG"),
    nr("Engine.Pawn", 0x4AC, None, "DesiredRotation", "STRONG"),
    nr("Engine.Pawn", 0x500, None, "", "UNKNOWN"),
    nr("UDKBase.UDKPawn", 0x590, Some(1), "", "TENTATIVE"),
    nr("UDKBase.UDKPawn", 0x590, Some(2), "", "TENTATIVE"),
    nr("UDKBase.UDKPawn", 0x590, Some(6), "", "TENTATIVE"),
    nr("UDKBase.UDKPawn", 0x590, Some(15), "", "TENTATIVE"),
    nr(
        "UDKBase.UDKPawn",
        0x5A0,
        None,
        "MultiJumpBoost",
        "TENTATIVE",
    ),
    nr(
        "UDKBase.UDKPawn",
        0x5A4,
        None,
        "CustomGravityScaling",
        "STRONG",
    ),
    nr("UDKBase.UDKPawn", 0x61C, None, "OldZ", "TENTATIVE"),
    nr("UDKBase.UDKPawn", 0x788, None, "", "UNKNOWN"),
    nr(
        "UDKBase.UDKPawn",
        0x78C,
        None,
        "SlopeBoostFriction",
        "TENTATIVE",
    ),
    nr("UDKBase.UDKPawn", 0x798, None, "MaxLeanRoll", "TENTATIVE"),
    nr("Engine.Controller", 0x250, None, "Pawn", "STRONG"),
    nr(
        "Engine.Controller",
        0x270,
        Some(4),
        "bNotifyPostLanded",
        "TENTATIVE",
    ),
    nr(
        "Engine.Controller",
        0x270,
        Some(5),
        "bNotifyApex",
        "TENTATIVE",
    ),
    nr(
        "Engine.Controller",
        0x270,
        Some(14),
        "bNotifyFallingHitWall",
        "TENTATIVE",
    ),
    nr(
        "Engine.Controller",
        0x270,
        Some(16),
        "bPreciseDestination",
        "TENTATIVE",
    ),
    nr("Engine.Controller", 0x278, None, "MinHitWall", "TENTATIVE"),
    nr("Engine.Controller", 0x29C, None, "MoveTimer", "STRONG"),
    nr(
        "Engine.PhysicsVolume",
        0x284,
        None,
        "ZoneVelocity",
        "STRONG",
    ),
    nr(
        "Engine.PhysicsVolume",
        0x290,
        Some(0),
        "bVelocityAffectsWalking",
        "TENTATIVE",
    ),
    nr(
        "Engine.PhysicsVolume",
        0x290,
        Some(12),
        "bWaterVolume",
        "STRONG",
    ),
    nr(
        "Engine.PhysicsVolume",
        0x294,
        None,
        "GroundFriction",
        "STRONG",
    ),
    nr(
        "Engine.PhysicsVolume",
        0x298,
        None,
        "TerminalVelocity",
        "STRONG",
    ),
    nr(
        "Engine.PhysicsVolume",
        0x2AC,
        None,
        "FluidFriction",
        "TENTATIVE",
    ),
    nr("Engine.GravityVolume", 0x2D8, None, "GravityZ", "STRONG"),
    nr("Engine.WorldInfo", 0x538, None, "TimeSeconds", "STRONG"),
    nr("Engine.WorldInfo", 0x5FC, None, "WorldGravityZ", "STRONG"),
    nr("Engine.WorldInfo", 0x600, None, "DefaultGravityZ", "STRONG"),
    nr("Engine.WorldInfo", 0x604, None, "GlobalGravityZ", "STRONG"),
    nr(
        "Engine.WorldInfo",
        0x608,
        None,
        "RBPhysicsGravityScaling",
        "STRONG",
    ),
    nr(
        "Engine.PrimitiveComponent",
        0x200,
        None,
        "Translation",
        "STRONG",
    ),
    nr(
        "Engine.CylinderComponent",
        0x238,
        None,
        "CollisionHeight",
        "STRONG",
    ),
    nr(
        "Engine.CylinderComponent",
        0x23C,
        None,
        "CollisionRadius",
        "STRONG",
    ),
    // Offsets mentioned in the NATIVE_PHYSICS.md prose (sections 1-6).
    nr("Engine.Actor", 0x0F0, Some(4), "", "UNKNOWN"),
    nr(
        "Engine.Controller",
        0x270,
        Some(16),
        "bPreciseDestination",
        "TENTATIVE",
    ),
    nr("Engine.Pawn", 0x4B4, None, "DesiredRotation", "STRONG"),
    nr("Engine.WorldInfo", 0x5B8, None, "NetMode", "STRONG"),
    nr("Engine.PlayerController", 0x4C0, None, "", "UNKNOWN"),
    nr("Engine.Camera", 0x4E0, None, "ModifierList", "TENTATIVE"),
    nr("Engine.Camera", 0x570, None, "", "UNKNOWN"),
    nr("Engine.SkeletalMeshComponent", 0x6B0, None, "", "TENTATIVE"),
    nr(
        "Engine.SkeletalMeshComponent",
        0x6E4,
        None,
        "RootMotionMode",
        "TENTATIVE",
    ),
    nr(
        "Engine.SkeletalMeshComponent",
        0x6E5,
        None,
        "PreviousRMM",
        "TENTATIVE",
    ),
    nr(
        "Engine.PrimitiveComponent",
        0x08C,
        None,
        "Bounds",
        "TENTATIVE",
    ),
];

/// Find the computed field at a native (offset, bit-of-64) location.
fn field_at(layout: &Layout, offset: u64, bit64: Option<u32>) -> Option<&Field> {
    match bit64 {
        Some(b) => {
            let word = offset + 4 * u64::from(b / 32);
            let bit = b % 32;
            layout
                .fields
                .iter()
                .find(|f| f.offset == word && f.bit == Some(bit))
        }
        None => layout
            .fields
            .iter()
            .find(|f| f.offset <= offset && offset < f.offset + f.size.max(1) && f.bit.is_none()),
    }
}

/// Name the struct member (recursively) at byte `rel` inside struct field `f`.
fn member_path(lay: &mut Layouter<'_>, f: &Field, rel: u64) -> Result<Option<String>> {
    let Some(sp) = &f.struct_path else {
        return Ok(None);
    };
    let l = lay.layout(sp)?;
    let elem = if f.array_dim > 1 {
        f.size / u64::from(f.array_dim)
    } else {
        f.size
    };
    let (index, rel) = match (rel.checked_div(elem), rel.checked_rem(elem)) {
        (Some(i), Some(r)) => (i, r),
        _ => (0, rel),
    };
    let Some(m) = l
        .fields
        .iter()
        .find(|m| m.offset <= rel && rel < m.offset + m.size.max(1))
    else {
        return Ok(Some("(padding)".to_owned()));
    };
    let prefix = if f.array_dim > 1 {
        format!("[{index}].{}", m.name)
    } else {
        m.name.clone()
    };
    let deeper = member_path(lay, m, rel - m.offset)?;
    Ok(Some(match deeper {
        Some(d) if m.offset != rel || m.struct_path.is_some() => format!("{prefix}.{d}"),
        _ => prefix,
    }))
}

fn hex(v: u64) -> String {
    format!("0x{v:03X}")
}

fn native_check(lay: &mut Layouter<'_>) -> Result<Json> {
    let mut rows = Vec::new();
    let (mut exact, mut named, mut mismatched) = (0usize, 0usize, 0usize);
    for r in NATIVE {
        let layout = lay.layout(r.class)?;
        let hit = field_at(&layout, r.offset, r.bit64);
        let (script, owner, start) = match hit {
            Some(f) => (f.name.clone(), f.owner.clone(), f.offset),
            None => ("(none)".to_owned(), String::new(), 0),
        };
        let inside = hit.is_some_and(|f| f.offset != r.offset && f.bit.is_none());
        let member = match hit {
            Some(f) if inside => member_path(lay, f, r.offset - f.offset)?,
            _ => None,
        };
        let verdict = if r.hypothesis.is_empty() {
            "named"
        } else if script.eq_ignore_ascii_case(r.hypothesis) {
            "match"
        } else {
            "mismatch"
        };
        match verdict {
            "match" => exact += 1,
            "named" => named += 1,
            _ => mismatched += 1,
        }
        let mut row = Map::new();
        row.insert("class".into(), json!(r.class));
        row.insert("native_offset".into(), json!(hex(r.offset)));
        if let Some(b) = r.bit64 {
            row.insert("native_bit_of_u64".into(), json!(b));
            row.insert(
                "u32_word".into(),
                json!(hex(r.offset + 4 * u64::from(b / 32))),
            );
            row.insert("u32_bit".into(), json!(b % 32));
        }
        row.insert("native_hypothesis".into(), json!(r.hypothesis));
        row.insert("native_name_confidence".into(), json!(r.evidence_conf));
        row.insert("script_property".into(), json!(script));
        row.insert("script_owner".into(), json!(owner));
        if inside {
            row.insert("inside_field_starting_at".into(), json!(hex(start)));
        }
        if let Some(m) = member {
            row.insert("struct_member".into(), json!(m));
        }
        row.insert("verdict".into(), json!(verdict));
        rows.push(Json::Object(row));
    }
    Ok(json!({
        "summary": {"match": exact, "mismatch": mismatched, "named_unknown": named, "total": NATIVE.len()},
        "rows": rows,
    }))
}

/// Own fields of each class (no inherited duplication), one row per field:
/// `[offset, name, property kind, size, bit or null, array dim]`.
fn layout_json(lay: &mut Layouter<'_>, classes: &[&str]) -> Result<Json> {
    let mut out = Map::new();
    for class in classes {
        let l = lay.layout(class)?;
        let def = lay
            .set
            .struct_def(class)
            .with_context(|| format!("no class {class}"))?;
        let own: Vec<Json> = l
            .fields
            .iter()
            .filter(|f| f.owner.eq_ignore_ascii_case(&def.path))
            .map(|f| {
                json!([
                    hex(f.offset),
                    f.name,
                    f.kind.trim_end_matches("Property"),
                    f.size,
                    f.bit,
                    f.array_dim
                ])
            })
            .collect();
        let first = own
            .first()
            .and_then(|r| r.get(0))
            .cloned()
            .unwrap_or(Json::Null);
        out.insert(
            def.path.clone(),
            json!({
                "super": def.super_path,
                "first_own_field": first,
                "end": hex(l.end),
                "align": l.align,
                "fields": own,
            }),
        );
    }
    Ok(json!({
        "schema": "asamu-decomp/native-layout/v1",
        "build": "Mac x86_64 (Steam build 1822049)",
        "row": ["offset", "name", "kind", "size", "bit", "array_dim"],
        "classes": out,
    }))
}

// ------------------------------------------------------------ class sizes

/// `sizeof` of each native class as passed to the `UClass` static constructor
/// in `<Class>::GetPrivateStaticClass<Class>`, parsed from `objdump -d` text of
/// those functions: the last `movl $imm, %edx` before the call to
/// `UClass::UClass(EStaticConstructor, ...)` (third argument = size).
fn parse_native_sizes(text: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut class: Option<String> = None;
    let mut last: Option<u64> = None;
    for line in text.lines() {
        let l = line.trim_end();
        if let Some(label) = l.strip_suffix(">:")
            && let Some(pos) = label.find('<')
        {
            let sym = &label[pos + 1..];
            class = sym
                .find("GetPrivateStaticClass")
                .map(|i| &sym[i + "GetPrivateStaticClass".len()..])
                .and_then(|rest| rest.strip_suffix("EPKw"))
                .map(str::to_owned);
            last = None;
            continue;
        }
        let Some(c) = &class else {
            continue;
        };
        if let Some(i) = l
            .find("movl\t$0x")
            .or_else(|| l.find("movl $0x"))
            .or_else(|| {
                l.find("movl")
                    .filter(|_| l.contains("$0x") && l.contains("%edx"))
            })
        {
            let rest = &l[i..];
            if rest.contains("%edx")
                && let Some(h) = rest.split("$0x").nth(1)
            {
                let hexs: String = h.chars().take_while(char::is_ascii_hexdigit).collect();
                last = u64::from_str_radix(&hexs, 16).ok();
            }
        }
        if l.contains("UClassC1E18EStaticConstructor") {
            if let Some(v) = last {
                out.insert(c.clone(), v);
            }
            class = None;
        }
    }
    out
}

/// Every class of the cooked script packages: `(package file name, class
/// path, native)`, in file then export order.
fn script_classes(set: &PackageSet, cooked: &Path) -> Result<Vec<(String, String, bool)>> {
    let mut files: Vec<PathBuf> = fs::read_dir(cooked)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".u") || n.eq_ignore_ascii_case("Startup.upk")
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in &files {
        let lp = set.open_file(f)?;
        for i in 0..lp.package.exports.len() {
            if lp.package.export_class_name(i).ok().as_deref() != Some("Class") {
                continue;
            }
            let path = lp.qualified(i)?;
            let model = set.class_model(&path)?;
            let native = model.class_flags & flags::class::NATIVE != 0;
            out.push((lp.name.clone(), path, native));
        }
    }
    Ok(out)
}

/// Compare computed layout sizes of every native script class with the
/// native `sizeof` values.
fn class_size_check(
    set: &PackageSet,
    lay: &mut Layouter<'_>,
    cooked: &Path,
    native: &BTreeMap<String, u64>,
    publish: &std::collections::HashSet<String>,
) -> Result<Json> {
    // C++ names without their one-letter prefix (AActor -> Actor).
    let mut by_name: HashMap<String, Vec<(String, u64)>> = HashMap::new();
    for (cpp, size) in native {
        let short = cpp.get(1..).unwrap_or(cpp).to_ascii_lowercase();
        by_name.entry(short).or_default().push((cpp.clone(), *size));
    }
    let (mut matched, mut mismatched, mut unpaired, mut nonnative) =
        (0usize, 0usize, 0usize, 0usize);
    let mut mismatches = Vec::new();
    let mut unpaired_names: BTreeMap<String, usize> = BTreeMap::new();
    let mut sizes = Map::new();
    for (file, path, is_native) in script_classes(set, cooked)? {
        if !is_native {
            nonnative += 1;
            continue;
        }
        let short = path
            .rsplit('.')
            .next()
            .unwrap_or(&path)
            .to_ascii_lowercase();
        let Some(cands) = by_name.get(&short) else {
            unpaired += 1;
            *unpaired_names
                .entry(format!("{file}: no GetPrivateStaticClass function"))
                .or_insert(0usize) += 1;
            continue;
        };
        let (cpp, nsize) = match cands.as_slice() {
            [one] => one.clone(),
            _ => {
                unpaired += 1;
                *unpaired_names
                    .entry(format!("{file}: several C++ classes share the name"))
                    .or_insert(0usize) += 1;
                continue;
            }
        };
        let computed = match lay.layout(&path) {
            Ok(l) => l.size,
            Err(e) => {
                mismatches.push(json!({"class": path, "error": e.to_string()}));
                mismatched += 1;
                continue;
            }
        };
        let ok = computed == nsize;
        if ok {
            matched += 1;
        } else {
            mismatched += 1;
            mismatches.push(json!({
                "class": path, "cpp": cpp,
                "script_layout_size": hex(computed), "native_sizeof": hex(nsize),
            }));
        }
        if publish.contains(&path.to_ascii_lowercase()) {
            sizes.insert(path.clone(), json!(hex(nsize)));
        }
    }
    Ok(json!({
        "schema": "asamu-decomp/native-class-sizes/v1",
        "method": "script layout size (UE3 link rules, 64-bit) vs sizeof passed to UClass::UClass(EStaticConstructor) in <Class>::GetPrivateStaticClass<Class>",
        "summary": {
            "native_sizes_parsed": native.len(),
            "native_script_classes_compared": matched + mismatched,
            "equal": matched,
            "different": mismatched,
            "native_script_classes_without_unique_cpp_match": unpaired,
            "non_native_script_classes_skipped": nonnative,
        },
        "different": mismatches,
        "not_compared": unpaired_names,
        "sizeof_by_script_class_note": "listed for the classes in the gameplay chains only; every compared class is equal",
        "sizeof_by_script_class": sizes,
    }))
}

// ------------------------------------------------------------------ Win32
//
// The Windows build (`Binaries/Win32/ASAMU-Win32-Shipping.exe`, Visual C++
// 2010, 32-bit) loads the same script packages as the Mac build, so the same
// property lists are laid out with the Win32 rules of [`Target::Win32`] and
// checked against the facts read from that executable
// (`docs/reverse-engineering/data/win32/`, `WINDOWS_BINARY.md`).

/// Schema of `native_layout_win32.json`.
const WIN32_LAYOUT_SCHEMA: &str = "asamu-decomp/native-layout-win32/v1";
/// Build description used in the Win32 data files.
const WIN32_BUILD: &str = "Win32 (Steam build 1822049)";
/// Game build id used in the Win32 data files.
const WIN32_GAME_BUILD: &str = "steam-1822049-win32";
/// Default folder of the Win32 binary-analysis data.
const WIN32_DATA_DIR: &str = "docs/reverse-engineering/data/win32";
/// File written by the Win32 mode inside [`WIN32_DATA_DIR`].
const WIN32_LAYOUT_FILE: &str = "native_layout_win32.json";
/// Default recorder layout written by the Win32 mode.
const WIN32_RECORDER_LAYOUT: &str = "tools/trace-recorder/layout_win_x86.json";
/// Recorder layout whose field list, sentinels and symbol names are reused.
const RECORDER_TEMPLATE: &str = "tools/trace-recorder/layout_mac_x86_64.json";

/// Classes whose own fields are written to `native_layout_win32.json`: the
/// Mac list ([`LAYOUTS`]) plus the engine and player classes the recorder
/// walks and the script classes it reads.
const WIN32_EXTRA_LAYOUTS: &[&str] = &[
    "Core.Subsystem",
    "Engine.Engine",
    "Engine.Player",
    "Engine.LocalPlayer",
    "Engine.UIRoot",
    "asamu.GrappleGun",
    "asamu.ASAMUPawn",
    "asamu.ASAMURocketBoots",
];

/// Script structs whose member layout is written to
/// `native_layout_win32.json` (array strides and dotted recorder fields).
const WIN32_STRUCTS: &[&str] = &[
    "Engine.Input.KeyBind",
    "Engine.Camera.TCameraCache",
    "Core.Object.TPOV",
    "Core.Object.Vector",
    "Core.Object.Rotator",
];

/// What each Win32 ablation does instead of the rule.
const WIN32_ABLATION_NOTES: &[(&str, &str)] = &[
    ("ptr64", "pointers take 8 bytes, aligned 8"),
    (
        "no-bool-merge",
        "every bool property gets its own 32-bit word",
    ),
    (
        "padded-class-base",
        "a class starts at its parent's full sizeof (rounded up to 16 for a 16-aligned parent)",
    ),
    (
        "pack4-class-base",
        "a class starts at its parent's end rounded up to min(alignment, 4): the Visual C++ non-virtual size",
    ),
    (
        "no-simd-align",
        "Matrix, Plane, Quat, Vector4, SHVector and SHVectorRGB are aligned like their members (4)",
    ),
    (
        "no-align16-matrix",
        "only Matrix loses its own 16-byte alignment (its Plane members keep theirs)",
    ),
    ("no-align16-plane", "only Plane loses its 16-byte alignment"),
    ("no-align16-quat", "only Quat loses its 16-byte alignment"),
    (
        "no-align16-vector4",
        "only Vector4 loses its 16-byte alignment",
    ),
    (
        "no-align16-shvector",
        "only SHVector loses its 16-byte alignment",
    ),
    (
        "no-align16-shvectorrgb",
        "only SHVectorRGB loses its own 16-byte alignment (its SHVector members keep theirs)",
    ),
    ("no-color-align", "Color is aligned like its members (1)"),
    (
        "wide-align-8",
        "QWord and Double are aligned to 8 (as without #pragma pack(4))",
    ),
];

/// How a function of the symbol-less Win32 executable is identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Locate {
    /// A function of `functions.json` (found by the binary scan).
    Function(&'static str),
    /// Entry of the native function name table: the ANSI literal
    /// `<Class>exec<Name>` is followed by the function's address.
    Exec(&'static str),
    /// Slot of a native class's vtable (`class_sizes.json` gives the table).
    Vtable(&'static str, u32),
    /// Target of the tail jump at the start of that vtable slot's function.
    TailOf(&'static str, u32),
    /// The only function that uses this UTF-16 literal of the executable:
    /// every use of the literal's address lies in it.
    Literal(&'static str),
    /// Target of a direct call in another entry of [`WIN32_FUNCTIONS`].
    CalledBy(&'static str),
    /// The instruction sequence itself, found by the binary scan
    /// (`native_evidence.json`); the enclosing function is not named.
    Scan,
}

/// A function of the Win32 executable in which field offsets were read.
struct Win32Function {
    name: &'static str,
    locate: Locate,
    /// RVA of the function's first instruction (0 for [`Locate::Scan`]).
    rva: u32,
    /// Why this is that function, in terms that do not use field offsets.
    identified: &'static str,
}

const fn wf(
    name: &'static str,
    locate: Locate,
    rva: u32,
    identified: &'static str,
) -> Win32Function {
    Win32Function {
        name,
        locate,
        rva,
        identified,
    }
}

/// Functions of the Win32 executable used as offset evidence. Mac slot
/// numbers are those of the symbolised Mac vtables (`__ZTV<Class>`, slot 0
/// after the two header words).
const WIN32_FUNCTIONS: &[Win32Function] = &[
    wf(
        "UObject::GetOutermost",
        Locate::Function("UObject::GetOutermost"),
        0x001F_2730,
        "export table (GetOutermost)",
    ),
    wf(
        "UObject::execIsA",
        Locate::Exec("UObjectexecIsA"),
        0x0018_D370,
        "native function name table",
    ),
    wf(
        "UGameEngine::Tick",
        Locate::Function("UGameEngine::Tick"),
        0x0058_9720,
        "the vtable slot FEngineLoop::Tick calls with (float)GDeltaTime (WINDOWS_BINARY.md 6)",
    ),
    wf(
        "FEngineLoop::Tick",
        Locate::Function("FEngineLoop::Tick"),
        0x014A_8CC0,
        "caller of the time update that increments GFrameCounter (WINDOWS_BINARY.md 6)",
    ),
    wf(
        "UWorld::Tick",
        Locate::Function("UWorld::Tick"),
        0x0063_5450,
        "the single GWorld->Tick(2, DeltaSeconds) call of UGameEngine::Tick (WINDOWS_BINARY.md 6)",
    ),
    wf(
        "appUpdateTimeAndHandleMaxTickRate",
        Locate::Function("appUpdateTimeAndHandleMaxTickRate"),
        0x014A_6820,
        "reads GIsBenchmarking and GUseFixedTimeStep and copies GFixedDeltaTime to GDeltaTime (WINDOWS_BINARY.md 6)",
    ),
    wf(
        "UWorld::IsPaused",
        Locate::Function("UWorld::IsPaused"),
        0x0061_8DD0,
        "called by UWorld::Tick after RealTimeSeconds is advanced; same statements as the Mac function",
    ),
    wf(
        "APawn::performPhysics",
        Locate::Vtable("APawn", 143),
        0x0071_40A0,
        "Mac slot 133. Exec thunks that call a virtual on both builds fix the slot shift at +10 for AActor::SetHardAttach (117 -> 127) and AActor::SetZone (127 -> 137); this is 6 slots after SetZone on both builds",
    ),
    wf(
        "APawn::processLanded",
        Locate::Vtable("APawn", 147),
        0x0071_8D00,
        "Mac slot 137, same +10 shift as APawn::performPhysics (4 slots later on both builds)",
    ),
    wf(
        "APawn::physFalling",
        Locate::Vtable("APawn", 148),
        0x0072_7590,
        "Mac slot 138, same +10 shift as APawn::performPhysics",
    ),
    wf(
        "APawn::physWalking",
        Locate::Vtable("APawn", 149),
        0x0072_5920,
        "Mac slot 139, same +10 shift as APawn::performPhysics; begins with the same test (no Controller and not bRunPhysicsWithNoController)",
    ),
    wf(
        "ABrush::GetWireColor",
        Locate::Vtable("ABrush", 246),
        0x0041_9160,
        "Mac slot 231, the last ABrush virtual on both builds; shift +15 as for every exec-thunk pair from Mac slot 228 up (for example AWorldInfo 228 -> 243)",
    ),
    wf(
        "AUDKPawn::TickSpecial",
        Locate::Vtable("AUDKPawn", 133),
        0x014D_BD70,
        "Mac slot 123, between the two exec-thunk pairs with shift +10 (117 -> 127, 127 -> 137)",
    ),
    wf(
        "AUDKPawn::UpdateEyeHeight",
        Locate::Vtable("AUDKPawn", 331),
        0x014B_7A90,
        "Mac slot 318; the two slots before it are fixed by exec thunks (RestorePreRagdollCollisionComponent 316 -> 329, EnsureOverlayComponentLast 317 -> 330)",
    ),
    wf(
        "APlayerController::Tick",
        Locate::Vtable("APlayerController", 116),
        0x0062_AA10,
        "Mac slot 105; picked among the slots near it by content (the same Role, RemoteRole, PlayerReplicationInfo and view-target statements as the Mac function). The verification pass paired 16 member accesses of the two functions by name; three of those offsets are fixed independently (ViewTarget by the engine's own offset check, TargetViewRotation and TargetEyeHeight by the replication function)",
    ),
    wf(
        "APlayerController::GetViewTarget",
        Locate::Vtable("APlayerController", 269),
        0x004E_BF80,
        "the virtual that the exec thunk APlayerControllerexecGetViewTarget calls (Mac slot 256)",
    ),
    wf(
        "APlayerController::HearSound",
        Locate::Vtable("APlayerController", 319),
        0x0075_7840,
        "Mac slot 311; shift +8 as for the exec-thunk pairs from Mac slot 287 to 306 (for example HasPeerConnection 306 -> 314)",
    ),
    wf(
        "UInput::GetBind",
        Locate::Vtable("UInput", 85),
        0x0091_5760,
        "the virtual that the exec thunk UInputexecGetBind calls (Mac slot 84)",
    ),
    wf(
        "UPlayerInput::InputKey",
        Locate::Vtable("UPlayerInput", 79),
        0x0091_79A0,
        "Mac slot 78; shift +1 as for the exec-thunk pairs UInput::ResetInput (83 -> 84) and UInput::GetBind (84 -> 85)",
    ),
    wf(
        "UPlayerInput::InputAxis",
        Locate::Vtable("UPlayerInput", 80),
        0x0091_6870,
        "Mac slot 79, same +1 shift",
    ),
    wf(
        "UPlayerInput::InputMotion",
        Locate::Vtable("UPlayerInput", 83),
        0x0091_68F0,
        "Mac slot 82, same +1 shift (next to UInput::ResetInput)",
    ),
    wf(
        "ACamera::CheckViewTarget",
        Locate::Vtable("ACamera", 245),
        0x004C_8D50,
        "the virtual that the exec thunk ACameraexecCheckViewTarget calls (Mac slot 230)",
    ),
    wf(
        "ACamera::StopAllCameraAnims",
        Locate::Vtable("ACamera", 247),
        0x004C_9120,
        "the virtual that the exec thunk ACameraexecStopAllCameraAnims calls (Mac slot 232)",
    ),
    wf(
        "ACamera::GetViewTarget",
        Locate::TailOf("APlayerController", 269),
        0x004B_AAC0,
        "tail-jump target of APlayerController::GetViewTarget when PlayerCamera is set, as on the Mac",
    ),
    wf(
        "AController::execWaitForLanding",
        Locate::Exec("AControllerexecWaitForLanding"),
        0x004E_5580,
        "native function name table",
    ),
    wf(
        "AWorldInfo::execAllControllers",
        Locate::Exec("AWorldInfoexecAllControllers"),
        0x0075_9710,
        "native function name table",
    ),
    wf(
        "AWorldInfo::execAllPawns",
        Locate::Exec("AWorldInfoexecAllPawns"),
        0x0075_99F0,
        "native function name table",
    ),
    wf(
        "FName validity test",
        Locate::Scan,
        0x0031_A3E0,
        "small accessor that indexes FName::Names (native_evidence.json)",
    ),
    wf(
        "FName lookup",
        Locate::Scan,
        0,
        "hash-chain compare of the name table (native_evidence.json)",
    ),
    // ---- functions of the independent verification (WIN32_SECOND). None of
    // them is identified through a vtable slot shift or through
    // functions.json.
    wf(
        "UGameEngine::Init",
        Locate::Literal("engine-ini:Engine.Engine.Client"),
        0x0058_DE50,
        "the only function that uses this configuration-key literal (the Mac function uses the same key); it holds the engine's own sixteen 'Class %s Member %s problem' checks of compiled member offsets",
    ),
    wf(
        "AActor::GetOptimizedRepList",
        Locate::Literal("bHardAttach"),
        0x0043_4F60,
        "the only function that uses the property-name literal bHardAttach: it looks up each replicated Actor property by name next to the comparison of that member (also slot 100 of AActor's vtable)",
    ),
    wf(
        "APawn::GetOptimizedRepList",
        Locate::Literal("TearOffMomentum"),
        0x0043_6400,
        "the only function that uses the property-name literal TearOffMomentum (also slot 100 of APawn's vtable)",
    ),
    wf(
        "AController::GetOptimizedRepList",
        Locate::Vtable("AController", 100),
        0x0044_3880,
        "slot 100, the slot of the four replication functions found by their literals; it looks up Pawn and PlayerReplicationInfo by name",
    ),
    wf(
        "APlayerController::GetOptimizedRepList",
        Locate::Literal("TargetEyeHeight"),
        0x0044_B800,
        "the only function that uses the property-name literal TargetEyeHeight (also slot 100 of APlayerController's vtable)",
    ),
    wf(
        "AWorldInfo::GetOptimizedRepList",
        Locate::Literal("WorldGravityZ"),
        0x0044_C640,
        "the only function that uses the property-name literal WorldGravityZ (also slot 100 of AWorldInfo's vtable)",
    ),
    wf(
        "UObject::execGetFuncName",
        Locate::Exec("UObjectexecGetFuncName"),
        0x0018_D1A0,
        "native function name table",
    ),
    wf(
        "AActor::execAllActors",
        Locate::Exec("AActorexecAllActors"),
        0x0076_3440,
        "native function name table",
    ),
    wf(
        "UObject::Register",
        Locate::Vtable("UObject", 35),
        0x001F_92A0,
        "slot 35 of UObject's vtable (Mac slot 36); three of the four literals of the Mac function are used only here",
    ),
    wf(
        "UObject::AddObject",
        Locate::CalledBy("UObject::Register"),
        0x001F_4090,
        "the last direct call of UObject::Register, as on the Mac; it stores the object into UObject::GObjObjects",
    ),
    wf(
        "AActor::execFastTrace",
        Locate::Exec("AActorexecFastTrace"),
        0x0075_7610,
        "native function name table",
    ),
    wf(
        "AActor::execIsBasedOn",
        Locate::Exec("AActorexecIsBasedOn"),
        0x0075_80B0,
        "native function name table",
    ),
    wf(
        "AActor::execIsOwnedBy",
        Locate::Exec("AActorexecIsOwnedBy"),
        0x0075_8130,
        "native function name table",
    ),
    wf(
        "AController::execMoveTo",
        Locate::Exec("AControllerexecMoveTo"),
        0x004E_5C60,
        "native function name table",
    ),
    wf(
        "UEngine::execGetAudioDevice",
        Locate::Exec("UEngineexecGetAudioDevice"),
        0x0053_B700,
        "native function name table",
    ),
    wf(
        "UEngine::execGetEngine",
        Locate::Exec("UEngineexecGetEngine"),
        0x0053_B810,
        "native function name table",
    ),
    wf(
        "UEngine::execGetCurrentWorldInfo",
        Locate::Exec("UEngineexecGetCurrentWorldInfo"),
        0x0053_B4C0,
        "native function name table",
    ),
    wf(
        "AActor::execGetALocalPlayerController",
        Locate::Exec("AActorexecGetALocalPlayerController"),
        0x0051_7580,
        "native function name table",
    ),
    wf(
        "AActor::GetALocalPlayerController",
        Locate::CalledBy("AActor::execGetALocalPlayerController"),
        0x0075_D430,
        "the function the exec thunk calls, as on the Mac",
    ),
    wf(
        "UInput::InputKey",
        Locate::Vtable("UInput", 79),
        0x0091_7640,
        "slot 79 of UInput's vtable (Mac slot 78); the two 'Received ... event for key' log literals of the Mac function are used only here",
    ),
    wf(
        "UInput::Exec",
        Locate::Literal("KEYBINDING"),
        0x0092_1B00,
        "the only function that uses the command literal KEYBINDING; all 13 literals of the Mac function are used here (also slot 86 of UInput's vtable)",
    ),
    wf(
        "FName::SafeString",
        Locate::Literal("*INVALID*"),
        0x001E_20D0,
        "the only function that uses this literal, as on the Mac",
    ),
    wf(
        "FName::ToString",
        Locate::CalledBy("FName::SafeString"),
        0x001E_17F0,
        "called by FName::SafeString for a valid name, as on the Mac",
    ),
    wf(
        "FName::ToString(FString&)",
        Locate::CalledBy("FName::ToString"),
        0x001D_7380,
        "the only call of FName::ToString",
    ),
    wf(
        "FName::AppendString",
        Locate::CalledBy("FName::ToString(FString&)"),
        0x001D_5920,
        "called after the result is sized; appends the entry's characters and the number",
    ),
    wf(
        "FEngineLoop::Exit",
        Locate::Literal("benchmark.log"),
        0x014A_6C10,
        "the only function that uses this file-name literal, as on the Mac",
    ),
    wf(
        "DrawUnitTimes",
        Locate::Literal("Game thread time"),
        0x0055_6920,
        "the only function that uses this literal, as on the Mac",
    ),
    wf(
        "UAudioDevice::GetSortedActiveWaveInstances",
        Locate::Literal("Sound stopped due to duration: %g > %g : %s"),
        0x004A_0750,
        "the only function that uses this log literal, as on the Mac",
    ),
    wf(
        "Matinee editor: fixed-time-step setter",
        Locate::Scan,
        0x0121_C470,
        "editor code (not in the Mac build; our name): it saves the configuration key FixedTimeStepPlayback of section Matinee, then sets the benchmarking flag and the fixed step",
    ),
    wf(
        "FDynamicLightEnvironmentState::Tick",
        Locate::Scan,
        0x002B_1140,
        "paired with the Mac function by content: after testing bit 1 of a flags byte it takes the frame counter modulo 10 (64-bit) and compares it with a random number",
    ),
];

/// A field offset shown by one instruction of the Win32 executable.
struct Win32Native {
    /// Script class that declares the field.
    class: &'static str,
    field: &'static str,
    offset: u64,
    /// Bit within the 32-bit word at `offset` (bool properties).
    bit: Option<u32>,
    /// Entry of [`WIN32_FUNCTIONS`].
    function: &'static str,
    /// RVA of the instruction.
    rva: u32,
    /// The instruction's encoding (one instruction; it contains `offset`).
    bytes: &'static str,
    /// What the instruction does with the field.
    does: &'static str,
}

#[allow(clippy::too_many_arguments)]
const fn wn(
    class: &'static str,
    field: &'static str,
    offset: u64,
    bit: Option<u32>,
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    does: &'static str,
) -> Win32Native {
    Win32Native {
        class,
        field,
        offset,
        bit,
        function,
        rva,
        bytes,
        does,
    }
}

/// Field offsets read from Win32 instructions. Each row was found by pairing
/// the function with its Mac counterpart (same statements, the Mac offsets of
/// `NATIVE_PHYSICS.md` / `native_layout.json`) and is checked two ways: here
/// against the Win32 layout rules, and by `asamu-trace check-recorder`
/// against the bytes of the executable.
const WIN32_NATIVE: &[Win32Native] = &[
    wn(
        "Core.Object",
        "Outer",
        0x28,
        None,
        "UObject::GetOutermost",
        0x1F2763,
        "8b4728",
        "loads",
    ),
    wn(
        "Core.Object",
        "Name",
        0x2C,
        None,
        "UObject::execIsA",
        0x18D3D5,
        "8d462c",
        "takes the address of",
    ),
    wn(
        "Core.Object",
        "Class",
        0x34,
        None,
        "UObject::execIsA",
        0x18D3C2,
        "8b7734",
        "loads",
    ),
    wn(
        "Engine.Engine",
        "Client",
        0x4B0,
        None,
        "UGameEngine::Tick",
        0x5897CE,
        "8b8eb0040000",
        "loads",
    ),
    wn(
        "Engine.Engine",
        "GamePlayers",
        0x4B4,
        None,
        "UGameEngine::Tick",
        0x589903,
        "8b86b4040000",
        "loads",
    ),
    wn(
        "Engine.Player",
        "Actor",
        0x40,
        None,
        "FEngineLoop::Tick",
        0x14A8FA5,
        "8b7140",
        "loads",
    ),
    wn(
        "Engine.Actor",
        "Location",
        0x54,
        None,
        "APawn::processLanded",
        0x718D81,
        "8d7e54",
        "takes the address of",
    ),
    wn(
        "Engine.Actor",
        "Rotation",
        0x60,
        None,
        "APawn::physFalling",
        0x72808A,
        "8d4660",
        "takes the address of",
    ),
    wn(
        "Engine.Actor",
        "Physics",
        0x94,
        None,
        "APawn::physFalling",
        0x7280C4,
        "80be9400000003",
        "compares",
    ),
    wn(
        "Engine.Actor",
        "Base",
        0xA0,
        None,
        "APawn::physWalking",
        0x725E19,
        "8b86a0000000",
        "loads",
    ),
    wn(
        "Engine.Actor",
        "bDeleteMe",
        0xB0,
        Some(3),
        "APawn::physFalling",
        0x7280B7,
        "f686b000000008",
        "tests",
    ),
    wn(
        "Engine.Actor",
        "WorldInfo",
        0xDC,
        None,
        "AUDKPawn::TickSpecial",
        0x14DBD8C,
        "8b86dc000000",
        "loads",
    ),
    wn(
        "Engine.Actor",
        "PhysicsVolume",
        0x134,
        None,
        "APawn::performPhysics",
        0x7140F2,
        "83be3401000000",
        "compares",
    ),
    wn(
        "Engine.Actor",
        "Velocity",
        0x138,
        None,
        "APawn::processLanded",
        0x718FA2,
        "f30f588638010000",
        "reads",
    ),
    wn(
        "Engine.Actor",
        "Acceleration",
        0x144,
        None,
        "APawn::processLanded",
        0x719187,
        "f30f108644010000",
        "loads",
    ),
    wn(
        "Engine.Actor",
        "CollisionComponent",
        0x18C,
        None,
        "APawn::processLanded",
        0x718D20,
        "8b868c010000",
        "loads",
    ),
    wn(
        "Engine.Brush",
        "CsgOper",
        0x1CC,
        None,
        "ABrush::GetWireColor",
        0x4191CC,
        "8a86cc010000",
        "loads",
    ),
    wn(
        "Engine.Brush",
        "BrushColor",
        0x1D0,
        None,
        "ABrush::GetWireColor",
        0x4192D0,
        "8b8ed0010000",
        "loads",
    ),
    wn(
        "Engine.Brush",
        "PolyFlags",
        0x1D4,
        None,
        "ABrush::GetWireColor",
        0x41920E,
        "8b86d4010000",
        "loads",
    ),
    wn(
        "Engine.Brush",
        "bColored",
        0x1D8,
        Some(0),
        "ABrush::GetWireColor",
        0x4191C0,
        "849ed8010000",
        "tests",
    ),
    wn(
        "Engine.PrimitiveComponent",
        "Translation",
        0x1A0,
        None,
        "APawn::physFalling",
        0x72799B,
        "f30f1080a0010000",
        "loads",
    ),
    wn(
        "Engine.CylinderComponent",
        "CollisionHeight",
        0x1D8,
        None,
        "APawn::physFalling",
        0x7279FC,
        "f30f1098d8010000",
        "loads",
    ),
    wn(
        "Engine.CylinderComponent",
        "CollisionRadius",
        0x1DC,
        None,
        "APawn::physFalling",
        0x7279F4,
        "f30f1080dc010000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "MaxStepHeight",
        0x1D0,
        None,
        "APawn::physWalking",
        0x725DBD,
        "f30f1086d0010000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "WalkableFloorZ",
        0x1D8,
        None,
        "APawn::physWalking",
        0x726A6D,
        "f30f10b6d8010000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "LedgeCheckThreshold",
        0x1DC,
        None,
        "APawn::processLanded",
        0x718E0F,
        "f30f108edc010000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "Controller",
        0x1EC,
        None,
        "APawn::physFalling",
        0x7275A8,
        "8b8eec010000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "bRunPhysicsWithNoController",
        0x204,
        Some(17),
        "APawn::physWalking",
        0x725962,
        "f7860402000000000200",
        "tests",
    ),
    wn(
        "Engine.Pawn",
        "GroundSpeed",
        0x28C,
        None,
        "APawn::physWalking",
        0x725D0E,
        "f30f10868c020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "AirSpeed",
        0x294,
        None,
        "APawn::physWalking",
        0x727112,
        "f30f108e94020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "AccelRate",
        0x29C,
        None,
        "APawn::physFalling",
        0x727948,
        "f30f10969c020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "JumpZ",
        0x2A0,
        None,
        "APawn::processLanded",
        0x7190BD,
        "f30f1086a0020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "AirControl",
        0x2AC,
        None,
        "APawn::physFalling",
        0x727785,
        "f30f10a6ac020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "BaseEyeHeight",
        0x2C4,
        None,
        "AUDKPawn::UpdateEyeHeight",
        0x14B7A9C,
        "d986c4020000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "EyeHeight",
        0x2C8,
        None,
        "AUDKPawn::UpdateEyeHeight",
        0x14B7AA2,
        "d99ec8020000",
        "stores",
    ),
    wn(
        "Engine.Pawn",
        "Floor",
        0x2CC,
        None,
        "APawn::processLanded",
        0x7190F9,
        "660fd686cc020000",
        "stores",
    ),
    wn(
        "Engine.Pawn",
        "Weapon",
        0x3C8,
        None,
        "AUDKPawn::TickSpecial",
        0x14DC9CE,
        "8b9ec8030000",
        "loads",
    ),
    wn(
        "Engine.Pawn",
        "FailedLandingCount",
        0x3F0,
        None,
        "APawn::processLanded",
        0x718FB8,
        "ff86f0030000",
        "increments",
    ),
    wn(
        "UDKBase.UDKPawn",
        "RootYaw",
        0x608,
        None,
        "AUDKPawn::TickSpecial",
        0x14DC0C5,
        "2b8608060000",
        "reads",
    ),
    wn(
        "UDKBase.UDKPawn",
        "CurrentSkelAim",
        0x614,
        None,
        "AUDKPawn::TickSpecial",
        0x14DC1CC,
        "f30f118e14060000",
        "stores",
    ),
    wn(
        "Engine.Controller",
        "Pawn",
        0x1D0,
        None,
        "AController::execWaitForLanding",
        0x4E55D5,
        "8b87d0010000",
        "loads",
    ),
    wn(
        "Engine.Actor",
        "LatentFloat",
        0x12C,
        None,
        "AController::execWaitForLanding",
        0x4E55E1,
        "f30f11872c010000",
        "stores",
    ),
    wn(
        "Engine.Controller",
        "PlayerReplicationInfo",
        0x1D4,
        None,
        "APlayerController::Tick",
        0x62AA61,
        "3986d4010000",
        "compares",
    ),
    wn(
        "Engine.Controller",
        "SightCounter",
        0x310,
        None,
        "APlayerController::Tick",
        0x62AEEA,
        "f30f108610030000",
        "loads",
    ),
    wn(
        "Engine.PlayerController",
        "Player",
        0x350,
        None,
        "APlayerController::HearSound",
        0x7578A1,
        "8b8750030000",
        "loads",
    ),
    wn(
        "Engine.PlayerController",
        "bCheckSoundOcclusion",
        0x35C,
        Some(26),
        "APlayerController::HearSound",
        0x757873,
        "8b8f5c030000",
        "reads",
    ),
    wn(
        "Engine.PlayerController",
        "PlayerCamera",
        0x354,
        None,
        "APlayerController::GetViewTarget",
        0x4EBF83,
        "8b8e54030000",
        "loads",
    ),
    wn(
        "Engine.PlayerController",
        "ViewTarget",
        0x374,
        None,
        "APlayerController::GetViewTarget",
        0x4EBFB0,
        "8b8e74030000",
        "loads",
    ),
    wn(
        "Engine.PlayerController",
        "RealViewTarget",
        0x378,
        None,
        "APlayerController::GetViewTarget",
        0x4EBF93,
        "8b8678030000",
        "loads",
    ),
    wn(
        "Engine.PlayerController",
        "TargetViewRotation",
        0x394,
        None,
        "APlayerController::Tick",
        0x62ABF3,
        "660fd68694030000",
        "stores",
    ),
    wn(
        "Engine.PlayerController",
        "PlayerInput",
        0x440,
        None,
        "APlayerController::Tick",
        0x62AC4D,
        "83be4004000000",
        "compares",
    ),
    wn(
        "Engine.PlayerController",
        "Interactions",
        0x468,
        None,
        "APlayerController::Tick",
        0x62ACC2,
        "8b8668040000",
        "loads",
    ),
    wn(
        "Engine.Input",
        "Bindings",
        0x78,
        None,
        "UInput::GetBind",
        0x9158E9,
        "8b4378",
        "loads",
    ),
    wn(
        "Engine.Input",
        "PressedKeys",
        0x84,
        None,
        "UInput::GetBind",
        0x915793,
        "8bb384000000",
        "loads",
    ),
    wn(
        "Engine.PlayerInput",
        "bUsingGamepad",
        0x118,
        Some(0),
        "UPlayerInput::InputKey",
        0x917A3F,
        "838e1801000001",
        "sets",
    ),
    wn(
        "Engine.PlayerInput",
        "LastAxisKeyName",
        0x11C,
        None,
        "UPlayerInput::InputKey",
        0x917A27,
        "8b8e1c010000",
        "loads",
    ),
    wn(
        "Engine.PlayerInput",
        "aTilt",
        0x178,
        None,
        "UPlayerInput::InputMotion",
        0x9169AF,
        "660fd68678010000",
        "stores",
    ),
    wn(
        "Engine.PlayerInput",
        "aAcceleration",
        0x19C,
        None,
        "UPlayerInput::InputMotion",
        0x916A4E,
        "660fd6869c010000",
        "stores",
    ),
    wn(
        "Engine.PlayerInput",
        "SmoothedMouse",
        0x24C,
        None,
        "UPlayerInput::InputAxis",
        0x91688E,
        "f30f10894c020000",
        "loads",
    ),
    wn(
        "Engine.PlayerInput",
        "MouseSamples",
        0x254,
        None,
        "UPlayerInput::InputAxis",
        0x9168A7,
        "ff8154020000",
        "increments",
    ),
    wn(
        "Engine.PlayerInput",
        "MouseSamplingTotal",
        0x258,
        None,
        "UPlayerInput::InputAxis",
        0x91689F,
        "f30f108958020000",
        "loads",
    ),
    wn(
        "Engine.Camera",
        "PCOwner",
        0x1CC,
        None,
        "ACamera::CheckViewTarget",
        0x4C8D62,
        "8b87cc010000",
        "loads",
    ),
    wn(
        "Engine.Camera",
        "ViewTarget",
        0x3C0,
        None,
        "ACamera::GetViewTarget",
        0x4BAAEC,
        "8dbec0030000",
        "takes the address of",
    ),
    wn(
        "Engine.Camera",
        "PendingViewTarget",
        0x3EC,
        None,
        "ACamera::GetViewTarget",
        0x4BAAC3,
        "83beec03000000",
        "compares",
    ),
    wn(
        "Engine.Camera",
        "ActiveAnims",
        0x48C,
        None,
        "ACamera::StopAllCameraAnims",
        0x4C9168,
        "8b838c040000",
        "loads",
    ),
    wn(
        "Engine.WorldInfo",
        "bRequestedBlockOnAsyncLoading",
        0x330,
        Some(7),
        "UWorld::IsPaused",
        0x618DF8,
        "f6863003000080",
        "tests",
    ),
    wn(
        "Engine.WorldInfo",
        "TimeDilation",
        0x41C,
        None,
        "UWorld::Tick",
        0x63569F,
        "f30f10831c040000",
        "loads",
    ),
    wn(
        "Engine.WorldInfo",
        "TimeSeconds",
        0x424,
        None,
        "UWorld::IsPaused",
        0x618DE3,
        "f30f108624040000",
        "loads",
    ),
    wn(
        "Engine.WorldInfo",
        "RealTimeSeconds",
        0x428,
        None,
        "UWorld::Tick",
        0x635841,
        "f30f118328040000",
        "stores",
    ),
    wn(
        "Engine.WorldInfo",
        "AudioTimeSeconds",
        0x42C,
        None,
        "UWorld::Tick",
        0x635863,
        "f30f11832c040000",
        "stores",
    ),
    wn(
        "Engine.WorldInfo",
        "DeltaSeconds",
        0x430,
        None,
        "UWorld::Tick",
        0x6358D4,
        "f30f118330040000",
        "stores",
    ),
    wn(
        "Engine.WorldInfo",
        "PauseDelay",
        0x434,
        None,
        "UWorld::IsPaused",
        0x618DEB,
        "0f2f8634040000",
        "compares",
    ),
    wn(
        "Engine.WorldInfo",
        "Pauser",
        0x43C,
        None,
        "UWorld::IsPaused",
        0x618DDA,
        "83be3c04000000",
        "compares",
    ),
    wn(
        "Engine.WorldInfo",
        "NetMode",
        0x47C,
        None,
        "UWorld::IsPaused",
        0x618E01,
        "80be7c04000003",
        "compares",
    ),
    wn(
        "Engine.WorldInfo",
        "ControllerList",
        0x4C4,
        None,
        "AWorldInfo::execAllControllers",
        0x7597C0,
        "8bbfc4040000",
        "loads",
    ),
    wn(
        "Engine.WorldInfo",
        "PawnList",
        0x4C8,
        None,
        "AWorldInfo::execAllPawns",
        0x759B21,
        "8bbfc8040000",
        "loads",
    ),
];

/// A field offset found a second time, by a route that shares nothing with
/// [`WIN32_NATIVE`]: the function is identified by a literal only it uses,
/// by the native function name table or by a direct call from such a
/// function (never by a vtable slot shift), and where `literals` is not
/// empty the instruction sits next to the executable's own use of the
/// member's name.
struct Win32Second {
    class: &'static str,
    field: &'static str,
    offset: u64,
    /// Entry of [`WIN32_FUNCTIONS`].
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    does: &'static str,
    /// UTF-16 literals of the executable (member name, class name) whose
    /// addresses are used within 0x80 bytes before or 0x40 bytes after the
    /// instruction.
    literals: &'static [&'static str],
}

#[allow(clippy::too_many_arguments)]
const fn ws(
    class: &'static str,
    field: &'static str,
    offset: u64,
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    does: &'static str,
    literals: &'static [&'static str],
) -> Win32Second {
    Win32Second {
        class,
        field,
        offset,
        function,
        rva,
        bytes,
        does,
        literals,
    }
}

/// What the engine's own member-offset check does with the offset.
const COMPILED: &str = "compares the property's linked offset with the compiled offset of";
/// What a replication function does right after looking the property up by name.
const REP_LOADS: &str = "after looking the property up by name, loads";
/// The same, for members compared through their address.
const REP_ADDRESS: &str = "after looking the property up by name, takes the address of";

/// Independent verification of the Win32 offsets (see [`Win32Second`]).
/// Three kinds of rows:
/// - `UGameEngine::Init` holds sixteen checks the engine makes of itself:
///   it finds a property by class and member name and compares the offset
///   the engine linked with an immediate, the offset the C++ compiler gave
///   that member;
/// - a replication function looks each replicated property up by its name
///   literal and compares the member of the actor with the same member of
///   its last replicated state;
/// - exec thunks from the native function name table (and functions they
///   call directly) that read a member on both builds.
const WIN32_SECOND: &[Win32Second] = &[
    ws(
        "Engine.Actor",
        "Owner",
        0x9C,
        "UGameEngine::Init",
        0x58_DFB6,
        "3d9c000000",
        COMPILED,
        &["Owner", "Actor"],
    ),
    ws(
        "Engine.PlayerController",
        "ViewTarget",
        0x374,
        "UGameEngine::Init",
        0x58_E11A,
        "3d74030000",
        COMPILED,
        &["ViewTarget", "PlayerController"],
    ),
    ws(
        "Engine.Pawn",
        "Health",
        0x2E0,
        "UGameEngine::Init",
        0x58_E288,
        "3de0020000",
        COMPILED,
        &["Health", "Pawn"],
    ),
    ws(
        "Engine.Texture",
        "UnpackMax",
        0x50,
        "UGameEngine::Init",
        0x58_E3EF,
        "83f850",
        COMPILED,
        &["UnpackMax", "Texture"],
    ),
    ws(
        "Engine.Sequence",
        "DefaultViewZoom",
        0x148,
        "UGameEngine::Init",
        0x58_E558,
        "3d48010000",
        COMPILED,
        &["DefaultViewZoom", "Sequence"],
    ),
    ws(
        "Engine.SequenceObject",
        "ObjInstanceVersion",
        0x3C,
        "UGameEngine::Init",
        0x58_E6BF,
        "83f83c",
        COMPILED,
        &["ObjInstanceVersion", "SequenceObject"],
    ),
    ws(
        "Engine.SequenceOp",
        "PlayerIndex",
        0xD4,
        "UGameEngine::Init",
        0x58_E81E,
        "3dd4000000",
        COMPILED,
        &["PlayerIndex", "SequenceOp"],
    ),
    ws(
        "Engine.SequenceAction",
        "HandlerName",
        0xE4,
        "UGameEngine::Init",
        0x58_E982,
        "3de4000000",
        COMPILED,
        &["HandlerName", "SequenceAction"],
    ),
    ws(
        "Engine.SeqAct_Latent",
        "LatentActors",
        0xFC,
        "UGameEngine::Init",
        0x58_EAEA,
        "3dfc000000",
        COMPILED,
        &["LatentActors", "SeqAct_Latent"],
    ),
    ws(
        "Engine.SeqAct_Interp",
        "PlayRate",
        0x188,
        "UGameEngine::Init",
        0x58_EC4E,
        "3d88010000",
        COMPILED,
        &["PlayRate", "SeqAct_Interp"],
    ),
    ws(
        "Engine.SeqAct_Interp",
        "RenderingOverrides",
        0x1D0,
        "UGameEngine::Init",
        0x58_EDB8,
        "3dd0010000",
        COMPILED,
        &["RenderingOverrides", "SeqAct_Interp"],
    ),
    ws(
        "Engine.PrimitiveComponent",
        "Tag",
        0x58,
        "UGameEngine::Init",
        0x58_EF28,
        "83f858",
        COMPILED,
        &["Tag", "PrimitiveComponent"],
    ),
    ws(
        "Engine.PrimitiveComponent",
        "LightingChannels",
        0x144,
        "UGameEngine::Init",
        0x58_F098,
        "3d44010000",
        COMPILED,
        &["LightingChannels", "PrimitiveComponent"],
    ),
    ws(
        "Engine.MeshComponent",
        "Materials",
        0x1D8,
        "UGameEngine::Init",
        0x58_F208,
        "3dd8010000",
        COMPILED,
        &["Materials", "MeshComponent"],
    ),
    ws(
        "Engine.SkeletalMeshComponent",
        "SkeletalMesh",
        0x1E4,
        "UGameEngine::Init",
        0x58_F378,
        "3de4010000",
        COMPILED,
        &["SkeletalMesh", "SkeletalMeshComponent"],
    ),
    ws(
        "Engine.SkeletalMesh",
        "RefBasesInvMatrix",
        0xF0,
        "UGameEngine::Init",
        0x58_F4E8,
        "3df0000000",
        COMPILED,
        &["RefBasesInvMatrix", "SkeletalMesh"],
    ),
    ws(
        "Engine.Actor",
        "Rotation",
        0x60,
        "AActor::GetOptimizedRepList",
        0x43_57ED,
        "8d4f60",
        REP_ADDRESS,
        &["Rotation"],
    ),
    ws(
        "Engine.Actor",
        "DrawScale",
        0x6C,
        "AActor::GetOptimizedRepList",
        0x43_5B85,
        "8b536c",
        REP_LOADS,
        &["DrawScale"],
    ),
    ws(
        "Engine.Actor",
        "Physics",
        0x94,
        "AActor::GetOptimizedRepList",
        0x43_55A7,
        "8a9694000000",
        REP_LOADS,
        &["Physics"],
    ),
    ws(
        "Engine.Actor",
        "RemoteRole",
        0x95,
        "AActor::GetOptimizedRepList",
        0x43_622F,
        "8a8a95000000",
        REP_LOADS,
        &["RemoteRole"],
    ),
    ws(
        "Engine.Actor",
        "Role",
        0x96,
        "AActor::GetOptimizedRepList",
        0x43_6158,
        "8a8a96000000",
        REP_LOADS,
        &["Role"],
    ),
    ws(
        "Engine.Actor",
        "ReplicatedCollisionType",
        0x98,
        "AActor::GetOptimizedRepList",
        0x43_5D0F,
        "8a9398000000",
        REP_LOADS,
        &["ReplicatedCollisionType"],
    ),
    ws(
        "Engine.Actor",
        "Owner",
        0x9C,
        "AActor::GetOptimizedRepList",
        0x43_5F2F,
        "8b919c000000",
        REP_LOADS,
        &["Owner"],
    ),
    ws(
        "Engine.Actor",
        "Base",
        0xA0,
        "AActor::GetOptimizedRepList",
        0x43_51A4,
        "8b82a0000000",
        REP_LOADS,
        &["Base"],
    ),
    ws(
        "Engine.Actor",
        "Instigator",
        0xD8,
        "AActor::GetOptimizedRepList",
        0x43_607D,
        "8b82d8000000",
        REP_LOADS,
        &["Instigator"],
    ),
    ws(
        "Engine.Actor",
        "Velocity",
        0x138,
        "AActor::GetOptimizedRepList",
        0x43_5939,
        "8d8f38010000",
        REP_ADDRESS,
        &["Velocity"],
    ),
    ws(
        "Engine.Actor",
        "RelativeLocation",
        0x174,
        "AActor::GetOptimizedRepList",
        0x43_52C9,
        "8d8f74010000",
        REP_ADDRESS,
        &["RelativeLocation"],
    ),
    ws(
        "Engine.Actor",
        "RelativeRotation",
        0x180,
        "AActor::GetOptimizedRepList",
        0x43_5352,
        "8d8f80010000",
        REP_ADDRESS,
        &["RelativeRotation"],
    ),
    ws(
        "Engine.Pawn",
        "Controller",
        0x1EC,
        "APawn::GetOptimizedRepList",
        0x43_6DD2,
        "8bbbec010000",
        REP_LOADS,
        &["Controller"],
    ),
    ws(
        "Engine.Pawn",
        "RemoteViewPitch",
        0x20A,
        "APawn::GetOptimizedRepList",
        0x43_689A,
        "8a930a020000",
        REP_LOADS,
        &["RemoteViewPitch"],
    ),
    ws(
        "Engine.Pawn",
        "GroundSpeed",
        0x28C,
        "APawn::GetOptimizedRepList",
        0x43_70FC,
        "8b938c020000",
        REP_LOADS,
        &["GroundSpeed"],
    ),
    ws(
        "Engine.Pawn",
        "WaterSpeed",
        0x290,
        "APawn::GetOptimizedRepList",
        0x43_71B9,
        "8b9390020000",
        REP_LOADS,
        &["WaterSpeed"],
    ),
    ws(
        "Engine.Pawn",
        "AirSpeed",
        0x294,
        "APawn::GetOptimizedRepList",
        0x43_7270,
        "8b9394020000",
        REP_LOADS,
        &["AirSpeed"],
    ),
    ws(
        "Engine.Pawn",
        "AccelRate",
        0x29C,
        "APawn::GetOptimizedRepList",
        0x43_7327,
        "8b939c020000",
        REP_LOADS,
        &["AccelRate"],
    ),
    ws(
        "Engine.Pawn",
        "JumpZ",
        0x2A0,
        "APawn::GetOptimizedRepList",
        0x43_73DE,
        "8b93a0020000",
        REP_LOADS,
        &["JumpZ"],
    ),
    ws(
        "Engine.Pawn",
        "AirControl",
        0x2AC,
        "APawn::GetOptimizedRepList",
        0x43_7495,
        "8b93ac020000",
        REP_LOADS,
        &["AirControl"],
    ),
    ws(
        "Engine.Pawn",
        "Health",
        0x2E0,
        "APawn::GetOptimizedRepList",
        0x43_6E9A,
        "8b93e0020000",
        REP_LOADS,
        &["Health"],
    ),
    ws(
        "Engine.Pawn",
        "HealthMax",
        0x2E4,
        "APawn::GetOptimizedRepList",
        0x43_67C1,
        "8b8be4020000",
        REP_LOADS,
        &["HealthMax"],
    ),
    ws(
        "Engine.Pawn",
        "PlayerReplicationInfo",
        0x34C,
        "APawn::GetOptimizedRepList",
        0x43_6BA8,
        "8bbb4c030000",
        REP_LOADS,
        &["PlayerReplicationInfo"],
    ),
    ws(
        "Engine.Pawn",
        "InvManager",
        0x3C4,
        "APawn::GetOptimizedRepList",
        0x43_7552,
        "8bbbc4030000",
        REP_LOADS,
        &["InvManager"],
    ),
    ws(
        "Engine.Pawn",
        "FlashLocation",
        0x3CC,
        "APawn::GetOptimizedRepList",
        0x43_7927,
        "8b93cc030000",
        REP_LOADS,
        &["FlashLocation"],
    ),
    ws(
        "Engine.Controller",
        "Pawn",
        0x1D0,
        "AController::GetOptimizedRepList",
        0x44_3A7B,
        "8bb0d0010000",
        REP_LOADS,
        &["Pawn"],
    ),
    ws(
        "Engine.Controller",
        "PlayerReplicationInfo",
        0x1D4,
        "AController::GetOptimizedRepList",
        0x44_3990,
        "8bb2d4010000",
        REP_LOADS,
        &["PlayerReplicationInfo"],
    ),
    ws(
        "Engine.PlayerController",
        "TargetViewRotation",
        0x394,
        "APlayerController::GetOptimizedRepList",
        0x44_B8FD,
        "8d8f94030000",
        REP_ADDRESS,
        &["TargetViewRotation"],
    ),
    ws(
        "Engine.PlayerController",
        "TargetEyeHeight",
        0x3A0,
        "APlayerController::GetOptimizedRepList",
        0x44_B96F,
        "8b97a0030000",
        REP_LOADS,
        &["TargetEyeHeight"],
    ),
    ws(
        "Engine.WorldInfo",
        "TimeDilation",
        0x41C,
        "AWorldInfo::GetOptimizedRepList",
        0x44_C829,
        "8b8e1c040000",
        REP_LOADS,
        &["TimeDilation"],
    ),
    ws(
        "Engine.WorldInfo",
        "Pauser",
        0x43C,
        "AWorldInfo::GetOptimizedRepList",
        0x44_C747,
        "8bb23c040000",
        REP_LOADS,
        &["Pauser"],
    ),
    ws(
        "Engine.WorldInfo",
        "WorldGravityZ",
        0x4B0,
        "AWorldInfo::GetOptimizedRepList",
        0x44_C8DD,
        "8b8eb0040000",
        REP_LOADS,
        &["WorldGravityZ"],
    ),
    ws(
        "Core.Object",
        "ObjectInternalInteger",
        0x20,
        "UObject::execGetFuncName",
        0x18_D1CA,
        "837e20ff",
        "compares with -1 (no index: the name is not valid) the object index",
        &[],
    ),
    ws(
        "Core.Object",
        "ObjectInternalInteger",
        0x20,
        "UObject::AddObject",
        0x1F_4140,
        "897720",
        "stores the slot it gave the object in UObject::GObjObjects to",
        &[],
    ),
    ws(
        "Core.Object",
        "Name",
        0x2C,
        "UObject::execGetFuncName",
        0x18_D1D0,
        "8b4e2c",
        "loads",
        &[],
    ),
    ws(
        "Core.Object",
        "Class",
        0x34,
        "AActor::execAllActors",
        0x76_35B6,
        "8b4734",
        "loads",
        &[],
    ),
    ws(
        "Engine.Actor",
        "Location",
        0x54,
        "AActor::execFastTrace",
        0x75_766D,
        "f30f7e4354",
        "loads (the default of the optional TraceStart argument)",
        &[],
    ),
    ws(
        "Engine.Actor",
        "Owner",
        0x9C,
        "AActor::execIsOwnedBy",
        0x75_8187,
        "8b809c000000",
        "loads",
        &[],
    ),
    ws(
        "Engine.Actor",
        "Base",
        0xA0,
        "AActor::execIsBasedOn",
        0x75_8107,
        "8b80a0000000",
        "loads",
        &[],
    ),
    ws(
        "Engine.Controller",
        "Pawn",
        0x1D0,
        "AController::execMoveTo",
        0x4E_5CDE,
        "8b87d0010000",
        "loads",
        &[],
    ),
    ws(
        "Engine.Engine",
        "Client",
        0x4B0,
        "UEngine::execGetAudioDevice",
        0x53_B72E,
        "8b88b0040000",
        "loads",
        &[],
    ),
    ws(
        "Engine.Engine",
        "GamePlayers",
        0x4B4,
        "AActor::GetALocalPlayerController",
        0x75_D446,
        "8db9b4040000",
        "takes the address of",
        &[],
    ),
    ws(
        "Engine.Player",
        "Actor",
        0x40,
        "AActor::GetALocalPlayerController",
        0x75_D51B,
        "8b4240",
        "loads",
        &[],
    ),
    ws(
        "Engine.Input",
        "Bindings",
        0x78,
        "UInput::Exec",
        0x92_1F8F,
        "8b4778",
        "loads",
        &[],
    ),
    ws(
        "Engine.Input",
        "PressedKeys",
        0x84,
        "UInput::InputKey",
        0x91_776D,
        "8b9684000000",
        "loads",
        &[],
    ),
];

/// A native structure member shown by one Win32 instruction.
struct Win32StructEvidence {
    /// Recorder structure name (`TArray`, `FName`, `FNameEntry`, `KeyBind`).
    structure: &'static str,
    what: &'static str,
    /// Entry of [`WIN32_FUNCTIONS`].
    function: &'static str,
    rva: u32,
    bytes: &'static str,
}

const fn se(
    structure: &'static str,
    what: &'static str,
    function: &'static str,
    rva: u32,
    bytes: &'static str,
) -> Win32StructEvidence {
    Win32StructEvidence {
        structure,
        what,
        function,
        rva,
        bytes,
    }
}

/// Members of the native structures the recorder reads, as Win32
/// instructions show them.
const WIN32_STRUCT_EVIDENCE: &[Win32StructEvidence] = &[
    se(
        "TArray",
        "count at +4: Engine.GamePlayers starts at +0x4B4 and the loop bound is the word at +0x4B8",
        "UGameEngine::Tick",
        0x0058_98C5,
        "39aeb8040000",
    ),
    se(
        "TArray",
        "count at +4: Input.PressedKeys starts at +0x84 and its count is the word at +0x88",
        "UInput::GetBind",
        0x0091_578D,
        "8bbb88000000",
    ),
    se(
        "FName",
        "index is the first word: loaded from +0, then compared with the count of FName::Names",
        "FName validity test",
        0x0031_A3E0,
        "8b01",
    ),
    se(
        "FName",
        "two words: after the index at +0 the number at +4 is compared (PressedKeys elements, 8 bytes apart)",
        "UInput::GetBind",
        0x0091_57C6,
        "395004",
    ),
    se(
        "FNameEntry",
        "wide flag = bit 0 of the word at +8",
        "FName lookup",
        0x001D_4777,
        "f6460801",
    ),
    se(
        "FNameEntry",
        "characters at +0x10 (the wide form is compared in place)",
        "FName lookup",
        0x001D_477D,
        "8d4610",
    ),
    se(
        "KeyBind",
        "24-byte elements: the element address is data + 8 * (3 * index)",
        "UInput::GetBind",
        0x0091_58EC,
        "8d0c76",
    ),
    se(
        "KeyBind",
        "modifier flags word at +0x14, after Name (+0, 8 bytes) and Command (+8, 12 bytes)",
        "UInput::GetBind",
        0x0091_5901,
        "8b4014",
    ),
    se(
        "KeyBind",
        "Command at +8: the address of the string copied into the result",
        "UInput::GetBind",
        0x0091_59A4,
        "8d4cd008",
    ),
    // ---- independent verification
    se(
        "TArray",
        "count at +4: FName::SafeString compares a name index with the word after the data pointer of FName::Names",
        "FName::SafeString",
        0x001E_2112,
        "3b05b4f4aa02",
    ),
    se(
        "TArray",
        "count at +4: Input.PressedKeys starts at +0x84 and UInput::InputKey loads its count from +0x88",
        "UInput::InputKey",
        0x0091_7773,
        "8b8688000000",
    ),
    se(
        "FName",
        "number at +4: FName::AppendString tests it and, when it is not zero, appends '_' and the number minus 1",
        "FName::AppendString",
        0x001D_5985,
        "837f0400",
    ),
    se(
        "FNameEntry",
        "wide flag = bit 0 of the word at +8 (FName::ToString)",
        "FName::ToString(FString&)",
        0x001D_73BF,
        "f6460801",
    ),
    se(
        "FNameEntry",
        "characters at +0x10 in both forms: FName::ToString measures the name from there in 2-byte or 1-byte steps",
        "FName::ToString(FString&)",
        0x001D_73C3,
        "8d4610",
    ),
    se(
        "FString",
        "2-byte characters: FName::SafeString allocates twice the character count for a copy (the Mac build shifts by 2)",
        "FName::SafeString",
        0x001E_217A,
        "8d3400",
    ),
];

/// The engine's reflection objects, as `UGameEngine::Init` walks them in
/// its member-offset checks. The recorder does not read them; a live check
/// can: every script property is a `UProperty` whose `Offset` is the offset
/// the engine itself linked for that field.
const WIN32_REFLECTION_EVIDENCE: &[Win32StructEvidence] = &[
    se(
        "UStruct",
        "Children at +0x4C: the first field of the class the property walk starts from",
        "UGameEngine::Init",
        0x0058_DF02,
        "8b504c",
    ),
    se(
        "UProperty",
        "Offset at +0x60: loaded and compared with the compiled offset of the member",
        "UGameEngine::Init",
        0x0058_DFB3,
        "8b4760",
    ),
    se(
        "UField",
        "Next at +0x3C: the walk steps to the next field of the class",
        "UGameEngine::Init",
        0x0058_DFE6,
        "8b573c",
    ),
];

/// An instruction that addresses a global of the recorder absolutely: its
/// bytes contain image base + RVA of the symbol (the file holds the
/// preferred-base address; the loader relocates it).
struct Win32SymbolEvidence {
    symbol: &'static str,
    what: &'static str,
    /// Entry of [`WIN32_FUNCTIONS`].
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    /// The instruction must come before the function's call to this symbol.
    before_call_to: Option<&'static str>,
}

const fn sy(
    symbol: &'static str,
    what: &'static str,
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    before_call_to: Option<&'static str>,
) -> Win32SymbolEvidence {
    Win32SymbolEvidence {
        symbol,
        what,
        function,
        rva,
        bytes,
        before_call_to,
    }
}

/// Where the frame loop reads or writes each global the recorder resolves.
const WIN32_SYMBOL_EVIDENCE: &[Win32SymbolEvidence] = &[
    sy(
        "GEngine",
        "UWorld::Tick loads GEngine for its GamePlayers loop",
        "UWorld::Tick",
        0x0063_5485,
        "8b0d0889b002",
        None,
    ),
    sy(
        "GWorld",
        "UGameEngine::Tick loads GWorld as this (ecx) of its single UWorld::Tick call",
        "UGameEngine::Tick",
        0x0058_98B1,
        "8b0d90c4b002",
        Some("UWorld::Tick"),
    ),
    sy(
        "GFrameCounter",
        "FEngineLoop::Tick stores the low half of the incremented 64-bit GFrameCounter",
        "FEngineLoop::Tick",
        0x014A_8F1E,
        "a320faa402",
        None,
    ),
    sy(
        "GIsBenchmarking",
        "the time update tests GIsBenchmarking first",
        "appUpdateTimeAndHandleMaxTickRate",
        0x014A_6876,
        "393d8c71a402",
        None,
    ),
    sy(
        "GUseFixedTimeStep",
        "the time update tests GUseFixedTimeStep second",
        "appUpdateTimeAndHandleMaxTickRate",
        0x014A_687E,
        "393d64faa402",
        None,
    ),
    sy(
        "GFixedDeltaTime",
        "the time update loads the double GFixedDeltaTime in fixed-step mode",
        "appUpdateTimeAndHandleMaxTickRate",
        0x014A_68A0,
        "f20f1005687e9a02",
        None,
    ),
    sy(
        "GDeltaTime",
        "the time update stores it to the double GDeltaTime",
        "appUpdateTimeAndHandleMaxTickRate",
        0x014A_68A8,
        "f20f1105707e9a02",
        None,
    ),
    sy(
        "FName::Names",
        "the name-table validity test loads the data pointer of FName::Names",
        "FName validity test",
        0x0031_A3EE,
        "8b0db0f4aa02",
        None,
    ),
    // ---- independent verification: other functions, identified without
    // the frame loop.
    sy(
        "GEngine",
        "UEngine::execGetEngine returns GEngine",
        "UEngine::execGetEngine",
        0x0053_B833,
        "8b0d0889b002",
        None,
    ),
    sy(
        "GEngine",
        "AActor::GetALocalPlayerController loads GEngine to walk its GamePlayers",
        "AActor::GetALocalPlayerController",
        0x0075_D430,
        "8b0d0889b002",
        None,
    ),
    sy(
        "GWorld",
        "UEngine::execGetCurrentWorldInfo loads GWorld as this of UWorld::GetWorldInfo",
        "UEngine::execGetCurrentWorldInfo",
        0x0053_B4DF,
        "8b0d90c4b002",
        None,
    ),
    sy(
        "GFrameCounter",
        "FDynamicLightEnvironmentState::Tick loads the low half of GFrameCounter for a 64-bit modulo 10",
        "FDynamicLightEnvironmentState::Tick",
        0x002B_1207,
        "a120faa402",
        None,
    ),
    sy(
        "GDeltaTime",
        "UAudioDevice::GetSortedActiveWaveInstances loads the double GDeltaTime and narrows it to float",
        "UAudioDevice::GetSortedActiveWaveInstances",
        0x004A_0A31,
        "f20f1005707e9a02",
        None,
    ),
    sy(
        "GFixedDeltaTime",
        "the Matinee editor's fixed-time-step setter stores a float widened to double to GFixedDeltaTime",
        "Matinee editor: fixed-time-step setter",
        0x0121_C52A,
        "f20f1105687e9a02",
        None,
    ),
    sy(
        "GIsBenchmarking",
        "the Matinee editor's fixed-time-step setter stores 1 to GIsBenchmarking",
        "Matinee editor: fixed-time-step setter",
        0x0121_C509,
        "c7058c71a40201000000",
        None,
    ),
    sy(
        "GIsBenchmarking",
        "FEngineLoop::Exit tests GIsBenchmarking before it writes the benchmark log",
        "FEngineLoop::Exit",
        0x014A_6CE2,
        "392d8c71a402",
        None,
    ),
    sy(
        "GUseFixedTimeStep",
        "DrawUnitTimes tests GUseFixedTimeStep right after GIsBenchmarking",
        "DrawUnitTimes",
        0x0055_695B,
        "393d64faa402",
        None,
    ),
    sy(
        "FName::Names",
        "FName::SafeString loads the data pointer of FName::Names after comparing the index with its count",
        "FName::SafeString",
        0x001E_211A,
        "8b0db0f4aa02",
        None,
    ),
];

/// An instruction that shows how `UWorld::Tick` receives its arguments.
struct Win32AbiEvidence {
    what: &'static str,
    /// Entry of [`WIN32_FUNCTIONS`].
    function: &'static str,
    rva: u32,
    bytes: &'static str,
    before_call_to: Option<&'static str>,
}

/// The sample point's calling convention: the breakpoint front end reads
/// `DeltaSeconds` from `[esp+8]` at the entry of `UWorld::Tick`.
const WIN32_ABI_EVIDENCE: &[Win32AbiEvidence] = &[
    Win32AbiEvidence {
        what: "UGameEngine::Tick writes DeltaSeconds into the stack slot it reserved; after the next push and the call's return address that slot is [esp+8] at the entry of UWorld::Tick",
        function: "UGameEngine::Tick",
        rva: 0x0058_98B7,
        bytes: "f30f110424",
        before_call_to: Some("UWorld::Tick"),
    },
    Win32AbiEvidence {
        what: "UGameEngine::Tick pushes the tick type 2 (LEVELTICK_All) last: [esp+4] at the entry of UWorld::Tick",
        function: "UGameEngine::Tick",
        rva: 0x0058_98BC,
        bytes: "6a02",
        before_call_to: Some("UWorld::Tick"),
    },
    Win32AbiEvidence {
        what: "UWorld::Tick returns with ret 8: two 4-byte stack arguments, this in ecx (thiscall)",
        function: "UWorld::Tick",
        rva: 0x0063_6DC5,
        bytes: "c20800",
        before_call_to: None,
    },
];

fn win32_function(name: &str) -> Result<&'static Win32Function> {
    WIN32_FUNCTIONS
        .iter()
        .find(|f| f.name == name)
        .with_context(|| format!("no Win32 function entry {name}"))
}

/// The field `class.name` (declared by `class` itself) in the computed layout.
fn own_field(lay: &mut Layouter<'_>, class: &str, name: &str) -> Result<Field> {
    let l = lay.layout(class)?;
    let def = lay
        .set
        .struct_def(class)
        .with_context(|| format!("no class {class}"))?;
    l.fields
        .iter()
        .find(|f| f.owner.eq_ignore_ascii_case(&def.path) && f.name == name)
        .cloned()
        .with_context(|| format!("{class} declares no field {name}"))
}

/// One step of a dotted recorder field: the class or struct that declares
/// the member, the member and its offset inside that container.
type MemberStep = (String, String, u64);

/// Offset, kind, size and bit of a recorder field: a property of `class`,
/// or a dotted path through struct members (`CameraCache.POV.FOV`). Also
/// returns the steps of the path.
fn resolve_field(
    lay: &mut Layouter<'_>,
    class: &str,
    dotted: &str,
) -> Result<(Field, Vec<MemberStep>)> {
    let mut parts = dotted.split('.');
    let first = parts.next().unwrap_or(dotted);
    let mut f = own_field(lay, class, first)?;
    let mut path = vec![(f.owner.clone(), first.to_owned(), f.offset)];
    for p in parts {
        let sp = f
            .struct_path
            .clone()
            .with_context(|| format!("{class}.{dotted}: {} is not a struct", f.name))?;
        let sl = lay.layout(&sp)?;
        let container = lay
            .set
            .struct_def(&sp)
            .with_context(|| format!("no struct {sp}"))?
            .path
            .clone();
        let m = sl
            .fields
            .iter()
            .find(|m| m.name == p)
            .cloned()
            .with_context(|| format!("{sp} has no member {p}"))?;
        path.push((container, p.to_owned(), m.offset));
        f = Field {
            offset: f.offset.saturating_add(m.offset),
            ..m
        };
    }
    Ok((f, path))
}

/// Check [`WIN32_NATIVE`] against the computed layout: returns the number of
/// rows whose field is at the offset (and bit) the instruction shows, and a
/// description of every other row.
fn win32_native_check(lay: &mut Layouter<'_>) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut bad = Vec::new();
    for r in WIN32_NATIVE {
        match own_field(lay, r.class, r.field) {
            Ok(f) if f.offset == r.offset && f.bit == r.bit => ok += 1,
            Ok(f) => bad.push(format!(
                "{}.{}: rules {} bit {:?}, native code {} bit {:?} ({})",
                r.class,
                r.field,
                hex(f.offset),
                f.bit,
                hex(r.offset),
                r.bit,
                r.function
            )),
            Err(e) => bad.push(format!("{}.{}: {e:#}", r.class, r.field)),
        }
    }
    (ok, bad)
}

/// Check [`WIN32_SECOND`] against the computed layout, like
/// [`win32_native_check`] (none of its rows is a bool).
fn win32_second_check(lay: &mut Layouter<'_>) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut bad = Vec::new();
    for r in WIN32_SECOND {
        match own_field(lay, r.class, r.field) {
            Ok(f) if f.offset == r.offset && f.bit.is_none() => ok += 1,
            Ok(f) => bad.push(format!(
                "{}.{}: rules {} bit {:?}, native code {} ({})",
                r.class,
                r.field,
                hex(f.offset),
                f.bit,
                hex(r.offset),
                r.function
            )),
            Err(e) => bad.push(format!("{}.{}: {e:#}", r.class, r.field)),
        }
    }
    (ok, bad)
}

/// One native class of the Win32 executable
/// (`docs/reverse-engineering/data/win32/class_sizes.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Win32Class {
    package: String,
    name: String,
    cpp: String,
    size: u64,
}

impl Win32Class {
    fn path(&self) -> String {
        format!("{}.{}", self.package, self.name)
    }
}

/// Parses the columnar `class_sizes.json` of the Win32 binary analysis.
fn parse_win32_classes(text: &str) -> Result<Vec<Win32Class>> {
    let j: Json = serde_json::from_str(text).context("class_sizes.json is not JSON")?;
    let columns: Vec<&str> = j["columns"]
        .as_array()
        .context("class_sizes.json has no columns")?
        .iter()
        .filter_map(Json::as_str)
        .collect();
    let col = |name: &str| {
        columns
            .iter()
            .position(|c| *c == name)
            .with_context(|| format!("class_sizes.json has no column {name}"))
    };
    let (package, name, cpp, size) = (
        col("package")?,
        col("name")?,
        col("cpp_name")?,
        col("size")?,
    );
    let mut out = Vec::new();
    for row in j["classes"]
        .as_array()
        .context("class_sizes.json has no classes")?
    {
        let text = |i: usize| row.get(i).and_then(Json::as_str).map(str::to_owned);
        let (Some(package), Some(name), Some(cpp), Some(size)) = (
            text(package),
            text(name),
            text(cpp),
            row.get(size).and_then(Json::as_u64),
        ) else {
            bail!("class_sizes.json: malformed row {row}");
        };
        out.push(Win32Class {
            package,
            name,
            cpp,
            size,
        });
    }
    Ok(out)
}

/// Result of comparing the computed Win32 layout with the native sizes.
#[derive(Debug, Default)]
struct Win32SizeCheck {
    /// Native script classes compared.
    compared: usize,
    /// Of those, classes whose computed size equals the native `sizeof`.
    equal: usize,
    /// `(script path, C++ name, computed, native)` of the others.
    different: Vec<(String, String, u64, u64)>,
    /// Native script classes without a registration in the executable, per
    /// package file.
    script_only: BTreeMap<String, usize>,
    /// Registered classes without a script class, per package.
    native_only: BTreeMap<String, usize>,
    /// Script classes that are not native (laid out by the same rules, but
    /// no native size exists to compare with).
    nonnative: usize,
}

/// Compare the computed layout size of every native script class with the
/// `sizeof` its Win32 registration passes. Classes pair by script path
/// (`Package.Name`), which the registration carries.
fn win32_class_size_check(
    lay: &mut Layouter<'_>,
    classes: &[(String, String, bool)],
    native: &[Win32Class],
) -> Result<Win32SizeCheck> {
    let mut by_path: HashMap<String, &Win32Class> = HashMap::new();
    for c in native {
        by_path.insert(c.path().to_ascii_lowercase(), c);
    }
    let mut r = Win32SizeCheck::default();
    let mut seen = std::collections::HashSet::new();
    for (file, path, is_native) in classes {
        let key = path.to_ascii_lowercase();
        let Some(c) = by_path.get(&key) else {
            if *is_native {
                *r.script_only.entry(file.clone()).or_insert(0) += 1;
            } else {
                r.nonnative += 1;
            }
            continue;
        };
        seen.insert(key);
        let computed = lay
            .layout(path)
            .with_context(|| format!("layout of {path}"))?
            .size;
        r.compared += 1;
        if computed == c.size {
            r.equal += 1;
        } else {
            r.different
                .push((path.clone(), c.cpp.clone(), computed, c.size));
        }
    }
    for c in native {
        if !seen.contains(&c.path().to_ascii_lowercase()) {
            *r.native_only.entry(c.package.clone()).or_insert(0) += 1;
        }
    }
    Ok(r)
}

/// Position of every field a script class declares itself:
/// `(class path, field) -> (offset, bit)`.
type FieldPositions = BTreeMap<(String, String), (u64, Option<u32>)>;

/// The [`FieldPositions`] of all script classes, native and script-only, so
/// two rule sets can be compared field by field.
fn win32_snapshot(
    lay: &mut Layouter<'_>,
    classes: &[(String, String, bool)],
) -> Result<FieldPositions> {
    let mut out = BTreeMap::new();
    for (_, path, _) in classes {
        let l = lay
            .layout(path)
            .with_context(|| format!("layout of {path}"))?;
        for f in l
            .fields
            .iter()
            .filter(|f| f.owner.eq_ignore_ascii_case(path))
        {
            out.insert((path.clone(), f.name.clone()), (f.offset, f.bit));
        }
    }
    Ok(out)
}

/// What one ablated rule changes.
struct Win32Ablation {
    rule: &'static str,
    instead: &'static str,
    class_sizes_wrong: usize,
    native_offsets_wrong: usize,
    /// Rows of [`WIN32_SECOND`] the ablated rules get wrong.
    second_offsets_wrong: usize,
    /// Declared fields (all script classes) whose offset or bit differs
    /// from the full rules.
    fields_moved: usize,
}

/// Runs the full Win32 validation: the class sizes, the native offsets and
/// one ablated run per rule.
struct Win32Validation {
    sizes: Win32SizeCheck,
    native_ok: usize,
    native_bad: Vec<String>,
    /// Rows of [`WIN32_SECOND`] at the rule offset, and the others.
    second_ok: usize,
    second_bad: Vec<String>,
    ablations: Vec<Win32Ablation>,
    /// Declared fields over all script classes.
    fields_total: usize,
    registered: usize,
}

fn win32_validate(
    set: &PackageSet,
    cooked: &Path,
    native: &[Win32Class],
    with_ablations: bool,
) -> Result<Win32Validation> {
    let classes = script_classes(set, cooked)?;
    let mut lay = Layouter::for_target(set, Target::Win32);
    let sizes = win32_class_size_check(&mut lay, &classes, native)?;
    let (native_ok, native_bad) = win32_native_check(&mut lay);
    let (second_ok, second_bad) = win32_second_check(&mut lay);
    let base = win32_snapshot(&mut lay, &classes)?;
    let mut ablations = Vec::new();
    if with_ablations {
        for (rule, instead) in WIN32_ABLATION_NOTES {
            let mut ab = Layouter::for_target(set, Target::Win32);
            ab.ablate = vec![(*rule).to_owned()];
            let s = win32_class_size_check(&mut ab, &classes, native)?;
            let (ok, _) = win32_native_check(&mut ab);
            let (second, _) = win32_second_check(&mut ab);
            let snap = win32_snapshot(&mut ab, &classes)?;
            let moved = base
                .iter()
                .filter(|(k, v)| snap.get(*k) != Some(*v))
                .count();
            ablations.push(Win32Ablation {
                rule,
                instead,
                class_sizes_wrong: s.different.len(),
                native_offsets_wrong: WIN32_NATIVE.len() - ok,
                second_offsets_wrong: WIN32_SECOND.len() - second,
                fields_moved: moved,
            });
        }
    }
    Ok(Win32Validation {
        sizes,
        native_ok,
        native_bad,
        second_ok,
        second_bad,
        ablations,
        fields_total: base.len(),
        registered: native.len(),
    })
}

fn locate_text(l: Locate) -> String {
    match l {
        Locate::Function(n) => format!("functions.json: {n}"),
        Locate::Exec(n) => format!("native function name table: {n}"),
        Locate::Vtable(c, s) => format!("vtable of {c}, slot {s} (+{:#X})", s * 4),
        Locate::TailOf(c, s) => format!("tail-jump target of vtable of {c}, slot {s}"),
        Locate::Literal(t) => format!("only user of the UTF-16 literal {t:?}"),
        Locate::CalledBy(f) => format!("called directly by {f}"),
        Locate::Scan => "instruction sequence (native_evidence.json)".to_owned(),
    }
}

fn rva_hex(v: u32) -> String {
    format!("0x{v:08X}")
}

/// The document written to `native_layout_win32.json`.
fn win32_layout_document(
    set: &PackageSet,
    v: &Win32Validation,
    native: &[Win32Class],
    dotted: &[(String, String)],
) -> Result<Json> {
    let mut lay = Layouter::for_target(set, Target::Win32);
    let mut member_paths = Map::new();
    for (class, name) in dotted {
        let (f, path) = resolve_field(&mut lay, class, name)?;
        let steps: Vec<Json> = path
            .iter()
            .map(|(container, member, offset)| json!([container, member, hex(*offset)]))
            .collect();
        member_paths.insert(
            format!("{class}.{name}"),
            json!({"offset": hex(f.offset), "steps": steps}),
        );
    }
    let classes: Vec<&str> = LAYOUTS
        .iter()
        .chain(WIN32_EXTRA_LAYOUTS.iter())
        .copied()
        .collect();
    let layout = layout_json(&mut lay, &classes)?;
    let mut structs = Map::new();
    for s in WIN32_STRUCTS {
        let l = lay.layout(s)?;
        let def = set
            .struct_def(s)
            .with_context(|| format!("no struct {s}"))?;
        let fields: Vec<Json> = l
            .fields
            .iter()
            .map(|f| {
                json!([
                    hex(f.offset),
                    f.name,
                    f.kind.trim_end_matches("Property"),
                    f.size,
                    f.bit,
                    f.array_dim
                ])
            })
            .collect();
        structs.insert(
            def.path.clone(),
            json!({"size": l.size, "align": l.align, "fields": fields}),
        );
    }
    let by_path: HashMap<String, u64> = native
        .iter()
        .map(|c| (c.path().to_ascii_lowercase(), c.size))
        .collect();
    let mut sizeof = Map::new();
    for c in &classes {
        let def = set.struct_def(c).with_context(|| format!("no class {c}"))?;
        if let Some(n) = by_path.get(&def.path.to_ascii_lowercase()) {
            let computed = lay.layout(c)?.size;
            if computed != *n {
                bail!(
                    "{c}: computed size {} differs from the native sizeof {}",
                    hex(computed),
                    hex(*n)
                );
            }
            sizeof.insert(def.path.clone(), json!(hex(*n)));
        }
    }
    let functions: Vec<Json> = WIN32_FUNCTIONS
        .iter()
        .map(|f| {
            json!([
                f.name,
                locate_text(f.locate),
                if f.rva == 0 {
                    Json::Null
                } else {
                    json!(rva_hex(f.rva))
                },
                f.identified
            ])
        })
        .collect();
    let mut offsets = Vec::new();
    for r in WIN32_NATIVE {
        win32_function(r.function)?;
        offsets.push(json!([
            r.class,
            r.field,
            hex(r.offset),
            r.bit,
            r.function,
            rva_hex(r.rva),
            r.does
        ]));
    }
    // The second, independent route: one row per instruction, and the
    // rule-derived row of every field whose class is not listed in full.
    let mut second = Vec::new();
    let mut other_fields: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for r in WIN32_SECOND {
        win32_function(r.function)?;
        second.push(json!([
            r.class,
            r.field,
            hex(r.offset),
            r.function,
            rva_hex(r.rva),
            r.does,
            r.literals
        ]));
        if !classes.contains(&r.class) {
            let f = own_field(&mut lay, r.class, r.field)?;
            let row = json!([
                hex(f.offset),
                f.name,
                f.kind.trim_end_matches("Property"),
                f.size,
                f.bit,
                f.array_dim
            ]);
            let rows = other_fields.entry(r.class.to_owned()).or_default();
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    let other_fields: Map<String, Json> = other_fields
        .into_iter()
        .map(|(class, rows)| (class, json!({"fields": rows})))
        .collect();
    let ablations: Vec<Json> = v
        .ablations
        .iter()
        .map(|a| {
            json!([
                a.rule,
                a.instead,
                a.class_sizes_wrong,
                a.native_offsets_wrong,
                a.second_offsets_wrong,
                a.fields_moved
            ])
        })
        .collect();
    let different: Vec<Json> = v
        .sizes
        .different
        .iter()
        .map(|(path, cpp, computed, n)| json!([path, cpp, hex(*computed), hex(*n)]))
        .collect();
    Ok(json!({
        "schema": WIN32_LAYOUT_SCHEMA,
        "build": WIN32_BUILD,
        "game_build": WIN32_GAME_BUILD,
        "method": "script property layout (the cooked script packages are byte-identical on Mac and Windows) laid out with the Win32 rules below; checked against the sizeof of every native class registered by the Win32 executable (class_sizes.json) and against field offsets read from its instructions (native_offsets)",
        "rules": {
            "1_elements": "byte 1/1; int, float and bool word 4/4; name 8/4; object, class and component pointers 4/4; string and dynamic array 12/4; interface 8/4; delegate 12/4; map = Core.Object.Map_Mirror (60 bytes)",
            "2_native_mirrors": "Core.Object.Pointer is 4/4; QWord and Double are 8 bytes aligned to 4 (the engine headers are compiled under #pragma pack(4)); all three equal their script declarations on this build",
            "3_bools": "consecutive bool properties of one class or struct share a 32-bit word, bit 0 first, up to 32 bits; a non-bool property, a static array or the start of a new class begins a new word",
            "4_parent": "a class continues at its parent's unpadded end, as on the Mac build: sizeof(PrimitiveComponent) is 0x1E0 and CylinderComponent's first field is at 0x1D8. Visual C++ starts a derived class at the parent's non-virtual size (its end rounded up to min(alignment, 4)), which is the same offset for every declared field of this game (ablation pack4-class-base). Script structs continue after the padded parent",
            "5_simd": "Matrix, Plane, Quat, Vector4, SHVector and SHVectorRGB are aligned to 16 bytes (declared alignment, not capped by the pack value); Color (4 bytes) is aligned to 4",
            "6_size": "struct size is padded to the struct's alignment; a class's sizeof is its end padded to its alignment",
        },
        "validation": {
            "class_sizes": {
                "native_classes_registered": v.registered,
                "native_script_classes_compared": v.sizes.compared,
                "equal": v.sizes.equal,
                "different": v.sizes.different.len(),
                "registered_without_script_class": v.sizes.native_only,
                "native_script_classes_without_registration": v.sizes.script_only,
                "non_native_script_classes": v.sizes.nonnative,
            },
            "class_size_difference_columns": ["class", "cpp_name", "script_layout_size", "native_sizeof"],
            "class_size_differences": different,
            "native_offsets": {
                "total": WIN32_NATIVE.len(),
                "match": v.native_ok,
                "mismatch": v.native_bad.len(),
            },
            "independent_offsets": {
                "total": WIN32_SECOND.len(),
                "match": v.second_ok,
                "mismatch": v.second_bad.len(),
                "compiled_member_offsets": WIN32_SECOND.iter().filter(|r| r.does == COMPILED).count(),
                "next_to_the_member_name": WIN32_SECOND.iter().filter(|r| !r.literals.is_empty()).count(),
            },
            "declared_fields_in_all_script_classes": v.fields_total,
            "ablation_columns": ["rule", "instead", "class_sizes_wrong", "native_offsets_wrong", "independent_offsets_wrong", "declared_fields_moved"],
            "ablations": ablations,
        },
        "sizeof": sizeof,
        "native_function_columns": ["function", "located_by", "rva", "identified_by"],
        "native_functions": functions,
        "native_offset_columns": ["class", "field", "offset", "bit", "function", "instruction_rva", "instruction"],
        "native_offsets": offsets,
        "independent_offsets_note": "A second route to the offsets that shares nothing with native_offsets: functions identified by a UTF-16 literal only they use, by the native function name table or by a direct call from such a function (never by a vtable slot shift). UGameEngine::Init rows are the engine's own checks of compiled member offsets (an immediate compared with the offset the engine linked for the property it found by class and member name). name_literals are literals of the executable used next to the instruction.",
        "independent_offset_columns": ["class", "field", "offset", "function", "instruction_rva", "instruction", "name_literals"],
        "independent_offsets": second,
        "other_fields_note": "rule-derived rows of the independent_offsets fields whose class is not listed under classes",
        "other_fields": other_fields,
        "row": ["offset", "name", "kind", "size", "bit", "array_dim"],
        "classes": layout["classes"],
        "structs": structs,
        "member_path_step_columns": ["declared_in", "member", "offset_in_container"],
        "member_paths": member_paths,
    }))
}

// ------------------------------------------------- Win32 recorder layout

/// Recorder symbols, in the order they are written.
const RECORDER_SYMBOLS: &[&str] = &[
    "GEngine",
    "GWorld",
    "GFrameCounter",
    "GDeltaTime",
    "GFixedDeltaTime",
    "GIsBenchmarking",
    "GUseFixedTimeStep",
    "FName::Names",
    "UWorld::Tick",
    "UGameEngine::Tick",
];

/// Native structures of the recorder layout, in the order they are written.
const RECORDER_STRUCTS: &[&str] = &["TArray", "FName", "FNameEntry", "FString", "KeyBind"];

fn js<T: serde::Serialize>(v: T) -> Result<String> {
    Ok(serde_json::to_string(&v)?)
}

/// A JSON object on one line with its keys in the given order (the values
/// are JSON text). `serde_json` maps sort their keys; the recorder layout
/// keeps the key order of the Mac file.
fn obj(pairs: &[(&str, String)]) -> Result<String> {
    let mut s = String::from("{");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&js(k)?);
        s.push_str(": ");
        s.push_str(v);
    }
    s.push('}');
    Ok(s)
}

/// `"key": [` / `{` block with one pre-rendered entry per line.
fn block(out: &mut String, key: &str, open: char, entries: &[String], last: bool) -> Result<()> {
    let close = if open == '[' { ']' } else { '}' };
    out.push_str(&format!("  {}: {open}\n", js(key)?));
    for (i, e) in entries.iter().enumerate() {
        out.push_str("    ");
        out.push_str(e);
        out.push_str(if i + 1 < entries.len() { ",\n" } else { "\n" });
    }
    out.push_str(&format!("  {close}{}\n", if last { "" } else { "," }));
    Ok(())
}

fn named<'j>(list: &'j Json, key: &str, name: &str) -> Result<&'j Json> {
    list[key]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str() == Some(name)))
        .with_context(|| format!("{key} has no entry {name}"))
}

/// The `anchor` object of an evidence entry. A nested anchor (the caller of
/// [`Locate::CalledBy`]) also carries the RVA of the function it names.
fn anchor_pairs(f: &Win32Function, depth: u8) -> Result<Option<Vec<(&'static str, String)>>> {
    Ok(match f.locate {
        Locate::Function(n) => Some(vec![("function", js(n)?)]),
        Locate::Exec(n) => Some(vec![("exec", js(n)?)]),
        Locate::Vtable(c, s) => Some(vec![("vtable", js(c)?), ("slot", js(s)?)]),
        Locate::TailOf(c, s) => Some(vec![("tail_of_vtable", js(c)?), ("slot", js(s)?)]),
        Locate::Literal(t) => Some(vec![("literal", js(t)?)]),
        Locate::CalledBy(caller) => {
            if depth >= 4 {
                bail!("{}: the chain of callers is too long", f.name);
            }
            let c = win32_function(caller)?;
            let mut inner = anchor_pairs(c, depth + 1)?
                .with_context(|| format!("{}: the caller {caller} has no anchor", f.name))?;
            inner.push(("rva", js(rva_hex(c.rva))?));
            Some(vec![("called_by", obj(&inner)?)])
        }
        Locate::Scan => None,
    })
}

fn anchor_json(f: &Win32Function) -> Result<Option<String>> {
    anchor_pairs(f, 0)?.map(|pairs| obj(&pairs)).transpose()
}

/// The `function`, `anchor`, `function_rva`, `rva` and `bytes` members of an
/// evidence entry.
fn evidence_tail(
    function: &str,
    rva: u32,
    bytes: &str,
    pairs: &mut Vec<(&'static str, String)>,
) -> Result<()> {
    let f = win32_function(function)?;
    pairs.push(("function", js(f.name)?));
    if let Some(a) = anchor_json(f)? {
        pairs.push(("anchor", a));
    }
    if f.rva != 0 {
        pairs.push(("function_rva", js(rva_hex(f.rva))?));
    }
    pairs.push(("rva", js(rva_hex(rva))?));
    pairs.push(("bytes", js(bytes)?));
    Ok(())
}

/// The recorder layout for the Win32 build
/// (`tools/trace-recorder/layout_win_x86.json`): the field list, sentinels
/// and symbol names of the Mac layout (`template`), with offsets from the
/// Win32 rules and addresses from the Win32 binary analysis.
fn win32_recorder_layout(
    set: &PackageSet,
    template: &Json,
    globals: &Json,
    functions: &Json,
    image: &Json,
) -> Result<String> {
    let mut lay = Layouter::for_target(set, Target::Win32);
    let text = |j: &Json, what: &str| -> Result<String> {
        j.as_str()
            .map(str::to_owned)
            .with_context(|| format!("{what} is not a string"))
    };

    // Symbols.
    let tsyms = template["symbols"]
        .as_object()
        .context("template has no symbols")?;
    if tsyms.len() != RECORDER_SYMBOLS.len()
        || RECORDER_SYMBOLS.iter().any(|s| !tsyms.contains_key(*s))
    {
        bail!("the template's symbols are not the expected recorder symbols");
    }
    let mut symbols = Vec::new();
    for name in RECORDER_SYMBOLS {
        let t = &tsyms[*name];
        let kind = text(&t["kind"], "symbol kind")?;
        let mut pairs: Vec<(&str, String)> = vec![("mangled", js(name)?)];
        if kind == "data" {
            let g = named(globals, "globals", name)?;
            let ty = text(&g["type"], "global type")?;
            if Some(ty.as_str()) != t["type"].as_str() {
                bail!("{name}: type {ty} differs from the template");
            }
            pairs.push(("rva", js(text(&g["rva_hex"], "rva_hex")?)?));
            pairs.push(("kind", js(&kind)?));
            pairs.push((
                "read",
                js(g["size"]
                    .as_u64()
                    .with_context(|| format!("{name}: no size"))?)?,
            ));
            pairs.push(("type", js(&ty)?));
            pairs.push(("section", js(text(&g["section"], "section")?)?));
            if t.get("file_value_f64").is_some() {
                let v = g["file_value_f64"]
                    .as_f64()
                    .with_context(|| format!("{name}: no file value"))?;
                pairs.push(("file_value_f64", js(v)?));
            }
        } else {
            let f = named(functions, "functions", name)?;
            pairs.push(("rva", js(text(&f["rva_hex"], "rva_hex")?)?));
            let end = f["end_rva"]
                .as_u64()
                .and_then(|e| u32::try_from(e).ok())
                .with_context(|| format!("{name}: no end_rva"))?;
            pairs.push(("end_rva", js(rva_hex(end))?));
            pairs.push(("kind", js(&kind)?));
            pairs.push(("section", js(".text")?));
            pairs.push(("role", js(text(&t["role"], "role")?)?));
            pairs.push((
                "abi",
                js(text(&f["calling_convention"], "calling_convention")?)?,
            ));
        }
        symbols.push(format!("{}: {}", js(name)?, obj(&pairs)?));
    }

    // Native structures.
    let tstructs = template["structs"]
        .as_object()
        .context("template has no structs")?;
    if tstructs.len() != RECORDER_STRUCTS.len()
        || RECORDER_STRUCTS.iter().any(|s| !tstructs.contains_key(*s))
    {
        bail!("the template's structs are not the expected recorder structs");
    }
    let keybind = lay.layout("Engine.Input.KeyBind")?;
    let member = |name: &str| -> Result<u64> {
        keybind
            .fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.offset)
            .with_context(|| format!("KeyBind has no member {name}"))
    };
    let (ptr, array) = {
        let (asize, _) = lay.element(&PropertyType::Str)?;
        (lay.ptr(), asize)
    };
    let structs = vec![
        format!(
            "\"TArray\": {}",
            obj(&[
                ("size", js(array)?),
                (
                    "members",
                    obj(&[("data", js(0)?), ("count", js(ptr)?), ("max", js(ptr + 4)?)])?
                ),
                (
                    "evidence",
                    js(
                        "native_code: UGameEngine::Tick loads the data pointer of Engine.GamePlayers from +0x4B4 and its count from +0x4B8; UInput::GetBind does the same with PressedKeys (+0x84, +0x88)"
                    )?
                ),
            ])?
        ),
        format!(
            "\"FName\": {}",
            obj(&[
                ("size", js(8)?),
                ("members", obj(&[("index", js(0)?), ("number", js(4)?)])?),
                (
                    "evidence",
                    js(
                        "native_code: the name-table validity test loads the index from +0; UInput::GetBind compares names as two words, +0 then +4; FName::AppendString tests the word at +4 and, when it is not zero, appends '_' and that number minus 1"
                    )?
                ),
            ])?
        ),
        format!(
            "\"FNameEntry\": {}",
            obj(&[
                ("size", js(16)?),
                (
                    "members",
                    obj(&[("flags", js(0)?), ("index", js(8)?), ("chars", js(16)?)])?
                ),
                ("wide_flag_mask", js(1)?),
                ("wide_char_size", js(2)?),
                (
                    "evidence",
                    js(
                        "native_code: the name lookup tests bit 0 of the word at +8 and reads the characters at +0x10 in both forms; the wide form is compared with a 2-byte-character routine (WINDOWS_BINARY.md 7)"
                    )?
                ),
            ])?
        ),
        format!(
            "\"FString\": {}",
            obj(&[
                ("size", js(array)?),
                ("members", obj(&[("data", js(0)?), ("count", js(ptr)?)])?),
                ("char_size", js(2)?),
                (
                    "evidence",
                    js(
                        "TArray of TCHAR; TCHAR is 2-byte UTF-16 on this build (WINDOWS_BINARY.md 1.4)"
                    )?
                ),
            ])?
        ),
        format!(
            "\"KeyBind\": {}",
            obj(&[
                ("size", js(keybind.size)?),
                (
                    "members",
                    obj(&[
                        ("Name", js(member("Name")?)?),
                        ("Command", js(member("Command")?)?)
                    ])?
                ),
                (
                    "evidence",
                    js(format!(
                        "native_code: UInput::GetBind steps through Bindings in {}-byte elements and reads Name at +0, Command at +8 and the modifier flags at +0x14; equal to the layout rule for Engine.Input.KeyBind (ends at {}, alignment {})",
                        keybind.size,
                        hex(keybind.end),
                        keybind.align
                    ))?
                ),
            ])?
        ),
    ];

    // Fields.
    let mut fields = Vec::new();
    for t in template["fields"]
        .as_array()
        .context("template has no fields")?
    {
        let class = text(&t["class"], "field class")?;
        let name = text(&t["name"], "field name")?;
        let (f, path) = resolve_field(&mut lay, &class, &name)?;
        let kind = f.kind.trim_end_matches("Property");
        if Some(kind) != t["kind"].as_str() {
            bail!("{class}.{name}: kind {kind} differs from the template");
        }
        if t["bit"].as_u64() != f.bit.map(u64::from) {
            bail!("{class}.{name}: bit {:?} differs from the template", f.bit);
        }
        let shown = WIN32_NATIVE
            .iter()
            .any(|r| r.class == class && r.field == name);
        let evidence = if shown {
            "native_code"
        } else if class.starts_with("asamu.") || name.contains('.') {
            "layout_rule"
        } else {
            "native_layout"
        };
        let mut pairs: Vec<(&str, String)> = vec![
            ("class", js(&class)?),
            ("name", js(&name)?),
            ("offset", js(hex(f.offset))?),
            ("kind", js(kind)?),
            ("size", js(f.size)?),
        ];
        if let Some(b) = f.bit {
            pairs.push(("bit", js(b)?));
        }
        pairs.push(("evidence", js(evidence)?));
        if path.len() > 1 {
            let steps: Vec<String> = path
                .iter()
                .enumerate()
                .map(|(i, (_, n, o))| {
                    if i == 0 {
                        format!("{n} at {}", hex(*o))
                    } else {
                        format!("{n} at +{o:#X}")
                    }
                })
                .collect();
            pairs.push(("note", js(steps.join(", then "))?));
        }
        fields.push(obj(&pairs)?);
    }

    // Sentinels (values, not offsets: the same on every build).
    let mut sentinels = Vec::new();
    for s in template["sentinels"]
        .as_array()
        .context("template has no sentinels")?
    {
        sentinels.push(obj(&[
            ("object", js(&s["object"])?),
            ("class", js(&s["class"])?),
            ("name", js(&s["name"])?),
            ("expected", js(&s["expected"])?),
            ("data_file", js(&s["data_file"])?),
        ])?);
    }

    // Native-code evidence.
    let mut evidence = Vec::new();
    for r in WIN32_NATIVE {
        let short = r.class.rsplit('.').next().unwrap_or(r.class);
        let what = match r.bit {
            Some(b) => format!(
                "{short}.{} = bit {b} of the word at +{:#X} ({} {} it)",
                r.field, r.offset, r.function, r.does
            ),
            None => format!(
                "{short}.{} at +{:#X} ({} {} it)",
                r.field, r.offset, r.function, r.does
            ),
        };
        let mut pairs: Vec<(&str, String)> = vec![
            ("what", js(what)?),
            ("field", js(format!("{}.{}", r.class, r.field))?),
            ("offset", js(hex(r.offset))?),
        ];
        if let Some(b) = r.bit {
            pairs.push(("bit", js(b)?));
        }
        evidence_tail(r.function, r.rva, r.bytes, &mut pairs)?;
        if r.class == "Engine.Engine" && r.field == "Client" {
            // Client->Tick dispatches the frame's input: it must come before
            // the world tick (the recorder's sample point).
            pairs.push(("before_call_to", js("UWorld::Tick")?));
        }
        evidence.push(obj(&pairs)?);
    }
    for r in WIN32_SECOND {
        let short = r.class.rsplit('.').next().unwrap_or(r.class);
        let mut pairs: Vec<(&str, String)> = vec![
            (
                "what",
                js(format!(
                    "{short}.{} at +{:#X} ({} {} it)",
                    r.field, r.offset, r.function, r.does
                ))?,
            ),
            ("field", js(format!("{}.{}", r.class, r.field))?),
            ("offset", js(hex(r.offset))?),
        ];
        evidence_tail(r.function, r.rva, r.bytes, &mut pairs)?;
        if !r.literals.is_empty() {
            pairs.push(("near_literals", js(r.literals)?));
        }
        evidence.push(obj(&pairs)?);
    }
    for r in WIN32_STRUCT_EVIDENCE
        .iter()
        .chain(WIN32_REFLECTION_EVIDENCE)
    {
        let mut pairs: Vec<(&str, String)> = vec![
            ("what", js(format!("{}: {}", r.structure, r.what))?),
            ("struct", js(r.structure)?),
        ];
        evidence_tail(r.function, r.rva, r.bytes, &mut pairs)?;
        evidence.push(obj(&pairs)?);
    }
    let image_base = image["image_base"]
        .as_str()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .context("image.json has no image_base")?;
    for r in WIN32_SYMBOL_EVIDENCE {
        // Keep the table honest: the bytes must hold the symbol's address.
        let g = named(globals, "globals", r.symbol)?;
        let rva = g["rva"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .with_context(|| format!("{}: no rva", r.symbol))?;
        let address: String = image_base
            .wrapping_add(rva)
            .to_le_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !r.bytes.contains(&address) {
            bail!(
                "{}: the instruction bytes {} do not hold its address",
                r.symbol,
                r.bytes
            );
        }
        let mut pairs: Vec<(&str, String)> = vec![("what", js(r.what)?), ("symbol", js(r.symbol)?)];
        evidence_tail(r.function, r.rva, r.bytes, &mut pairs)?;
        if let Some(c) = r.before_call_to {
            pairs.push(("before_call_to", js(c)?));
        }
        evidence.push(obj(&pairs)?);
    }
    for r in WIN32_ABI_EVIDENCE {
        let mut pairs: Vec<(&str, String)> = vec![("what", js(r.what)?)];
        evidence_tail(r.function, r.rva, r.bytes, &mut pairs)?;
        if let Some(c) = r.before_call_to {
            pairs.push(("before_call_to", js(c)?));
        }
        evidence.push(obj(&pairs)?);
    }

    let calls = vec![obj(&[
        (
            "what",
            js("UGameEngine::Tick ticks the world exactly once per frame")?,
        ),
        ("caller", js("UGameEngine::Tick")?),
        ("callee", js("UWorld::Tick")?),
        ("count", js(1)?),
    ])?];

    let exe = text(&image["executable"], "image executable")?;
    let module = exe.rsplit('/').next().unwrap_or(&exe).to_owned();
    let image_line = obj(&[
        ("module", js(&module)?),
        ("image_base", js(&image["image_base"])?),
        ("time_date_stamp", js(&image["time_date_stamp"])?),
        ("size_of_image", js(&image["size_of_image"])?),
        ("address_rule", js("runtime address = module base + rva")?),
    ])?;
    let notes = [
        "Everything the trace recorder reads from the running Windows game (a 32-bit process). Same schema, fields, structures and sentinels as layout_mac_x86_64.json. A symbol's address is the module base of the executable + rva: the image is relocatable, so read the base for each process (image gives the header values that identify the build). mangled is the key the recorder core hands to the front end's symbol resolver; on this build it is the symbol's own name.",
        "Generated by: cargo run --release -p asamu-inspect --example gameplay_defaults -- --target win32 (DEFAULTS.md, Win32 layout). Do not edit by hand. asamu-trace check-recorder checks this file against native_layout_win32.json, the defaults data, the Win32 binary data, the Mac layout's field list and, when a local copy of the executable is present, the executable's bytes.",
        "evidence: native_code = an instruction listed under native_evidence reads or writes the field at this offset (CONFIRMED statically); native_layout = row of native_layout_win32.json for a native class whose sizeof the Win32 rules reproduce (rules CONFIRMED, this offset STRONG); layout_rule = the same rules applied to a script-only class or to a struct member (STRONG). native_evidence also lists fields the recorder does not read: they bracket the ones it does.",
        "native_evidence holds two independent sets. The first locates most functions by vtable slot. The second (entries whose anchor is a literal, an exec thunk other than the four of the first set, or called_by; and every entry with near_literals) was added by a separate verification pass: it identifies each function by a UTF-16 literal only that function uses, by the native function name table or by a direct call from such a function, and includes the engine's own checks of compiled member offsets in UGameEngine::Init. It also shows where the engine's reflection objects keep a property's offset (struct UProperty, UStruct, UField), which a live check can read for every field.",
        "Live validation is pending: nothing in this file has been read from the running Windows game yet. The sentinels are the first live check.",
    ];
    let notes: Vec<String> = notes.iter().map(js).collect::<Result<_>>()?;

    let mut out = String::from("{\n");
    for (k, v) in [
        ("schema", js(&template["schema"])?),
        ("id", js("win-x86-steam-1822049")?),
        ("build", js(WIN32_BUILD)?),
        ("game_build", js(WIN32_GAME_BUILD)?),
        ("executable", js(&exe)?),
        ("pointer_size", js(ptr)?),
        ("image", image_line),
    ] {
        out.push_str(&format!("  {}: {v},\n", js(k)?));
    }
    block(&mut out, "notes", '[', &notes, false)?;
    block(&mut out, "symbols", '{', &symbols, false)?;
    block(&mut out, "structs", '{', &structs, false)?;
    block(&mut out, "fields", '[', &fields, false)?;
    block(&mut out, "sentinels", '[', &sentinels, false)?;
    block(&mut out, "native_evidence", '[', &evidence, false)?;
    block(&mut out, "calls", '[', &calls, true)?;
    out.push_str("}\n");
    Ok(out)
}

/// Options of the Win32 mode.
struct Win32Options {
    data_dir: PathBuf,
    recorder_layout: PathBuf,
    recorder_template: PathBuf,
    layouts: Vec<String>,
    ablate: Vec<String>,
    check: bool,
}

fn read_json(path: &Path) -> Result<Json> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The two files the Win32 mode produces, as text:
/// `(native_layout_win32.json, layout_win_x86.json)`.
fn win32_outputs(
    set: &PackageSet,
    cooked: &Path,
    data_dir: &Path,
    template: &Path,
) -> Result<(Win32Validation, String, String)> {
    let sizes = data_dir.join("class_sizes.json");
    let text =
        fs::read_to_string(&sizes).with_context(|| format!("reading {}", sizes.display()))?;
    let native = parse_win32_classes(&text)?;
    let v = win32_validate(set, cooked, &native, true)?;
    if !v.sizes.different.is_empty() || !v.native_bad.is_empty() || !v.second_bad.is_empty() {
        bail!(
            "the Win32 rules fail validation: {} class sizes differ, native offsets: {:?}, independent offsets: {:?}",
            v.sizes.different.len(),
            v.native_bad,
            v.second_bad
        );
    }
    let template = read_json(template)?;
    let dotted: Vec<(String, String)> = template["fields"]
        .as_array()
        .context("template has no fields")?
        .iter()
        .filter_map(|f| Some((f["class"].as_str()?, f["name"].as_str()?)))
        .filter(|(_, name)| name.contains('.'))
        .map(|(c, n)| (c.to_owned(), n.to_owned()))
        .collect();
    let doc = win32_layout_document(set, &v, &native, &dotted)?;
    let mut layout = String::new();
    render(&doc, 0, 0, 4, &mut layout)?;
    layout.push('\n');
    let recorder = win32_recorder_layout(
        set,
        &template,
        &read_json(&data_dir.join("globals.json"))?,
        &read_json(&data_dir.join("functions.json"))?,
        &read_json(&data_dir.join("image.json"))?,
    )?;
    Ok((v, layout, recorder))
}

/// The Win32 mode (`--target win32`).
fn win32_main(set: &PackageSet, cooked: &Path, o: &Win32Options) -> Result<()> {
    if !o.layouts.is_empty() {
        let mut lay = Layouter::for_target(set, Target::Win32);
        lay.ablate.clone_from(&o.ablate);
        let names: Vec<&str> = o.layouts.iter().map(String::as_str).collect();
        let mut j = layout_json(&mut lay, &names)?;
        j["build"] = json!(WIN32_BUILD);
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    if !o.ablate.is_empty() {
        // Ablation run: report how many checks the rules decide; write nothing.
        let sizes = o.data_dir.join("class_sizes.json");
        let text =
            fs::read_to_string(&sizes).with_context(|| format!("reading {}", sizes.display()))?;
        let native = parse_win32_classes(&text)?;
        let classes = script_classes(set, cooked)?;
        let mut lay = Layouter::for_target(set, Target::Win32);
        lay.ablate.clone_from(&o.ablate);
        let s = win32_class_size_check(&mut lay, &classes, &native)?;
        let (ok, mut bad) = win32_native_check(&mut lay);
        let (second, second_bad) = win32_second_check(&mut lay);
        bad.extend(second_bad);
        println!(
            "win32 ablate {:?}: class sizes compared {} equal {} different {}; native offsets {ok}/{} match; independent offsets {second}/{} match",
            o.ablate,
            s.compared,
            s.equal,
            s.different.len(),
            WIN32_NATIVE.len(),
            WIN32_SECOND.len()
        );
        for (path, cpp, computed, n) in s.different.iter().take(20) {
            println!(
                "  {path} ({cpp}): rules {} native {}",
                hex(*computed),
                hex(*n)
            );
        }
        for b in bad.iter().take(20) {
            println!("  {b}");
        }
        let mut full = Layouter::for_target(set, Target::Win32);
        let base = win32_snapshot(&mut full, &classes)?;
        let snap = win32_snapshot(&mut lay, &classes)?;
        let moved: Vec<_> = base
            .iter()
            .filter(|(k, v)| snap.get(*k) != Some(*v))
            .collect();
        println!("  declared fields moved: {} of {}", moved.len(), base.len());
        for ((class, field), (offset, bit)) in moved.iter().take(20) {
            let now = snap.get(&(class.clone(), field.clone()));
            println!(
                "  {class}.{field}: rules {} bit {bit:?}, ablated {}",
                hex(*offset),
                now.map_or_else(
                    || "missing".to_owned(),
                    |(o, b)| format!("{} bit {b:?}", hex(*o))
                )
            );
        }
        return Ok(());
    }
    let (v, layout, recorder) = win32_outputs(set, cooked, &o.data_dir, &o.recorder_template)?;
    println!(
        "win32 class sizes: {} registered, {} compared, {} equal, {} different; without script class {:?}; non-native script classes {}",
        v.registered,
        v.sizes.compared,
        v.sizes.equal,
        v.sizes.different.len(),
        v.sizes.native_only,
        v.sizes.nonnative
    );
    println!(
        "win32 native offsets: {}/{} match; independent offsets: {}/{} match",
        v.native_ok,
        WIN32_NATIVE.len(),
        v.second_ok,
        WIN32_SECOND.len()
    );
    for a in &v.ablations {
        println!(
            "win32 ablate {}: class sizes wrong {}, native offsets wrong {}, independent offsets wrong {}, declared fields moved {} of {}",
            a.rule,
            a.class_sizes_wrong,
            a.native_offsets_wrong,
            a.second_offsets_wrong,
            a.fields_moved,
            v.fields_total
        );
    }
    let layout_path = o.data_dir.join(WIN32_LAYOUT_FILE);
    if o.check {
        let mut stale = Vec::new();
        for (path, text) in [(&layout_path, &layout), (&o.recorder_layout, &recorder)] {
            if fs::read_to_string(path).ok().as_deref() != Some(text.as_str()) {
                stale.push(path.display().to_string());
            }
        }
        if !stale.is_empty() {
            bail!("out of date (regenerate without --check): {stale:?}");
        }
        println!("win32 check: 2 files compared, 0 differ");
        return Ok(());
    }
    fs::write(&layout_path, layout)
        .with_context(|| format!("writing {}", layout_path.display()))?;
    fs::write(&o.recorder_layout, recorder)
        .with_context(|| format!("writing {}", o.recorder_layout.display()))?;
    println!(
        "wrote {} and {}",
        layout_path.display(),
        o.recorder_layout.display()
    );
    Ok(())
}

// ------------------------------------------------------- map instances

/// Placeable classes whose per-instance overrides are summarized.
const PLACED: &[&str] = &[
    "asamu.ASAMUCheckpoint",
    "asamu.ASAMUKillZone",
    "asamu.ASAMUDynamicKillZone",
    "asamu.ASAMUVelocityCone",
    "asamu.ASAMUTelePad_Attractor",
    "asamu.ASAMUFallingRock",
    "asamu.ASAMUFallingRockManager",
    "asamu.ASAMUFallingWhenGrappledRock",
    "asamu.ASAMUFloatingRock",
    "asamu.ASAMURechargeCrystal",
    "asamu.ASAMUGlowFlower",
    "asamu.ASAMUTimedPowerup",
    "Engine.WorldInfo",
];

/// Per-instance transform/identity properties left out of the summary.
const NOISE: &[&str] = &[
    "Location",
    "Rotation",
    "DrawScale",
    "DrawScale3D",
    "PrePivot",
    "Base",
    "BaseBoneName",
    "BaseSkelComponent",
    "Tag",
    "Layer",
    "Group",
    "Owner",
    "Components",
    "AttachedComponents",
    "CollisionComponent",
    "bHiddenEdLayer",
    "bHiddenEdGroup",
    "bHiddenEdLevel",
    "bHiddenEd",
    "bEditable",
    "bLockLocation",
    "bPathColliding",
    "bPathTemp",
    "OverlapTag",
    "CreationTime",
    "ObjectArchetype",
    "LightingGuid",
    "Brush",
    "BrushComponent",
    "Polys",
    "PolyFlags",
    "RelativeLocation",
    "RelativeRotation",
];

fn simple_value(v: &Value) -> bool {
    match v {
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Byte(_)
        | Value::Enum(_)
        | Value::Name(_) => true,
        Value::Struct { fields, .. } => {
            fields.len() <= 4 && fields.iter().all(|f| simple_value(&f.value))
        }
        _ => false,
    }
}

fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(f64::from(*i)),
        Value::Float(f) => f.to_string().parse::<f64>().ok(),
        Value::Byte(b) => Some(f64::from(*b)),
        _ => None,
    }
}

/// Merged tags with the object/class each value came from.
type Tags = Vec<(Property, String)>;
/// Per class: instance count per map, statistics per property.
type ClassStats = (BTreeMap<String, usize>, BTreeMap<String, PropStats>);

#[derive(Default)]
struct PropStats {
    instances: usize,
    values: BTreeMap<String, usize>,
    min: Option<f64>,
    max: Option<f64>,
}

fn map_instances(set: &PackageSet, maps_dir: &Path) -> Result<Json> {
    let mut files: Vec<PathBuf> = fs::read_dir(maps_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("asamu"))
        .collect();
    files.sort();
    let mut per_class: BTreeMap<String, ClassStats> = BTreeMap::new();
    for f in &files {
        let map = f
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_owned();
        let lp = set.open_file(f)?;
        for i in 0..lp.package.exports.len() {
            let Ok(cname) = lp.package.export_class_name(i) else {
                continue;
            };
            let Some(target) = PLACED.iter().find(|c| {
                c.rsplit('.')
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&cname))
            }) else {
                continue;
            };
            let e = lp.package.export(i)?;
            if e.object_flags & (flags::object::CLASS_DEFAULT_OBJECT | 0x400) != 0 {
                continue;
            }
            let obj = set.decode(&lp, i)?;
            if !obj.class.eq_ignore_ascii_case(target) {
                continue;
            }
            let (counts, stats) = per_class.entry((*target).to_owned()).or_default();
            *counts.entry(map.clone()).or_default() += 1;
            for p in &obj.properties {
                if NOISE.iter().any(|n| n.eq_ignore_ascii_case(&p.name)) || !simple_value(&p.value)
                {
                    continue;
                }
                let st = stats.entry(label(p)).or_default();
                st.instances += 1;
                let key = serde_json::to_string(&value_json(&p.value, false))?;
                *st.values.entry(key).or_default() += 1;
                if let Some(x) = numeric(&p.value) {
                    st.min = Some(st.min.map_or(x, |m| m.min(x)));
                    st.max = Some(st.max.map_or(x, |m| m.max(x)));
                }
            }
        }
    }
    let mut out = Map::new();
    for (class, (counts, stats)) in per_class {
        let mut props = Map::new();
        for (name, st) in stats {
            let mut top: Vec<(&String, &usize)> = st.values.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let values: Vec<Json> = top
                .iter()
                .take(8)
                .map(|(v, n)| json!([serde_json::from_str::<Json>(v).unwrap_or(Json::Null), n]))
                .collect();
            let mut m = Map::new();
            m.insert("instances".into(), json!(st.instances));
            m.insert("distinct".into(), json!(st.values.len()));
            if let (Some(a), Some(b)) = (st.min, st.max) {
                m.insert("min".into(), json!(a));
                m.insert("max".into(), json!(b));
            }
            m.insert("top_values".into(), json!(values));
            props.insert(name, Json::Object(m));
        }
        let total: usize = counts.values().sum();
        out.insert(
            class,
            json!({"instances": total, "per_map": counts, "overridden_properties": props}),
        );
    }
    Ok(json!({
        "schema": "asamu-decomp/map-instance-overrides/v1",
        "note": "tags stored on map-placed instances (deltas against the class defaults); scalar and small-struct values only; transforms and editor fields left out; top_values = [value, instance count]",
        "classes": out,
    }))
}

// ------------------------------------------------------------------ config

/// One `Key=Value` line that survived the merge.
#[derive(Debug, Clone)]
struct IniValue {
    raw: String,
    file: String,
}

/// Merged `.ini` hierarchy of one config name: section -> key -> values.
#[derive(Debug, Default)]
struct IniSet {
    files: Vec<String>,
    sections: BTreeMap<String, BTreeMap<String, Vec<IniValue>>>,
}

impl IniSet {
    fn get(&self, section: &str, key: &str) -> Option<&Vec<IniValue>> {
        self.sections
            .get(&section.to_ascii_lowercase())?
            .get(&key.to_ascii_lowercase())
            .filter(|v| !v.is_empty())
    }
}

/// Loads config hierarchies (`Base*.ini` -> `Default*.ini` -> Mac overlays)
/// following `[Configuration] BasedOn=` exactly as shipped.
struct Configs {
    res: PathBuf,
    cache: HashMap<String, Arc<IniSet>>,
}

impl Configs {
    fn new(res: PathBuf) -> Self {
        Configs {
            res,
            cache: HashMap::new(),
        }
    }

    /// The leaf file for a config name (the Mac overlay when it exists).
    fn leaf(&self, name: &str) -> Option<PathBuf> {
        let mac = self.res.join(format!("ASAMU/Config/Mac/Mac{name}.ini"));
        if mac.is_file() {
            return Some(mac);
        }
        let def = self.res.join(format!("ASAMU/Config/Default{name}.ini"));
        def.is_file().then_some(def)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.res)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn get(&mut self, name: &str) -> Result<Arc<IniSet>> {
        let key = name.to_ascii_lowercase();
        if let Some(s) = self.cache.get(&key) {
            return Ok(s.clone());
        }
        let mut chain = Vec::new();
        let mut cur = self.leaf(name);
        while let Some(p) = cur {
            if chain.len() > 8 || chain.contains(&p) {
                bail!("config BasedOn cycle at {}", p.display());
            }
            let text =
                fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            cur = based_on(&text).map(|b| {
                // `..\X` is relative to the binaries folder, one level below Resources.
                let b = b.replace('\\', "/");
                self.res.join(b.trim_start_matches("../"))
            });
            chain.push(p);
        }
        chain.reverse();
        let mut set = IniSet::default();
        for p in &chain {
            let text = fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            let file = self.rel(p);
            merge_ini(&mut set, &text, &file);
            set.files.push(file);
        }
        let set = Arc::new(set);
        self.cache.insert(key, set.clone());
        Ok(set)
    }
}

fn based_on(text: &str) -> Option<String> {
    let mut in_cfg = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_cfg = l.eq_ignore_ascii_case("[Configuration]");
            continue;
        }
        if in_cfg
            && let Some((k, v)) = l.split_once('=')
            && k.trim().eq_ignore_ascii_case("BasedOn")
        {
            return Some(v.trim().to_owned());
        }
    }
    None
}

/// UE3 combine rules: a plain key in a later file replaces the earlier
/// values; within one file repeated plain keys accumulate; `+` adds when
/// absent, `.` always adds, `-` removes an equal value, `!` clears.
fn merge_ini(set: &mut IniSet, text: &str, file: &str) {
    let mut section = String::new();
    let mut seen_plain: std::collections::HashSet<(String, String)> = Default::default();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with(';') || l.starts_with("//") {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        if section == "configuration" {
            continue;
        }
        let k = k.trim();
        let (op, key) = match k.chars().next() {
            Some(c @ ('+' | '-' | '.' | '!')) => (c, k[1..].trim()),
            _ => (' ', k),
        };
        let key = key.to_ascii_lowercase();
        let val = IniValue {
            raw: v.trim().to_owned(),
            file: file.to_owned(),
        };
        let entry = set
            .sections
            .entry(section.clone())
            .or_default()
            .entry(key.clone())
            .or_default();
        match op {
            '+' => {
                if !entry.iter().any(|e| e.raw == val.raw) {
                    entry.push(val);
                }
            }
            '.' => entry.push(val),
            '-' => entry.retain(|e| e.raw != val.raw),
            '!' => entry.clear(),
            _ => {
                if seen_plain.insert((section.clone(), key)) {
                    entry.clear();
                }
                entry.push(val);
            }
        }
    }
}

/// Parse a config text value into a JSON value of the property's type.
fn config_json(ty: &PropertyType, raw: &str) -> Json {
    let t = raw.trim().trim_end_matches(';').trim();
    let unq = t.trim_matches('"');
    match ty {
        PropertyType::Float => unq
            .parse::<f32>()
            .ok()
            .and_then(|f| f.to_string().parse::<f64>().ok())
            .map_or_else(|| json!(t), |f| json!(f)),
        PropertyType::Int => unq.parse::<i64>().map_or_else(|_| json!(t), |i| json!(i)),
        PropertyType::Bool => match unq.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => json!(true),
            "false" | "no" | "off" | "0" => json!(false),
            _ => json!(t),
        },
        PropertyType::Byte { enum_path: None } => {
            unq.parse::<u8>().map_or_else(|_| json!(t), |b| json!(b))
        }
        _ => json!(unq),
    }
}

// ------------------------------------------------------------------ values

fn float_json(f: f32) -> Json {
    if !f.is_finite() {
        return json!(f.to_string());
    }
    // Shortest decimal that round-trips to the same f32.
    f.to_string()
        .parse::<f64>()
        .map_or_else(|_| json!(f.to_string()), |v| json!(v))
}

fn obj_json(path: &str) -> Json {
    if path.is_empty() || path.eq_ignore_ascii_case("None") {
        Json::Null
    } else {
        json!(path)
    }
}

/// Convert a decoded value to sanitized JSON.
fn value_json(v: &Value, localized: bool) -> Json {
    match v {
        Value::Int(i) => json!(i),
        Value::Float(f) => float_json(*f),
        Value::Bool(b) => json!(b),
        Value::Byte(b) => json!(b),
        Value::Enum(e) | Value::Name(e) => json!(e),
        Value::Str(s) => {
            let n = s.chars().count();
            if localized {
                json!({"omitted": "localized string", "chars": n})
            } else if n > MAX_STR {
                json!({"omitted": "long string", "chars": n})
            } else {
                json!(s)
            }
        }
        Value::Object(o) | Value::Interface(o) => obj_json(&o.path),
        Value::Delegate { object, function } => {
            json!({"object": obj_json(&object.path), "function": function})
        }
        Value::Array(items) => {
            if items.len() > MAX_ARRAY {
                let first: Vec<Json> = items
                    .iter()
                    .take(MAX_ARRAY)
                    .map(|x| value_json(x, localized))
                    .collect();
                json!({"count": items.len(), "first": first})
            } else {
                Json::Array(items.iter().map(|x| value_json(x, localized)).collect())
            }
        }
        Value::RawArray { count, bytes } => json!({"raw_array": {"count": count, "bytes": bytes}}),
        Value::Struct { name, fields, .. } if fields.len() > MAX_ARRAY => {
            json!({"omitted": format!("large struct {name}"), "members": fields.len()})
        }
        Value::Struct { fields, .. } => {
            let mut m = Map::new();
            for f in fields {
                let k = if f.array_index > 0 {
                    format!("{}[{}]", f.name, f.array_index)
                } else {
                    f.name.clone()
                };
                m.insert(k, value_json(&f.value, localized));
            }
            Json::Object(m)
        }
        Value::Raw { bytes, reason } => json!({"raw": {"bytes": bytes, "reason": reason}}),
    }
}

fn zero_json(ty: &PropertyType) -> Json {
    match ty {
        PropertyType::Int => json!(0),
        PropertyType::Float => json!(0.0),
        PropertyType::Byte { enum_path: None } => json!(0),
        PropertyType::Byte { enum_path: Some(_) } => json!("(enum value 0)"),
        PropertyType::Bool => json!(false),
        PropertyType::Name => json!("None"),
        PropertyType::Str => json!(""),
        PropertyType::Array { .. } | PropertyType::Map { .. } => json!([]),
        PropertyType::Struct { .. } => json!("(all members zero)"),
        _ => Json::Null,
    }
}

/// Merge `child` tags onto `base` (child wins; tagged structs member-wise).
fn merge_tags(base: &mut Vec<(Property, String)>, child: &[Property], source: &str) {
    for p in child {
        let k = (p.name.to_ascii_lowercase(), p.array_index);
        match base
            .iter_mut()
            .find(|(b, _)| (b.name.to_ascii_lowercase(), b.array_index) == k)
        {
            Some((b, src)) => {
                merge_value(&mut b.value, &p.value);
                *src = source.to_owned();
            }
            None => base.push((p.clone(), source.to_owned())),
        }
    }
}

fn merge_value(into: &mut Value, from: &Value) {
    match (into, from) {
        (
            Value::Struct {
                binary: false,
                fields: a,
                ..
            },
            Value::Struct {
                binary: false,
                fields: b,
                ..
            },
        ) => {
            let mut tmp: Vec<(Property, String)> =
                a.drain(..).map(|p| (p, String::new())).collect();
            merge_tags(&mut tmp, b, "");
            *a = tmp.into_iter().map(|(p, _)| p).collect();
        }
        (slot, v) => *slot = v.clone(),
    }
}

// ------------------------------------------------------------------ report

/// Classes reported (qualified paths).
const CLASSES: &[&str] = &[
    "asamu.ASAMUPawn",
    "asamu.ASAMUPlayerController",
    "asamu.ASAMUPlayerInput",
    "asamu.ASAMUControllerInput",
    "asamu.GrappleGun",
    "asamu.GrappleGunHitLocActor",
    "asamu.ASAMUInventoryManager",
    "asamu.ASAMUPowerJump",
    "asamu.ASAMURocketBoots",
    "asamu.ASAMUTimedPowerup",
    "asamu.ASAMUCamera",
    "asamu.ASAMUGameInfo",
    "asamu.ASAMUGameInfoTimeTrial",
    "asamu.ASAMUCheckpoint",
    "asamu.ASAMUCheckpointManager",
    "asamu.ASAMUKillZone",
    "asamu.ASAMUDynamicKillZone",
    "asamu.ASAMUVelocityCone",
    "asamu.ASAMUTelePad_Attractor",
    "asamu.ASAMUFallingRock",
    "asamu.ASAMUFallingRockManager",
    "asamu.ASAMUFallingWhenGrappledRock",
    "asamu.ASAMUFloatingRock",
    "asamu.ASAMURechargeCrystal",
    "asamu.ASAMUGlowFlower",
    "asamu.ASAMUSettingsManager",
    "asamu.SeqAct_SetPawnSize",
    "asamu.SeqAct_SetMaxGrapples",
    "Engine.WorldInfo",
    "Engine.PhysicsVolume",
    "Engine.DefaultPhysicsVolume",
];

/// Inherited properties always listed, even when zero (movement physics inputs).
const FOCUS: &[&str] = &[
    "GroundSpeed",
    "AirSpeed",
    "WaterSpeed",
    "LadderSpeed",
    "AccelRate",
    "AirControl",
    "JumpZ",
    "MaxStepHeight",
    "MaxJumpHeight",
    "WalkableFloorZ",
    "LedgeCheckThreshold",
    "MaxFallSpeed",
    "CustomGravityScaling",
    "BaseEyeHeight",
    "EyeHeight",
    "bLimitFallAccel",
    "bForceMaxAccel",
    "bCanJump",
    "bJumpCapable",
    "bCanWalk",
    "bCanCrouch",
    "bCanFly",
    "bCanSwim",
    "bCanClimbLadders",
    "bCanWalkOffLedges",
    "bAvoidLedges",
    "bStopAtLedges",
    "bSimulateGravity",
    "bAllowLedgeOverhang",
    "bForceFloorCheck",
    "bRunPhysicsWithNoController",
    "bForceRMVelocity",
    "bForceRegularVelocity",
    "bIsWalking",
    "bIsCrouched",
    "bWantsToCrouch",
    "bDirectHitWall",
    "bRollToDesired",
    "bNeedsBaseTickedFirst",
    "bCanStepUpOn",
    "bCollideActors",
    "bCollideWorld",
    "bBlockActors",
    "WalkingPct",
    "CrouchedPct",
    "MovementSpeedModifier",
    "CrouchHeight",
    "CrouchRadius",
    "DesiredSpeed",
    "MaxDesiredSpeed",
    "Buoyancy",
    "Mass",
    "OutofWaterZ",
    "MaxOutOfWaterStepHeight",
    "SlopeBoostFriction",
    "MaxMultiJump",
    "MultiJumpRemaining",
    "MultiJumpBoost",
    "MaxDoubleJumpHeight",
    "DoubleJumpEyeHeight",
    "DodgeSpeed",
    "DodgeSpeedZ",
    "bCanDoubleJump",
    "bRequiresDoubleJump",
    "bNoJumpAdjust",
    "bUpdateEyeheight",
    "bNotifyStopFalling",
    "DefaultAirControl",
    "MaxLeanRoll",
    "FailedLandingCount",
    "StartedFallingTime",
    "Bob",
    "bWeaponBob",
    "ViewPitchMin",
    "ViewPitchMax",
    "RotationRate",
    "Physics",
    "WalkingPhysics",
    "LandMovementState",
    "WaterMovementState",
    "DefaultGravityZ",
    "GlobalGravityZ",
    "WorldGravityZ",
    "RBPhysicsGravityScaling",
    "KillZ",
    "GroundFriction",
    "TerminalVelocity",
    "FluidFriction",
    "ZoneVelocity",
    "DefaultFOV",
    "FOVAngle",
    "DesiredFOV",
    "DefaultAimingFOV",
    "MaxTimeMargin",
    "MinHitWall",
    "bNotifyApex",
    "bNotifyPostLanded",
    "bNotifyFallingHitWall",
    "FireInterval",
    "WeaponRange",
    "Spread",
    "InstantHitDamage",
    "InstantHitMomentum",
    "WeaponFireTypes",
    "EquipTime",
    "PutDownTime",
];

struct Reporter<'a> {
    set: &'a PackageSet,
    lay: Layouter<'a>,
    cfg: Configs,
    inherited: HashMap<String, Arc<Vec<(Property, String)>>>,
}

impl<'a> Reporter<'a> {
    /// Merged CDO tags of `class` (root first), cached.
    fn merged(&mut self, class: &str) -> Result<Arc<Vec<(Property, String)>>> {
        let key = class.to_ascii_lowercase();
        if let Some(v) = self.inherited.get(&key) {
            return Ok(v.clone());
        }
        let d = self
            .set
            .inherited_defaults(class)
            .with_context(|| format!("inherited defaults of {class}"))?;
        if !d.warnings.is_empty() {
            eprintln!("warning: {class}: {:?}", d.warnings);
        }
        let v: Vec<(Property, String)> = d
            .values
            .into_iter()
            .map(|r| {
                (
                    Property {
                        name: r.name,
                        type_name: r.type_name,
                        array_index: r.array_index,
                        size: 0,
                        struct_name: None,
                        enum_name: None,
                        value: r.value,
                        offset: 0,
                    },
                    r.source,
                )
            })
            .collect();
        let v = Arc::new(v);
        self.inherited.insert(key, v.clone());
        Ok(v)
    }

    /// An object merged with its archetype chain (component templates,
    /// archetypes); ends at the class defaults of the root archetype's class.
    fn merged_object(&mut self, path: &str) -> Result<(Tags, Vec<String>)> {
        let mut chain = Vec::new();
        let mut cur = Some(path.to_owned());
        let mut class_root: Option<String> = None;
        while let Some(p) = cur.take() {
            if chain.len() > 16 {
                bail!("archetype chain too long at {p}");
            }
            let (lp, i) = self
                .set
                .locate(&p)
                .with_context(|| format!("object {p} not found"))?;
            let e = lp.package.export(i)?;
            if e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0 {
                let obj = self.set.decode(&lp, i)?;
                class_root = Some(obj.class.clone());
                break;
            }
            chain.push((lp.clone(), i, p.clone()));
            cur = lp.ref_path(e.archetype_index)?;
            if cur.is_none() {
                // A null archetype means the class default object of the
                // object's own class (root component templates).
                class_root = Some(self.set.decode(&lp, i)?.class);
            }
        }
        let mut merged: Vec<(Property, String)> = match &class_root {
            Some(c) => self.merged(c)?.as_ref().clone(),
            None => Vec::new(),
        };
        let mut names = Vec::new();
        if let Some(c) = &class_root {
            names.push(format!("class defaults of {c}"));
        }
        for (lp, i, p) in chain.iter().rev() {
            let obj = self.set.decode(lp, *i)?;
            merge_tags(&mut merged, &obj.properties, p);
            names.push(p.clone());
        }
        Ok((merged, names))
    }

    fn report(&mut self, class: &str) -> Result<Json> {
        let def = self
            .set
            .struct_def(class)
            .with_context(|| format!("no class {class}"))?;
        let class = def.path.clone();
        let model = self.set.class_model(&class)?;
        let mut chain: Vec<String> = model.super_chain.clone();
        chain.reverse();
        chain.push(class.clone());
        // Per-class: definition, native flag, config name, own CDO tag names.
        let mut defs = Vec::new();
        let mut native_class: HashMap<String, bool> = HashMap::new();
        let mut config_name: HashMap<String, String> = HashMap::new();
        for c in &chain {
            let m = self.set.class_model(c)?;
            native_class.insert(
                c.to_ascii_lowercase(),
                m.class_flags & flags::class::NATIVE != 0,
            );
            config_name.insert(c.to_ascii_lowercase(), m.config_name.clone());
            let d = self
                .set
                .struct_def(c)
                .with_context(|| format!("no def {c}"))?;
            defs.push(d);
        }
        let merged = self.merged(&class)?;
        let layout = self.lay.layout(&class).ok();
        let own_cfg = model.config_name.clone();
        let cfg = if own_cfg.is_empty() || own_cfg.eq_ignore_ascii_case("None") {
            None
        } else {
            Some(self.cfg.get(&own_cfg)?)
        };
        let mut props = Vec::new();
        let mut omitted_zero = 0usize;
        let mut config_hits = Vec::new();
        for d in &defs {
            let owner = d.path.clone();
            let owner_native = native_class
                .get(&owner.to_ascii_lowercase())
                .copied()
                .unwrap_or(false);
            for p in &d.properties {
                let dim = p.array_dim.max(1);
                for idx in 0..dim {
                    let entry = self.property_entry(
                        &class,
                        &chain,
                        &owner,
                        owner_native,
                        p,
                        idx,
                        &merged,
                        layout.as_deref(),
                        &config_name,
                    )?;
                    let declared_here = owner.eq_ignore_ascii_case(&class)
                        && (class.to_ascii_lowercase().starts_with("asamu.") || is_simple(&p.ty));
                    let keep = declared_here
                        || entry.get("source").and_then(Json::as_str) != Some("zero")
                        || FOCUS.iter().any(|f| f.eq_ignore_ascii_case(&p.name))
                        || entry.get("config").is_some();
                    let native_only =
                        entry.get("source").and_then(Json::as_str) == Some("not_serialized");
                    if entry.get("config").is_some() {
                        config_hits.push(p.name.clone());
                    }
                    if keep
                        && !(native_only
                            && !declared_here
                            && !FOCUS.iter().any(|f| f.eq_ignore_ascii_case(&p.name)))
                    {
                        props.push(entry);
                    } else {
                        omitted_zero += 1;
                    }
                }
            }
        }
        // Component templates of the class default object.
        let mut comps = Map::new();
        for c in &model.components {
            match self.merged_object(&c.template) {
                Ok((vals, chain)) => {
                    let cdef = self
                        .set
                        .locate(&c.template)
                        .and_then(|(lp, i)| self.set.decode(&lp, i).ok())
                        .map(|o| o.class)
                        .unwrap_or_default();
                    let mut vs = Vec::new();
                    let cylinder = cdef.to_ascii_lowercase().ends_with("cylindercomponent");
                    for (p, src) in &vals {
                        // Values set by a template in the chain (not by the
                        // component class defaults), plus the cylinder shape.
                        let from_template = src.contains("Default__") && src.contains('.');
                        let shape = cylinder && component_value_wanted(&cdef, &p.name);
                        if !(from_template || shape) {
                            continue;
                        }
                        vs.push(json!({
                            "name": label(p),
                            "value": value_json(&p.value, false),
                            "value_from": src,
                        }));
                    }
                    if !vs.is_empty() {
                        comps.insert(
                            c.name.clone(),
                            json!({"template": c.template, "class": cdef, "archetype_chain": chain, "values": vs}),
                        );
                    }
                }
                Err(e) => {
                    comps.insert(
                        c.name.clone(),
                        json!({"template": c.template, "error": e.to_string()}),
                    );
                }
            }
        }
        let mut cdo_sources = Vec::new();
        for c in &chain {
            let n = self
                .set
                .class_defaults(c)
                .map(|o| o.properties.len())
                .unwrap_or(0);
            cdo_sources.push(json!({"class": c, "own_tags": n}));
        }
        // Config keys in the chain's sections that name no property.
        let mut unmatched = Vec::new();
        if let Some(cfg) = &cfg {
            for c in &chain {
                if let Some(sec) = cfg.sections.get(&ini_section(c)) {
                    for (k, vals) in sec {
                        let known = defs
                            .iter()
                            .any(|d| d.properties.iter().any(|p| p.name.eq_ignore_ascii_case(k)));
                        if !known && !vals.is_empty() {
                            unmatched.push(json!({"section": ini_section_display(c), "key": k, "values": raw_list(vals)}));
                        }
                    }
                }
            }
        }
        Ok(json!({
            "schema": "asamu-decomp/gameplay-defaults/v1",
            "class": class,
            "super_chain": model.super_chain,
            "class_flags": model.class_flag_names,
            "config_name": own_cfg,
            "config_files": cfg.as_ref().map(|c| c.files.clone()).unwrap_or_default(),
            "cdo_sources": cdo_sources,
            "property_count_listed": props.len(),
            "property_count_omitted_zero_inherited": omitted_zero,
            "properties": props,
            "component_templates": comps,
            "config_keys_without_property": unmatched,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn property_entry(
        &mut self,
        class: &str,
        chain: &[String],
        owner: &str,
        owner_native: bool,
        p: &PropertyDef,
        idx: i32,
        merged: &[(Property, String)],
        layout: Option<&Layout>,
        config_name: &HashMap<String, String>,
    ) -> Result<Json> {
        let localized = p.has(flags::property::LOCALIZED);
        let is_native = p.has(flags::property::NATIVE);
        let is_config = p.has(flags::property::CONFIG) || p.has(flags::property::GLOBAL_CONFIG);
        let hit = merged
            .iter()
            .find(|(m, _)| m.name.eq_ignore_ascii_case(&p.name) && m.array_index == idx);
        let mut e = Map::new();
        let name = if p.array_dim > 1 {
            format!("{}[{idx}]", p.name)
        } else {
            p.name.clone()
        };
        e.insert("name".into(), json!(name));
        e.insert("type".into(), json!(p.ty.describe()));
        e.insert("declared_in".into(), json!(owner));
        let fl: Vec<String> = p
            .flag_names()
            .into_iter()
            .filter(|f| {
                matches!(
                    f.as_str(),
                    "Config"
                        | "GlobalConfig"
                        | "Native"
                        | "Transient"
                        | "Localized"
                        | "Const"
                        | "Edit"
                        | "Net"
                        | "EditConst"
                        | "Deprecated"
                )
            })
            .collect();
        if !fl.is_empty() {
            e.insert("flags".into(), json!(fl));
        }
        if owner_native
            && let Some(l) = layout
            && let Some(f) = l
                .fields
                .iter()
                .find(|f| f.owner.eq_ignore_ascii_case(owner) && f.name == p.name)
        {
            let elem = f.size / u64::from(f.array_dim.max(1));
            let off = f.offset + elem * u64::try_from(idx).unwrap_or(0);
            e.insert("native_offset".into(), json!(hex(off)));
            if let Some(b) = f.bit {
                e.insert("native_bit".into(), json!(b));
            }
        }
        let (mut value, mut source, mut from) = match hit {
            Some((m, src)) => {
                let s = if src.eq_ignore_ascii_case(class) {
                    "cdo"
                } else {
                    "inherited"
                };
                (value_json(&m.value, localized), s, Some(src.clone()))
            }
            None if is_native => (Json::Null, "not_serialized", None),
            None => {
                let z = match &p.ty {
                    // Enum value 0 = the first enumerator.
                    PropertyType::Byte { enum_path: Some(e) } => self
                        .set
                        .enum_names(e)
                        .and_then(|n| n.first().cloned())
                        .map_or_else(|| zero_json(&p.ty), |n| json!(n)),
                    t => zero_json(t),
                };
                (z, "zero", None)
            }
        };
        let mut confidence = match source {
            "cdo" | "inherited" => "CONFIRMED",
            "zero" => "STRONG",
            _ => "UNKNOWN",
        };
        if is_config {
            let global = p.has(flags::property::GLOBAL_CONFIG);
            let found = self.config_value(chain, owner, &p.name, global, config_name, hit)?;
            if let Some((sec, file_name, vals)) = found {
                let cdo_value = value.clone();
                value = if p.array_dim > 1 || matches!(p.ty, PropertyType::Array { .. }) {
                    let inner = match &p.ty {
                        PropertyType::Array { inner } => inner.ty.clone(),
                        t => t.clone(),
                    };
                    if p.array_dim > 1 {
                        vals.get(usize::try_from(idx).unwrap_or(0))
                            .map_or(cdo_value.clone(), |v| config_json(&inner, &v.raw))
                    } else if vals.len() > MAX_ARRAY {
                        json!({"count": vals.len()})
                    } else {
                        Json::Array(vals.iter().map(|v| config_json(&inner, &v.raw)).collect())
                    }
                } else {
                    vals.last()
                        .map_or(Json::Null, |v| config_json(&p.ty, &v.raw))
                };
                e.insert(
                    "config".into(),
                    json!({
                        "config_name": file_name,
                        "section": sec,
                        "key": p.name,
                        "raw": raw_list(&vals),
                        "file": vals.last().map(|v| v.file.clone()),
                    }),
                );
                if cdo_value != value {
                    e.insert("cdo_value".into(), cdo_value);
                }
                source = "config";
                from = None;
                confidence = "STRONG";
            }
        }
        e.insert("value".into(), value);
        e.insert("source".into(), json!(source));
        if let Some(f) = from {
            e.insert("value_from".into(), json!(f));
        }
        e.insert("confidence".into(), json!(confidence));
        Ok(Json::Object(e))
    }

    /// UE3 `LoadConfig`: a non-global config property is read from each
    /// class's own section (the most derived class that has the key, unless
    /// a more derived CDO stores its own value); a globalconfig property only
    /// from the declaring class's section.
    fn config_value(
        &mut self,
        chain: &[String],
        owner: &str,
        name: &str,
        global: bool,
        config_name: &HashMap<String, String>,
        hit: Option<&(Property, String)>,
    ) -> Result<Option<(String, String, Vec<IniValue>)>> {
        let cfg_of = |c: &str| {
            config_name
                .get(&c.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default()
        };
        if global {
            let cn = cfg_of(owner);
            if cn.is_empty() || cn.eq_ignore_ascii_case("None") {
                return Ok(None);
            }
            let set = self.cfg.get(&cn)?;
            return Ok(set
                .get(&ini_section_display(owner), name)
                .map(|v| (ini_section_display(owner), cn.clone(), v.clone())));
        }
        // Walk from the most derived class toward the declaring class.
        let start = chain
            .iter()
            .position(|c| c.eq_ignore_ascii_case(owner))
            .unwrap_or(0);
        let cdo_src = hit.map(|(_, s)| s.to_ascii_lowercase());
        for c in chain[start..].iter().rev() {
            let cn = cfg_of(c);
            if !cn.is_empty() && !cn.eq_ignore_ascii_case("None") {
                let set = self.cfg.get(&cn)?;
                if let Some(v) = set.get(&ini_section_display(c), name) {
                    return Ok(Some((ini_section_display(c), cn, v.clone())));
                }
            }
            // A CDO value stored by this class beats parents' config.
            if cdo_src.as_deref() == Some(c.to_ascii_lowercase().as_str()) {
                return Ok(None);
            }
        }
        Ok(None)
    }
}

/// Scalar, name, vector and rotator types (listed for engine classes even when zero).
fn is_simple(ty: &PropertyType) -> bool {
    match ty {
        PropertyType::Struct { struct_path } => {
            let n = struct_path.to_ascii_lowercase();
            n.ends_with(".vector") || n.ends_with(".rotator")
        }
        PropertyType::Byte { .. }
        | PropertyType::Int
        | PropertyType::Float
        | PropertyType::Bool
        | PropertyType::Name => true,
        _ => false,
    }
}

/// Config text values for the report (capped in count and length).
fn raw_list(vals: &[IniValue]) -> Json {
    if vals.len() > MAX_ARRAY {
        return json!({"count": vals.len()});
    }
    Json::Array(
        vals.iter()
            .map(|v| {
                let n = v.raw.chars().count();
                if n > MAX_RAW {
                    json!({"omitted": "long config value", "chars": n})
                } else {
                    json!(v.raw)
                }
            })
            .collect(),
    )
}

/// `[Package.Class]` section name used by the ini files.
fn ini_section_display(class: &str) -> String {
    class.to_owned()
}

fn ini_section(class: &str) -> String {
    class.to_ascii_lowercase()
}

fn label(p: &Property) -> String {
    if p.array_index > 0 {
        format!("{}[{}]", p.name, p.array_index)
    } else {
        p.name.clone()
    }
}

/// Component values worth listing (collision shape, translation, scale).
fn component_value_wanted(class: &str, name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let c = class.to_ascii_lowercase();
    if c.ends_with("cylindercomponent") {
        return matches!(
            n.as_str(),
            "collisionradius"
                | "collisionheight"
                | "translation"
                | "scale"
                | "scale3d"
                | "collideactors"
                | "blockactors"
                | "blockzeroextent"
                | "blocknonzeroextent"
                | "blockrigidbody"
                | "alwaysloadoncl"
                | "rbchannel"
                | "rbcollidewithchannels"
                | "cancharacterstepupon"
        );
    }
    matches!(
        n.as_str(),
        "translation"
            | "rotation"
            | "scale"
            | "scale3d"
            | "collideactors"
            | "blockactors"
            | "blockzeroextent"
            | "blocknonzeroextent"
            | "blockrigidbody"
            | "hiddengame"
            | "fovangle"
            | "radius"
            | "brightness"
    )
}

// ------------------------------------------------------------------ main

fn install_resources() -> Result<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(p) => PathBuf::from(p),
        None => {
            let home = std::env::var_os("HOME").context("HOME not set")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let res = root.join("A Story About My Uncle.app/Contents/Resources");
    if !res.join("ASAMU/CookedMac").is_dir() {
        bail!(
            "original install not found at {} (set ASAMU_ORIGINAL_DIR)",
            root.display()
        );
    }
    Ok(res)
}

/// The folder that holds the cooked script packages: `CookedMac` of the Mac
/// app bundle, or `ASAMU/CookedPC` when `ASAMU_ORIGINAL_DIR` is a Windows
/// install root. The script packages are the same files in both installs.
fn cooked_script_dir() -> Result<PathBuf> {
    if let Ok(res) = install_resources() {
        return Ok(res.join("ASAMU/CookedMac"));
    }
    if let Some(root) = std::env::var_os("ASAMU_ORIGINAL_DIR") {
        let pc = PathBuf::from(root).join("ASAMU/CookedPC");
        if pc.is_dir() {
            return Ok(pc);
        }
    }
    bail!(
        "original install not found (set ASAMU_ORIGINAL_DIR to the folder that holds the Mac app bundle, or to a Windows install root)"
    )
}

/// Classes whose full native layout is written (`native_layout_<Class>.json`).
const LAYOUTS: &[&str] = &[
    "Core.Object",
    "Engine.Actor",
    "Engine.Pawn",
    "GameFramework.GamePawn",
    "UDKBase.UDKPawn",
    "Engine.Controller",
    "Engine.PlayerController",
    "GameFramework.GamePlayerController",
    "UDKBase.UDKPlayerController",
    "Engine.Brush",
    "Engine.Volume",
    "Engine.PhysicsVolume",
    "Engine.GravityVolume",
    "Engine.Info",
    "Engine.ZoneInfo",
    "Engine.WorldInfo",
    "Core.Component",
    "Engine.ActorComponent",
    "Engine.PrimitiveComponent",
    "Engine.CylinderComponent",
    "Engine.Camera",
    "Engine.Inventory",
    "Engine.Weapon",
    "UDKBase.UDKWeapon",
    "Engine.Interaction",
    "Engine.Input",
    "Engine.PlayerInput",
];

fn main() -> Result<()> {
    let mut out = PathBuf::from("research/local/defaults");
    let mut layouts: Vec<String> = Vec::new();
    let mut only: Vec<String> = Vec::new();
    let mut native_sizes: Option<PathBuf> = None;
    let mut ablate: Vec<String> = Vec::new();
    let mut target = Target::MacX8664;
    let mut win32_data = PathBuf::from(WIN32_DATA_DIR);
    let mut recorder_layout = PathBuf::from(WIN32_RECORDER_LAYOUT);
    let mut recorder_template = PathBuf::from(RECORDER_TEMPLATE);
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().context("--out needs a path")?),
            "--layout" => layouts.push(args.next().context("--layout needs a class")?),
            "--class" => only.push(args.next().context("--class needs a class")?),
            "--ablate" => ablate.push(args.next().context("--ablate needs a rule")?),
            "--target" => {
                let t = args.next().context("--target needs mac or win32")?;
                target = Target::parse(&t).with_context(|| format!("unknown target {t}"))?;
            }
            "--win32-data" => {
                win32_data = PathBuf::from(args.next().context("--win32-data needs a folder")?);
            }
            "--recorder-layout" => {
                recorder_layout =
                    PathBuf::from(args.next().context("--recorder-layout needs a file")?);
            }
            "--recorder-template" => {
                recorder_template =
                    PathBuf::from(args.next().context("--recorder-template needs a file")?);
            }
            "--check" => check = true,
            "--native-sizes" => {
                native_sizes = Some(PathBuf::from(
                    args.next()
                        .context("--native-sizes needs an objdump text file")?,
                ));
            }
            other => bail!("unknown argument {other}"),
        }
    }
    for a in &ablate {
        if !target.ablations().contains(&a.as_str()) {
            bail!(
                "unknown rule {a} for {target:?}; one of {:?}",
                target.ablations()
            );
        }
    }
    if target == Target::Win32 {
        let cooked = cooked_script_dir()?;
        let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
        return win32_main(
            &set,
            &cooked,
            &Win32Options {
                data_dir: win32_data,
                recorder_layout,
                recorder_template,
                layouts,
                ablate,
                check,
            },
        );
    }
    if check {
        bail!("--check is a Win32 option (--target win32)");
    }
    let res = install_resources()?;
    let cooked = res.join("ASAMU/CookedMac");
    let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
    if ablate.is_empty() && layouts.is_empty() {
        fs::create_dir_all(&out)?;
    }
    let mut rep = Reporter {
        set: &set,
        lay: Layouter::new(&set),
        cfg: Configs::new(res.clone()),
        inherited: HashMap::new(),
    };
    if !layouts.is_empty() {
        let names: Vec<&str> = layouts.iter().map(String::as_str).collect();
        let j = layout_json(&mut rep.lay, &names)?;
        println!("{}", serde_json::to_string_pretty(&j)?);
    }
    if !layouts.is_empty() {
        return Ok(());
    }
    rep.lay.ablate = ablate.clone();
    if !ablate.is_empty() {
        // Ablation run: report how many checks a rule decides; write nothing.
        let check = native_check(&mut rep.lay)?;
        println!(
            "ablate {ablate:?}: named offsets {}",
            serde_json::to_string(&check["summary"])?
        );
        if let Some(ns) = &native_sizes {
            let text =
                fs::read_to_string(ns).with_context(|| format!("reading {}", ns.display()))?;
            let sizes = parse_native_sizes(&text);
            let j = class_size_check(&set, &mut rep.lay, &cooked, &sizes, &Default::default())?;
            println!(
                "ablate {ablate:?}: class sizes {}",
                serde_json::to_string(&j["summary"])?
            );
        }
        return Ok(());
    }
    let check = native_check(&mut rep.lay)?;
    write_json(&out.join("native_layout_check.json"), &check)?;
    println!(
        "native layout check: {}",
        serde_json::to_string(&check["summary"])?
    );
    if let Some(ns) = &native_sizes {
        let text = fs::read_to_string(ns).with_context(|| format!("reading {}", ns.display()))?;
        let sizes = parse_native_sizes(&text);
        let mut publish = std::collections::HashSet::new();
        for c in CLASSES.iter().chain(LAYOUTS.iter()) {
            publish.insert(c.to_ascii_lowercase());
            for s in set.super_chain(c) {
                publish.insert(s.to_ascii_lowercase());
            }
        }
        let j = class_size_check(&set, &mut rep.lay, &cooked, &sizes, &publish)?;
        println!(
            "native class sizes: {}",
            serde_json::to_string(&j["summary"])?
        );
        write_json_depth(&out.join("native_class_sizes.json"), &j, 2)?;
    }
    let mi = map_instances(&set, &cooked.join("Maps"))?;
    write_json_depth(&out.join("map_instances.json"), &mi, 4)?;
    let j = layout_json(&mut rep.lay, LAYOUTS)?;
    write_json_depth(&out.join("native_layout.json"), &j, 4)?;
    let classes: Vec<String> = if only.is_empty() {
        CLASSES.iter().map(|s| (*s).to_owned()).collect()
    } else {
        only
    };
    for c in &classes {
        let j = rep.report(c).with_context(|| format!("report for {c}"))?;
        let short = c.rsplit('.').next().unwrap_or(c);
        write_json(&out.join(format!("{short}.json")), &j)?;
        println!(
            "{c}: {} listed, {} zero inherited omitted",
            j["property_count_listed"], j["property_count_omitted_zero_inherited"]
        );
    }
    Ok(())
}

/// Pretty-print the top two levels and keep deeper values on one line, so
/// one property is one line (diff-friendly, about a third of the size).
fn render(v: &Json, indent: usize, depth: usize, max: usize, out: &mut String) -> Result<()> {
    let pad = "  ".repeat(indent + 1);
    let close = "  ".repeat(indent);
    match v {
        Json::Object(m) if depth < max && !m.is_empty() => {
            out.push_str("{\n");
            for (i, (k, x)) in m.iter().enumerate() {
                out.push_str(&pad);
                out.push_str(&serde_json::to_string(k)?);
                out.push_str(": ");
                render(x, indent + 1, depth + 1, max, out)?;
                out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
            }
            out.push_str(&close);
            out.push('}');
        }
        Json::Array(a)
            if depth < max && !a.is_empty() && a.iter().any(|x| x.is_object() || x.is_array()) =>
        {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad);
                render(x, indent + 1, depth + 1, max, out)?;
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&close);
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other)?),
    }
    Ok(())
}

fn write_json(path: &Path, v: &Json) -> Result<()> {
    write_json_depth(path, v, 2)
}

fn write_json_depth(path: &Path, v: &Json, max: usize) -> Result<()> {
    let mut s = String::new();
    render(v, 0, 0, max, &mut s)?;
    s.push('\n');
    fs::write(path, s).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    //! Synthetic tests of the pure helpers, plus one real-data check that
    //! skips when the install is absent. Run with
    //! `cargo test -p asamu-inspect --example gameplay_defaults`.

    use super::*;

    #[test]
    fn align_up_rounds_to_multiples() {
        assert_eq!(align_up(0x24C, 8), 0x250);
        assert_eq!(align_up(0x248, 8), 0x248);
        assert_eq!(align_up(5, 1), 5);
        assert_eq!(align_up(5, 0), 5);
        // Saturates instead of overflowing.
        assert_eq!(align_up(u64::MAX, 16), u64::MAX);
    }

    #[test]
    fn native_sizes_parse_last_edx_before_ctor_call() {
        let text = "\
00000001006b2ff0 <__ZN6AActor27GetPrivateStaticClassAActorEPKw>:
1006b2ffe:     \tmovl\t$0x2a8, %edi
1006b3082:     \tmovl\t$0x248, %edx            ## imm = 0x248
1006b3092:     \tcallq\t0x10008c380 <__ZN6UClassC1E18EStaticConstructorjjjPKwS2_S2_yPFvPvEM7UObjectFvvES8_>
00000001006b3100 <__ZN5APawn26GetPrivateStaticClassAPawnEPKw>:
1006b3110:     \tmovl\t$0x10, %edx
1006b3120:     \tmovl\t$0x590, %edx
1006b3130:     \tcallq\t0x10008c380 <__ZN6UClassC1E18EStaticConstructorjjjPKwS2_S2_yPFvPvEM7UObjectFvvES8_>
00000001006b3200 <__ZN6UField27GetPrivateStaticClassUFieldEPKw>:
1006b3210:     \tmovl\t$0x68, 0x88(%rbx)
";
        let m = parse_native_sizes(text);
        assert_eq!(m.get("AActor"), Some(&0x248));
        assert_eq!(m.get("APawn"), Some(&0x590));
        assert!(!m.contains_key("UField"), "no constructor call, no size");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn ini_merge_follows_ue3_operators() {
        let mut set = IniSet::default();
        merge_ini(
            &mut set,
            "[Configuration]\nBasedOn=..\\X\n[A.B]\nK=1\nK=2\n+L=x\n; comment\n[C.D]\nM=5\n",
            "base",
        );
        merge_ini(
            &mut set,
            "[A.B]\nK=3\n+L=x\n+L=y\n.L=y\n-L=x\n!N=\n",
            "child",
        );
        let k: Vec<&str> = set
            .get("a.b", "K")
            .into_iter()
            .flatten()
            .map(|v| v.raw.as_str())
            .collect();
        assert_eq!(k, ["3"], "a plain key in a later file replaces the values");
        let l: Vec<&str> = set
            .get("A.B", "l")
            .into_iter()
            .flatten()
            .map(|v| v.raw.as_str())
            .collect();
        assert_eq!(l, ["y", "y"]);
        assert_eq!(
            set.get("C.D", "M").map(|v| v[0].file.as_str()),
            Some("base")
        );
        assert!(set.get("configuration", "BasedOn").is_none());
        assert_eq!(
            based_on("[Configuration]\nBasedOn=..\\Engine\\Config\\BaseGame.ini\n"),
            Some("..\\Engine\\Config\\BaseGame.ini".to_owned())
        );
        assert_eq!(based_on("[Other]\nBasedOn=x\n"), None);
    }

    #[test]
    fn config_text_is_typed_by_the_property() {
        assert_eq!(config_json(&PropertyType::Float, "-520.0"), json!(-520.0));
        assert_eq!(config_json(&PropertyType::Float, "0.010"), json!(0.01));
        assert_eq!(config_json(&PropertyType::Int, "16"), json!(16));
        assert_eq!(config_json(&PropertyType::Bool, "TRUE"), json!(true));
        assert_eq!(config_json(&PropertyType::Bool, "0"), json!(false));
        assert_eq!(
            config_json(&PropertyType::Str, "\"ASAMU.ASAMUGameInfo\";"),
            json!("ASAMU.ASAMUGameInfo")
        );
        assert_eq!(config_json(&PropertyType::Float, "abc"), json!("abc"));
    }

    #[test]
    fn floats_use_the_shortest_round_trip_decimal() {
        assert_eq!(float_json(0.3), json!(0.3));
        assert_eq!(float_json(0.78), json!(0.78));
        assert_eq!(float_json(1.0 / 3.0), json!(0.33333334));
        assert_eq!(float_json(f32::NAN), json!("NaN"));
        let back: f32 = "0.33333334".parse().unwrap_or(0.0);
        assert_eq!(back.to_bits(), (1.0f32 / 3.0).to_bits());
    }

    #[test]
    fn strings_are_sanitized() {
        assert_eq!(
            value_json(&Value::Str("short".into()), false),
            json!("short")
        );
        let long = "x".repeat(MAX_STR + 1);
        assert_eq!(
            value_json(&Value::Str(long), false),
            json!({"omitted": "long string", "chars": MAX_STR + 1})
        );
        assert_eq!(
            value_json(&Value::Str("hi".into()), true),
            json!({"omitted": "localized string", "chars": 2})
        );
    }

    #[test]
    fn tagged_structs_merge_member_wise_and_arrays_replace() {
        let prop = |name: &str, value: Value| Property {
            name: name.into(),
            type_name: String::new(),
            array_index: 0,
            size: 0,
            struct_name: None,
            enum_name: None,
            value,
            offset: 0,
        };
        let s = |fields: Vec<Property>| Value::Struct {
            name: "S".into(),
            binary: false,
            fields,
        };
        let mut base = vec![
            (
                prop(
                    "V",
                    s(vec![
                        prop("X", Value::Float(1.0)),
                        prop("Y", Value::Float(2.0)),
                    ]),
                ),
                "Parent".to_owned(),
            ),
            (
                prop("A", Value::Array(vec![Value::Int(1), Value::Int(2)])),
                "Parent".to_owned(),
            ),
        ];
        merge_tags(
            &mut base,
            &[
                prop("v", s(vec![prop("Y", Value::Float(5.0))])),
                prop("A", Value::Array(vec![Value::Int(9)])),
            ],
            "Child",
        );
        assert_eq!(
            value_json(&base[0].0.value, false),
            json!({"X": 1.0, "Y": 5.0})
        );
        assert_eq!(base[0].1, "Child");
        assert_eq!(value_json(&base[1].0.value, false), json!([9]));
    }

    #[test]
    fn render_keeps_deep_values_on_one_line() {
        let v = json!({"a": [{"x": 1, "y": [1, 2]}, {"x": 2}], "b": 3});
        let mut s = String::new();
        assert!(render(&v, 0, 0, 2, &mut s).is_ok());
        assert_eq!(
            s,
            "{\n  \"a\": [\n    {\"x\":1,\"y\":[1,2]},\n    {\"x\":2}\n  ],\n  \"b\": 3\n}"
        );
        let back: Json = serde_json::from_str(&s).unwrap_or(Json::Null);
        assert_eq!(back, v);
    }

    /// Real data: the computed layout reproduces every named native offset.
    /// Skips (passes) when the original install is not available.
    #[test]
    fn real_install_layout_matches_native_offsets() {
        let Ok(res) = install_resources() else {
            eprintln!("skipping: original install not found (set ASAMU_ORIGINAL_DIR)");
            return;
        };
        let cooked = res.join("ASAMU/CookedMac");
        let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
        let mut lay = Layouter::new(&set);
        let check = native_check(&mut lay).unwrap_or(Json::Null);
        assert_eq!(check["summary"]["mismatch"], json!(0));
        assert_eq!(check["summary"]["match"], json!(105));
        let pawn = lay.layout("Engine.Pawn");
        assert!(pawn.is_ok());
        let pawn = pawn.unwrap_or_else(|_| {
            Arc::new(Layout {
                end: 0,
                align: 1,
                size: 0,
                fields: Vec::new(),
            })
        });
        let f = pawn.fields.iter().find(|f| f.name == "bLimitFallAccel");
        assert_eq!(f.map(|f| (f.offset, f.bit)), Some((0x29C, Some(19))));
        assert_eq!(pawn.size, 0x590);
        // An ablated rule must break the match.
        let mut ab = Layouter::new(&set);
        ab.ablate = vec!["no-bool-merge".to_owned()];
        let bad = native_check(&mut ab).unwrap_or(Json::Null);
        assert_ne!(bad["summary"]["mismatch"], json!(0));
    }

    // ------------------------------------------------------------- Win32

    fn parent(end: u64, align: u64) -> Layout {
        Layout {
            end,
            align,
            size: align_up(end, align),
            fields: Vec::new(),
        }
    }

    #[test]
    fn targets_differ_in_pointer_size_wide_alignment_and_parent_rule() {
        assert_eq!(Target::parse("win32"), Some(Target::Win32));
        assert_eq!(Target::parse("mac"), Some(Target::MacX8664));
        assert_eq!(Target::parse("ps3"), None);
        // No packages are needed for the rule functions.
        let set = PackageSet::new::<&Path>(&[]);
        let mac = Layouter::new(&set);
        let mut win = Layouter::for_target(&set, Target::Win32);
        assert_eq!((mac.ptr(), mac.wide_align()), (8, 8));
        assert_eq!((win.ptr(), win.wide_align()), (4, 4));

        // PrimitiveComponent: ends at 0x1D8, 16-aligned, sizeof 0x1E0.
        let simd = parent(0x1D8, 16);
        // AnimNodeBlendBase: ends with a byte at 0xF0, 16-aligned.
        let byte_end = parent(0xF1, 16);
        // Both builds: classes continue at the unpadded end, structs after
        // the padded parent.
        for lay in [&mac, &win] {
            assert_eq!(lay.start_after_parent(&simd, false), 0x1D8);
            assert_eq!(lay.start_after_parent(&simd, true), 0x1E0);
            assert_eq!(lay.start_after_parent(&byte_end, false), 0xF1);
        }
        win.ablate = vec!["padded-class-base".to_owned()];
        assert_eq!(win.start_after_parent(&simd, false), 0x1E0);
        // The Visual C++ non-virtual size: the end rounded up to
        // min(alignment, 4).
        win.ablate = vec!["pack4-class-base".to_owned()];
        assert_eq!(win.start_after_parent(&simd, false), 0x1D8);
        assert_eq!(win.start_after_parent(&byte_end, false), 0xF4);
        assert_eq!(win.start_after_parent(&parent(0x41, 1), false), 0x41);
        assert_eq!(win.start_after_parent(&parent(0x42, 2), false), 0x42);
        assert_eq!(win.start_after_parent(&simd, true), 0x1E0);
        win.ablate = vec!["ptr64".to_owned(), "wide-align-8".to_owned()];
        assert_eq!((win.ptr(), win.wide_align()), (8, 8));

        // Every ablation has a description, and no other name is described.
        let described: Vec<&str> = WIN32_ABLATION_NOTES.iter().map(|(r, _)| *r).collect();
        assert_eq!(described, WIN32_ABLATIONS);
        assert_eq!(Target::Win32.ablations(), WIN32_ABLATIONS);
        assert_eq!(Target::MacX8664.ablations(), ABLATIONS);
    }

    fn decode(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0, "{hex}");
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn holds(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn win32_evidence_tables_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for r in WIN32_NATIVE {
            let id = format!("{}.{}", r.class, r.field);
            assert!(seen.insert(id.clone()), "{id} is listed twice");
            let f = win32_function(r.function).unwrap();
            assert!(f.rva != 0, "{id}: {} has no start", r.function);
            assert!(
                r.rva >= f.rva && r.rva - f.rva < 0x4000,
                "{id}: instruction outside {}",
                r.function
            );
            // One instruction whose displacement is the offset: a 32-bit
            // displacement, or an 8-bit one for offsets below 0x80.
            let bytes = decode(r.bytes);
            assert!(
                (2..=10).contains(&bytes.len()),
                "{id}: {} bytes",
                bytes.len()
            );
            let disp32 = u32::try_from(r.offset).unwrap().to_le_bytes();
            let disp8 = u8::try_from(r.offset).ok().filter(|o| *o < 0x80);
            assert!(
                holds(&bytes, &disp32) || disp8.is_some_and(|d| bytes.contains(&d)),
                "{id}: {} does not hold offset {:#x}",
                r.bytes,
                r.offset
            );
            assert!(r.bit.is_none_or(|b| b < 32));
            assert!(!r.does.is_empty());
        }
        for f in WIN32_FUNCTIONS {
            assert!(!f.identified.is_empty());
            assert_eq!(
                f.locate == Locate::Scan && f.name == "FName lookup",
                f.rva == 0
            );
            assert_eq!(
                WIN32_FUNCTIONS.iter().filter(|g| g.name == f.name).count(),
                1,
                "{} is listed twice",
                f.name
            );
        }
        for r in WIN32_STRUCT_EVIDENCE {
            assert!(RECORDER_STRUCTS.contains(&r.structure));
            win32_function(r.function).unwrap();
            assert!(!decode(r.bytes).is_empty());
        }
        for r in WIN32_REFLECTION_EVIDENCE {
            assert!(!RECORDER_STRUCTS.contains(&r.structure));
            assert_eq!(r.function, "UGameEngine::Init");
            assert!(!decode(r.bytes).is_empty());
        }
        // The second route: no function of it is found through a vtable
        // slot shift or functions.json (three are vtable slots that literals
        // of the Mac function confirm), and it shares no function with the
        // first route.
        let first: std::collections::HashSet<&str> =
            WIN32_NATIVE.iter().map(|r| r.function).collect();
        for r in WIN32_SECOND {
            let id = format!("{}.{} in {}", r.class, r.field, r.function);
            assert!(
                !first.contains(r.function),
                "{id}: function of the first route"
            );
            let f = win32_function(r.function).unwrap();
            assert!(!matches!(
                f.locate,
                Locate::Function(_) | Locate::Scan | Locate::TailOf(..)
            ));
            assert!(r.rva >= f.rva && r.rva - f.rva < 0x4000, "{id}");
            let bytes = decode(r.bytes);
            assert!((2..=10).contains(&bytes.len()), "{id}");
            let disp32 = u32::try_from(r.offset).unwrap().to_le_bytes();
            let disp8 = u8::try_from(r.offset).ok().filter(|o| *o < 0x80);
            assert!(
                holds(&bytes, &disp32) || disp8.is_some_and(|d| bytes.contains(&d)),
                "{id}: {} does not hold offset {:#x}",
                r.bytes,
                r.offset
            );
            // A member-name literal is the field's own name; the engine's
            // own checks also name the class.
            assert!(r.literals.iter().all(|t| !t.is_empty() && t.len() <= 48));
            if let Some(first) = r.literals.first() {
                assert_eq!(*first, r.field, "{id}");
            }
            if r.does == COMPILED {
                assert_eq!(r.function, "UGameEngine::Init");
                assert_eq!(r.literals.len(), 2);
                assert_eq!(r.literals[1], r.class.rsplit('.').next().unwrap());
                assert!(
                    matches!(bytes[0], 0x3D | 0x83),
                    "{id}: not a compare with an immediate"
                );
            }
        }
        assert_eq!(
            WIN32_SECOND.iter().filter(|r| r.does == COMPILED).count(),
            16
        );
        // A caller chain ends in a function that is found by itself.
        for f in WIN32_FUNCTIONS {
            let mut at = f;
            let mut steps = 0;
            while let Locate::CalledBy(caller) = at.locate {
                at = win32_function(caller).unwrap();
                steps += 1;
                assert!(steps <= 4, "{}: caller chain too long", f.name);
            }
            if steps > 0 {
                assert!(!matches!(at.locate, Locate::Scan), "{}", f.name);
                assert!(anchor_json(f).unwrap().unwrap().contains("called_by"));
            }
        }
        for r in WIN32_SYMBOL_EVIDENCE {
            assert!(RECORDER_SYMBOLS.contains(&r.symbol));
            win32_function(r.function).unwrap();
            // An absolute 32-bit address follows the opcode bytes.
            assert!(decode(r.bytes).len() >= 5);
        }
        // Every data symbol of the recorder is tied to an instruction.
        for s in RECORDER_SYMBOLS.iter().filter(|s| !s.contains("Tick")) {
            assert!(
                WIN32_SYMBOL_EVIDENCE.iter().any(|r| r.symbol == *s),
                "{s} has no instruction"
            );
        }
        for r in WIN32_ABI_EVIDENCE {
            win32_function(r.function).unwrap();
            assert!(!decode(r.bytes).is_empty());
        }
        assert!(win32_function("no such function").is_err());
    }

    #[test]
    fn win32_class_table_parses_by_column_name() {
        let text = r#"{"columns": ["package", "name", "cpp_name", "size", "super"],
            "classes": [["Engine", "Pawn", "APawn", 1108, "AActor"],
                        ["Core", "Object", "UObject", 60, null]]}"#;
        let c = parse_win32_classes(text).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].path().as_str(), c[0].cpp.as_str(), c[0].size),
            ("Engine.Pawn", "APawn", 1108)
        );
        assert_eq!(c[1].size, 0x3C);
        // A missing column, a malformed row and non-JSON text are errors.
        assert!(parse_win32_classes(r#"{"columns": ["package", "name"], "classes": []}"#).is_err());
        assert!(
            parse_win32_classes(
                r#"{"columns": ["package", "name", "cpp_name", "size"], "classes": [["Core", "Object", "UObject", "60"]]}"#
            )
            .is_err()
        );
        assert!(parse_win32_classes("not json").is_err());
    }

    #[test]
    fn recorder_layout_writer_keeps_key_order() {
        let line = obj(&[
            ("zeta", js(1).unwrap()),
            ("alpha", js("a\"b").unwrap()),
            (
                "members",
                obj(&[("data", js(0).unwrap()), ("count", js(4).unwrap())]).unwrap(),
            ),
        ])
        .unwrap();
        assert_eq!(
            line,
            r#"{"zeta": 1, "alpha": "a\"b", "members": {"data": 0, "count": 4}}"#
        );
        let back: Json = serde_json::from_str(&line).unwrap();
        assert_eq!(back["members"]["count"], json!(4));
        let mut out = String::new();
        block(
            &mut out,
            "fields",
            '[',
            &[line.clone(), "{}".to_owned()],
            false,
        )
        .unwrap();
        block(&mut out, "calls", '{', &[], true).unwrap();
        assert_eq!(
            out,
            format!("  \"fields\": [\n    {line},\n    {{}}\n  ],\n  \"calls\": {{\n  }}\n")
        );
        let whole: Json = serde_json::from_str(&format!("{{\n{out}}}")).unwrap();
        assert_eq!(whole["fields"][0]["zeta"], json!(1));
        assert_eq!(rva_hex(0x63_5450), "0x00635450");
        assert_eq!(
            locate_text(Locate::Vtable("APawn", 149)),
            "vtable of APawn, slot 149 (+0x254)"
        );
        assert_eq!(
            anchor_json(win32_function("UObject::AddObject").unwrap()).unwrap(),
            Some(
                r#"{"called_by": {"vtable": "UObject", "slot": 35, "rva": "0x001F92A0"}}"#
                    .to_owned()
            )
        );
        assert_eq!(
            anchor_json(win32_function("FName::SafeString").unwrap()).unwrap(),
            Some(r#"{"literal": "*INVALID*"}"#.to_owned())
        );
        assert_eq!(
            anchor_json(win32_function("FName lookup").unwrap()).unwrap(),
            None
        );
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Real data: the Win32 rules reproduce every native class size and every
    /// native offset, each rule but one is decided by a check, and the
    /// committed Win32 files are what the rules generate. Skips (passes) when
    /// the original install is not available.
    #[test]
    fn real_install_win32_layout_matches_sizes_offsets_and_committed_files() {
        let Ok(cooked) = cooked_script_dir() else {
            eprintln!("skipping: original install not found (set ASAMU_ORIGINAL_DIR)");
            return;
        };
        let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
        let root = repo_root();
        let data = root.join(WIN32_DATA_DIR);
        let (v, layout, recorder) =
            win32_outputs(&set, &cooked, &data, &root.join(RECORDER_TEMPLATE)).unwrap();
        assert_eq!(v.registered, 1982);
        assert_eq!((v.sizes.compared, v.sizes.equal), (1652, 1652));
        assert!(v.sizes.different.is_empty() && v.sizes.script_only.is_empty());
        assert_eq!(v.native_ok, WIN32_NATIVE.len());
        assert!(v.native_bad.is_empty(), "{:?}", v.native_bad);
        assert_eq!(v.second_ok, WIN32_SECOND.len());
        assert!(v.second_bad.is_empty(), "{:?}", v.second_bad);
        for a in &v.ablations {
            // Rules that move nothing in this game's classes:
            // - no class ends off a 4-byte boundary before a child that
            //   starts with a byte, so the Visual C++ rule and the unpadded
            //   end give the same offsets;
            // - Matrix and SHVectorRGB take their alignment from their
            //   members, and no layout depends on SHVector's own alignment.
            let inert = [
                "pack4-class-base",
                "no-align16-matrix",
                "no-align16-shvector",
                "no-align16-shvectorrgb",
            ];
            if inert.contains(&a.rule) {
                assert_eq!(
                    (a.class_sizes_wrong, a.native_offsets_wrong, a.fields_moved),
                    (0, 0, 0),
                    "{}",
                    a.rule
                );
            } else {
                assert!(
                    a.class_sizes_wrong > 0 || a.native_offsets_wrong > 0,
                    "no check decides {}",
                    a.rule
                );
                assert!(a.fields_moved > 0);
            }
        }
        // The fields the Windows recorder depends on.
        let mut lay = Layouter::for_target(&set, Target::Win32);
        let at = |lay: &mut Layouter<'_>, class: &str, name: &str| {
            resolve_field(lay, class, name).map(|(f, _)| (f.offset, f.bit, f.size))
        };
        assert_eq!(
            at(&mut lay, "Engine.Actor", "Location").unwrap(),
            (0x54, None, 12)
        );
        assert_eq!(
            at(&mut lay, "Engine.Pawn", "bLimitFallAccel").unwrap(),
            (0x204, Some(19), 4)
        );
        assert_eq!(
            at(&mut lay, "Engine.Camera", "CameraCache.POV.FOV").unwrap(),
            (0x39C, None, 4)
        );
        assert_eq!(lay.layout("Engine.Input.KeyBind").unwrap().size, 24);
        assert_eq!(lay.layout("Core.Object.Map_Mirror").unwrap().size, 60);
        assert_eq!(lay.layout("Engine.Pawn").unwrap().size, 0x454);
        assert!(at(&mut lay, "Engine.Actor", "NoSuchField").is_err());
        assert!(at(&mut lay, "Engine.Actor", "Location.X.Y").is_err());
        // The committed files are exactly what the rules generate.
        let committed = |p: PathBuf| fs::read_to_string(&p).unwrap_or_default();
        assert_eq!(
            committed(data.join(WIN32_LAYOUT_FILE)),
            layout,
            "regenerate with --target win32"
        );
        assert_eq!(
            committed(root.join(WIN32_RECORDER_LAYOUT)),
            recorder,
            "regenerate with --target win32"
        );
    }
}
