//! Gameplay defaults with provenance, from the user's own install.
//!
//! Reads class default objects (inheritance resolved), component templates
//! (archetype chain resolved) and the shipped `.ini` hierarchy, computes the
//! native x86_64 field layout of each class from its script property layout,
//! and writes sanitized JSON (names, types, numeric values, offsets) to an
//! output directory.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/wf2-defaults cargo run --release -p asamu-inspect \
//!     --example gameplay_defaults -- --out docs/reverse-engineering/data/defaults \
//!     [--native-sizes research/local/defaults/gpsc.asm]   # objdump text, see DEFAULTS.md
//!     [--ablate <rule>]       # report what one layout rule decides; writes nothing
//!     [--layout <Class>]...   # print the own-field layout of classes; writes nothing
//!     [--class <Class>]...    # report only these classes
//! ```
//!
//! Method and results: `docs/reverse-engineering/DEFAULTS.md`.
//!
//! The install is located through `ASAMU_ORIGINAL_DIR` or the default macOS
//! Steam library. Nothing is written inside the install. No script source,
//! payload bytes or long strings are emitted: string values longer than
//! [`MAX_STR`] characters, and every localized string, are replaced by their
//! length. The output is deterministic (sorted maps, declaration order).

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
/// Size of a pointer on the x86_64 Mac build.
const PTR: u64 = 8;

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

/// Layout engine: UE3 property linking rules for a 64-bit little-endian build.
struct Layouter<'a> {
    set: &'a PackageSet,
    cache: HashMap<String, Arc<Layout>>,
    depth: usize,
    /// Rules switched off (`--ablate`), to measure what each rule decides.
    ablate: Vec<String>,
}

/// Layout rules that `--ablate` can switch off.
const ABLATIONS: &[&str] = &[
    "padded-class-base",
    "no-simd-align",
    "no-color-align",
    "ptr32",
    "no-bool-merge",
];

impl<'a> Layouter<'a> {
    fn new(set: &'a PackageSet) -> Self {
        Layouter {
            set,
            cache: HashMap::new(),
            depth: 0,
            ablate: Vec::new(),
        }
    }

    fn off(&self, rule: &str) -> bool {
        self.ablate.iter().any(|a| a == rule)
    }

    fn ptr(&self) -> u64 {
        if self.off("ptr32") { 4 } else { PTR }
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
            let n = if short == "pointer" { self.ptr() } else { 8 };
            let l = Arc::new(Layout {
                end: n,
                align: n,
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
                // Classes continue at the parent's unpadded end (UE3
                // `PropertiesSize`; the Itanium C++ ABI of the Mac build
                // reuses a non-POD base's tail padding). Script structs are
                // POD: their members start after the padded parent.
                let start = if def.kind == asamu_ue3::schema::StructKind::ScriptStruct
                    || self.off("padded-class-base")
                {
                    b.size
                } else {
                    b.end
                };
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
            "matrix" | "plane" | "quat" | "vector4" | "shvector" | "shvectorrgb"
                if !self.off("no-simd-align") =>
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
    let mut files: Vec<PathBuf> = fs::read_dir(cooked)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".u") || n.eq_ignore_ascii_case("Startup.upk")
        })
        .collect();
    files.sort();
    let (mut matched, mut mismatched, mut unpaired, mut nonnative) =
        (0usize, 0usize, 0usize, 0usize);
    let mut mismatches = Vec::new();
    let mut unpaired_names: BTreeMap<String, usize> = BTreeMap::new();
    let mut sizes = Map::new();
    for f in &files {
        let lp = set.open_file(f)?;
        for i in 0..lp.package.exports.len() {
            if lp.package.export_class_name(i).ok().as_deref() != Some("Class") {
                continue;
            }
            let path = lp.qualified(i)?;
            let model = set.class_model(&path)?;
            if model.class_flags & flags::class::NATIVE == 0 {
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
                    .entry(format!("{}: no GetPrivateStaticClass function", lp.name))
                    .or_insert(0usize) += 1;
                continue;
            };
            let (cpp, nsize) = match cands.as_slice() {
                [one] => one.clone(),
                _ => {
                    unpaired += 1;
                    *unpaired_names
                        .entry(format!("{}: several C++ classes share the name", lp.name))
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
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().context("--out needs a path")?),
            "--layout" => layouts.push(args.next().context("--layout needs a class")?),
            "--class" => only.push(args.next().context("--class needs a class")?),
            "--ablate" => {
                let a = args.next().context("--ablate needs a rule")?;
                if !ABLATIONS.contains(&a.as_str()) {
                    bail!("unknown rule {a}; one of {ABLATIONS:?}");
                }
                ablate.push(a);
            }
            "--native-sizes" => {
                native_sizes = Some(PathBuf::from(
                    args.next()
                        .context("--native-sizes needs an objdump text file")?,
                ));
            }
            other => bail!("unknown argument {other}"),
        }
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
}
