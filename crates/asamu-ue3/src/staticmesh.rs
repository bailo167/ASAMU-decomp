//! `StaticMesh` native payload decoding for UE3 v868 / licensee 0.
//!
//! A `StaticMesh` export is the usual object prelude and tagged properties
//! (see [`crate::object`]) followed by native data written by
//! `UStaticMesh::Serialize`. The layout below is the one under which **every**
//! `StaticMesh` export of every shipped package is consumed exactly to
//! `SerialSize` (see `docs/reverse-engineering/MESHES.md` for the evidence,
//! coverage and confidence of each field):
//!
//! ```text
//! FBoxSphereBounds   Origin(FVector) BoxExtent(FVector) f32 SphereRadius
//! obj                BodySetup (RB_BodySetup)
//! kDOP tree          TkDOP RootBound (f32 Min[3], f32 Max[3])
//!                    bulk TArray<compact node, 6 bytes>
//!                    bulk TArray<collision triangle, 8 bytes>
//! i32                InternalVersion
//! u32                bHasSourceData (+ one LOD model when non-zero)
//! TArray             optimisation settings (24 bytes each)
//! u32                bHasBeenSimplified
//! u32                bIsMeshProxy
//! TArray             LOD models (see LodModel)
//! i32                LOD info count (elements carry no data when loading)
//! FRotator           ThumbnailAngle
//! f32                ThumbnailDistance
//! FString            HighResSourceMeshName
//! u32                HighResSourceMeshCRC
//! FGuid              LightingGuid
//! i32                VertexPositionVersionNumber
//! TArray<f32>        CachedStreamingTextureFactors
//! u32                bRemoveDegenerates
//! u32                bPerLODStaticLightingForInstancing
//! i32                ConsolePreallocInstanceCount
//! ```
//!
//! A *bulk* array (`TArray::BulkSerialize`) is `i32 ElementSize`, `i32 Count`
//! and `Count * ElementSize` raw bytes. Every bulk array here has a fixed
//! element size; a different serialized size is rejected.
//!
//! One LOD model (`FStaticMeshRenderData::Serialize`):
//!
//! ```text
//! bulk data record   RawTriangles (read with [`crate::bulkdata::read_bulk_record`]):
//!                    u32 Flags, i32 Count, i32 SizeOnDisk, i32 OffsetInFile
//!                    (+ SizeOnDisk inline bytes unless Flags & 1; empty in every
//!                    cooked mesh)
//! TArray             sections (FStaticMeshElement)
//! position buffer    u32 Stride, u32 NumVertices, bulk TArray<FVector>
//! vertex buffer      u32 NumTexCoords, u32 Stride, u32 NumVertices,
//!                    u32 bUseFullPrecisionUVs, bulk TArray<vertex>
//!                    vertex = FPackedNormal TangentX, FPackedNormal TangentZ,
//!                    NumTexCoords x (half2 | float2)
//! color buffer       u32 Stride, u32 NumVertices, bulk TArray<FColor> only when
//!                    NumVertices != 0
//! u32                NumVertices
//! index buffer       bulk TArray<u16>
//! wireframe buffer   bulk TArray<u16>
//! adjacency buffer   bulk TArray<u16>
//! ```
//!
//! One section (`FStaticMeshElement`): `obj Material`, `u32 EnableCollision`,
//! `u32 OldEnableCollision`, `u32 bEnableShadowCasting`, `u32 FirstIndex`,
//! `u32 NumTriangles`, `u32 MinVertexIndex`, `u32 MaxVertexIndex`,
//! `i32 MaterialIndex`, `TArray<{i32, i32}> Fragments`, `u8 bHasPlatformData`
//! (must be 0: the platform arrays are never exercised by the shipped data).
//!
//! Besides the decoder ([`decode_static_mesh`], [`decode_static_mesh_native`])
//! the module offers structural cross-checks ([`validate_static_mesh`]), a
//! per-package coverage pass ([`mesh_coverage`]) and the exact inverse
//! encoder ([`encode_static_mesh_native`]), which reproduces every shipped
//! native tail byte for byte and builds synthetic fixtures for tests.
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
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::{Guid, PackageIndex};
use crate::writer::Writer;

/// Element size of `FPositionVertex` (one `FVector`).
pub const POSITION_ELEMENT_SIZE: usize = 12;
/// Element size of a compact kDOP node.
pub const KDOP_NODE_ELEMENT_SIZE: usize = 6;
/// Element size of a kDOP collision triangle.
pub const KDOP_TRIANGLE_ELEMENT_SIZE: usize = 8;
/// Element size of an `FColor`.
pub const COLOR_ELEMENT_SIZE: usize = 4;
/// Element size of a 16-bit index.
pub const INDEX_ELEMENT_SIZE: usize = 2;
/// In-memory element size of one `FStaticMeshTriangle` in the raw-triangle
/// bulk data (`FStaticMeshTriangleBulkData::GetElementSize`).
pub const RAW_TRIANGLE_ELEMENT_SIZE: usize = 0x174;
/// Largest `NumTexCoords` the vertex buffer accepts (the engine selects one of
/// four vertex types for 1..=4 channels).
pub const MAX_TEX_COORDS: u32 = 4;
/// Upper bound on LOD models accepted (sanity limit, not a format value).
pub const MAX_LODS: usize = 64;
/// Size of one optimisation-settings record on disk (v >= 863).
pub const OPTIMIZATION_SETTINGS_SIZE: usize = 24;
/// Smallest serialized `FStaticMeshElement` (no fragments).
pub const ELEMENT_MIN_SIZE: usize = 4 * 9 + 4 + 1;

/// `FBoxSphereBounds`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BoxSphereBounds {
    /// Box (and sphere) centre.
    pub origin: [f32; 3],
    /// Half-size of the box along each axis.
    pub box_extent: [f32; 3],
    /// Sphere radius.
    pub sphere_radius: f32,
}

/// Axis-aligned float bounds of the kDOP tree root (`TkDOP`: `Min[3]`, `Max[3]`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct KdopBounds {
    /// Minimum corner.
    pub min: [f32; 3],
    /// Maximum corner.
    pub max: [f32; 3],
}

/// One compact kDOP node: six opaque bytes. They quantise child bounding
/// volumes relative to the parent's (the engine's traversal compares each byte
/// against 127.5); the exact decoding is not reproduced (TENTATIVE). The
/// collision triangles are fully decoded, so a consumer can rebuild its own
/// bounding-volume hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompactKdopNode {
    /// The six stored bytes.
    pub bytes: [u8; 6],
}

/// One kDOP collision triangle (`FkDOPCollisionTriangle<WORD>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CollisionTriangle {
    /// Vertex indices into LOD 0's position buffer.
    pub vertices: [u16; 3],
    /// Section (material) index.
    pub material_index: u16,
}

