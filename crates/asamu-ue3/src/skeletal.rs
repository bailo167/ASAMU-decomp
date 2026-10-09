//! `SkeletalMesh` native payload decoding for UE3 v868 / licensee 0.
//!
//! A `SkeletalMesh` export is the usual object prelude and tagged properties
//! (see [`crate::object`]) followed by native data written by
//! `USkeletalMesh::Serialize`. The layout below is the one under which
//! **every** `SkeletalMesh` export of every shipped package is consumed
//! exactly to `SerialSize` and re-encoded byte for byte (see
//! `docs/reverse-engineering/SKELETAL.md` for the evidence, coverage and
//! confidence of each field):
//!
//! ```text
//! FBoxSphereBounds   Origin(FVector) BoxExtent(FVector) f32 SphereRadius
//! TArray<obj>        Materials
//! FVector            Origin
//! FRotator           RotOrigin (i32 pitch, yaw, roll)
//! TArray<FMeshBone>  RefSkeleton (52 bytes each, below)
//! i32                SkeletalDepth
//! TArray             LODModels (FStaticLODModel, below)
//! TMap<FName, i32>   NameIndexMap (i32 count, then FName + i32 pairs)
//! TArray             PerPolyBoneKDOPs (FPerPolyBoneCollisionData)
//! TArray<FString>    BoneBreakNames
//! TArray<u8>         BoneBreakOptions
//! TArray<obj>        ClothingAssets (native copy)
//! TArray<f32>        CachedStreamingTextureFactors
//! u32                bHaveSourceData (+ one FStaticLODModel when 1)
//! ```
//!
//! `FMeshBone`: `FName Name`, `u32 Flags`, `FQuat Orientation` (x, y, z, w),
//! `FVector Position`, `i32 NumChildren`, `i32 ParentIndex`, `FColor BoneColor`.
//!
//! One LOD (`FStaticLODModel::Serialize`):
//!
//! ```text
//! TArray<FSkelMeshSection>   u16 MaterialIndex, u16 ChunkIndex, u32 BaseIndex,
//!                            u32 NumTriangles, u8 TriangleSorting (13 bytes)
//! FMultiSizeIndexContainer   u32 bNeedsCPUAccess, u8 DataTypeSize (2 | 4),
//!                            bulk TArray<u16 | u32> (element size = DataTypeSize)
//! TArray<u16>                ActiveBoneIndices
//! TArray<FSkelMeshChunk>     chunks (below)
//! u32                        Size
//! u32                        NumVertices
//! TArray<u8>                 RequiredBones
//! bulk data record           RawPointIndices (i32 elements)
//! u32                        NumTexCoords
//! FSkeletalMeshVertexBuffer  u32 NumTexCoords, u32 bUseFullPrecisionUVs,
//!                            u32 bUsePackedPosition, FVector MeshExtension,
//!                            FVector MeshOrigin, bulk TArray<GPU skin vertex>
//! [bHasVertexColors]         bulk TArray<FColor> (element size 4)
//! TArray<FSkeletalMeshVertexInfluences>
//! FMultiSizeIndexContainer   adjacency indices
//! ```
//!
//! `bHasVertexColors` is a tagged property of the mesh (a bit of the same
//! bitfield the engine tests while loading); the caller passes it in.
//!
//! GPU skin vertex (raw memory layout of `TGPUSkinVertexFloat{16,32}Uvs[32Xyz]<N>`):
//! `FPackedNormal TangentX`, `FPackedNormal TangentZ`, `u8 InfluenceBones[4]`,
//! `u8 InfluenceWeights[4]`, `FVector` position, then `N` UVs (half2 or
//! float2). `bUsePackedPosition` is stored but does not select the vertex
//! type in this build (positions are always full floats).
//!
//! Chunk (`FSkelMeshChunk`): `u32 BaseVertexIndex`, `TArray<FRigidSkinVertex>`,
//! `TArray<FSoftSkinVertex>`, `TArray<u16> BoneMap`, `i32 NumRigidVertices`,
//! `i32 NumSoftVertices`, `i32 MaxBoneInfluences`. A rigid vertex is
//! `FVector Position`, three `FPackedNormal` (TangentX/Y/Z), four `FVector2D`
//! UVs, `FColor Color`, `u8 Bone` (61 bytes); a soft vertex replaces the bone
//! with `u8 InfluenceBones[4]` and `u8 InfluenceWeights[4]` (68 bytes).
//!
//! Besides the decoder ([`decode_skeletal_mesh`], [`decode_skeletal_mesh_native`])
//! the module offers structural cross-checks ([`validate_skeletal_mesh`]), a
//! per-package coverage pass ([`skeletal_coverage`]) and the exact inverse
//! encoder ([`encode_skeletal_mesh_native`]).
//!
//! Every count is checked against the remaining bytes before anything is
//! allocated, arithmetic is checked, and malformed input yields an
//! [`ObjectError`]; nothing here panics.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::bulkdata::{
    BulkCompression, BulkDataRecord, BulkStorage, inline_bytes, read_bulk_record,
};
use crate::model::LoadedPackage;
use crate::object::{DecodedObject, ObjResult, ObjectError, decode_object};
use crate::package::Package;
use crate::reader::Reader;
use crate::schema::Schema;
use crate::staticmesh::{BoxSphereBounds, KdopBounds, PackedNormal, f32_to_half, half_to_f32};
use crate::types::{FName, PackageIndex};
use crate::writer::Writer;

/// Serialized size of one `FMeshBone`.
pub const MESH_BONE_SIZE: usize = 52;
/// Serialized size of one `FSkelMeshSection`.
pub const SECTION_SIZE: usize = 13;
/// Serialized size of one `FRigidSkinVertex`.
pub const RIGID_VERTEX_SIZE: usize = 61;
/// Serialized size of one `FSoftSkinVertex`.
pub const SOFT_VERTEX_SIZE: usize = 68;
/// Smallest serialized `FSkelMeshChunk` (empty arrays).
pub const CHUNK_MIN_SIZE: usize = 4 + 4 + 4 + 4 + 12;
/// Smallest serialized `FStaticLODModel` (empty arrays, no colors):
/// sections, index container, active bones, chunks, `Size`, `NumVertices`,
/// required bones, raw-point record, `NumTexCoords`, vertex buffer header,
/// vertex influences, adjacency container.
pub const LOD_MIN_SIZE: usize = 4 + 13 + 4 + 4 + 4 + 4 + 4 + 16 + 4 + 44 + 4 + 13;
/// Element size of a compact kDOP node.
pub const KDOP_NODE_SIZE: usize = 6;
/// Element size of a kDOP collision triangle.
pub const KDOP_TRIANGLE_SIZE: usize = 8;
/// Element size of one `FVertexInfluence`.
pub const VERTEX_INFLUENCE_SIZE: usize = 8;
/// Element size of one raw point index.
pub const RAW_POINT_INDEX_SIZE: usize = 4;
/// Element size of a vertex color.
pub const COLOR_SIZE: usize = 4;
/// Largest UV channel count of a vertex buffer.
pub const MAX_TEX_COORDS: u32 = 4;
/// Influences per skinned vertex.
pub const MAX_INFLUENCES: usize = 4;
/// Upper bound on LOD models accepted (sanity limit, not a format value).
pub const MAX_LODS: usize = 64;

/// One reference-skeleton bone (`FMeshBone`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MeshBone {
    /// Bone name (package name-table entry).
    pub name: FName,
    /// `Flags`.
    pub flags: u32,
    /// Reference-pose rotation relative to the parent, `(x, y, z, w)`.
    pub orientation: [f32; 4],
    /// Reference-pose translation relative to the parent (UE3 units, axes).
    pub position: [f32; 3],
    /// `NumChildren`.
    pub num_children: i32,
    /// `ParentIndex` (the root's is 0, its own index).
    pub parent_index: i32,
    /// Editor bone color, stored byte order `B, G, R, A`.
    pub bone_color: [u8; 4],
}

