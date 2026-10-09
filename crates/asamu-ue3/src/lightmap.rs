//! Precomputed (baked) lighting attached to primitive components: light maps,
//! shadow maps and per-instance lighting data, for UE3 v868 / licensee 0.
//!
//! The coefficient *textures* themselves (`LightMapTexture2D`,
//! `ShadowMapTexture2D`) are decoded by [`crate::texture`]; this module
//! decodes the component data that says which texture region, scale and UV
//! channel each component uses. Layouts were read from the unstripped Mac
//! executable's serializers (local Ghidra decompilation, never committed) and
//! then proven on the data: every component export of every shipped package
//! is consumed exactly to `SerialSize` and re-encodes byte for byte (see
//! `docs/reverse-engineering/LIGHTMAPS.md` for evidence and confidence).
//!
//! ```text
//! light map reference (FLightMapSerializeHelper)
//!   u32 Type                     0 none | 1 FLightMap1D | 2 FLightMap2D
//! FLightMap1D (vertex light map)
//!   TArray<FGuid> LightGuids
//!   obj Owner
//!   bulk data DirectionalSamples elements of 8 bytes: 2 x FColor (directional coefficients)
//!   3 x FVector ScaleVectors     one per stored coefficient
//!   bulk data SimpleSamples      elements of 4 bytes: 1 x FColor (simple coefficient)
//! FLightMap2D (texture light map)
//!   TArray<FGuid> LightGuids
//!   3 x { obj Texture, FVector ScaleVector }
//!   FVector2D CoordinateScale, FVector2D CoordinateBias
//!
//! StaticMeshComponent           (after tags) TArray<FStaticMeshComponentLODInfo> LODData
//! FStaticMeshComponentLODInfo   TArray<obj> ShadowMaps (ShadowMap2D)
//!                               TArray<obj> ShadowVertexBuffers (ShadowMap1D)
//!                               light map reference
//!                               u8 bHasOverrideVertexColors [+ FColorVertexBuffer]
//!                               TArray<FPaintedVertex> PaintedVertices
//! FColorVertexBuffer            u32 Stride, u32 NumVertices, bulk TArray<FColor> when NumVertices != 0
//! InstancedStaticMeshComponent  StaticMeshComponent data + bulk TArray<instance, 80 bytes>
//!                               instance = FMatrix Transform, FVector2D LightmapUVBias,
//!                                          FVector2D ShadowmapUVBias
//! ModelComponent                obj Model, i32 ZoneIndex, TIndirectArray<FModelElement> Elements,
//!                               u16 ComponentIndex, TArray<u16> Nodes
//! FModelElement                 light map reference, obj Component, obj Material,
//!                               TArray<u16> Nodes, TArray<obj> ShadowMaps, TArray<FGuid> IrrelevantLights
//! SpeedTreeComponent            5 light map references
//! FluidSurfaceComponent         1 light map reference
//! ShadowMap1D                   TArray<f32> Samples, FGuid LightGuid
//! ```
//!
//! Coefficient semantics (from the executable's `FLightMap2D::GetInteraction`):
//! with directional light maps enabled the renderer binds coefficients 0
//! and 1 (two textures); without, it binds only coefficient
//! [`SIMPLE_LIGHTMAP_COEF_INDEX`] (2), the "simple" light map. Each sampled
//! texel is multiplied by its coefficient's `ScaleVector`.
//!
//! Every count is checked against the remaining bytes before anything is
//! allocated, arithmetic is checked, and malformed input yields an
//! [`ObjectError`]; nothing here panics.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::bulkdata::{BulkCompression, BulkDataRecord, BulkStorage, read_bulk_record};
use crate::model::LoadedPackage;
use crate::object::{DecodedObject, ObjResult, ObjectError, decode_object};
use crate::package::Package;
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::{Guid, PackageIndex};
use crate::writer::Writer;

/// Light map reference type: no light map.
pub const LIGHTMAP_TYPE_NONE: u32 = 0;
/// Light map reference type: [`LightMap1D`] (per-vertex samples).
pub const LIGHTMAP_TYPE_1D: u32 = 1;
/// Light map reference type: [`LightMap2D`] (texture).
pub const LIGHTMAP_TYPE_2D: u32 = 2;
/// Coefficients stored per light sample: two directional plus one simple.
pub const NUM_STORED_LIGHTMAP_COEF: usize = 3;
/// Directional coefficients per light sample.
pub const NUM_DIRECTIONAL_LIGHTMAP_COEF: usize = 2;
/// Index of the simple (non-directional) coefficient.
pub const SIMPLE_LIGHTMAP_COEF_INDEX: usize = 2;
/// Serialized size of one directional sample (two `FColor`s).
pub const DIRECTIONAL_SAMPLE_SIZE: usize = 8;
/// Serialized size of one simple sample (one `FColor`).
pub const SIMPLE_SAMPLE_SIZE: usize = 4;
/// Serialized size of one `FInstancedStaticMeshInstanceData`.
pub const INSTANCE_DATA_SIZE: usize = 80;
/// Serialized size of one `FPaintedVertex` (position, packed normal, color).
pub const PAINTED_VERTEX_SIZE: usize = 20;
/// Light map references of a `SpeedTreeComponent` (branch, frond, leaf card,
/// leaf mesh, billboard).
pub const SPEEDTREE_LIGHTMAPS: usize = 5;
/// Element size of an `FColor` in a color vertex buffer.
pub const COLOR_ELEMENT_SIZE: usize = 4;
/// Smallest serialized `FStaticMeshComponentLODInfo` (empty arrays, no light
/// map, no colors).
pub const LOD_INFO_MIN_SIZE: usize = 4 + 4 + 4 + 1 + 4;
/// Smallest serialized `FModelElement`.
pub const MODEL_ELEMENT_MIN_SIZE: usize = 4 + 4 + 4 + 4 + 4 + 4;

/// Quantized samples of a vertex light map (an inline, uncompressed bulk
/// record; the raw bytes are kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuantizedSamples {
    /// The bulk data record header.
    pub record: BulkDataRecord,
    /// Element size the record was read with.
    pub element_size: usize,
    /// Raw sample bytes (`ElementCount * element_size`).
    #[serde(skip)]
    pub data: Vec<u8>,
}

impl QuantizedSamples {
    /// Number of samples.
    pub fn len(&self) -> usize {
        self.data.len().checked_div(self.element_size).unwrap_or(0)
    }