/// The collision kDOP tree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KdopTree {
    /// Root bounds.
    pub root_bounds: KdopBounds,
    /// Compact nodes.
    pub nodes: Vec<CompactKdopNode>,
    /// Collision triangles.
    pub triangles: Vec<CollisionTriangle>,
}

/// `FFragmentRange` (index range of one fracture fragment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FragmentRange {
    /// First index.
    pub base_index: i32,
    /// Number of primitives.
    pub num_primitives: i32,
}

/// One section of a LOD model (`FStaticMeshElement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MeshSection {
    /// Material (package index; null when unassigned).
    pub material: PackageIndex,
    /// `EnableCollision`.
    pub enable_collision: u32,
    /// `OldEnableCollision`.
    pub old_enable_collision: u32,
    /// `bEnableShadowCasting`.
    pub enable_shadow_casting: u32,
    /// First index into the index buffer.
    pub first_index: u32,
    /// Number of triangles.
    pub num_triangles: u32,
    /// Smallest vertex index referenced.
    pub min_vertex_index: u32,
    /// Largest vertex index referenced.
    pub max_vertex_index: u32,
    /// Material slot index.
    pub material_index: i32,
    /// Fracture fragment ranges.
    pub fragments: Vec<FragmentRange>,
}

/// An `FPackedNormal`: four bytes X, Y, Z, W, each mapping `b / 127.5 - 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PackedNormal(pub [u8; 4]);

impl PackedNormal {
    /// Unpacked `(x, y, z, w)` in `[-1, 1]`.
    pub fn unpack(self) -> [f32; 4] {
        self.0.map(|b| f32::from(b) / 127.5 - 1.0)
    }
}

/// Position vertex buffer (`FPositionVertexBuffer`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionBuffer {
    /// Serialized stride.
    pub stride: u32,
    /// Serialized vertex count.
    pub num_vertices: u32,
    /// Positions (UE3 units, UE3 axes).
    pub positions: Vec<[f32; 3]>,
}

/// Tangent-basis and texture-coordinate buffer (`FStaticMeshVertexBuffer`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VertexBuffer {
    /// Number of UV channels (1..=4).
    pub num_tex_coords: u32,
    /// Serialized stride.
    pub stride: u32,
    /// Serialized vertex count.
    pub num_vertices: u32,
    /// UVs stored as 32-bit floats (otherwise 16-bit halves).
    pub full_precision_uvs: bool,
    /// `TangentX` per vertex.
    pub tangent_x: Vec<PackedNormal>,
    /// `TangentZ` (normal; W = bitangent sign) per vertex.
    pub tangent_z: Vec<PackedNormal>,
    /// UVs per channel, then per vertex (`uvs[channel][vertex]`).
    pub uvs: Vec<Vec<[f32; 2]>>,
}

/// Vertex color buffer (`FColorVertexBuffer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColorBuffer {
    /// Serialized stride.
    pub stride: u32,
    /// Serialized vertex count (0 = no colors).
    pub num_vertices: u32,
    /// Colors in stored byte order `B, G, R, A`.
    pub colors_bgra: Vec<[u8; 4]>,
}

/// One LOD model (`FStaticMeshRenderData`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LodModel {
    /// Raw source triangles (`FStaticMeshTriangleBulkData`; editor data,
    /// header only: the payload is skipped, never decoded).
    pub raw_triangles: BulkDataRecord,
    /// Sections.
    pub sections: Vec<MeshSection>,
    /// Positions.
    pub positions: PositionBuffer,
    /// Tangents and UVs.
    pub vertices: VertexBuffer,
    /// Vertex colors.
    pub colors: ColorBuffer,
    /// `NumVertices`.
    pub num_vertices: u32,
    /// Triangle-list index buffer.
    pub indices: Vec<u16>,
    /// Wireframe (line-list) index buffer.
    pub wireframe_indices: Vec<u16>,
    /// Adjacency index buffer (12 indices per triangle when present).
    pub adjacency_indices: Vec<u16>,
}

impl LodModel {
    /// Total triangles over all sections.
    pub fn triangle_count(&self) -> u64 {
        self.sections
            .iter()
            .map(|s| u64::from(s.num_triangles))
            .sum()
    }
}

/// One `FStaticMeshOptimizationSettings` record (v >= 863). Field names follow
/// UE3 conventions (TENTATIVE); the byte layout is CONFIRMED.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OptimizationSettings {
    /// `ReductionMethod` (byte).
    pub reduction_method: u8,
    /// `NumOfTrianglesPercentage`.
    pub num_triangles_percentage: f32,
    /// `MaxDeviationPercentage`.
    pub max_deviation_percentage: f32,
    /// `SilhouetteImportance` (byte).
    pub silhouette_importance: u8,
    /// `TextureImportance` (byte).
    pub texture_importance: u8,
    /// `ShadingImportance` (byte).
    pub shading_importance: u8,
    /// `bRecalcNormals`.
    pub recalc_normals: u32,
    /// `NormalsThreshold`.
    pub normals_threshold: f32,
    /// `WeldingThreshold`.
    pub welding_threshold: f32,
}

/// Everything `UStaticMesh::Serialize` writes after the tagged properties.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticMeshNative {
    /// Payload offset where the native data starts (end of tagged properties).
    pub start: usize,
    /// Bounds.
    pub bounds: BoxSphereBounds,
    /// `BodySetup` (package index of an `RB_BodySetup`, or null).
    pub body_setup: PackageIndex,
    /// Collision kDOP tree.
    pub kdop: KdopTree,
    /// `InternalVersion`.
    pub internal_version: i32,
    /// Source (pre-optimisation) render data, when stored.
    pub source_data: Option<Box<LodModel>>,
    /// Optimisation settings.
    pub optimization_settings: Vec<OptimizationSettings>,
    /// `bHasBeenSimplified` (name TENTATIVE).
    pub has_been_simplified: u32,
    /// `bIsMeshProxy` (name TENTATIVE).
    pub is_mesh_proxy: u32,
    /// LOD models, finest first.
    pub lods: Vec<LodModel>,
    /// Element count of the LOD info array (its elements carry no data on load).
    pub lod_info_count: u32,
    /// `ThumbnailAngle` (pitch, yaw, roll in UE3 rotator units).
    pub thumbnail_angle: [i32; 3],
    /// `ThumbnailDistance`.
    pub thumbnail_distance: f32,
    /// `HighResSourceMeshName`.
    pub high_res_source_mesh_name: String,
    /// `HighResSourceMeshCRC`.
    pub high_res_source_mesh_crc: u32,
    /// `LightingGuid`.
    pub lighting_guid: Guid,
    /// `VertexPositionVersionNumber`.
    pub vertex_position_version: i32,
    /// `CachedStreamingTextureFactors`.
    pub cached_streaming_texture_factors: Vec<f32>,
    /// `bRemoveDegenerates` (name TENTATIVE).
    pub remove_degenerates: u32,
    /// `bPerLODStaticLightingForInstancing` (name TENTATIVE).
    pub per_lod_static_lighting_for_instancing: u32,
    /// `ConsolePreallocInstanceCount` (name TENTATIVE).
    pub console_prealloc_instance_count: i32,
}

