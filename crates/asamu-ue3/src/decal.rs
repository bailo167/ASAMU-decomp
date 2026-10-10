//! Decals: `DecalActor` / `DecalComponent` decoding, the decal frame and
//! projection math, and per-map decal extraction (see
//! `docs/reverse-engineering/VFX_DECALS.md`).
//!
//! # `DecalComponent` native data — CONFIRMED (exact consumption, every map)
//!
//! The native serializer was read locally in the unstripped Mac executable
//! (`UDecalComponent::Serialize`, the `FStaticReceiverData` and
//! `FDecalVertex` `operator<<`s; decompiled output stays in the ignored
//! `research/`). `UPrimitiveComponent::Serialize` writes nothing to disk in
//! this build (LIGHTMAPS.md), so after the tagged properties a decal
//! component stores:
//!
//! ```text
//! i32 Count                        static receivers
//! Count × FStaticReceiverData:
//!   obj Component                  the receiving primitive component
//!   bulk TArray<FDecalVertex>      i32 ElementSize (28), i32 Count, elements
//!   bulk TArray<u16> Indices       i32 ElementSize (2), i32 Count, elements
//!   u32 NumTriangles
//!   light map reference            (lightmap.rs: u32 type + FLightMap1D/2D)
//!   TArray<obj> ShadowMap1D        (file version > 665)
//!   i32 Data                       (file version > 620)
//!   i32 InstanceIndex              (file version > 664; 0 in every shipped receiver)
//! FDecalVertex (28 bytes at v868): FVector Position, FPackedNormal TangentX,
//!   FPackedNormal TangentZ, FVector2D LegacyLightMapCoordinate
//! ```
//!
//! Field names are UE3 conventions (TENTATIVE where only the layout is
//! proven); order and sizes are CONFIRMED by exact consumption and a
//! byte-for-byte re-encode ([`encode_decal_component_native`]).
//!
//! # Decal frame (from the native `UDecalComponent::UpdateOrthoPlanes`)
//!
//! A decal is a box in front of its location: the projection direction is
//! the orientation's forward axis `D` (`Orientation.Vector()`); the width
//! axis is the orientation's Y axis turned by `DecalRotation` degrees about
//! `D` towards its Z axis, the height axis the Z axis turned the same way.
//! Points inside satisfy `|W·(p−L)| ≤ Width/2`, `|H·(p−L)| ≤ Height/2` and
//! `Near ≤ D·(p−L) ≤ Far` ([`DecalFrame`]). CONFIRMED (native) for the plane
//! construction; the texture coordinates of [`DecalFrame::uv`] follow the
//! native decal matrix and the shipped decal vertex shaders.
//!
//! **Mirrored owners.** The same function sets `bFlipBackfaceDirection` when
//! the decal is static (`bStaticDecal`) and its owner's `DrawScale3D` has a
//! negative product, and then negates the stored normal: the near and far
//! planes and the face test follow it, so such a decal projects along `−D`
//! ([`decal_is_mirrored`], [`DecalFrame::mirrored`]); `W` and `H` stay.
//! CONFIRMED (native; one shipped decal, whose stored `HitNormal` is `+D`
//! and whose receivers only lie in the mirrored box).
//!
//! # Receivers
//!
//! A decal with cooked `FStaticReceiverData` attaches to exactly those
//! (`AttachToStaticReceivers`). Every other decal computes its receivers
//! when play begins (`UDecalComponent::BeginPlay` → `ComputeReceivers`): the
//! primitives of the owner's level that the world's collision hash returns
//! for the decal's bounds and that accept the decal, are not hidden and
//! pass the filter; on each, the triangles of the mesh's collision kDOP
//! tree that face the decal are clipped to the box
//! (`UStaticMeshComponent::GenerateDecalRenderData`). The serialized
//! `DecalReceivers` list is only the editor's last result; it is cleared on
//! attach. [`DecalProjector::world_receivers`] models that query for static
//! mesh components. CONFIRMED (native) for the control flow; STRONG for the
//! query's contents (VFX_DECALS.md §8.5).
//!
//! Decal data is original game data: keep converted output user-local.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::bsp::{Vec3, read_vec3};
use crate::level::{
    ComponentTransform, MAX_ARCHETYPE_DEPTH, Mat4, actor_local_to_world, as_f32, as_i32,
    as_rotator, as_vec3, component_local_to_world, decode_level, level_exports, member,
    merge_properties, prop, rotator_sin_cos, transform_point,
};
use crate::lightmap::{LightMap, encode_light_map, read_light_map};
use crate::model::{LoadedPackage, PackageSet};
use crate::object::{ObjResult, ObjectError, export_class_path};
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::PackageIndex;
use crate::writer::Writer;

/// Serialized size of one `FDecalVertex` at file version 868.
pub const DECAL_VERTEX_SIZE: usize = 28;
/// Serialized size of one decal index (`u16`).
pub const DECAL_INDEX_SIZE: usize = 2;
/// Smallest serialized `FStaticReceiverData`: component, two empty bulk
/// arrays, triangle count, a light map reference of type none, an empty
/// shadow map array, `Data`, `InstanceIndex`.
pub const STATIC_RECEIVER_MIN_SIZE: usize = 4 + 8 + 8 + 4 + 4 + 4 + 4 + 4;
/// `format` field of the per-map decal JSON (`asamu-import decals`).
pub const DECALS_FORMAT: &str = "asamu-decals";
/// `version` field of the per-map decal JSON.
pub const DECALS_VERSION: u32 = 1;
/// Most decal triangles extracted per map package (hostile-input bound; the
/// largest shipped map has 32,601).
pub const MAX_MAP_DECAL_TRIANGLES: usize = 1 << 22;
/// Most receiver triangles one [`DecalProjector`] tests (hostile-input
/// bound on the projection work).
pub const MAX_PROJECTION_WORK: usize = 1 << 28;
/// A cooked receiver counts as unclipped when one of its vertices lies more
/// than this outside the decal box, UU (clipped receivers stay within about
/// 8 UU: the cooker clips in the receiver's scaled local space).
pub const UNCLIPPED_TOLERANCE: f32 = 16.0;
/// Most vertices accepted per static receiver (hostile-input bound; the
/// shipped maximum is far lower, see VFX_DECALS.md).
pub const MAX_RECEIVER_VERTICES: usize = 1 << 20;
/// Most receiving components projected per decal (hostile-input bound; the
/// shipped maximum is a few dozen).
pub const MAX_DECAL_RECEIVERS: usize = 4096;
/// Most components [`DecalProjector::world_receivers`] returns per map
/// (hostile-input bound; the largest shipped map has about 5,400).
pub const MAX_WORLD_RECEIVERS: usize = 1 << 18;
/// Class of the components the run-time receiver query is modelled for.
pub const STATIC_MESH_COMPONENT_CLASS: &str = "Engine.StaticMeshComponent";

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// Native data.
// ---------------------------------------------------------------------------

/// One `FDecalVertex` (v868 layout).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DecalVertex {
    /// `Position`, in the receiving component's local space (CONFIRMED on
    /// the shipped receivers, VFX_DECALS.md §8.3).
    pub position: Vec3,
    /// `TangentX` (packed normal: 4 bytes, `x/127.5 − 1`).
    pub tangent_x: [u8; 4],
    /// `TangentZ` (packed normal).
    pub tangent_z: [u8; 4],
    /// `LegacyLightMapCoordinate`.
    pub light_map_coordinate: [f32; 2],
}

/// One `FStaticReceiverData`: the decal geometry the cooker clipped onto a
/// receiving primitive.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticReceiver {
    /// `Component`: the receiving primitive.
    pub component: PackageIndex,
    /// `Vertices`.
    pub vertices: Vec<DecalVertex>,
    /// `Indices` (triangle list).
    pub indices: Vec<u16>,
    /// `NumTriangles`.
    pub num_triangles: u32,
    /// The receiver's vertex light map for the decal.
    pub light_map: LightMap,
    /// `ShadowMap1D`.
    pub shadow_maps: Vec<PackageIndex>,
    /// `Data` (UE3: receiver-specific data, TENTATIVE).
    pub data: i32,
    /// `InstanceIndex` (the constructor's value is −1; 0 in every shipped
    /// receiver).
    pub instance_index: i32,
}

/// `DecalComponent` native data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecalComponentNative {
    /// Static receivers.
    pub receivers: Vec<StaticReceiver>,
}

fn read_decal_vertex(r: &mut Reader<'_>) -> ObjResult<DecalVertex> {
    let position = read_vec3(r)?;
    let mut tangent_x = [0u8; 4];
    tangent_x.copy_from_slice(r.read_bytes(4)?);
    let mut tangent_z = [0u8; 4];
    tangent_z.copy_from_slice(r.read_bytes(4)?);
    let light_map_coordinate = [r.read_f32()?, r.read_f32()?];
    Ok(DecalVertex {
        position,
        tangent_x,
        tangent_z,
        light_map_coordinate,
    })
}

/// Reads a native bulk array header (`i32 ElementSize`, `i32 Count`) and
/// checks the element size and that the elements fit.
fn read_bulk_header(
    r: &mut Reader<'_>,
    what: &'static str,
    element_size: usize,
    max: usize,
) -> ObjResult<usize> {
    let at = r.position();
    let raw = r.read_i32()?;
    if usize::try_from(raw).ok() != Some(element_size) {
        return Err(malformed(
            what,
            at,
            format!("bulk element size {raw}, expected {element_size}"),
        ));
    }
    let count = r.read_count(what, element_size)?;
    if count > max {
        return Err(malformed(
            what,
            at,
            format!("{count} elements exceed {max}"),
        ));
    }
    Ok(count)
}

/// Reads one `FStaticReceiverData`.
pub fn read_static_receiver(r: &mut Reader<'_>) -> ObjResult<StaticReceiver> {
    let component = r.read_package_index()?;
    let nv = read_bulk_header(
        r,
        "FStaticReceiverData.Vertices",
        DECAL_VERTEX_SIZE,
        MAX_RECEIVER_VERTICES,
    )?;
    let mut vertices = Vec::with_capacity(nv);
    for _ in 0..nv {
        vertices.push(read_decal_vertex(r)?);
    }
    let ni = read_bulk_header(
        r,
        "FStaticReceiverData.Indices",
        DECAL_INDEX_SIZE,
        MAX_RECEIVER_VERTICES.saturating_mul(6),
    )?;
    let mut indices = Vec::with_capacity(ni);
    for _ in 0..ni {
        indices.push(r.read_u16()?);
    }
    let num_triangles = r.read_u32()?;
    let light_map = read_light_map(r)?;
    let shadow_maps = r.read_tarray("FStaticReceiverData.ShadowMap1D", 4, |r| {
        r.read_package_index()
    })?;
    let data = r.read_i32()?;
    let instance_index = r.read_i32()?;
    Ok(StaticReceiver {
        component,
        vertices,
        indices,
        num_triangles,
        light_map,
        shadow_maps,
        data,
        instance_index,
    })
}