/// One section (`FSkelMeshSection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SkelSection {
    /// Material slot (index into `Materials`).
    pub material_index: u16,
    /// Chunk whose bone map the section's vertices use.
    pub chunk_index: u16,
    /// First index into the index buffer.
    pub base_index: u32,
    /// Triangle count.
    pub num_triangles: u32,
    /// `TriangleSorting` (ETriangleSortOption).
    pub triangle_sorting: u8,
}

/// `FMultiSizeIndexContainer`: an index buffer of 16- or 32-bit indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MultiSizeIndices {
    /// `bNeedsCPUAccess`.
    pub needs_cpu_access: u32,
    /// Bytes per index (2 or 4).
    pub data_type_size: u8,
    /// Indices (widened to `u32`).
    pub indices: Vec<u32>,
}

/// One rigid (single-bone) source vertex (`FRigidSkinVertex`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RigidSkinVertex {
    /// Position.
    pub position: [f32; 3],
    /// `TangentX`, `TangentY`, `TangentZ`.
    pub tangents: [PackedNormal; 3],
    /// Four UV channels.
    pub uvs: [[f32; 2]; 4],
    /// Color (`B, G, R, A`).
    pub color: [u8; 4],
    /// Bone (index into the chunk's bone map).
    pub bone: u8,
}

/// One soft (multi-bone) source vertex (`FSoftSkinVertex`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SoftSkinVertex {
    /// Position.
    pub position: [f32; 3],
    /// `TangentX`, `TangentY`, `TangentZ`.
    pub tangents: [PackedNormal; 3],
    /// Four UV channels.
    pub uvs: [[f32; 2]; 4],
    /// Color (`B, G, R, A`).
    pub color: [u8; 4],
    /// Influence bones (indices into the chunk's bone map).
    pub influence_bones: [u8; 4],
    /// Influence weights (0..=255).
    pub influence_weights: [u8; 4],
}

/// One chunk (`FSkelMeshChunk`): a vertex range sharing one bone map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkelChunk {
    /// First vertex of the chunk in the vertex buffer.
    pub base_vertex_index: u32,
    /// Rigid source vertices (often stripped by the cooker).
    pub rigid_vertices: Vec<RigidSkinVertex>,
    /// Soft source vertices (often stripped by the cooker).
    pub soft_vertices: Vec<SoftSkinVertex>,
    /// Chunk bone index -> reference-skeleton bone index.
    pub bone_map: Vec<u16>,
    /// `NumRigidVertices`.
    pub num_rigid_vertices: i32,
    /// `NumSoftVertices`.
    pub num_soft_vertices: i32,
    /// `MaxBoneInfluences`.
    pub max_bone_influences: i32,
}

impl SkelChunk {
    /// Vertices covered by the chunk (`NumRigidVertices + NumSoftVertices`),
    /// or `None` when negative or overflowing.
    pub fn vertex_count(&self) -> Option<u32> {
        let n =
            i64::from(self.num_rigid_vertices).checked_add(i64::from(self.num_soft_vertices))?;
        u32::try_from(n).ok()
    }
}

/// One GPU skin vertex.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GpuSkinVertex {
    /// `TangentX`.
    pub tangent_x: PackedNormal,
    /// `TangentZ` (normal; W = bitangent sign).
    pub tangent_z: PackedNormal,
    /// Influence bones (indices into the owning chunk's bone map).
    pub influence_bones: [u8; 4],
    /// Influence weights (sum 255 when normalised).
    pub influence_weights: [u8; 4],
    /// Reference-pose position (mesh space, UE3 units and axes).
    pub position: [f32; 3],
    /// UVs; only the first `NumTexCoords` channels are stored.
    pub uvs: [[f32; 2]; 4],
}

/// `FSkeletalMeshVertexBuffer`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuSkinVertexBuffer {
    /// UV channels per vertex (1..=4).
    pub num_tex_coords: u32,
    /// UVs stored as 32-bit floats (otherwise halves).
    pub use_full_precision_uvs: bool,
    /// `bUsePackedPosition` as stored. This build's loader ignores it: the
    /// vertex type depends only on the UV precision and channel count, and
    /// positions are always full `FVector`s.
    pub use_packed_position: bool,
    /// `MeshExtension` (scale of packed positions; name TENTATIVE).
    pub mesh_extension: [f32; 3],
    /// `MeshOrigin` (offset of packed positions; name TENTATIVE).
    pub mesh_origin: [f32; 3],
    /// Vertices.
    pub vertices: Vec<GpuSkinVertex>,
}

/// `FVertexInfluence`: four weights and four bones packed in two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VertexInfluence {
    /// Weights (four bytes).
    pub weights: [u8; 4],
    /// Bones (four bytes).
    pub bones: [u8; 4],
}

/// `FSkeletalMeshVertexInfluences` (alternative weights; layout of a
/// non-empty mapping is TENTATIVE).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VertexInfluences {
    /// Influences per vertex.
    pub influences: Vec<VertexInfluence>,
    /// `VertexInfluenceMapping`: bone pair -> vertex list.
    pub mapping: Vec<([i32; 2], Vec<u32>)>,
    /// Sections.
    pub sections: Vec<SkelSection>,
    /// Chunks.
    pub chunks: Vec<SkelChunk>,
    /// `RequiredBones`.
    pub required_bones: Vec<u8>,
    /// `Usage`.
    pub usage: u8,
}

/// One LOD model (`FStaticLODModel`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkelLodModel {
    /// Sections.
    pub sections: Vec<SkelSection>,
    /// Triangle-list index buffer.
    pub indices: MultiSizeIndices,
    /// `ActiveBoneIndices`.
    pub active_bone_indices: Vec<u16>,
    /// Chunks.
    pub chunks: Vec<SkelChunk>,
    /// `Size`.
    pub size: u32,
    /// `NumVertices`.
    pub num_vertices: u32,
    /// `RequiredBones`.
    pub required_bones: Vec<u8>,
    /// `RawPointIndices` bulk-data record header.
    pub raw_point_indices_record: BulkDataRecord,
    /// `RawPointIndices` values when stored inline and uncompressed.
    pub raw_point_indices: Vec<u32>,
    /// `NumTexCoords`.
    pub num_tex_coords: u32,
    /// GPU skin vertex buffer.
    pub vertex_buffer: GpuSkinVertexBuffer,
    /// Vertex colors (`B, G, R, A`), present when the mesh has vertex colors.
    pub colors: Option<Vec<[u8; 4]>>,
    /// Alternative influences.
    pub vertex_influences: Vec<VertexInfluences>,
    /// Adjacency index buffer.
    pub adjacency: MultiSizeIndices,
}

impl SkelLodModel {
    /// Total triangles over all sections.
    pub fn triangle_count(&self) -> u64 {
        self.sections
            .iter()
            .map(|s| u64::from(s.num_triangles))
            .sum()
    }

    /// Index of the chunk containing vertex `v` (by base vertex index).
    pub fn chunk_of_vertex(&self, v: u32) -> Option<usize> {
        self.chunks.iter().position(|c| {
            c.vertex_count().is_some_and(|n| {
                v >= c.base_vertex_index
                    && u64::from(v) < u64::from(c.base_vertex_index) + u64::from(n)
            })
        })
    }
}