/// A decoded `StaticMesh` export: generic object plus native data.
#[derive(Debug, Clone, Serialize)]
pub struct StaticMesh {
    /// Prelude and tagged properties.
    pub object: DecodedObject,
    /// Native data.
    pub native: StaticMeshNative,
}

/// Native default of `UseSimpleLineCollision` (`UStaticMesh`'s intrinsic
/// property initialiser; see MESHES.md, "Simple collision").
pub const DEFAULT_USE_SIMPLE_LINE_COLLISION: bool = true;
/// Native default of `UseSimpleBoxCollision`.
pub const DEFAULT_USE_SIMPLE_BOX_COLLISION: bool = true;
/// Native default of `UseSimpleRigidBodyCollision`.
pub const DEFAULT_USE_SIMPLE_RIGID_BODY_COLLISION: bool = true;

/// The three simple-collision switches of a `StaticMesh`.
///
/// `StaticMesh` is an intrinsic class: its properties are registered and
/// initialised by native code, there is no script class and no class default
/// object in any package, and a tag is written only when the value differs
/// from the native default. All three default to **true** (the constants
/// above); the shipped packages only ever store `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SimpleCollisionFlags {
    /// `UseSimpleLineCollision`: zero-extent (line) traces that are not
    /// forced to the triangles use the simple collision shapes.
    pub line: bool,
    /// `UseSimpleBoxCollision`: non-zero-extent (swept box) traces use the
    /// simple collision shapes.
    pub box_: bool,
    /// `UseSimpleRigidBodyCollision`: the physics body is built from the
    /// simple collision shapes.
    pub rigid_body: bool,
}

impl Default for SimpleCollisionFlags {
    /// The native defaults.
    fn default() -> Self {
        SimpleCollisionFlags {
            line: DEFAULT_USE_SIMPLE_LINE_COLLISION,
            box_: DEFAULT_USE_SIMPLE_BOX_COLLISION,
            rigid_body: DEFAULT_USE_SIMPLE_RIGID_BODY_COLLISION,
        }
    }
}

impl StaticMesh {
    /// `UseSimpleLineCollision` as stored (`None`: not tagged, the native
    /// default applies).
    pub fn stored_use_simple_line_collision(&self) -> Option<bool> {
        self.bool_property("UseSimpleLineCollision")
    }

    /// `UseSimpleBoxCollision` as stored (`None`: not tagged).
    pub fn stored_use_simple_box_collision(&self) -> Option<bool> {
        self.bool_property("UseSimpleBoxCollision")
    }

    /// `UseSimpleRigidBodyCollision` as stored (`None`: not tagged).
    pub fn stored_use_simple_rigid_body_collision(&self) -> Option<bool> {
        self.bool_property("UseSimpleRigidBodyCollision")
    }

    /// The effective simple-collision switches: the stored value, else the
    /// native default.
    pub fn simple_collision_flags(&self) -> SimpleCollisionFlags {
        let d = SimpleCollisionFlags::default();
        SimpleCollisionFlags {
            line: self.stored_use_simple_line_collision().unwrap_or(d.line),
            box_: self.stored_use_simple_box_collision().unwrap_or(d.box_),
            rigid_body: self
                .stored_use_simple_rigid_body_collision()
                .unwrap_or(d.rigid_body),
        }
    }

    /// The tagged `BodySetup` object property (package index), when present.
    /// The same reference is stored again in the native data
    /// ([`StaticMeshNative::body_setup`]).
    pub fn tagged_body_setup(&self) -> Option<PackageIndex> {
        self.object.properties.iter().find_map(|p| {
            if p.name.eq_ignore_ascii_case("BodySetup")
                && let crate::Value::Object(o) = &p.value
            {
                Some(PackageIndex(o.index))
            } else {
                None
            }
        })
    }

    fn bool_property(&self, name: &str) -> Option<bool> {
        self.object.properties.iter().find_map(|p| {
            if p.name.eq_ignore_ascii_case(name)
                && let crate::Value::Bool(v) = p.value
            {
                Some(v)
            } else {
                None
            }
        })
    }

    /// `LightMapCoordinateIndex` tagged property, when present.
    pub fn light_map_coordinate_index(&self) -> Option<i32> {
        self.int_property("LightMapCoordinateIndex")
    }

    /// `LightMapResolution` tagged property, when present.
    pub fn light_map_resolution(&self) -> Option<i32> {
        self.int_property("LightMapResolution")
    }

    fn int_property(&self, name: &str) -> Option<i32> {
        self.object.properties.iter().find_map(|p| {
            if p.name.eq_ignore_ascii_case(name)
                && let crate::Value::Int(v) = p.value
            {
                Some(v)
            } else {
                None
            }
        })
    }
}

/// Decode a 16-bit IEEE half float exactly into an `f32`.
pub fn half_to_f32(h: u16) -> f32 {
    let negative = h & 0x8000 != 0;
    let exp = u32::from((h >> 10) & 0x1f);
    let mant = u32::from(h & 0x3ff);
    let magnitude = if exp == 0 {
        // Zero or subnormal: mant * 2^-24 (exact in f32).
        f32::from(h & 0x3ff) * (1.0 / 16_777_216.0)
    } else if exp == 31 {
        f32::from_bits(0x7f80_0000 | (mant << 13))
    } else {
        f32::from_bits(((exp + 112) << 23) | (mant << 13))
    };
    if negative { -magnitude } else { magnitude }
}

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

/// `TArray<T>` read with an object-level element decoder: `i32 count` (checked
/// against `count * min_element_size` remaining bytes) then the elements.
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

fn read_vec3(r: &mut Reader<'_>) -> ObjResult<[f32; 3]> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

fn read_u32_bool(r: &mut Reader<'_>, what: &'static str) -> ObjResult<bool> {
    let at = r.position();
    match r.read_u32()? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(malformed(what, at, format!("boolean is {v}, not 0 or 1"))),
    }
}

/// Read a bulk array header (`i32 ElementSize`, `i32 Count`), require the
/// element size to be `expected`, and return the count after checking that
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

fn read_bulk_u16(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<u16>> {
    let n = read_bulk_count(r, what, INDEX_ELEMENT_SIZE)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(r.read_u16()?);
    }
    Ok(out)
}