/// Decodes the native tail of a `DecalComponent` payload from `start`
/// (the end of its tagged properties). Must consume `data` exactly.
pub fn decode_decal_component_native(data: &[u8], start: usize) -> ObjResult<DecalComponentNative> {
    let mut r = Reader::at(data, start)?;
    let count = r.read_count("DecalComponent.StaticReceivers", STATIC_RECEIVER_MIN_SIZE)?;
    let mut receivers = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        receivers.push(read_static_receiver(&mut r)?);
    }
    if r.remaining() != 0 {
        return Err(malformed(
            "DecalComponent native data",
            r.position(),
            format!("{} bytes left after the static receivers", r.remaining()),
        ));
    }
    Ok(DecalComponentNative { receivers })
}

fn put_count(w: &mut Writer, n: usize) -> Option<()> {
    w.i32(i32::try_from(n).ok()?);
    Some(())
}

/// Encodes `DecalComponent` native data (inverse of
/// [`decode_decal_component_native`]). `native_base` is the absolute stream
/// offset of the native data (the export's `SerialOffset` plus the payload
/// offset): the vertex light maps' bulk sample offsets are recomputed from
/// it, as the cooker wrote them; `None` keeps the decoded offsets.
pub fn encode_decal_component_native(
    n: &DecalComponentNative,
    native_base: Option<i64>,
) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    put_count(&mut w, n.receivers.len())?;
    for rec in &n.receivers {
        w.i32(rec.component.0);
        put_count(&mut w, DECAL_VERTEX_SIZE)?;
        put_count(&mut w, rec.vertices.len())?;
        for v in &rec.vertices {
            for c in v.position {
                w.bytes(&c.to_le_bytes());
            }
            w.bytes(&v.tangent_x);
            w.bytes(&v.tangent_z);
            for c in v.light_map_coordinate {
                w.bytes(&c.to_le_bytes());
            }
        }
        put_count(&mut w, DECAL_INDEX_SIZE)?;
        put_count(&mut w, rec.indices.len())?;
        for i in &rec.indices {
            w.u16(*i);
        }
        w.u32(rec.num_triangles);
        let base = match native_base {
            Some(b) => Some(b.checked_add(i64::try_from(w.len()).ok()?)?),
            None => None,
        };
        let mut lm = Writer::new();
        encode_light_map(&mut lm, &rec.light_map, base)?;
        w.bytes(&lm.into_bytes());
        put_count(&mut w, rec.shadow_maps.len())?;
        for s in &rec.shadow_maps {
            w.i32(s.0);
        }
        w.i32(rec.data);
        w.i32(rec.instance_index);
    }
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Effective values (object over archetype / template / class defaults).
// ---------------------------------------------------------------------------

/// Merges objects over their archetypes (component templates) and class
/// defaults, with caching: the delta rule of LEVEL_FORMAT.md, followed for
/// at most [`MAX_ARCHETYPE_DEPTH`] links.
pub struct Effective<'a> {
    set: &'a PackageSet,
    defaults: HashMap<String, Arc<Vec<Property>>>,
    cache: HashMap<(String, usize), Arc<Vec<Property>>>,
    /// Non-fatal problems met while resolving.
    pub warnings: Vec<String>,
}

impl<'a> Effective<'a> {
    /// A resolver over `set`.
    #[must_use]
    pub fn new(set: &'a PackageSet) -> Self {
        Self {
            set,
            defaults: HashMap::new(),
            cache: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    fn class_defaults(&mut self, class: &str) -> Arc<Vec<Property>> {
        let key = class.to_ascii_lowercase();
        if let Some(v) = self.defaults.get(&key) {
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
                self.warnings
                    .push(format!("class defaults of {class} unavailable: {e}"));
                Vec::new()
            }
        };
        let v = Arc::new(props);
        self.defaults.insert(key, v.clone());
        v
    }

    /// Effective properties of export `index` of `lp`.
    pub fn of(&mut self, lp: &Arc<LoadedPackage>, index: usize) -> Arc<Vec<Property>> {
        self.resolve(lp, index, 0, true)
    }

    /// Like [`Effective::of`], without keeping the result (its archetypes
    /// are still cached): for one pass over many objects.
    pub fn of_transient(&mut self, lp: &Arc<LoadedPackage>, index: usize) -> Arc<Vec<Property>> {
        self.resolve(lp, index, 0, false)
    }

    fn resolve(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
        depth: usize,
        keep: bool,
    ) -> Arc<Vec<Property>> {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let own = match self.set.decode(lp, index) {
            Ok(o) => o.properties,
            Err(e) => {
                self.warnings
                    .push(format!("{}: export {index} undecodable: {e}", lp.name));
                Vec::new()
            }
        };
        let archetype = lp
            .package
            .export(index)
            .map(|e| e.archetype_index)
            .unwrap_or_default();
        let mut base = None;
        if !archetype.is_null() {
            if depth >= MAX_ARCHETYPE_DEPTH {
                self.warnings.push(format!(
                    "{}: export {index}: archetype chain longer than {MAX_ARCHETYPE_DEPTH}",
                    lp.name
                ));
            } else if let Ok(Some(path)) = lp.ref_path(archetype) {
                match self.set.locate(&path) {
                    Some((alp, ai)) => base = Some(self.resolve(&alp, ai, depth + 1, true)),
                    None => self
                        .warnings
                        .push(format!("{}: archetype {path} not found", lp.name)),
                }
            }
        }
        let base = match base {
            Some(b) => b,
            None => {
                let class =
                    export_class_path(&lp.package, Some(&lp.name), index).unwrap_or_default();
                self.class_defaults(&class)
            }
        };
        let merged = if own.is_empty() {
            base
        } else {
            let mut m = (*base).clone();
            merge_properties(&mut m, &own);
            Arc::new(m)
        };
        if keep {
            self.cache.insert(key, merged.clone());
        }
        merged
    }
}

// ---------------------------------------------------------------------------
// Decal parameters.
// ---------------------------------------------------------------------------

fn f32_or(props: &[Property], name: &str, default: f32) -> f32 {
    prop(props, name).and_then(as_f32).unwrap_or(default)
}

fn bool_of(props: &[Property], name: &str) -> bool {
    matches!(prop(props, name), Some(Value::Bool(true)))
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Name(s) | Value::Str(s) | Value::Enum(s) => Some(s.clone()),
        _ => None,
    }
}

fn object_of(v: &Value) -> Option<String> {
    match v {
        Value::Object(o) if o.index != 0 => Some(o.path.clone()),
        _ => None,
    }
}

/// The `DecalComponent` values a renderer needs (effective values: the
/// instance over its template chain and the class defaults).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecalParams {
    /// `DecalMaterial` (`None`: the engine's default decal material).
    pub material: Option<String>,
    /// `Width`, UU.
    pub width: f32,
    /// `Height`, UU.
    pub height: f32,
    /// `TileX`.
    pub tile_x: f32,
    /// `TileY`.
    pub tile_y: f32,
    /// `OffsetX`.
    pub offset_x: f32,
    /// `OffsetY`.
    pub offset_y: f32,
    /// `DecalRotation`, degrees.
    pub rotation_degrees: f32,
    /// `NearPlane`, UU along the projection direction.
    pub near_plane: f32,
    /// `FarPlane`, UU along the projection direction.
    pub far_plane: f32,
    /// `FieldOfView` (perspective decals; 80 by default, unused by ortho).
    pub field_of_view: f32,
    /// `bNoClip`.
    pub no_clip: bool,
    /// `bStaticDecal`.
    pub static_decal: bool,
    /// `bMovableDecal`.
    pub movable_decal: bool,
    /// `bProjectOnBackfaces`.
    pub project_on_backfaces: bool,
    /// `bProjectOnHidden`.
    pub project_on_hidden: bool,
    /// `bProjectOnBSP`.
    pub project_on_bsp: bool,
    /// `bProjectOnStaticMeshes`.
    pub project_on_static_meshes: bool,
    /// `bProjectOnSkeletalMeshes`.
    pub project_on_skeletal_meshes: bool,
    /// `bProjectOnTerrain`.
    pub project_on_terrain: bool,
    /// `bFlipBackfaceDirection`.
    pub flip_backface_direction: bool,
    /// `DepthBias`.
    pub depth_bias: f32,
    /// `SlopeScaleDepthBias`.
    pub slope_scale_depth_bias: f32,
    /// `SortOrder`.
    pub sort_order: i32,
    /// `BackfaceAngle`.
    pub backface_angle: f32,
    /// `BlendRange` (degrees: start, end of the fade by surface angle).
    pub blend_range: [f32; 2],
    /// `DecalTransform` enumerator.
    pub decal_transform: Option<String>,
    /// `FilterMode` enumerator.
    pub filter_mode: Option<String>,
    /// `Filter` actors.
    pub filter: Vec<String>,
    /// `DecalReceivers[].Component`: components the editor attached it to.
    pub receivers: Vec<String>,
    /// `HitLocation`.
    pub hit_location: Vec3,
    /// `HitNormal`.
    pub hit_normal: Vec3,
    /// `HitTangent`.
    pub hit_tangent: Vec3,
    /// `HitBinormal`.
    pub hit_binormal: Vec3,
    /// `ParentRelativeLocation`.
    pub parent_relative_location: Vec3,
    /// `ParentRelativeOrientation`.
    pub parent_relative_orientation: [i32; 3],
    /// `HiddenGame`.
    pub hidden_game: bool,
}

