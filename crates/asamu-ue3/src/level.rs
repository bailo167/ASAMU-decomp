//! Level (map) scene extraction: the `ULevel` native data (actor list, URL,
//! BSP model, cached physics and precomputed lighting blocks) and per-map
//! scene descriptions built from it (see
//! `docs/reverse-engineering/LEVEL_FORMAT.md`).
//!
//! `ULevel` layout after the object prelude and tagged properties, CONFIRMED
//! by exact consumption of every map's `PersistentLevel` (the native
//! serializers were read locally in the original executable first):
//!
//! ```text
//! obj Actors.Owner | array<obj> Actors | FURL (4 FStrings, array<FString> Op, i32 Port, i32 Valid)
//! obj Model | array<obj> ModelComponents | array<obj> GameSequences
//! map<obj, array<20-byte texture instance>> TextureToInstancesMap
//! map<obj, array<32-byte dynamic texture instance>> DynamicTextureInstances
//! i32 N + N bytes (a block the loader skips)
//! bulk<u8> CachedPhysBSPData | map<obj, (vec3, i32)> CachedPhysSMDataMap
//! array<array<bulk<u8>>> CachedPhysSMDataStore | map<obj, (vec3, i32)> CachedPhysPerTriSMDataMap
//! array<bulk<u8>> CachedPhysPerTriSMDataStore | i32 CachedPhysBSPDataVersion | i32 CachedPhysSMDataVersion
//! map<obj, u32> ForceStreamTextures | array<bulk<u8>> CachedPhysConvexBSPData | i32 CachedPhysConvexBSPVersion
//! obj NavListStart, NavListEnd, CoverListStart, CoverListEnd, PylonListStart, PylonListEnd
//! array<(guid, i32)> CrossLevelCoverGuidRefs | array<obj> CoverLinkRefs | array<(i32, u8)> CoverIndexPairs
//! array<obj> CrossLevelActors
//! FPrecomputedLightVolume       u32 bInitialized [FBox, f32, array<33-byte sample>]
//! FPrecomputedVisibilityHandler vec2, i32 x4, array<bucket {i32, array<16-byte cell>,
//!                               array<chunk {u32, i32, array<u8>}>}>
//! FPrecomputedVolumeDistanceField f32, FBox, i32 x3, array<FColor>
//! ```
//!
//! A [`Scene`] lists every actor of `ULevel::Actors` with its transform,
//! collision flags, tags, components (static meshes with material overrides,
//! lights, cylinders, brushes), gameplay parameters, Matinee references and,
//! for brushes and volumes, world-space geometry. Values are the object's own
//! tagged properties merged over its archetype (or class defaults), the same
//! delta rule the engine uses. Scenes are original game data: keep them in
//! user-local output only; publish counts and structure, never scene dumps.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::TAU;
use std::sync::Arc;

use serde::Serialize;

use crate::bsp::{
    self, BoxSphereBounds, Model, ModelCheck, TriangleMesh, Vec3, decode_with_tail, read_array,
    read_vec3,
};
use crate::error::Ue3Error;
use crate::model::{LoadedPackage, PackageSet};
use crate::object::{DecodedObject, ObjResult, ObjectError};
use crate::package::Package;
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::schema::{Schema, last_component};
use crate::types::PackageIndex;

/// `format` field of the scene JSON.
pub const SCENE_FORMAT: &str = "asamu-scene";
/// `version` field of the scene JSON (bumped on incompatible changes).
pub const SCENE_VERSION: u32 = 1;
/// Longest archetype chain followed when merging values.
pub const MAX_ARCHETYPE_DEPTH: usize = 16;
/// Most warnings kept per scene (the rest are counted).
pub const MAX_SCENE_WARNINGS: usize = 512;
/// Serialized `FStreamableTextureInstance` size.
pub const STREAMABLE_TEXTURE_INSTANCE_SIZE: usize = 20;
/// Serialized `FDynamicTextureInstance` size.
pub const DYNAMIC_TEXTURE_INSTANCE_SIZE: usize = 32;
/// Serialized `FVolumeLightingSample` size (v868).
pub const VOLUME_LIGHTING_SAMPLE_SIZE: usize = 33;

/// A row-vector transform (UE3 `FMatrix` convention: `p' = p · M`, row 3 is
/// the translation).
pub type Mat4 = [[f32; 4]; 4];

/// Identity transform.
pub const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

// ------------------------------------------------------------ native tail

/// `FURL`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Url {
    /// Protocol.
    pub protocol: String,
    /// Host.
    pub host: String,
    /// Map.
    pub map: String,
    /// Portal.
    pub portal: String,
    /// Options.
    pub options: Vec<String>,
    /// Port.
    pub port: i32,
    /// Valid flag.
    pub valid: i32,
}

/// Entry and value counts of a serialized map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct MapCount {
    /// Keys.
    pub entries: usize,
    /// Values summed over all keys (for array-valued maps).
    pub values: usize,
}

/// `FPrecomputedLightVolume` summary.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct LightVolumeInfo {
    /// Bounds minimum.
    pub bounds_min: Vec3,
    /// Bounds maximum.
    pub bounds_max: Vec3,
    /// Bounds valid flag.
    pub bounds_valid: u8,
    /// The `f32` after the bounds (UE3: sample spacing; 0 in every shipped map).
    pub spacing: f32,
    /// Volume lighting samples.
    pub samples: usize,
}

/// `FPrecomputedVisibilityHandler` summary.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct VisibilityInfo {
    /// Cell bucket origin (XY).
    pub cell_bucket_origin: [f32; 2],
    /// `CellSizeXY`, `CellSizeZ`, `CellBucketSizeXY`, `NumCellBuckets`.
    pub sizes: [i32; 4],
    /// Buckets.
    pub buckets: usize,
    /// Cells over all buckets.
    pub cells: usize,
    /// Compressed chunks over all buckets.
    pub chunks: usize,
    /// Chunk data bytes.
    pub chunk_bytes: usize,
}

/// `FPrecomputedVolumeDistanceField` summary.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct DistanceFieldInfo {
    /// Maximum distance.
    pub max_distance: f32,
    /// Bounds minimum.
    pub bounds_min: Vec3,
    /// Bounds maximum.
    pub bounds_max: Vec3,
    /// Bounds valid flag.
    pub bounds_valid: u8,
    /// Volume size in voxels.
    pub size: [i32; 3],
    /// Stored voxels.
    pub voxels: usize,
}

/// Decoded `ULevel` native data. Bulk payloads (cooked physics, lighting)
/// are counted, not kept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LevelTail {
    /// Owner of the actor array (the level itself).
    pub actors_owner: PackageIndex,
    /// `Actors` (null entries are deleted actors).
    pub actors: Vec<PackageIndex>,
    /// `URL`.
    pub url: Url,
    /// BSP model.
    pub model: PackageIndex,
    /// Model components.
    pub model_components: Vec<PackageIndex>,
    /// Kismet root sequences.
    pub game_sequences: Vec<PackageIndex>,
    /// `TextureToInstancesMap`.
    pub texture_to_instances: MapCount,
    /// `DynamicTextureInstances`.
    pub dynamic_texture_instances: MapCount,
    /// Size of the block the loader skips.
    pub skipped_block_bytes: usize,
    /// `CachedPhysBSPData` bytes.
    pub cached_phys_bsp_bytes: usize,
    /// `CachedPhysSMDataMap` entries.
    pub cached_phys_sm_map: usize,
    /// `CachedPhysSMDataStore` (convex data sets, elements).
    pub cached_phys_sm_store: MapCount,
    /// `CachedPhysPerTriSMDataMap` entries.
    pub cached_phys_per_tri_map: usize,
    /// `CachedPhysPerTriSMDataStore` entries.
    pub cached_phys_per_tri_store: usize,
    /// `CachedPhysBSPDataVersion`.
    pub cached_phys_bsp_version: i32,
    /// `CachedPhysSMDataVersion`.
    pub cached_phys_sm_version: i32,
    /// `ForceStreamTextures` entries.
    pub force_stream_textures: usize,
    /// `CachedPhysConvexBSPData` elements.
    pub cached_phys_convex_bsp: usize,
    /// `CachedPhysConvexBSPVersion`.
    pub cached_phys_convex_bsp_version: i32,
    /// Navigation / cover / pylon list heads and tails.
    pub nav_cover_pylon: [PackageIndex; 6],
    /// `CrossLevelCoverGuidRefs` entries.
    pub cross_level_cover_guid_refs: usize,
    /// `CoverLinkRefs`.
    pub cover_link_refs: Vec<PackageIndex>,
    /// `CoverIndexPairs` entries.
    pub cover_index_pairs: usize,
    /// `CrossLevelActors`.
    pub cross_level_actors: Vec<PackageIndex>,
    /// Precomputed light volume (when initialized).
    pub light_volume: Option<LightVolumeInfo>,
    /// Precomputed visibility.
    pub visibility: VisibilityInfo,
    /// Precomputed volume distance field.
    pub distance_field: DistanceFieldInfo,
}

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

/// Skip a bulk array (`i32 ElementSize`, `i32 Count`, data) whose element
/// size must be one of `sizes`; returns the element count.
fn skip_bulk(r: &mut Reader<'_>, what: &'static str, sizes: &[usize]) -> ObjResult<usize> {
    let at = r.position();
    let raw = r.read_i32()?;
    let elem = usize::try_from(raw)
        .ok()
        .filter(|s| sizes.contains(s))
        .ok_or_else(|| malformed(what, at, format!("bulk element size {raw}")))?;
    let count = r.read_count(what, elem)?;
    r.skip(count.saturating_mul(elem))?;
    Ok(count)
}

/// Skip an array of fixed-size elements; returns the count.
fn skip_array(r: &mut Reader<'_>, what: &'static str, size: usize) -> ObjResult<usize> {
    let count = r.read_count(what, size)?;
    r.skip(count.saturating_mul(size))?;
    Ok(count)
}