    /// True when there are no samples.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Coefficient `coef` of sample `i` as stored (`FColor` bytes B, G, R, A).
    pub fn color(&self, i: usize, coef: usize) -> Option<[u8; 4]> {
        if coef.checked_mul(4)? >= self.element_size {
            return None;
        }
        let start = i.checked_mul(self.element_size)?.checked_add(coef * 4)?;
        let b = self.data.get(start..start.checked_add(4)?)?;
        Some([b[0], b[1], b[2], b[3]])
    }
}

/// `FLightMap1D`: one quantized sample per mesh vertex.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LightMap1D {
    /// GUIDs of the lights baked into this light map.
    pub light_guids: Vec<Guid>,
    /// Owner object (package index).
    pub owner: PackageIndex,
    /// Directional coefficients (2 per sample).
    pub directional_samples: QuantizedSamples,
    /// Per-coefficient scale (directional 0, directional 1, simple).
    pub scale_vectors: [[f32; 3]; NUM_STORED_LIGHTMAP_COEF],
    /// Simple coefficient (1 per sample).
    pub simple_samples: QuantizedSamples,
}

/// `FLightMap2D`: a region of three coefficient textures.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LightMap2D {
    /// GUIDs of the lights baked into this light map.
    pub light_guids: Vec<Guid>,
    /// Coefficient textures (`LightMapTexture2D`; directional 0, directional
    /// 1, simple).
    pub textures: [PackageIndex; NUM_STORED_LIGHTMAP_COEF],
    /// Per-coefficient scale applied to the sampled texel.
    pub scale_vectors: [[f32; 3]; NUM_STORED_LIGHTMAP_COEF],
    /// Light map UV → texture UV scale.
    pub coordinate_scale: [f32; 2],
    /// Light map UV → texture UV bias.
    pub coordinate_bias: [f32; 2],
}

impl LightMap2D {
    /// The texture rectangle `[min_u, min_v, max_u, max_v]` that light map
    /// UVs in `[0, 1]` map to (`uv * scale + bias`).
    pub fn uv_rect(&self) -> [f32; 4] {
        let [su, sv] = self.coordinate_scale;
        let [bu, bv] = self.coordinate_bias;
        [bu, bv, bu + su, bv + sv]
    }
}

/// A serialized light map reference.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum LightMap {
    /// No light map (type 0).
    None,
    /// Vertex light map (type 1).
    OneD(LightMap1D),
    /// Texture light map (type 2).
    TwoD(LightMap2D),
}

impl LightMap {
    /// The serialized type value.
    pub fn type_value(&self) -> u32 {
        match self {
            LightMap::None => LIGHTMAP_TYPE_NONE,
            LightMap::OneD(_) => LIGHTMAP_TYPE_1D,
            LightMap::TwoD(_) => LIGHTMAP_TYPE_2D,
        }
    }

    /// Baked light GUIDs (empty for [`LightMap::None`]).
    pub fn light_guids(&self) -> &[Guid] {
        match self {
            LightMap::None => &[],
            LightMap::OneD(m) => &m.light_guids,
            LightMap::TwoD(m) => &m.light_guids,
        }
    }
}

/// `FColorVertexBuffer` (per-instance vertex color override).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColorVertexBuffer {
    /// `Stride`.
    pub stride: u32,
    /// `NumVertices`.
    pub num_vertices: u32,
    /// Colors as stored (B, G, R, A).
    pub colors: Vec<[u8; 4]>,
}

/// `FPaintedVertex` (mesh-paint source vertex).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PaintedVertex {
    /// Position.
    pub position: [f32; 3],
    /// Packed normal bytes.
    pub normal: [u8; 4],
    /// Color (B, G, R, A).
    pub color: [u8; 4],
}

/// `FStaticMeshComponentLODInfo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticMeshComponentLodInfo {
    /// `ShadowMap2D` objects (static shadows of lights that are not baked
    /// into the light map).
    pub shadow_maps: Vec<PackageIndex>,
    /// `ShadowMap1D` objects (per-vertex static shadows).
    pub shadow_vertex_buffers: Vec<PackageIndex>,
    /// The light map.
    pub light_map: LightMap,
    /// Per-instance vertex colors.
    pub override_vertex_colors: Option<ColorVertexBuffer>,
    /// Mesh-paint source vertices.
    pub painted_vertices: Vec<PaintedVertex>,
}

/// `FInstancedStaticMeshInstanceData`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct InstanceData {
    /// Instance transform (row-major `FMatrix`, row-vector convention).
    pub transform: [f32; 16],
    /// Light map UV offset of this instance.
    pub lightmap_uv_bias: [f32; 2],
    /// Shadow map UV offset of this instance.
    pub shadowmap_uv_bias: [f32; 2],
}

/// Native data of a `StaticMeshComponent` (and `InstancedStaticMeshComponent`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticMeshComponentNative {
    /// Per-LOD lighting.
    pub lods: Vec<StaticMeshComponentLodInfo>,
    /// Instances (`InstancedStaticMeshComponent` only).
    pub instances: Option<Vec<InstanceData>>,
}

/// `FModelElement`: the BSP nodes of a model component that share a material
/// and a light map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelElement {
    /// The light map.
    pub light_map: LightMap,
    /// Owning component.
    pub component: PackageIndex,
    /// Material.
    pub material: PackageIndex,
    /// BSP node indices (into the level model's nodes).
    pub nodes: Vec<u16>,
    /// `ShadowMap2D` objects.
    pub shadow_maps: Vec<PackageIndex>,
    /// GUIDs of lights that do not affect this element.
    pub irrelevant_lights: Vec<Guid>,
}

/// Native data of a `ModelComponent` (a group of BSP surfaces).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelComponentNative {
    /// The model (level BSP).
    pub model: PackageIndex,
    /// Zone index.
    pub zone_index: i32,
    /// Elements.
    pub elements: Vec<ModelElement>,
    /// Index of this component in the level's `ModelComponents`.
    pub component_index: u16,
    /// BSP nodes of the whole component.
    pub nodes: Vec<u16>,
}

/// Native data of a `ShadowMap1D`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShadowMap1DNative {
    /// One shadow factor per vertex.
    pub samples: Vec<f32>,
    /// The light whose shadowing this is.
    pub light_guid: Guid,
}

/// Which native layout an export uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum LightingLayout {
    /// `Engine.StaticMeshComponent`.
    StaticMeshComponent,
    /// `Engine.InstancedStaticMeshComponent`.
    InstancedStaticMeshComponent,
    /// `Engine.ModelComponent`.
    ModelComponent,
    /// `Engine.SpeedTreeComponent`.
    SpeedTreeComponent,
    /// `Engine.FluidSurfaceComponent`.
    FluidSurfaceComponent,
    /// `Engine.ShadowMap1D`.
    ShadowMap1D,
}