fn read_kdop(r: &mut Reader<'_>) -> ObjResult<KdopTree> {
    let root_bounds = KdopBounds {
        min: read_vec3(r)?,
        max: read_vec3(r)?,
    };
    let n = read_bulk_count(r, "kDOP nodes", KDOP_NODE_ELEMENT_SIZE)?;
    let mut nodes = Vec::with_capacity(n);
    for _ in 0..n {
        let b = r.read_bytes(KDOP_NODE_ELEMENT_SIZE)?;
        let mut bytes = [0u8; KDOP_NODE_ELEMENT_SIZE];
        bytes.copy_from_slice(b);
        nodes.push(CompactKdopNode { bytes });
    }
    let n = read_bulk_count(r, "kDOP triangles", KDOP_TRIANGLE_ELEMENT_SIZE)?;
    let mut triangles = Vec::with_capacity(n);
    for _ in 0..n {
        triangles.push(CollisionTriangle {
            vertices: [r.read_u16()?, r.read_u16()?, r.read_u16()?],
            material_index: r.read_u16()?,
        });
    }
    Ok(KdopTree {
        root_bounds,
        nodes,
        triangles,
    })
}

fn read_raw_triangles(r: &mut Reader<'_>) -> ObjResult<BulkDataRecord> {
    let at = r.position();
    read_bulk_record(r).map_err(|e| malformed("RawTriangles bulk data", at, e.to_string()))
}

fn read_section(r: &mut Reader<'_>) -> ObjResult<MeshSection> {
    let material = r.read_package_index()?;
    let enable_collision = r.read_u32()?;
    let old_enable_collision = r.read_u32()?;
    let enable_shadow_casting = r.read_u32()?;
    let first_index = r.read_u32()?;
    let num_triangles = r.read_u32()?;
    let min_vertex_index = r.read_u32()?;
    let max_vertex_index = r.read_u32()?;
    let material_index = r.read_i32()?;
    let fragments = r.read_tarray("FStaticMeshElement.Fragments", 8, |r| {
        Ok(FragmentRange {
            base_index: r.read_i32()?,
            num_primitives: r.read_i32()?,
        })
    })?;
    let at = r.position();
    let platform = r.read_u8()?;
    if platform != 0 {
        // Eight platform arrays would follow; their element layouts are never
        // exercised by the shipped data, so refuse rather than guess.
        return Err(malformed(
            "FStaticMeshElement platform data",
            at,
            format!("flag {platform}: platform data present (layout unverified)"),
        ));
    }
    Ok(MeshSection {
        material,
        enable_collision,
        old_enable_collision,
        enable_shadow_casting,
        first_index,
        num_triangles,
        min_vertex_index,
        max_vertex_index,
        material_index,
        fragments,
    })
}

fn read_position_buffer(r: &mut Reader<'_>) -> ObjResult<PositionBuffer> {
    let stride = r.read_u32()?;
    let num_vertices = r.read_u32()?;
    let n = read_bulk_count(r, "position vertices", POSITION_ELEMENT_SIZE)?;
    let mut positions = Vec::with_capacity(n);
    for _ in 0..n {
        positions.push(read_vec3(r)?);
    }
    Ok(PositionBuffer {
        stride,
        num_vertices,
        positions,
    })
}

/// Serialized size of one tangent/UV vertex.
pub fn vertex_element_size(num_tex_coords: u32, full_precision_uvs: bool) -> Option<usize> {
    if !(1..=MAX_TEX_COORDS).contains(&num_tex_coords) {
        return None;
    }
    let per_uv = if full_precision_uvs { 8 } else { 4 };
    let n = usize::try_from(num_tex_coords).ok()?;
    n.checked_mul(per_uv)?.checked_add(8)
}

fn read_vertex_buffer(r: &mut Reader<'_>) -> ObjResult<VertexBuffer> {
    let at = r.position();
    let num_tex_coords = r.read_u32()?;
    let stride = r.read_u32()?;
    let num_vertices = r.read_u32()?;
    let full_precision_uvs = read_u32_bool(r, "bUseFullPrecisionUVs")?;
    let elem = vertex_element_size(num_tex_coords, full_precision_uvs).ok_or_else(|| {
        malformed(
            "NumTexCoords",
            at,
            format!("{num_tex_coords} UV channels (expected 1..={MAX_TEX_COORDS})"),
        )
    })?;
    let n = read_bulk_count(r, "tangent/UV vertices", elem)?;
    let channels = usize::try_from(num_tex_coords).unwrap_or(0);
    let mut tangent_x = Vec::with_capacity(n);
    let mut tangent_z = Vec::with_capacity(n);
    let mut uvs: Vec<Vec<[f32; 2]>> = (0..channels).map(|_| Vec::with_capacity(n)).collect();
    for _ in 0..n {
        tangent_x.push(PackedNormal(read4(r)?));
        tangent_z.push(PackedNormal(read4(r)?));
        for ch in uvs.iter_mut() {
            let uv = if full_precision_uvs {
                [r.read_f32()?, r.read_f32()?]
            } else {
                [half_to_f32(r.read_u16()?), half_to_f32(r.read_u16()?)]
            };
            ch.push(uv);
        }
    }
    Ok(VertexBuffer {
        num_tex_coords,
        stride,
        num_vertices,
        full_precision_uvs,
        tangent_x,
        tangent_z,
        uvs,
    })
}

fn read4(r: &mut Reader<'_>) -> ObjResult<[u8; 4]> {
    let b = r.read_bytes(4)?;
    let mut out = [0u8; 4];
    out.copy_from_slice(b);
    Ok(out)
}

fn read_color_buffer(r: &mut Reader<'_>) -> ObjResult<ColorBuffer> {
    let stride = r.read_u32()?;
    let num_vertices = r.read_u32()?;
    let mut colors_bgra = Vec::new();
    if num_vertices != 0 {
        let n = read_bulk_count(r, "vertex colors", COLOR_ELEMENT_SIZE)?;
        colors_bgra.reserve_exact(n);
        for _ in 0..n {
            colors_bgra.push(read4(r)?);
        }
    }
    Ok(ColorBuffer {
        stride,
        num_vertices,
        colors_bgra,
    })
}

/// Decode one `FStaticMeshRenderData` at the reader's position.
pub fn read_lod_model(r: &mut Reader<'_>) -> ObjResult<LodModel> {
    let raw_triangles = read_raw_triangles(r)?;
    let sections = read_array(r, "LOD sections", ELEMENT_MIN_SIZE, read_section)?;
    let positions = read_position_buffer(r)?;
    let vertices = read_vertex_buffer(r)?;
    let colors = read_color_buffer(r)?;
    let num_vertices = r.read_u32()?;
    let indices = read_bulk_u16(r, "index buffer")?;
    let wireframe_indices = read_bulk_u16(r, "wireframe index buffer")?;
    let adjacency_indices = read_bulk_u16(r, "adjacency index buffer")?;
    Ok(LodModel {
        raw_triangles,
        sections,
        positions,
        vertices,
        colors,
        num_vertices,
        indices,
        wireframe_indices,
        adjacency_indices,
    })
}