fn read_obj_array(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<PackageIndex>> {
    read_array(r, what, 4, |r| Ok(r.read_package_index()?))
}

fn read_url(r: &mut Reader<'_>) -> ObjResult<Url> {
    Ok(Url {
        protocol: r.read_fstring()?,
        host: r.read_fstring()?,
        map: r.read_fstring()?,
        portal: r.read_fstring()?,
        options: read_array(r, "URL.Op", 4, |r| Ok(r.read_fstring()?))?,
        port: r.read_i32()?,
        valid: r.read_i32()?,
    })
}

/// `map<obj, array<elem>>`.
fn skip_instance_map(r: &mut Reader<'_>, what: &'static str, elem: usize) -> ObjResult<MapCount> {
    let entries = r.read_count(what, 8)?;
    let mut values = 0usize;
    for _ in 0..entries {
        r.read_package_index()?;
        values = values.saturating_add(skip_array(r, what, elem)?);
    }
    Ok(MapCount { entries, values })
}

fn read_box(r: &mut Reader<'_>) -> Result<(Vec3, Vec3, u8), Ue3Error> {
    Ok((read_vec3(r)?, read_vec3(r)?, r.read_u8()?))
}

/// Read the `ULevel` native data that follows the tagged properties.
pub fn read_level_tail(r: &mut Reader<'_>) -> ObjResult<LevelTail> {
    let actors_owner = r.read_package_index()?;
    let actors = read_obj_array(r, "Level.Actors")?;
    let url = read_url(r)?;
    let model = r.read_package_index()?;
    let model_components = read_obj_array(r, "Level.ModelComponents")?;
    let game_sequences = read_obj_array(r, "Level.GameSequences")?;
    let texture_to_instances = skip_instance_map(
        r,
        "Level.TextureToInstancesMap",
        STREAMABLE_TEXTURE_INSTANCE_SIZE,
    )?;
    let dynamic_texture_instances = skip_instance_map(
        r,
        "Level.DynamicTextureInstances",
        DYNAMIC_TEXTURE_INSTANCE_SIZE,
    )?;
    let skipped = r.read_non_negative("Level.SkippedBlock")?;
    let skipped_block_bytes = usize::try_from(skipped).unwrap_or(usize::MAX);
    r.skip(skipped_block_bytes)?;
    let cached_phys_bsp_bytes = skip_bulk(r, "Level.CachedPhysBSPData", &[1])?;
    let cached_phys_sm_map = skip_array(r, "Level.CachedPhysSMDataMap", 20)?;
    let mut sm_store = MapCount {
        entries: r.read_count("Level.CachedPhysSMDataStore", 4)?,
        values: 0,
    };
    for _ in 0..sm_store.entries {
        let n = r.read_count("CachedPhysSMDataStore.Elements", 8)?;
        for _ in 0..n {
            skip_bulk(r, "CachedPhysSMDataStore.Element", &[1])?;
        }
        sm_store.values = sm_store.values.saturating_add(n);
    }
    let cached_phys_per_tri_map = skip_array(r, "Level.CachedPhysPerTriSMDataMap", 20)?;
    let cached_phys_per_tri_store = r.read_count("Level.CachedPhysPerTriSMDataStore", 8)?;
    for _ in 0..cached_phys_per_tri_store {
        skip_bulk(r, "CachedPhysPerTriSMDataStore.Element", &[1])?;
    }
    let cached_phys_bsp_version = r.read_i32()?;
    let cached_phys_sm_version = r.read_i32()?;
    let force_stream_textures = skip_array(r, "Level.ForceStreamTextures", 8)?;
    let cached_phys_convex_bsp = r.read_count("Level.CachedPhysConvexBSPData", 8)?;
    for _ in 0..cached_phys_convex_bsp {
        skip_bulk(r, "CachedPhysConvexBSPData.Element", &[1])?;
    }
    let cached_phys_convex_bsp_version = r.read_i32()?;
    let mut nav_cover_pylon = [PackageIndex::NULL; 6];
    for slot in &mut nav_cover_pylon {
        *slot = r.read_package_index()?;
    }
    let cross_level_cover_guid_refs = skip_array(r, "Level.CrossLevelCoverGuidRefs", 20)?;
    let cover_link_refs = read_obj_array(r, "Level.CoverLinkRefs")?;
    let cover_index_pairs = skip_array(r, "Level.CoverIndexPairs", 5)?;
    let cross_level_actors = read_obj_array(r, "Level.CrossLevelActors")?;
    let light_volume = if r.read_u32()? != 0 {
        let (bounds_min, bounds_max, bounds_valid) = read_box(r)?;
        let spacing = r.read_f32()?;
        let samples = skip_array(r, "LightVolume.Samples", VOLUME_LIGHTING_SAMPLE_SIZE)?;
        Some(LightVolumeInfo {
            bounds_min,
            bounds_max,
            bounds_valid,
            spacing,
            samples,
        })
    } else {
        None
    };
    let mut visibility = VisibilityInfo {
        cell_bucket_origin: [r.read_f32()?, r.read_f32()?],
        ..VisibilityInfo::default()
    };
    for s in &mut visibility.sizes {
        *s = r.read_i32()?;
    }
    visibility.buckets = r.read_count("Visibility.Buckets", 12)?;
    for _ in 0..visibility.buckets {
        r.read_i32()?;
        let cells = skip_array(r, "Visibility.Cells", 16)?;
        visibility.cells = visibility.cells.saturating_add(cells);
        let chunks = r.read_count("Visibility.Chunks", 12)?;
        for _ in 0..chunks {
            r.read_u32()?;
            r.read_i32()?;
            let n = skip_array(r, "Visibility.ChunkData", 1)?;
            visibility.chunk_bytes = visibility.chunk_bytes.saturating_add(n);
        }
        visibility.chunks = visibility.chunks.saturating_add(chunks);
    }
    let max_distance = r.read_f32()?;
    let (bounds_min, bounds_max, bounds_valid) = read_box(r)?;
    let size = [r.read_i32()?, r.read_i32()?, r.read_i32()?];
    let voxels = skip_array(r, "DistanceField.Data", 4)?;
    Ok(LevelTail {
        actors_owner,
        actors,
        url,
        model,
        model_components,
        game_sequences,
        texture_to_instances,
        dynamic_texture_instances,
        skipped_block_bytes,
        cached_phys_bsp_bytes,
        cached_phys_sm_map,
        cached_phys_sm_store: sm_store,
        cached_phys_per_tri_map,
        cached_phys_per_tri_store,
        cached_phys_bsp_version,
        cached_phys_sm_version,
        force_stream_textures,
        cached_phys_convex_bsp,
        cached_phys_convex_bsp_version,
        nav_cover_pylon,
        cross_level_cover_guid_refs,
        cover_link_refs,
        cover_index_pairs,
        cross_level_actors,
        light_volume,
        visibility,
        distance_field: DistanceFieldInfo {
            max_distance,
            bounds_min,
            bounds_max,
            bounds_valid,
            size,
            voxels,
        },
    })
}

/// Decode a `Level` export strictly (exact consumption).
pub fn decode_level(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<(DecodedObject, LevelTail)> {
    decode_with_tail(pkg, own_name, index, schema, "Level", read_level_tail)
}

/// Exports of class `Level` in `pkg`.
pub fn level_exports(pkg: &Package) -> Vec<usize> {
    (0..pkg.exports.len())
        .filter(|&i| pkg.export_class_name(i).is_ok_and(|c| c == "Level"))
        .collect()
}

// ------------------------------------------------------------ transforms

/// Sine and cosine of a rotator angle as the native transform code computes
/// them: a 16384-entry table indexed by `(angle >> 2) & 0x3FFF` (CONFIRMED in
/// `AActor::LocalToWorld`), cosine read a quarter turn later. The table holds
/// `sin(i · 2π / 16384)` (TENTATIVE: UE3 convention; filled at run time).
pub fn rotator_sin_cos(units: i32) -> (f64, f64) {
    let entry = |u: u32| f64::from((u >> 2) & 0x3FFF) * TAU / 16384.0;
    let bits = units as u32;
    (entry(bits).sin(), entry(bits.wrapping_add(0x4000)).sin())
}

/// Rotation × scale rows (`FRotationMatrix` scaled per axis), f64.
fn rotation_rows(rotation: [i32; 3], scale: [f64; 3]) -> [[f64; 3]; 3] {
    let (sp, cp) = rotator_sin_cos(rotation[0]);
    let (sy, cy) = rotator_sin_cos(rotation[1]);
    let (sr, cr) = rotator_sin_cos(rotation[2]);
    [
        [cp * cy * scale[0], cp * sy * scale[0], sp * scale[0]],
        [
            (sr * sp * cy - cr * sy) * scale[1],
            (sr * sp * sy + cr * cy) * scale[1],
            -sr * cp * scale[1],
        ],
        [
            -(cr * sp * cy + sr * sy) * scale[2],
            (cy * sr - cr * sp * sy) * scale[2],
            cr * cp * scale[2],
        ],
    ]
}

fn mat_from_rows(rows: [[f64; 3]; 3], t: [f64; 3]) -> Mat4 {
    let mut m = IDENTITY;
    for (k, row) in rows.iter().enumerate() {
        m[k] = [row[0] as f32, row[1] as f32, row[2] as f32, 0.0];
    }
    m[3] = [t[0] as f32, t[1] as f32, t[2] as f32, 1.0];
    m
}

fn f64v(v: Vec3) -> [f64; 3] {
    [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
}

/// `AActor::LocalToWorld` (CONFIRMED from native code):
/// `world = R(S · (local − PrePivot)) + Location` with
/// `S = DrawScale · DrawScale3D`, as a row-vector matrix.
pub fn actor_local_to_world(
    location: Vec3,
    rotation: [i32; 3],
    draw_scale: f32,
    draw_scale3d: Vec3,
    pre_pivot: Vec3,
) -> Mat4 {
    let ds = f64::from(draw_scale);
    let s = f64v(draw_scale3d);
    let rows = rotation_rows(rotation, [s[0] * ds, s[1] * ds, s[2] * ds]);
    let pp = f64v(pre_pivot);
    let loc = f64v(location);
    let mut t = loc;
    for (k, tk) in t.iter_mut().enumerate() {
        *tk -= pp[0] * rows[0][k] + pp[1] * rows[1][k] + pp[2] * rows[2][k];
    }
    mat_from_rows(rows, t)
}

/// Product `a · b` (apply `a`, then `b`).
pub fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let mut s = 0.0f64;
            for k in 0..4 {
                s += f64::from(a[i][k]) * f64::from(b[k][j]);
            }
            *cell = s as f32;
        }
    }
    out
}

/// Transform a point (`p · M`).
pub fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        let s = f64::from(p[0]) * f64::from(m[0][j])
            + f64::from(p[1]) * f64::from(m[1][j])
            + f64::from(p[2]) * f64::from(m[2][j])
            + f64::from(m[3][j]);
        *o = s as f32;
    }
    out
}

/// Component placement relative to its owner (`UPrimitiveComponent`
/// `Translation`, `Rotation`, `Scale`, `Scale3D` and the absolute flags).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ComponentTransform {
    /// `Translation`.
    pub translation: Vec3,
    /// `Rotation` (Pitch, Yaw, Roll; 65536 units per turn).
    pub rotation: [i32; 3],
    /// `Scale`.
    pub scale: f32,
    /// `Scale3D`.
    pub scale3d: Vec3,
    /// `AbsoluteTranslation`.
    pub absolute_translation: bool,
    /// `AbsoluteRotation`.
    pub absolute_rotation: bool,
    /// `AbsoluteScale`.
    pub absolute_scale: bool,
}

impl Default for ComponentTransform {
    fn default() -> Self {
        ComponentTransform {
            translation: [0.0; 3],
            rotation: [0; 3],
            scale: 1.0,
            scale3d: [1.0; 3],
            absolute_translation: false,
            absolute_rotation: false,
            absolute_scale: false,
        }
    }
}

