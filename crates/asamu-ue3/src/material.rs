//! Materials for UE3 v868: `Material` (and `DecalMaterial`),
//! `MaterialInstanceConstant`, `MaterialInstanceTimeVarying`,
//! `MaterialFunction`, their tagged properties, their expression graphs
//! (`MaterialExpression*` subobjects) and the cooked native tail, plus an
//! **approximate** description of every material for a modern PBR renderer.
//!
//! Native tail (CONFIRMED by exact consumption of every material export of
//! the macOS build; see `docs/reverse-engineering/MATERIALS.md`):
//!
//! ```text
//! UMaterial / UDecalMaterial (after the tagged properties)
//!   u32 QualityMask                    bit 0: high-quality resource, bit 1: low-quality resource
//!   per set bit, low bit first: FMaterialResource
//!
//! UMaterialInstance (Constant / TimeVarying), only when the tagged bool
//! bHasStaticPermutationResource is true (otherwise nothing follows the tags)
//!   u32 QualityMask
//!   per set bit: FMaterialResource, FStaticParameterSet
//!
//! FMaterialResource
//!   TArray<FString>   CompileErrors
//!   i32 count, count x { obj Expression, i32 Length }   TextureDependencyLengthMap
//!   i32               MaxTextureDependencyLength
//!   FGuid             Id
//!   u32               NumUserTexCoords
//!   TArray<obj>       UniformExpressionTextures
//!   u32 x 5           bUsesSceneColor, bUsesSceneDepth, bUsesDynamicParameter,
//!                     bUsesLightmapUVs, bUsesMaterialVertexPositionOffset
//!   u32               UsingTransforms
//!   TArray<FTextureLookup>  { i32 TexCoordIndex, i32 TextureIndex, f32 UScale, f32 VScale }
//!   u32               (legacy value, discarded on load)
//!   u32 x 3           FMaterialResource extras (first discarded on load)
//!
//! FStaticParameterSet
//!   FGuid BaseMaterialId
//!   TArray { FName Name, u32 Value, u32 bOverride, FGuid ExpressionGUID }               static switches
//!   TArray { FName Name, u32 R, G, B, A, u32 bOverride, FGuid ExpressionGUID }          component masks
//!   TArray { FName Name, u8 CompressionSettings, u32 bOverride, FGuid ExpressionGUID }  normal parameters
//!   TArray { FName Name, i32 WeightmapIndex, u32 bOverride, FGuid ExpressionGUID }      terrain layer weights
//! ```
//!
//! The compiled shader maps are **not** stored in the material: they live in
//! the `ShaderCache` objects of the `RefShaderCache-*.upk` packages, keyed by
//! the resource `Id` / static parameter set. Nothing here decodes shader code.
//!
//! Expression subobjects and material functions carry only tagged properties.
//!
//! On top of the decoder, [`MaterialDecoder::approximate`] resolves instance
//! chains (instance → parent → ... → `Material`), applies parameter
//! overrides (scalar, vector, texture, static switch, static component mask)
//! and walks the expression graph of each material input with a small
//! symbolic evaluator, producing an [`ApproxMaterial`]: base colour, normal
//! map, emissive, specular/roughness, opacity (masked with clip value or
//! translucent), two-sided and unlit flags, and texture coordinate transforms
//! (channel, tiling, offset, panning, rotation) where the graph makes them
//! derivable. Every simplification is listed in the material's `notes`.
//!
//! Every count is checked against the remaining bytes before allocating,
//! graph walks are depth- and work-limited, and malformed input yields an
//! error; nothing here panics.

use std::cell::RefCell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::flags;
use crate::model::{LoadedPackage, PackageSet};
use crate::object::{ObjResult, ObjectError};
use crate::package::Package;
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::texture::{TextureDecoder, decode_to_rgba8};
use crate::types::{FName, Guid, PackageIndex};
use crate::writer::Writer;

#[path = "material_extra.rs"]
mod extra;

pub use extra::{
    GREY_CHROMA, GradientFactor, Lost, MAX_GRADIENT_FACTORS, STATS_MAX_EDGE, TextureStats,
    UvGradient,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from material decoding.
#[derive(Debug, Error)]
pub enum MaterialError {
    /// Prelude, tagged properties or native data failed to decode.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A low-level read failed.
    #[error(transparent)]
    Ue3(#[from] crate::error::Ue3Error),
    /// The export is not a material, material instance or material function.
    #[error("export {export} ({class}) is not a material")]
    NotAMaterial {
        /// Export index.
        export: usize,
        /// Class path.
        class: String,
    },
    /// The data is inconsistent.
    #[error("material {path}: {detail}")]
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

/// Material-related class of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum MaterialClass {
    /// `Engine.Material`.
    Material,
    /// `Engine.DecalMaterial` (a `Material` subclass with the same native data).
    DecalMaterial,
    /// Another subclass of `Engine.Material` (e.g. the editor's `PreviewMaterial`).
    OtherMaterial,
    /// `Engine.MaterialInstanceConstant`.
    MaterialInstanceConstant,
    /// `Engine.MaterialInstanceTimeVarying`.
    MaterialInstanceTimeVarying,
    /// `Engine.MaterialInstance` itself or another subclass of it.
    OtherInstance,
    /// `Engine.MaterialFunction` (a reusable expression graph; not renderable).
    MaterialFunction,
}

/// What follows the tagged properties of a material export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeKind {
    /// `UMaterial::Serialize`: quality mask + resources.
    Material,
    /// `UMaterialInstance::Serialize`: quality mask + resources + static
    /// parameter sets when `static_permutation` (the tagged
    /// `bHasStaticPermutationResource`) is set, nothing otherwise.
    Instance {
        /// `bHasStaticPermutationResource`.
        static_permutation: bool,
    },
    /// No native data (material functions, class default objects).
    None,
}

impl MaterialClass {
    /// Class name.
    pub fn name(self) -> &'static str {
        match self {
            MaterialClass::Material => "Material",
            MaterialClass::DecalMaterial => "DecalMaterial",
            MaterialClass::OtherMaterial => "OtherMaterial",
            MaterialClass::MaterialInstanceConstant => "MaterialInstanceConstant",
            MaterialClass::MaterialInstanceTimeVarying => "MaterialInstanceTimeVarying",
            MaterialClass::OtherInstance => "OtherInstance",
            MaterialClass::MaterialFunction => "MaterialFunction",
        }
    }

    /// True for `Material` and its subclasses.
    pub fn is_material(self) -> bool {
        matches!(
            self,
            MaterialClass::Material | MaterialClass::DecalMaterial | MaterialClass::OtherMaterial
        )
    }

    /// True for `MaterialInstance` and its subclasses.
    pub fn is_instance(self) -> bool {
        matches!(
            self,
            MaterialClass::MaterialInstanceConstant
                | MaterialClass::MaterialInstanceTimeVarying
                | MaterialClass::OtherInstance
        )
    }

    /// Classify a class from its qualified path and super chain (qualified
    /// paths, nearest first). Returns `None` for unrelated classes.
    pub fn classify(class_path: &str, super_chain: &[String]) -> Option<MaterialClass> {
        let is = |s: &str, n: &str| s.eq_ignore_ascii_case(n);
        let exact = [
            ("Engine.Material", MaterialClass::Material),
            ("Engine.DecalMaterial", MaterialClass::DecalMaterial),
            (
                "Engine.MaterialInstanceConstant",
                MaterialClass::MaterialInstanceConstant,
            ),
            (
                "Engine.MaterialInstanceTimeVarying",
                MaterialClass::MaterialInstanceTimeVarying,
            ),
            ("Engine.MaterialInstance", MaterialClass::OtherInstance),
            ("Engine.MaterialFunction", MaterialClass::MaterialFunction),
        ];
        if let Some((_, c)) = exact.iter().find(|(n, _)| is(class_path, n)) {
            return Some(*c);
        }
        for s in super_chain {
            if is(s, "Engine.Material") {
                return Some(MaterialClass::OtherMaterial);
            }
            if is(s, "Engine.MaterialInstance") {
                return Some(MaterialClass::OtherInstance);
            }
            if is(s, "Engine.MaterialFunction") {
                return Some(MaterialClass::MaterialFunction);
            }
        }
        None
    }
}

/// Short expression class name: `Engine.MaterialExpressionTextureSample` →
/// `TextureSample`. `None` when the class is not a material expression.
pub fn expression_kind(class_path: &str) -> Option<&str> {
    let bare = class_path.rsplit('.').next().unwrap_or(class_path);
    let prefix = "MaterialExpression";
    // `get` instead of slicing: a class name from a hostile name table may
    // put a multi-byte character across the prefix boundary.
    let head = bare.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        bare.get(prefix.len()..)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Native data
// ---------------------------------------------------------------------------

/// Largest number of quality levels (`MSQ_MAX`): two quality-mask bits.
pub const MAX_QUALITY_LEVELS: u32 = 2;

/// Serialized size of one `FTextureLookup`.
pub const TEXTURE_LOOKUP_SIZE: usize = 16;

/// `FMaterial::FTextureLookup`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TextureLookup {
    /// UV channel used by the lookup.
    pub tex_coord_index: i32,
    /// Index into the resource's uniform expression textures.
    pub texture_index: i32,
    /// U scale.
    pub u_scale: f32,
    /// V scale.
    pub v_scale: f32,
}

/// One entry of `TextureDependencyLengthMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DependencyLength {
    /// The expression (a package index).
    pub expression: PackageIndex,
    /// Its texture dependency length.
    pub length: i32,
}

/// `FMaterialResource` (`FMaterial::Serialize` + the resource's own fields).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaterialResource {
    /// `CompileErrors`.
    pub compile_errors: Vec<String>,
    /// `TextureDependencyLengthMap` in stored order.
    pub texture_dependency_lengths: Vec<DependencyLength>,
    /// `MaxTextureDependencyLength`.
    pub max_texture_dependency_length: i32,
    /// `Id`: identifies the compiled shader map in the shader caches.
    pub id: Guid,
    /// `NumUserTexCoords`: UV channels the compiled material reads.
    pub num_user_tex_coords: u32,
    /// `UniformExpressionTextures` (package indices of textures).
    pub uniform_expression_textures: Vec<PackageIndex>,
    /// `bUsesSceneColor`.
    pub uses_scene_color: bool,
    /// `bUsesSceneDepth`.
    pub uses_scene_depth: bool,
    /// `bUsesDynamicParameter`.
    pub uses_dynamic_parameter: bool,
    /// `bUsesLightmapUVs`.
    pub uses_lightmap_uvs: bool,
    /// `bUsesMaterialVertexPositionOffset`.
    pub uses_vertex_position_offset: bool,
    /// `UsingTransforms` bit field.
    pub using_transforms: u32,
    /// `TextureLookups`.
    pub texture_lookups: Vec<TextureLookup>,
    /// A `u32` the loader reads and discards (UE3: dropped fallback components).
    pub legacy_u32: u32,
    /// The three `u32` written by `FMaterialResource::Serialize` after the
    /// common part. The loader discards the first; the other two are kept
    /// (UE3 names TENTATIVE: blend-mode override value / overridden flag /
    /// masked override value).
    pub resource_u32: [u32; 3],
}

/// `FStaticSwitchParameter`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticSwitchParameter {
    /// Parameter name.
    pub name: FName,
    /// Value.
    pub value: bool,
    /// `bOverride`.
    pub overridden: bool,
    /// Expression GUID.
    pub expression_guid: Guid,
}

/// `FStaticComponentMaskParameter`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticComponentMaskParameter {
    /// Parameter name.
    pub name: FName,
    /// R, G, B, A.
    pub mask: [bool; 4],
    /// `bOverride`.
    pub overridden: bool,
    /// Expression GUID.
    pub expression_guid: Guid,
}

/// `FNormalParameter`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NormalParameter {
    /// Parameter name.
    pub name: FName,
    /// `CompressionSettings` (byte).
    pub compression_settings: u8,
    /// `bOverride`.
    pub overridden: bool,
    /// Expression GUID.
    pub expression_guid: Guid,
}

/// `FTerrainLayerWeightParameter`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerrainLayerWeightParameter {
    /// Parameter name.
    pub name: FName,
    /// `WeightmapIndex`.
    pub weightmap_index: i32,
    /// `bOverride`.
    pub overridden: bool,
    /// Expression GUID.
    pub expression_guid: Guid,
}

/// `FStaticParameterSet`.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct StaticParameterSet {
    /// `BaseMaterialId`: the `Id` of the base material's resource.
    pub base_material_id: Guid,
    /// Static switches.
    pub static_switches: Vec<StaticSwitchParameter>,
    /// Static component masks.
    pub component_masks: Vec<StaticComponentMaskParameter>,
    /// Normal parameters.
    pub normal_parameters: Vec<NormalParameter>,
    /// Terrain layer weights.
    pub terrain_layer_weights: Vec<TerrainLayerWeightParameter>,
}

/// One quality level's resource.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualityResource {
    /// Quality-mask bit (0 = high, 1 = low; UE3 `EMaterialShaderQuality`
    /// names TENTATIVE).
    pub quality: u32,
    /// The resource.
    pub resource: MaterialResource,
    /// Static parameter set (instances only).
    pub static_parameters: Option<StaticParameterSet>,
}

/// Decoded native tail of a material or material instance.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct MaterialNative {
    /// `QualityMask`; `None` for an instance without a static permutation
    /// resource (nothing is serialized then).
    pub quality_mask: Option<u32>,
    /// One entry per set mask bit, low bit first.
    pub resources: Vec<QualityResource>,
}

fn read_bool32(r: &mut Reader<'_>, what: &'static str) -> ObjResult<bool> {
    let at = r.position();
    match r.read_u32()? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(ObjectError::Malformed {
            what,
            offset: at,
            detail: format!("boolean holds {v}"),
        }),
    }
}

fn read_guid(r: &mut Reader<'_>) -> ObjResult<Guid> {
    Ok(r.read_guid()?)
}

/// Read an `FString` and refuse the two encodings this build's writer never
/// produces (so that every accepted value re-encodes to the same bytes): an
/// empty string stored with a terminator (length 1 or -1; the writer stores
/// an empty string as length 0) and a UTF-16 string whose characters all fit
/// in one byte (the writer, `operator<<(FArchive&, FString&)`, picks UTF-16
/// only when `appIsPureAnsi` fails, i.e. for a character of 0x100 or above).
fn read_canonical_fstring(r: &mut Reader<'_>, what: &'static str) -> ObjResult<String> {
    let at = r.position();
    let len = r.read_i32()?;
    r.seek(at)?;
    let s = r.read_fstring()?;
    let non_canonical = match len {
        1 | -1 => true,
        l if l < 0 => s.chars().all(|c| u32::from(c) <= 0xFF),
        _ => false,
    };
    if non_canonical {
        return Err(ObjectError::Malformed {
            what,
            offset: at,
            detail: format!("non-canonical string encoding (length {len})"),
        });
    }
    Ok(s)
}

/// Read one `FMaterialResource`.
pub fn read_material_resource(r: &mut Reader<'_>) -> ObjResult<MaterialResource> {
    let n = r.read_count("CompileErrors", 4)?;
    let mut compile_errors = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        compile_errors.push(read_canonical_fstring(r, "CompileErrors")?);
    }
    let n = r.read_count("TextureDependencyLengthMap", 8)?;
    let mut texture_dependency_lengths = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let expression = r.read_package_index()?;
        let length = r.read_i32()?;
        texture_dependency_lengths.push(DependencyLength { expression, length });
    }
    let max_texture_dependency_length = r.read_i32()?;
    let id = read_guid(r)?;
    let num_user_tex_coords = r.read_u32()?;
    let uniform_expression_textures =
        r.read_tarray("UniformExpressionTextures", 4, Reader::read_package_index)?;
    let uses_scene_color = read_bool32(r, "bUsesSceneColor")?;
    let uses_scene_depth = read_bool32(r, "bUsesSceneDepth")?;
    let uses_dynamic_parameter = read_bool32(r, "bUsesDynamicParameter")?;
    let uses_lightmap_uvs = read_bool32(r, "bUsesLightmapUVs")?;
    let uses_vertex_position_offset = read_bool32(r, "bUsesMaterialVertexPositionOffset")?;
    let using_transforms = r.read_u32()?;
    let texture_lookups = r.read_tarray("TextureLookups", TEXTURE_LOOKUP_SIZE, |r| {
        Ok(TextureLookup {
            tex_coord_index: r.read_i32()?,
            texture_index: r.read_i32()?,
            u_scale: r.read_f32()?,
            v_scale: r.read_f32()?,
        })
    })?;
    let legacy_u32 = r.read_u32()?;
    let resource_u32 = [r.read_u32()?, r.read_u32()?, r.read_u32()?];
    Ok(MaterialResource {
        compile_errors,
        texture_dependency_lengths,
        max_texture_dependency_length,
        id,
        num_user_tex_coords,
        uniform_expression_textures,
        uses_scene_color,
        uses_scene_depth,
        uses_dynamic_parameter,
        uses_lightmap_uvs,
        uses_vertex_position_offset,
        using_transforms,
        texture_lookups,
        legacy_u32,
        resource_u32,
    })
}