/// Reads [`DecalParams`] from effective `DecalComponent` properties. Values
/// absent from `props` take the native zero (the class defaults are
/// expected to be merged in already).
#[must_use]
pub fn decal_params(props: &[Property]) -> DecalParams {
    let vec = |n: &str| prop(props, n).and_then(as_vec3).unwrap_or([0.0; 3]);
    let objects = |n: &str| -> Vec<String> {
        match prop(props, n) {
            Some(Value::Array(items)) => items.iter().filter_map(object_of).collect(),
            _ => Vec::new(),
        }
    };
    let receivers = match prop(props, "DecalReceivers") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|it| member(it, "Component").and_then(object_of))
            .collect(),
        _ => Vec::new(),
    };
    let blend = prop(props, "BlendRange");
    DecalParams {
        material: prop(props, "DecalMaterial").and_then(object_of),
        width: f32_or(props, "Width", 0.0),
        height: f32_or(props, "Height", 0.0),
        tile_x: f32_or(props, "TileX", 0.0),
        tile_y: f32_or(props, "TileY", 0.0),
        offset_x: f32_or(props, "OffsetX", 0.0),
        offset_y: f32_or(props, "OffsetY", 0.0),
        rotation_degrees: f32_or(props, "DecalRotation", 0.0),
        near_plane: f32_or(props, "NearPlane", 0.0),
        far_plane: f32_or(props, "FarPlane", 0.0),
        field_of_view: f32_or(props, "FieldOfView", 0.0),
        no_clip: bool_of(props, "bNoClip"),
        static_decal: bool_of(props, "bStaticDecal"),
        movable_decal: bool_of(props, "bMovableDecal"),
        project_on_backfaces: bool_of(props, "bProjectOnBackfaces"),
        project_on_hidden: bool_of(props, "bProjectOnHidden"),
        project_on_bsp: bool_of(props, "bProjectOnBSP"),
        project_on_static_meshes: bool_of(props, "bProjectOnStaticMeshes"),
        project_on_skeletal_meshes: bool_of(props, "bProjectOnSkeletalMeshes"),
        project_on_terrain: bool_of(props, "bProjectOnTerrain"),
        flip_backface_direction: bool_of(props, "bFlipBackfaceDirection"),
        depth_bias: f32_or(props, "DepthBias", 0.0),
        slope_scale_depth_bias: f32_or(props, "SlopeScaleDepthBias", 0.0),
        sort_order: prop(props, "SortOrder").and_then(as_i32).unwrap_or(0),
        backface_angle: f32_or(props, "BackfaceAngle", 0.0),
        blend_range: [
            blend
                .and_then(|b| member(b, "X"))
                .and_then(as_f32)
                .unwrap_or(0.0),
            blend
                .and_then(|b| member(b, "Y"))
                .and_then(as_f32)
                .unwrap_or(0.0),
        ],
        decal_transform: prop(props, "DecalTransform").and_then(text_of),
        filter_mode: prop(props, "FilterMode").and_then(text_of),
        filter: objects("Filter"),
        receivers,
        hit_location: vec("HitLocation"),
        hit_normal: vec("HitNormal"),
        hit_tangent: vec("HitTangent"),
        hit_binormal: vec("HitBinormal"),
        parent_relative_location: vec("ParentRelativeLocation"),
        parent_relative_orientation: prop(props, "ParentRelativeOrientation")
            .and_then(as_rotator)
            .unwrap_or([0; 3]),
        hidden_game: bool_of(props, "HiddenGame"),
    }
}

// ---------------------------------------------------------------------------
// Frame math (pure).
// ---------------------------------------------------------------------------

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn normalize(v: Vec3) -> Vec3 {
    let len = dot(v, v).sqrt();
    if len > 1e-8 && len.is_finite() {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0; 3]
    }
}

/// The orientation axes of a UE3 rotator (rows X, Y, Z of `FRotationMatrix`,
/// with the native table sine; LEVEL_FORMAT.md "Transforms").
#[must_use]
pub fn rotator_axes(rotation: [i32; 3]) -> [Vec3; 3] {
    let (sp, cp) = rotator_sin_cos(rotation[0]);
    let (sy, cy) = rotator_sin_cos(rotation[1]);
    let (sr, cr) = rotator_sin_cos(rotation[2]);
    let f = |v: f64| v as f32;
    [
        [f(cp * cy), f(cp * sy), f(sp)],
        [
            f(sr * sp * cy - cr * sy),
            f(sr * sp * sy + cr * cy),
            f(-sr * cp),
        ],
        [
            f(-(cr * sp * cy + sr * sy)),
            f(cy * sr - cr * sp * sy),
            f(cr * cp),
        ],
    ]
}

/// `bFlipBackfaceDirection` as the native `UpdateOrthoPlanes` recomputes it
/// on every update: a static decal (`bStaticDecal`, which the movable decal
/// actors' template inherits too) whose owner's `DrawScale3D` has a negative
/// product. Such a decal projects along the reversed direction
/// ([`DecalFrame::mirrored`]). CONFIRMED (native); on the shipped data the
/// serialized flag equals this rule on every decal.
#[must_use]
pub fn decal_is_mirrored(static_decal: bool, owner_draw_scale3d: Vec3) -> bool {
    static_decal && owner_draw_scale3d[0] * owner_draw_scale3d[1] * owner_draw_scale3d[2] < 0.0
}

/// A decal's projection box (see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DecalFrame {
    /// Decal location `L`, UU.
    pub origin: Vec3,
    /// Projection direction `D` (unit; into the receiving surface).
    pub direction: Vec3,
    /// Width axis `W` (unit).
    pub width_axis: Vec3,
    /// Height axis `H` (unit).
    pub height_axis: Vec3,
}

impl DecalFrame {
    /// The frame of a decal at `origin` with `orientation` (rotator units)
    /// turned by `rotation_degrees` (`DecalRotation`) about its forward axis.
    #[must_use]
    pub fn new(origin: Vec3, orientation: [i32; 3], rotation_degrees: f32) -> Self {
        Self::from_axes(origin, rotator_axes(orientation), rotation_degrees)
    }

    /// The frame of a decal at `origin` whose orientation has the axes
    /// `[x, y, z]` (forward, right, up), turned by `rotation_degrees` about
    /// its forward axis.
    #[must_use]
    pub fn from_axes(origin: Vec3, axes: [Vec3; 3], rotation_degrees: f32) -> Self {
        let [x, y, z] = axes;
        let a = f64::from(rotation_degrees).to_radians();
        let (s, c) = (a.sin() as f32, a.cos() as f32);
        let width_axis = normalize([
            c * y[0] + s * z[0],
            c * y[1] + s * z[1],
            c * y[2] + s * z[2],
        ]);
        let height_axis = normalize([
            c * z[0] - s * y[0],
            c * z[1] - s * y[1],
            c * z[2] - s * y[2],
        ]);
        Self {
            origin,
            direction: normalize(x),
            width_axis,
            height_axis,
        }
    }

    /// Coordinates of `p` in the frame: (along `W`, along `H`, along `D`).
    #[must_use]
    pub fn local(&self, p: Vec3) -> Vec3 {
        let d = sub(p, self.origin);
        [
            dot(d, self.width_axis),
            dot(d, self.height_axis),
            dot(d, self.direction),
        ]
    }

    /// `true` if `p` lies inside the box of `width × height` between the near
    /// and far planes (`tolerance` UU of slack on every face).
    #[must_use]
    pub fn contains(
        &self,
        p: Vec3,
        width: f32,
        height: f32,
        near: f32,
        far: f32,
        tolerance: f32,
    ) -> bool {
        let [u, v, w] = self.local(p);
        u.abs() <= width * 0.5 + tolerance
            && v.abs() <= height * 0.5 + tolerance
            && w >= near - tolerance
            && w <= far + tolerance
    }

    /// The six bounding planes `(X, Y, Z, W)` with outward normals: a point
    /// `p` is inside when `N·p − W ≤ 0` for all of them. Order: +W, −W, +H,
    /// −H, far (+D), near (−D).
    #[must_use]
    pub fn planes(&self, width: f32, height: f32, near: f32, far: f32) -> [[f32; 4]; 6] {
        let plane = |n: Vec3, offset: f32| [n[0], n[1], n[2], dot(n, self.origin) + offset];
        let neg = |v: Vec3| [-v[0], -v[1], -v[2]];
        [
            plane(self.width_axis, width * 0.5),
            plane(neg(self.width_axis), width * 0.5),
            plane(self.height_axis, height * 0.5),
            plane(neg(self.height_axis), height * 0.5),
            plane(self.direction, far),
            plane(neg(self.direction), -near),
        ]
    }

    /// The frame of the same decal on a mirrored owner
    /// ([`decal_is_mirrored`]): the projection direction is reversed, the
    /// width and height axes stay (the native code negates only the normal,
    /// from which it builds the near and far planes).
    #[must_use]
    pub fn mirrored(mut self) -> Self {
        self.direction = [-self.direction[0], -self.direction[1], -self.direction[2]];
        self
    }

    /// `HitNormal` of the native state: the negated projection direction.
    #[must_use]
    pub fn hit_normal(&self) -> Vec3 {
        [-self.direction[0], -self.direction[1], -self.direction[2]]
    }

    /// World-space bounds `(min, max)` of the box of `width × height`
    /// between the near and far planes (its eight corners; the native
    /// `UpdateBounds` uses the same corners).
    #[must_use]
    pub fn bounds(&self, width: f32, height: f32, near: f32, far: f32) -> (Vec3, Vec3) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for sw in [-0.5f32, 0.5] {
            for sh in [-0.5f32, 0.5] {
                for d in [near, far] {
                    for k in 0..3 {
                        let c = self.origin[k]
                            + self.width_axis[k] * sw * width
                            + self.height_axis[k] * sh * height
                            + self.direction[k] * d;
                        lo[k] = lo[k].min(c);
                        hi[k] = hi[k].max(c);
                    }
                }
            }
        }
        (lo, hi)
    }

    /// `HitTangent` of the native state: the negated width axis
    /// (`UpdateOrthoPlanes` stores it so; CONFIRMED (native) and on the
    /// shipped components' stored `HitTangent`).
    #[must_use]
    pub fn tangent(&self) -> Vec3 {
        [
            -self.width_axis[0],
            -self.width_axis[1],
            -self.width_axis[2],
        ]
    }

    /// Texture coordinates of `p`.
    ///
    /// The native decal matrix (`UDecalComponent::CaptureDecalState`) maps
    /// `p − L` to `m.x` along `HitTangent · TileX / Width` (= `−W`) and
    /// `m.y` along `HitBinormal · TileY / Height` (= `+H`); the decal
    /// vertex shaders then output `−m.xy + DecalOffset + 0.5` (read in the
    /// GLSL of the shipped OpenGL shader cache: 192 vertex shaders end
    /// their decal coordinate that way). So `u` grows along `+W`, `v` along
    /// `−H`, and the box centre maps to 0.5 plus the offset. CONFIRMED
    /// (native, shader cache); that the offset uniform is `(OffsetX,
    /// OffsetY)` is STRONG (`FDecalVertexFactoryBase::SetDecalOffset`).
    #[must_use]
    pub fn uv(
        &self,
        p: Vec3,
        width: f32,
        height: f32,
        tile: [f32; 2],
        offset: [f32; 2],
    ) -> [f32; 2] {
        let [a, b, _] = self.local(p);
        let u = if width.abs() > 1e-6 {
            a / width * tile[0]
        } else {
            0.0
        };
        let v = if height.abs() > 1e-6 {
            -b / height * tile[1]
        } else {
            0.0
        };
        [u + 0.5 + offset[0], v + 0.5 + offset[1]]
    }
}