/// `UPrimitiveComponent::SetTransformedToWorld` (CONFIRMED from native
/// code): the component's own scale/rotation/translation applied before the
/// parent transform; `AbsoluteTranslation` drops the parent translation,
/// `AbsoluteScale` normalizes the parent axes, `AbsoluteRotation` keeps only
/// the parent axis lengths.
pub fn component_local_to_world(c: &ComponentTransform, parent: &Mat4) -> Mat4 {
    let mut p = *parent;
    if c.absolute_translation {
        p[3] = [0.0, 0.0, 0.0, p[3][3]];
    }
    if c.absolute_scale || c.absolute_rotation {
        let mut rows = [[0.0f64; 3]; 3];
        for (k, row) in rows.iter_mut().enumerate() {
            *row = [f64::from(p[k][0]), f64::from(p[k][1]), f64::from(p[k][2])];
        }
        if c.absolute_scale {
            for row in &mut rows {
                let len = (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt();
                if len > 1e-8 {
                    for v in row.iter_mut() {
                        *v /= len;
                    }
                }
            }
        }
        if c.absolute_rotation {
            let lens: Vec<f64> = rows
                .iter()
                .map(|r| (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt())
                .collect();
            rows = [[0.0; 3]; 3];
            for (k, (row, len)) in rows.iter_mut().zip(lens).enumerate() {
                row[k] = len;
            }
        }
        for k in 0..3 {
            p[k] = [rows[k][0] as f32, rows[k][1] as f32, rows[k][2] as f32, 0.0];
        }
    }
    let s = f64::from(c.scale);
    let s3 = f64v(c.scale3d);
    let rows = rotation_rows(c.rotation, [s3[0] * s, s3[1] * s, s3[2] * s]);
    let local = mat_from_rows(rows, f64v(c.translation));
    mat_mul(&local, &p)
}

// ------------------------------------------------------------ values

/// A property value in a compact JSON-friendly form.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ParamValue {
    /// Bool.
    Bool(bool),
    /// Int or byte.
    Int(i64),
    /// Float.
    Float(f32),
    /// String, name, enumerator or object path.
    Text(String),
    /// Array.
    List(Vec<ParamValue>),
    /// Struct members (`Name` or `Name[i]` for static arrays).
    Map(BTreeMap<String, ParamValue>),
    /// Null object reference or undecodable value.
    Null,
}

fn member_key(p: &Property) -> String {
    if p.array_index > 0 {
        format!("{}[{}]", p.name, p.array_index)
    } else {
        p.name.clone()
    }
}

/// Convert a decoded value.
pub fn param_value(v: &Value) -> ParamValue {
    match v {
        Value::Int(i) => ParamValue::Int(i64::from(*i)),
        Value::Float(f) => ParamValue::Float(*f),
        Value::Bool(b) => ParamValue::Bool(*b),
        Value::Byte(b) => ParamValue::Int(i64::from(*b)),
        Value::Enum(s) | Value::Name(s) | Value::Str(s) => ParamValue::Text(s.clone()),
        Value::Object(o) | Value::Interface(o) => {
            if o.index == 0 {
                ParamValue::Null
            } else {
                ParamValue::Text(o.path.clone())
            }
        }
        Value::Delegate { object, function } => {
            ParamValue::Text(format!("{}.{function}", object.path))
        }
        Value::Array(items) => ParamValue::List(items.iter().map(param_value).collect()),
        Value::Struct { fields, .. } => ParamValue::Map(
            fields
                .iter()
                .map(|f| (member_key(f), param_value(&f.value)))
                .collect(),
        ),
        Value::RawArray { .. } | Value::Raw { .. } => ParamValue::Null,
    }
}

/// Convert a property list.
pub fn param_map(props: &[Property]) -> BTreeMap<String, ParamValue> {
    props
        .iter()
        .map(|p| (member_key(p), param_value(&p.value)))
        .collect()
}

fn key_of(p: &Property) -> (String, i32) {
    (p.name.to_ascii_lowercase(), p.array_index)
}

/// Merge `from` over `into` (the engine's delta rule): tagged structs merge
/// member-wise, everything else (arrays, binary structs, scalars) replaces.
/// Names compare case-insensitively; when `into` holds the same key twice,
/// the first entry receives the value. Linear in `into.len() + from.len()`
/// (hashed keys), so hostile property lists cannot make it quadratic.
pub fn merge_properties(into: &mut Vec<Property>, from: &[Property]) {
    if from.is_empty() {
        return;
    }
    let mut index: HashMap<(String, i32), usize> = HashMap::with_capacity(into.len());
    for (i, q) in into.iter().enumerate() {
        index.entry(key_of(q)).or_insert(i);
    }
    for p in from {
        let key = key_of(p);
        match index.get(&key).and_then(|&i| into.get_mut(i)) {
            Some(q) => merge_value(&mut q.value, &p.value),
            None => {
                index.insert(key, into.len());
                into.push(p.clone());
            }
        }
    }
}

/// True when two entries of `props` share a name (ignoring ASCII case) and
/// array index.
fn has_repeated_keys(props: &[Property]) -> bool {
    let mut seen = HashSet::with_capacity(props.len());
    !props.iter().all(|p| seen.insert(key_of(p)))
}

/// Approximate in-memory size of a decoded value, in units of one small
/// value: one per value, array element and struct member, plus one per 32
/// bytes of text. Used to bound how much merged data a scene may build.
pub fn value_weight(v: &Value) -> usize {
    let text = |s: &str| 1usize.saturating_add(s.len() / 32);
    match v {
        Value::Array(items) => items
            .iter()
            .fold(1usize, |a, x| a.saturating_add(value_weight(x))),
        Value::Struct { name, fields, .. } => fields.iter().fold(text(name), |a, p| {
            a.saturating_add(property_weight(std::slice::from_ref(p)))
        }),
        Value::Enum(s) | Value::Name(s) | Value::Str(s) => text(s),
        Value::Object(o) | Value::Interface(o) => text(&o.path),
        Value::Delegate { object, function } => {
            text(&object.path).saturating_add(function.len() / 32)
        }
        Value::Raw { reason, .. } => text(reason),
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Byte(_)
        | Value::RawArray { .. } => 1,
    }
}

/// [`value_weight`] summed over a property list (tag names included).
pub fn property_weight(props: &[Property]) -> usize {
    props.iter().fold(0usize, |a, p| {
        let names = (p.name.len() + p.type_name.len()) / 32;
        a.saturating_add(1)
            .saturating_add(names)
            .saturating_add(value_weight(&p.value))
    })
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

/// Property `name` (array index 0) in `props`.
pub fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Value> {
    props
        .iter()
        .find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

/// Struct member `name` of a struct value.
pub fn member<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct { fields, .. } => prop(fields, name),
        _ => None,
    }
}

/// Numeric value as `f32`.
pub fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f32),
        Value::Byte(b) => Some(f32::from(*b)),
        _ => None,
    }
}

/// Integer value.
pub fn as_i32(v: &Value) -> Option<i32> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(i32::from(*b)),
        _ => None,
    }
}

/// `Vector` struct value.
pub fn as_vec3(v: &Value) -> Option<Vec3> {
    if !matches!(v, Value::Struct { .. }) {
        return None;
    }
    Some([
        member(v, "X").and_then(as_f32).unwrap_or(0.0),
        member(v, "Y").and_then(as_f32).unwrap_or(0.0),
        member(v, "Z").and_then(as_f32).unwrap_or(0.0),
    ])
}

/// `Rotator` struct value as `[Pitch, Yaw, Roll]`.
pub fn as_rotator(v: &Value) -> Option<[i32; 3]> {
    if !matches!(v, Value::Struct { .. }) {
        return None;
    }
    Some([
        member(v, "Pitch").and_then(as_i32).unwrap_or(0),
        member(v, "Yaw").and_then(as_i32).unwrap_or(0),
        member(v, "Roll").and_then(as_i32).unwrap_or(0),
    ])
}

/// `Color` struct value as `[R, G, B, A]`.
pub fn as_color(v: &Value) -> Option<[u8; 4]> {
    let c = |n: &str| {
        member(v, n)
            .and_then(as_i32)
            .and_then(|x| u8::try_from(x).ok())
            .unwrap_or(0)
    };
    matches!(v, Value::Struct { .. }).then(|| [c("R"), c("G"), c("B"), c("A")])
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Name(s) | Value::Str(s) | Value::Enum(s) => Some(s.clone()),
        _ => None,
    }
}

fn as_object(v: &Value) -> Option<(i32, String)> {
    match v {
        Value::Object(o) if o.index != 0 => Some((o.index, o.path.clone())),
        _ => None,
    }
}

struct Props<'a>(&'a [Property]);

impl Props<'_> {
    fn f32_or(&self, name: &str, default: f32) -> f32 {
        prop(self.0, name).and_then(as_f32).unwrap_or(default)
    }
    fn bool(&self, name: &str) -> bool {
        prop(self.0, name).and_then(as_bool).unwrap_or(false)
    }
    fn vec3_or(&self, name: &str, default: Vec3) -> Vec3 {
        prop(self.0, name).and_then(as_vec3).unwrap_or(default)
    }
    fn rotator(&self, name: &str) -> [i32; 3] {
        prop(self.0, name).and_then(as_rotator).unwrap_or([0; 3])
    }
    fn text(&self, name: &str) -> Option<String> {
        prop(self.0, name).and_then(as_text)
    }
    fn object(&self, name: &str) -> Option<(i32, String)> {
        prop(self.0, name).and_then(as_object)
    }
}

// ------------------------------------------------------------ scene model

/// Gameplay role of an actor, from its class chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// `WorldInfo`.
    WorldInfo,
    /// `PlayerStart`.
    PlayerStart,
    /// `asamu.ASAMUCheckpoint` (and subclasses).
    Checkpoint,
    /// `asamu.ASAMUCheckpointVisuals` (the visible marker a checkpoint uses).
    CheckpointVisuals,
    /// `asamu.ASAMUKillZone`.
    KillZone,
    /// `asamu.ASAMUDynamicKillZone`.
    DynamicKillZone,
    /// `asamu.ASAMUFallingRock`.
    FallingRock,
    /// `asamu.ASAMUFallingWhenGrappledRock`.
    FallingWhenGrappledRock,
    /// `asamu.ASAMURechargeCrystal`.
    RechargeCrystal,
    /// `asamu.ASAMUTelePad_Attractor`.
    TelePadAttractor,
    /// `asamu.ASAMUCollectible` (and subclasses).
    Collectible,
    /// Other `asamu` actor classes.
    AsamuOther,
    /// `TriggerVolume` (and subclasses not listed above).
    TriggerVolume,
    /// `Trigger`.
    Trigger,
    /// `BlockingVolume`.
    BlockingVolume,
    /// `GravityVolume`.
    GravityVolume,
    /// `PhysicsVolume`.
    PhysicsVolume,
    /// Any other `Volume`.
    Volume,
    /// A CSG brush (`Brush` that is not a volume).
    Brush,
    /// `InterpActor` (and subclasses not listed above).
    InterpActor,
    /// `StaticMeshActor` / `StaticMeshActorBase`.
    StaticMesh,
    /// `Light`.
    Light,
    /// `Emitter`.
    Emitter,
    /// `AmbientSound*`.
    Sound,
    /// Anything else.
    Other,
}

/// Classify an actor from its lower-case class chain (nearest first).
pub fn classify_actor(chain: &[String]) -> ActorKind {
    let has = |n: &str| chain.iter().any(|c| c == n);
    let has_prefix = |p: &str| chain.iter().any(|c| c.starts_with(p));
    let leaf = chain.first().map(String::as_str).unwrap_or("");
    if has("worldinfo") {
        ActorKind::WorldInfo
    } else if has("asamucheckpointvisuals") {
        ActorKind::CheckpointVisuals
    } else if has("asamucheckpoint") {
        ActorKind::Checkpoint
    } else if has("asamudynamickillzone") {
        ActorKind::DynamicKillZone
    } else if has("asamukillzone") {
        ActorKind::KillZone
    } else if has("asamufallingwhengrappledrock") {
        ActorKind::FallingWhenGrappledRock
    } else if has("asamufallingrock") {
        ActorKind::FallingRock
    } else if has("asamurechargecrystal") {
        ActorKind::RechargeCrystal
    } else if has("asamutelepad_attractor") {
        ActorKind::TelePadAttractor
    } else if has_prefix("asamucollectible") {
        ActorKind::Collectible
    } else if has("playerstart") {
        ActorKind::PlayerStart
    } else if has("gravityvolume") {
        ActorKind::GravityVolume
    } else if has("physicsvolume") {
        ActorKind::PhysicsVolume
    } else if has("blockingvolume") {
        ActorKind::BlockingVolume
    } else if has("triggervolume") {
        ActorKind::TriggerVolume
    } else if has("volume") {
        ActorKind::Volume
    } else if has("brush") {
        ActorKind::Brush
    } else if has("trigger") {
        ActorKind::Trigger
    } else if leaf.starts_with("asamu") {
        ActorKind::AsamuOther
    } else if has("interpactor") {
        ActorKind::InterpActor
    } else if has("staticmeshactorbase") || has("staticmeshactor") {
        ActorKind::StaticMesh
    } else if has("light") {
        ActorKind::Light
    } else if has("emitter") {
        ActorKind::Emitter
    } else if leaf.starts_with("ambientsound") {
        ActorKind::Sound
    } else {
        ActorKind::Other
    }
}

/// Role of a component, from its class chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// `StaticMeshComponent`.
    StaticMesh,
    /// `SkeletalMeshComponent`.
    SkeletalMesh,
    /// `LightComponent`.
    Light,
    /// `CylinderComponent`.
    Cylinder,
    /// `BrushComponent`.
    Brush,
    /// `ParticleSystemComponent`.
    ParticleSystem,
    /// `AudioComponent`.
    Audio,
    /// Editor-only helpers (sprites, arrows, radius displays).
    EditorHelper,
    /// Any other primitive component.
    OtherPrimitive,
    /// Any other component.
    Other,
}