/// One per-poly bone collision record (`FPerPolyBoneCollisionData`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerPolyBoneCollision {
    /// kDOP root bounds.
    pub root_bounds: KdopBounds,
    /// Compact nodes (six opaque bytes each).
    pub nodes: Vec<[u8; 6]>,
    /// Collision triangles: three vertex indices and a material index.
    pub triangles: Vec<[u16; 4]>,
    /// Collision vertices.
    pub vertices: Vec<[f32; 3]>,
}

/// Everything `USkeletalMesh::Serialize` writes after the tagged properties.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkeletalMeshNative {
    /// Payload offset where the native data starts (end of tagged properties).
    pub start: usize,
    /// Bounds.
    pub bounds: BoxSphereBounds,
    /// Materials.
    pub materials: Vec<PackageIndex>,
    /// `Origin`.
    pub origin: [f32; 3],
    /// `RotOrigin` (pitch, yaw, roll in UE3 rotator units).
    pub rot_origin: [i32; 3],
    /// Reference skeleton.
    pub ref_skeleton: Vec<MeshBone>,
    /// `SkeletalDepth`.
    pub skeletal_depth: i32,
    /// LOD models, finest first.
    pub lods: Vec<SkelLodModel>,
    /// `NameIndexMap` pairs in stored order.
    pub name_index_map: Vec<(FName, i32)>,
    /// `PerPolyBoneKDOPs`.
    pub per_poly_bone_kdops: Vec<PerPolyBoneCollision>,
    /// `BoneBreakNames`.
    pub bone_break_names: Vec<String>,
    /// `BoneBreakOptions`.
    pub bone_break_options: Vec<u8>,
    /// Native `ClothingAssets` array.
    pub clothing_assets: Vec<PackageIndex>,
    /// `CachedStreamingTextureFactors`.
    pub cached_streaming_texture_factors: Vec<f32>,
    /// Source (import) model, when stored.
    pub source_data: Option<Box<SkelLodModel>>,
}

/// A decoded `SkeletalMesh` export: generic object plus native data.
#[derive(Debug, Clone, Serialize)]
pub struct SkeletalMesh {
    /// Prelude and tagged properties.
    pub object: DecodedObject,
    /// `bHasVertexColors` (tagged property; selects the color buffer).
    pub has_vertex_colors: bool,
    /// Native data.
    pub native: SkeletalMeshNative,
}