// ---------------------------------------------------------------------------
// Projection onto receiver triangles (pure).
// ---------------------------------------------------------------------------

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Clips the convex polygon `poly` against the plane `N·p − W ≤ 0`, keeping
/// the inside (Sutherland–Hodgman).
#[must_use]
pub fn clip_polygon(poly: &[Vec3], plane: [f32; 4]) -> Vec<Vec3> {
    let n = [plane[0], plane[1], plane[2]];
    let dist = |p: Vec3| dot(n, p) - plane[3];
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (i, &a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let (da, db) = (dist(a), dist(b));
        if da <= 0.0 {
            out.push(a);
        }
        if (da <= 0.0) != (db <= 0.0) {
            let t = da / (da - db);
            if t.is_finite() {
                out.push([
                    a[0] + (b[0] - a[0]) * t,
                    a[1] + (b[1] - a[1]) * t,
                    a[2] + (b[2] - a[2]) * t,
                ]);
            }
        }
    }
    out
}

/// The face normal of a receiver triangle in UE3's winding (the cooked
/// meshes wind so that `(c − a) × (b − a)` points out of the surface;
/// CONFIRMED against the shipped static receiver, VFX_DECALS.md).
#[must_use]
pub fn face_normal(tri: [Vec3; 3]) -> Vec3 {
    normalize(cross(sub(tri[2], tri[0]), sub(tri[1], tri[0])))
}

/// A decal box ready to project onto receiver triangles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecalBox {
    /// The frame.
    pub frame: DecalFrame,
    /// `Width`.
    pub width: f32,
    /// `Height`.
    pub height: f32,
    /// `NearPlane`.
    pub near: f32,
    /// `FarPlane`.
    pub far: f32,
    /// `bProjectOnBackfaces`.
    pub backfaces: bool,
    /// `bNoClip`: whole triangles are kept instead of clipped.
    pub no_clip: bool,
    /// `BackfaceAngle` (class default 0.001): a triangle receives the decal
    /// when the cosine between its outward normal and the reversed
    /// projection direction exceeds this. A negative value keeps every
    /// triangle when backfaces are projected.
    pub backface_angle: f32,
}

impl DecalBox {
    /// The box of a decal with `params` at `frame`.
    #[must_use]
    pub fn new(frame: DecalFrame, p: &DecalParams) -> Self {
        Self {
            frame,
            width: p.width,
            height: p.height,
            near: p.near_plane,
            far: p.far_plane,
            backfaces: p.project_on_backfaces,
            no_clip: p.no_clip,
            backface_angle: if p.backface_angle.is_finite() {
                p.backface_angle
            } else {
                0.0
            },
        }
    }

    /// The native face test (`UStaticMeshComponent::GenerateDecalRenderData`):
    /// with `c` the cosine between the triangle's outward normal and the
    /// reversed projection direction, the triangle receives the decal when
    /// `c > BackfaceAngle`, or, with `bProjectOnBackfaces`, when
    /// `|c| > BackfaceAngle`. A degenerate triangle (no normal) never does.
    #[must_use]
    pub fn faces(&self, tri: [Vec3; 3]) -> bool {
        let c = -dot(face_normal(tri), self.frame.direction);
        c > self.backface_angle || (self.backfaces && c.abs() > self.backface_angle)
    }

    /// The part of the world triangle `tri` the decal covers: `None` when the
    /// triangle fails the face test ([`DecalBox::faces`]) or lies outside the
    /// box; else the clipped polygon (the whole triangle with `bNoClip`, when
    /// it touches the box).
    #[must_use]
    pub fn clip_triangle(&self, tri: [Vec3; 3]) -> Option<Vec<Vec3>> {
        if !self.faces(tri) {
            return None;
        }
        let planes = self
            .frame
            .planes(self.width, self.height, self.near, self.far);
        let mut poly = tri.to_vec();
        for plane in planes {
            poly = clip_polygon(&poly, plane);
            if poly.len() < 3 {
                return None;
            }
        }
        if self.no_clip {
            return Some(tri.to_vec());
        }
        Some(poly)
    }
}

/// Collects clipped polygons into an indexed triangle mesh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecalMeshBuilder {
    /// Positions, UU.
    pub positions: Vec<Vec3>,
    /// Unit normals (the receiver triangle's face normal).
    pub normals: Vec<Vec3>,
    /// Triangles.
    pub triangles: Vec<[u32; 3]>,
}

impl DecalMeshBuilder {
    /// Adds a convex polygon (fan-triangulated, receiver winding kept).
    pub fn add_polygon(&mut self, poly: &[Vec3], normal: Vec3) {
        let Ok(base) = u32::try_from(self.positions.len()) else {
            return;
        };
        if poly.len() < 3 || self.positions.len().saturating_add(poly.len()) > MAX_RECEIVER_VERTICES
        {
            return;
        }
        for p in poly {
            self.positions.push(*p);
            self.normals.push(normal);
        }
        for k in 1..poly.len() - 1 {
            let (Ok(b), Ok(c)) = (u32::try_from(k), u32::try_from(k + 1)) else {
                return;
            };
            self.triangles.push([base, base + b, base + c]);
        }
    }

    /// Projects `tris` (world space) through `bx`.
    pub fn project(&mut self, bx: &DecalBox, tris: impl IntoIterator<Item = [Vec3; 3]>) {
        for t in tris {
            if let Some(poly) = bx.clip_triangle(t) {
                self.add_polygon(&poly, face_normal(t));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Map extraction.
// ---------------------------------------------------------------------------

/// Where a receiver mesh comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverSource {
    /// The cooked `FStaticReceiverData` (static decals; vertices stored in
    /// the receiver's local space, transformed to world space here).
    Cooked,
    /// Projected here: the receiver's collision triangles clipped against
    /// the decal box ([`DecalBox`]; reproduces the cooked data,
    /// VFX_DECALS.md).
    Projected,
}

/// Decal geometry on one receiving component, in world space.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceiverMesh {
    /// Receiving component path.
    pub component: Option<String>,
    /// Class of the receiving component.
    pub component_class: Option<String>,
    /// Origin of the geometry.
    pub source: ReceiverSource,
    /// World-space positions, UU.
    pub positions: Vec<Vec3>,
    /// Unit normals: the outward face normal of the receiver triangle each
    /// vertex was clipped from (UE3 winding, [`face_normal`]).
    pub normals: Vec<Vec3>,
    /// Decal texture coordinates ([`DecalFrame::uv`]).
    pub uvs: Vec<[f32; 2]>,
    /// Triangles (indices into `positions`; receiver winding).
    pub triangles: Vec<[u32; 3]>,
    /// Vertices outside the decal box by more than 1 UU (a structural
    /// check; see VFX_DECALS.md).
    pub outside: usize,
    /// The cooked receiver carries a vertex light map (cooked only).
    pub has_light_map: bool,
    /// The component is a cooked receiver or in the editor's
    /// `DecalReceivers` list; `false` for a receiver only the run-time query
    /// finds ([`DecalProjector::world_receivers`]).
    pub listed: bool,
}

/// One placed decal actor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapDecal {
    /// Index in `ULevel::Actors` (the world actor slot the runtime uses).
    pub slot: usize,
    /// Actor object name.
    pub name: String,
    /// Actor object path.
    pub path: String,
    /// Actor class (`Engine.DecalActorMovable`, `Engine.DecalActor`, or
    /// another actor class that owns a decal component, e.g.
    /// `asamu.ASAMUCheckpointVisuals`).
    pub class: String,
    /// The owner is a `DecalActorBase`.
    pub decal_actor: bool,
    /// Component object path.
    pub component: Option<String>,
    /// Actor `Location`.
    pub location: Vec3,
    /// Actor `Rotation` (Pitch, Yaw, Roll).
    pub rotation: [i32; 3],
    /// Actor `DrawScale` (does not scale the decal box: CONFIRMED by the
    /// cooked receiver, VFX_DECALS.md).
    pub draw_scale: f32,
    /// Actor `DrawScale3D`.
    pub draw_scale3d: Vec3,
    /// Actor `bHidden`.
    pub hidden: bool,
    /// Actor `Tag`.
    pub tag: Option<String>,
    /// Actor `Base`.
    pub base: Option<String>,
    /// Decal parameters.
    pub params: DecalParams,
    /// The owner mirrors the decal ([`decal_is_mirrored`]): `frame` projects
    /// along the reversed orientation.
    pub mirrored: bool,
    /// The frame used for projection and texture coordinates.
    pub frame: DecalFrame,
    /// Cooked static receivers (count).
    pub static_receivers: usize,
    /// Of those, stored as whole (unclipped) triangles: some cooked vertex
    /// lies more than [`UNCLIPPED_TOLERANCE`] outside the decal box.
    pub unclipped_receivers: usize,
    /// Editor receivers skipped because they are hidden (`HiddenGame` on the
    /// component or `bHidden` on its actor) and the decal does not project
    /// on hidden receivers.
    pub hidden_receivers: usize,
    /// Geometry per receiving component.
    pub receivers: Vec<ReceiverMesh>,
    /// `DecalReceivers` entries that could not be resolved to triangles
    /// (neither a static mesh nor a BSP model component, mesh missing or
    /// undecodable).
    pub unresolved_receivers: Vec<String>,
}

impl MapDecal {
    /// Triangles over all receivers.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.receivers.iter().map(|r| r.triangles.len()).sum()
    }
}