fn read_optimization_settings(r: &mut Reader<'_>) -> ObjResult<OptimizationSettings> {
    Ok(OptimizationSettings {
        reduction_method: r.read_u8()?,
        num_triangles_percentage: r.read_f32()?,
        max_deviation_percentage: r.read_f32()?,
        silhouette_importance: r.read_u8()?,
        texture_importance: r.read_u8()?,
        shading_importance: r.read_u8()?,
        recalc_normals: r.read_u32()?,
        normals_threshold: r.read_f32()?,
        welding_threshold: r.read_f32()?,
    })
}

/// Decode the native data of a `StaticMesh` payload starting at `start`
/// (the end of the tagged properties). The decoder must end exactly at the
/// end of `data`; otherwise [`ObjectError::Malformed`] is returned.
pub fn decode_static_mesh_native(data: &[u8], start: usize) -> ObjResult<StaticMeshNative> {
    let mut r = Reader::at(data, start)?;
    let bounds = BoxSphereBounds {
        origin: read_vec3(&mut r)?,
        box_extent: read_vec3(&mut r)?,
        sphere_radius: r.read_f32()?,
    };
    let body_setup = r.read_package_index()?;
    let kdop = read_kdop(&mut r)?;
    let internal_version = r.read_i32()?;
    let source_data = if read_u32_bool(&mut r, "bHasSourceData")? {
        Some(Box::new(read_lod_model(&mut r)?))
    } else {
        None
    };
    let optimization_settings = read_array(
        &mut r,
        "optimization settings",
        OPTIMIZATION_SETTINGS_SIZE,
        read_optimization_settings,
    )?;
    let has_been_simplified = r.read_u32()?;
    let is_mesh_proxy = r.read_u32()?;
    let at = r.position();
    let lod_count = r.read_count("LOD models", 1)?;
    if lod_count > MAX_LODS {
        return Err(malformed(
            "LOD models",
            at,
            format!("{lod_count} LODs exceed the sanity limit {MAX_LODS}"),
        ));
    }
    let mut lods = Vec::with_capacity(lod_count);
    for _ in 0..lod_count {
        lods.push(read_lod_model(&mut r)?);
    }
    let at = r.position();
    let lod_info_count = r.read_u32()?;
    if i32::try_from(lod_info_count).is_err() {
        return Err(malformed(
            "LOD info count",
            at,
            format!("negative count {}", lod_info_count.cast_signed()),
        ));
    }
    let thumbnail_angle = [r.read_i32()?, r.read_i32()?, r.read_i32()?];
    let thumbnail_distance = r.read_f32()?;
    let high_res_source_mesh_name = r.read_fstring()?;
    let high_res_source_mesh_crc = r.read_u32()?;
    let lighting_guid = r.read_guid()?;
    let vertex_position_version = r.read_i32()?;
    let cached_streaming_texture_factors =
        r.read_tarray("CachedStreamingTextureFactors", 4, |r| r.read_f32())?;
    let remove_degenerates = r.read_u32()?;
    let per_lod_static_lighting_for_instancing = r.read_u32()?;
    let console_prealloc_instance_count = r.read_i32()?;
    if r.remaining() != 0 {
        return Err(malformed(
            "StaticMesh native data",
            r.position(),
            format!(
                "{} bytes left after the last known field (payload {} bytes)",
                r.remaining(),
                data.len()
            ),
        ));
    }
    Ok(StaticMeshNative {
        start,
        bounds,
        body_setup,
        kdop,
        internal_version,
        source_data,
        optimization_settings,
        has_been_simplified,
        is_mesh_proxy,
        lods,
        lod_info_count,
        thumbnail_angle,
        thumbnail_distance,
        high_res_source_mesh_name,
        high_res_source_mesh_crc,
        lighting_guid,
        vertex_position_version,
        cached_streaming_texture_factors,
        remove_degenerates,
        per_lod_static_lighting_for_instancing,
        console_prealloc_instance_count,
    })
}

/// True when export `index` is an `Engine.StaticMesh` (exactly; subclasses
/// such as `FracturedStaticMesh` append more data and are not accepted).
pub fn is_static_mesh(pkg: &Package, index: usize) -> bool {
    let Ok(class) = pkg.export_class_name(index) else {
        return false;
    };
    if class != "StaticMesh" {
        return false;
    }
    matches!(pkg.export_class_package(index), Ok(Some(p)) if p.eq_ignore_ascii_case("Engine"))
}