/// Classify a component from its lower-case class chain.
pub fn classify_component(chain: &[String]) -> ComponentKind {
    let has = |n: &str| chain.iter().any(|c| c == n);
    if has("staticmeshcomponent") {
        ComponentKind::StaticMesh
    } else if has("skeletalmeshcomponent") {
        ComponentKind::SkeletalMesh
    } else if has("lightcomponent") {
        ComponentKind::Light
    } else if has("cylindercomponent") {
        ComponentKind::Cylinder
    } else if has("brushcomponent") {
        ComponentKind::Brush
    } else if has("particlesystemcomponent") {
        ComponentKind::ParticleSystem
    } else if has("audiocomponent") {
        ComponentKind::Audio
    } else if has("spritecomponent")
        || has("arrowcomponent")
        || has("drawlightradiuscomponent")
        || has("drawlightconecomponent")
        || has("drawsoundradiuscomponent")
        || has("drawspherecomponent")
        || has("drawboxcomponent")
        || has("drawcylindercomponent")
        || has("drawconecomponent")
        || has("drawfrustumcomponent")
    {
        ComponentKind::EditorHelper
    } else if has("primitivecomponent") {
        ComponentKind::OtherPrimitive
    } else {
        ComponentKind::Other
    }
}

/// Light component parameters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LightInfo {
    /// Component class name (`PointLightComponent`, ...).
    pub light_type: String,
    /// `Brightness`.
    pub brightness: f32,
    /// `LightColor` as `[R, G, B, A]`.
    pub color: [u8; 4],
    /// `Radius` (point and spot lights).
    pub radius: Option<f32>,
    /// `FalloffExponent` (point and spot lights).
    pub falloff_exponent: Option<f32>,
    /// `InnerConeAngle` (spot lights, degrees).
    pub inner_cone_angle: Option<f32>,
    /// `OuterConeAngle` (spot lights, degrees).
    pub outer_cone_angle: Option<f32>,
    /// `bEnabled`.
    pub enabled: bool,
    /// `CastShadows`.
    pub cast_shadows: bool,
}

/// One world-space convex hull of a brush or volume.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConvexHull {
    /// World-space vertices.
    pub vertices: Vec<Vec3>,
    /// Triangles (indices into `vertices`; from `FaceTriData`).
    pub triangles: Vec<[u32; 3]>,
    /// World-space face planes `(X, Y, Z, W)` (from `FacePlaneData`; empty
    /// when the transform scales non-uniformly).
    pub planes: Vec<[f32; 4]>,
}

/// Geometry of a brush or volume, in world space.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct VolumeGeometry {
    /// Convex hulls of the brush component's collision (`BrushAggGeom`).
    pub hulls: Vec<ConvexHull>,
    /// Triangles of the brush polygons (`Model` → `Polys`), when the cooked
    /// map still contains them.
    pub polys: Option<TriangleMesh>,
    /// World bounds `(min, max)` over all of the above.
    pub bounds: Option<(Vec3, Vec3)>,
}

/// A component of a placed actor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SceneComponent {
    /// Object name.
    pub name: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified class path.
    pub class: String,
    /// Role.
    pub kind: ComponentKind,
    /// Archetype (template) path.
    pub archetype: Option<String>,
    /// Placement relative to the owner.
    pub transform: ComponentTransform,
    /// World transform (row-vector matrix).
    pub local_to_world: Mat4,
    /// `HiddenGame`.
    pub hidden_game: bool,
    /// `CollideActors`.
    pub collide_actors: bool,
    /// `BlockActors`.
    pub block_actors: bool,
    /// `BlockRigidBody`.
    pub block_rigid_body: bool,
    /// `BlockZeroExtent`.
    pub block_zero_extent: bool,
    /// `BlockNonZeroExtent`.
    pub block_non_zero_extent: bool,
    /// `StaticMesh` (static mesh components).
    pub static_mesh: Option<String>,
    /// `SkeletalMesh` (skeletal mesh components).
    pub skeletal_mesh: Option<String>,
    /// `Materials` overrides (index = material slot; `None` = keep the mesh's).
    pub materials: Vec<Option<String>>,
    /// Light parameters (light components).
    pub light: Option<LightInfo>,
    /// `CollisionRadius`, `CollisionHeight` (cylinder components).
    pub cylinder: Option<[f32; 2]>,
    /// `Brush` model (brush components).
    pub brush_model: Option<String>,
    /// Convex elements in `BrushAggGeom` (brush components).
    pub brush_convex_elems: usize,
    /// Cooked physics convex elements in the native tail (brush components).
    pub brush_cached_physics: usize,
}

/// A Matinee (`SeqAct_Interp`) that drives an actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatineeRef {
    /// The `SeqAct_Interp` object.
    pub action: String,
    /// Its `InterpData`, if linked.
    pub interp_data: Option<String>,
    /// Variable link description (the Matinee group name).
    pub group: String,
}

/// One placed actor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SceneActor {
    /// Index in `ULevel::Actors`.
    pub slot: usize,
    /// Export index.
    pub export_index: usize,
    /// Object name.
    pub name: String,
    /// Qualified class path.
    pub class: String,
    /// Gameplay role.
    pub kind: ActorKind,
    /// Archetype path (null = the class default object).
    pub archetype: Option<String>,
    /// `Location`.
    pub location: Vec3,
    /// `Rotation` (Pitch, Yaw, Roll; 65536 units per turn).
    pub rotation: [i32; 3],
    /// `DrawScale`.
    pub draw_scale: f32,
    /// `DrawScale3D`.
    pub draw_scale3d: Vec3,
    /// `PrePivot`.
    pub pre_pivot: Vec3,
    /// `AActor::LocalToWorld` (row-vector matrix).
    pub local_to_world: Mat4,
    /// `Base` (attachment parent).
    pub base: Option<String>,
    /// `BaseBoneName`.
    pub base_bone: Option<String>,
    /// `bHardAttach`.
    pub hard_attach: bool,
    /// `bHidden`.
    pub hidden: bool,
    /// `bCollideActors`.
    pub collide_actors: bool,
    /// `bBlockActors`.
    pub block_actors: bool,
    /// `bStatic`.
    pub is_static: bool,
    /// `bMovable`.
    pub movable: bool,
    /// `bNoDelete`.
    pub no_delete: bool,
    /// `Physics` (the enum's first value when not stored).
    pub physics: Option<String>,
    /// `CollisionType` (the enum's first value when not stored).
    pub collision_type: Option<String>,
    /// `Tag`.
    pub tag: Option<String>,
    /// `Group`.
    pub group: Option<String>,
    /// `Layer`.
    pub layer: Option<String>,
    /// Components (subobjects of the actor that are components).
    pub components: Vec<SceneComponent>,
    /// Effective values (instance over archetype/defaults) of the properties
    /// declared by the actor's classes below `Engine.Actor`.
    pub params: BTreeMap<String, ParamValue>,
    /// Values stored on the instance itself (the level designer's deltas).
    pub instance: BTreeMap<String, ParamValue>,
    /// Brush / volume geometry (world space).
    pub volume: Option<VolumeGeometry>,
    /// Matinee actions that reference this actor.
    pub matinee: Vec<MatineeRef>,
}

/// A streaming sub-level reference (`WorldInfo.StreamingLevels`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamingLevel {
    /// The `LevelStreaming*` object.
    pub object: String,
    /// Its class name.
    pub class: String,
    /// `PackageName`.
    pub package_name: Option<String>,
    /// `Offset`.
    pub offset: Vec3,
    /// All effective values.
    pub params: BTreeMap<String, ParamValue>,
}

/// `WorldInfo` settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorldInfoSummary {
    /// The actor.
    pub object: String,
    /// `Title`.
    pub title: Option<String>,
    /// `KillZ`.
    pub kill_z: f32,
    /// `bSoftKillZ`.
    pub soft_kill_z: bool,
    /// `DefaultGravityZ` (class default; config may override at run time).
    pub default_gravity_z: f32,
    /// `GlobalGravityZ` (0 = use the default).
    pub global_gravity_z: f32,
    /// `DefaultGameType`.
    pub default_game_type: Option<String>,
}

/// Counts over a scene.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct SceneStats {
    /// Entries in `ULevel::Actors`.
    pub actor_slots: usize,
    /// Null entries (deleted actors).
    pub null_slots: usize,
    /// Actors extracted.
    pub actors: usize,
    /// Actors per kind.
    pub kinds: BTreeMap<ActorKind, usize>,
    /// Actors per class name.
    pub classes: BTreeMap<String, usize>,
    /// Components extracted.
    pub components: usize,
    /// Components per kind.
    pub component_kinds: BTreeMap<ComponentKind, usize>,
    /// Exports whose outer is the level and whose class is an actor but which
    /// are not listed in `Actors` (should be 0).
    pub unlisted_actors: usize,
    /// Listed actors whose outer is not the level (should be 0).
    pub foreign_actors: usize,
    /// Brushes/volumes with geometry.
    pub volumes_with_geometry: usize,
    /// Objects whose payload failed to decode.
    pub decode_failures: usize,
    /// Non-null `ULevel::Actors` entries that repeat an earlier entry
    /// (skipped; 0 in every shipped map).
    pub duplicate_slots: usize,
    /// Merged-value weight built for the scene ([`value_weight`] units:
    /// effective values, parameters, material lists, Matinee references),
    /// checked against [`SceneOptions::max_merged_weight`].
    pub merged_weight: usize,
    /// World-space volume geometry elements built (vertices, triangles,
    /// planes), checked against [`SceneOptions::max_geometry`].
    pub geometry_elements: usize,
    /// Values or geometry left out because a budget ran out (each with a
    /// warning; 0 in every shipped map).
    pub budget_skips: usize,
}

/// Scene description of one map package's level.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scene {
    /// [`SCENE_FORMAT`].
    pub format: &'static str,
    /// [`SCENE_VERSION`].
    pub version: u32,
    /// Map package name.
    pub package: String,
    /// Level object path.
    pub level: String,
    /// Level export index.
    pub level_export: usize,
    /// Units and axes of every position in the scene.
    pub coordinates: &'static str,
    /// Level native data.
    pub tail: LevelTail,
    /// `WorldInfo` settings.
    pub world_info: Option<WorldInfoSummary>,
    /// Streaming sub-levels.
    pub streaming_levels: Vec<StreamingLevel>,
    /// BSP model path.
    pub bsp_model: Option<String>,
    /// Actors in `ULevel::Actors` order.
    pub actors: Vec<SceneActor>,
    /// Counts.
    pub stats: SceneStats,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// Coordinate note stored in every scene.
pub const COORDINATES: &str = "UE3 world space: Unreal units, left-handed, X forward, Y right, Z up; \
     rotators in 65536 units per turn; matrices are row-vector (p' = p * M, row 3 = translation)";

// ------------------------------------------------------------ resolution

type EffectiveKey = (String, usize);

/// A property list shared between objects, with its [`property_weight`].
type Weighted = (Arc<Vec<Property>>, usize);

/// Merges objects over their archetypes / class defaults, with caching, and
/// keeps the scene's budgets: merged values may share data (an archetype or
/// template inherited by many objects) and would otherwise grow with the
/// product of sharers and shared size.
struct Resolver<'a> {
    set: &'a PackageSet,
    class_defaults: RefCell<HashMap<String, Weighted>>,
    own: RefCell<HashMap<EffectiveKey, Arc<Vec<Property>>>>,
    effective: RefCell<HashMap<EffectiveKey, Weighted>>,
    param_names: RefCell<HashMap<String, Arc<HashSet<String>>>>,
    brush_polys: RefCell<HashMap<EffectiveKey, Option<Arc<bsp::Polys>>>>,
    warnings: RefCell<Vec<String>>,
    dropped: Cell<usize>,
    failures: Cell<usize>,
    merged: Cell<usize>,
    merge_budget: usize,
    geometry: Cell<usize>,
    geometry_budget: usize,
    budget_skips: Cell<usize>,
}

impl<'a> Resolver<'a> {
    fn new(set: &'a PackageSet, opts: &SceneOptions) -> Self {
        Resolver {
            set,
            class_defaults: RefCell::default(),
            own: RefCell::default(),
            effective: RefCell::default(),
            param_names: RefCell::default(),
            brush_polys: RefCell::default(),
            warnings: RefCell::default(),
            dropped: Cell::new(0),
            failures: Cell::new(0),
            merged: Cell::new(0),
            merge_budget: opts.max_merged_weight,
            geometry: Cell::new(0),
            geometry_budget: opts.max_geometry,
            budget_skips: Cell::new(0),
        }
    }