/// Counts for one map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DecalStats {
    /// `DecalComponent` exports in the package (class default objects
    /// excluded).
    pub components: usize,
    /// `DecalComponent` exports whose native data decoded exactly.
    pub components_exact: usize,
    /// Decals: decal components owned by an actor listed in
    /// `ULevel::Actors`.
    pub actors: usize,
    /// Of those, owned by a `DecalActorBase` (the rest belong to other
    /// actor classes, e.g. `ASAMUCheckpointVisuals`).
    pub decal_actors: usize,
    /// Decals whose component decoded.
    pub actors_with_component: usize,
    /// Decal components not owned by a listed actor (templates).
    pub orphan_components: usize,
    /// Decal actors with cooked static receivers.
    pub with_static_receivers: usize,
    /// Cooked static receivers.
    pub static_receivers: usize,
    /// Cooked static receivers stored as whole (unclipped) triangles.
    pub unclipped_receivers: usize,
    /// Decal actors with geometry (cooked or projected).
    pub with_geometry: usize,
    /// Receiver meshes with at least one triangle (cooked + projected).
    pub receiver_meshes: usize,
    /// Of those, projected here.
    pub projected_receivers: usize,
    /// `DecalReceivers` entries that did not resolve.
    pub unresolved_receivers: usize,
    /// `DecalReceivers` entries skipped because the receiver is hidden.
    pub hidden_receivers: usize,
    /// Decal triangles.
    pub triangles: usize,
    /// Decal vertices.
    pub vertices: usize,
    /// Cooked vertices outside the decal box by more than 1 UU.
    pub vertices_outside: usize,
    /// Receiver meshes on components outside the editor's `DecalReceivers`
    /// list, found by the run-time receiver query.
    pub world_receivers: usize,
    /// Receiver meshes on BSP model components.
    pub bsp_receivers: usize,
    /// Decals on a mirrored owner ([`decal_is_mirrored`]).
    pub mirrored: usize,
    /// Decals with `bMovableDecal`.
    pub movable: usize,
    /// Decals hidden at level start.
    pub hidden: usize,
    /// Decals whose material is unset (the engine's default decal material).
    pub without_material: usize,
    /// Receiver triangles tested while projecting.
    pub projection_tests: usize,
}

impl DecalStats {
    /// Adds `other` into `self`.
    pub fn add(&mut self, o: &DecalStats) {
        self.components += o.components;
        self.components_exact += o.components_exact;
        self.actors += o.actors;
        self.decal_actors += o.decal_actors;
        self.orphan_components += o.orphan_components;
        self.actors_with_component += o.actors_with_component;
        self.with_static_receivers += o.with_static_receivers;
        self.static_receivers += o.static_receivers;
        self.unclipped_receivers += o.unclipped_receivers;
        self.with_geometry += o.with_geometry;
        self.receiver_meshes += o.receiver_meshes;
        self.projected_receivers += o.projected_receivers;
        self.unresolved_receivers += o.unresolved_receivers;
        self.hidden_receivers += o.hidden_receivers;
        self.triangles += o.triangles;
        self.vertices += o.vertices;
        self.vertices_outside += o.vertices_outside;
        self.world_receivers += o.world_receivers;
        self.bsp_receivers += o.bsp_receivers;
        self.mirrored += o.mirrored;
        self.movable += o.movable;
        self.hidden += o.hidden;
        self.without_material += o.without_material;
        self.projection_tests += o.projection_tests;
    }
}

/// The decals of one map package.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapDecals {
    /// [`DECALS_FORMAT`].
    pub format: &'static str,
    /// [`DECALS_VERSION`].
    pub version: u32,
    /// Map package name.
    pub package: String,
    /// Coordinate note.
    pub coordinates: &'static str,
    /// Decal actors in `ULevel::Actors` order (all levels of the package).
    pub decals: Vec<MapDecal>,
    /// Counts.
    pub stats: DecalStats,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// Coordinate note stored in every decal file.
pub const DECAL_COORDINATES: &str = "UE3 world space: Unreal units, left-handed, X forward, Y right, \
     Z up; rotators in 65536 units per turn; uv: u along the width axis, v against the height axis, box centre 0.5";

/// Unpacks an `FPackedNormal` (`byte / 127.5 − 1` per component).
#[must_use]
pub fn unpack_normal(p: [u8; 4]) -> Vec3 {
    let c = |b: u8| f32::from(b) / 127.5 - 1.0;
    normalize([c(p[0]), c(p[1]), c(p[2])])
}

fn class_chain_has(set: &PackageSet, class: &str, name: &str) -> bool {
    set.class_chain(class)
        .iter()
        .any(|c| c.eq_ignore_ascii_case(name))
}

/// The component transform values of an effective property list.
fn component_transform(props: &[Property]) -> ComponentTransform {
    ComponentTransform {
        translation: prop(props, "Translation")
            .and_then(as_vec3)
            .unwrap_or([0.0; 3]),
        rotation: prop(props, "Rotation")
            .and_then(as_rotator)
            .unwrap_or([0; 3]),
        scale: f32_or(props, "Scale", 1.0),
        scale3d: prop(props, "Scale3D").and_then(as_vec3).unwrap_or([1.0; 3]),
        absolute_translation: bool_of(props, "AbsoluteTranslation"),
        absolute_rotation: bool_of(props, "AbsoluteRotation"),
        absolute_scale: bool_of(props, "AbsoluteScale"),
    }
}

/// `AActor::LocalToWorld` of an actor's effective properties.
fn actor_matrix(props: &[Property]) -> Mat4 {
    actor_local_to_world(
        prop(props, "Location")
            .and_then(as_vec3)
            .unwrap_or([0.0; 3]),
        prop(props, "Rotation")
            .and_then(as_rotator)
            .unwrap_or([0; 3]),
        f32_or(props, "DrawScale", 1.0),
        prop(props, "DrawScale3D")
            .and_then(as_vec3)
            .unwrap_or([1.0; 3]),
        prop(props, "PrePivot")
            .and_then(as_vec3)
            .unwrap_or([0.0; 3]),
    )
}

/// Area-weighted vertex normals of a triangle mesh (UE3 winding,
/// [`face_normal`]).
#[must_use]
pub fn vertex_normals(positions: &[Vec3], triangles: &[[u32; 3]]) -> Vec<Vec3> {
    let mut acc = vec![[0.0f32; 3]; positions.len()];
    for t in triangles {
        let (Some(a), Some(b), Some(c)) = (
            positions.get(t[0] as usize),
            positions.get(t[1] as usize),
            positions.get(t[2] as usize),
        ) else {
            continue;
        };
        let n = cross(sub(*c, *a), sub(*b, *a));
        for &i in t {
            if let Some(v) = acc.get_mut(i as usize) {
                v[0] += n[0];
                v[1] += n[1];
                v[2] += n[2];
            }
        }
    }
    acc.into_iter().map(normalize).collect()
}

/// Positions and collision triangles of a static mesh.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeshTriangles {
    /// LOD 0 positions (local space).
    pub positions: Vec<Vec3>,
    /// Triangle list of the mesh's collision (kDOP) triangles: LOD 0 vertex
    /// indices, in LOD 0's winding, a repeated triangle taken once. The
    /// native decal code finds a receiver's triangles through that tree, so
    /// sections without collision take no decal (CONFIRMED (native); the
    /// kDOP triangles are LOD 0's triangles of the collision-enabled
    /// sections, MESHES.md).
    pub indices: Vec<u32>,
    /// Local bounds `(min, max)` of the vertices the triangles use (`None`
    /// without a triangle).
    pub bounds: Option<(Vec3, Vec3)>,
}

impl MeshTriangles {
    /// A mesh of `positions` and `triangles` (vertex index triples; a
    /// triangle with an index out of range, a non-finite corner or one
    /// already seen is dropped).
    #[must_use]
    pub fn new(positions: Vec<Vec3>, triangles: impl IntoIterator<Item = [u32; 3]>) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut indices = Vec::new();
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for t in triangles {
            let corners = t.map(|i| positions.get(i as usize).copied());
            let [Some(a), Some(b), Some(c)] = corners else {
                continue;
            };
            if ![a, b, c].iter().flatten().all(|v| v.is_finite()) || !seen.insert(t) {
                continue;
            }
            for p in [a, b, c] {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            indices.extend_from_slice(&t);
        }
        let bounds = (!indices.is_empty()).then_some((lo, hi));
        Self {
            positions,
            indices,
            bounds,
        }
    }
}

/// A static mesh component the run-time receiver query can return for a
/// decal ([`DecalProjector::world_receivers`]).
#[derive(Debug, Clone, PartialEq)]
pub struct WorldReceiver {
    /// Component export index.
    pub export: usize,
    /// Component object path.
    pub path: String,
    /// Export index of the level whose `Actors` list the owner.
    pub level: usize,
    /// World bounds of its collision triangles: minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
    /// `bAcceptsStaticDecals`.
    pub accepts_static: bool,
    /// `bAcceptsDynamicDecals`.
    pub accepts_dynamic: bool,
    /// Hidden in game (`HiddenGame`, or `bHidden` on its actor).
    pub hidden: bool,
}

impl WorldReceiver {
    /// The native acceptance rule (`UDecalComponent::AttachReceiver`, once
    /// play has begun): a primitive that accepts static decals takes a
    /// static decal (or a movable one); one that accepts dynamic decals takes
    /// a dynamic decal (or a movable one). CONFIRMED (native).
    #[must_use]
    pub fn accepts(&self, p: &DecalParams) -> bool {
        (self.accepts_static && (p.static_decal || p.movable_decal))
            || (self.accepts_dynamic && (!p.static_decal || p.movable_decal))
    }

    /// `true` when the bounds overlap the box `(min, max)`.
    #[must_use]
    pub fn overlaps(&self, min: Vec3, max: Vec3) -> bool {
        (0..3).all(|k| self.min[k] <= max[k] && self.max[k] >= min[k])
    }
}

/// `true` when the decal's actor filter (`FilterMode`, `Filter`) lets it
/// attach to a component of the actor `owner` (`UDecalComponent::
/// FilterComponent`: `FM_Ignore` skips the listed actors, `FM_Affect` takes
/// only them). CONFIRMED (native); no shipped decal uses a filter.
#[must_use]
pub fn filter_passes(p: &DecalParams, owner: Option<&str>) -> bool {
    let listed = owner.is_some_and(|o| p.filter.iter().any(|f| f.eq_ignore_ascii_case(o)));
    match p.filter_mode.as_deref() {
        Some(m) if m.eq_ignore_ascii_case("FM_Ignore") => !listed,
        Some(m) if m.eq_ignore_ascii_case("FM_Affect") => listed,
        _ => true,
    }
}

/// The owner part of a component path (`Map.TheWorld.Level.Actor.Component`
/// → `Map.TheWorld.Level.Actor`).
fn owner_path(component: &str) -> Option<&str> {
    component.rsplit_once('.').map(|(owner, _)| owner)
}