/// Decode export `index` as a `StaticMesh` (prelude, tags and native data).
pub fn decode_static_mesh(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<StaticMesh> {
    if !is_static_mesh(pkg, index) {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Engine.StaticMesh",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    }
    let object = decode_object(pkg, own_name, index, schema)?;
    let data = pkg.export_data(index)?;
    let native = decode_static_mesh_native(data, object.properties_end)?;
    Ok(StaticMesh { object, native })
}

// ---------------------------------------------------------------------------
// Encoder (inverse of the decoder; synthetic fixtures and round-trip checks)
// ---------------------------------------------------------------------------

/// Encode an `f32` as an IEEE half (round to nearest, ties to even;
/// overflow saturates to infinity). Exact for every value produced by
/// [`half_to_f32`], NaN payloads included (the top ten mantissa bits are
/// kept; a NaN whose payload lives only in the low bits becomes `0x7e00`).
pub fn f32_to_half(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = u16::try_from((bits >> 16) & 0x8000).unwrap_or(0);
    let exp = i32::try_from((bits >> 23) & 0xff).unwrap_or(0);
    let mant = bits & 0x007f_ffff;
    if exp == 0xff {
        if mant == 0 {
            return sign | 0x7c00;
        }
        let payload = u16::try_from(mant >> 13).unwrap_or(0x200);
        return sign | 0x7c00 | if payload == 0 { 0x200 } else { payload };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    let round = |value: u32, shift: u32| -> u32 {
        let kept = value >> shift;
        let rem = value & ((1u32 << shift) - 1);
        let half = 1u32 << (shift - 1);
        if rem > half || (rem == half && kept & 1 == 1) {
            kept + 1
        } else {
            kept
        }
    };
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        // Subnormal half: implicit bit made explicit, shifted into place.
        let shift = u32::try_from(14 - e).unwrap_or(24);
        let m = round(mant | 0x0080_0000, shift);
        return sign | u16::try_from(m).unwrap_or(0x7c00);
    }
    let e = u32::try_from(e).unwrap_or(0);
    // A mantissa carry correctly bumps the exponent (up to infinity).
    let h = (e << 10) + round(mant, 13);
    sign | u16::try_from(h).unwrap_or(0x7c00)
}

fn put_vec3(w: &mut Writer, v: [f32; 3]) {
    for c in v {
        w.u32(c.to_bits());
    }
}

fn put_count(w: &mut Writer, n: usize) -> Option<()> {
    w.i32(i32::try_from(n).ok()?);
    Some(())
}

fn put_bulk_header(w: &mut Writer, element_size: usize, count: usize) -> Option<()> {
    put_count(w, element_size)?;
    put_count(w, count)
}

fn encode_lod(w: &mut Writer, lod: &LodModel) -> Option<()> {
    let rt = &lod.raw_triangles;
    w.u32(rt.flags);
    w.i32(rt.element_count);
    w.i32(rt.size_on_disk);
    w.i32(rt.offset_in_file);
    if rt.has_inline_bytes() {
        // Payload bytes are not retained by the decoder.
        let n = usize::try_from(rt.size_on_disk).ok()?;
        w.bytes(&vec![0u8; n]);
    }
    put_count(w, lod.sections.len())?;
    for s in &lod.sections {
        w.i32(s.material.0);
        w.u32(s.enable_collision);
        w.u32(s.old_enable_collision);
        w.u32(s.enable_shadow_casting);
        w.u32(s.first_index);
        w.u32(s.num_triangles);
        w.u32(s.min_vertex_index);
        w.u32(s.max_vertex_index);
        w.i32(s.material_index);
        put_count(w, s.fragments.len())?;
        for f in &s.fragments {
            w.i32(f.base_index);
            w.i32(f.num_primitives);
        }
        w.u8(0);
    }
    w.u32(lod.positions.stride);
    w.u32(lod.positions.num_vertices);
    put_bulk_header(w, POSITION_ELEMENT_SIZE, lod.positions.positions.len())?;
    for p in &lod.positions.positions {
        put_vec3(w, *p);
    }
    let vb = &lod.vertices;
    let elem = vertex_element_size(vb.num_tex_coords, vb.full_precision_uvs)?;
    let n = vb.tangent_x.len();
    if vb.tangent_z.len() != n
        || usize::try_from(vb.num_tex_coords).ok()? != vb.uvs.len()
        || vb.uvs.iter().any(|c| c.len() != n)
    {
        return None;
    }
    w.u32(vb.num_tex_coords);
    w.u32(vb.stride);
    w.u32(vb.num_vertices);
    w.u32(u32::from(vb.full_precision_uvs));
    put_bulk_header(w, elem, n)?;
    for v in 0..n {
        w.bytes(&vb.tangent_x[v].0);
        w.bytes(&vb.tangent_z[v].0);
        for ch in &vb.uvs {
            for c in ch[v] {
                if vb.full_precision_uvs {
                    w.u32(c.to_bits());
                } else {
                    w.u16(f32_to_half(c));
                }
            }
        }
    }
    let cb = &lod.colors;
    w.u32(cb.stride);
    w.u32(cb.num_vertices);
    if cb.num_vertices != 0 {
        put_bulk_header(w, COLOR_ELEMENT_SIZE, cb.colors_bgra.len())?;
        for c in &cb.colors_bgra {
            w.bytes(c);
        }
    } else if !cb.colors_bgra.is_empty() {
        return None;
    }
    w.u32(lod.num_vertices);
    for buf in [&lod.indices, &lod.wireframe_indices, &lod.adjacency_indices] {
        put_bulk_header(w, INDEX_ELEMENT_SIZE, buf.len())?;
        for &i in buf.iter() {
            w.u16(i);
        }
    }
    Some(())
}

/// Serialize native data in the v868 layout: the exact inverse of
/// [`decode_static_mesh_native`] (used for synthetic fixtures and to prove
/// byte-exact round trips). The decoder does not keep inline raw-triangle
/// payload bytes, so `SizeOnDisk` zero bytes are written in their place.
/// Returns `None` when a count does not fit in an `i32` or the vertex buffer's
/// channel count and arrays disagree.
pub fn encode_static_mesh_native(n: &StaticMeshNative) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    put_vec3(&mut w, n.bounds.origin);
    put_vec3(&mut w, n.bounds.box_extent);
    w.u32(n.bounds.sphere_radius.to_bits());
    w.i32(n.body_setup.0);
    put_vec3(&mut w, n.kdop.root_bounds.min);
    put_vec3(&mut w, n.kdop.root_bounds.max);
    put_bulk_header(&mut w, KDOP_NODE_ELEMENT_SIZE, n.kdop.nodes.len())?;
    for node in &n.kdop.nodes {
        w.bytes(&node.bytes);
    }
    put_bulk_header(&mut w, KDOP_TRIANGLE_ELEMENT_SIZE, n.kdop.triangles.len())?;
    for t in &n.kdop.triangles {
        for v in t.vertices {
            w.u16(v);
        }
        w.u16(t.material_index);
    }
    w.i32(n.internal_version);
    match &n.source_data {
        Some(src) => {
            w.u32(1);
            encode_lod(&mut w, src)?;
        }
        None => w.u32(0),
    }
    put_count(&mut w, n.optimization_settings.len())?;
    for o in &n.optimization_settings {
        w.u8(o.reduction_method);
        w.u32(o.num_triangles_percentage.to_bits());
        w.u32(o.max_deviation_percentage.to_bits());
        w.u8(o.silhouette_importance);
        w.u8(o.texture_importance);
        w.u8(o.shading_importance);
        w.u32(o.recalc_normals);
        w.u32(o.normals_threshold.to_bits());
        w.u32(o.welding_threshold.to_bits());
    }
    w.u32(n.has_been_simplified);
    w.u32(n.is_mesh_proxy);
    put_count(&mut w, n.lods.len())?;
    for lod in &n.lods {
        encode_lod(&mut w, lod)?;
    }
    w.u32(n.lod_info_count);
    for a in n.thumbnail_angle {
        w.i32(a);
    }
    w.u32(n.thumbnail_distance.to_bits());
    if !w.fstring(&n.high_res_source_mesh_name) {
        return None;
    }
    w.u32(n.high_res_source_mesh_crc);
    w.guid(n.lighting_guid);
    w.i32(n.vertex_position_version);
    put_count(&mut w, n.cached_streaming_texture_factors.len())?;
    for f in &n.cached_streaming_texture_factors {
        w.u32(f.to_bits());
    }
    w.u32(n.remove_degenerates);
    w.u32(n.per_lod_static_lighting_for_instancing);
    w.i32(n.console_prealloc_instance_count);
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Structural validation
// ---------------------------------------------------------------------------

/// Context for [`validate_static_mesh`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidationContext {
    /// Absolute offset of the export payload in the uncompressed stream
    /// (`SerialOffset`), used to cross-check inline bulk-data `OffsetInFile`.
    pub payload_stream_offset: Option<i64>,
    /// Number of imports + exports, to range-check object references.
    pub imports: usize,
    /// Number of exports.
    pub exports: usize,
}

fn ref_in_range(idx: PackageIndex, ctx: &ValidationContext) -> bool {
    if idx.is_null() {
        return true;
    }
    if let Some(e) = idx.export_index() {
        return e < ctx.exports;
    }
    idx.import_index().is_some_and(|i| i < ctx.imports)
}

const BOUNDS_TOLERANCE: f32 = 1e-2;

/// Structural cross-checks of a decoded mesh. Returns one message per problem
/// (empty when consistent). These are consistency rules between fields, not
/// parse requirements; the real-data test requires them all to hold.
pub fn validate_static_mesh(native: &StaticMeshNative, ctx: &ValidationContext) -> Vec<String> {
    let mut issues = Vec::new();
    if !ref_in_range(native.body_setup, ctx) {
        issues.push(format!(
            "BodySetup reference {} out of range",
            native.body_setup.0
        ));
    }
    if native.lods.is_empty() {
        issues.push("no LOD models".to_owned());
    }
    if usize::try_from(native.lod_info_count).ok() != Some(native.lods.len()) {
        issues.push(format!(
            "LOD info count {} != {} LOD models",
            native.lod_info_count,
            native.lods.len()
        ));
    }
    if let Some(src) = &native.source_data {
        validate_lod(src, "source data", ctx, &native.bounds, false, &mut issues);
    }
    for (li, lod) in native.lods.iter().enumerate() {
        let label = format!("LOD {li}");
        validate_lod(lod, &label, ctx, &native.bounds, li == 0, &mut issues);
    }
    if let Some(lod0) = native.lods.first() {
        let nv = lod0.positions.positions.len();
        let ns = lod0.sections.len();
        for (ti, t) in native.kdop.triangles.iter().enumerate() {
            if t.vertices.iter().any(|&v| usize::from(v) >= nv) {
                issues.push(format!(
                    "collision triangle {ti} references a vertex >= {nv}"
                ));
                break;
            }
            if usize::from(t.material_index) >= ns.max(1) {
                issues.push(format!(
                    "collision triangle {ti} material index {} >= {ns} sections",
                    t.material_index
                ));
                break;
            }
        }
    }
    issues
}

fn validate_lod(
    lod: &LodModel,
    label: &str,
    ctx: &ValidationContext,
    bounds: &BoxSphereBounds,
    check_bounds: bool,
    issues: &mut Vec<String>,
) {
    let nv = lod.positions.positions.len();
    let nv_u32 = u32::try_from(nv).unwrap_or(u32::MAX);
    let mut push = |m: String| issues.push(format!("{label}: {m}"));
    // Raw triangle bulk data: an uncompressed inline payload holds exactly
    // ElementCount records, and its recorded offset is its stream position.
    let rt = &lod.raw_triangles;
    if rt.storage() == BulkStorage::Inline && rt.compression() == BulkCompression::None {
        if Some(rt.stored_len()) != rt.uncompressed_len(RAW_TRIANGLE_ELEMENT_SIZE) {
            push(format!(
                "raw triangles: SizeOnDisk {} does not match {} elements (flags {:#x})",
                rt.size_on_disk, rt.element_count, rt.flags
            ));
        }
        if let Some(base) = ctx.payload_stream_offset
            && !rt.inline_offset_matches(base)
        {
            push(format!(
                "raw triangles: OffsetInFile {} is not the payload's stream position",
                rt.offset_in_file
            ));
        }
    }
    // Vertex counts.
    if lod.positions.num_vertices != nv_u32 {
        push(format!(
            "position NumVertices {} != {nv} stored",
            lod.positions.num_vertices
        ));
    }
    if usize::try_from(lod.positions.stride).ok() != Some(POSITION_ELEMENT_SIZE) {
        push(format!("position stride {}", lod.positions.stride));
    }
    if lod.num_vertices != nv_u32 {
        push(format!(
            "NumVertices {} != {nv} positions",
            lod.num_vertices
        ));
    }
    let vb = &lod.vertices;
    if vb.num_vertices != nv_u32 || vb.tangent_z.len() != nv {
        push(format!(
            "vertex buffer has {} vertices (NumVertices {}), positions {nv}",
            vb.tangent_z.len(),
            vb.num_vertices
        ));
    }
    if usize::try_from(vb.stride).ok()
        != vertex_element_size(vb.num_tex_coords, vb.full_precision_uvs)
    {
        push(format!("vertex stride {}", vb.stride));
    }
    let cb = &lod.colors;
    if cb.num_vertices != 0 {
        if cb.num_vertices != nv_u32 || cb.colors_bgra.len() != nv {
            push(format!(
                "color buffer has {} colors (NumVertices {}), positions {nv}",
                cb.colors_bgra.len(),
                cb.num_vertices
            ));
        }
        if usize::try_from(cb.stride).ok() != Some(COLOR_ELEMENT_SIZE) {
            push(format!("color stride {}", cb.stride));
        }
    }
    // Index ranges.
    if let Some(&bad) = lod.indices.iter().find(|&&i| usize::from(i) >= nv) {
        push(format!("index {bad} >= {nv} vertices"));
    }
    if !lod.wireframe_indices.len().is_multiple_of(2) {
        push(format!(
            "wireframe index count {} is odd",
            lod.wireframe_indices.len()
        ));
    }
    if let Some(&bad) = lod
        .wireframe_indices
        .iter()
        .find(|&&i| usize::from(i) >= nv)
    {
        push(format!("wireframe index {bad} >= {nv} vertices"));
    }
    if let Some(&bad) = lod
        .adjacency_indices
        .iter()
        .find(|&&i| usize::from(i) >= nv)
    {
        push(format!("adjacency index {bad} >= {nv} vertices"));
    }
    let tris = lod.triangle_count();
    if !lod.adjacency_indices.is_empty()
        && u64::try_from(lod.adjacency_indices.len()).ok() != tris.checked_mul(12)
    {
        push(format!(
            "adjacency index count {} != 12 x {tris} triangles",
            lod.adjacency_indices.len()
        ));
    }
    // Sections.
    let mut covered = 0u64;
    for (si, s) in lod.sections.iter().enumerate() {
        if !ref_in_range(s.material, ctx) {
            push(format!("section {si} material reference out of range"));
        }
        // u32 operands: the products and sums below cannot overflow u64.
        let first = u64::from(s.first_index);
        let count = u64::from(s.num_triangles) * 3;
        let end = first + count;
        if end > u64::try_from(lod.indices.len()).unwrap_or(u64::MAX) {
            push(format!(
                "section {si} indices {first}..{end} exceed {} indices",
                lod.indices.len()
            ));
            continue;
        }
        covered = covered.saturating_add(count);
        if s.num_triangles > 0 {
            let range = usize::try_from(first).unwrap_or(0)..usize::try_from(end).unwrap_or(0);
            if let Some(slice) = lod.indices.get(range)
                && slice.iter().any(|&i| {
                    u32::from(i) < s.min_vertex_index || u32::from(i) > s.max_vertex_index
                })
            {
                push(format!(
                    "section {si} indices outside [{}, {}]",
                    s.min_vertex_index, s.max_vertex_index
                ));
            }
        }
    }
    if covered != u64::try_from(lod.indices.len()).unwrap_or(u64::MAX) {
        push(format!(
            "sections cover {covered} of {} indices",
            lod.indices.len()
        ));
    }
    // LOD 0 positions lie inside the bounds box. The tolerance is relative to
    // the coordinate magnitude (f32 rounding of large world-space meshes).
    // Coarser LODs are not checked: the bounds come from LOD 0 and simplified
    // LODs may poke slightly outside it.
    if check_bounds {
        let tol: [f32; 3] = std::array::from_fn(|k| {
            BOUNDS_TOLERANCE + 1e-5 * (bounds.origin[k].abs() + bounds.box_extent[k].abs())
        });
        let lo: [f32; 3] =
            std::array::from_fn(|k| bounds.origin[k] - bounds.box_extent[k] - tol[k]);
        let hi: [f32; 3] =
            std::array::from_fn(|k| bounds.origin[k] + bounds.box_extent[k] + tol[k]);
        if let Some(p) = lod
            .positions
            .positions
            .iter()
            .find(|p| (0..3).any(|k| !(p[k] >= lo[k] && p[k] <= hi[k])))
        {
            push(format!("position {p:?} outside the bounds box"));
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage over a package
// ---------------------------------------------------------------------------

/// Most failure samples kept per package.
const MAX_FAILURE_SAMPLES: usize = 16;

/// StaticMesh coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MeshCoverage {
    /// Package name.
    pub package: String,
    /// `StaticMesh` exports found.
    pub total: usize,
    /// Exports whose native data decoded and ended exactly at `SerialSize`.
    pub exact: usize,
    /// Exports that also passed every structural cross-check.
    pub valid: usize,
    /// Exports whose decoded native data re-encodes ([`encode_static_mesh_native`])
    /// to exactly the original native bytes.
    pub round_trip: usize,
    /// Native tail bytes consumed (sum over exact exports).
    pub native_bytes: u64,
    /// LOD models per mesh -> mesh count.
    pub lod_histogram: BTreeMap<usize, usize>,
    /// Sections over all LODs.
    pub sections: usize,
    /// Vertices over all LODs.
    pub vertices: u64,
    /// Triangles over all LODs.
    pub triangles: u64,
    /// Collision triangles.
    pub collision_triangles: u64,
    /// Meshes with a vertex color buffer in LOD 0.
    pub with_colors: usize,
    /// UV channel count -> LOD count.
    pub uv_channels: BTreeMap<u32, usize>,
    /// LODs with full-precision UVs.
    pub full_precision_uv_lods: usize,
    /// Meshes with a non-null BodySetup.
    pub with_body_setup: usize,
    /// Meshes with source data.
    pub with_source_data: usize,
    /// LODs with an adjacency buffer.
    pub with_adjacency: usize,
    /// Raw-triangle bulk-data flags -> LOD count.
    pub raw_triangle_flags: BTreeMap<String, usize>,
    /// First decode failures (`export: error`).
    pub failures: Vec<String>,
    /// First validation issues (`export: issue`).
    pub issues: Vec<String>,
}

/// Decode every `StaticMesh` export of `lp` and gather statistics.
pub fn mesh_coverage(lp: &LoadedPackage, schema: &dyn Schema) -> MeshCoverage {
    let pkg = &lp.package;
    let mut cov = MeshCoverage {
        package: lp.name.clone(),
        ..MeshCoverage::default()
    };
    for i in 0..pkg.exports.len() {
        if !is_static_mesh(pkg, i) {
            continue;
        }
        cov.total += 1;
        let mesh = match decode_static_mesh(pkg, Some(&lp.name), i, schema) {
            Ok(m) => m,
            Err(e) => {
                if cov.failures.len() < MAX_FAILURE_SAMPLES {
                    cov.failures.push(format!("{i}: {e}"));
                }
                continue;
            }
        };
        cov.exact += 1;
        let n = &mesh.native;
        let original = pkg
            .export_data(i)
            .ok()
            .and_then(|d| d.get(mesh.object.properties_end..));
        if original.is_some() && encode_static_mesh_native(n).as_deref() == original {
            cov.round_trip += 1;
        }
        cov.native_bytes += u64::try_from(mesh.object.native_tail()).unwrap_or(0);
        let ctx = ValidationContext {
            payload_stream_offset: pkg.export(i).ok().map(|e| i64::from(e.serial_offset)),
            imports: pkg.imports.len(),
            exports: pkg.exports.len(),
        };
        let issues = validate_static_mesh(n, &ctx);
        if issues.is_empty() {
            cov.valid += 1;
        } else {
            for m in issues {
                if cov.issues.len() < MAX_FAILURE_SAMPLES {
                    cov.issues.push(format!("{i}: {m}"));
                }
            }
        }
        *cov.lod_histogram.entry(n.lods.len()).or_insert(0) += 1;
        cov.collision_triangles += u64::try_from(n.kdop.triangles.len()).unwrap_or(0);
        if !n.body_setup.is_null() {
            cov.with_body_setup += 1;
        }
        if n.source_data.is_some() {
            cov.with_source_data += 1;
        }
        if n.lods.first().is_some_and(|l| l.colors.num_vertices != 0) {
            cov.with_colors += 1;
        }
        for lod in &n.lods {
            cov.sections += lod.sections.len();
            cov.vertices += u64::try_from(lod.positions.positions.len()).unwrap_or(0);
            cov.triangles += lod.triangle_count();
            *cov.uv_channels
                .entry(lod.vertices.num_tex_coords)
                .or_insert(0) += 1;
            if lod.vertices.full_precision_uvs {
                cov.full_precision_uv_lods += 1;
            }
            if !lod.adjacency_indices.is_empty() {
                cov.with_adjacency += 1;
            }
            *cov.raw_triangle_flags
                .entry(format!("{:#x}", lod.raw_triangles.flags))
                .or_insert(0) += 1;
        }
    }
    cov
}