    fn warn(&self, msg: String) {
        let mut w = self.warnings.borrow_mut();
        if w.len() < MAX_SCENE_WARNINGS {
            w.push(msg);
        } else {
            self.dropped.set(self.dropped.get().saturating_add(1));
        }
    }

    fn fail(&self, msg: String) {
        self.failures.set(self.failures.get().saturating_add(1));
        self.warn(msg);
    }

    /// Spend `weight` merged-value units; false (with a warning naming
    /// `what`) when the budget does not allow it.
    fn charge(&self, weight: usize, what: &dyn Fn() -> String) -> bool {
        let total = self.merged.get().saturating_add(weight);
        if total > self.merge_budget {
            self.budget_skips
                .set(self.budget_skips.get().saturating_add(1));
            self.warn(format!(
                "{}: merged-value budget of {} units exhausted; left out",
                what(),
                self.merge_budget
            ));
            return false;
        }
        self.merged.set(total);
        true
    }

    /// Spend `elements` geometry units (vertices, triangles, planes).
    fn charge_geometry(&self, elements: usize, what: &dyn Fn() -> String) -> bool {
        let total = self.geometry.get().saturating_add(elements);
        if total > self.geometry_budget {
            self.budget_skips
                .set(self.budget_skips.get().saturating_add(1));
            self.warn(format!(
                "{}: geometry budget of {} elements exhausted; left out",
                what(),
                self.geometry_budget
            ));
            return false;
        }
        self.geometry.set(total);
        true
    }

    fn defaults(&self, class: &str) -> Weighted {
        let key = class.to_ascii_lowercase();
        if let Some(v) = self.class_defaults.borrow().get(&key) {
            return v.clone();
        }
        let props: Vec<Property> = match self.set.inherited_defaults(class) {
            Ok(d) => d
                .values
                .into_iter()
                .map(|r| Property {
                    name: r.name,
                    type_name: r.type_name,
                    array_index: r.array_index,
                    size: 0,
                    struct_name: None,
                    enum_name: None,
                    value: r.value,
                    offset: 0,
                })
                .collect(),
            Err(e) => {
                self.warn(format!("class defaults of {class} unavailable: {e}"));
                Vec::new()
            }
        };
        let weight = property_weight(&props);
        let v = (Arc::new(props), weight);
        self.class_defaults.borrow_mut().insert(key, v.clone());
        v
    }

    /// Own (stored) properties of export `index` of `lp`, cached; empty on
    /// failure (counted, with a warning).
    fn own(&self, lp: &LoadedPackage, index: usize) -> Arc<Vec<Property>> {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(v) = self.own.borrow().get(&key) {
            return v.clone();
        }
        let v = match self.set.decode(lp, index) {
            Ok(o) => Arc::new(o.properties),
            Err(e) => {
                self.fail(format!("{}: export {index} does not decode: {e}", lp.name));
                Arc::new(Vec::new())
            }
        };
        self.own.borrow_mut().insert(key, v.clone());
        v
    }

    /// Effective properties of export `index` of `lp`: its own values merged
    /// over its archetype's effective values (or, without an archetype, over
    /// its class defaults).
    fn effective(&self, lp: &Arc<LoadedPackage>, index: usize, depth: usize) -> Arc<Vec<Property>> {
        self.effective_weighted(lp, index, depth).0
    }

    fn effective_weighted(&self, lp: &Arc<LoadedPackage>, index: usize, depth: usize) -> Weighted {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(v) = self.effective.borrow().get(&key) {
            return v.clone();
        }
        let own = self.own(lp, index);
        let path = lp
            .qualified(index)
            .unwrap_or_else(|_| format!("export {index}"));
        let archetype = lp
            .package
            .export(index)
            .map(|e| e.archetype_index)
            .unwrap_or_default();
        let mut base = None;
        if !archetype.is_null() {
            if depth >= MAX_ARCHETYPE_DEPTH {
                self.warn(format!(
                    "{path}: archetype chain longer than {MAX_ARCHETYPE_DEPTH}"
                ));
            } else {
                match lp.ref_path(archetype) {
                    Ok(Some(apath)) => match self.set.locate(&apath) {
                        Some((alp, ai)) => {
                            base = Some(self.effective_weighted(&alp, ai, depth + 1));
                        }
                        None => self.warn(format!("{path}: archetype {apath} not found")),
                    },
                    Ok(None) => {}
                    Err(e) => self.warn(format!("{path}: archetype unresolvable: {e}")),
                }
            }
        }
        let (base, base_weight) = match base {
            Some(b) => b,
            None => {
                let class = crate::object::export_class_path(&lp.package, Some(&lp.name), index)
                    .unwrap_or_default();
                self.defaults(&class)
            }
        };
        let own_weight = property_weight(&own);
        // Share instead of copying when there is nothing to merge (a stored
        // list that repeats a key still goes through the merge, so that the
        // later value wins as when the engine loads the tags in order).
        let v = if own.is_empty() {
            (base, base_weight)
        } else if base.is_empty() && !has_repeated_keys(&own) {
            (own, own_weight)
        } else {
            let weight = base_weight.saturating_add(own_weight);
            if self.charge(weight, &|| format!("{path}: inherited values")) {
                let mut merged = (*base).clone();
                merge_properties(&mut merged, &own);
                (Arc::new(merged), weight)
            } else {
                (own, own_weight)
            }
        };
        self.effective.borrow_mut().insert(key, v.clone());
        v
    }

    fn class_chain(&self, class: &str) -> Vec<String> {
        self.set.class_chain(class)
    }

    /// Enumerator stored in `name`, or, when the value is not stored (zero),
    /// the enum's first enumerator.
    fn enum_value(&self, class: &str, props: &[Property], name: &str) -> Option<String> {
        if let Some(v) = prop(props, name).and_then(as_text) {
            return Some(v);
        }
        let def = self.set.find_property(class, name)?;
        match &def.ty {
            crate::schema::PropertyType::Byte {
                enum_path: Some(path),
            } => self.set.enum_names(path)?.first().cloned(),
            _ => None,
        }
    }

    /// Names (lower case) of properties declared by `class` and its supers
    /// strictly below `Engine.Actor`, cached per class.
    fn param_names(&self, class: &str) -> Arc<HashSet<String>> {
        let key = class.to_ascii_lowercase();
        if let Some(v) = self.param_names.borrow().get(&key) {
            return v.clone();
        }
        let mut classes = vec![class.to_owned()];
        classes.extend(self.set.super_chain(class));
        let mut out = HashSet::new();
        for c in classes {
            if c.eq_ignore_ascii_case("Engine.Actor") || c.eq_ignore_ascii_case("Core.Object") {
                break;
            }
            if let Some(def) = self.set.struct_def(&c) {
                out.extend(def.properties.iter().map(|p| p.name.to_ascii_lowercase()));
            }
        }
        let v = Arc::new(out);
        self.param_names.borrow_mut().insert(key, v.clone());
        v
    }

    /// Decoded `Polys` of the brush model export `model` of `lp`, decoded
    /// once per model however many brushes share it.
    fn brush_polys(&self, lp: &Arc<LoadedPackage>, model: usize) -> Option<Arc<bsp::Polys>> {
        let key = (lp.name.to_ascii_lowercase(), model);
        if let Some(v) = self.brush_polys.borrow().get(&key) {
            return v.clone();
        }
        let v = self.decode_brush_polys(lp, model);
        self.brush_polys.borrow_mut().insert(key, v.clone());
        v
    }

    fn decode_brush_polys(&self, lp: &LoadedPackage, model: usize) -> Option<Arc<bsp::Polys>> {
        let (_, m) = match bsp::decode_model(&lp.package, Some(&lp.name), model, self.set) {
            Ok(m) => m,
            Err(e) => {
                self.fail(format!("{}: brush model export {model}: {e}", lp.name));
                return None;
            }
        };
        let pi = m.polys.export_index()?;
        match bsp::decode_polys(&lp.package, Some(&lp.name), pi, self.set) {
            Ok((_, p)) => Some(Arc::new(p)),
            Err(e) => {
                self.fail(format!("{}: polys export {pi}: {e}", lp.name));
                None
            }
        }
    }
}

fn export_name(pkg: &Package, i: usize) -> String {
    pkg.export(i)
        .map(|e| pkg.fname(e.object_name))
        .unwrap_or_default()
}

/// Children (by outer) of every export.
fn children_by_outer(pkg: &Package) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); pkg.exports.len()];
    for (i, e) in pkg.exports.iter().enumerate() {
        if let Some(o) = e.outer_index.export_index()
            && let Some(list) = out.get_mut(o)
        {
            list.push(i);
        }
    }
    out
}

fn export_of(index: i32) -> Option<usize> {
    PackageIndex(index).export_index()
}

fn component_transform(p: &Props<'_>) -> ComponentTransform {
    ComponentTransform {
        translation: p.vec3_or("Translation", [0.0; 3]),
        rotation: p.rotator("Rotation"),
        scale: p.f32_or("Scale", 1.0),
        scale3d: p.vec3_or("Scale3D", [1.0; 3]),
        absolute_translation: p.bool("AbsoluteTranslation"),
        absolute_rotation: p.bool("AbsoluteRotation"),
        absolute_scale: p.bool("AbsoluteScale"),
    }
}

fn light_info(class: &str, chain: &[String], p: &Props<'_>) -> LightInfo {
    let point = chain.iter().any(|c| c == "pointlightcomponent");
    let spot = chain.iter().any(|c| c == "spotlightcomponent");
    LightInfo {
        light_type: last_component(class).to_owned(),
        brightness: p.f32_or("Brightness", 0.0),
        color: prop(p.0, "LightColor").and_then(as_color).unwrap_or([0; 4]),
        radius: point.then(|| p.f32_or("Radius", 0.0)),
        falloff_exponent: point.then(|| p.f32_or("FalloffExponent", 0.0)),
        inner_cone_angle: spot.then(|| p.f32_or("InnerConeAngle", 0.0)),
        outer_cone_angle: spot.then(|| p.f32_or("OuterConeAngle", 0.0)),
        enabled: p.bool("bEnabled"),
        cast_shadows: p.bool("CastShadows"),
    }
}

fn materials(p: &Props<'_>) -> Vec<Option<String>> {
    match prop(p.0, "Materials") {
        Some(Value::Array(items)) => items.iter().map(|v| as_object(v).map(|o| o.1)).collect(),
        _ => Vec::new(),
    }
}

/// True when the 3x3 part of `m` is a scaled rotation with uniform scale
/// (planes then transform by the same matrix).
fn uniform_scale(m: &Mat4) -> Option<f64> {
    let len = |k: usize| {
        (f64::from(m[k][0]).powi(2) + f64::from(m[k][1]).powi(2) + f64::from(m[k][2]).powi(2))
            .sqrt()
    };
    let (a, b, c) = (len(0), len(1), len(2));
    let ok = a > 1e-8 && (a - b).abs() <= 1e-4 * a && (a - c).abs() <= 1e-4 * a;
    ok.then_some(a)
}

fn array_len(v: Option<&Value>) -> usize {
    match v {
        Some(Value::Array(items)) => items.len(),
        _ => 0,
    }
}

/// Geometry elements (vertices, triangles, planes) that [`hull_from_elem`]
/// builds from `elem`, computed before building anything.
fn hull_size(elem: &Value) -> usize {
    array_len(member(elem, "VertexData"))
        .saturating_add(array_len(member(elem, "FaceTriData")) / 3)
        .saturating_add(array_len(member(elem, "FacePlaneData")))
}