/// Resolves receivers to world triangles and projects decals onto them,
/// caching decoded meshes.
pub struct DecalProjector<'a> {
    set: &'a PackageSet,
    /// Effective-value resolver (shared with the caller's lookups).
    pub eff: Effective<'a>,
    meshes: HashMap<String, Option<Arc<MeshTriangles>>>,
    models: HashMap<(String, usize), Option<Arc<crate::bsp::Model>>>,
    /// Receiver triangles tested so far (against [`MAX_PROJECTION_WORK`]).
    work: usize,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

impl<'a> DecalProjector<'a> {
    /// A projector over `set`.
    #[must_use]
    pub fn new(set: &'a PackageSet) -> Self {
        Self {
            set,
            eff: Effective::new(set),
            meshes: HashMap::new(),
            models: HashMap::new(),
            work: 0,
            warnings: Vec::new(),
        }
    }

    /// Receiver triangles (and candidate components) tested so far.
    #[must_use]
    pub fn work(&self) -> usize {
        self.work
    }

    /// Charges `amount` units of work; `false` once the budget
    /// ([`MAX_PROJECTION_WORK`]) is exhausted (warned about once).
    fn charge(&mut self, amount: usize) -> bool {
        let before = self.work;
        self.work = self.work.saturating_add(amount);
        if self.work > MAX_PROJECTION_WORK {
            if before <= MAX_PROJECTION_WORK {
                self.warnings.push(format!(
                    "decal projection budget of {MAX_PROJECTION_WORK} triangle tests exhausted"
                ));
            }
            return false;
        }
        true
    }

    /// The world matrix of component export `index` of `lp` (its owner is
    /// its outer actor).
    pub fn component_world_matrix(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
    ) -> Option<Mat4> {
        component_world_matrix(&mut self.eff, lp, index)
    }

    /// `true` when component export `index` of `lp` is hidden in game
    /// (`HiddenGame`) or its owning actor is (`bHidden`).
    pub fn receiver_hidden(&mut self, lp: &Arc<LoadedPackage>, index: usize) -> bool {
        let comp = self.eff.of(lp, index);
        if bool_of(&comp, "HiddenGame") {
            return true;
        }
        lp.package
            .export(index)
            .ok()
            .and_then(|e| e.outer_index.export_index())
            .is_some_and(|owner| bool_of(&self.eff.of(lp, owner), "bHidden"))
    }

    /// The collision triangles of the static mesh at `path` (cached).
    pub fn mesh(&mut self, path: &str) -> Option<Arc<MeshTriangles>> {
        let key = path.to_ascii_lowercase();
        if let Some(m) = self.meshes.get(&key) {
            return m.clone();
        }
        let decoded =
            self.set.locate(path).and_then(
                |(mlp, mi)| match crate::staticmesh::decode_static_mesh(
                    &mlp.package,
                    Some(&mlp.name),
                    mi,
                    self.set,
                ) {
                    Ok(sm) => {
                        let crate::staticmesh::StaticMeshNative { kdop, lods, .. } = sm.native;
                        lods.into_iter().next().map(|lod| {
                            Arc::new(MeshTriangles::new(
                                lod.positions.positions,
                                kdop.triangles.iter().map(|t| t.vertices.map(u32::from)),
                            ))
                        })
                    }
                    Err(e) => {
                        self.warnings.push(format!("static mesh {path}: {e}"));
                        None
                    }
                },
            );
        self.meshes.insert(key, decoded.clone());
        decoded
    }

    /// The static mesh of component export `index` of `lp` and the number of
    /// triangles a projection onto it tests (`None` when it is not a static
    /// mesh component or its mesh does not resolve).
    fn component_mesh(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
    ) -> Option<Arc<MeshTriangles>> {
        let class = export_class_path(&lp.package, Some(&lp.name), index).ok()?;
        if !class_chain_has(self.set, &class, "StaticMeshComponent") {
            return None;
        }
        let props = self.eff.of(lp, index);
        let path = prop(&props, "StaticMesh").and_then(object_of)?;
        self.mesh(&path)
    }

    /// World triangles of the static mesh component export `index` of `lp`
    /// (`None` when it is not a static mesh component or its mesh does not
    /// resolve): the mesh's collision triangles ([`MeshTriangles`]).
    pub fn component_triangles(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
    ) -> Option<Vec<[Vec3; 3]>> {
        let mesh = self.component_mesh(lp, index)?;
        let world = self.component_world_matrix(lp, index)?;
        Some(world_triangles(&mesh, &world))
    }

    /// Projects the decal box `bx` onto the static mesh component export
    /// `index` of `lp` (`None` when the component does not resolve or the
    /// work budget is exhausted).
    pub fn project(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
        bx: &DecalBox,
    ) -> Option<DecalMeshBuilder> {
        let mesh = self.component_mesh(lp, index)?;
        // Work budget (hostile input: many decals listing one huge
        // receiver), checked before anything is allocated; the shipped maps
        // need a few million triangle tests.
        if !self.charge(mesh.indices.len() / 3) {
            return None;
        }
        let world = self.component_world_matrix(lp, index)?;
        let mut b = DecalMeshBuilder::default();
        b.project(bx, world_triangles(&mesh, &world));
        Some(b)
    }

    /// The BSP model export `index` of `lp` (cached).
    fn model(&mut self, lp: &Arc<LoadedPackage>, index: usize) -> Option<Arc<crate::bsp::Model>> {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(m) = self.models.get(&key) {
            return m.clone();
        }
        let decoded = match crate::bsp::decode_model(&lp.package, Some(&lp.name), index, self.set) {
            Ok((_, m)) => Some(Arc::new(m)),
            Err(e) => {
                self.warnings
                    .push(format!("{}: BSP model export {index}: {e}", lp.name));
                None
            }
        };
        self.models.insert(key, decoded.clone());
        decoded
    }

    /// Projects the decal box `bx` onto the BSP model component export
    /// `index` of `lp`: the polygons of the component's nodes on drawn
    /// surfaces, wound so that [`face_normal`] is the node plane's normal
    /// (`None` when it is not a model component, its model does not decode
    /// or the work budget is exhausted). The component is in world space.
    /// TENTATIVE: that the node plane's normal is the visible side and the
    /// surface classification are UE3 conventions; one shipped decal lists a
    /// BSP receiver (VFX_DECALS.md §8.5).
    pub fn project_bsp(
        &mut self,
        lp: &Arc<LoadedPackage>,
        index: usize,
        bx: &DecalBox,
    ) -> Option<DecalMeshBuilder> {
        let class = export_class_path(&lp.package, Some(&lp.name), index).ok()?;
        if !class_chain_has(self.set, &class, "ModelComponent") {
            return None;
        }
        let obj = self.set.decode(lp, index).ok()?;
        let data = lp.package.export_data(index).ok()?;
        let native =
            crate::lightmap::decode_model_component_native(data, obj.properties_end).ok()?;
        let model = self.model(lp, native.model.export_index()?)?;
        if !self.charge(native.nodes.len()) {
            return None;
        }
        let mut b = DecalMeshBuilder::default();
        for &node in &native.nodes {
            let i = usize::from(node);
            let (Some(n), Some(surf)) = (model.nodes.get(i), model.node_surf(i)) else {
                continue;
            };
            if !crate::bsp::is_visible_surface(surf) {
                continue;
            }
            let Some(poly) = model.node_polygon(i) else {
                continue;
            };
            let normal = [n.plane[0], n.plane[1], n.plane[2]];
            for k in 1..poly.len().saturating_sub(1) {
                let mut tri = [poly[0], poly[k], poly[k + 1]];
                if dot(face_normal(tri), normal) < 0.0 {
                    tri.swap(1, 2);
                }
                if let Some(clipped) = bx.clip_triangle(tri) {
                    b.add_polygon(&clipped, face_normal(tri));
                }
            }
        }
        Some(b)
    }

    /// The static mesh components of `lp` a decal can attach to at run time:
    /// the model of the world's collision hash query in
    /// `UDecalComponent::ComputeReceivers`. A component is returned when its
    /// class is exactly [`STATIC_MESH_COMPONENT_CLASS`], its owner is an
    /// actor in `level_of_actor` (actor export → level export), it collides
    /// (`CollideActors` on the component and `bCollideActors` on the owner:
    /// only such primitives are in the hash) and its mesh has collision
    /// triangles. At most [`MAX_WORLD_RECEIVERS`], in export order.
    ///
    /// Evidence (VFX_DECALS.md §8.5): CONFIRMED (native) that a decal
    /// without cooked receivers queries the hash by its bounds when play
    /// begins; STRONG that the hash holds exactly the colliding primitives
    /// (every one of the 2,455 static mesh components in the editor's
    /// receiver lists collides, while about one placed component in eight
    /// does not).
    pub fn world_receivers(
        &mut self,
        lp: &Arc<LoadedPackage>,
        level_of_actor: &HashMap<usize, usize>,
    ) -> Vec<WorldReceiver> {
        let pkg = &lp.package;
        let mut out = Vec::new();
        for i in 0..pkg.exports.len() {
            let Some(owner) = pkg
                .export(i)
                .ok()
                .and_then(|e| e.outer_index.export_index())
            else {
                continue;
            };
            let Some(&level) = level_of_actor.get(&owner) else {
                continue;
            };
            let Ok(class) = export_class_path(pkg, Some(&lp.name), i) else {
                continue;
            };
            if !class.eq_ignore_ascii_case(STATIC_MESH_COMPONENT_CLASS) {
                continue;
            }
            if out.len() >= MAX_WORLD_RECEIVERS {
                self.warnings.push(format!(
                    "{}: more than {MAX_WORLD_RECEIVERS} possible decal receivers; the rest left out",
                    lp.name
                ));
                break;
            }
            let comp = self.eff.of_transient(lp, i);
            if !bool_of(&comp, "CollideActors") {
                continue;
            }
            let actor = self.eff.of_transient(lp, owner);
            if !bool_of(&actor, "bCollideActors") {
                continue;
            }
            let Some(mesh) = prop(&comp, "StaticMesh")
                .and_then(object_of)
                .and_then(|path| self.mesh(&path))
            else {
                continue;
            };
            let Some((lo, hi)) = mesh.bounds else {
                continue;
            };
            let world =
                component_local_to_world(&component_transform(&comp), &actor_matrix(&actor));
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for corner in 0..8u8 {
                let p = transform_point(
                    &world,
                    [
                        if corner & 1 == 0 { lo[0] } else { hi[0] },
                        if corner & 2 == 0 { lo[1] } else { hi[1] },
                        if corner & 4 == 0 { lo[2] } else { hi[2] },
                    ],
                );
                for k in 0..3 {
                    min[k] = min[k].min(p[k]);
                    max[k] = max[k].max(p[k]);
                }
            }
            if !min.iter().chain(&max).all(|c| c.is_finite()) {
                continue;
            }
            let Ok(path) = lp.qualified(i) else {
                continue;
            };
            out.push(WorldReceiver {
                export: i,
                path,
                level,
                min,
                max,
                accepts_static: bool_of(&comp, "bAcceptsStaticDecals"),
                accepts_dynamic: bool_of(&comp, "bAcceptsDynamicDecals"),
                hidden: bool_of(&comp, "HiddenGame") || bool_of(&actor, "bHidden"),
            });
        }
        out
    }
}