impl LightingLayout {
    /// Engine class name of the layout.
    pub fn class_name(self) -> &'static str {
        match self {
            LightingLayout::StaticMeshComponent => "StaticMeshComponent",
            LightingLayout::InstancedStaticMeshComponent => "InstancedStaticMeshComponent",
            LightingLayout::ModelComponent => "ModelComponent",
            LightingLayout::SpeedTreeComponent => "SpeedTreeComponent",
            LightingLayout::FluidSurfaceComponent => "FluidSurfaceComponent",
            LightingLayout::ShadowMap1D => "ShadowMap1D",
        }
    }

    /// Every layout.
    pub const ALL: [LightingLayout; 6] = [
        LightingLayout::StaticMeshComponent,
        LightingLayout::InstancedStaticMeshComponent,
        LightingLayout::ModelComponent,
        LightingLayout::SpeedTreeComponent,
        LightingLayout::FluidSurfaceComponent,
        LightingLayout::ShadowMap1D,
    ];
}

/// Decoded native lighting data of one export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum LightingNative {
    /// Static mesh component (instanced or not).
    StaticMesh(StaticMeshComponentNative),
    /// BSP model component.
    Model(ModelComponentNative),
    /// SpeedTree component light maps.
    SpeedTree(Vec<LightMap>),
    /// Fluid surface light map.
    FluidSurface(LightMap),
    /// Per-vertex shadow map.
    ShadowMap1D(ShadowMap1DNative),
}