/// World-space hull of one `KConvexElem`. Triangles are kept only when every
/// vertex and every index decodes (a dropped element would shift the
/// indices onto the wrong vertices) and every index is in range.
fn hull_from_elem(elem: &Value, l2w: &Mat4) -> Option<ConvexHull> {
    let raw_verts: Vec<Option<Vec3>> = match member(elem, "VertexData") {
        Some(Value::Array(items)) => items.iter().map(as_vec3).collect(),
        _ => Vec::new(),
    };
    let complete = raw_verts.iter().all(Option::is_some);
    let verts: Vec<Vec3> = raw_verts.into_iter().flatten().collect();
    if verts.is_empty() {
        return None;
    }
    let n = verts.len();
    let tri_data: Option<Vec<i32>> = match member(elem, "FaceTriData") {
        Some(Value::Array(items)) => items.iter().map(as_i32).collect(),
        _ => Some(Vec::new()),
    };
    let mut triangles = Vec::new();
    if complete
        && let Some(tri_data) = tri_data
        && tri_data.len().is_multiple_of(3)
    {
        let index = |i: i32| {
            usize::try_from(i)
                .ok()
                .filter(|&i| i < n)
                .and_then(|i| u32::try_from(i).ok())
        };
        for t in tri_data.as_chunks::<3>().0 {
            if let (Some(a), Some(b), Some(c)) = (index(t[0]), index(t[1]), index(t[2])) {
                triangles.push([a, b, c]);
            }
        }
    }
    let mut planes = Vec::new();
    if let (Some(Value::Array(items)), Some(scale)) =
        (member(elem, "FacePlaneData"), uniform_scale(l2w))
    {
        for pv in items {
            let get = |k: &str| member(pv, k).and_then(as_f32).unwrap_or(0.0);
            let normal = [get("X"), get("Y"), get("Z")];
            let w = get("W");
            // A point on the plane, transformed; the normal rotates with the
            // matrix (uniform scale removed).
            let on = [normal[0] * w, normal[1] * w, normal[2] * w];
            let p = transform_point(l2w, on);
            let mut wn = [0.0f64; 3];
            for (j, slot) in wn.iter_mut().enumerate() {
                *slot = (f64::from(normal[0]) * f64::from(l2w[0][j])
                    + f64::from(normal[1]) * f64::from(l2w[1][j])
                    + f64::from(normal[2]) * f64::from(l2w[2][j]))
                    / scale;
            }
            let ww = wn[0] * f64::from(p[0]) + wn[1] * f64::from(p[1]) + wn[2] * f64::from(p[2]);
            planes.push([wn[0] as f32, wn[1] as f32, wn[2] as f32, ww as f32]);
        }
    }
    Some(ConvexHull {
        vertices: verts.iter().map(|v| transform_point(l2w, *v)).collect(),
        triangles,
        planes,
    })
}

fn extend_bounds(b: &mut Option<(Vec3, Vec3)>, p: Vec3) {
    match b {
        Some((lo, hi)) => {
            for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *l = l.min(v);
                *h = h.max(v);
            }
        }
        None => *b = Some((p, p)),
    }
}

/// Default [`SceneOptions::max_merged_weight`]: about six times the largest
/// shipped map (AG-StarHaven: 2.7 million units, also with `all_params`; see
/// LEVEL_FORMAT.md). One unit is roughly 100 bytes in memory.
pub const MAX_SCENE_MERGED_WEIGHT: usize = 1 << 24;
/// Default [`SceneOptions::max_geometry`]: over 300 times the largest shipped
/// map (AG-StarHaven: 12,988 elements) while bounding memory to tens of MB.
pub const MAX_SCENE_GEOMETRY: usize = 1 << 22;

/// Options for [`extract_scene`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneOptions {
    /// Build world-space geometry for brushes and volumes.
    pub volume_geometry: bool,
    /// Include the effective class parameters of every actor (otherwise only
    /// for actors that are not plain static meshes, lights or editor brushes).
    pub all_params: bool,
    /// Most merged-value weight ([`value_weight`] units) the scene may build:
    /// values inherited from shared archetypes, templates or class defaults,
    /// parameter maps, material lists and Matinee references. Shared data
    /// would otherwise grow with the product of sharers and shared size in a
    /// crafted package. Past the budget, values are left out with a warning.
    pub max_merged_weight: usize,
    /// Most world-space volume geometry elements (vertices, triangles,
    /// planes) the scene may build; past it, geometry is left out with a
    /// warning.
    pub max_geometry: usize,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions {
            volume_geometry: true,
            all_params: false,
            max_merged_weight: MAX_SCENE_MERGED_WEIGHT,
            max_geometry: MAX_SCENE_GEOMETRY,
        }
    }
}

/// World-space triangles of decoded brush polygons (one fan per polygon,
/// tagged with the polygon index).
fn polys_mesh(polys: &bsp::Polys, l2w: &Mat4) -> TriangleMesh {
    let mut mesh = TriangleMesh::default();
    for (k, poly) in polys.polys.iter().enumerate() {
        let world: Vec<Vec3> = poly
            .vertices
            .iter()
            .map(|v| transform_point(l2w, *v))
            .collect();
        mesh.add_polygon(&world, u32::try_from(k).unwrap_or(u32::MAX));
    }
    mesh
}

/// Geometry elements [`polys_mesh`] builds (vertices plus triangles).
fn polys_size(polys: &bsp::Polys) -> usize {
    polys.polys.iter().fold(0usize, |a, p| {
        a.saturating_add(p.vertices.len().saturating_mul(2))
    })
}

/// Brush polygons of the `Model` export `model` in world space, within the
/// geometry budget.
fn brush_mesh(
    res: &Resolver<'_>,
    lp: &Arc<LoadedPackage>,
    model: usize,
    l2w: &Mat4,
    owner: &str,
) -> Option<TriangleMesh> {
    let polys = res.brush_polys(lp, model)?;
    res.charge_geometry(polys_size(&polys), &|| format!("{owner}: brush polygons"))
        .then(|| polys_mesh(&polys, l2w))
}

/// Collect Matinee references: actor export index → actions that link it.
fn matinee_refs(res: &Resolver<'_>, lp: &Arc<LoadedPackage>) -> HashMap<usize, Vec<MatineeRef>> {
    let pkg = &lp.package;
    let mut out: HashMap<usize, Vec<MatineeRef>> = HashMap::new();
    for i in 0..pkg.exports.len() {
        let Ok(class) = pkg.export_class_name(i) else {
            continue;
        };
        if !class.eq_ignore_ascii_case("SeqAct_Interp") {
            continue;
        }
        let props = res.effective(lp, i, 0);
        let action = lp.qualified(i).unwrap_or_default();
        let Some(Value::Array(links)) = prop(&props, "VariableLinks") else {
            continue;
        };
        let mut interp_data = None;
        let mut actors: Vec<(usize, String)> = Vec::new();
        for link in links {
            let desc = member(link, "LinkDesc")
                .and_then(as_text)
                .unwrap_or_default();
            let Some(Value::Array(vars)) = member(link, "LinkedVariables") else {
                continue;
            };
            for var in vars {
                let Some((idx, path)) = as_object(var) else {
                    continue;
                };
                let Some(vi) = export_of(idx) else {
                    continue;
                };
                let vclass = pkg.export_class_name(vi).unwrap_or_default();
                if vclass.eq_ignore_ascii_case("InterpData") {
                    interp_data = Some(path);
                } else if vclass.eq_ignore_ascii_case("SeqVar_Object") {
                    let vp = res.effective(lp, vi, 0);
                    if let Some((oi, _)) = Props(&vp).object("ObjValue")
                        && let Some(ai) = export_of(oi)
                    {
                        actors.push((ai, desc.clone()));
                    }
                }
            }
        }
        let interp_len = interp_data.as_ref().map_or(0, String::len);
        for (ai, group) in actors {
            let weight = 1usize.saturating_add(
                action
                    .len()
                    .saturating_add(interp_len)
                    .saturating_add(group.len())
                    / 32,
            );
            if !res.charge(weight, &|| format!("{action}: Matinee references")) {
                return out;
            }
            out.entry(ai).or_default().push(MatineeRef {
                action: action.clone(),
                interp_data: interp_data.clone(),
                group,
            });
        }
    }
    out
}