/// `true` when the transform `m` mirrors (its 3 × 3 part has a negative
/// determinant: an odd number of negative scale factors). A mirrored
/// receiver's triangles come out with the opposite winding, so their
/// winding normal ([`face_normal`]) points into the surface.
#[must_use]
pub fn transform_mirrors(m: &Mat4) -> bool {
    let d = |r: usize, c: usize| f64::from(m[r][c]);
    let det = d(0, 0) * (d(1, 1) * d(2, 2) - d(1, 2) * d(2, 1))
        - d(0, 1) * (d(1, 0) * d(2, 2) - d(1, 2) * d(2, 0))
        + d(0, 2) * (d(1, 0) * d(2, 1) - d(1, 1) * d(2, 0));
    det < 0.0
}

/// The triangles of `mesh` transformed by `world`, wound so that
/// [`face_normal`] is the surface's outward normal also when `world`
/// mirrors the mesh. The native code tests each triangle in the receiver's
/// local space, where a mirror changes nothing; in world space the corners
/// of a mirrored receiver have to be swapped back. (On the shipped maps
/// three receivers of movable decals are mirrored; the decal's box holds
/// almost only faces whose winding normal points away from it.)
fn world_triangles(mesh: &MeshTriangles, world: &Mat4) -> Vec<[Vec3; 3]> {
    let world_pos: Vec<Vec3> = mesh
        .positions
        .iter()
        .map(|p| transform_point(world, *p))
        .collect();
    let mirrored = transform_mirrors(world);
    let mut out = Vec::with_capacity(mesh.indices.len() / 3);
    for t in mesh.indices.as_chunks::<3>().0 {
        if let (Some(a), Some(b), Some(c)) = (
            world_pos.get(t[0] as usize),
            world_pos.get(t[1] as usize),
            world_pos.get(t[2] as usize),
        ) {
            out.push(if mirrored { [*a, *c, *b] } else { [*a, *b, *c] });
        }
    }
    out
}

/// The world matrix of component export `index` (its owner is its outer
/// actor).
pub fn component_world_matrix(
    eff: &mut Effective<'_>,
    lp: &Arc<LoadedPackage>,
    index: usize,
) -> Option<Mat4> {
    let outer = lp.package.export(index).ok()?.outer_index.export_index()?;
    let owner = eff.of(lp, outer);
    let comp = eff.of(lp, index);
    Some(component_local_to_world(
        &component_transform(&comp),
        &actor_matrix(&owner),
    ))
}

/// Extracts every decal actor of the map package `lp` (all its levels).
pub fn extract_map_decals(set: &PackageSet, lp: &Arc<LoadedPackage>) -> ObjResult<MapDecals> {
    let pkg = &lp.package;
    let mut proj = DecalProjector::new(set);
    let mut stats = DecalStats::default();
    let mut warnings = Vec::new();
    let mut decals = Vec::new();

    // Every DecalComponent export (class default objects excluded), by
    // owning export.
    let mut by_owner: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..pkg.exports.len() {
        let Ok(class) = export_class_path(pkg, Some(&lp.name), i) else {
            continue;
        };
        if !class_chain_has(set, &class, "DecalComponent") {
            continue;
        }
        if crate::object::in_class_default_object(pkg, i) {
            continue;
        }
        stats.components += 1;
        match decode_component(set, lp, i) {
            Ok(_) => stats.components_exact += 1,
            Err(e) => warnings.push(format!("{}: {e}", lp.qualified(i).unwrap_or_default())),
        }
        if let Some(owner) = pkg
            .export(i)
            .ok()
            .and_then(|e| e.outer_index.export_index())
        {
            by_owner.entry(owner).or_default().push(i);
        }
    }

    // The levels' actor lists (an actor listed twice, hostile input, belongs
    // to its first entry).
    let mut levels = Vec::new();
    let mut level_of_actor: HashMap<usize, usize> = HashMap::new();
    for level in level_exports(pkg) {
        let (_, tail) = decode_level(pkg, Some(&lp.name), level, set)?;
        for a in &tail.actors {
            if let Some(ai) = a.export_index() {
                level_of_actor.entry(ai).or_insert(level);
            }
        }
        levels.push((level, tail.actors));
    }

    let mut owned = 0usize;
    let mut listed = std::collections::HashSet::new();
    let mut budget_warned = false;
    // The run-time receiver candidates, built when the first decal needs
    // them.
    let mut world: Option<Vec<WorldReceiver>> = None;
    for (level, actors) in &levels {
        for (slot, a) in actors.iter().enumerate() {
            let Some(ai) = a.export_index() else {
                continue;
            };
            let Some(components) = by_owner.get(&ai) else {
                continue;
            };
            // A repeated `Actors` entry (hostile input) is taken once.
            if !listed.insert(ai) {
                continue;
            }
            let Ok(class) = export_class_path(pkg, Some(&lp.name), ai) else {
                continue;
            };
            let decal_actor = class_chain_has(set, &class, "DecalActorBase");
            for &ci in components {
                // Geometry budget (hostile input: many decals sharing large
                // receivers); the shipped maximum is 32,601 triangles.
                if stats.triangles >= MAX_MAP_DECAL_TRIANGLES {
                    if !budget_warned {
                        budget_warned = true;
                        warnings.push(format!(
                            "decal geometry budget of {MAX_MAP_DECAL_TRIANGLES} triangles exhausted; \
                             further decals left out"
                        ));
                    }
                    continue;
                }
                owned += 1;
                stats.actors += 1;
                if decal_actor {
                    stats.decal_actors += 1;
                }
                let place = Placement {
                    level: *level,
                    slot,
                    actor: ai,
                    component: ci,
                    class: &class,
                    decal_actor,
                    triangle_budget: MAX_MAP_DECAL_TRIANGLES - stats.triangles,
                };
                match extract_decal(set, &mut proj, lp, &place, &level_of_actor, &mut world) {
                    Ok(d) => {
                        if d.component.is_some() {
                            stats.actors_with_component += 1;
                        }
                        if d.static_receivers > 0 {
                            stats.with_static_receivers += 1;
                        }
                        stats.static_receivers += d.static_receivers;
                        stats.unclipped_receivers += d.unclipped_receivers;
                        if d.triangle_count() > 0 {
                            stats.with_geometry += 1;
                        }
                        stats.unresolved_receivers += d.unresolved_receivers.len();
                        stats.hidden_receivers += d.hidden_receivers;
                        for r in &d.receivers {
                            if !r.triangles.is_empty() {
                                stats.receiver_meshes += 1;
                                if r.source == ReceiverSource::Projected {
                                    stats.projected_receivers += 1;
                                }
                                if !r.listed {
                                    stats.world_receivers += 1;
                                }
                                if r.component_class
                                    .as_deref()
                                    .is_some_and(|c| class_chain_has(set, c, "ModelComponent"))
                                {
                                    stats.bsp_receivers += 1;
                                }
                            }
                            stats.triangles += r.triangles.len();
                            stats.vertices += r.positions.len();
                            stats.vertices_outside += r.outside;
                        }
                        if d.mirrored {
                            stats.mirrored += 1;
                        }
                        if d.params.movable_decal {
                            stats.movable += 1;
                        }
                        if d.hidden {
                            stats.hidden += 1;
                        }
                        if d.params.material.is_none() {
                            stats.without_material += 1;
                        }
                        decals.push(d);
                    }
                    Err(e) => {
                        warnings.push(format!("{}: {e}", lp.qualified(ai).unwrap_or_default()))
                    }
                }
            }
        }
    }
    stats.orphan_components = stats.components.saturating_sub(owned);
    stats.projection_tests = proj.work;
    warnings.append(&mut proj.eff.warnings);
    warnings.append(&mut proj.warnings);
    Ok(MapDecals {
        format: DECALS_FORMAT,
        version: DECALS_VERSION,
        package: lp.name.clone(),
        coordinates: DECAL_COORDINATES,
        decals,
        stats,
        warnings,
    })
}

/// Decodes a `DecalComponent` export strictly: prelude, tagged properties
/// and native data consumed exactly.
pub fn decode_component(
    set: &PackageSet,
    lp: &LoadedPackage,
    index: usize,
) -> ObjResult<(crate::object::DecodedObject, DecalComponentNative)> {
    let obj = set.decode(lp, index)?;
    let data = lp.package.export_data(index)?;
    let native = decode_decal_component_native(data, obj.properties_end)?;
    Ok((obj, native))
}

/// A receiver mesh before its derived data.
struct RawMesh {
    component: Option<String>,
    component_class: Option<String>,
    source: ReceiverSource,
    mesh: DecalMeshBuilder,
    has_light_map: bool,
    listed: bool,
}

fn finish_mesh(frame: &DecalFrame, params: &DecalParams, raw: RawMesh) -> ReceiverMesh {
    let RawMesh {
        component,
        component_class,
        source,
        mesh,
        has_light_map,
        listed,
    } = raw;
    // Every vertex carries the face normal of the receiver triangle it was
    // clipped from (a clipped polygon can repeat a corner; normals derived
    // from the fan's triangles would leave such a vertex without one).
    let DecalMeshBuilder {
        positions,
        normals,
        triangles,
    } = mesh;
    let tile = [params.tile_x, params.tile_y];
    let offset = [params.offset_x, params.offset_y];
    let uvs = positions
        .iter()
        .map(|p| frame.uv(*p, params.width, params.height, tile, offset))
        .collect();
    let outside = positions
        .iter()
        .filter(|p| {
            !frame.contains(
                **p,
                params.width,
                params.height,
                params.near_plane,
                params.far_plane,
                1.0,
            )
        })
        .count();
    ReceiverMesh {
        component,
        component_class,
        source,
        positions,
        normals,
        uvs,
        triangles,
        outside,
        has_light_map,
        listed,
    }
}