/// A decoded export: prelude, tags and lighting native data.
#[derive(Debug, Clone, Serialize)]
pub struct DecodedLighting {
    /// Prelude and tagged properties.
    pub object: DecodedObject,
    /// Layout used.
    pub layout: LightingLayout,
    /// Native data.
    pub native: LightingNative,
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

fn read_array<T>(
    r: &mut Reader<'_>,
    what: &'static str,
    min_element_size: usize,
    mut f: impl FnMut(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<Vec<T>> {
    let n = r.read_count(what, min_element_size)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(f(r)?);
    }
    Ok(out)
}

fn read_vec2(r: &mut Reader<'_>) -> ObjResult<[f32; 2]> {
    Ok([r.read_f32()?, r.read_f32()?])
}

fn read_vec3(r: &mut Reader<'_>) -> ObjResult<[f32; 3]> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

fn read_color(r: &mut Reader<'_>) -> ObjResult<[u8; 4]> {
    let b = r.read_bytes(4)?;
    Ok([b[0], b[1], b[2], b[3]])
}

fn read_index_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<PackageIndex>> {
    read_array(r, what, 4, |r| Ok(r.read_package_index()?))
}

fn read_guid_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<Guid>> {
    read_array(r, what, 16, |r| Ok(r.read_guid()?))
}

fn read_u16_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<u16>> {
    read_array(r, what, 2, |r| Ok(r.read_u16()?))
}

/// Read a bulk array header (`i32 ElementSize`, `i32 Count`) whose element
/// size must be `expected`; returns the count after checking that
/// `count * expected` bytes remain.
fn read_bulk_count(r: &mut Reader<'_>, what: &'static str, expected: usize) -> ObjResult<usize> {
    let at = r.position();
    let elem = r.read_i32()?;
    if usize::try_from(elem).ok() != Some(expected) {
        return Err(malformed(
            what,
            at,
            format!("bulk element size {elem}, expected {expected}"),
        ));
    }
    Ok(r.read_count(what, expected)?)
}

/// Read a sample bulk record: inline, uncompressed, `SizeOnDisk ==
/// ElementCount * element_size` (the only form the shipped data uses; any
/// other form is refused rather than guessed).
fn read_samples(
    r: &mut Reader<'_>,
    what: &'static str,
    element_size: usize,
) -> ObjResult<QuantizedSamples> {
    let at = r.position();
    let record = read_bulk_record(r).map_err(|e| malformed(what, at, e.to_string()))?;
    if record.storage() != BulkStorage::Inline || record.compression() != BulkCompression::None {
        return Err(malformed(
            what,
            at,
            format!(
                "bulk flags {:#x}: only inline uncompressed samples are supported",
                record.flags
            ),
        ));
    }
    let expected = record.uncompressed_len(element_size);
    if expected != Some(record.stored_len()) {
        return Err(malformed(
            what,
            at,
            format!(
                "{} elements of {element_size} bytes but {} bytes stored",
                record.element_count, record.size_on_disk
            ),
        ));
    }
    let start = record.inline_start();
    let end = start
        .checked_add(record.stored_len())
        .ok_or_else(|| malformed(what, at, "payload end overflows"))?;
    let data = r
        .data()
        .get(start..end)
        .ok_or_else(|| malformed(what, at, "payload outside the export"))?
        .to_vec();
    Ok(QuantizedSamples {
        record,
        element_size,
        data,
    })
}

fn read_scale_vectors(r: &mut Reader<'_>) -> ObjResult<[[f32; 3]; NUM_STORED_LIGHTMAP_COEF]> {
    let mut out = [[0.0; 3]; NUM_STORED_LIGHTMAP_COEF];
    for s in &mut out {
        *s = read_vec3(r)?;
    }
    Ok(out)
}

/// Read one serialized light map reference.
pub fn read_light_map(r: &mut Reader<'_>) -> ObjResult<LightMap> {
    let at = r.position();
    match r.read_u32()? {
        LIGHTMAP_TYPE_NONE => Ok(LightMap::None),
        LIGHTMAP_TYPE_1D => {
            let light_guids = read_guid_array(r, "FLightMap1D.LightGuids")?;
            let owner = r.read_package_index()?;
            let directional_samples =
                read_samples(r, "FLightMap1D.DirectionalSamples", DIRECTIONAL_SAMPLE_SIZE)?;
            let scale_vectors = read_scale_vectors(r)?;
            let simple_samples = read_samples(r, "FLightMap1D.SimpleSamples", SIMPLE_SAMPLE_SIZE)?;
            Ok(LightMap::OneD(LightMap1D {
                light_guids,
                owner,
                directional_samples,
                scale_vectors,
                simple_samples,
            }))
        }
        LIGHTMAP_TYPE_2D => {
            let light_guids = read_guid_array(r, "FLightMap2D.LightGuids")?;
            let mut textures = [PackageIndex::NULL; NUM_STORED_LIGHTMAP_COEF];
            let mut scale_vectors = [[0.0; 3]; NUM_STORED_LIGHTMAP_COEF];
            for i in 0..NUM_STORED_LIGHTMAP_COEF {
                textures[i] = r.read_package_index()?;
                scale_vectors[i] = read_vec3(r)?;
            }
            let coordinate_scale = read_vec2(r)?;
            let coordinate_bias = read_vec2(r)?;
            Ok(LightMap::TwoD(LightMap2D {
                light_guids,
                textures,
                scale_vectors,
                coordinate_scale,
                coordinate_bias,
            }))
        }
        t => Err(malformed(
            "light map type",
            at,
            format!("type {t}; only 0 (none), 1 (1D) and 2 (2D) exist"),
        )),
    }
}

fn read_color_vertex_buffer(r: &mut Reader<'_>) -> ObjResult<ColorVertexBuffer> {
    let stride = r.read_u32()?;
    let num_vertices = r.read_u32()?;
    let colors = if num_vertices == 0 {
        Vec::new()
    } else {
        let n = read_bulk_count(r, "override vertex colors", COLOR_ELEMENT_SIZE)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(read_color(r)?);
        }
        v
    };
    Ok(ColorVertexBuffer {
        stride,
        num_vertices,
        colors,
    })
}

fn read_lod_info(r: &mut Reader<'_>) -> ObjResult<StaticMeshComponentLodInfo> {
    let shadow_maps = read_index_array(r, "LODInfo.ShadowMaps")?;
    let shadow_vertex_buffers = read_index_array(r, "LODInfo.ShadowVertexBuffers")?;
    let light_map = read_light_map(r)?;
    let at = r.position();
    let override_vertex_colors = match r.read_u8()? {
        0 => None,
        1 => Some(read_color_vertex_buffer(r)?),
        v => {
            return Err(malformed(
                "LODInfo.bLoadVertexColorData",
                at,
                format!("flag {v}, not 0 or 1"),
            ));
        }
    };
    let painted_vertices = read_array(r, "LODInfo.PaintedVertices", PAINTED_VERTEX_SIZE, |r| {
        Ok(PaintedVertex {
            position: read_vec3(r)?,
            normal: read_color(r)?,
            color: read_color(r)?,
        })
    })?;
    Ok(StaticMeshComponentLodInfo {
        shadow_maps,
        shadow_vertex_buffers,
        light_map,
        override_vertex_colors,
        painted_vertices,
    })
}

fn read_instance(r: &mut Reader<'_>) -> ObjResult<InstanceData> {
    let mut transform = [0.0; 16];
    for v in &mut transform {
        *v = r.read_f32()?;
    }
    Ok(InstanceData {
        transform,
        lightmap_uv_bias: read_vec2(r)?,
        shadowmap_uv_bias: read_vec2(r)?,
    })
}

fn finish<T>(r: &Reader<'_>, value: T, what: &'static str) -> ObjResult<T> {
    if r.remaining() != 0 {
        return Err(malformed(
            what,
            r.position(),
            format!("{} bytes left after the native data", r.remaining()),
        ));
    }
    Ok(value)
}

/// Decode `StaticMeshComponent` native data starting at payload offset
/// `start` (the end of the tagged properties); `instanced` adds the
/// `InstancedStaticMeshComponent` instance array. Must end exactly at the end
/// of `data`.
pub fn decode_static_mesh_component_native(
    data: &[u8],
    start: usize,
    instanced: bool,
) -> ObjResult<StaticMeshComponentNative> {
    let mut r = Reader::at(data, start)?;
    let lods = read_array(&mut r, "LODData", LOD_INFO_MIN_SIZE, read_lod_info)?;
    let instances = if instanced {
        let n = read_bulk_count(&mut r, "PerInstanceSMData", INSTANCE_DATA_SIZE)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(read_instance(&mut r)?);
        }
        Some(v)
    } else {
        None
    };
    finish(
        &r,
        StaticMeshComponentNative { lods, instances },
        "StaticMeshComponent",
    )
}

fn read_model_element(r: &mut Reader<'_>) -> ObjResult<ModelElement> {
    let light_map = read_light_map(r)?;
    let component = r.read_package_index()?;
    let material = r.read_package_index()?;
    let nodes = read_u16_array(r, "FModelElement.Nodes")?;
    let shadow_maps = read_index_array(r, "FModelElement.ShadowMaps")?;
    let irrelevant_lights = read_guid_array(r, "FModelElement.IrrelevantLights")?;
    Ok(ModelElement {
        light_map,
        component,
        material,
        nodes,
        shadow_maps,
        irrelevant_lights,
    })
}

/// Decode `ModelComponent` native data (see the module documentation).
pub fn decode_model_component_native(data: &[u8], start: usize) -> ObjResult<ModelComponentNative> {
    let mut r = Reader::at(data, start)?;
    let model = r.read_package_index()?;
    let zone_index = r.read_i32()?;
    let elements = read_array(
        &mut r,
        "ModelComponent.Elements",
        MODEL_ELEMENT_MIN_SIZE,
        read_model_element,
    )?;
    let component_index = r.read_u16()?;
    let nodes = read_u16_array(&mut r, "ModelComponent.Nodes")?;
    finish(
        &r,
        ModelComponentNative {
            model,
            zone_index,
            elements,
            component_index,
            nodes,
        },
        "ModelComponent",
    )
}

/// Decode `SpeedTreeComponent` native data: five light map references.
pub fn decode_speedtree_component_native(data: &[u8], start: usize) -> ObjResult<Vec<LightMap>> {
    let mut r = Reader::at(data, start)?;
    let mut maps = Vec::with_capacity(SPEEDTREE_LIGHTMAPS);
    for _ in 0..SPEEDTREE_LIGHTMAPS {
        maps.push(read_light_map(&mut r)?);
    }
    finish(&r, maps, "SpeedTreeComponent")
}

/// Decode `FluidSurfaceComponent` native data: one light map reference.
pub fn decode_fluid_surface_component_native(data: &[u8], start: usize) -> ObjResult<LightMap> {
    let mut r = Reader::at(data, start)?;
    let map = read_light_map(&mut r)?;
    finish(&r, map, "FluidSurfaceComponent")
}

/// Decode `ShadowMap1D` native data.
pub fn decode_shadow_map_1d_native(data: &[u8], start: usize) -> ObjResult<ShadowMap1DNative> {
    let mut r = Reader::at(data, start)?;
    let samples = read_array(&mut r, "ShadowMap1D.Samples", 4, |r| Ok(r.read_f32()?))?;
    let light_guid = r.read_guid()?;
    finish(
        &r,
        ShadowMap1DNative {
            samples,
            light_guid,
        },
        "ShadowMap1D",
    )
}

/// Decode native data of `layout` starting at `start`.
pub fn decode_lighting_native(
    layout: LightingLayout,
    data: &[u8],
    start: usize,
) -> ObjResult<LightingNative> {
    Ok(match layout {
        LightingLayout::StaticMeshComponent => {
            LightingNative::StaticMesh(decode_static_mesh_component_native(data, start, false)?)
        }
        LightingLayout::InstancedStaticMeshComponent => {
            LightingNative::StaticMesh(decode_static_mesh_component_native(data, start, true)?)
        }
        LightingLayout::ModelComponent => {
            LightingNative::Model(decode_model_component_native(data, start)?)
        }
        LightingLayout::SpeedTreeComponent => {
            LightingNative::SpeedTree(decode_speedtree_component_native(data, start)?)
        }
        LightingLayout::FluidSurfaceComponent => {
            LightingNative::FluidSurface(decode_fluid_surface_component_native(data, start)?)
        }
        LightingLayout::ShadowMap1D => {
            LightingNative::ShadowMap1D(decode_shadow_map_1d_native(data, start)?)
        }
    })
}

/// The lighting layout of export `index`, when its class is exactly one of
/// the [`LightingLayout`] classes of package `Engine` (subclasses may append
/// data and are not accepted).
pub fn lighting_layout(pkg: &Package, index: usize) -> Option<LightingLayout> {
    let class = pkg.export_class_name(index).ok()?;
    let layout = LightingLayout::ALL
        .into_iter()
        .find(|l| l.class_name() == class)?;
    matches!(pkg.export_class_package(index), Ok(Some(p)) if p.eq_ignore_ascii_case("Engine"))
        .then_some(layout)
}

/// Decode export `index` (prelude, tags and lighting native data).
pub fn decode_lighting(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<DecodedLighting> {
    let Some(layout) = lighting_layout(pkg, index) else {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "a component with baked lighting data",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    };
    decode_lighting_as(pkg, own_name, index, schema, layout)
}

/// The nearest [`LightingLayout`] class in the class chain of export
/// `index` (for script subclasses such as `UTGibStaticMeshComponent`, which
/// add no native data of their own), or `None`.
pub fn inherited_lighting_layout(
    pkg: &Package,
    index: usize,
    schema: &dyn Schema,
) -> Option<LightingLayout> {
    let class = pkg.export_class_name(index).ok()?;
    let cpkg = pkg.export_class_package(index).ok()??;
    let chain = schema.class_chain(&format!("{cpkg}.{class}"));
    chain.iter().find_map(|c| {
        LightingLayout::ALL
            .into_iter()
            .find(|l| l.class_name().eq_ignore_ascii_case(c))
    })
}

/// Decode export `index` with an explicit layout (see
/// [`inherited_lighting_layout`]).
pub fn decode_lighting_as(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
    layout: LightingLayout,
) -> ObjResult<DecodedLighting> {
    let object = decode_object(pkg, own_name, index, schema)?;
    let data = pkg.export_data(index)?;
    let native = decode_lighting_native(layout, data, object.properties_end)?;
    Ok(DecodedLighting {
        object,
        layout,
        native,
    })
}

// ---------------------------------------------------------------------------
// ShadowMap2D (tagged properties only)
// ---------------------------------------------------------------------------

/// The tagged properties of a `ShadowMap2D` (the class has no native data).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShadowMap2DInfo {
    /// `ShadowMapTexture2D` path (`None` when absent or null).
    pub texture: Option<String>,
    /// UV scale into the texture.
    pub coordinate_scale: [f32; 2],
    /// UV bias into the texture.
    pub coordinate_bias: [f32; 2],
    /// The light whose static shadowing this is.
    pub light_guid: Option<String>,
    /// `bIsShadowFactorTexture`.
    pub is_shadow_factor_texture: bool,
}

fn vec2_of(v: &Value) -> Option<[f32; 2]> {
    let Value::Struct { fields, .. } = v else {
        return None;
    };
    let get = |n: &str| {
        fields
            .iter()
            .find_map(|f| match (&f.value, f.name.eq_ignore_ascii_case(n)) {
                (Value::Float(x), true) => Some(*x),
                _ => None,
            })
    };
    Some([get("X").unwrap_or(0.0), get("Y").unwrap_or(0.0)])
}

/// GUID text of a tagged `Guid` struct (A, B, C, D as UE3 prints them).
pub fn guid_of(v: &Value) -> Option<String> {
    let Value::Struct { fields, .. } = v else {
        return None;
    };
    let get = |n: &str| {
        fields
            .iter()
            .find_map(|f| match (&f.value, f.name.eq_ignore_ascii_case(n)) {
                (Value::Int(x), true) => Some(u32::from_ne_bytes(x.to_ne_bytes())),
                _ => None,
            })
    };
    let g = Guid {
        a: get("A")?,
        b: get("B")?,
        c: get("C")?,
        d: get("D")?,
    };
    Some(g.to_string())
}

/// Read the `ShadowMap2D` properties from its tags (absent members keep the
/// UE3 zero defaults; `CoordinateScale`/`CoordinateBias` are always tagged in
/// the shipped data).
pub fn shadow_map_2d_info(properties: &[Property]) -> ShadowMap2DInfo {
    let find = |n: &str| properties.iter().find(|p| p.name.eq_ignore_ascii_case(n));
    ShadowMap2DInfo {
        texture: find("Texture").and_then(|p| match &p.value {
            Value::Object(o) if o.index != 0 => Some(o.path.clone()),
            _ => None,
        }),
        coordinate_scale: find("CoordinateScale")
            .and_then(|p| vec2_of(&p.value))
            .unwrap_or([0.0; 2]),
        coordinate_bias: find("CoordinateBias")
            .and_then(|p| vec2_of(&p.value))
            .unwrap_or([0.0; 2]),
        light_guid: find("LightGuid").and_then(|p| guid_of(&p.value)),
        is_shadow_factor_texture: find("bIsShadowFactorTexture")
            .is_some_and(|p| matches!(p.value, Value::Bool(true))),
    }
}

// ---------------------------------------------------------------------------
// Encoder (exact inverse; synthetic fixtures and round-trip checks)
// ---------------------------------------------------------------------------

fn put_count(w: &mut Writer, n: usize) -> Option<()> {
    w.i32(i32::try_from(n).ok()?);
    Some(())
}

fn put_f32s(w: &mut Writer, v: &[f32]) {
    for x in v {
        w.u32(x.to_bits());
    }
}

fn put_guids(w: &mut Writer, g: &[Guid]) -> Option<()> {
    put_count(w, g.len())?;
    for x in g {
        w.guid(*x);
    }
    Some(())
}

fn put_indices(w: &mut Writer, v: &[PackageIndex]) -> Option<()> {
    put_count(w, v.len())?;
    for x in v {
        w.i32(x.0);
    }
    Some(())
}

fn put_u16s(w: &mut Writer, v: &[u16]) -> Option<()> {
    put_count(w, v.len())?;
    for x in v {
        w.u16(*x);
    }
    Some(())
}

/// Encode inline samples. The record's offset field is rewritten to
/// `serial_offset + position of the payload` when `serial_offset` is given,
/// so fixtures carry self-consistent offsets; otherwise the stored offset is
/// kept.
fn put_samples(w: &mut Writer, s: &QuantizedSamples, serial_offset: Option<i64>) -> Option<()> {
    w.u32(s.record.flags);
    w.i32(i32::try_from(s.data.len().checked_div(s.element_size)?).ok()?);
    w.i32(i32::try_from(s.data.len()).ok()?);
    let offset = match serial_offset {
        Some(base) => {
            i32::try_from(base.checked_add(i64::try_from(w.len().checked_add(4)?).ok()?)?).ok()?
        }
        None => s.record.offset_in_file,
    };
    w.i32(offset);
    w.bytes(&s.data);
    Some(())
}

/// Encode a light map reference into `w`. `native_base` is the absolute
/// stream offset at which `w`'s first byte lies (the export's
/// `SerialOffset` plus the payload offset of the native data): bulk sample
/// offsets are then recomputed as absolute stream positions, as the cooker
/// wrote them. `None` keeps the decoded offsets.
pub fn encode_light_map(w: &mut Writer, m: &LightMap, native_base: Option<i64>) -> Option<()> {
    w.u32(m.type_value());
    match m {
        LightMap::None => {}
        LightMap::OneD(m) => {
            put_guids(w, &m.light_guids)?;
            w.i32(m.owner.0);
            put_samples(w, &m.directional_samples, native_base)?;
            for s in &m.scale_vectors {
                put_f32s(w, s);
            }
            put_samples(w, &m.simple_samples, native_base)?;
        }
        LightMap::TwoD(m) => {
            put_guids(w, &m.light_guids)?;
            for i in 0..NUM_STORED_LIGHTMAP_COEF {
                w.i32(m.textures[i].0);
                put_f32s(w, &m.scale_vectors[i]);
            }
            put_f32s(w, &m.coordinate_scale);
            put_f32s(w, &m.coordinate_bias);
        }
    }
    Some(())
}

fn put_lod_info(w: &mut Writer, l: &StaticMeshComponentLodInfo, base: Option<i64>) -> Option<()> {
    put_indices(w, &l.shadow_maps)?;
    put_indices(w, &l.shadow_vertex_buffers)?;
    encode_light_map(w, &l.light_map, base)?;
    match &l.override_vertex_colors {
        None => w.u8(0),
        Some(c) => {
            w.u8(1);
            w.u32(c.stride);
            w.u32(c.num_vertices);
            if c.num_vertices != 0 {
                put_count(w, COLOR_ELEMENT_SIZE)?;
                put_count(w, c.colors.len())?;
                for col in &c.colors {
                    w.bytes(col);
                }
            } else if !c.colors.is_empty() {
                return None;
            }
        }
    }
    put_count(w, l.painted_vertices.len())?;
    for p in &l.painted_vertices {
        put_f32s(w, &p.position);
        w.bytes(&p.normal);
        w.bytes(&p.color);
    }
    Some(())
}

/// Encode `StaticMeshComponent` native data (inverse of
/// [`decode_static_mesh_component_native`]). `native_base` is the export's
/// `SerialOffset` plus the payload offset of the native data (recomputes the
/// bulk offsets), or `None` to keep the decoded ones.
pub fn encode_static_mesh_component_native(
    n: &StaticMeshComponentNative,
    native_base: Option<i64>,
) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    put_count(&mut w, n.lods.len())?;
    for l in &n.lods {
        put_lod_info(&mut w, l, native_base)?;
    }
    if let Some(inst) = &n.instances {
        put_count(&mut w, INSTANCE_DATA_SIZE)?;
        put_count(&mut w, inst.len())?;
        for i in inst {
            put_f32s(&mut w, &i.transform);
            put_f32s(&mut w, &i.lightmap_uv_bias);
            put_f32s(&mut w, &i.shadowmap_uv_bias);
        }
    }
    Some(w.into_bytes())
}