/// Extract the scene of level export `level` in `lp`.
///
/// Bounded on hostile input: repeated `Actors` entries are skipped, every
/// brush model is decoded once however many brushes share it, and merged
/// values and volume geometry stay within the budgets of `opts` (see
/// [`SceneOptions`]); what does not fit is left out with a warning and
/// counted in [`SceneStats::budget_skips`].
pub fn extract_scene(
    set: &PackageSet,
    lp: &Arc<LoadedPackage>,
    level: usize,
    opts: &SceneOptions,
) -> ObjResult<Scene> {
    let pkg = &lp.package;
    let (_, tail) = decode_level(pkg, Some(&lp.name), level, set)?;
    let res = Resolver::new(set, opts);
    let children = children_by_outer(pkg);
    let matinee = matinee_refs(&res, lp);
    let mut stats = SceneStats {
        actor_slots: tail.actors.len(),
        ..SceneStats::default()
    };
    let mut actors = Vec::new();
    let mut listed = HashSet::new();
    let mut world_info = None;
    let mut streaming_levels = Vec::new();

    for (slot, a) in tail.actors.iter().enumerate() {
        if a.is_null() {
            stats.null_slots += 1;
            continue;
        }
        let Some(ai) = a.export_index() else {
            res.warn(format!("actor slot {slot}: {a} is not an export"));
            continue;
        };
        let Ok(export) = pkg.export(ai) else {
            res.warn(format!("actor slot {slot}: export {ai} out of range"));
            continue;
        };
        if !listed.insert(ai) {
            stats.duplicate_slots += 1;
            res.warn(format!(
                "actor slot {slot}: export {ai} is listed again; skipped"
            ));
            continue;
        }
        if export.outer_index.export_index() != Some(level) {
            stats.foreign_actors += 1;
        }
        let class = crate::object::export_class_path(pkg, Some(&lp.name), ai)
            .unwrap_or_else(|_| "?".to_owned());
        let chain = res.class_chain(&class);
        let kind = classify_actor(&chain);
        let props = res.effective(lp, ai, 0);
        let p = Props(&props);
        let own = res.own(lp, ai);
        let name = export_name(pkg, ai);
        let location = p.vec3_or("Location", [0.0; 3]);
        let rotation = p.rotator("Rotation");
        let draw_scale = p.f32_or("DrawScale", 1.0);
        let draw_scale3d = p.vec3_or("DrawScale3D", [1.0; 3]);
        let pre_pivot = p.vec3_or("PrePivot", [0.0; 3]);
        let l2w = actor_local_to_world(location, rotation, draw_scale, draw_scale3d, pre_pivot);

        // Components: subobjects of the actor whose class is a component.
        let mut components = Vec::new();
        let mut volume = VolumeGeometry::default();
        let wants_volume = opts.volume_geometry && chain.iter().any(|c| c == "brush");
        for &ci in children.get(ai).map(Vec::as_slice).unwrap_or(&[]) {
            let cclass = crate::object::export_class_path(pkg, Some(&lp.name), ci)
                .unwrap_or_else(|_| "?".to_owned());
            let cchain = res.class_chain(&cclass);
            if !cchain.iter().any(|c| c == "component") {
                continue;
            }
            let ckind = classify_component(&cchain);
            let cprops = res.effective(lp, ci, 0);
            let cp = Props(&cprops);
            let transform = component_transform(&cp);
            let c_l2w = component_local_to_world(&transform, &l2w);
            let cname = export_name(pkg, ci);
            let mut comp = SceneComponent {
                name: cname.clone(),
                export_index: ci,
                class: cclass.clone(),
                kind: ckind,
                archetype: pkg
                    .export(ci)
                    .ok()
                    .and_then(|e| lp.ref_path(e.archetype_index).ok().flatten()),
                transform,
                local_to_world: c_l2w,
                hidden_game: cp.bool("HiddenGame"),
                collide_actors: cp.bool("CollideActors"),
                block_actors: cp.bool("BlockActors"),
                block_rigid_body: cp.bool("BlockRigidBody"),
                block_zero_extent: cp.bool("BlockZeroExtent"),
                block_non_zero_extent: cp.bool("BlockNonZeroExtent"),
                static_mesh: None,
                skeletal_mesh: None,
                materials: Vec::new(),
                light: None,
                cylinder: None,
                brush_model: None,
                brush_convex_elems: 0,
                brush_cached_physics: 0,
            };
            let with_materials = |comp: &mut SceneComponent| {
                let list = materials(&cp);
                let weight = list.iter().fold(0usize, |a, m| {
                    a.saturating_add(1)
                        .saturating_add(m.as_ref().map_or(0, |s| s.len() / 32))
                });
                if list.is_empty()
                    || res.charge(weight, &|| format!("{name}.{cname}: material overrides"))
                {
                    comp.materials = list;
                }
            };
            match ckind {
                ComponentKind::StaticMesh => {
                    comp.static_mesh = cp.object("StaticMesh").map(|o| o.1);
                    with_materials(&mut comp);
                }
                ComponentKind::SkeletalMesh => {
                    comp.skeletal_mesh = cp.object("SkeletalMesh").map(|o| o.1);
                    with_materials(&mut comp);
                }
                ComponentKind::Light => comp.light = Some(light_info(&cclass, &cchain, &cp)),
                ComponentKind::Cylinder => {
                    comp.cylinder = Some([
                        cp.f32_or("CollisionRadius", 0.0),
                        cp.f32_or("CollisionHeight", 0.0),
                    ]);
                }
                ComponentKind::Brush => {
                    comp.brush_model = cp.object("Brush").map(|o| o.1);
                    match bsp::decode_brush_component(pkg, Some(&lp.name), ci, set) {
                        Ok((_, t)) => comp.brush_cached_physics = t.len(),
                        Err(e) => res.fail(format!("{}: brush component {ci}: {e}", lp.name)),
                    }
                    let elems = prop(&cprops, "BrushAggGeom")
                        .and_then(|g| member(g, "ConvexElems"))
                        .and_then(|v| match v {
                            Value::Array(items) => Some(items.as_slice()),
                            _ => None,
                        })
                        .unwrap_or(&[]);
                    comp.brush_convex_elems = elems.len();
                    if wants_volume {
                        for e in elems {
                            if !res.charge_geometry(hull_size(e), &|| {
                                format!("{name}.{cname}: convex hulls")
                            }) {
                                break;
                            }
                            if let Some(h) = hull_from_elem(e, &c_l2w) {
                                volume.hulls.push(h);
                            }
                        }
                        if volume.polys.is_none()
                            && let Some((mi, _)) = cp.object("Brush")
                            && let Some(mi) = export_of(mi)
                        {
                            volume.polys = brush_mesh(&res, lp, mi, &c_l2w, &name);
                        }
                    }
                }
                _ => {}
            }
            components.push(comp);
        }
        // Brushes whose component has no model reference: the actor's own.
        if wants_volume
            && volume.polys.is_none()
            && let Some((mi, _)) = p.object("Brush")
            && let Some(mi) = export_of(mi)
        {
            volume.polys = brush_mesh(&res, lp, mi, &l2w, &name);
        }
        let volume = if wants_volume {
            for h in &volume.hulls {
                for v in &h.vertices {
                    extend_bounds(&mut volume.bounds, *v);
                }
            }
            if let Some(m) = &volume.polys {
                for v in &m.positions {
                    extend_bounds(&mut volume.bounds, *v);
                }
            }
            if volume.bounds.is_some() {
                stats.volumes_with_geometry += 1;
            }
            Some(volume)
        } else {
            None
        };

        let plain = matches!(
            kind,
            ActorKind::StaticMesh | ActorKind::Light | ActorKind::Brush | ActorKind::Emitter
        );
        let params = if opts.all_params || !plain {
            let names = res.param_names(&class);
            let selected: Vec<Property> = props
                .iter()
                .filter(|q| names.contains(&q.name.to_ascii_lowercase()))
                .cloned()
                .collect();
            if res.charge(property_weight(&selected), &|| {
                format!("{name}: parameters")
            }) {
                param_map(&selected)
            } else {
                BTreeMap::new()
            }
        } else {
            BTreeMap::new()
        };
        let instance = param_map(&own);

        if kind == ActorKind::WorldInfo && world_info.is_none() {
            world_info = Some(WorldInfoSummary {
                object: lp.qualified(ai).unwrap_or_else(|_| name.clone()),
                title: p.text("Title").filter(|t| !t.is_empty()),
                kill_z: p.f32_or("KillZ", 0.0),
                soft_kill_z: p.bool("bSoftKillZ"),
                default_gravity_z: p.f32_or("DefaultGravityZ", 0.0),
                global_gravity_z: p.f32_or("GlobalGravityZ", 0.0),
                default_game_type: p.object("DefaultGameType").map(|o| o.1),
            });
            if let Some(Value::Array(items)) = prop(&props, "StreamingLevels") {
                for item in items {
                    let Some((si, spath)) = as_object(item) else {
                        continue;
                    };
                    let Some(se) = export_of(si) else {
                        continue;
                    };
                    let (sp, weight) = res.effective_weighted(lp, se, 0);
                    if !res.charge(weight, &|| format!("{spath}: streaming level values")) {
                        break;
                    }
                    let sprops = Props(&sp);
                    streaming_levels.push(StreamingLevel {
                        object: spath,
                        class: pkg.export_class_name(se).unwrap_or_default(),
                        package_name: sprops.text("PackageName"),
                        offset: sprops.vec3_or("Offset", [0.0; 3]),
                        params: param_map(&sp),
                    });
                }
            }
        }

        *stats.kinds.entry(kind).or_default() += 1;
        *stats
            .classes
            .entry(last_component(&class).to_owned())
            .or_default() += 1;
        stats.components += components.len();
        for c in &components {
            *stats.component_kinds.entry(c.kind).or_default() += 1;
        }
        let physics = res.enum_value(&class, &props, "Physics");
        let collision_type = res.enum_value(&class, &props, "CollisionType");
        actors.push(SceneActor {
            slot,
            export_index: ai,
            name,
            class,
            kind,
            archetype: lp.ref_path(export.archetype_index).ok().flatten(),
            location,
            rotation,
            draw_scale,
            draw_scale3d,
            pre_pivot,
            local_to_world: l2w,
            base: p.object("Base").map(|o| o.1),
            base_bone: p.text("BaseBoneName").filter(|s| s != "None"),
            hard_attach: p.bool("bHardAttach"),
            hidden: p.bool("bHidden"),
            collide_actors: p.bool("bCollideActors"),
            block_actors: p.bool("bBlockActors"),
            is_static: p.bool("bStatic"),
            movable: p.bool("bMovable"),
            no_delete: p.bool("bNoDelete"),
            physics,
            collision_type,
            tag: p.text("Tag").filter(|s| s != "None"),
            group: p.text("Group").filter(|s| s != "None"),
            layer: p.text("Layer").filter(|s| s != "None"),
            components,
            params,
            instance,
            volume,
            matinee: matinee.get(&ai).cloned().unwrap_or_default(),
        });
    }
    stats.actors = actors.len();

    // Actors directly inside the level that the list does not mention.
    for &ci in children.get(level).map(Vec::as_slice).unwrap_or(&[]) {
        if listed.contains(&ci) {
            continue;
        }
        let class = crate::object::export_class_path(pkg, Some(&lp.name), ci).unwrap_or_default();
        if res.class_chain(&class).iter().any(|c| c == "actor") {
            stats.unlisted_actors += 1;
        }
    }
    stats.decode_failures = res.failures.get();
    stats.merged_weight = res.merged.get();
    stats.geometry_elements = res.geometry.get();
    stats.budget_skips = res.budget_skips.get();
    let mut warnings = res.warnings.borrow().clone();
    let dropped = res.dropped.get();
    if dropped > 0 {
        warnings.push(format!("{dropped} further warnings not listed"));
    }
    Ok(Scene {
        format: SCENE_FORMAT,
        version: SCENE_VERSION,
        package: lp.name.clone(),
        level: lp.qualified(level)?,
        level_export: level,
        coordinates: COORDINATES,
        bsp_model: lp.ref_path(tail.model)?,
        tail,
        world_info,
        streaming_levels,
        actors,
        stats,
        warnings,
    })
}

// ------------------------------------------------------------ BSP geometry

/// One BSP surface, for rendering (material and texture axes) and collision
/// classification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SurfaceInfo {
    /// Material path.
    pub material: Option<String>,
    /// Surface flags.
    pub poly_flags: u32,
    /// Texture origin.
    pub base: Vec3,
    /// Surface normal.
    pub normal: Vec3,
    /// Texture U axis.
    pub texture_u: Vec3,
    /// Texture V axis.
    pub texture_v: Vec3,
    /// Source brush actor.
    pub actor: Option<String>,
    /// Source polygon index in that brush's `Polys` (`-1` = none).
    pub brush_poly: i32,
    /// Surface plane `(X, Y, Z, W)`.
    pub plane: [f32; 4],
}

/// BSP geometry of a level's model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BspGeometry {
    /// Model path.
    pub model: String,
    /// Bounds.
    pub bounds: BoxSphereBounds,
    /// Consistency check of the decoded model.
    pub check: ModelCheck,
    /// Node, surface, point and vertex counts.
    pub counts: BspCounts,
    /// Surfaces (triangle tags index this list).
    pub surfaces: Vec<SurfaceInfo>,
    /// Triangles of drawn surfaces (TENTATIVE classification).
    pub visible: TriangleMesh,
    /// Triangles of blocking surfaces (TENTATIVE classification).
    pub collision: TriangleMesh,
}

/// Size of a BSP model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct BspCounts {
    /// Nodes.
    pub nodes: usize,
    /// Nodes with a polygon.
    pub polygon_nodes: usize,
    /// Surfaces.
    pub surfs: usize,
    /// Points.
    pub points: usize,
    /// Vectors.
    pub vectors: usize,
    /// Polygon vertex references.
    pub verts: usize,
    /// Zones.
    pub zones: usize,
    /// Render vertices.
    pub render_vertices: usize,
}

/// Counts of a decoded model.
pub fn bsp_counts(m: &Model) -> BspCounts {
    BspCounts {
        nodes: m.nodes.len(),
        polygon_nodes: m.nodes.iter().filter(|n| n.num_vertices > 0).count(),
        surfs: m.surfs.len(),
        points: m.points.len(),
        vectors: m.vectors.len(),
        verts: m.verts.len(),
        zones: m.zones.len(),
        render_vertices: m.vertex_buffer.len(),
    }
}