/// Read one `FStaticParameterSet`.
pub fn read_static_parameter_set(r: &mut Reader<'_>) -> ObjResult<StaticParameterSet> {
    let base_material_id = read_guid(r)?;
    let n = r.read_count("StaticSwitchParameters", 32)?;
    let mut static_switches = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        static_switches.push(StaticSwitchParameter {
            name: r.read_fname()?,
            value: read_bool32(r, "StaticSwitchParameter.Value")?,
            overridden: read_bool32(r, "StaticSwitchParameter.bOverride")?,
            expression_guid: read_guid(r)?,
        });
    }
    let n = r.read_count("StaticComponentMaskParameters", 44)?;
    let mut component_masks = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let name = r.read_fname()?;
        let mut mask = [false; 4];
        for m in &mut mask {
            *m = read_bool32(r, "StaticComponentMaskParameter.Mask")?;
        }
        component_masks.push(StaticComponentMaskParameter {
            name,
            mask,
            overridden: read_bool32(r, "StaticComponentMaskParameter.bOverride")?,
            expression_guid: read_guid(r)?,
        });
    }
    let n = r.read_count("NormalParameters", 29)?;
    let mut normal_parameters = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        normal_parameters.push(NormalParameter {
            name: r.read_fname()?,
            compression_settings: r.read_u8()?,
            overridden: read_bool32(r, "NormalParameter.bOverride")?,
            expression_guid: read_guid(r)?,
        });
    }
    let n = r.read_count("TerrainLayerWeightParameters", 32)?;
    let mut terrain_layer_weights = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        terrain_layer_weights.push(TerrainLayerWeightParameter {
            name: r.read_fname()?,
            weightmap_index: r.read_i32()?,
            overridden: read_bool32(r, "TerrainLayerWeightParameter.bOverride")?,
            expression_guid: read_guid(r)?,
        });
    }
    Ok(StaticParameterSet {
        base_material_id,
        static_switches,
        component_masks,
        normal_parameters,
        terrain_layer_weights,
    })
}

/// Decode the native tail starting at payload offset `start`. The decoder
/// must end exactly at the end of `data` (the payload).
pub fn decode_material_native(
    data: &[u8],
    start: usize,
    kind: NativeKind,
) -> ObjResult<MaterialNative> {
    let mut r = Reader::at(data, start)?;
    let with_static = match kind {
        NativeKind::None
        | NativeKind::Instance {
            static_permutation: false,
        } => {
            if r.remaining() != 0 {
                return Err(ObjectError::Malformed {
                    what: "material native data",
                    offset: start,
                    detail: format!(
                        "{} unexpected bytes after the tagged properties",
                        r.remaining()
                    ),
                });
            }
            return Ok(MaterialNative::default());
        }
        NativeKind::Material => false,
        NativeKind::Instance { .. } => true,
    };
    let at = r.position();
    let quality_mask = r.read_u32()?;
    if quality_mask >> MAX_QUALITY_LEVELS != 0 {
        return Err(ObjectError::Malformed {
            what: "QualityMask",
            offset: at,
            detail: format!("unknown quality bits in {quality_mask:#x}"),
        });
    }
    let mut resources = Vec::new();
    for quality in 0..MAX_QUALITY_LEVELS {
        if quality_mask & (1 << quality) == 0 {
            continue;
        }
        let resource = read_material_resource(&mut r)?;
        let static_parameters = if with_static {
            Some(read_static_parameter_set(&mut r)?)
        } else {
            None
        };
        resources.push(QualityResource {
            quality,
            resource,
            static_parameters,
        });
    }
    if r.remaining() != 0 {
        return Err(ObjectError::Malformed {
            what: "material native data",
            offset: r.position(),
            detail: format!("{} bytes left after the last resource", r.remaining()),
        });
    }
    Ok(MaterialNative {
        quality_mask: Some(quality_mask),
        resources,
    })
}

fn put_count(w: &mut Writer, n: usize) -> Option<()> {
    w.i32(i32::try_from(n).ok()?);
    Some(())
}

fn put_bool(w: &mut Writer, b: bool) {
    w.u32(u32::from(b));
}

/// Encode one `FMaterialResource` (inverse of [`read_material_resource`]).
pub fn encode_material_resource(w: &mut Writer, m: &MaterialResource) -> Option<()> {
    put_count(w, m.compile_errors.len())?;
    for e in &m.compile_errors {
        if !w.fstring(e) {
            return None;
        }
    }
    put_count(w, m.texture_dependency_lengths.len())?;
    for d in &m.texture_dependency_lengths {
        w.i32(d.expression.0);
        w.i32(d.length);
    }
    w.i32(m.max_texture_dependency_length);
    w.guid(m.id);
    w.u32(m.num_user_tex_coords);
    put_count(w, m.uniform_expression_textures.len())?;
    for t in &m.uniform_expression_textures {
        w.i32(t.0);
    }
    for b in [
        m.uses_scene_color,
        m.uses_scene_depth,
        m.uses_dynamic_parameter,
        m.uses_lightmap_uvs,
        m.uses_vertex_position_offset,
    ] {
        put_bool(w, b);
    }
    w.u32(m.using_transforms);
    put_count(w, m.texture_lookups.len())?;
    for l in &m.texture_lookups {
        w.i32(l.tex_coord_index);
        w.i32(l.texture_index);
        w.bytes(&l.u_scale.to_le_bytes());
        w.bytes(&l.v_scale.to_le_bytes());
    }
    w.u32(m.legacy_u32);
    for v in m.resource_u32 {
        w.u32(v);
    }
    Some(())
}

/// Encode one `FStaticParameterSet` (inverse of [`read_static_parameter_set`]).
pub fn encode_static_parameter_set(w: &mut Writer, s: &StaticParameterSet) -> Option<()> {
    w.guid(s.base_material_id);
    put_count(w, s.static_switches.len())?;
    for p in &s.static_switches {
        w.fname(p.name);
        put_bool(w, p.value);
        put_bool(w, p.overridden);
        w.guid(p.expression_guid);
    }
    put_count(w, s.component_masks.len())?;
    for p in &s.component_masks {
        w.fname(p.name);
        for m in p.mask {
            put_bool(w, m);
        }
        put_bool(w, p.overridden);
        w.guid(p.expression_guid);
    }
    put_count(w, s.normal_parameters.len())?;
    for p in &s.normal_parameters {
        w.fname(p.name);
        w.u8(p.compression_settings);
        put_bool(w, p.overridden);
        w.guid(p.expression_guid);
    }
    put_count(w, s.terrain_layer_weights.len())?;
    for p in &s.terrain_layer_weights {
        w.fname(p.name);
        w.i32(p.weightmap_index);
        put_bool(w, p.overridden);
        w.guid(p.expression_guid);
    }
    Some(())
}

/// Encode a native tail (inverse of [`decode_material_native`]). Returns
/// `None` when the value cannot be encoded for `kind` (inconsistent mask and
/// resources, static parameters on a material, a length beyond `i32`).
pub fn encode_material_native(n: &MaterialNative, kind: NativeKind) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    let with_static = match kind {
        NativeKind::None
        | NativeKind::Instance {
            static_permutation: false,
        } => {
            return (n.quality_mask.is_none() && n.resources.is_empty()).then(Vec::new);
        }
        NativeKind::Material => false,
        NativeKind::Instance { .. } => true,
    };
    let mask = n.quality_mask?;
    if mask >> MAX_QUALITY_LEVELS != 0 {
        return None;
    }
    let expected: Vec<u32> = (0..MAX_QUALITY_LEVELS)
        .filter(|q| mask & (1 << q) != 0)
        .collect();
    let got: Vec<u32> = n.resources.iter().map(|r| r.quality).collect();
    if expected != got {
        return None;
    }
    w.u32(mask);
    for q in &n.resources {
        encode_material_resource(&mut w, &q.resource)?;
        match (&q.static_parameters, with_static) {
            (Some(s), true) => encode_static_parameter_set(&mut w, s)?,
            (None, false) => {}
            _ => return None,
        }
    }
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Property helpers
// ---------------------------------------------------------------------------

fn key_of(p: &Property) -> (String, i32) {
    (p.name.to_ascii_lowercase(), p.array_index)
}

/// Merge `from` onto `into` the way class defaults merge: tagged structs
/// member-wise, everything else replaced.
pub fn merge_properties(into: &mut Vec<Property>, from: &[Property]) {
    for p in from {
        match into.iter_mut().find(|q| key_of(q) == key_of(p)) {
            Some(q) => merge_value(&mut q.value, &p.value),
            None => into.push(p.clone()),
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
        ) => merge_properties(a, b),
        (slot, v) => *slot = v.clone(),
    }
}

/// Value of property `name` (array index 0), case-insensitive.
pub fn prop<'p>(props: &'p [Property], name: &str) -> Option<&'p Value> {
    props
        .iter()
        .find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

fn value_f32(v: &Value) -> Option<f32> {
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

fn value_object(v: &Value) -> Option<&str> {
    match v {
        Value::Object(o) | Value::Interface(o) if o.index != 0 => Some(o.path.as_str()),
        _ => None,
    }
}

fn value_name(v: &Value) -> Option<&str> {
    match v {
        Value::Name(n) | Value::Enum(n) | Value::Str(n) => Some(n.as_str()),
        _ => None,
    }
}

/// A `Guid` struct value as its four members (`None` for the zero GUID).
fn guid_value(v: &Value) -> Option<[i32; 4]> {
    let f = struct_fields(v)?;
    let g = [
        get_i32(f, "A")?,
        get_i32(f, "B")?,
        get_i32(f, "C")?,
        get_i32(f, "D")?,
    ];
    (g != [0; 4]).then_some(g)
}

fn struct_fields(v: &Value) -> Option<&[Property]> {
    match v {
        Value::Struct { fields, .. } => Some(fields),
        _ => None,
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

fn get_name<'p>(props: &'p [Property], name: &str) -> Option<&'p str> {
    prop(props, name).and_then(value_name)
}

/// `LinearColor` (R, G, B, A floats) or `Vector` / `Vector4` value as RGBA.
fn value_linear_color(v: &Value) -> Option<[f32; 4]> {
    let f = struct_fields(v)?;
    let c = |n: &str, d: f32| get_f32(f, n).unwrap_or(d);
    if prop(f, "R").is_some() || prop(f, "G").is_some() || prop(f, "B").is_some() {
        Some([c("R", 0.0), c("G", 0.0), c("B", 0.0), c("A", 1.0)])
    } else if prop(f, "X").is_some() {
        Some([c("X", 0.0), c("Y", 0.0), c("Z", 0.0), c("W", 1.0)])
    } else {
        None
    }
}

/// `Color` (bytes B, G, R, A) converted the way `FLinearColor(FColor)` does
/// in this build: R, G, B through `(c / 255) ^ 2.2` (the executable's
/// `PowOneOver255Table`), A linearly. CONFIRMED from the executable.
///
/// The power is [`crate::portable_math::powf`], not the platform's C
/// library (whose `powf` differs between systems in the last bit: the macOS
/// one returns the neighbouring value for the bytes 79 and 198), so every
/// system converts a colour to the same values.
pub fn color_to_linear(bgra: [u8; 4]) -> [f32; 4] {
    let g = |b: u8| crate::portable_math::powf(f32::from(b) / 255.0, 2.2);
    [
        g(bgra[2]),
        g(bgra[1]),
        g(bgra[0]),
        f32::from(bgra[3]) / 255.0,
    ]
}

fn value_color_bytes(v: &Value) -> Option<[u8; 4]> {
    let f = struct_fields(v)?;
    let b = |n: &str| match prop(f, n) {
        Some(Value::Byte(x)) => *x,
        Some(Value::Int(x)) => u8::try_from(*x).unwrap_or(0),
        _ => 0,
    };
    Some([b("B"), b("G"), b("R"), b("A")])
}

// ---------------------------------------------------------------------------
// Decoded material objects
// ---------------------------------------------------------------------------

/// A decoded material, material instance or material function export.
#[derive(Debug, Clone, Serialize)]
pub struct MaterialObject {
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Qualified class path.
    pub class_path: String,
    /// Material class.
    pub class: MaterialClass,
    /// `RF_ClassDefaultObject` is set.
    pub is_default_object: bool,
    /// Tagged properties as stored.
    pub tagged: Vec<Property>,
    /// Tagged properties merged onto the class defaults (structs member-wise).
    pub properties: Vec<Property>,
    /// Native tail.
    pub native: MaterialNative,
    /// Static parameter names of the native data resolved through the
    /// package's name table (instances with a static permutation resource).
    pub static_parameters: ResolvedStaticParameters,
    /// Payload offset just after the tagged properties.
    pub properties_end: usize,
    /// `SerialSize`.
    pub payload_size: usize,
}

/// Overriding static parameters of an instance, by name (lower case).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResolvedStaticParameters {
    /// Static switches with `bOverride` set.
    pub switches: BTreeMap<String, bool>,
    /// Static component masks with `bOverride` set.
    pub component_masks: BTreeMap<String, [bool; 4]>,
}

impl MaterialObject {
    /// `Parent` of an instance.
    pub fn parent(&self) -> Option<&str> {
        if self.class.is_instance() {
            get_object(&self.properties, "Parent")
        } else {
            None
        }
    }
}

/// Loaded expressions by (package name, path), both lower case.
type ExpressionCache = HashMap<(String, String), Option<Rc<ExpressionNode>>>;

/// Decodes material exports of a [`PackageSet`], caching class information.
pub struct MaterialDecoder<'a> {
    set: &'a PackageSet,
    classes: RefCell<HashMap<String, Option<MaterialClass>>>,
    defaults: RefCell<HashMap<String, Arc<Vec<Property>>>>,
    expressions: RefCell<ExpressionCache>,
    textures: TextureDecoder<'a>,
    texture_info: RefCell<HashMap<String, TextureInfo>>,
}

/// Longest edge of a texture whose largest mip is checked for being one
/// colour everywhere ([`TextureStats::uniform`]): a small mip of a large
/// texture can average sparse detail (stars, thin lines) away, so only a
/// texture this small is ever called uniform.
pub const UNIFORM_MAX_EDGE: u32 = 256;

/// What the reduction knows about a referenced texture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct TextureInfo {
    /// Statistics of a small mip (`None`: not found, not a 2D texture with
    /// stored texels, or not decodable).
    stats: Option<TextureStats>,
    /// The object is filled while the game runs (a render target, a movie
    /// or a scripted texture): there are no texels to convert.
    runtime: bool,
    /// The object is a cube map: whatever expression samples it, the
    /// lookup is by a direction and has no place on the surface.
    cube: bool,
}

impl<'a> MaterialDecoder<'a> {
    /// Decoder over `set`.
    pub fn new(set: &'a PackageSet) -> MaterialDecoder<'a> {
        MaterialDecoder {
            set,
            classes: RefCell::default(),
            defaults: RefCell::default(),
            expressions: RefCell::default(),
            textures: TextureDecoder::new(set),
            texture_info: RefCell::default(),
        }
    }

    /// The package set.
    pub fn set(&self) -> &'a PackageSet {
        self.set
    }

    /// What the texture at `path` holds on average, looked for in the
    /// packages `near` (the material's own, then its chain's) and then in
    /// the whole set. `None` when it is not found or has no texels stored
    /// in its package for a small mip.
    pub fn texture_stats(&self, near: &[Arc<LoadedPackage>], path: &str) -> Option<TextureStats> {
        self.texture_info(near, path).stats
    }

    /// Tells the decoder what the texture at `path` is instead of letting
    /// it read the packages: its statistics (`None`: unknown), whether it
    /// is filled at run time and whether it is a cube map. For a caller
    /// that knows better than the packages at hand, and for tests whose
    /// textures have no texels.
    pub fn assume_texture(
        &self,
        path: &str,
        stats: Option<TextureStats>,
        runtime: bool,
        cube: bool,
    ) {
        self.texture_info.borrow_mut().insert(
            path.to_ascii_lowercase(),
            TextureInfo {
                stats,
                runtime,
                cube,
            },
        );
    }

    fn texture_info(&self, near: &[Arc<LoadedPackage>], path: &str) -> TextureInfo {
        let key = path.to_ascii_lowercase();
        if let Some(i) = self.texture_info.borrow().get(&key) {
            return *i;
        }
        let info = self.read_texture_info(near, path);
        self.texture_info.borrow_mut().insert(key, info);
        info
    }

    fn read_texture_info(&self, near: &[Arc<LoadedPackage>], path: &str) -> TextureInfo {
        let located = near
            .iter()
            .find_map(|lp| lp.export_by_qualified(path).map(|i| (lp.clone(), i)))
            .or_else(|| self.set.locate(path));
        let Some((lp, index)) = located else {
            return TextureInfo::default();
        };
        let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let runtime = ["rendertarget", "texturemovie", "scriptedtexture"]
            .iter()
            .any(|k| class.contains(k));
        let cube = class.contains("texturecube");
        TextureInfo {
            stats: if runtime {
                None
            } else if cube {
                self.read_cube_stats(&lp, index)
            } else {
                self.read_texture_stats(&lp, index)
            },
            runtime,
            cube,
        }
    }

    /// The mean of a cube map's faces (`FacePosX` ... `FaceNegZ`, each a
    /// 2D texture): every face that is found and readable counts the same.
    /// A cube map is never called uniform.
    fn read_cube_stats(&self, lp: &Arc<LoadedPackage>, index: usize) -> Option<TextureStats> {
        let cube = self.set.decode(lp, index).ok()?;
        let mut sum = [0.0f32; 4];
        let mut chroma = 0.0f32;
        let mut faces = 0u8;
        for name in [
            "FacePosX", "FaceNegX", "FacePosY", "FaceNegY", "FacePosZ", "FaceNegZ",
        ] {
            let Some(path) = get_object(&cube.properties, name) else {
                continue;
            };
            let located = lp
                .export_by_qualified(path)
                .map(|i| (lp.clone(), i))
                .or_else(|| self.set.locate(path));
            let Some(s) = located.and_then(|(flp, i)| self.read_texture_stats(&flp, i)) else {
                continue;
            };
            for (slot, v) in sum.iter_mut().zip(s.mean) {
                *slot += v;
            }
            chroma += s.chroma;
            faces += 1;
        }
        (faces > 0).then(|| {
            let n = f32::from(faces);
            TextureStats {
                mean: sum.map(|v| v / n),
                chroma: chroma / n,
                uniform: false,
            }
        })
    }