/// Where a decal component sits: origin and orientation axes (forward,
/// right, up).
///
/// `DecalTransform_OwnerRelative` (the checkpoint visuals' decal): the
/// owner's transform applied to `ParentRelativeLocation` /
/// `ParentRelativeOrientation` (CONFIRMED on the cooked receivers: every
/// cooked vertex lies in the resulting box, VFX_DECALS.md). Otherwise
/// (`DecalTransform_OwnerAbsolute`, the decal actors) the owner's location
/// and rotation.
fn decal_placement(aprops: &[Property], params: &DecalParams) -> (Vec3, [Vec3; 3]) {
    let location = prop(aprops, "Location")
        .and_then(as_vec3)
        .unwrap_or([0.0; 3]);
    let rotation = prop(aprops, "Rotation")
        .and_then(as_rotator)
        .unwrap_or([0; 3]);
    let owner = rotator_axes(rotation);
    let relative = params
        .decal_transform
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case("DecalTransform_OwnerRelative"));
    if !relative {
        return (location, owner);
    }
    let origin = transform_point(&actor_matrix(aprops), params.parent_relative_location);
    let rel = rotator_axes(params.parent_relative_orientation);
    let mut axes = [[0.0f32; 3]; 3];
    for (axis, r) in axes.iter_mut().zip(rel) {
        for (k, a) in axis.iter_mut().enumerate() {
            *a = r[0] * owner[0][k] + r[1] * owner[1][k] + r[2] * owner[2][k];
        }
    }
    (origin, axes)
}

/// Which decal [`extract_decal`] extracts.
struct Placement<'p> {
    /// Export index of the owner's level.
    level: usize,
    /// Index of the owner in that level's `Actors`.
    slot: usize,
    /// Owner export.
    actor: usize,
    /// Decal component export.
    component: usize,
    /// Owner class path.
    class: &'p str,
    /// The owner is a `DecalActorBase`.
    decal_actor: bool,
    /// Triangles the map's budget still allows.
    triangle_budget: usize,
}

fn extract_decal(
    set: &PackageSet,
    proj: &mut DecalProjector<'_>,
    lp: &Arc<LoadedPackage>,
    place: &Placement<'_>,
    level_of_actor: &HashMap<usize, usize>,
    world: &mut Option<Vec<WorldReceiver>>,
) -> ObjResult<MapDecal> {
    let pkg = &lp.package;
    let (actor, component) = (place.actor, place.component);
    let aprops = proj.eff.of(lp, actor);
    let path = lp.qualified(actor)?;
    let name = pkg.fname(pkg.export(actor)?.object_name);
    let location = prop(&aprops, "Location")
        .and_then(as_vec3)
        .unwrap_or([0.0; 3]);
    let rotation = prop(&aprops, "Rotation")
        .and_then(as_rotator)
        .unwrap_or([0; 3]);
    let draw_scale3d = prop(&aprops, "DrawScale3D")
        .and_then(as_vec3)
        .unwrap_or([1.0; 3]);
    let (_, native) = decode_component(set, lp, component)?;
    let cprops = proj.eff.of(lp, component);
    let params = decal_params(&cprops);
    let component_path = Some(lp.qualified(component)?);
    let (origin, axes) = decal_placement(&aprops, &params);
    let mirrored = decal_is_mirrored(params.static_decal, draw_scale3d);
    let mut frame = DecalFrame::from_axes(origin, axes, params.rotation_degrees);
    if mirrored {
        frame = frame.mirrored();
    }
    let mut receivers: Vec<ReceiverMesh> = Vec::new();
    let mut unresolved = Vec::new();
    let mut unclipped_receivers = 0usize;
    let mut hidden_receivers = 0usize;
    let mut triangles = 0usize;
    if native.receivers.is_empty() {
        // No cooked receivers (the movable decals): the engine computes the
        // receivers when play begins. First the components the editor
        // attached (its `DecalReceivers` list, each taken once), then the
        // rest of what the run-time query returns.
        let bx = DecalBox::new(frame, &params);
        let mut taken = std::collections::HashSet::new();
        for rc in &params.receivers {
            if !taken.insert(rc.to_ascii_lowercase()) {
                continue;
            }
            if taken.len() > MAX_DECAL_RECEIVERS || triangles >= place.triangle_budget {
                break;
            }
            let Some(ci) = lp.export_by_qualified(rc) else {
                unresolved.push(rc.clone());
                continue;
            };
            // `bProjectOnHidden` (false on every shipped decal): a hidden
            // receiver takes no decal.
            if !params.project_on_hidden && proj.receiver_hidden(lp, ci) {
                hidden_receivers += 1;
                continue;
            }
            let cls = export_class_path(pkg, Some(&lp.name), ci).ok();
            let bsp = cls
                .as_deref()
                .is_some_and(|c| class_chain_has(set, c, "ModelComponent"));
            let projected = if bsp {
                // `bProjectOnBSP` off: the component takes nothing.
                if !params.project_on_bsp {
                    continue;
                }
                proj.project_bsp(lp, ci, &bx)
            } else {
                proj.project(lp, ci, &bx)
            };
            match projected {
                Some(b) => {
                    if b.triangles.is_empty() {
                        continue;
                    }
                    triangles += b.triangles.len();
                    receivers.push(finish_mesh(
                        &frame,
                        &params,
                        RawMesh {
                            component: Some(rc.clone()),
                            component_class: cls,
                            source: ReceiverSource::Projected,
                            mesh: b,
                            has_light_map: false,
                            listed: true,
                        },
                    ));
                }
                None => unresolved.push(rc.clone()),
            }
        }
        if params.project_on_static_meshes {
            let candidates = world.get_or_insert_with(|| proj.world_receivers(lp, level_of_actor));
            let (lo, hi) = frame.bounds(
                params.width,
                params.height,
                params.near_plane,
                params.far_plane,
            );
            // The scan itself counts against the work budget (hostile
            // input: very many decals against very many components).
            if proj.charge(candidates.len() / 64 + 1) {
                for c in candidates.iter() {
                    if taken.len() >= MAX_DECAL_RECEIVERS || triangles >= place.triangle_budget {
                        break;
                    }
                    if c.level != place.level
                        || !c.overlaps(lo, hi)
                        || !c.accepts(&params)
                        || (c.hidden && !params.project_on_hidden)
                        || !filter_passes(&params, owner_path(&c.path))
                        || !taken.insert(c.path.to_ascii_lowercase())
                    {
                        continue;
                    }
                    let Some(b) = proj.project(lp, c.export, &bx) else {
                        continue;
                    };
                    if b.triangles.is_empty() {
                        continue;
                    }
                    triangles += b.triangles.len();
                    receivers.push(finish_mesh(
                        &frame,
                        &params,
                        RawMesh {
                            component: Some(c.path.clone()),
                            component_class: Some(STATIC_MESH_COMPONENT_CLASS.to_owned()),
                            source: ReceiverSource::Projected,
                            mesh: b,
                            has_light_map: false,
                            listed: false,
                        },
                    ));
                }
            }
        }
    } else {
        // Every face the cooker chose is kept (it already dropped the ones
        // facing away): the negative angle passes them all.
        let cooked_box = DecalBox {
            backfaces: true,
            no_clip: false,
            backface_angle: -1.0,
            ..DecalBox::new(frame, &params)
        };
        for rec in &native.receivers {
            if triangles >= place.triangle_budget {
                break;
            }
            let world = rec
                .component
                .export_index()
                .and_then(|ci| proj.component_world_matrix(lp, ci));
            let comp_path = lp.ref_path(rec.component).ok().flatten();
            let Some(world) = world else {
                unresolved.push(comp_path.unwrap_or_default());
                continue;
            };
            let cooked: Vec<Vec3> = rec
                .vertices
                .iter()
                .map(|v| transform_point(&world, v.position))
                .collect();
            // The cooker stores some receivers clipped to the box and others
            // as whole triangles (the engine then clips per pixel): clip
            // them all here.
            let unclipped = cooked.iter().any(|p| {
                !frame.contains(
                    *p,
                    params.width,
                    params.height,
                    params.near_plane,
                    params.far_plane,
                    UNCLIPPED_TOLERANCE,
                )
            });
            if unclipped {
                unclipped_receivers += 1;
            }
            // A mirrored receiver: the cooked triangles are in its local
            // space, so their world winding is reversed.
            let mirrored_receiver = transform_mirrors(&world);
            let mut b = DecalMeshBuilder::default();
            for t in rec.indices.as_chunks::<3>().0 {
                if let (Some(p0), Some(p1), Some(p2)) = (
                    cooked.get(usize::from(t[0])),
                    cooked.get(usize::from(t[1])),
                    cooked.get(usize::from(t[2])),
                ) {
                    let tri = if mirrored_receiver {
                        [*p0, *p2, *p1]
                    } else {
                        [*p0, *p1, *p2]
                    };
                    if let Some(poly) = cooked_box.clip_triangle(tri) {
                        b.add_polygon(&poly, face_normal(tri));
                    }
                }
            }
            let cls = rec
                .component
                .export_index()
                .and_then(|ci| export_class_path(pkg, Some(&lp.name), ci).ok());
            triangles += b.triangles.len();
            receivers.push(finish_mesh(
                &frame,
                &params,
                RawMesh {
                    component: comp_path,
                    component_class: cls,
                    source: ReceiverSource::Cooked,
                    mesh: b,
                    has_light_map: !matches!(rec.light_map, LightMap::None),
                    listed: true,
                },
            ));
        }
    }
    Ok(MapDecal {
        slot: place.slot,
        name,
        path,
        class: place.class.to_owned(),
        decal_actor: place.decal_actor,
        component: component_path,
        location,
        rotation,
        draw_scale: f32_or(&aprops, "DrawScale", 1.0),
        draw_scale3d,
        hidden: bool_of(&aprops, "bHidden"),
        tag: prop(&aprops, "Tag").and_then(text_of),
        base: prop(&aprops, "Base").and_then(object_of),
        static_receivers: native.receivers.len(),
        unclipped_receivers,
        hidden_receivers,
        params,
        mirrored,
        frame,
        receivers,
        unresolved_receivers: unresolved,
    })
}

/// Transforms a point by a row-vector matrix (re-export for tools).
#[must_use]
pub fn transform(m: &Mat4, p: Vec3) -> Vec3 {
    transform_point(m, p)
}