/// Decode `model` (a `Model` export of `lp`) and build its geometry.
pub fn extract_bsp(set: &PackageSet, lp: &LoadedPackage, model: usize) -> ObjResult<BspGeometry> {
    let (obj, m) = bsp::decode_model(&lp.package, Some(&lp.name), model, set)?;
    let at = |i: i32, list: &[Vec3]| {
        usize::try_from(i)
            .ok()
            .and_then(|i| list.get(i).copied())
            .unwrap_or([0.0; 3])
    };
    let surfaces = m
        .surfs
        .iter()
        .map(|s| SurfaceInfo {
            material: lp.ref_path(s.material).ok().flatten(),
            poly_flags: s.poly_flags,
            base: at(s.base, &m.points),
            normal: at(s.normal, &m.vectors),
            texture_u: at(s.texture_u, &m.vectors),
            texture_v: at(s.texture_v, &m.vectors),
            actor: lp.ref_path(s.actor).ok().flatten(),
            brush_poly: s.brush_poly,
            plane: s.plane,
        })
        .collect();
    Ok(BspGeometry {
        model: obj.path,
        bounds: m.bounds,
        check: m.check(),
        counts: bsp_counts(&m),
        surfaces,
        visible: m.triangulate(|_, s| bsp::is_visible_surface(s)),
        collision: m.triangulate(|_, s| bsp::is_collision_surface(s)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3)
    }

    #[test]
    fn rotator_table_matches_quarter_turns() {
        let (s, c) = rotator_sin_cos(16384);
        assert!((s - 1.0).abs() < 1e-12 && c.abs() < 1e-12);
        let (s, c) = rotator_sin_cos(-16384);
        assert!((s + 1.0).abs() < 1e-12 && c.abs() < 1e-12);
        // Quantized to 4 units: 1..3 behave like 0.
        assert_eq!(rotator_sin_cos(3), rotator_sin_cos(0));
        assert_eq!(rotator_sin_cos(65536 + 8192), rotator_sin_cos(8192));
    }

    #[test]
    fn actor_transform_applies_prepivot_scale_rotation_location() {
        // Yaw 90°: +X → +Y.
        let m = actor_local_to_world(
            [100.0, 0.0, 0.0],
            [0, 16384, 0],
            2.0,
            [1.0, 1.0, 1.0],
            [0.0; 3],
        );
        assert!(close(
            transform_point(&m, [1.0, 0.0, 0.0]),
            [100.0, 2.0, 0.0]
        ));
        // PrePivot is subtracted before scaling.
        let m = actor_local_to_world([0.0; 3], [0, 0, 0], 1.0, [2.0, 3.0, 4.0], [1.0, 1.0, 1.0]);
        assert!(close(transform_point(&m, [1.0, 1.0, 1.0]), [0.0, 0.0, 0.0]));
        assert!(close(transform_point(&m, [2.0, 2.0, 2.0]), [2.0, 3.0, 4.0]));
        // Pitch 90°: +X → +Z.
        let m = actor_local_to_world([0.0; 3], [16384, 0, 0], 1.0, [1.0; 3], [0.0; 3]);
        assert!(close(transform_point(&m, [1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]));
        // Roll 90°: +Y → −Z? (UE3: roll about X; Y axis row = (.., .., -SR·CP)).
        let m = actor_local_to_world([0.0; 3], [0, 0, 16384], 1.0, [1.0; 3], [0.0; 3]);
        assert!(close(
            transform_point(&m, [0.0, 1.0, 0.0]),
            [0.0, 0.0, -1.0]
        ));
    }

    #[test]
    fn component_transform_composes_with_parent() {
        let parent = actor_local_to_world([10.0, 0.0, 0.0], [0, 16384, 0], 1.0, [1.0; 3], [0.0; 3]);
        let c = ComponentTransform {
            translation: [5.0, 0.0, 0.0],
            ..ComponentTransform::default()
        };
        let m = component_local_to_world(&c, &parent);
        assert!(close(transform_point(&m, [0.0; 3]), [10.0, 5.0, 0.0]));
        let abs = ComponentTransform {
            absolute_translation: true,
            ..c
        };
        let m = component_local_to_world(&abs, &parent);
        assert!(close(transform_point(&m, [0.0; 3]), [0.0, 5.0, 0.0]));
        let scaled = actor_local_to_world([0.0; 3], [0, 16384, 0], 3.0, [1.0; 3], [0.0; 3]);
        let rot_only = ComponentTransform {
            absolute_rotation: true,
            ..ComponentTransform::default()
        };
        let m = component_local_to_world(&rot_only, &scaled);
        assert!(close(transform_point(&m, [1.0, 0.0, 0.0]), [3.0, 0.0, 0.0]));
        let scale_only = ComponentTransform {
            absolute_scale: true,
            ..ComponentTransform::default()
        };
        let m = component_local_to_world(&scale_only, &scaled);
        assert!(close(transform_point(&m, [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    #[test]
    fn classification() {
        let chain = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_actor(&chain(&[
                "asamudynamickillzone",
                "dynamictriggervolume",
                "triggervolume",
                "volume",
                "brush",
                "actor",
                "object"
            ])),
            ActorKind::DynamicKillZone
        );
        assert_eq!(
            classify_actor(&chain(&[
                "asamufallingrock",
                "interpactor",
                "dynamicsmactor",
                "actor",
                "object"
            ])),
            ActorKind::FallingRock
        );
        assert_eq!(
            classify_actor(&chain(&[
                "blockingvolume",
                "volume",
                "brush",
                "actor",
                "object"
            ])),
            ActorKind::BlockingVolume
        );
        assert_eq!(
            classify_actor(&chain(&["brush", "actor", "object"])),
            ActorKind::Brush
        );
        assert_eq!(
            classify_component(&chain(&[
                "pointlightcomponent",
                "lightcomponent",
                "actorcomponent",
                "component",
                "object"
            ])),
            ComponentKind::Light
        );
    }

    #[test]
    fn merge_is_member_wise_for_tagged_structs() {
        let f = |n: &str, v: Value| Property {
            name: n.to_owned(),
            type_name: String::new(),
            array_index: 0,
            size: 0,
            struct_name: None,
            enum_name: None,
            value: v,
            offset: 0,
        };
        let s = |fields: Vec<Property>, binary: bool| Value::Struct {
            name: "S".to_owned(),
            binary,
            fields,
        };
        let mut base = vec![f(
            "A",
            s(vec![f("X", Value::Int(1)), f("Y", Value::Int(2))], false),
        )];
        merge_properties(
            &mut base,
            &[
                f("A", s(vec![f("Y", Value::Int(5))], false)),
                f("B", Value::Bool(true)),
            ],
        );
        assert_eq!(base.len(), 2);
        let a = prop(&base, "a").unwrap();
        assert_eq!(member(a, "X"), Some(&Value::Int(1)));
        assert_eq!(member(a, "Y"), Some(&Value::Int(5)));
        // Binary structs replace wholesale.
        let mut base = vec![f(
            "V",
            s(
                vec![f("X", Value::Float(1.0)), f("Y", Value::Float(2.0))],
                true,
            ),
        )];
        merge_properties(
            &mut base,
            &[f("V", s(vec![f("X", Value::Float(3.0))], true))],
        );
        assert_eq!(member(prop(&base, "V").unwrap(), "Y"), None);
        // Case-insensitive names; static-array elements stay apart; a key
        // repeated in `from` merges into the entry it created.
        let mut base = vec![f("Speed", Value::Int(1))];
        let mut second = f("speed", Value::Int(7));
        second.array_index = 1;
        merge_properties(
            &mut base,
            &[
                f("SPEED", Value::Int(2)),
                second,
                f("New", Value::Int(3)),
                f("new", Value::Int(4)),
            ],
        );
        assert_eq!(base.len(), 3);
        assert_eq!(prop(&base, "speed"), Some(&Value::Int(2)));
        assert_eq!(base[1].array_index, 1);
        assert_eq!(prop(&base, "NEW"), Some(&Value::Int(4)));
        assert!(has_repeated_keys(&[
            f("A", Value::Int(1)),
            f("a", Value::Int(2))
        ]));
        assert!(!has_repeated_keys(&base));
    }

    fn p(n: &str, v: Value) -> Property {
        Property {
            name: n.to_owned(),
            type_name: String::new(),
            array_index: 0,
            size: 0,
            struct_name: None,
            enum_name: None,
            value: v,
            offset: 0,
        }
    }

    #[test]
    fn merge_is_linear_on_large_inputs() {
        // 50,000 over 50,000 distinct keys would take ~2.5e9 string
        // comparisons with a linear search per key.
        let mut base: Vec<Property> = (0..50_000)
            .map(|i| p(&format!("P{i}"), Value::Int(i)))
            .collect();
        let over: Vec<Property> = (25_000..75_000)
            .map(|i| p(&format!("p{i}"), Value::Int(-i)))
            .collect();
        let t = std::time::Instant::now();
        merge_properties(&mut base, &over);
        assert!(t.elapsed().as_secs() < 5);
        assert_eq!(base.len(), 75_000);
        assert_eq!(base[30_000].value, Value::Int(-30_000));
        assert_eq!(base[60_000].name, "p60000");
    }

    #[test]
    fn weights_count_values_and_text() {
        let v = Value::Array(vec![Value::Int(1), Value::Str("x".repeat(320))]);
        assert_eq!(value_weight(&v), 1 + 1 + 11);
        let props = vec![p("A", v), p("B", Value::Bool(true))];
        assert_eq!(property_weight(&props), (1 + 13) + (1 + 1));
        let s = Value::Struct {
            name: "S".to_owned(),
            binary: false,
            fields: props,
        };
        assert_eq!(value_weight(&s), 1 + 16);
    }

    fn vecv(x: f32, y: f32, z: f32) -> Value {
        Value::Struct {
            name: "Vector".to_owned(),
            binary: true,
            fields: vec![
                p("X", Value::Float(x)),
                p("Y", Value::Float(y)),
                p("Z", Value::Float(z)),
            ],
        }
    }

    fn plane(x: f32, y: f32, z: f32, w: f32) -> Value {
        Value::Struct {
            name: "Plane".to_owned(),
            binary: true,
            fields: vec![
                p("W", Value::Float(w)),
                p("X", Value::Float(x)),
                p("Y", Value::Float(y)),
                p("Z", Value::Float(z)),
            ],
        }
    }

    fn elem(verts: Vec<Value>, tris: Vec<Value>, planes: Vec<Value>) -> Value {
        Value::Struct {
            name: "KConvexElem".to_owned(),
            binary: false,
            fields: vec![
                p("VertexData", Value::Array(verts)),
                p("FaceTriData", Value::Array(tris)),
                p("FacePlaneData", Value::Array(planes)),
            ],
        }
    }

    #[test]
    fn hulls_keep_only_consistent_triangles() {
        let verts = || {
            vec![
                vecv(0.0, 0.0, 0.0),
                vecv(1.0, 0.0, 0.0),
                vecv(0.0, 1.0, 0.0),
                vecv(0.0, 0.0, 1.0),
            ]
        };
        let ints = |v: &[i32]| v.iter().map(|&i| Value::Int(i)).collect::<Vec<_>>();
        let m = actor_local_to_world([10.0, 0.0, 0.0], [0, 16384, 0], 2.0, [1.0; 3], [0.0; 3]);
        // Valid hull: one triangle in range, one out of range, one negative.
        let e = elem(
            verts(),
            ints(&[0, 1, 2, 0, 1, 9, -1, 2, 3]),
            vec![plane(0.0, 0.0, 1.0, 1.0)],
        );
        let h = hull_from_elem(&e, &m).unwrap();
        assert_eq!(h.triangles, vec![[0, 1, 2]]);
        assert_eq!(h.vertices[1], [10.0, 2.0, 0.0]);
        // Plane z = 1 (local) → z = 2 (world, scale 2), unit normal kept.
        assert_eq!(h.planes.len(), 1);
        let [x, y, z, w] = h.planes[0];
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6 && (z - 1.0).abs() < 1e-6);
        assert!((w - 2.0).abs() < 1e-4, "{w}");
        // Index count not a multiple of 3, or a non-integer index: no
        // triangles (the rest would be misaligned).
        let e = elem(verts(), ints(&[0, 1, 2, 3]), Vec::new());
        assert!(hull_from_elem(&e, &m).unwrap().triangles.is_empty());
        let mut tris = ints(&[0, 1, 2, 0, 2, 3]);
        tris[1] = Value::Float(1.0);
        let e = elem(verts(), tris, Vec::new());
        assert!(hull_from_elem(&e, &m).unwrap().triangles.is_empty());
        // A vertex that is not a vector: vertices kept, triangles dropped.
        let mut v = verts();
        v[0] = Value::Int(5);
        let e = elem(v, ints(&[0, 1, 2]), Vec::new());
        let h = hull_from_elem(&e, &m).unwrap();
        assert_eq!((h.vertices.len(), h.triangles.len()), (3, 0));
        // No vertices: no hull. Non-uniform scale: no planes.
        assert!(hull_from_elem(&elem(Vec::new(), Vec::new(), Vec::new()), &m).is_none());
        let skew = actor_local_to_world([0.0; 3], [0; 3], 1.0, [1.0, 2.0, 1.0], [0.0; 3]);
        let e = elem(verts(), Vec::new(), vec![plane(0.0, 0.0, 1.0, 1.0)]);
        assert!(hull_from_elem(&e, &skew).unwrap().planes.is_empty());
        assert_eq!(hull_size(&e), 4 + 1);
        // NaN transforms never panic.
        let nan = actor_local_to_world([f32::NAN; 3], [0; 3], f32::NAN, [1.0; 3], [0.0; 3]);
        let _ = hull_from_elem(&e, &nan);
    }
}