    /// Statistics of the largest mip within [`STATS_MAX_EDGE`] that is
    /// stored in the package itself (the small mips are; nothing is read
    /// from a texture file cache). `uniform` is decided on the texture's
    /// largest mip, and only when that is within [`UNIFORM_MAX_EDGE`].
    fn read_texture_stats(&self, lp: &Arc<LoadedPackage>, index: usize) -> Option<TextureStats> {
        let tex = self.textures.decode(lp, index).ok()?;
        let format = tex.format?;
        let srgb = tex.props.srgb.unwrap_or(false);
        let payload = lp.package.export_data(tex.export_index).ok()?;
        let first = tex.first_stored_mip()?;
        let mips = tex.mips();
        let read = |level: usize| -> Option<TextureStats> {
            let mip = mips.get(level)?;
            let (w, h) = (
                u32::try_from(mip.size_x).ok()?,
                u32::try_from(mip.size_y).ok()?,
            );
            let texels = tex.load_mip(payload, None, level).ok()?;
            let rgba = decode_to_rgba8(format, w, h, &texels).ok()?;
            TextureStats::of_rgba8(&rgba, srgb)
        };
        let edge = |level: usize| -> Option<u32> {
            let mip = mips.get(level)?;
            Some(u32::try_from(mip.size_x.max(mip.size_y)).ok()?.max(1))
        };
        // A texture without a mip that small (one stored without a mip
        // chain) is read whole when it is not large.
        let small = (first..mips.len())
            .find(|l| edge(*l).is_some_and(|e| e <= STATS_MAX_EDGE))
            .or_else(|| {
                let last = mips.len().checked_sub(1)?;
                edge(last)
                    .is_some_and(|e| e <= UNIFORM_MAX_EDGE * 4)
                    .then_some(last)
            })?;
        let mut stats = read(small)?;
        stats.uniform = stats.uniform
            && edge(first).is_some_and(|e| e <= UNIFORM_MAX_EDGE)
            && (small == first || read(first).is_some_and(|s| s.uniform));
        Some(stats)
    }

    /// Material class of the class at qualified `class_path`.
    pub fn class_of(&self, class_path: &str) -> Option<MaterialClass> {
        let key = class_path.to_ascii_lowercase();
        if let Some(c) = self.classes.borrow().get(&key) {
            return *c;
        }
        let c = MaterialClass::classify(class_path, &self.set.super_chain(class_path));
        self.classes.borrow_mut().insert(key, c);
        c
    }