/// Encode `ModelComponent` native data.
pub fn encode_model_component_native(
    n: &ModelComponentNative,
    native_base: Option<i64>,
) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    w.i32(n.model.0);
    w.i32(n.zone_index);
    put_count(&mut w, n.elements.len())?;
    for e in &n.elements {
        encode_light_map(&mut w, &e.light_map, native_base)?;
        w.i32(e.component.0);
        w.i32(e.material.0);
        put_u16s(&mut w, &e.nodes)?;
        put_indices(&mut w, &e.shadow_maps)?;
        put_guids(&mut w, &e.irrelevant_lights)?;
    }
    w.u16(n.component_index);
    put_u16s(&mut w, &n.nodes)?;
    Some(w.into_bytes())
}

/// Encode native data of any layout (see [`encode_static_mesh_component_native`]
/// for `native_base`).
pub fn encode_lighting_native(n: &LightingNative, native_base: Option<i64>) -> Option<Vec<u8>> {
    match n {
        LightingNative::StaticMesh(s) => encode_static_mesh_component_native(s, native_base),
        LightingNative::Model(m) => encode_model_component_native(m, native_base),
        LightingNative::SpeedTree(maps) => {
            let mut w = Writer::new();
            for m in maps {
                encode_light_map(&mut w, m, native_base)?;
            }
            Some(w.into_bytes())
        }
        LightingNative::FluidSurface(m) => {
            let mut w = Writer::new();
            encode_light_map(&mut w, m, native_base)?;
            Some(w.into_bytes())
        }
        LightingNative::ShadowMap1D(s) => {
            let mut w = Writer::new();
            put_count(&mut w, s.samples.len())?;
            put_f32s(&mut w, &s.samples);
            w.guid(s.light_guid);
            Some(w.into_bytes())
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding semantics (decode helpers)
// ---------------------------------------------------------------------------

/// The sRGB transfer function (IEC 61966-2-1) from a stored byte to linear.
///
/// Light map textures are sampled with sRGB decoding: `LightMapTexture2D`
/// has no tagged `SRGB`, so it keeps the `Engine.Texture` default (`true`);
/// see `LIGHTMAPS.md`.
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Approximate non-directional irradiance (UE3 light units, linear RGB) of
/// one directional light map sample.
///
/// `color` is coefficient 0 (`NormalizedAverageColor` texel, R, G, B bytes),
/// `max_components` coefficient 1 (`DirectionalMaxComponent` texel, R, G, B
/// bytes); `scale` the light map's [`LightMap2D::scale_vectors`]. Both are
/// sRGB-decoded and scaled; the result is the colour times the mean of the
/// three scaled max components.
///
/// APPROXIMATION (TENTATIVE): the shipped shaders that combine the two
/// coefficients with the surface normal are not recovered. The mean is what
/// an unperturbed normal receives if the three channels are intensities
/// along three basis directions with equal weight; in the shipped vertex
/// light maps, which keep both forms, the simple coefficient matches this
/// product to within about 30% (`LIGHTMAPS.md`).
pub fn directional_irradiance(
    color: [u8; 3],
    max_components: [u8; 3],
    scale: &[[f32; 3]; NUM_STORED_LIGHTMAP_COEF],
) -> [f32; 3] {
    let mut mean = 0.0;
    for (i, c) in max_components.iter().enumerate() {
        mean += srgb_to_linear(*c) * scale[1][i];
    }
    mean /= 3.0;
    let mut out = [0.0; 3];
    for (i, o) in out.iter_mut().enumerate() {
        *o = srgb_to_linear(color[i]) * scale[0][i] * mean;
    }
    out
}

/// Irradiance (UE3 light units, linear RGB) of vertex `i` of a vertex light
/// map from its simple coefficient (sRGB-decoded like the textures; the
/// vertex path's transfer function is TENTATIVE).
pub fn vertex_irradiance(m: &LightMap1D, i: usize) -> Option<[f32; 3]> {
    let c = m.simple_samples.color(i, 0)?;
    let s = m.scale_vectors[SIMPLE_LIGHTMAP_COEF_INDEX];
    // FColor bytes are B, G, R, A.
    Some([
        srgb_to_linear(c[2]) * s[0],
        srgb_to_linear(c[1]) * s[1],
        srgb_to_linear(c[0]) * s[2],
    ])
}

// ---------------------------------------------------------------------------
// Light map iteration and coverage
// ---------------------------------------------------------------------------

impl LightingNative {
    /// Every light map reference in the data, in serialization order.
    pub fn light_maps(&self) -> Vec<&LightMap> {
        match self {
            LightingNative::StaticMesh(s) => s.lods.iter().map(|l| &l.light_map).collect(),
            LightingNative::Model(m) => m.elements.iter().map(|e| &e.light_map).collect(),
            LightingNative::SpeedTree(v) => v.iter().collect(),
            LightingNative::FluidSurface(m) => vec![m],
            LightingNative::ShadowMap1D(_) => Vec::new(),
        }
    }

    /// Every `ShadowMap2D` reference.
    pub fn shadow_maps_2d(&self) -> Vec<PackageIndex> {
        match self {
            LightingNative::StaticMesh(s) => s
                .lods
                .iter()
                .flat_map(|l| l.shadow_maps.iter().copied())
                .collect(),
            LightingNative::Model(m) => m
                .elements
                .iter()
                .flat_map(|e| e.shadow_maps.iter().copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Every `ShadowMap1D` reference.
    pub fn shadow_maps_1d(&self) -> Vec<PackageIndex> {
        match self {
            LightingNative::StaticMesh(s) => s
                .lods
                .iter()
                .flat_map(|l| l.shadow_vertex_buffers.iter().copied())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Most failure samples kept per package.
const MAX_FAILURE_SAMPLES: usize = 16;

/// Per-layout counts of a coverage pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayoutCoverage {
    /// Exports of the class.
    pub total: usize,
    /// Exports whose native data decoded and ended exactly at `SerialSize`.
    pub exact: usize,
    /// Exports whose native data re-encodes to exactly the original bytes
    /// (bulk offsets recomputed from the export's position).
    pub round_trip: usize,
    /// Exports that passed every structural cross-check.
    pub valid: usize,
    /// Native bytes consumed (exact exports).
    pub native_bytes: u64,
}

/// Lighting coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LightingCoverage {
    /// Package name.
    pub package: String,
    /// Counts per layout.
    pub layouts: BTreeMap<LightingLayout, LayoutCoverage>,
    /// Light map references by type (`none`, `1d`, `2d`).
    pub light_maps: BTreeMap<&'static str, usize>,
    /// Static mesh component LOD count -> components.
    pub lod_histogram: BTreeMap<usize, usize>,
    /// `ShadowMap2D` references.
    pub shadow_map_2d_refs: usize,
    /// `ShadowMap1D` references.
    pub shadow_map_1d_refs: usize,
    /// LODs with override vertex colors.
    pub override_color_lods: usize,
    /// Painted vertices.
    pub painted_vertices: usize,
    /// Instances of instanced static mesh components.
    pub instances: usize,
    /// Baked light GUID references (all light maps).
    pub light_guid_refs: usize,
    /// Model elements.
    pub model_elements: usize,
    /// 2D light maps whose two directional coefficient textures (0 and 1)
    /// are distinct `LightMapTexture2D` exports of this package.
    pub light_map_2d_textures_ok: usize,
    /// 2D light maps whose simple coefficient texture (2) is not null.
    pub light_map_2d_simple_textures: usize,
    /// 2D light maps whose UV rectangle lies inside `[0, 1]`.
    pub light_map_2d_rect_ok: usize,
    /// Class default objects of lighting classes (no native data; skipped).
    pub default_objects: usize,
    /// Classes outside the layouts above whose chain contains a layout
    /// class, decoded with that layout: class -> (exports, exact).
    pub subclasses: BTreeMap<String, (usize, usize)>,
    /// First decode failures (`export: error`).
    pub failures: Vec<String>,
    /// First validation issues (`export: issue`).
    pub issues: Vec<String>,
}

fn is_class(pkg: &Package, idx: PackageIndex, class: &str) -> bool {
    match idx.export_index() {
        Some(i) => pkg.export_class_name(i).is_ok_and(|c| c == class),
        None => false,
    }
}

/// Structural cross-checks of one decoded export (empty = valid).
pub fn validate_lighting(pkg: &Package, native: &LightingNative) -> Vec<String> {
    let mut issues = Vec::new();
    let in_range = |idx: PackageIndex| match idx.export_index() {
        Some(i) => i < pkg.exports.len(),
        None => idx.import_index().is_none_or(|i| i < pkg.imports.len()),
    };
    for m in native.light_maps() {
        match m {
            LightMap::None => {}
            LightMap::OneD(m) => {
                if m.directional_samples.len() != m.simple_samples.len() {
                    issues.push(format!(
                        "1D light map: {} directional vs {} simple samples",
                        m.directional_samples.len(),
                        m.simple_samples.len()
                    ));
                }
                if !in_range(m.owner) {
                    issues.push("1D light map owner out of range".to_owned());
                }
            }
            LightMap::TwoD(m) => {
                for (i, t) in m.textures.iter().enumerate() {
                    if !in_range(*t) {
                        issues.push(format!("2D light map texture {i} out of range"));
                    }
                }
            }
        }
    }
    for s in native.shadow_maps_2d() {
        if !is_class(pkg, s, "ShadowMap2D") {
            issues.push(format!("shadow map {} is not a ShadowMap2D export", s.0));
        }
    }
    for s in native.shadow_maps_1d() {
        if !is_class(pkg, s, "ShadowMap1D") {
            issues.push(format!(
                "shadow vertex buffer {} is not a ShadowMap1D export",
                s.0
            ));
        }
    }
    if let LightingNative::StaticMesh(s) = native {
        for l in &s.lods {
            if let Some(c) = &l.override_vertex_colors
                && usize::try_from(c.num_vertices).ok() != Some(c.colors.len())
            {
                issues.push("override colors: NumVertices disagrees with the array".to_owned());
            }
        }
    }
    if let LightingNative::Model(m) = native {
        if !in_range(m.model) {
            issues.push("model out of range".to_owned());
        }
        for e in &m.elements {
            if !in_range(e.component) || !in_range(e.material) {
                issues.push("model element reference out of range".to_owned());
            }
        }
    }
    issues
}

fn rect_inside_unit(m: &LightMap2D) -> bool {
    let [a, b, c, d] = m.uv_rect();
    let ok = |v: f32| v.is_finite() && (-1e-6..=1.0 + 1e-6).contains(&v);
    ok(a) && ok(b) && ok(c) && ok(d) && a <= c && b <= d
}

/// Decode every lighting export of `lp` and gather statistics.
pub fn lighting_coverage(lp: &LoadedPackage, schema: &dyn Schema) -> LightingCoverage {
    let pkg = &lp.package;
    let mut cov = LightingCoverage {
        package: lp.name.clone(),
        ..LightingCoverage::default()
    };
    for i in 0..pkg.exports.len() {
        // Class default objects carry no native data.
        if pkg
            .export(i)
            .is_ok_and(|e| e.object_flags & crate::flags::object::CLASS_DEFAULT_OBJECT != 0)
        {
            if inherited_lighting_layout(pkg, i, schema).is_some() {
                cov.default_objects += 1;
            }
            continue;
        }
        let Some(layout) = lighting_layout(pkg, i) else {
            // Script subclasses of a layout class: decoded with the inherited
            // layout and reported separately.
            if let Some(layout) = inherited_lighting_layout(pkg, i, schema) {
                let class = pkg.export_class_name(i).unwrap_or_default();
                let ok = decode_lighting_as(pkg, Some(&lp.name), i, schema, layout).is_ok();
                let e = cov.subclasses.entry(class).or_insert((0, 0));
                e.0 += 1;
                e.1 += usize::from(ok);
            }
            continue;
        };
        let lc = cov.layouts.entry(layout).or_default();
        lc.total += 1;
        let decoded = match decode_lighting(pkg, Some(&lp.name), i, schema) {
            Ok(d) => d,
            Err(e) => {
                if cov.failures.len() < MAX_FAILURE_SAMPLES {
                    cov.failures.push(format!("{i}: {e}"));
                }
                continue;
            }
        };
        lc.exact += 1;
        lc.native_bytes += u64::try_from(decoded.object.native_tail()).unwrap_or(0);
        let start = decoded.object.properties_end;
        let base = pkg
            .export(i)
            .ok()
            .and_then(|e| i64::from(e.serial_offset).checked_add(i64::try_from(start).ok()?));
        let original = pkg.export_data(i).ok().and_then(|d| d.get(start..));
        if original.is_some()
            && encode_lighting_native(&decoded.native, base).as_deref() == original
        {
            lc.round_trip += 1;
        }
        let issues = validate_lighting(pkg, &decoded.native);
        if issues.is_empty() {
            lc.valid += 1;
        } else {
            for m in issues {
                if cov.issues.len() < MAX_FAILURE_SAMPLES {
                    cov.issues.push(format!("{i}: {m}"));
                }
            }
        }
        for m in decoded.native.light_maps() {
            let key = match m {
                LightMap::None => "none",
                LightMap::OneD(_) => "1d",
                LightMap::TwoD(t) => {
                    let [t0, t1, t2] = t.textures;
                    if t0 != t1
                        && is_class(pkg, t0, "LightMapTexture2D")
                        && is_class(pkg, t1, "LightMapTexture2D")
                    {
                        cov.light_map_2d_textures_ok += 1;
                    }
                    if !t2.is_null() {
                        cov.light_map_2d_simple_textures += 1;
                    }
                    if rect_inside_unit(t) {
                        cov.light_map_2d_rect_ok += 1;
                    }
                    "2d"
                }
            };
            *cov.light_maps.entry(key).or_insert(0) += 1;
            cov.light_guid_refs += m.light_guids().len();
        }
        cov.shadow_map_2d_refs += decoded.native.shadow_maps_2d().len();
        cov.shadow_map_1d_refs += decoded.native.shadow_maps_1d().len();
        match &decoded.native {
            LightingNative::StaticMesh(s) => {
                *cov.lod_histogram.entry(s.lods.len()).or_insert(0) += 1;
                for l in &s.lods {
                    if l.override_vertex_colors.is_some() {
                        cov.override_color_lods += 1;
                    }
                    cov.painted_vertices += l.painted_vertices.len();
                }
                cov.instances += s.instances.as_ref().map_or(0, Vec::len);
            }
            LightingNative::Model(m) => cov.model_elements += m.elements.len(),
            _ => {}
        }
    }
    cov
}