impl SkeletalMesh {
    /// Object references of the tagged `Sockets` array.
    pub fn socket_refs(&self) -> Vec<crate::ObjRef> {
        self.object
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("Sockets"))
            .and_then(|p| match &p.value {
                crate::Value::Array(items) => Some(
                    items
                        .iter()
                        .filter_map(|v| match v {
                            crate::Value::Object(o) => Some(o.clone()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// Resolve every bone name through the package name table.
pub fn bone_names(pkg: &Package, native: &SkeletalMeshNative) -> Vec<String> {
    native
        .ref_skeleton
        .iter()
        .map(|b| pkg.fname(b.name))
        .collect()
}

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

fn read_vec3(r: &mut Reader<'_>) -> ObjResult<[f32; 3]> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

fn read4(r: &mut Reader<'_>) -> ObjResult<[u8; 4]> {
    let b = r.read_bytes(4)?;
    let mut out = [0u8; 4];
    out.copy_from_slice(b);
    Ok(out)
}

fn read_u32_bool(r: &mut Reader<'_>, what: &'static str) -> ObjResult<bool> {
    let at = r.position();
    match r.read_u32()? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(malformed(what, at, format!("boolean is {v}, not 0 or 1"))),
    }
}

/// `TArray<T>` with an object-level element decoder.
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

/// Bulk array header with a required element size; returns the count.
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

fn read_bone(r: &mut Reader<'_>) -> ObjResult<MeshBone> {
    let name = r.read_fname()?;
    let flags = r.read_u32()?;
    let orientation = [r.read_f32()?, r.read_f32()?, r.read_f32()?, r.read_f32()?];
    let position = read_vec3(r)?;
    let num_children = r.read_i32()?;
    let parent_index = r.read_i32()?;
    let bone_color = read4(r)?;
    Ok(MeshBone {
        name,
        flags,
        orientation,
        position,
        num_children,
        parent_index,
        bone_color,
    })
}

fn read_section(r: &mut Reader<'_>) -> ObjResult<SkelSection> {
    Ok(SkelSection {
        material_index: r.read_u16()?,
        chunk_index: r.read_u16()?,
        base_index: r.read_u32()?,
        num_triangles: r.read_u32()?,
        triangle_sorting: r.read_u8()?,
    })
}

fn read_multi_size_indices(r: &mut Reader<'_>, what: &'static str) -> ObjResult<MultiSizeIndices> {
    let needs_cpu_access = r.read_u32()?;
    let at = r.position();
    let data_type_size = r.read_u8()?;
    if data_type_size != 2 && data_type_size != 4 {
        return Err(malformed(
            what,
            at,
            format!("index size {data_type_size}, expected 2 or 4"),
        ));
    }
    let n = read_bulk_count(r, what, usize::from(data_type_size))?;
    let mut indices = Vec::with_capacity(n);
    for _ in 0..n {
        indices.push(if data_type_size == 2 {
            u32::from(r.read_u16()?)
        } else {
            r.read_u32()?
        });
    }
    Ok(MultiSizeIndices {
        needs_cpu_access,
        data_type_size,
        indices,
    })
}

fn read_uv4(r: &mut Reader<'_>) -> ObjResult<[[f32; 2]; 4]> {
    let mut uvs = [[0f32; 2]; 4];
    for uv in &mut uvs {
        *uv = [r.read_f32()?, r.read_f32()?];
    }
    Ok(uvs)
}

fn read_tangents(r: &mut Reader<'_>) -> ObjResult<[PackedNormal; 3]> {
    Ok([
        PackedNormal(read4(r)?),
        PackedNormal(read4(r)?),
        PackedNormal(read4(r)?),
    ])
}

fn read_rigid_vertex(r: &mut Reader<'_>) -> ObjResult<RigidSkinVertex> {
    Ok(RigidSkinVertex {
        position: read_vec3(r)?,
        tangents: read_tangents(r)?,
        uvs: read_uv4(r)?,
        color: read4(r)?,
        bone: r.read_u8()?,
    })
}

fn read_soft_vertex(r: &mut Reader<'_>) -> ObjResult<SoftSkinVertex> {
    Ok(SoftSkinVertex {
        position: read_vec3(r)?,
        tangents: read_tangents(r)?,
        uvs: read_uv4(r)?,
        color: read4(r)?,
        influence_bones: read4(r)?,
        influence_weights: read4(r)?,
    })
}

fn read_u16_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<u16>> {
    Ok(r.read_tarray(what, 2, |r| r.read_u16())?)
}

fn read_u8_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<u8>> {
    let n = r.read_count(what, 1)?;
    Ok(r.read_bytes(n)?.to_vec())
}

fn read_chunk(r: &mut Reader<'_>) -> ObjResult<SkelChunk> {
    let base_vertex_index = r.read_u32()?;
    let rigid_vertices = read_array(
        r,
        "chunk rigid vertices",
        RIGID_VERTEX_SIZE,
        read_rigid_vertex,
    )?;
    let soft_vertices = read_array(r, "chunk soft vertices", SOFT_VERTEX_SIZE, read_soft_vertex)?;
    let bone_map = read_u16_array(r, "chunk bone map")?;
    Ok(SkelChunk {
        base_vertex_index,
        rigid_vertices,
        soft_vertices,
        bone_map,
        num_rigid_vertices: r.read_i32()?,
        num_soft_vertices: r.read_i32()?,
        max_bone_influences: r.read_i32()?,
    })
}

/// Serialized size of one GPU skin vertex: 28 bytes plus 4 (half) or 8
/// (float) per UV channel.
pub fn gpu_vertex_size(num_tex_coords: u32, full_precision_uvs: bool) -> Option<usize> {
    if !(1..=MAX_TEX_COORDS).contains(&num_tex_coords) {
        return None;
    }
    let uv = if full_precision_uvs { 8 } else { 4 };
    usize::try_from(num_tex_coords)
        .ok()?
        .checked_mul(uv)?
        .checked_add(28)
}

fn read_vertex_buffer(r: &mut Reader<'_>) -> ObjResult<GpuSkinVertexBuffer> {
    let at = r.position();
    let num_tex_coords = r.read_u32()?;
    let use_full_precision_uvs = read_u32_bool(r, "bUseFullPrecisionUVs")?;
    let use_packed_position = read_u32_bool(r, "bUsePackedPosition")?;
    let mesh_extension = read_vec3(r)?;
    let mesh_origin = read_vec3(r)?;
    let elem = gpu_vertex_size(num_tex_coords, use_full_precision_uvs).ok_or_else(|| {
        malformed(
            "skeletal vertex buffer NumTexCoords",
            at,
            format!("{num_tex_coords} UV channels (expected 1..={MAX_TEX_COORDS})"),
        )
    })?;
    let n = read_bulk_count(r, "GPU skin vertices", elem)?;
    let channels = usize::try_from(num_tex_coords).unwrap_or(0);
    let mut vertices = Vec::with_capacity(n);
    for _ in 0..n {
        let tangent_x = PackedNormal(read4(r)?);
        let tangent_z = PackedNormal(read4(r)?);
        let influence_bones = read4(r)?;
        let influence_weights = read4(r)?;
        let position = read_vec3(r)?;
        let mut uvs = [[0f32; 2]; 4];
        for uv in uvs.iter_mut().take(channels) {
            *uv = if use_full_precision_uvs {
                [r.read_f32()?, r.read_f32()?]
            } else {
                [half_to_f32(r.read_u16()?), half_to_f32(r.read_u16()?)]
            };
        }
        vertices.push(GpuSkinVertex {
            tangent_x,
            tangent_z,
            influence_bones,
            influence_weights,
            position,
            uvs,
        });
    }
    Ok(GpuSkinVertexBuffer {
        num_tex_coords,
        use_full_precision_uvs,
        use_packed_position,
        mesh_extension,
        mesh_origin,
        vertices,
    })
}

fn read_vertex_influences(r: &mut Reader<'_>) -> ObjResult<VertexInfluences> {
    let influences = read_array(r, "vertex influences", VERTEX_INFLUENCE_SIZE, |r| {
        Ok(VertexInfluence {
            weights: read4(r)?,
            bones: read4(r)?,
        })
    })?;
    let mapping = read_array(r, "vertex influence mapping", 12, |r| {
        let key = [r.read_i32()?, r.read_i32()?];
        let verts = r.read_tarray("vertex influence mapping list", 4, |r| r.read_u32())?;
        Ok((key, verts))
    })?;
    let sections = read_array(r, "vertex influence sections", SECTION_SIZE, read_section)?;
    let chunks = read_array(r, "vertex influence chunks", CHUNK_MIN_SIZE, read_chunk)?;
    let required_bones = read_u8_array(r, "vertex influence required bones")?;
    let usage = r.read_u8()?;
    Ok(VertexInfluences {
        influences,
        mapping,
        sections,
        chunks,
        required_bones,
        usage,
    })
}

fn read_raw_point_indices(r: &mut Reader<'_>) -> ObjResult<(BulkDataRecord, Vec<u32>)> {
    let at = r.position();
    let rec = read_bulk_record(r)
        .map_err(|e| malformed("RawPointIndices bulk data", at, e.to_string()))?;
    let mut values = Vec::new();
    if rec.storage() == BulkStorage::Inline && rec.compression() == BulkCompression::None {
        let bytes = inline_bytes(&rec, r.data())
            .map_err(|e| malformed("RawPointIndices bulk data", at, e.to_string()))?;
        if Some(bytes.len()) != rec.uncompressed_len(RAW_POINT_INDEX_SIZE) {
            return Err(malformed(
                "RawPointIndices bulk data",
                at,
                format!(
                    "{} inline bytes for {} elements",
                    bytes.len(),
                    rec.element_count
                ),
            ));
        }
        values.reserve_exact(bytes.len() / RAW_POINT_INDEX_SIZE);
        values.extend(
            bytes
                .as_chunks::<RAW_POINT_INDEX_SIZE>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c)),
        );
    } else if rec.has_inline_bytes() && rec.size_on_disk != 0 {
        // Compressed inline payloads are never present in the shipped data;
        // refuse rather than keep bytes that cannot be re-encoded.
        return Err(malformed(
            "RawPointIndices bulk data",
            at,
            format!("unsupported inline payload (flags {:#x})", rec.flags),
        ));
    }
    Ok((rec, values))
}

/// Decode one `FStaticLODModel` at the reader's position.
pub fn read_skel_lod(r: &mut Reader<'_>, has_vertex_colors: bool) -> ObjResult<SkelLodModel> {
    let sections = read_array(r, "LOD sections", SECTION_SIZE, read_section)?;
    let indices = read_multi_size_indices(r, "LOD index buffer")?;
    let active_bone_indices = read_u16_array(r, "ActiveBoneIndices")?;
    let chunks = read_array(r, "LOD chunks", CHUNK_MIN_SIZE, read_chunk)?;
    let size = r.read_u32()?;
    let num_vertices = r.read_u32()?;
    let required_bones = read_u8_array(r, "RequiredBones")?;
    let (raw_point_indices_record, raw_point_indices) = read_raw_point_indices(r)?;
    let num_tex_coords = r.read_u32()?;
    let vertex_buffer = read_vertex_buffer(r)?;
    let colors = if has_vertex_colors {
        let n = read_bulk_count(r, "skeletal vertex colors", COLOR_SIZE)?;
        let mut c = Vec::with_capacity(n);
        for _ in 0..n {
            c.push(read4(r)?);
        }
        Some(c)
    } else {
        None
    };
    let vertex_influences = read_array(
        r,
        "vertex influence sets",
        4 * 5 + 1,
        read_vertex_influences,
    )?;
    let adjacency = read_multi_size_indices(r, "adjacency index buffer")?;
    Ok(SkelLodModel {
        sections,
        indices,
        active_bone_indices,
        chunks,
        size,
        num_vertices,
        required_bones,
        raw_point_indices_record,
        raw_point_indices,
        num_tex_coords,
        vertex_buffer,
        colors,
        vertex_influences,
        adjacency,
    })
}

fn read_per_poly(r: &mut Reader<'_>) -> ObjResult<PerPolyBoneCollision> {
    let root_bounds = KdopBounds {
        min: read_vec3(r)?,
        max: read_vec3(r)?,
    };
    let n = read_bulk_count(r, "per-poly kDOP nodes", KDOP_NODE_SIZE)?;
    let mut nodes = Vec::with_capacity(n);
    for _ in 0..n {
        let b = r.read_bytes(KDOP_NODE_SIZE)?;
        let mut node = [0u8; 6];
        node.copy_from_slice(b);
        nodes.push(node);
    }
    let n = read_bulk_count(r, "per-poly kDOP triangles", KDOP_TRIANGLE_SIZE)?;
    let mut triangles = Vec::with_capacity(n);
    for _ in 0..n {
        triangles.push([r.read_u16()?, r.read_u16()?, r.read_u16()?, r.read_u16()?]);
    }
    let vertices = read_array(r, "per-poly collision vertices", 12, read_vec3)?;
    Ok(PerPolyBoneCollision {
        root_bounds,
        nodes,
        triangles,
        vertices,
    })
}

/// Decode the native data of a `SkeletalMesh` payload starting at `start`
/// (the end of the tagged properties). `has_vertex_colors` is the mesh's
/// `bHasVertexColors` property. The decoder must end exactly at the end of
/// `data`; otherwise [`ObjectError::Malformed`] is returned.
pub fn decode_skeletal_mesh_native(
    data: &[u8],
    start: usize,
    has_vertex_colors: bool,
) -> ObjResult<SkeletalMeshNative> {
    let mut r = Reader::at(data, start)?;
    let bounds = BoxSphereBounds {
        origin: read_vec3(&mut r)?,
        box_extent: read_vec3(&mut r)?,
        sphere_radius: r.read_f32()?,
    };
    let materials = r.read_tarray("Materials", 4, |r| r.read_package_index())?;
    let origin = read_vec3(&mut r)?;
    let rot_origin = [r.read_i32()?, r.read_i32()?, r.read_i32()?];
    let ref_skeleton = read_array(&mut r, "RefSkeleton", MESH_BONE_SIZE, read_bone)?;
    let skeletal_depth = r.read_i32()?;
    let at = r.position();
    let lod_count = r.read_count("LOD models", LOD_MIN_SIZE)?;
    if lod_count > MAX_LODS {
        return Err(malformed(
            "LOD models",
            at,
            format!("{lod_count} LODs exceed the sanity limit {MAX_LODS}"),
        ));
    }
    let mut lods = Vec::with_capacity(lod_count);
    for _ in 0..lod_count {
        lods.push(read_skel_lod(&mut r, has_vertex_colors)?);
    }
    let name_index_map = read_array(&mut r, "NameIndexMap", 12, |r| {
        Ok((r.read_fname()?, r.read_i32()?))
    })?;
    let per_poly_bone_kdops = read_array(&mut r, "PerPolyBoneKDOPs", 24 + 16 + 4, read_per_poly)?;
    let bone_break_names = r.read_tarray("BoneBreakNames", 4, |r| r.read_fstring())?;
    let bone_break_options = read_u8_array(&mut r, "BoneBreakOptions")?;
    let clothing_assets = r.read_tarray("ClothingAssets", 4, |r| r.read_package_index())?;
    let cached_streaming_texture_factors =
        r.read_tarray("CachedStreamingTextureFactors", 4, |r| r.read_f32())?;
    let source_data = if read_u32_bool(&mut r, "bHaveSourceData")? {
        Some(Box::new(read_skel_lod(&mut r, has_vertex_colors)?))
    } else {
        None
    };
    if r.remaining() != 0 {
        return Err(malformed(
            "SkeletalMesh native data",
            r.position(),
            format!(
                "{} bytes left after the last known field (payload {} bytes)",
                r.remaining(),
                data.len()
            ),
        ));
    }
    Ok(SkeletalMeshNative {
        start,
        bounds,
        materials,
        origin,
        rot_origin,
        ref_skeleton,
        skeletal_depth,
        lods,
        name_index_map,
        per_poly_bone_kdops,
        bone_break_names,
        bone_break_options,
        clothing_assets,
        cached_streaming_texture_factors,
        source_data,
    })
}

/// True when export `index` is an `Engine.SkeletalMesh` (exactly).
pub fn is_skeletal_mesh(pkg: &Package, index: usize) -> bool {
    let Ok(class) = pkg.export_class_name(index) else {
        return false;
    };
    if class != "SkeletalMesh" {
        return false;
    }
    matches!(pkg.export_class_package(index), Ok(Some(p)) if p.eq_ignore_ascii_case("Engine"))
}

/// True when the object's tagged properties set `bHasVertexColors`.
pub fn has_vertex_colors(object: &DecodedObject) -> bool {
    object.properties.iter().any(|p| {
        p.name.eq_ignore_ascii_case("bHasVertexColors")
            && matches!(p.value, crate::Value::Bool(true))
    })
}

/// Decode export `index` as a `SkeletalMesh` (prelude, tags and native data).
pub fn decode_skeletal_mesh(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<SkeletalMesh> {
    if !is_skeletal_mesh(pkg, index) {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Engine.SkeletalMesh",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    }
    let object = decode_object(pkg, own_name, index, schema)?;
    let colors = has_vertex_colors(&object);
    let data = pkg.export_data(index)?;
    let native = decode_skeletal_mesh_native(data, object.properties_end, colors)?;
    Ok(SkeletalMesh {
        object,
        has_vertex_colors: colors,
        native,
    })
}

// ---------------------------------------------------------------------------
// Encoder (inverse of the decoder; synthetic fixtures and round-trip checks)
// ---------------------------------------------------------------------------

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

fn put_section(w: &mut Writer, s: &SkelSection) {
    w.u16(s.material_index);
    w.u16(s.chunk_index);
    w.u32(s.base_index);
    w.u32(s.num_triangles);
    w.u8(s.triangle_sorting);
}

fn put_multi_size(w: &mut Writer, m: &MultiSizeIndices) -> Option<()> {
    w.u32(m.needs_cpu_access);
    w.u8(m.data_type_size);
    put_bulk_header(w, usize::from(m.data_type_size), m.indices.len())?;
    for &i in &m.indices {
        match m.data_type_size {
            2 => w.u16(u16::try_from(i).ok()?),
            4 => w.u32(i),
            _ => return None,
        }
    }
    Some(())
}

fn put_uv4(w: &mut Writer, uvs: &[[f32; 2]; 4]) {
    for uv in uvs {
        w.u32(uv[0].to_bits());
        w.u32(uv[1].to_bits());
    }
}

fn put_chunk(w: &mut Writer, c: &SkelChunk) -> Option<()> {
    w.u32(c.base_vertex_index);
    put_count(w, c.rigid_vertices.len())?;
    for v in &c.rigid_vertices {
        put_vec3(w, v.position);
        for t in &v.tangents {
            w.bytes(&t.0);
        }
        put_uv4(w, &v.uvs);
        w.bytes(&v.color);
        w.u8(v.bone);
    }
    put_count(w, c.soft_vertices.len())?;
    for v in &c.soft_vertices {
        put_vec3(w, v.position);
        for t in &v.tangents {
            w.bytes(&t.0);
        }
        put_uv4(w, &v.uvs);
        w.bytes(&v.color);
        w.bytes(&v.influence_bones);
        w.bytes(&v.influence_weights);
    }
    put_count(w, c.bone_map.len())?;
    for &b in &c.bone_map {
        w.u16(b);
    }
    w.i32(c.num_rigid_vertices);
    w.i32(c.num_soft_vertices);
    w.i32(c.max_bone_influences);
    Some(())
}

fn put_vertex_buffer(w: &mut Writer, vb: &GpuSkinVertexBuffer) -> Option<()> {
    let elem = gpu_vertex_size(vb.num_tex_coords, vb.use_full_precision_uvs)?;
    w.u32(vb.num_tex_coords);
    w.u32(u32::from(vb.use_full_precision_uvs));
    w.u32(u32::from(vb.use_packed_position));
    put_vec3(w, vb.mesh_extension);
    put_vec3(w, vb.mesh_origin);
    put_bulk_header(w, elem, vb.vertices.len())?;
    let channels = usize::try_from(vb.num_tex_coords).ok()?;
    for v in &vb.vertices {
        w.bytes(&v.tangent_x.0);
        w.bytes(&v.tangent_z.0);
        w.bytes(&v.influence_bones);
        w.bytes(&v.influence_weights);
        put_vec3(w, v.position);
        for uv in v.uvs.iter().take(channels) {
            if vb.use_full_precision_uvs {
                w.u32(uv[0].to_bits());
                w.u32(uv[1].to_bits());
            } else {
                w.u16(f32_to_half(uv[0]));
                w.u16(f32_to_half(uv[1]));
            }
        }
    }
    Some(())
}

fn put_lod(w: &mut Writer, lod: &SkelLodModel, has_vertex_colors: bool) -> Option<()> {
    put_count(w, lod.sections.len())?;
    for s in &lod.sections {
        put_section(w, s);
    }
    put_multi_size(w, &lod.indices)?;
    put_count(w, lod.active_bone_indices.len())?;
    for &b in &lod.active_bone_indices {
        w.u16(b);
    }
    put_count(w, lod.chunks.len())?;
    for c in &lod.chunks {
        put_chunk(w, c)?;
    }
    w.u32(lod.size);
    w.u32(lod.num_vertices);
    put_count(w, lod.required_bones.len())?;
    w.bytes(&lod.required_bones);
    let rec = &lod.raw_point_indices_record;
    w.u32(rec.flags);
    w.i32(rec.element_count);
    w.i32(rec.size_on_disk);
    w.i32(rec.offset_in_file);
    if rec.has_inline_bytes() {
        let stored = usize::try_from(rec.size_on_disk).ok()?;
        if stored
            != lod
                .raw_point_indices
                .len()
                .checked_mul(RAW_POINT_INDEX_SIZE)?
        {
            return None;
        }
        for &v in &lod.raw_point_indices {
            w.u32(v);
        }
    }
    w.u32(lod.num_tex_coords);
    put_vertex_buffer(w, &lod.vertex_buffer)?;
    match (&lod.colors, has_vertex_colors) {
        (Some(colors), true) => {
            put_bulk_header(w, COLOR_SIZE, colors.len())?;
            for c in colors {
                w.bytes(c);
            }
        }
        (None, false) => {}
        _ => return None,
    }
    put_count(w, lod.vertex_influences.len())?;
    for vi in &lod.vertex_influences {
        put_count(w, vi.influences.len())?;
        for i in &vi.influences {
            w.bytes(&i.weights);
            w.bytes(&i.bones);
        }
        put_count(w, vi.mapping.len())?;
        for (key, verts) in &vi.mapping {
            w.i32(key[0]);
            w.i32(key[1]);
            put_count(w, verts.len())?;
            for &v in verts {
                w.u32(v);
            }
        }
        put_count(w, vi.sections.len())?;
        for s in &vi.sections {
            put_section(w, s);
        }
        put_count(w, vi.chunks.len())?;
        for c in &vi.chunks {
            put_chunk(w, c)?;
        }
        put_count(w, vi.required_bones.len())?;
        w.bytes(&vi.required_bones);
        w.u8(vi.usage);
    }
    put_multi_size(w, &lod.adjacency)
}

/// Serialize native data in the v868 layout: the exact inverse of
/// [`decode_skeletal_mesh_native`]. Returns `None` when a count does not fit
/// in an `i32`, an index does not fit its declared size, or the vertex
/// buffer / color settings and arrays disagree.
pub fn encode_skeletal_mesh_native(
    n: &SkeletalMeshNative,
    has_vertex_colors: bool,
) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    put_vec3(&mut w, n.bounds.origin);
    put_vec3(&mut w, n.bounds.box_extent);
    w.u32(n.bounds.sphere_radius.to_bits());
    put_count(&mut w, n.materials.len())?;
    for m in &n.materials {
        w.i32(m.0);
    }
    put_vec3(&mut w, n.origin);
    for a in n.rot_origin {
        w.i32(a);
    }
    put_count(&mut w, n.ref_skeleton.len())?;
    for b in &n.ref_skeleton {
        w.fname(b.name);
        w.u32(b.flags);
        for c in b.orientation {
            w.u32(c.to_bits());
        }
        put_vec3(&mut w, b.position);
        w.i32(b.num_children);
        w.i32(b.parent_index);
        w.bytes(&b.bone_color);
    }
    w.i32(n.skeletal_depth);
    put_count(&mut w, n.lods.len())?;
    for lod in &n.lods {
        put_lod(&mut w, lod, has_vertex_colors)?;
    }
    put_count(&mut w, n.name_index_map.len())?;
    for (name, idx) in &n.name_index_map {
        w.fname(*name);
        w.i32(*idx);
    }
    put_count(&mut w, n.per_poly_bone_kdops.len())?;
    for p in &n.per_poly_bone_kdops {
        put_vec3(&mut w, p.root_bounds.min);
        put_vec3(&mut w, p.root_bounds.max);
        put_bulk_header(&mut w, KDOP_NODE_SIZE, p.nodes.len())?;
        for node in &p.nodes {
            w.bytes(node);
        }
        put_bulk_header(&mut w, KDOP_TRIANGLE_SIZE, p.triangles.len())?;
        for t in &p.triangles {
            for &v in t {
                w.u16(v);
            }
        }
        put_count(&mut w, p.vertices.len())?;
        for v in &p.vertices {
            put_vec3(&mut w, *v);
        }
    }
    put_count(&mut w, n.bone_break_names.len())?;
    for s in &n.bone_break_names {
        if !w.fstring(s) {
            return None;
        }
    }
    put_count(&mut w, n.bone_break_options.len())?;
    w.bytes(&n.bone_break_options);
    put_count(&mut w, n.clothing_assets.len())?;
    for c in &n.clothing_assets {
        w.i32(c.0);
    }
    put_count(&mut w, n.cached_streaming_texture_factors.len())?;
    for f in &n.cached_streaming_texture_factors {
        w.u32(f.to_bits());
    }
    match &n.source_data {
        Some(src) => {
            w.u32(1);
            put_lod(&mut w, src, has_vertex_colors)?;
        }
        None => w.u32(0),
    }
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Structural validation
// ---------------------------------------------------------------------------

/// Context for [`validate_skeletal_mesh`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidationContext {
    /// Absolute offset of the export payload in the uncompressed stream
    /// (`SerialOffset`), used to cross-check inline bulk-data `OffsetInFile`.
    pub payload_stream_offset: Option<i64>,
    /// Number of imports, to range-check object references.
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

/// Depth of the reference skeleton (bones on the longest root-to-leaf path),
/// or `None` when a parent link is out of range or forms a cycle.
pub fn skeleton_depth(bones: &[MeshBone]) -> Option<usize> {
    let mut depth = vec![0usize; bones.len()];
    for (i, b) in bones.iter().enumerate() {
        if i == 0 {
            depth[0] = 1;
            continue;
        }
        let p = usize::try_from(b.parent_index).ok()?;
        if p >= i {
            return None;
        }
        depth[i] = depth.get(p)?.checked_add(1)?;
    }
    depth.iter().copied().max().or(Some(0))
}

const BOUNDS_TOLERANCE: f32 = 1e-2;

/// Structural cross-checks of a decoded mesh. Returns one message per problem
/// (empty when consistent). `bone_names` (resolved names, same order as the
/// skeleton) enables the name checks when provided.
pub fn validate_skeletal_mesh(
    native: &SkeletalMeshNative,
    bone_names: Option<&[String]>,
    ctx: &ValidationContext,
) -> Vec<String> {
    let mut issues = Vec::new();
    for (i, m) in native.materials.iter().enumerate() {
        if !ref_in_range(*m, ctx) {
            issues.push(format!("material {i} reference {} out of range", m.0));
        }
    }
    for (i, m) in native.clothing_assets.iter().enumerate() {
        if !ref_in_range(*m, ctx) {
            issues.push(format!("clothing asset {i} reference {} out of range", m.0));
        }
    }
    let nb = native.ref_skeleton.len();
    if nb == 0 {
        issues.push("empty reference skeleton".to_owned());
    }
    if let Some(root) = native.ref_skeleton.first()
        && root.parent_index != 0
    {
        issues.push(format!("root parent index {}", root.parent_index));
    }
    let mut children = vec![0i64; nb];
    for (i, b) in native.ref_skeleton.iter().enumerate().skip(1) {
        match usize::try_from(b.parent_index) {
            Ok(p) if p < i => children[p] += 1,
            _ => issues.push(format!(
                "bone {i} parent {} is not an earlier bone",
                b.parent_index
            )),
        }
    }
    for (i, b) in native.ref_skeleton.iter().enumerate() {
        if i64::from(b.num_children) != children[i] {
            issues.push(format!(
                "bone {i} NumChildren {} but {} children link to it",
                b.num_children, children[i]
            ));
        }
        let q = b.orientation;
        let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        if !(len - 1.0).abs().lt(&1e-3) {
            issues.push(format!("bone {i} orientation length {len}"));
        }
        if b.position.iter().any(|c| !c.is_finite()) {
            issues.push(format!("bone {i} position not finite"));
        }
    }
    match skeleton_depth(&native.ref_skeleton) {
        Some(d) if i64::try_from(d).ok() == Some(i64::from(native.skeletal_depth)) => {}
        d => issues.push(format!(
            "SkeletalDepth {} but the skeleton is {d:?} bones deep",
            native.skeletal_depth
        )),
    }
    if !native.name_index_map.is_empty() {
        if native.name_index_map.len() != nb {
            issues.push(format!(
                "NameIndexMap has {} entries for {nb} bones",
                native.name_index_map.len()
            ));
        }
        for (name, idx) in &native.name_index_map {
            let ok = usize::try_from(*idx)
                .ok()
                .and_then(|i| native.ref_skeleton.get(i))
                .is_some_and(|b| b.name == *name);
            if !ok {
                issues.push(format!(
                    "NameIndexMap entry -> {idx} does not name that bone"
                ));
                break;
            }
        }
    }
    if let Some(names) = bone_names {
        let mut seen = std::collections::BTreeSet::new();
        for n in names {
            if !seen.insert(n.to_ascii_lowercase()) {
                issues.push(format!("duplicate bone name {n}"));
            }
        }
    }
    if native.origin.iter().any(|c| !c.is_finite()) {
        issues.push("Origin not finite".to_owned());
    }
    if native.lods.is_empty() {
        issues.push("no LOD models".to_owned());
    }
    for (li, lod) in native.lods.iter().enumerate() {
        let label = format!("LOD {li}");
        validate_lod(lod, &label, native, li == 0, ctx, &mut issues);
    }
    if let Some(src) = &native.source_data {
        validate_lod(src, "source data", native, false, ctx, &mut issues);
    }
    for (pi, p) in native.per_poly_bone_kdops.iter().enumerate() {
        let nv = p.vertices.len();
        if p.triangles
            .iter()
            .any(|t| t[..3].iter().any(|&v| usize::from(v) >= nv))
        {
            issues.push(format!(
                "per-poly collision {pi}: triangle vertex out of range"
            ));
        }
    }
    issues
}

fn validate_indices(
    m: &MultiSizeIndices,
    nv: u32,
    label: &str,
    what: &str,
    push: &mut impl FnMut(String),
) {
    if m.data_type_size == 2 && m.indices.iter().any(|&i| i > u32::from(u16::MAX)) {
        push(format!("{label}: {what} index exceeds 16 bits"));
    }
    if let Some(&bad) = m.indices.iter().find(|&&i| i >= nv) {
        push(format!("{label}: {what} index {bad} >= {nv} vertices"));
    }
}

fn validate_lod(
    lod: &SkelLodModel,
    label: &str,
    mesh: &SkeletalMeshNative,
    check_bounds: bool,
    ctx: &ValidationContext,
    issues: &mut Vec<String>,
) {
    let mut push = |m: String| issues.push(format!("{label}: {m}"));
    let nb = mesh.ref_skeleton.len();
    let nv = lod.num_vertices;
    let vb = &lod.vertex_buffer;
    if u32::try_from(vb.vertices.len()).ok() != Some(nv) {
        push(format!(
            "vertex buffer has {} vertices, NumVertices {nv}",
            vb.vertices.len()
        ));
    }
    if vb.num_tex_coords != lod.num_tex_coords {
        push(format!(
            "vertex buffer NumTexCoords {} != LOD NumTexCoords {}",
            vb.num_tex_coords, lod.num_tex_coords
        ));
    }
    if let Some(colors) = &lod.colors
        && u32::try_from(colors.len()).ok() != Some(nv)
    {
        push(format!("{} colors for {nv} vertices", colors.len()));
    }
    validate_indices(&lod.indices, nv, "", "index buffer", &mut push);
    validate_indices(&lod.adjacency, nv, "", "adjacency", &mut push);
    let tris = lod.triangle_count();
    if !lod.adjacency.indices.is_empty()
        && u64::try_from(lod.adjacency.indices.len()).ok() != tris.checked_mul(12)
    {
        push(format!(
            "adjacency index count {} != 12 x {tris} triangles",
            lod.adjacency.indices.len()
        ));
    }
    // Sections: inside the index buffer, covering it, valid chunk/material.
    let mut covered = 0u64;
    for (si, s) in lod.sections.iter().enumerate() {
        if usize::from(s.chunk_index) >= lod.chunks.len() {
            push(format!(
                "section {si} chunk {} >= {} chunks",
                s.chunk_index,
                lod.chunks.len()
            ));
        }
        if usize::from(s.material_index) >= mesh.materials.len().max(1) {
            push(format!(
                "section {si} material {} >= {} materials",
                s.material_index,
                mesh.materials.len()
            ));
        }
        let first = u64::from(s.base_index);
        let count = u64::from(s.num_triangles) * 3;
        let end = first + count;
        if end > u64::try_from(lod.indices.indices.len()).unwrap_or(u64::MAX) {
            push(format!(
                "section {si} indices {first}..{end} exceed {} indices",
                lod.indices.indices.len()
            ));
            continue;
        }
        covered = covered.saturating_add(count);
        // Every vertex of the section lies in its chunk.
        if let Some(chunk) = lod.chunks.get(usize::from(s.chunk_index))
            && let Some(n) = chunk.vertex_count()
        {
            let lo = u64::from(chunk.base_vertex_index);
            let hi = lo + u64::from(n);
            let range = usize::try_from(first).unwrap_or(0)..usize::try_from(end).unwrap_or(0);
            if let Some(slice) = lod.indices.indices.get(range)
                && slice
                    .iter()
                    .any(|&i| u64::from(i) < lo || u64::from(i) >= hi)
            {
                push(format!(
                    "section {si} references vertices outside chunk {}",
                    s.chunk_index
                ));
            }
        }
    }
    if covered != u64::try_from(lod.indices.indices.len()).unwrap_or(u64::MAX) {
        push(format!(
            "sections cover {covered} of {} indices",
            lod.indices.indices.len()
        ));
    }
    // Chunks: contiguous vertex ranges covering the buffer.
    let mut next = 0u64;
    for (ci, c) in lod.chunks.iter().enumerate() {
        if u64::from(c.base_vertex_index) != next {
            push(format!(
                "chunk {ci} starts at vertex {} (expected {next})",
                c.base_vertex_index
            ));
        }
        match c.vertex_count() {
            Some(n) => next = u64::from(c.base_vertex_index) + u64::from(n),
            None => push(format!(
                "chunk {ci} vertex counts {} + {}",
                c.num_rigid_vertices, c.num_soft_vertices
            )),
        }
        if !(1..=4).contains(&c.max_bone_influences) {
            push(format!(
                "chunk {ci} MaxBoneInfluences {}",
                c.max_bone_influences
            ));
        }
        if let Some(&bad) = c.bone_map.iter().find(|&&b| usize::from(b) >= nb) {
            push(format!("chunk {ci} bone map entry {bad} >= {nb} bones"));
        }
        let rigid_ok = c.rigid_vertices.is_empty()
            || usize::try_from(c.num_rigid_vertices).ok() == Some(c.rigid_vertices.len());
        let soft_ok = c.soft_vertices.is_empty()
            || usize::try_from(c.num_soft_vertices).ok() == Some(c.soft_vertices.len());
        if !rigid_ok || !soft_ok {
            push(format!(
                "chunk {ci} stores {} rigid / {} soft vertices for counts {} / {}",
                c.rigid_vertices.len(),
                c.soft_vertices.len(),
                c.num_rigid_vertices,
                c.num_soft_vertices
            ));
        }
        // GPU vertices of the chunk use bones inside its bone map.
        if let Some(n) = c.vertex_count() {
            let lo = usize::try_from(c.base_vertex_index).unwrap_or(usize::MAX);
            let hi = lo.saturating_add(usize::try_from(n).unwrap_or(0));
            let nmap = c.bone_map.len();
            if let Some(slice) = vb.vertices.get(lo..hi.min(vb.vertices.len())) {
                for v in slice {
                    let bad = (0..MAX_INFLUENCES).any(|k| {
                        v.influence_weights[k] != 0 && usize::from(v.influence_bones[k]) >= nmap
                    });
                    if bad {
                        push(format!(
                            "chunk {ci} vertex uses a bone outside its bone map"
                        ));
                        break;
                    }
                }
            }
        }
    }
    if next != u64::from(nv) {
        push(format!("chunks cover {next} of {nv} vertices"));
    }
    if let Some(&bad) = lod
        .active_bone_indices
        .iter()
        .find(|&&b| usize::from(b) >= nb)
    {
        push(format!("active bone {bad} >= {nb} bones"));
    }
    if let Some(&bad) = lod.required_bones.iter().find(|&&b| usize::from(b) >= nb) {
        push(format!("required bone {bad} >= {nb} bones"));
    }
    let rec = &lod.raw_point_indices_record;
    if rec.storage() == BulkStorage::Inline
        && rec.compression() == BulkCompression::None
        && let Some(base) = ctx.payload_stream_offset
        && !rec.inline_offset_matches(base)
    {
        push(format!(
            "RawPointIndices OffsetInFile {} is not the payload's stream position",
            rec.offset_in_file
        ));
    }
    if check_bounds {
        let b = &mesh.bounds;
        let tol: [f32; 3] = std::array::from_fn(|k| {
            BOUNDS_TOLERANCE + 1e-5 * (b.origin[k].abs() + b.box_extent[k].abs())
        });
        let outside = vb.vertices.iter().any(|v| {
            let p = v.position;
            (0..3).any(|k| {
                !(p[k] >= b.origin[k] - b.box_extent[k] - tol[k]
                    && p[k] <= b.origin[k] + b.box_extent[k] + tol[k])
            })
        });
        if outside {
            push("a reference-pose position lies outside the bounds box".to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage over a package
// ---------------------------------------------------------------------------

/// Most failure samples kept per package.
const MAX_FAILURE_SAMPLES: usize = 16;

/// SkeletalMesh coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SkeletalCoverage {
    /// Package name.
    pub package: String,
    /// `SkeletalMesh` exports found.
    pub total: usize,
    /// Exports whose native data decoded and ended exactly at `SerialSize`.
    pub exact: usize,
    /// Exports that also passed every structural cross-check.
    pub valid: usize,
    /// Exports whose decoded native data re-encodes to the original bytes.
    pub round_trip: usize,
    /// Native tail bytes consumed (sum over exact exports).
    pub native_bytes: u64,
    /// LOD models per mesh -> mesh count.
    pub lod_histogram: BTreeMap<usize, usize>,
    /// Bones over all meshes.
    pub bones: usize,
    /// Sections over all LODs.
    pub sections: usize,
    /// Chunks over all LODs.
    pub chunks: usize,
    /// Vertices over all LODs.
    pub vertices: u64,
    /// Triangles over all LODs.
    pub triangles: u64,
    /// Meshes with vertex colors.
    pub with_colors: usize,
    /// UV channel count -> LOD count.
    pub uv_channels: BTreeMap<u32, usize>,
    /// LODs with full-precision UVs.
    pub full_precision_uv_lods: usize,
    /// LODs whose vertex buffer has `bUsePackedPosition` set (stored only).
    pub packed_position_flag_lods: usize,
    /// Index size (2/4) -> LOD count.
    pub index_sizes: BTreeMap<u8, usize>,
    /// LODs with an adjacency buffer.
    pub with_adjacency: usize,
    /// Chunks storing rigid or soft source vertices.
    pub chunks_with_source_vertices: usize,
    /// LODs with alternative vertex influences.
    pub with_vertex_influences: usize,
    /// Meshes with per-poly bone collision.
    pub with_per_poly: usize,
    /// Meshes with source data.
    pub with_source_data: usize,
    /// First decode failures (`export: error`).
    pub failures: Vec<String>,
    /// First validation issues (`export: issue`).
    pub issues: Vec<String>,
}

/// Decode every `SkeletalMesh` export of `lp` and gather statistics.
pub fn skeletal_coverage(lp: &LoadedPackage, schema: &dyn Schema) -> SkeletalCoverage {
    let pkg = &lp.package;
    let mut cov = SkeletalCoverage {
        package: lp.name.clone(),
        ..SkeletalCoverage::default()
    };
    for i in 0..pkg.exports.len() {
        if !is_skeletal_mesh(pkg, i) {
            continue;
        }
        cov.total += 1;
        let mesh = match decode_skeletal_mesh(pkg, Some(&lp.name), i, schema) {
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
        if original.is_some()
            && encode_skeletal_mesh_native(n, mesh.has_vertex_colors).as_deref() == original
        {
            cov.round_trip += 1;
        }
        cov.native_bytes += u64::try_from(mesh.object.native_tail()).unwrap_or(0);
        let ctx = ValidationContext {
            payload_stream_offset: pkg.export(i).ok().map(|e| i64::from(e.serial_offset)),
            imports: pkg.imports.len(),
            exports: pkg.exports.len(),
        };
        let names = bone_names(pkg, n);
        let issues = validate_skeletal_mesh(n, Some(&names), &ctx);
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
        cov.bones += n.ref_skeleton.len();
        if mesh.has_vertex_colors {
            cov.with_colors += 1;
        }
        if !n.per_poly_bone_kdops.is_empty() {
            cov.with_per_poly += 1;
        }
        if n.source_data.is_some() {
            cov.with_source_data += 1;
        }
        for lod in &n.lods {
            cov.sections += lod.sections.len();
            cov.chunks += lod.chunks.len();
            cov.vertices += u64::from(lod.num_vertices);
            cov.triangles += lod.triangle_count();
            *cov.uv_channels
                .entry(lod.vertex_buffer.num_tex_coords)
                .or_insert(0) += 1;
            if lod.vertex_buffer.use_full_precision_uvs {
                cov.full_precision_uv_lods += 1;
            }
            if lod.vertex_buffer.use_packed_position {
                cov.packed_position_flag_lods += 1;
            }
            *cov.index_sizes
                .entry(lod.indices.data_type_size)
                .or_insert(0) += 1;
            if !lod.adjacency.indices.is_empty() {
                cov.with_adjacency += 1;
            }
            cov.chunks_with_source_vertices += lod
                .chunks
                .iter()
                .filter(|c| !c.rigid_vertices.is_empty() || !c.soft_vertices.is_empty())
                .count();
            if !lod.vertex_influences.is_empty() {
                cov.with_vertex_influences += 1;
            }
        }
    }
    cov
}