    /// Material class of export `index`, if it is one.
    pub fn export_class(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Option<(String, MaterialClass)> {
        let class_path =
            crate::object::export_class_path(&lp.package, Some(&lp.name), index).ok()?;
        let c = self.class_of(&class_path)?;
        Some((class_path, c))
    }

    /// Merged class defaults of `class_path` as a property list.
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

    /// Decode export `index` of `lp` strictly: prelude, tagged properties and
    /// native data must consume exactly `SerialSize` bytes.
    pub fn decode(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<MaterialObject, MaterialError> {
        let Some((class_path, class)) = self.export_class(lp, index) else {
            let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
                .unwrap_or_default();
            return Err(MaterialError::NotAMaterial {
                export: index,
                class,
            });
        };
        let entry = lp.package.export(index)?;
        let is_default_object = entry.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0;
        let obj = self.set.decode(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let defaults = self.class_defaults(&class_path);
        let mut properties: Vec<Property> = defaults.as_ref().clone();
        merge_properties(&mut properties, &obj.properties);
        let kind = if is_default_object {
            NativeKind::None
        } else if class.is_material() {
            NativeKind::Material
        } else if class.is_instance() {
            NativeKind::Instance {
                static_permutation: get_bool(&obj.properties, "bHasStaticPermutationResource")
                    .unwrap_or(false),
            }
        } else {
            NativeKind::None
        };
        let native = decode_material_native(payload, obj.properties_end, kind)?;
        let static_parameters =
            resolve_static(&lp.package, &native).map_err(|detail| MaterialError::Malformed {
                path: obj.path.clone(),
                detail,
            })?;
        Ok(MaterialObject {
            export_index: index,
            path: obj.path,
            class_path,
            class,
            is_default_object,
            tagged: obj.properties,
            properties,
            native,
            static_parameters,
            properties_end: obj.properties_end,
            payload_size: payload.len(),
        })
    }
}

/// The native kind of a decoded object (for re-encoding).
pub fn native_kind_of(m: &MaterialObject) -> NativeKind {
    if m.is_default_object {
        NativeKind::None
    } else if m.class.is_material() {
        NativeKind::Material
    } else if m.class.is_instance() {
        NativeKind::Instance {
            static_permutation: get_bool(&m.tagged, "bHasStaticPermutationResource")
                .unwrap_or(false),
        }
    } else {
        NativeKind::None
    }
}

fn resolve_static(pkg: &Package, n: &MaterialNative) -> Result<ResolvedStaticParameters, String> {
    let mut out = ResolvedStaticParameters::default();
    // The first resource's set wins (the quality levels carry the same set in
    // the shipped data; see MATERIALS.md).
    for q in n.resources.iter().rev() {
        let Some(s) = &q.static_parameters else {
            continue;
        };
        for p in &s.static_switches {
            let name = pkg.try_fname(p.name).map_err(|e| e.to_string())?;
            if p.overridden {
                out.switches.insert(name.to_ascii_lowercase(), p.value);
            }
        }
        for p in &s.component_masks {
            let name = pkg.try_fname(p.name).map_err(|e| e.to_string())?;
            if p.overridden {
                out.component_masks
                    .insert(name.to_ascii_lowercase(), p.mask);
            }
        }
        for p in &s.normal_parameters {
            pkg.try_fname(p.name).map_err(|e| e.to_string())?;
        }
        for p in &s.terrain_layer_weights {
            pkg.try_fname(p.name).map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Expression graph
// ---------------------------------------------------------------------------

/// One `MaterialExpression*` subobject with its properties merged onto its
/// class defaults.
#[derive(Debug, Clone, Serialize)]
pub struct ExpressionNode {
    /// Qualified object path.
    pub path: String,
    /// Short class name (`TextureSample`, `Multiply`, ...).
    pub kind: String,
    /// Properties (class defaults merged with the tags).
    pub properties: Vec<Property>,
    /// Bytes after the tagged properties (always 0 in the shipped data).
    pub native_tail: usize,
}

/// A decoded `ExpressionInput` / `MaterialInput` link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExprLink {
    /// Expression path (`None` when unconnected).
    pub expression: Option<String>,
    /// Output of the expression.
    pub output_index: i32,
    /// Channel mask (R, G, B, A) when `Mask` is non-zero.
    pub mask: Option<[bool; 4]>,
}

/// Read an `ExpressionInput`-shaped struct value.
pub fn expr_link(v: &Value) -> ExprLink {
    let Some(f) = struct_fields(v) else {
        return ExprLink {
            expression: None,
            output_index: 0,
            mask: None,
        };
    };
    let i = |n: &str| get_i32(f, n).unwrap_or(0);
    let mask = (i("Mask") != 0).then(|| {
        [
            i("MaskR") != 0,
            i("MaskG") != 0,
            i("MaskB") != 0,
            i("MaskA") != 0,
        ]
    });
    ExprLink {
        expression: get_object(f, "Expression").map(str::to_owned),
        output_index: i("OutputIndex"),
        mask,
    }
}

impl ExpressionNode {
    /// Link stored in property `name`.
    pub fn link(&self, name: &str) -> Option<ExprLink> {
        let l = expr_link(prop(&self.properties, name)?);
        l.expression.is_some().then_some(l)
    }

    /// Float property.
    pub fn f32(&self, name: &str) -> Option<f32> {
        get_f32(&self.properties, name)
    }

    /// Bool property.
    pub fn bool(&self, name: &str) -> Option<bool> {
        get_bool(&self.properties, name)
    }

    /// Object property path.
    pub fn object(&self, name: &str) -> Option<&str> {
        get_object(&self.properties, name)
    }

    /// Name property.
    pub fn name(&self, name: &str) -> Option<&str> {
        get_name(&self.properties, name)
    }
}

impl MaterialDecoder<'_> {
    /// Load the expression at `path`, looking in `lp` first and then in the
    /// whole set. `None` when it is missing or not an expression.
    pub fn expression(&self, lp: &LoadedPackage, path: &str) -> Option<Rc<ExpressionNode>> {
        let key = (lp.name.to_ascii_lowercase(), path.to_ascii_lowercase());
        if let Some(n) = self.expressions.borrow().get(&key) {
            return n.clone();
        }
        let node = self.load_expression(lp, path);
        self.expressions.borrow_mut().insert(key, node.clone());
        node
    }

    fn load_expression(&self, lp: &LoadedPackage, path: &str) -> Option<Rc<ExpressionNode>> {
        let (owner, index) = match lp.export_by_qualified(path) {
            Some(i) => (None, i),
            None => {
                let (o, i) = self.set.locate(path)?;
                (Some(o), i)
            }
        };
        let pkg: &LoadedPackage = owner.as_deref().unwrap_or(lp);
        let class_path =
            crate::object::export_class_path(&pkg.package, Some(&pkg.name), index).ok()?;
        let kind = expression_kind(&class_path)?.to_owned();
        let obj = self.set.decode(pkg, index).ok()?;
        let mut properties: Vec<Property> = self.class_defaults(&class_path).as_ref().clone();
        merge_properties(&mut properties, &obj.properties);
        Some(Rc::new(ExpressionNode {
            path: obj.path.clone(),
            kind,
            native_tail: obj.native_tail(),
            properties,
        }))
    }

    /// Locate a material object by path, preferring `lp`.
    fn locate_material(
        &self,
        lp: &Arc<LoadedPackage>,
        path: &str,
    ) -> Option<(Arc<LoadedPackage>, usize)> {
        if let Some(i) = lp.export_by_qualified(path) {
            return Some((lp.clone(), i));
        }
        self.set.locate(path)
    }

    /// Resolve the instance chain of export `index`: the object itself, its
    /// parent, ..., up to a `Material` (or until the chain breaks).
    pub fn resolve_chain(&self, lp: &Arc<LoadedPackage>, index: usize) -> MaterialChain {
        let mut chain = MaterialChain::default();
        let mut cur = Some((lp.clone(), index));
        let mut seen = BTreeSet::new();
        while let Some((pkg, i)) = cur.take() {
            let m = match self.decode(&pkg, i) {
                Ok(m) => m,
                Err(e) => {
                    chain.error = Some(format!("decoding export {i} of {}: {e}", pkg.name));
                    break;
                }
            };
            if !seen.insert(m.path.to_ascii_lowercase()) || chain.links.len() >= MAX_CHAIN {
                chain.error = Some(format!(
                    "instance chain cycle or longer than {MAX_CHAIN} at {}",
                    m.path
                ));
                break;
            }
            let parent = m.parent().map(str::to_owned);
            let is_instance = m.class.is_instance();
            chain.links.push((pkg.clone(), m));
            if !is_instance {
                break;
            }
            match parent {
                None => {
                    chain.error = Some("instance without a parent".to_owned());
                }
                Some(p) => match self.locate_material(&pkg, &p) {
                    Some(hit) => cur = Some(hit),
                    None => chain.error = Some(format!("parent {p} not found")),
                },
            }
        }
        chain
    }
}

/// Longest instance chain followed.
pub const MAX_CHAIN: usize = 16;
/// Deepest expression recursion followed.
pub const MAX_EXPRESSION_DEPTH: usize = 64;
/// Most expression visits per material input.
pub const MAX_EXPRESSION_VISITS: usize = 20_000;

/// An instance chain: `links[0]` is the starting object, the last link the
/// base `Material` when the chain is complete.
#[derive(Debug, Default)]
pub struct MaterialChain {
    /// Objects, with the package each was found in.
    pub links: Vec<(Arc<LoadedPackage>, MaterialObject)>,
    /// Why the chain stopped before reaching a `Material`.
    pub error: Option<String>,
}

impl MaterialChain {
    /// The base material (last link) when the chain reached one.
    pub fn base(&self) -> Option<&(Arc<LoadedPackage>, MaterialObject)> {
        self.links.last().filter(|(_, m)| m.class.is_material())
    }
}

/// Parameter values in effect for a material, gathered along its instance
/// chain (the nearest instance wins).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParameterValues {
    /// Scalar parameters by lower-case name.
    pub scalars: BTreeMap<String, f32>,
    /// Vector parameters (linear RGBA) by lower-case name.
    pub vectors: BTreeMap<String, [f32; 4]>,
    /// Texture parameters by lower-case name: the nearest non-null override;
    /// `None` when every instance naming the parameter sets it to null (the
    /// expression's own texture is then used, as in the engine).
    pub textures: BTreeMap<String, Option<String>>,
    /// Static switches.
    pub switches: BTreeMap<String, bool>,
    /// Static component masks.
    pub component_masks: BTreeMap<String, [bool; 4]>,
    /// Parameters in effect that come from time-varying instances (curves
    /// sampled at their first key).
    pub time_varying: usize,
}

fn param_elements(props: &[Property], name: &str) -> Vec<Vec<Property>> {
    match prop(props, name) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| struct_fields(v).map(<[Property]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

/// First key of an `InterpCurve*` value: (`OutVal` value).
fn curve_first_out(v: Option<&Value>) -> Option<&Value> {
    let f = struct_fields(v?)?;
    match prop(f, "Points")? {
        Value::Array(points) => {
            let p = struct_fields(points.first()?)?;
            prop(p, "OutVal")
        }
        _ => None,
    }
}

impl ParameterValues {
    /// Gather the parameters of `chain` (instances only; the base material
    /// contributes nothing: its parameter defaults live in the expressions).
    pub fn from_chain(chain: &MaterialChain) -> ParameterValues {
        let mut out = ParameterValues::default();
        for (_, m) in &chain.links {
            if !m.class.is_instance() {
                continue;
            }
            let tv = m.class == MaterialClass::MaterialInstanceTimeVarying;
            for e in param_elements(&m.properties, "ScalarParameterValues") {
                let Some(name) = get_name(&e, "ParameterName") else {
                    continue;
                };
                let mut v = get_f32(&e, "ParameterValue");
                if tv
                    && let Some(c) =
                        curve_first_out(prop(&e, "ParameterValueCurve")).and_then(value_f32)
                {
                    v = Some(c);
                }
                if let Some(v) = v
                    && let Entry::Vacant(slot) = out.scalars.entry(name.to_ascii_lowercase())
                {
                    slot.insert(v);
                    out.time_varying += usize::from(tv);
                }
            }
            for list in ["VectorParameterValues", "LinearColorParameterValues"] {
                for e in param_elements(&m.properties, list) {
                    let Some(name) = get_name(&e, "ParameterName") else {
                        continue;
                    };
                    let mut v = prop(&e, "ParameterValue").and_then(value_linear_color);
                    if tv
                        && let Some(c) = curve_first_out(prop(&e, "ParameterValueCurve"))
                            .and_then(value_linear_color)
                    {
                        let a = v.map_or(1.0, |x| x[3]);
                        v = Some([
                            c[0],
                            c[1],
                            c[2],
                            if list == "LinearColorParameterValues" {
                                c[3]
                            } else {
                                a
                            },
                        ]);
                    }
                    if let Some(v) = v
                        && let Entry::Vacant(slot) = out.vectors.entry(name.to_ascii_lowercase())
                    {
                        slot.insert(v);
                        out.time_varying += usize::from(tv);
                    }
                }
            }
            // Texture parameters: the first entry of a name in an instance
            // decides for that instance, but a null texture there does not
            // clear the parameter: the render proxy then asks the parent
            // (CONFIRMED from `FMaterialInstanceConstantResource::
            // GetTextureValue` in the executable). So a null entry only
            // stands when no farther instance sets a texture.
            let mut first: BTreeMap<String, Option<String>> = BTreeMap::new();
            for e in param_elements(&m.properties, "TextureParameterValues") {
                let Some(name) = get_name(&e, "ParameterName") else {
                    continue;
                };
                let t = get_object(&e, "ParameterValue").map(str::to_owned);
                first.entry(name.to_ascii_lowercase()).or_insert(t);
            }
            for (name, t) in first {
                match out.textures.entry(name) {
                    Entry::Vacant(slot) => {
                        out.time_varying += usize::from(tv && t.is_some());
                        slot.insert(t);
                    }
                    Entry::Occupied(mut slot) if slot.get().is_none() && t.is_some() => {
                        out.time_varying += usize::from(tv);
                        slot.insert(t);
                    }
                    Entry::Occupied(_) => {}
                }
            }
            for (k, v) in &m.static_parameters.switches {
                out.switches.entry(k.clone()).or_insert(*v);
            }
            for (k, v) in &m.static_parameters.component_masks {
                out.component_masks.entry(k.clone()).or_insert(*v);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Symbolic evaluation
// ---------------------------------------------------------------------------

/// A texture coordinate transform: `uv' = uv[channel] * scale + offset +
/// panning * time`, then a rotation by `rotation_angle + rotation * time`
/// radians about `rotation_center` (UE3 `Rotator`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct UvTransform {
    /// UV channel (`TexCoord` index).
    pub channel: i32,
    /// Tiling.
    pub scale: [f32; 2],
    /// Offset.
    pub offset: [f32; 2],
    /// Panning speed in UV units per second.
    pub panning: [f32; 2],
    /// Rotation speed (radians per second; 0 = none).
    pub rotation: f32,
    /// Fixed rotation angle in radians (a `Rotator` whose time input is a
    /// constant or a scalar parameter). Omitted from JSON when 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub rotation_angle: f32,
    /// Rotation centre.
    pub rotation_center: [f32; 2],
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl UvTransform {
    /// True when a rotation (animated or fixed) is applied.
    pub fn rotated(&self) -> bool {
        self.rotation != 0.0 || self.rotation_angle != 0.0
    }
}

impl Default for UvTransform {
    fn default() -> Self {
        UvTransform {
            channel: 0,
            scale: [1.0, 1.0],
            offset: [0.0, 0.0],
            panning: [0.0, 0.0],
            rotation: 0.0,
            rotation_angle: 0.0,
            rotation_center: [0.5, 0.5],
        }
    }
}

/// What drives a `Panner` / `Rotator`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TimeInput {
    /// Game time times a factor (also the unconnected default, factor 1).
    Linear(f32),
    /// A constant (or scalar parameter): a fixed offset / angle.
    Constant(f32),
    /// Anything else.
    Other,
}

/// A texture sample in the symbolic value: `texture.swizzle * factor + bias`.
///
/// `width` is the number of meaningful output components. It equals the
/// swizzle length, except that a single texture channel combined with an
/// `n`-component constant is broadcast (as UE3 does for `float1 op floatN`):
/// the swizzle stays one channel and `factor` / `bias` carry `n`
/// per-component values (a mask texture tinted by a colour). A one-channel
/// term keeps `factor` / `bias` splatted so that per-component arithmetic
/// broadcasts correctly.
#[derive(Debug, Clone, PartialEq)]
struct TexTerm {
    texture: Option<String>,
    parameter: Option<String>,
    sampler: &'static str,
    uv: UvTransform,
    swizzle: Vec<u8>,
    width: u8,
    factor: [f32; 4],
    bias: [f32; 4],
    vertex_color: bool,
    /// What the texture holds on average, when known.
    stats: Option<TextureStats>,
    /// The sample's coordinates follow the mesh's (false: they came from
    /// something not representable, a view vector for instance, and `uv`
    /// is only a stand-in).
    placed: bool,
}

impl TexTerm {
    /// The term's mean value and component count, when the texture's
    /// statistics are known: what stands in for the sample when another
    /// texture is kept in its place.
    fn mean(&self) -> Option<([f32; 4], u8)> {
        let s = self.stats?;
        let mut m = [0.0f32; 4];
        if let [c] = self.swizzle.as_slice() {
            let v = s.mean[usize::from((*c).min(3))];
            for (slot, (f, b)) in m.iter_mut().zip(self.factor.iter().zip(self.bias.iter())) {
                *slot = v * f + b;
            }
            Some((m, self.width.clamp(1, 4)))
        } else {
            for (i, c) in self.swizzle.iter().enumerate().take(4) {
                m[i] = s.mean[usize::from((*c).min(3))] * self.factor[i] + self.bias[i];
            }
            Some((m, u8::try_from(self.swizzle.len().clamp(1, 4)).unwrap_or(4)))
        }
    }

    /// A sample of more than one channel whose texture is known to carry
    /// colour (`Some(true)`) or to be grey (`Some(false)`).
    fn coloured(&self) -> Option<bool> {
        if self.swizzle.len() < 2 {
            return None;
        }
        self.stats.map(|s| !s.grey())
    }
}

/// A value computed from the texture coordinates: `value` times the
/// product of the factors (see [`UvGradient`]).
#[derive(Debug, Clone, PartialEq)]
struct GradTerm {
    factors: Vec<GradientFactor>,
    value: [f32; 4],
    n: u8,
}

#[derive(Debug, Clone, PartialEq)]
enum Term {
    /// Constant with `n` meaningful components.
    Const([f32; 4], u8),
    /// Texture sample.
    Tex(Box<TexTerm>),
    /// Texture coordinates.
    Uv(UvTransform),
    /// Vertex colour.
    VertexColor,
    /// Game time times a factor.
    Time(f32),
    /// A function of the texture coordinates.
    Grad(Box<GradTerm>),
    /// Not representable: a factor the reduction treats as neutral (1 in a
    /// product, 0 in a sum) next to something it can represent.
    Unknown(String),
    /// Not representable, and a length or position in world units
    /// ([`Lost::is_dimensional`]): it keeps the constants combined with it
    /// (they scale and offset *it*), and becomes a neutral factor once it
    /// is clamped or meets something representable.
    Dim(String),
}

/// A not representable value as a neutral factor.
fn demote(t: Term) -> Term {
    match t {
        Term::Dim(k) => Term::Unknown(k),
        t => t,
    }
}

/// One texture coordinate component as `scale * uv[channel][component] +
/// offset`, when the transform has it that simple (no panning, no rotation).
fn uv_axis(uv: &UvTransform, component: usize) -> Option<GradientFactor> {
    if uv.rotated() || uv.panning != [0.0, 0.0] {
        return None;
    }
    Some(GradientFactor {
        uv_channel: uv.channel,
        component: if component == 0 { "u" } else { "v" },
        scale: *uv.scale.get(component)?,
        offset: *uv.offset.get(component)?,
        power: 1.0,
    })
}

/// A term as one coordinate axis for `AppendVector`: a constant (`None`
/// axis) or a plain single-factor gradient.
fn append_axis(t: &Term) -> Option<(Option<(i32, &'static str)>, f32, f32)> {
    match t {
        Term::Const(v, 1) => Some((None, 0.0, v[0])),
        Term::Grad(g) if g.n == 1 => match g.factors.as_slice() {
            [f] if f.power == 1.0 => Some((
                Some((f.uv_channel, f.component)),
                f.scale * g.value[0],
                f.offset * g.value[0],
            )),
            _ => None,
        },
        _ => None,
    }
}

fn splat(v: f32) -> [f32; 4] {
    [v; 4]
}

fn bcast(v: [f32; 4], n: u8) -> [f32; 4] {
    if n == 1 { splat(v[0]) } else { v }
}

/// Set the components from `n` on (when `n > 1`) to `fill`: a value with
/// fewer than four meaningful components gets neutral padding.
fn pad(mut v: [f32; 4], n: usize, fill: f32) -> [f32; 4] {
    if n > 1 {
        for c in v.iter_mut().skip(n) {
            *c = fill;
        }
    }
    v
}

fn zip(a: [f32; 4], b: [f32; 4], f: impl Fn(f32, f32) -> f32) -> [f32; 4] {
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), f(a[3], b[3])]
}

fn swizzle_mask(sw: &[u8], mask: [bool; 4]) -> Vec<u8> {
    sw.iter()
        .enumerate()
        .filter(|(i, _)| mask.get(*i).copied().unwrap_or(false))
        .map(|(_, c)| *c)
        .collect()
}

fn select4(v: [f32; 4], mask: [bool; 4]) -> ([f32; 4], u8) {
    let mut out = [0.0; 4];
    let mut n = 0usize;
    for (i, m) in mask.iter().enumerate() {
        if *m && let Some(slot) = out.get_mut(n) {
            *slot = v[i];
            n += 1;
        }
    }
    (out, u8::try_from(n.max(1)).unwrap_or(1))
}

/// Evaluation state for one material.
struct Eval<'e, 'a> {
    dec: &'e MaterialDecoder<'a>,
    params: &'e ParameterValues,
    notes: BTreeSet<String>,
    unsupported: BTreeMap<String, usize>,
    reached: BTreeMap<String, usize>,
    extra_textures: BTreeSet<String>,
    visits: usize,
    lossy: bool,
    /// The walk, one line per evaluated link (only when asked for).
    trace: Option<Vec<String>>,
    /// What the link under evaluation lost so far.
    lost: Lost,
    /// What the link evaluated last lost in all.
    last_lost: Lost,
    /// The gradient of the material input reduced last, if it is one.
    gradient: Option<UvGradient>,
    /// The coordinates read last follow the mesh's (see `TexTerm::placed`).
    placed: bool,
    /// The input under reduction is a colour whose average matters more
    /// than its peaks (diffuse, specular): a dropped texture stands in by
    /// its mean. Not so for an emissive colour (a glow keeps its strength
    /// where its mask lets it through) or an opacity (a mean could push a
    /// whole cut-out under its clip value), which drop it as before.
    fold_means: bool,
    /// Packages of the instance chain (texture lookups).
    homes: Vec<Arc<LoadedPackage>>,
    /// Statistics of the textures sampled, by path.
    stats: BTreeMap<String, TextureStats>,
    /// Sampled objects that are filled at run time.
    runtime_textures: BTreeSet<String>,
    /// What each reduced material input lost, by channel key.
    dropped: BTreeMap<&'static str, Vec<&'static str>>,
    /// The gradient of each material input that is one, by channel key.
    gradients: BTreeMap<&'static str, UvGradient>,
}

/// Function-call frame: inputs of the call, evaluated in the caller's frame.
struct Frame<'f> {
    call: Rc<ExpressionNode>,
    caller_pkg: Arc<LoadedPackage>,
    parent: Option<&'f Frame<'f>>,
}

impl Eval<'_, '_> {
    fn note(&mut self, s: impl Into<String>) {
        self.lossy = true;
        if self.notes.len() < 64 {
            self.notes.insert(s.into());
        }
    }

    fn unknown(&mut self, kind: &str) -> Term {
        *self.unsupported.entry(kind.to_owned()).or_insert(0) += 1;
        self.lossy = true;
        if Lost::is_dimensional(kind) {
            Term::Dim(kind.to_owned())
        } else {
            Term::Unknown(kind.to_owned())
        }
    }

    /// Records that the link under evaluation does not have `what`.
    fn lose(&mut self, what: Lost) {
        self.lossy = true;
        self.lost.insert(what);
    }

    /// A value derived from a not representable one keeps its kind (and so
    /// its class) without counting it again.
    fn derived(kind: &str, bounded: bool) -> Term {
        if !bounded && Lost::is_dimensional(kind) {
            Term::Dim(kind.to_owned())
        } else {
            Term::Unknown(kind.to_owned())
        }
    }

    /// Evaluate a link (applying its mask).
    fn link(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        link: &ExprLink,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Option<Term> {
        self.last_lost = Lost::NONE;
        let path = link.expression.as_deref()?;
        if depth > MAX_EXPRESSION_DEPTH || self.visits >= MAX_EXPRESSION_VISITS {
            return Some(self.unknown("depth-or-work-limit"));
        }
        self.visits += 1;
        let Some(node) = self.dec.expression(pkg, path) else {
            return Some(self.unknown("missing-expression"));
        };
        let slot = self.trace.as_mut().map(|t| {
            t.push(String::new());
            t.len() - 1
        });
        // What this link loses is what its inputs lost plus what its own
        // node drops; the link above gets the union.
        let outer = std::mem::take(&mut self.lost);
        let t = self.node(pkg, &node, link.output_index, frame, depth + 1);
        let t = match link.mask {
            Some(m) => self.mask(t, m),
            None => t,
        };
        let mine = self.lost;
        self.lost = outer.with(mine);
        self.last_lost = mine;
        if let Some(line) = slot.and_then(|i| self.trace.as_mut().and_then(|tr| tr.get_mut(i))) {
            let lost = if mine.is_empty() {
                String::new()
            } else {
                format!(" (lost {})", mine.names().join(", "))
            };
            *line = format!(
                "{:indent$}{}{} -> {}{lost}",
                "",
                node.kind,
                describe_node(&node, link),
                describe_term(&t),
                indent = depth.min(MAX_EXPRESSION_DEPTH) * 2
            );
        }
        Some(t)
    }

    fn mask(&mut self, t: Term, m: [bool; 4]) -> Term {
        match t {
            Term::Const(v, _) => {
                let (v, n) = select4(v, m);
                Term::Const(v, n)
            }
            Term::Tex(mut tx) => {
                let broadcast = tx.swizzle.len() == 1 && tx.width > 1;
                if !broadcast {
                    let sw = swizzle_mask(&tx.swizzle, m);
                    if sw.is_empty() {
                        return Term::Tex(tx);
                    }
                    tx.swizzle = sw;
                }
                // A broadcast single channel keeps its channel; the mask
                // selects among the per-component multipliers.
                let (f, n) = select4(tx.factor, m);
                let (b, _) = select4(tx.bias, m);
                tx.width = if broadcast {
                    n
                } else {
                    u8::try_from(tx.swizzle.len()).unwrap_or(4)
                };
                (tx.factor, tx.bias) = if tx.width == 1 {
                    (splat(f[0]), splat(b[0]))
                } else {
                    (f, b)
                };
                Term::Tex(tx)
            }
            // One component of the texture coordinates is a value along
            // that axis (both components are still coordinates).
            Term::Uv(uv) if m[0] != m[1] => match uv_axis(&uv, usize::from(m[1])) {
                Some(f) => Term::Grad(Box::new(GradTerm {
                    factors: vec![f],
                    value: splat(1.0),
                    n: 1,
                })),
                None => self.unknown("coordinate-math"),
            },
            Term::Grad(mut g) => {
                let (v, n) = select4(bcast(g.value, g.n), m);
                (g.value, g.n) = (v, n);
                Term::Grad(g)
            }
            other => other,
        }
    }

    fn input(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        node: &ExpressionNode,
        name: &str,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Option<Term> {
        self.last_lost = Lost::NONE;
        let l = node.link(name)?;
        self.link(pkg, &l, frame, depth)
    }

    fn node(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &Rc<ExpressionNode>,
        output: i32,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        *self.reached.entry(n.kind.clone()).or_insert(0) += 1;
        let kind = n.kind.to_ascii_lowercase();
        let f = |name: &str, d: f32| n.f32(name).unwrap_or(d);
        match kind.as_str() {
            "constant" => Term::Const(splat(f("R", 0.0)), 1),
            "constant2vector" => Term::Const([f("R", 0.0), f("G", 0.0), 0.0, 0.0], 2),
            "constant3vector" => Term::Const([f("R", 0.0), f("G", 0.0), f("B", 0.0), 0.0], 3),
            "constant4vector" => {
                Term::Const([f("R", 0.0), f("G", 0.0), f("B", 0.0), f("A", 0.0)], 4)
            }
            "scalarparameter" => {
                let name = n.name("ParameterName").unwrap_or("").to_ascii_lowercase();
                let v = self
                    .params
                    .scalars
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| f("DefaultValue", 0.0));
                Term::Const(splat(v), 1)
            }
            "vectorparameter" => {
                let name = n.name("ParameterName").unwrap_or("").to_ascii_lowercase();
                let v = self.params.vectors.get(&name).copied().unwrap_or_else(|| {
                    prop(&n.properties, "DefaultValue")
                        .and_then(value_linear_color)
                        .unwrap_or([0.0, 0.0, 0.0, 1.0])
                });
                Term::Const(v, 4)
            }
            "staticboolparameter" => {
                let v = self.static_switch(n);
                Term::Const(splat(if v { 1.0 } else { 0.0 }), 1)
            }
            "time" => Term::Time(1.0),
            "vertexcolor" | "meshemittervertexcolor" => Term::VertexColor,
            "texturecoordinate" => {
                if n.bool("UnMirrorU").unwrap_or(false) || n.bool("UnMirrorV").unwrap_or(false) {
                    self.note("TextureCoordinate un-mirroring ignored");
                }
                Term::Uv(UvTransform {
                    channel: get_i32(&n.properties, "CoordinateIndex").unwrap_or(0),
                    scale: [f("UTiling", 1.0), f("VTiling", 1.0)],
                    ..UvTransform::default()
                })
            }
            "panner" => self.panner(pkg, n, frame, depth),
            "rotator" => self.rotator(pkg, n, frame, depth),
            "bumpoffset" => {
                self.note("BumpOffset (parallax) ignored: plain coordinates used");
                match self.input(pkg, n, "Coordinate", frame, depth) {
                    Some(t) => t,
                    None => Term::Uv(UvTransform::default()),
                }
            }
            // `MaterialExpressionDepthBiasBlend` extends `TextureSample`
            // (class model in Engine.u): its texture with a soft depth fade.
            "depthbiasblend" => {
                self.note("DepthBiasBlend (soft depth fade) ignored: its texture used");
                self.lose(Lost::DEPTH);
                self.texture_sample(pkg, n, "texturesample", frame, depth)
            }
            k if is_texture_sample(k) => self.texture_sample(pkg, n, k, frame, depth),
            "multiply" => self.binary(pkg, n, frame, depth, Op::Mul),
            "add" => self.binary(pkg, n, frame, depth, Op::Add),
            "subtract" => self.binary(pkg, n, frame, depth, Op::Sub),
            "divide" => self.binary(pkg, n, frame, depth, Op::Div),
            "linearinterpolate" => self.lerp(pkg, n, frame, depth),
            "oneminus" => {
                let t = self.input(pkg, n, "Input", frame, depth);
                match t {
                    Some(t) => self.combine(Op::Sub, Some(Term::Const(splat(1.0), 1)), Some(t)),
                    None => self.unknown("OneMinus-unconnected"),
                }
            }
            "componentmask" => {
                let m = [
                    n.bool("R").unwrap_or(false),
                    n.bool("G").unwrap_or(false),
                    n.bool("B").unwrap_or(false),
                    n.bool("A").unwrap_or(false),
                ];
                match self.input(pkg, n, "Input", frame, depth) {
                    Some(t) => self.mask(t, m),
                    None => self.unknown("ComponentMask-unconnected"),
                }
            }
            "staticcomponentmaskparameter" => {
                let name = n.name("ParameterName").unwrap_or("").to_ascii_lowercase();
                let m = self.params.component_masks.get(&name).copied().unwrap_or([
                    n.bool("DefaultR").unwrap_or(false),
                    n.bool("DefaultG").unwrap_or(false),
                    n.bool("DefaultB").unwrap_or(false),
                    n.bool("DefaultA").unwrap_or(false),
                ]);
                match self.input(pkg, n, "Input", frame, depth) {
                    Some(t) => self.mask(t, m),
                    None => self.unknown("StaticComponentMaskParameter-unconnected"),
                }
            }
            "staticswitchparameter" => {
                let v = self.static_switch(n);
                let side = if v { "A" } else { "B" };
                match self.input(pkg, n, side, frame, depth) {
                    Some(t) => t,
                    None => self.unknown("StaticSwitchParameter-unconnected"),
                }
            }
            "constantclamp" => {
                let (lo, hi) = (f("Min", 0.0), f("Max", 1.0));
                match self.input(pkg, n, "Input", frame, depth) {
                    Some(Term::Const(v, k)) => {
                        Term::Const(v.map(|x| x.clamp(lo.min(hi), hi.max(lo))), k)
                    }
                    // A clamped length is a factor in a fixed range.
                    Some(Term::Dim(k) | Term::Unknown(k)) => Term::Unknown(k),
                    Some(t) => {
                        self.note("clamp of a non-constant value ignored");
                        t
                    }
                    None => self.unknown("ConstantClamp-unconnected"),
                }
            }
            "clamp" => {
                let t = self.input(pkg, n, "Input", frame, depth);
                let lo = self.input(pkg, n, "Min", frame, depth);
                let hi = self.input(pkg, n, "Max", frame, depth);
                match (t, lo, hi) {
                    (Some(Term::Const(v, k)), Some(Term::Const(a, _)), Some(Term::Const(b, _))) => {
                        Term::Const(zip(zip(v, a, f32::max), b, f32::min), k)
                    }
                    (Some(Term::Dim(k) | Term::Unknown(k)), _, _) => Term::Unknown(k),
                    (Some(t), _, _) => {
                        self.note("clamp of a non-constant value ignored");
                        t
                    }
                    _ => self.unknown("Clamp-unconnected"),
                }
            }
            "constantbiasscale" => {
                let (bias, scale) = (f("Bias", 1.0), f("Scale", 0.5));
                let t = self.input(pkg, n, "Input", frame, depth);
                let t = self.combine(Op::Add, t, Some(Term::Const(splat(bias), 1)));
                self.combine(Op::Mul, Some(t), Some(Term::Const(splat(scale), 1)))
            }
            "power" => {
                let b = self.input(pkg, n, "Base", frame, depth);
                let e = self.input(pkg, n, "Exponent", frame, depth);
                match (b, e) {
                    (Some(Term::Const(v, k)), Some(Term::Const(x, _))) => {
                        // Portable power, sine and cosine here and below:
                        // folded constants reach converted data.
                        Term::Const(v.map(|c| crate::portable_math::powf(c.max(0.0), x[0])), k)
                    }
                    // (v * f^p)^x = v^x * f^(p x) for the non-negative
                    // bases a gradient has.
                    (Some(Term::Grad(mut g)), Some(Term::Const(x, _))) => {
                        for f in &mut g.factors {
                            f.power *= x[0];
                        }
                        g.value = g
                            .value
                            .map(|c| crate::portable_math::powf(c.max(0.0), x[0]));
                        Term::Grad(g)
                    }
                    (Some(t @ (Term::Dim(_) | Term::Unknown(_))), _) => t,
                    (Some(t), _) => {
                        self.note("Power of a non-constant value ignored");
                        t
                    }
                    _ => self.unknown("Power-unconnected"),
                }
            }
            "desaturation" => {
                let t = self.input(pkg, n, "Input", frame, depth);
                let p = match self.input(pkg, n, "Percent", frame, depth) {
                    Some(Term::Const(v, _)) => Some(v[0]),
                    None => Some(1.0),
                    Some(_) => None,
                };
                let lum = prop(&n.properties, "LuminanceFactors")
                    .and_then(value_linear_color)
                    .unwrap_or([0.3, 0.59, 0.11, 0.0]);
                match (t, p) {
                    (Some(Term::Const(v, k)), Some(p)) => {
                        let l = v[0] * lum[0] + v[1] * lum[1] + v[2] * lum[2];
                        Term::Const(
                            [
                                v[0] + (l - v[0]) * p,
                                v[1] + (l - v[1]) * p,
                                v[2] + (l - v[2]) * p,
                                v[3],
                            ],
                            k,
                        )
                    }
                    (Some(t), _) => {
                        self.note("Desaturation of a non-constant value ignored");
                        t
                    }
                    _ => self.unknown("Desaturation-unconnected"),
                }
            }
            "appendvector" => {
                let a = self.input(pkg, n, "A", frame, depth);
                let b = self.input(pkg, n, "B", frame, depth);
                match (a, b) {
                    (Some(Term::Const(x, na)), Some(Term::Const(y, nb))) => {
                        let mut v = [0.0; 4];
                        let mut k = 0usize;
                        for c in x
                            .iter()
                            .take(usize::from(na))
                            .chain(y.iter().take(usize::from(nb)))
                        {
                            if let Some(s) = v.get_mut(k) {
                                *s = *c;
                                k += 1;
                            }
                        }
                        Term::Const(v, u8::try_from(k.max(1)).unwrap_or(4))
                    }
                    (Some(a), Some(b)) => match (append_axis(&a), append_axis(&b)) {
                        // Coordinates put together from their components:
                        // U from a value along u (or a constant), V from a
                        // value along v (or a constant), of one channel.
                        (Some((ua, us, uo)), Some((va, vs, vo)))
                            if (ua.is_some() || va.is_some())
                                && ua.is_none_or(|(_, c)| c == "u")
                                && va.is_none_or(|(_, c)| c == "v")
                                && match (ua, va) {
                                    (Some((x, _)), Some((y, _))) => x == y,
                                    _ => true,
                                } =>
                        {
                            Term::Uv(UvTransform {
                                channel: ua.or(va).map_or(0, |(c, _)| c),
                                scale: [us, vs],
                                offset: [uo, vo],
                                ..UvTransform::default()
                            })
                        }
                        _ => match (a, b) {
                            (Term::Dim(k) | Term::Unknown(k), _)
                            | (_, Term::Dim(k) | Term::Unknown(k)) => Self::derived(&k, false),
                            _ => self.unknown("AppendVector"),
                        },
                    },
                    _ => self.unknown("AppendVector"),
                }
            }
            "abs" | "floor" | "ceil" | "frac" | "squareroot" | "sine" | "cosine" => {
                let input = self.input(pkg, n, "Input", frame, depth);
                match input {
                    Some(Term::Const(v, k)) => {
                        let period = f("Period", 1.0);
                        let g = |x: f32| -> f32 {
                            match kind.as_str() {
                                "abs" => x.abs(),
                                "floor" => x.floor(),
                                "ceil" => x.ceil(),
                                "frac" => x - x.floor(),
                                "squareroot" => x.max(0.0).sqrt(),
                                "sine" if period > 0.0 => {
                                    crate::portable_math::sinf(x * std::f32::consts::TAU / period)
                                }
                                "sine" => crate::portable_math::sinf(x),
                                _ if period > 0.0 => {
                                    crate::portable_math::cosf(x * std::f32::consts::TAU / period)
                                }
                                _ => crate::portable_math::cosf(x),
                            }
                        };
                        Term::Const(v.map(g), k)
                    }
                    // A function of something not representable is not
                    // representable for the same reason; the periodic ones
                    // and the fraction are bounded.
                    Some(Term::Dim(k) | Term::Unknown(k)) => {
                        Self::derived(&k, matches!(kind.as_str(), "frac" | "sine" | "cosine"))
                    }
                    Some(Term::Time(_)) => self.unknown(&format!("{}(Time)", n.kind)),
                    Some(Term::Uv(_) | Term::Grad(_)) => self.unknown("coordinate-math"),
                    _ => self.unknown(&n.kind),
                }
            }
            "depthbiasedblend" => {
                self.note("DepthBiasedBlend (soft depth fade) ignored: RGB input used");
                self.lose(Lost::DEPTH);
                match self.input(pkg, n, "RGB", frame, depth) {
                    Some(t) => t,
                    None => self.unknown("DepthBiasedBlend-unconnected"),
                }
            }
            "if" => {
                let a = self.input(pkg, n, "A", frame, depth);
                let b = self.input(pkg, n, "B", frame, depth);
                match (a, b) {
                    (Some(Term::Const(x, _)), Some(Term::Const(y, _))) if x[0] != y[0] => {
                        let branch = if x[0] > y[0] {
                            "AGreaterThanB"
                        } else {
                            "ALessThanB"
                        };
                        match self.input(pkg, n, branch, frame, depth) {
                            Some(t) => t,
                            None => self.unknown("If-unconnected"),
                        }
                    }
                    // A comparison with something not representable is not
                    // representable for the same reason (a 0/1-like choice).
                    (Some(Term::Dim(k) | Term::Unknown(k)), _)
                    | (_, Some(Term::Dim(k) | Term::Unknown(k))) => Self::derived(&k, true),
                    _ => self.unknown("If"),
                }
            }
            "depthbiasedalpha" => {
                self.note("DepthBiasedAlpha (soft depth fade) ignored");
                self.lose(Lost::DEPTH);
                match self.input(pkg, n, "Alpha", frame, depth) {
                    Some(t) => t,
                    None => self.unknown("DepthBiasedAlpha-unconnected"),
                }
            }
            "materialfunctioncall" => self.function_call(pkg, n, output, frame, depth),
            "functioninput" => self.function_input(pkg, n, frame, depth),
            "functionoutput" => match self.input(pkg, n, "A", frame, depth) {
                Some(t) => t,
                None => self.unknown("FunctionOutput-unconnected"),
            },
            _ => self.unknown(&n.kind),
        }
    }

    fn static_switch(&mut self, n: &ExpressionNode) -> bool {
        let name = n.name("ParameterName").unwrap_or("").to_ascii_lowercase();
        match self.params.switches.get(&name) {
            Some(v) => *v,
            None => n.bool("DefaultValue").unwrap_or(false),
        }
    }

    fn coordinates(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        name: &str,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> UvTransform {
        let input = self.input(pkg, n, name, frame, depth);
        self.placed = matches!(input, None | Some(Term::Uv(_)));
        match input {
            None => UvTransform::default(),
            Some(Term::Uv(uv)) => uv,
            Some(Term::Const(..)) => {
                self.note("texture sampled at constant coordinates");
                UvTransform::default()
            }
            Some(Term::Unknown(k) | Term::Dim(k)) => {
                self.note(format!(
                    "texture coordinates not derivable ({k}): UV 0 used"
                ));
                self.lose(Lost::of_kind(&k));
                UvTransform::default()
            }
            Some(t) => {
                self.lose(match t {
                    Term::Tex(_) => Lost::TEXTURE,
                    Term::VertexColor => Lost::VERTEX_COLOR,
                    Term::Time(_) => Lost::TIME,
                    _ => Lost::COORDINATES,
                });
                self.note("texture coordinates not derivable: UV 0 used");
                UvTransform::default()
            }
        }
    }

    fn time_input(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> TimeInput {
        match self.input(pkg, n, "Time", frame, depth) {
            None => TimeInput::Linear(1.0),
            Some(Term::Time(s)) => TimeInput::Linear(s),
            Some(Term::Const(c, _)) => TimeInput::Constant(c[0]),
            Some(_) => TimeInput::Other,
        }
    }

    fn panner(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        let mut uv = self.coordinates(pkg, n, "Coordinate", frame, depth);
        let speed = [
            n.f32("SpeedX").unwrap_or(0.0),
            n.f32("SpeedY").unwrap_or(0.0),
        ];
        if uv.rotated() && speed != [0.0, 0.0] {
            self.note("Panner applied after a Rotator approximated as panning before it");
        }
        match self.time_input(pkg, n, frame, depth) {
            TimeInput::Linear(s) => {
                uv.panning[0] += speed[0] * s;
                uv.panning[1] += speed[1] * s;
            }
            // A constant time is a fixed shift of `speed * time`.
            TimeInput::Constant(c) => {
                uv.offset[0] += speed[0] * c;
                uv.offset[1] += speed[1] * c;
            }
            TimeInput::Other => {
                self.lose(Lost::TIME);
                self.note("Panner driven by a non-linear time input: plain speed used");
                uv.panning[0] += speed[0];
                uv.panning[1] += speed[1];
            }
        }
        Term::Uv(uv)
    }

    fn rotator(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        let mut uv = self.coordinates(pkg, n, "Coordinate", frame, depth);
        let speed = n.f32("Speed").unwrap_or(0.25);
        if uv.rotated() {
            self.note("nested Rotator expressions approximated by one rotation");
        }
        match self.time_input(pkg, n, frame, depth) {
            TimeInput::Linear(s) => uv.rotation += speed * s,
            // A constant time is a fixed angle of `speed * time` radians.
            TimeInput::Constant(c) => uv.rotation_angle += speed * c,
            TimeInput::Other => {
                self.lose(Lost::TIME);
                self.note("Rotator driven by a non-linear time input: plain speed used");
                uv.rotation += speed;
            }
        }
        uv.rotation_center = [
            n.f32("CenterX").unwrap_or(0.5),
            n.f32("CenterY").unwrap_or(0.5),
        ];
        Term::Uv(uv)
    }

    fn texture_sample(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        kind: &str,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        let sampler = sampler_of(kind);
        let mut texture = n.object("Texture").map(str::to_owned);
        let mut parameter = None;
        if kind.contains("parameter") {
            let name = n.name("ParameterName").unwrap_or("").to_owned();
            // A null override falls through to the expression's own texture
            // (see `ParameterValues::from_chain`).
            if let Some(Some(t)) = self.params.textures.get(&name.to_ascii_lowercase()) {
                texture = Some(t.clone());
            }
            parameter = Some(name);
        }
        if matches!(sampler, "subuv" | "flipbook" | "movie") {
            self.note(format!(
                "{} animation not reproduced: whole texture used",
                n.kind
            ));
        }
        if n.link("TextureObject").is_some() {
            self.note("TextureObject input ignored");
        }
        let info = texture.as_deref().map(|t| self.texture_info(pkg, t));
        let stats = info.and_then(|i| i.stats);
        if let (Some(t), Some(i)) = (&texture, info) {
            if i.runtime || sampler == "movie" {
                self.runtime_textures.insert(t.clone());
            }
            if let Some(s) = i.stats {
                self.stats.insert(t.clone(), s);
            }
        }
        // A texture that is one colour everywhere is that colour (the
        // placeholder a texture parameter defaults to, usually).
        if let Some(s) = stats.filter(|s| s.uniform && sampler == "2d") {
            return Term::Const(s.mean, 4);
        }
        let uv = self.coordinates(pkg, n, "Coordinates", frame, depth);
        let cube = sampler == "cube" || info.is_some_and(|i| i.cube);
        if cube {
            self.placed = false;
        }
        // A sample whose coordinates are not the mesh's (a lookup by the
        // view or reflection vector, say, or any lookup in a cube map) has
        // no place on the surface: the texture's mean is all that can be
        // said of it.
        if let Some(s) = stats.filter(|_| !self.placed && (sampler == "2d" || cube)) {
            self.note(
                "a texture sampled at coordinates that are not the mesh's stands in by its mean",
            );
            return Term::Const(s.mean, 4);
        }
        Term::Tex(Box::new(TexTerm {
            texture,
            parameter,
            sampler,
            uv,
            swizzle: vec![0, 1, 2, 3],
            width: 4,
            factor: splat(1.0),
            bias: splat(0.0),
            vertex_color: false,
            stats,
            placed: self.placed,
        }))
    }

    fn texture_info(&mut self, pkg: &Arc<LoadedPackage>, path: &str) -> TextureInfo {
        let mut near = Vec::with_capacity(self.homes.len() + 1);
        near.push(pkg.clone());
        near.extend(self.homes.iter().cloned());
        self.dec.texture_info(&near, path)
    }

    fn binary(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        frame: Option<&Frame<'_>>,
        depth: usize,
        op: Op,
    ) -> Term {
        let a = self.input(pkg, n, "A", frame, depth);
        let b = self.input(pkg, n, "B", frame, depth);
        if a.is_none() || b.is_none() {
            return self.unknown(&format!("{}-unconnected", n.kind));
        }
        self.combine(op, a, b)
    }

    /// Combine two symbolic values.
    fn combine(&mut self, op: Op, a: Option<Term>, b: Option<Term>) -> Term {
        let (Some(a), Some(b)) = (a, b) else {
            return self.unknown("unconnected-input");
        };
        // A length or position keeps the constants (and the other not
        // representable values) combined with it: `depth * k - offset` is
        // still a depth, and `k` says nothing without it.
        match (&a, &b) {
            (Term::Dim(_), Term::Const(..) | Term::Dim(_) | Term::Unknown(_) | Term::Time(_)) => {
                return a;
            }
            (Term::Const(..) | Term::Unknown(_) | Term::Time(_), Term::Dim(_)) => return b,
            _ => {}
        }
        match (demote(a), demote(b)) {
            (Term::Const(x, nx), Term::Const(y, ny)) => {
                let (x4, y4) = (bcast(x, nx), bcast(y, ny));
                let n = nx.max(ny);
                Term::Const(op.apply(x4, y4), n)
            }
            (Term::Tex(mut t), Term::Const(c, nc)) => {
                let c = bcast(c, nc);
                if t.width == 1 {
                    t.width = nc.clamp(1, 4);
                }
                match op {
                    Op::Mul => {
                        t.factor = zip(t.factor, c, |a, b| a * b);
                        t.bias = zip(t.bias, c, |a, b| a * b);
                    }
                    Op::Div => {
                        let r = c.map(|v| if v == 0.0 { 0.0 } else { 1.0 / v });
                        t.factor = zip(t.factor, r, |a, b| a * b);
                        t.bias = zip(t.bias, r, |a, b| a * b);
                    }
                    Op::Add => t.bias = zip(t.bias, c, |a, b| a + b),
                    Op::Sub => t.bias = zip(t.bias, c, |a, b| a - b),
                }
                Term::Tex(t)
            }
            (Term::Const(c, nc), Term::Tex(mut t)) => {
                let c = bcast(c, nc);
                if t.width == 1 && op != Op::Div {
                    t.width = nc.clamp(1, 4);
                }
                match op {
                    Op::Mul => {
                        t.factor = zip(t.factor, c, |a, b| a * b);
                        t.bias = zip(t.bias, c, |a, b| a * b);
                    }
                    Op::Add => t.bias = zip(t.bias, c, |a, b| a + b),
                    Op::Sub => {
                        t.factor = t.factor.map(|v| -v);
                        t.bias = zip(c, t.bias, |a, b| a - b);
                    }
                    Op::Div => {
                        self.note("constant divided by a texture approximated by the constant");
                        return Term::Const(c, nc);
                    }
                }
                Term::Tex(t)
            }
            (Term::Tex(x), Term::Tex(y)) => self.combine_textures(op, x, y),
            (Term::Grad(mut g), Term::Const(c, nc)) => {
                let c4 = bcast(c, nc);
                match op {
                    Op::Mul => {
                        g.value = zip(bcast(g.value, g.n), c4, |a, b| a * b);
                        g.n = g.n.max(nc);
                        Term::Grad(g)
                    }
                    Op::Div => {
                        let r = c4.map(|v| if v == 0.0 { 0.0 } else { 1.0 / v });
                        g.value = zip(bcast(g.value, g.n), r, |a, b| a * b);
                        g.n = g.n.max(nc);
                        Term::Grad(g)
                    }
                    Op::Add => self.gradient_offset(g, 1.0, c[0], nc),
                    Op::Sub => self.gradient_offset(g, 1.0, -c[0], nc),
                }
            }
            (Term::Const(c, nc), Term::Grad(mut g)) => match op {
                Op::Mul => {
                    g.value = zip(bcast(g.value, g.n), bcast(c, nc), |a, b| a * b);
                    g.n = g.n.max(nc);
                    Term::Grad(g)
                }
                Op::Add => self.gradient_offset(g, 1.0, c[0], nc),
                // c - g
                Op::Sub => self.gradient_offset(g, -1.0, c[0], nc),
                Op::Div => self.unknown("coordinate-math"),
            },
            (Term::Grad(mut x), Term::Grad(y)) => {
                if op == Op::Mul && x.factors.len() + y.factors.len() <= MAX_GRADIENT_FACTORS {
                    x.factors.extend(y.factors.iter().copied());
                    x.value = zip(bcast(x.value, x.n), bcast(y.value, y.n), |a, b| a * b);
                    x.n = x.n.max(y.n);
                    Term::Grad(x)
                } else {
                    self.unknown("coordinate-math")
                }
            }
            (Term::Tex(t), Term::Grad(_)) | (Term::Grad(_), Term::Tex(t)) => {
                self.note(format!(
                    "{} of a texture and a function of the coordinates: kept the texture",
                    op.name()
                ));
                self.lose(Lost::COORDINATES);
                Term::Tex(t)
            }
            (Term::Uv(uv), Term::Const(c, nc)) => {
                if uv.rotated() {
                    self.note("coordinate math after a Rotator applied before the rotation");
                }
                Term::Uv(uv_affine(uv, op, bcast(c, nc)))
            }
            (Term::Const(c, nc), Term::Uv(uv)) => {
                if uv.rotated() {
                    self.note("coordinate math after a Rotator applied before the rotation");
                }
                let c = bcast(c, nc);
                match op {
                    Op::Mul | Op::Add => Term::Uv(uv_affine(uv, op, c)),
                    Op::Sub => {
                        // c - uv = uv * -1 + c
                        let flipped = uv_affine(uv, Op::Mul, splat(-1.0));
                        Term::Uv(uv_affine(flipped, Op::Add, c))
                    }
                    Op::Div => {
                        self.note("constant divided by texture coordinates ignored");
                        Term::Uv(uv)
                    }
                }
            }
            (Term::Time(s), Term::Const(c, _)) | (Term::Const(c, _), Term::Time(s)) => match op {
                Op::Mul => Term::Time(s * c[0]),
                Op::Div if c[0] != 0.0 => Term::Time(s / c[0]),
                _ => {
                    self.note("time offset ignored");
                    Term::Time(s)
                }
            },
            // Vertex colour is not exported: it counts as white, the colour
            // of a mesh that was never painted (`color * x` is `x`, `color +
            // x` is `1 + x`), and the channel is flagged.
            (a @ Term::VertexColor, b) | (a, b @ Term::VertexColor) => {
                self.note(format!("vertex colour in a {} taken as white", op.name()));
                self.lose(Lost::VERTEX_COLOR);
                let white = |t: Term| match t {
                    Term::VertexColor => Term::Const(splat(1.0), 1),
                    t => t,
                };
                match self.combine(op, Some(white(a)), Some(white(b))) {
                    Term::Tex(mut t) => {
                        t.vertex_color = true;
                        Term::Tex(t)
                    }
                    t => t,
                }
            }
            // A component of a direction (centred on 0) scaled by a
            // constant is still one; it only drops out of a sum.
            (Term::Unknown(k), Term::Const(..))
                if Lost::is_direction(&k) && matches!(op, Op::Mul | Op::Div) =>
            {
                Term::Unknown(k)
            }
            (Term::Const(..), Term::Unknown(k)) if Lost::is_direction(&k) && op == Op::Mul => {
                Term::Unknown(k)
            }
            (Term::Unknown(k), other) | (other, Term::Unknown(k)) => {
                self.note(format!(
                    "unsupported {k} treated as neutral in {}",
                    op.name()
                ));
                self.lose(Lost::of_kind(&k));
                other
            }
            (x, y) => {
                self.note(format!(
                    "{} of incompatible values approximated by its first input",
                    op.name()
                ));
                self.lose(match y {
                    Term::Uv(_) | Term::Grad(_) => Lost::COORDINATES,
                    Term::Time(_) => Lost::TIME,
                    _ => Lost::OTHER,
                });
                x
            }
        }
    }

    /// `sign * g + c` for a gradient of one plain factor (`value * (s x +
    /// o)` stays one factor); anything else is not a product of powers.
    fn gradient_offset(&mut self, mut g: Box<GradTerm>, sign: f32, c: f32, nc: u8) -> Term {
        let v = g.value[0];
        match g.factors.as_mut_slice() {
            [f] if g.n == 1 && nc == 1 && f.power == 1.0 => {
                f.scale *= v * sign;
                f.offset = f.offset * v * sign + c;
                g.value = splat(1.0);
                Term::Grad(g)
            }
            _ => self.unknown("coordinate-math"),
        }
    }

    /// Two texture samples combined: one is kept, the other is recorded as
    /// dropped and stands in by its mean value when that is known.
    ///
    /// Kept: a sample whose coordinates follow the mesh's over one whose
    /// coordinates could not be derived; then the sample with more channels
    /// (colour over a mask channel); between two colour samples of a
    /// product the one whose texture carries colour over one that is grey
    /// (a tiling surface texture over the occlusion or weight map that
    /// modulates it); otherwise (and always in a sum, whose terms are
    /// peers) the one with the coarser tiling (base layer over detail
    /// layer), then the first.
    fn combine_textures(&mut self, op: Op, x: Box<TexTerm>, y: Box<TexTerm>) -> Term {
        let coarse = |t: &TexTerm| t.uv.scale[0].abs().max(t.uv.scale[1].abs());
        let keep_first = if x.placed != y.placed {
            x.placed
        } else {
            match x.swizzle.len().cmp(&y.swizzle.len()) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                // In a product a grey texture modulates a coloured one.
                // The terms of a sum are peers: the tiling decides.
                std::cmp::Ordering::Equal => match (x.coloured(), y.coloured()) {
                    (Some(true), Some(false)) if matches!(op, Op::Mul | Op::Div) => true,
                    (Some(false), Some(true)) if matches!(op, Op::Mul | Op::Div) => false,
                    _ => coarse(&x) <= coarse(&y),
                },
            }
        };
        let (keep, drop) = if keep_first { (x, y) } else { (y, x) };
        if let Some(d) = &drop.texture {
            self.extra_textures.insert(d.clone());
        }
        self.note(format!(
            "{} of two textures: kept the {}-channel sample",
            op.name(),
            keep.swizzle.len()
        ));
        self.lose(Lost::TEXTURE);
        let Some((mean, n)) = drop.mean().filter(|_| self.fold_means) else {
            return Term::Tex(keep);
        };
        self.note("a dropped texture stands in by its mean value");
        let (keep, mean) = (Some(Term::Tex(keep)), Some(Term::Const(mean, n)));
        if keep_first {
            self.combine(op, keep, mean)
        } else {
            self.combine(op, mean, keep)
        }
    }

    fn lerp(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &ExpressionNode,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        let a = self.input(pkg, n, "A", frame, depth);
        let b = self.input(pkg, n, "B", frame, depth);
        let alpha = self.input(pkg, n, "Alpha", frame, depth);
        // A constant that stands in for something that varies in the
        // original (a depth or view term treated as neutral on the way) is
        // not a blend weight.
        let alpha_varies = !self.last_lost.is_empty();
        let (Some(a), Some(b)) = (a, b) else {
            return self.unknown("LinearInterpolate-unconnected");
        };
        match alpha {
            Some(Term::Const(c, _)) if !alpha_varies => {
                let w = c[0];
                if w <= 0.0 {
                    return a;
                }
                if w >= 1.0 {
                    return b;
                }
                let wa = Term::Const(splat(1.0 - w), 1);
                let wb = Term::Const(splat(w), 1);
                let ta = self.combine(Op::Mul, Some(a), Some(wa));
                let tb = self.combine(Op::Mul, Some(b), Some(wb));
                self.combine(Op::Add, Some(ta), Some(tb))
            }
            alpha => {
                // Where the alpha is known to be 1 and above (or 0 and
                // below) everywhere, the lerp is its input B (or A) and
                // nothing is lost by saying so. That can only be said of
                // a constant or of a texture term's range (a texel is in
                // 0..1), and only when nothing but texture detail and
                // vertex colour (taken as white) was left out of it: an
                // alpha that assumed a neutral depth, view or world term
                // says nothing about the blend.
                let known = Lost::TEXTURE
                    .with(Lost::VERTEX_COLOR)
                    .contains(self.last_lost);
                let range = match &alpha {
                    Some(Term::Tex(t)) if known => {
                        let (f, b) = (t.factor[0], t.bias[0]);
                        Some((b.min(f + b), b.max(f + b)))
                    }
                    Some(Term::Const(c, _)) if known => Some((c[0], c[0])),
                    Some(Term::VertexColor) => Some((1.0, 1.0)),
                    _ => None,
                };
                let side = match range {
                    Some((lo, _)) if lo >= 1.0 => Some(true),
                    Some((_, hi)) if hi <= 0.0 => Some(false),
                    _ => None,
                };
                match &alpha {
                    Some(Term::Unknown(k) | Term::Dim(k)) => self.lose(Lost::of_kind(k)),
                    Some(Term::Tex(_)) => self.lose(Lost::TEXTURE),
                    Some(Term::VertexColor) => self.lose(Lost::VERTEX_COLOR),
                    Some(Term::Uv(_) | Term::Grad(_)) => self.lose(Lost::COORDINATES),
                    Some(Term::Time(_)) => self.lose(Lost::TIME),
                    Some(Term::Const(..)) | None => {}
                }
                // A clamped depth is a factor that is 1 beyond its range:
                // the blend it drives between two tilings of one texture
                // (a finer one near the camera) shows its far side over
                // most of what is on screen.
                let by_distance = matches!(
                    &alpha,
                    Some(Term::Unknown(k)) if Lost::of_kind(k) == Lost::DEPTH
                );
                match side {
                    Some(true) => b,
                    Some(false) => a,
                    None => self.lerp_varying(a, b, by_distance),
                }
            }
        }
    }

    /// A lerp whose alpha varies: one side stands for both.
    fn lerp_varying(&mut self, a: Term, b: Term, by_distance: bool) -> Term {
        match (demote(a), demote(b)) {
            (Term::Const(x, nx), Term::Const(y, ny)) => {
                self.note("lerp with a varying alpha between constants: average used");
                Term::Const(
                    zip(bcast(x, nx), bcast(y, ny), |p, q| 0.5 * (p + q)),
                    nx.max(ny),
                )
            }
            (Term::Tex(t), Term::Tex(other)) if by_distance && t.texture == other.texture => {
                self.note(
                    "distance blend between two tilings of one texture: kept the far one (input B)",
                );
                Term::Tex(other)
            }
            (Term::Tex(t), Term::Tex(other)) => {
                if let Some(d) = &other.texture {
                    self.extra_textures.insert(d.clone());
                }
                self.note("lerp with a varying alpha between two textures: kept input A");
                self.lose(Lost::TEXTURE);
                Term::Tex(t)
            }
            (Term::Unknown(_), x) | (x, Term::Unknown(_)) => {
                self.note("lerp with a varying alpha: kept the supported input");
                x
            }
            (Term::Tex(t), _) | (_, Term::Tex(t)) => {
                self.note("lerp with a varying alpha: kept the textured input");
                Term::Tex(t)
            }
            (x, _) => {
                self.note("lerp with a varying alpha: kept input A");
                x
            }
        }
    }

    /// Evaluate output `output` of a `MaterialFunctionCall`. The call's
    /// outputs and inputs name the function's `FunctionOutput` /
    /// `FunctionInput` expressions by their `Id` GUID (the object references
    /// in those structs are not serialized in the cooked data).
    fn function_call(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &Rc<ExpressionNode>,
        output: i32,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        let outputs = param_elements(&n.properties, "FunctionOutputs");
        let Some(out) = usize::try_from(output).ok().and_then(|i| outputs.get(i)) else {
            return self.unknown("MaterialFunctionCall-output");
        };
        let out_id = prop(out, "ExpressionOutputId").and_then(guid_value);
        let out_path = get_object(out, "ExpressionOutput").map(str::to_owned);
        let Some(fp) = n.object("MaterialFunction") else {
            return self.unknown("MaterialFunction-missing");
        };
        let located = match pkg.export_by_qualified(fp) {
            Some(i) => Some((pkg.clone(), i)),
            None => self.dec.set().locate(fp),
        };
        let Some((fpkg, findex)) = located else {
            return self.unknown("MaterialFunction-missing");
        };
        let Ok(fobj) = self.dec.set().decode(&fpkg, findex) else {
            return self.unknown("MaterialFunction-undecodable");
        };
        let members: Vec<String> = match prop(&fobj.properties, "FunctionExpressions") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(value_object)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        // Scanning the members is work too: count it against the budget so a
        // hostile function with a huge member list called many times stays
        // bounded.
        self.visits = self.visits.saturating_add(members.len());
        if self.visits >= MAX_EXPRESSION_VISITS {
            return self.unknown("depth-or-work-limit");
        }
        let mut out_node = None;
        for m in &members {
            let Some(node) = self.dec.expression(&fpkg, m) else {
                continue;
            };
            if !node.kind.eq_ignore_ascii_case("FunctionOutput") {
                continue;
            }
            let by_id =
                out_id.is_some() && prop(&node.properties, "Id").and_then(guid_value) == out_id;
            let by_path = out_path
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(&node.path));
            if by_id || by_path {
                out_node = Some(node);
                break;
            }
        }
        let Some(out_node) = out_node else {
            return self.unknown("FunctionOutput-missing");
        };
        let inner = Frame {
            call: n.clone(),
            caller_pkg: pkg.clone(),
            parent: frame,
        };
        let fname = fp.rsplit('.').next().unwrap_or(fp).to_owned();
        match self.input(&fpkg, &out_node, "A", Some(&inner), depth) {
            Some(Term::Unknown(_) | Term::Dim(_)) | None => {
                // Image-adjustment and blend functions reduce to unsupported
                // math; approximate the call by its textured (or first
                // representable) input.
                let mut best: Option<Term> = None;
                for e in param_elements(&n.properties, "FunctionInputs") {
                    let Some(l) = prop(&e, "Input").map(expr_link) else {
                        continue;
                    };
                    let Some(t) = self.link(pkg, &l, frame, depth) else {
                        continue;
                    };
                    let better = match (&best, &t) {
                        (_, Term::Unknown(_) | Term::Dim(_)) => false,
                        (None, _) => true,
                        (Some(Term::Tex(_)), _) => false,
                        (Some(_), Term::Tex(_)) => true,
                        _ => false,
                    };
                    if better {
                        best = Some(t);
                    }
                }
                match best {
                    Some(t) => {
                        self.note(format!(
                            "material function {fname} approximated by its input"
                        ));
                        t
                    }
                    None => self.unknown(&format!("MaterialFunction:{fname}")),
                }
            }
            Some(t) => t,
        }
    }

    fn function_input(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        n: &Rc<ExpressionNode>,
        frame: Option<&Frame<'_>>,
        depth: usize,
    ) -> Term {
        if let Some(fr) = frame {
            let id = prop(&n.properties, "Id").and_then(guid_value);
            for e in param_elements(&fr.call.properties, "FunctionInputs") {
                let by_id =
                    id.is_some() && prop(&e, "ExpressionInputId").and_then(guid_value) == id;
                let by_path = get_object(&e, "ExpressionInput")
                    .is_some_and(|p| p.eq_ignore_ascii_case(&n.path));
                if !(by_id || by_path) {
                    continue;
                }
                let link = prop(&e, "Input").map(expr_link);
                if let Some(l) = link.filter(|l| l.expression.is_some()) {
                    let caller = fr.caller_pkg.clone();
                    if let Some(t) = self.link(&caller, &l, fr.parent, depth) {
                        return t;
                    }
                }
            }
        }
        // Unbound: the input's preview value or preview expression.
        if n.bool("bUsePreviewValueAsDefault").unwrap_or(false)
            && let Some(v) = prop(&n.properties, "PreviewValue").and_then(value_linear_color)
        {
            return Term::Const(v, 4);
        }
        match self.input(pkg, n, "Preview", frame, depth) {
            Some(t) => t,
            None => self.unknown("FunctionInput-unbound"),
        }
    }
}

/// The properties of an expression node that the walk reads, for the
/// trace: names and numbers only (a local diagnostic, never written to the
/// converted data).
fn describe_node(n: &ExpressionNode, link: &ExprLink) -> String {
    let mut out = String::new();
    for key in ["ParameterName", "Texture", "MaterialFunction"] {
        let v = n.name(key).or_else(|| n.object(key));
        if let Some(v) = v {
            out.push_str(&format!(" {key}={}", v.rsplit('.').next().unwrap_or(v)));
        }
    }
    for key in [
        "R",
        "G",
        "B",
        "A",
        "DefaultValue",
        "UTiling",
        "VTiling",
        "SpeedX",
        "SpeedY",
        "Speed",
        "Min",
        "Max",
        "Bias",
        "Scale",
        "ConstExponent",
        "Exponent",
    ] {
        if let Some(v) = n.f32(key) {
            out.push_str(&format!(" {key}={v}"));
        } else if let Some(v) = n.bool(key) {
            out.push_str(&format!(" {key}={v}"));
        }
    }
    if let Some(v) = prop(&n.properties, "DefaultValue").and_then(value_linear_color) {
        out.push_str(&format!(" DefaultValue={v:?}"));
    }
    if let Some(i) = get_i32(&n.properties, "CoordinateIndex") {
        out.push_str(&format!(" CoordinateIndex={i}"));
    }
    if link.output_index != 0 {
        out.push_str(&format!(" out={}", link.output_index));
    }
    if let Some(m) = link.mask {
        let sel: String = ['r', 'g', 'b', 'a']
            .iter()
            .zip(m)
            .filter(|(_, on)| *on)
            .map(|(c, _)| *c)
            .collect();
        out.push_str(&format!(" mask={sel}"));
    }
    out
}

/// A reduced value, for the trace.
fn describe_term(t: &Term) -> String {
    match t {
        Term::Const(v, n) => format!("const{n} {:?}", &v[..usize::from((*n).clamp(1, 4))]),
        Term::Tex(x) => format!(
            "tex {}.{} w{} *{:?} +{:?} uv{} x{:?} +{:?} pan{:?}",
            x.texture
                .as_deref()
                .map_or("(none)", |p| p.rsplit('.').next().unwrap_or(p)),
            channels_text(&x.swizzle),
            x.width,
            &x.factor[..usize::from(x.width.clamp(1, 4))],
            &x.bias[..usize::from(x.width.clamp(1, 4))],
            x.uv.channel,
            x.uv.scale,
            x.uv.offset,
            x.uv.panning
        ),
        Term::Uv(uv) => format!(
            "uv{} x{:?} +{:?} pan{:?} rot{}",
            uv.channel, uv.scale, uv.offset, uv.panning, uv.rotation
        ),
        Term::VertexColor => "vertex colour".to_owned(),
        Term::Time(s) => format!("time x{s}"),
        Term::Grad(g) => format!(
            "gradient {:?} * {:?}",
            &g.value[..usize::from(g.n.clamp(1, 4))],
            g.factors
                .iter()
                .map(|f| format!(
                    "({} * {}{} + {})^{}",
                    f.scale, f.component, f.uv_channel, f.offset, f.power
                ))
                .collect::<Vec<_>>()
        ),
        Term::Unknown(k) => format!("unsupported {k}"),
        Term::Dim(k) => format!("unsupported {k} (a length)"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Mul,
    Add,
    Sub,
    Div,
}

impl Op {
    fn apply(self, a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
        match self {
            Op::Mul => zip(a, b, |x, y| x * y),
            Op::Add => zip(a, b, |x, y| x + y),
            Op::Sub => zip(a, b, |x, y| x - y),
            Op::Div => zip(a, b, |x, y| if y == 0.0 { 0.0 } else { x / y }),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Op::Mul => "multiply",
            Op::Add => "add",
            Op::Sub => "subtract",
            Op::Div => "divide",
        }
    }
}

/// Apply `uv op c` to a coordinate transform (the transform is affine in
/// the coordinates, so scaling scales offset and panning too).
fn uv_affine(mut uv: UvTransform, op: Op, c: [f32; 4]) -> UvTransform {
    for (i, k) in c.iter().take(2).enumerate() {
        match op {
            Op::Mul | Op::Div => {
                let f = match op {
                    Op::Div if *k == 0.0 => 0.0,
                    Op::Div => 1.0 / *k,
                    _ => *k,
                };
                uv.scale[i] *= f;
                uv.offset[i] *= f;
                uv.panning[i] *= f;
            }
            Op::Add => uv.offset[i] += *k,
            Op::Sub => uv.offset[i] -= *k,
        }
    }
    uv
}

fn is_texture_sample(kind: &str) -> bool {
    kind.starts_with("texturesample")
        || matches!(
            kind,
            "particlesubuv"
                | "flipbooksample"
                | "meshsubuv"
                | "meshsubuvblend"
                | "antialiasedtexturemask"
                | "fontsample"
                | "fontsampleparameter"
                | "textureobject"
                | "textureobjectparameter"
        )
}

fn sampler_of(kind: &str) -> &'static str {
    if kind.contains("cube") {
        "cube"
    } else if kind.contains("subuv") {
        "subuv"
    } else if kind.contains("flipbook") {
        "flipbook"
    } else if kind.contains("movie") {
        "movie"
    } else if kind.contains("font") {
        "font"
    } else if kind.contains("normal") {
        "normal"
    } else {
        "2d"
    }
}

// ---------------------------------------------------------------------------
// Approximate PBR description
// ---------------------------------------------------------------------------

/// Where a channel's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSource {
    /// The input's `UseConstant` constant (wins over a connected expression,
    /// as in `F*MaterialInput::Compile`).
    Constant,
    /// The connected expression graph.
    Expression,
    /// Nothing connected: the engine's compile-time default.
    Default,
}

/// A texture bound to a channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextureBinding {
    /// Texture object path (as keyed in the texture manifest); `None` for a
    /// texture parameter without a value.
    pub texture: Option<String>,
    /// Parameter name for `TextureSampleParameter*` expressions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
    /// `2d`, `normal`, `cube`, `subuv`, `flipbook`, `movie` or `font`.
    pub sampler: &'static str,
    /// Texture channels read, in output order (`rgb`, `a`, `r`, `rgba`...).
    pub channels: String,
    /// Coordinate transform.
    pub uv: UvTransform,
}

/// One approximated material input. The channel's value is
/// `texture.channels * value + bias` when a texture is bound, else `value`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Channel {
    /// Source of the value.
    pub source: ChannelSource,
    /// Constant value (linear RGBA), or the multiplier of the texture.
    pub value: [f32; 4],
    /// Added after the multiplier (non-zero for remaps such as `1 - x`).
    pub bias: [f32; 4],
    /// Bound texture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture: Option<TextureBinding>,
    /// The graph multiplies by vertex colour (not reproduced in `value`).
    pub vertex_color: bool,
    /// False when the graph reduced to something this approximation cannot
    /// represent; `value` then holds the engine default for the input.
    pub resolved: bool,
}

/// Overall result for one material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApproxStatus {
    /// The instance chain reached a base material and the main colour
    /// channel was derived from its graph (possibly with notes).
    Approximated,
    /// Chain broken or main colour channel unresolved: neutral defaults.
    Fallback,
}

/// Approximate PBR description of a material or material instance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApproxMaterial {
    /// Material class of the object itself.
    pub class: MaterialClass,
    /// Object paths from the object to its base material.
    pub chain: Vec<String>,
    /// Base `Material` path (last link) when the chain is complete.
    pub base_material: Option<String>,
    /// Result.
    pub status: ApproxStatus,
    /// `Id` of the resource whose shader map renders this object (the
    /// object's own static-permutation resource, else the base material's),
    /// as 32 hex digits; it keys the shader map in the `RefShaderCache-*`
    /// packages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// No approximation was needed: every connected expression was
    /// represented exactly (no notes).
    pub lossless: bool,
    /// UE3 `EBlendMode` name of the base material.
    pub blend_mode: String,
    /// `opaque`, `mask`, `blend`, `add`, `modulate` or `premultiplied`.
    pub alpha_mode: &'static str,
    /// Clip value for `mask` (`OpacityMaskClipValue`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_cutoff: Option<f32>,
    /// `TwoSided`.
    pub two_sided: bool,
    /// `LightingModel == MLM_Unlit`: show `emissive` without lighting.
    pub unlit: bool,
    /// UE3 `EMaterialLightingModel` name.
    pub lighting_model: String,
    /// The base material is a `DecalMaterial` (projected, not drawn on meshes).
    pub decal: bool,
    /// `DiffuseColor` (default black).
    pub base_color: Channel,
    /// `Normal` (tangent space; `None` when unconnected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<Channel>,
    /// `EmissiveColor` (linear, may exceed 1; default black).
    pub emissive: Channel,
    /// Largest of the emissive value's R, G, B (0 when black): the HDR
    /// strength of the emissive colour or of its texture multiplier.
    pub emissive_intensity: f32,
    /// `SpecularColor` (default black).
    pub specular: Channel,
    /// `SpecularPower` (Phong exponent, default 15).
    pub specular_power: Channel,
    /// Scalar specular level for metallic-roughness renderers (mean of the
    /// specular colour value, clamped to 0..1).
    pub specular_level: f32,
    /// Perceptual roughness derived from the specular power `p`:
    /// `sqrt(sqrt(2 / (p + 2)))` (Blinn-Phong to Beckmann slope, then the
    /// square root that GGX "perceptual roughness" uses).
    pub roughness: f32,
    /// Always 0: UE3's Phong model has no metalness.
    pub metallic: f32,
    /// `OpacityMask` for `mask`, `Opacity` for the translucent modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<Channel>,
    /// `PhysMaterial` (nearest set in the chain).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phys_material: Option<String>,
    /// Parameter values in effect (instance overrides).
    pub parameters: ParameterValues,
    /// Every texture bound to a channel.
    pub textures: Vec<String>,
    /// Textures the graph combines in but the approximation dropped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped_textures: Vec<String>,
    /// Expression nodes visited, by kind.
    pub expressions: BTreeMap<String, usize>,
    /// Expression kinds that could not be represented, with counts.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unsupported: BTreeMap<String, usize>,
    /// What was simplified.
    pub notes: Vec<String>,
    /// Per channel (`base_color`, `normal`, `emissive`, `specular`,
    /// `specular_power`, `opacity`): the inputs of the original graph the
    /// channel does not have ([`Lost::NAMES`]). A channel that lost nothing
    /// is absent.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped: BTreeMap<&'static str, Vec<&'static str>>,
    /// Per channel: the function of the texture coordinates that multiplies
    /// the channel's `value` (a channel computed from coordinates alone).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub gradients: BTreeMap<&'static str, UvGradient>,
    /// What each bound texture holds on average (those whose small mips are
    /// stored in the package).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub texture_stats: BTreeMap<String, TextureStats>,
    /// Bound objects that are filled while the game runs (render targets,
    /// movies, scripted textures): there is no picture to convert.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runtime_textures: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputShape {
    Color,
    Scalar,
    Vector,
}

fn alpha_mode_of(blend: &str) -> (&'static str, Option<&'static str>) {
    match blend.to_ascii_lowercase().as_str() {
        "blend_opaque" => ("opaque", None),
        "blend_masked" => ("mask", None),
        "blend_softmasked" => ("mask", Some("BLEND_SoftMasked approximated as a hard mask")),
        "blend_translucent" => ("blend", None),
        "blend_additive" => ("add", None),
        "blend_modulate" => ("modulate", None),
        "blend_modulateandadd" => (
            "modulate",
            Some("BLEND_ModulateAndAdd approximated as modulate"),
        ),
        "blend_alphacomposite" => ("premultiplied", None),
        "blend_ditheredtranslucent" => (
            "blend",
            Some("BLEND_DitheredTranslucent approximated as blend"),
        ),
        _ => ("opaque", Some("unknown blend mode treated as opaque")),
    }
}

/// Perceptual roughness for a Phong/Blinn exponent (see [`ApproxMaterial::roughness`]).
pub fn roughness_from_specular_power(p: f32) -> f32 {
    let p = if p.is_finite() { p.max(0.0) } else { 0.0 };
    (2.0 / (p + 2.0)).sqrt().sqrt().clamp(0.0, 1.0)
}

fn channels_text(sw: &[u8]) -> String {
    sw.iter()
        .map(|c| match c {
            0 => 'r',
            1 => 'g',
            2 => 'b',
            _ => 'a',
        })
        .collect()
}

impl Eval<'_, '_> {
    /// Evaluate a material input struct property of the base material.
    fn material_input(
        &mut self,
        pkg: &Arc<LoadedPackage>,
        props: &[Property],
        name: &str,
        key: &'static str,
        shape: InputShape,
        default: [f32; 4],
    ) -> Channel {
        let fields = prop(props, name).and_then(struct_fields).unwrap_or(&[]);
        let use_constant = get_bool(fields, "UseConstant").unwrap_or(false);
        if use_constant {
            let value = match shape {
                InputShape::Color => prop(fields, "Constant")
                    .and_then(value_color_bytes)
                    .map_or(default, color_to_linear),
                InputShape::Scalar => splat(get_f32(fields, "Constant").unwrap_or(default[0])),
                InputShape::Vector => prop(fields, "Constant")
                    .and_then(value_linear_color)
                    .map_or(default, |v| [v[0], v[1], v[2], 0.0]),
            };
            return Channel {
                source: ChannelSource::Constant,
                value,
                bias: splat(0.0),
                texture: None,
                vertex_color: false,
                resolved: true,
            };
        }
        let link = prop(props, name)
            .map(expr_link)
            .filter(|l| l.expression.is_some());
        let Some(link) = link else {
            return Channel {
                source: ChannelSource::Default,
                value: default,
                bias: splat(0.0),
                texture: None,
                vertex_color: false,
                resolved: true,
            };
        };
        if let Some(t) = self.trace.as_mut() {
            t.push(format!("== {name}"));
        }
        self.lost = Lost::NONE;
        self.gradient = None;
        self.fold_means = matches!(key, "base_color" | "specular");
        let term = self.link(pkg, &link, None, 0);
        let mut ch = self.channel(term, default, name);
        if shape == InputShape::Scalar {
            // A scalar input reads the first component of a wider value
            // (the shader compiler's cast truncates to `.r`).
            ch.value = splat(ch.value[0]);
            ch.bias = splat(ch.bias[0]);
        }
        let lost = std::mem::take(&mut self.lost);
        if !lost.is_empty() {
            self.dropped.insert(key, lost.names());
        }
        if let Some(g) = self.gradient.take() {
            self.gradients.insert(key, g);
        }
        ch
    }

    fn channel(&mut self, term: Option<Term>, default: [f32; 4], name: &str) -> Channel {
        let mut ch = Channel {
            source: ChannelSource::Expression,
            value: default,
            bias: splat(0.0),
            texture: None,
            vertex_color: false,
            resolved: true,
        };
        match term {
            Some(Term::Const(v, n)) => ch.value = pad(bcast(v, n), usize::from(n), 1.0),
            Some(Term::Tex(t)) => {
                let k = usize::from(t.width.clamp(1, 4));
                let scalar = k == 1;
                ch.value = if scalar {
                    splat(t.factor[0])
                } else {
                    pad(t.factor, k, 1.0)
                };
                ch.bias = if scalar {
                    splat(t.bias[0])
                } else {
                    pad(t.bias, k, 0.0)
                };
                ch.vertex_color = t.vertex_color;
                ch.texture = Some(TextureBinding {
                    texture: t.texture.clone(),
                    parameter: t.parameter.clone(),
                    sampler: t.sampler,
                    channels: channels_text(&t.swizzle),
                    uv: t.uv,
                });
            }
            Some(Term::VertexColor) => {
                self.note(format!("{name}: vertex colour alone approximated by white"));
                self.lose(Lost::VERTEX_COLOR);
                ch.value = splat(1.0);
                ch.vertex_color = true;
            }
            Some(Term::Grad(g)) => {
                ch.value = pad(bcast(g.value, g.n), usize::from(g.n), 1.0);
                self.gradient = Some(UvGradient { factors: g.factors });
            }
            Some(t @ (Term::Uv(_) | Term::Time(_))) => {
                self.note(format!(
                    "{name}: coordinates/time used as a colour; default kept"
                ));
                self.lose(if matches!(t, Term::Time(_)) {
                    Lost::TIME
                } else {
                    Lost::COORDINATES
                });
                ch.resolved = false;
            }
            Some(Term::Unknown(k) | Term::Dim(k)) => {
                self.note(format!("{name}: unsupported {k}; default kept"));
                self.lose(Lost::of_kind(&k));
                ch.resolved = false;
            }
            None => ch.resolved = false,
        }
        ch
    }
}

/// A GUID as 32 upper-case hex digits (`A`, `B`, `C`, `D` in order).
pub fn guid_hex(g: Guid) -> String {
    format!("{:08X}{:08X}{:08X}{:08X}", g.a, g.b, g.c, g.d)
}

/// The 16 serialized bytes of a GUID.
pub fn guid_bytes(g: Guid) -> [u8; 16] {
    let mut k = [0u8; 16];
    for (i, v) in [g.a, g.b, g.c, g.d].into_iter().enumerate() {
        k[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    k
}

/// Count, for each GUID in `ids`, how often its 16 serialized bytes occur in
/// `stream` (used to show that material resource `Id`s key the shader maps
/// of the `RefShaderCache-*` packages without decoding them).
pub fn count_guid_occurrences(stream: &[u8], ids: &[Guid]) -> HashMap<Guid, usize> {
    let wanted: HashMap<[u8; 16], Guid> = ids.iter().map(|g| (guid_bytes(*g), *g)).collect();
    let mut out: HashMap<Guid, usize> = ids.iter().map(|g| (*g, 0)).collect();
    for w in stream.windows(16) {
        if let Ok(k) = <[u8; 16]>::try_from(w)
            && let Some(g) = wanted.get(&k)
            && let Some(c) = out.get_mut(g)
        {
            *c += 1;
        }
    }
    out
}

fn add_texture(list: &mut BTreeSet<String>, ch: &Channel) {
    if let Some(t) = ch.texture.as_ref().and_then(|b| b.texture.as_ref()) {
        list.insert(t.clone());
    }
}

impl MaterialDecoder<'_> {
    /// Approximate export `index` of `lp` (a material or material instance).
    pub fn approximate(
        &self,
        lp: &Arc<LoadedPackage>,
        index: usize,
    ) -> Result<ApproxMaterial, MaterialError> {
        self.approximate_inner(lp, index, false).map(|(a, _)| a)
    }

    /// [`Self::approximate`] plus the walk of every material input: one
    /// line per evaluated expression (kind, the properties read, the reduced
    /// value), indented by depth, under a line naming the input. A local
    /// diagnostic for `asamu-import materials --explain`; it describes the
    /// original's graph, so it is printed and never written to converted
    /// data or the repository.
    pub fn approximate_traced(
        &self,
        lp: &Arc<LoadedPackage>,
        index: usize,
    ) -> Result<(ApproxMaterial, Vec<String>), MaterialError> {
        self.approximate_inner(lp, index, true)
    }

    fn approximate_inner(
        &self,
        lp: &Arc<LoadedPackage>,
        index: usize,
        trace: bool,
    ) -> Result<(ApproxMaterial, Vec<String>), MaterialError> {
        let chain = self.resolve_chain(lp, index);
        let Some((_, leaf)) = chain.links.first() else {
            return Err(MaterialError::Malformed {
                path: lp.qualified(index).unwrap_or_default(),
                detail: chain.error.unwrap_or_else(|| "not decodable".to_owned()),
            });
        };
        if leaf.class == MaterialClass::MaterialFunction {
            return Err(MaterialError::NotAMaterial {
                export: index,
                class: leaf.class_path.clone(),
            });
        }
        let params = ParameterValues::from_chain(&chain);
        let mut ev = Eval {
            dec: self,
            params: &params,
            notes: BTreeSet::new(),
            unsupported: BTreeMap::new(),
            reached: BTreeMap::new(),
            extra_textures: BTreeSet::new(),
            visits: 0,
            lossy: false,
            trace: trace.then(Vec::new),
            lost: Lost::NONE,
            last_lost: Lost::NONE,
            gradient: None,
            placed: true,
            fold_means: false,
            homes: chain.links.iter().map(|(p, _)| p.clone()).collect(),
            stats: BTreeMap::new(),
            runtime_textures: BTreeSet::new(),
            dropped: BTreeMap::new(),
            gradients: BTreeMap::new(),
        };
        let phys_material = chain
            .links
            .iter()
            .find_map(|(_, m)| get_object(&m.properties, "PhysMaterial").map(str::to_owned));
        let chain_paths: Vec<String> = chain.links.iter().map(|(_, m)| m.path.clone()).collect();
        let resource_id = chain
            .links
            .iter()
            .find_map(|(_, m)| m.native.resources.first().map(|r| guid_hex(r.resource.id)));
        let black = [0.0, 0.0, 0.0, 1.0];
        let Some((bpkg, base)) = chain.base() else {
            let reason = chain
                .error
                .clone()
                .unwrap_or_else(|| "no base material".to_owned());
            ev.note(format!("fallback: {reason}"));
            let grey = Channel {
                source: ChannelSource::Default,
                value: [0.5, 0.5, 0.5, 1.0],
                bias: splat(0.0),
                texture: None,
                vertex_color: false,
                resolved: false,
            };
            let zero = Channel {
                value: black,
                resolved: true,
                ..grey.clone()
            };
            let approx = ApproxMaterial {
                class: leaf.class,
                chain: chain_paths,
                base_material: None,
                status: ApproxStatus::Fallback,
                resource_id,
                lossless: false,
                blend_mode: "BLEND_Opaque".to_owned(),
                alpha_mode: "opaque",
                alpha_cutoff: None,
                two_sided: false,
                unlit: false,
                lighting_model: "MLM_Phong".to_owned(),
                decal: false,
                base_color: grey,
                normal: None,
                emissive: zero.clone(),
                emissive_intensity: 0.0,
                specular: zero,
                specular_power: Channel {
                    value: splat(15.0),
                    source: ChannelSource::Default,
                    bias: splat(0.0),
                    texture: None,
                    vertex_color: false,
                    resolved: true,
                },
                specular_level: 0.0,
                roughness: roughness_from_specular_power(15.0),
                metallic: 0.0,
                opacity: None,
                phys_material,
                parameters: params.clone(),
                textures: Vec::new(),
                dropped_textures: Vec::new(),
                expressions: BTreeMap::new(),
                unsupported: BTreeMap::new(),
                notes: ev.notes.into_iter().collect(),
                dropped: BTreeMap::new(),
                gradients: BTreeMap::new(),
                texture_stats: BTreeMap::new(),
                runtime_textures: Vec::new(),
            };
            return Ok((approx, Vec::new()));
        };
        let props = &base.properties;
        let blend_mode = get_name(props, "BlendMode")
            .unwrap_or("BLEND_Opaque")
            .to_owned();
        let lighting_model = get_name(props, "LightingModel")
            .unwrap_or("MLM_Phong")
            .to_owned();
        let (alpha_mode, blend_note) = alpha_mode_of(&blend_mode);
        if let Some(n) = blend_note {
            ev.note(n);
        }
        let unlit = lighting_model.eq_ignore_ascii_case("MLM_Unlit");
        if lighting_model.eq_ignore_ascii_case("MLM_Custom") {
            ev.note("MLM_Custom lighting approximated by Phong (CustomLighting inputs ignored)");
        }
        let base_color = ev.material_input(
            bpkg,
            props,
            "DiffuseColor",
            "base_color",
            InputShape::Color,
            black,
        );
        let normal = prop(props, "Normal")
            .map(expr_link)
            .is_some_and(|l| l.expression.is_some())
            .then(|| {
                ev.material_input(
                    bpkg,
                    props,
                    "Normal",
                    "normal",
                    InputShape::Vector,
                    [0.0, 0.0, 1.0, 0.0],
                )
            });
        let emissive = ev.material_input(
            bpkg,
            props,
            "EmissiveColor",
            "emissive",
            InputShape::Color,
            black,
        );
        let specular = ev.material_input(
            bpkg,
            props,
            "SpecularColor",
            "specular",
            InputShape::Color,
            black,
        );
        let specular_power = ev.material_input(
            bpkg,
            props,
            "SpecularPower",
            "specular_power",
            InputShape::Scalar,
            splat(15.0),
        );
        let opacity = match alpha_mode {
            "opaque" => None,
            "mask" => Some(ev.material_input(
                bpkg,
                props,
                "OpacityMask",
                "opacity",
                InputShape::Scalar,
                splat(1.0),
            )),
            _ => Some(ev.material_input(
                bpkg,
                props,
                "Opacity",
                "opacity",
                InputShape::Scalar,
                splat(1.0),
            )),
        };
        let alpha_cutoff = (alpha_mode == "mask")
            .then(|| get_f32(props, "OpacityMaskClipValue").unwrap_or(0.3333));
        if specular_power.texture.is_some() {
            ev.note("SpecularPower from a texture: roughness from its multiplier");
        }
        let specular_level =
            ((specular.value[0] + specular.value[1] + specular.value[2]) / 3.0).clamp(0.0, 1.0);
        let roughness = roughness_from_specular_power(specular_power.value[0]);
        let main = if unlit { &emissive } else { &base_color };
        let status = if main.resolved {
            ApproxStatus::Approximated
        } else {
            ApproxStatus::Fallback
        };
        let mut textures = BTreeSet::new();
        for ch in [&base_color, &emissive, &specular, &specular_power]
            .into_iter()
            .chain(normal.as_ref())
            .chain(opacity.as_ref())
        {
            add_texture(&mut textures, ch);
        }
        let dropped: Vec<String> = ev
            .extra_textures
            .iter()
            .filter(|t| !textures.contains(*t))
            .cloned()
            .collect();
        let walk = ev.trace.take().unwrap_or_default();
        let texture_stats: BTreeMap<String, TextureStats> = ev
            .stats
            .iter()
            .filter(|(t, _)| textures.contains(*t))
            .map(|(t, s)| (t.clone(), *s))
            .collect();
        let runtime_textures: Vec<String> = ev
            .runtime_textures
            .iter()
            .filter(|t| textures.contains(*t))
            .cloned()
            .collect();
        let approx = ApproxMaterial {
            class: leaf.class,
            chain: chain_paths,
            base_material: Some(base.path.clone()),
            status,
            resource_id,
            lossless: !ev.lossy,
            blend_mode,
            alpha_mode,
            alpha_cutoff,
            two_sided: get_bool(props, "TwoSided").unwrap_or(false),
            unlit,
            lighting_model,
            decal: base.class == MaterialClass::DecalMaterial,
            base_color,
            normal,
            emissive_intensity: emissive.value[..3].iter().copied().fold(0.0f32, f32::max),
            emissive,
            specular,
            specular_power,
            specular_level,
            roughness,
            metallic: 0.0,
            opacity,
            phys_material,
            parameters: params.clone(),
            textures: textures.into_iter().collect(),
            dropped_textures: dropped,
            expressions: ev.reached,
            unsupported: ev.unsupported,
            notes: ev.notes.into_iter().collect(),
            dropped: ev.dropped,
            gradients: ev.gradients,
            texture_stats,
            runtime_textures,
        };
        Ok((approx, walk))
    }
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// Decode results for one material class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClassCoverage {
    /// Exports of the class.
    pub total: usize,
    /// Of which class default objects.
    pub default_objects: usize,
    /// Prelude, tags and native data consumed exactly.
    pub decoded: usize,
    /// Native data re-encoded byte for byte.
    pub round_trip: usize,
}

/// Material coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MaterialCoverage {
    /// Package name.
    pub package: String,
    /// Per material class.
    pub classes: BTreeMap<String, ClassCoverage>,
    /// Expression exports by kind.
    pub expression_exports: BTreeMap<String, usize>,
    /// Expression exports whose tags end exactly at `SerialSize`.
    pub expression_exact: usize,
    /// Expression exports that failed to decode.
    pub expression_failures: usize,
    /// Native bytes consumed by material tails.
    pub native_bytes: u64,
    /// Quality masks seen.
    pub quality_masks: BTreeMap<u32, usize>,
    /// Resources with compile errors.
    pub resources_with_compile_errors: usize,
    /// Instances with a static permutation resource.
    pub static_permutation_instances: usize,
    /// Approximation results (materials and instances, not functions or
    /// default objects).
    pub approximated: usize,
    /// Of `approximated`, those without any note.
    pub lossless: usize,
    /// Fallbacks.
    pub fallback: usize,
    /// Alpha modes.
    pub alpha_modes: BTreeMap<String, usize>,
    /// Lighting models.
    pub lighting_models: BTreeMap<String, usize>,
    /// Base colour source (`texture`, `constant`, `default`, `unresolved`).
    pub base_color: BTreeMap<String, usize>,
    /// With a normal map.
    pub normal_maps: usize,
    /// With a textured emissive channel.
    pub emissive_textures: usize,
    /// With a panning or rotating texture somewhere.
    pub animated_uvs: usize,
    /// With tiling other than 1 or a UV channel other than 0 somewhere.
    pub transformed_uvs: usize,
    /// Unsupported expression kinds (summed over materials).
    pub unsupported: BTreeMap<String, usize>,
    /// Failures (path: error), capped.
    pub failures: Vec<String>,
}

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

fn channel_kind(ch: &Channel) -> &'static str {
    if !ch.resolved {
        "unresolved"
    } else if ch.texture.is_some() {
        "texture"
    } else {
        match ch.source {
            ChannelSource::Constant => "constant",
            ChannelSource::Default => "default",
            ChannelSource::Expression if ch.vertex_color => "vertex_color",
            ChannelSource::Expression => "graph_constant",
        }
    }
}

impl MaterialCoverage {
    /// Fold one approximation into the statistics.
    pub fn add_approximation(&mut self, a: &ApproxMaterial) {
        match a.status {
            ApproxStatus::Approximated => self.approximated += 1,
            ApproxStatus::Fallback => self.fallback += 1,
        }
        if a.status == ApproxStatus::Approximated && a.lossless {
            self.lossless += 1;
        }
        bump(&mut self.alpha_modes, a.alpha_mode.to_owned());
        bump(&mut self.lighting_models, a.lighting_model.clone());
        bump(&mut self.base_color, channel_kind(&a.base_color).to_owned());
        if a.normal.as_ref().is_some_and(|n| n.texture.is_some()) {
            self.normal_maps += 1;
        }
        if a.emissive.texture.is_some() {
            self.emissive_textures += 1;
        }
        let chans: Vec<&Channel> = [&a.base_color, &a.emissive, &a.specular, &a.specular_power]
            .into_iter()
            .chain(a.normal.as_ref())
            .chain(a.opacity.as_ref())
            .collect();
        let uvs: Vec<&UvTransform> = chans
            .iter()
            .filter_map(|c| c.texture.as_ref().map(|t| &t.uv))
            .collect();
        if uvs
            .iter()
            .any(|u| u.panning != [0.0, 0.0] || u.rotation != 0.0)
        {
            self.animated_uvs += 1;
        }
        if uvs.iter().any(|u| {
            u.scale != [1.0, 1.0]
                || u.channel != 0
                || u.offset != [0.0, 0.0]
                || u.rotation_angle != 0.0
        }) {
            self.transformed_uvs += 1;
        }
        for (k, v) in &a.unsupported {
            *self.unsupported.entry(k.clone()).or_insert(0) += v;
        }
    }
}

/// Decode and approximate every material-related export of `lp`.
pub fn material_coverage(dec: &MaterialDecoder<'_>, lp: &Arc<LoadedPackage>) -> MaterialCoverage {
    scan_package(dec, lp, &mut |_, _| {})
}

/// Decode and approximate every material-related export of `lp`, calling
/// `on_material` with each renderable material or instance (not functions
/// or class default objects) and its approximation.
pub fn scan_package(
    dec: &MaterialDecoder<'_>,
    lp: &Arc<LoadedPackage>,
    on_material: &mut dyn FnMut(&MaterialObject, Result<&ApproxMaterial, &MaterialError>),
) -> MaterialCoverage {
    let mut cov = MaterialCoverage {
        package: lp.name.clone(),
        ..MaterialCoverage::default()
    };
    let fail = |cov: &mut MaterialCoverage, s: String| {
        if cov.failures.len() < 64 {
            cov.failures.push(s);
        }
    };
    for index in 0..lp.package.exports.len() {
        let Ok(class_path) = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
        else {
            continue;
        };
        if let Some(kind) = expression_kind(&class_path) {
            if dec
                .set()
                .super_chain(&class_path)
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Engine.MaterialExpression"))
                || class_path.eq_ignore_ascii_case("Engine.MaterialExpression")
            {
                bump(&mut cov.expression_exports, kind.to_owned());
                match dec.set().decode(lp, index) {
                    Ok(o) if o.native_tail() == 0 => cov.expression_exact += 1,
                    Ok(_) => {}
                    Err(e) => {
                        cov.expression_failures += 1;
                        fail(&mut cov, format!("export {index}: {e}"));
                    }
                }
            }
            continue;
        }
        let Some(class) = dec.class_of(&class_path) else {
            continue;
        };
        let entry = cov.classes.entry(class.name().to_owned()).or_default();
        entry.total += 1;
        let m = match dec.decode(lp, index) {
            Ok(m) => m,
            Err(e) => {
                fail(&mut cov, format!("export {index}: {e}"));
                continue;
            }
        };
        let entry = cov.classes.entry(class.name().to_owned()).or_default();
        entry.decoded += 1;
        if m.is_default_object {
            entry.default_objects += 1;
        }
        let tail = lp
            .package
            .export_data(index)
            .ok()
            .and_then(|p| p.get(m.properties_end..));
        if let (Some(tail), Some(enc)) =
            (tail, encode_material_native(&m.native, native_kind_of(&m)))
        {
            if enc == tail {
                entry.round_trip += 1;
            }
            cov.native_bytes += tail.len() as u64;
        }
        if let Some(q) = m.native.quality_mask {
            bump(&mut cov.quality_masks, q);
        }
        cov.resources_with_compile_errors += m
            .native
            .resources
            .iter()
            .filter(|r| !r.resource.compile_errors.is_empty())
            .count();
        if m.native
            .resources
            .iter()
            .any(|r| r.static_parameters.is_some())
        {
            cov.static_permutation_instances += 1;
        }
        if m.is_default_object || class == MaterialClass::MaterialFunction {
            continue;
        }
        match dec.approximate(lp, index) {
            Ok(a) => {
                cov.add_approximation(&a);
                on_material(&m, Ok(&a));
            }
            Err(e) => {
                cov.fallback += 1;
                fail(&mut cov, format!("{}: {e}", m.path));
                on_material(&m, Err(&e));
            }
        }
    }
    cov
}
