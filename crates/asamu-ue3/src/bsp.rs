//! UModel (BSP / brush model) native data, `UPolys` and the `BrushComponent`
//! native tail, for UE3 v868 (see `docs/reverse-engineering/LEVEL_FORMAT.md`).
//!
//! Layout after the object prelude and tagged properties, CONFIRMED by exact
//! consumption of every `Model` / `Polys` / `BrushComponent` export of the
//! shipped maps (tests `level_real_data.rs`):
//!
//! ```text
//! UModel   FBoxSphereBounds Bounds            Origin vec3, BoxExtent vec3, f32 SphereRadius
//!          bulk<vec3>        Vectors           i32 ElementSize (12), i32 Count, raw elements
//!          bulk<vec3>        Points
//!          bulk<FBspNode>    Nodes             64-byte elements (layout below)
//!          obj               Surfs.Owner       (TTransArray owner)
//!          array<FBspSurf>   Surfs             i32 Count + 60-byte elements
//!          bulk<FVert>       Verts             24-byte elements (16 when cooked without
//!                                              the back-face shadow coordinate)
//!          i32 NumSharedSides | i32 NumZones | FZoneProperties[NumZones] (24 bytes each, max 64)
//!          obj Polys | bulk<i32> LeafHulls | bulk<i32> Leaves | u32 RootOutside | u32 Linked
//!          bulk<i32> PortalNodes | u32 NumVertices | bulk<FModelVertex> VertexBuffer (36 bytes)
//!          FGuid LightingGuid | array<FLightmassPrimitiveSettings> (36 bytes each)
//! FBspNode FPlane (X,Y,Z,W) | i32 iVertPool | i32 iSurf | i32 iVertexIndex | u16 ComponentIndex
//!          | u16 ComponentNodeIndex | i32 ComponentElementIndex | i32 iBack | i32 iFront
//!          | i32 iPlane | i32 iCollisionBound | u8 iZone[2] | u8 NumVertices | u8 NodeFlags
//!          | i32 iLeaf[2]
//! FBspSurf obj Material | u32 PolyFlags | i32 pBase | i32 vNormal | i32 vTextureU | i32 vTextureV
//!          | i32 iBrushPoly | obj Actor | FPlane | f32 ShadowMapScale | u32 LightingChannels
//!          | i32 iLightmassIndex
//! FVert    i32 pVertex | i32 iSide | vec2 ShadowTexCoord | [vec2 BackfaceShadowTexCoord]
//! UPolys   i32 Num | i32 Max | obj Owner | FPoly[Num]
//! FPoly    vec3 Base, Normal, TextureU, TextureV | array<vec3> Vertices | u32 PolyFlags | obj Actor
//!          | FName ItemName | obj Material | i32 iLink | i32 iBrushPoly | f32 ShadowMapScale
//!          | u32 LightingChannels | FLightmassPrimitiveSettings | FName RulesetVariation
//! BrushComponent  array<bulk<u8>> CachedPhysBrushData (cooked physics convex data)
//! ```
//!
//! Field *names* follow UE3 conventions (TENTATIVE where only the layout is
//! proven); the order and sizes are CONFIRMED (native serializers read locally
//! in the original executable, then exact consumption plus value checks over
//! all shipped data: index ranges, unit plane normals, points on their planes).
//!
//! Note: a native `FPlane` is serialized `X, Y, Z, W`, unlike the binary
//! tagged-struct form of `Core.Object.Plane`, which stores `W` first.
//!
//! Hostile-input discipline: every count is checked against the remaining
//! bytes, pre-allocation is capped, element decoders must consume exactly the
//! serialized element size, and nothing panics on malformed data.

use serde::Serialize;

use crate::error::Ue3Error;
use crate::object::{DecodedObject, ObjResult, ObjectError, decode_object};
use crate::package::Package;
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::{FName, Guid, PackageIndex};

/// A 3-component vector in Unreal units (UE3 axes: X forward, Y right, Z up).
pub type Vec3 = [f32; 3];

/// Largest element count reserved up front for a decoded array.
pub const MAX_PREALLOC: usize = 4096;
/// Serialized size of one `FBspNode` (bulk element size, CONFIRMED).
pub const BSP_NODE_SIZE: usize = 64;
/// Serialized size of one `FBspSurf` (CONFIRMED).
pub const BSP_SURF_SIZE: usize = 60;
/// Serialized size of one `FVert` with the back-face shadow coordinate.
pub const VERT_SIZE: usize = 24;
/// Serialized size of one `FVert` without the back-face shadow coordinate
/// (the native code selects it when cooking for some targets; not seen in
/// the shipped data).
pub const VERT_SIZE_SHORT: usize = 16;
/// Serialized size of one `FModelVertex`.
pub const MODEL_VERTEX_SIZE: usize = 36;
/// Serialized size of one `FZoneProperties`.
pub const ZONE_PROPERTIES_SIZE: usize = 24;
/// Serialized size of `FLightmassPrimitiveSettings` (v868).
pub const LIGHTMASS_SETTINGS_SIZE: usize = 36;
/// Capacity of `UModel::Zones` (the in-memory fixed array ends where
/// `Bounds` starts: 64 entries of 32 bytes). Larger `NumZones` are rejected.
pub const MAX_ZONES: usize = 64;
/// Smallest serialized `FPoly` (no vertices).
pub const POLY_MIN_SIZE: usize =
    48 + 4 + 4 + 4 + 8 + 4 + 4 + 4 + 4 + 4 + LIGHTMASS_SETTINGS_SIZE + 8;
/// Fixed part of a serialized `UModel` tail (all arrays empty, no zones).
pub const MODEL_TAIL_MIN_SIZE: usize =
    28 + 8 * 3 + 4 + 4 + 8 + 4 + 4 + 4 + 8 + 8 + 4 + 4 + 8 + 4 + 8 + 16 + 4;

/// Surface (`FBspSurf::PolyFlags` / `FPoly::PolyFlags`) bits used by this
/// crate. Bit values are standard UE3 (`EPolyFlags`); their meaning in ASAMU
/// is TENTATIVE (consistent with the data: see LEVEL_FORMAT.md).
pub mod poly_flags {
    /// `PF_Invisible`: not rendered (still collides).
    pub const INVISIBLE: u32 = 0x0000_0001;
    /// `PF_NotSolid`: does not block (rendered only).
    pub const NOT_SOLID: u32 = 0x0000_0008;
    /// `PF_Semisolid`: collision-solid but does not cut CSG.
    pub const SEMISOLID: u32 = 0x0000_0020;
    /// `PF_TwoSided`: visible from both sides.
    pub const TWO_SIDED: u32 = 0x0000_0100;
    /// `PF_Portal`: zone portal (neither rendered nor solid).
    pub const PORTAL: u32 = 0x0400_0000;
}

/// `FBoxSphereBounds`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct BoxSphereBounds {
    /// Centre.
    pub origin: Vec3,
    /// Half extents of the box.
    pub box_extent: Vec3,
    /// Sphere radius.
    pub sphere_radius: f32,
}

/// One BSP node (`FBspNode`, 64 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BspNode {
    /// Node plane `(X, Y, Z, W)`: points `p` on the plane satisfy `n·p = W`.
    pub plane: [f32; 4],
    /// First entry of this node's polygon in `Model::verts`.
    pub vert_pool: i32,
    /// Surface index into `Model::surfs`.
    pub surf: i32,
    /// First vertex in the model's render vertex buffer.
    pub vertex_index: i32,
    /// Model component containing the node.
    pub component_index: u16,
    /// Node index within that component.
    pub component_node_index: u16,
    /// Element index within that component.
    pub component_element_index: i32,
    /// Back child (`-1` = none).
    pub back: i32,
    /// Front child (`-1` = none).
    pub front: i32,
    /// Next coplanar node (`-1` = none).
    pub coplanar: i32,
    /// Collision bound index into `Model::leaf_hulls` (`-1` = none).
    pub collision_bound: i32,
    /// Zone on the back / front side.
    pub zone: [u8; 2],
    /// Polygon vertex count (0 = node without a polygon).
    pub num_vertices: u8,
    /// Node flags (the native loader keeps only the low five bits).
    pub node_flags: u8,
    /// Leaf on the back / front side (`-1` = not a leaf).
    pub leaf: [i32; 2],
}

/// One BSP surface (`FBspSurf`, 60 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BspSurf {
    /// Material.
    pub material: PackageIndex,
    /// Surface flags ([`poly_flags`]).
    pub poly_flags: u32,
    /// Texture origin: index into `Model::points`.
    pub base: i32,
    /// Normal: index into `Model::vectors`.
    pub normal: i32,
    /// Texture U axis: index into `Model::vectors`.
    pub texture_u: i32,
    /// Texture V axis: index into `Model::vectors`.
    pub texture_v: i32,
    /// Source polygon in the brush (`-1` = none).
    pub brush_poly: i32,
    /// Source brush actor.
    pub actor: PackageIndex,
    /// Surface plane `(X, Y, Z, W)`.
    pub plane: [f32; 4],
    /// Shadow map scale.
    pub shadow_map_scale: f32,
    /// Lighting channel bit field.
    pub lighting_channels: u32,
    /// Index into the model's lightmass settings.
    pub lightmass_index: i32,
}

/// One polygon vertex reference (`FVert`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Vert {
    /// Point index into `Model::points`.
    pub point: i32,
    /// Side index (shared side bookkeeping).
    pub side: i32,
    /// Shadow-map texture coordinate.
    pub shadow_uv: [f32; 2],
    /// Back-face shadow-map texture coordinate (24-byte elements only).
    pub backface_shadow_uv: Option<[f32; 2]>,
}

/// `FZoneProperties`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ZoneProperties {
    /// Zone actor.
    pub zone_actor: PackageIndex,
    /// Connectivity bit set.
    pub connectivity: u64,
    /// Visibility bit set.
    pub visibility: u64,
    /// Last render time.
    pub last_render_time: f32,
}

/// Render vertex (`FModelVertex`, 36 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ModelVertex {
    /// Position.
    pub position: Vec3,
    /// Packed tangent X.
    pub tangent_x: u32,
    /// Packed tangent Z (normal).
    pub tangent_z: u32,
    /// Texture coordinate.
    pub uv: [f32; 2],
    /// Shadow-map coordinate.
    pub shadow_uv: [f32; 2],
}

/// `FLightmassPrimitiveSettings` (v868 serialization: nine 4-byte fields).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct LightmassPrimitiveSettings {
    /// `bUseTwoSidedLighting` (stored as `u32`).
    pub use_two_sided_lighting: u32,
    /// `bShadowIndirectOnly`.
    pub shadow_indirect_only: u32,
    /// `FullyOccludedSamplesFraction`.
    pub fully_occluded_samples_fraction: f32,
    /// `bUseEmissiveForStaticLighting`.
    pub use_emissive_for_static_lighting: u32,
    /// `EmissiveLightFalloffExponent`.
    pub emissive_light_falloff_exponent: f32,
    /// `EmissiveLightExplicitInfluenceRadius`.
    pub emissive_light_explicit_influence_radius: f32,
    /// `EmissiveBoost`.
    pub emissive_boost: f32,
    /// `DiffuseBoost`.
    pub diffuse_boost: f32,
    /// `SpecularBoost`.
    pub specular_boost: f32,
}

/// Decoded `UModel` native data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Model {
    /// Bounds.
    pub bounds: BoxSphereBounds,
    /// Normals and texture axes.
    pub vectors: Vec<Vec3>,
    /// Vertex positions.
    pub points: Vec<Vec3>,
    /// BSP nodes.
    pub nodes: Vec<BspNode>,
    /// Owner of the surface array.
    pub surfs_owner: PackageIndex,
    /// Surfaces.
    pub surfs: Vec<BspSurf>,
    /// Serialized element size of `verts` (24 or 16).
    pub vert_element_size: usize,
    /// Polygon vertex pool.
    pub verts: Vec<Vert>,
    /// `NumSharedSides`.
    pub num_shared_sides: i32,
    /// Zones (`NumZones` entries).
    pub zones: Vec<ZoneProperties>,
    /// Source polygons (`UPolys`).
    pub polys: PackageIndex,
    /// Leaf hulls.
    pub leaf_hulls: Vec<i32>,
    /// Leaves (zone index per leaf).
    pub leaves: Vec<i32>,
    /// `RootOutside`.
    pub root_outside: u32,
    /// `Linked`.
    pub linked: u32,
    /// Portal nodes.
    pub portal_nodes: Vec<i32>,
    /// `NumVertices`.
    pub num_vertices: u32,
    /// Render vertex buffer.
    pub vertex_buffer: Vec<ModelVertex>,
    /// Lighting GUID.
    pub lighting_guid: Guid,
    /// Lightmass settings per surface group.
    pub lightmass_settings: Vec<LightmassPrimitiveSettings>,
}

/// One source polygon (`FPoly`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Poly {
    /// Texture base point.
    pub base: Vec3,
    /// Normal.
    pub normal: Vec3,
    /// Texture U axis.
    pub texture_u: Vec3,
    /// Texture V axis.
    pub texture_v: Vec3,
    /// Vertices (brush-local space).
    pub vertices: Vec<Vec3>,
    /// Polygon flags ([`poly_flags`]).
    pub poly_flags: u32,
    /// Owning brush actor.
    pub actor: PackageIndex,
    /// Item name.
    pub item_name: FName,
    /// Material.
    pub material: PackageIndex,
    /// `iLink`.
    pub link: i32,
    /// `iBrushPoly`.
    pub brush_poly: i32,
    /// Shadow map scale.
    pub shadow_map_scale: f32,
    /// Lighting channel bit field.
    pub lighting_channels: u32,
    /// Lightmass settings.
    pub lightmass: LightmassPrimitiveSettings,
    /// Ruleset variation name.
    pub ruleset_variation: FName,
}

/// Decoded `UPolys` native data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Polys {
    /// Serialized `Max` (allocation hint, `>= polys.len()` in the data).
    pub max: i32,
    /// Owner (the brush model).
    pub owner: PackageIndex,
    /// Polygons.
    pub polys: Vec<Poly>,
}

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

/// Read a `vec3` (three `f32`).
pub fn read_vec3(r: &mut Reader<'_>) -> Result<Vec3, Ue3Error> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

/// Read a native `FPlane` (`X, Y, Z, W`).
pub fn read_plane(r: &mut Reader<'_>) -> Result<[f32; 4], Ue3Error> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

fn read_vec2(r: &mut Reader<'_>) -> Result<[f32; 2], Ue3Error> {
    Ok([r.read_f32()?, r.read_f32()?])
}

/// Read a native `TArray::BulkSerialize` array: `i32 ElementSize`, `i32
/// Count`, then `Count` raw elements. `sizes` lists the accepted element
/// sizes; `f` decodes one element and must consume exactly the element size.
pub fn read_bulk<T>(
    r: &mut Reader<'_>,
    what: &'static str,
    sizes: &[usize],
    mut f: impl FnMut(&mut Reader<'_>, usize) -> Result<T, Ue3Error>,
) -> ObjResult<(usize, Vec<T>)> {
    let at = r.position();
    let raw = r.read_i32()?;
    let elem = usize::try_from(raw)
        .ok()
        .filter(|s| sizes.contains(s))
        .ok_or_else(|| {
            malformed(
                what,
                at,
                format!("bulk element size {raw}, expected one of {sizes:?}"),
            )
        })?;
    let count = r.read_count(what, elem)?;
    let mut out = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        let start = r.position();
        let v = f(r, elem)?;
        let used = r.position().saturating_sub(start);
        if used != elem {
            return Err(malformed(
                what,
                start,
                format!("element decoder used {used} of {elem} bytes"),
            ));
        }
        out.push(v);
    }
    Ok((elem, out))
}

/// Read a plain `TArray` of fixed-size elements: `i32 Count` then elements.
pub fn read_array<T>(
    r: &mut Reader<'_>,
    what: &'static str,
    min_size: usize,
    mut f: impl FnMut(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<Vec<T>> {
    let count = r.read_count(what, min_size)?;
    let mut out = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        out.push(f(r)?);
    }
    Ok(out)
}

/// Read `FBoxSphereBounds`.
pub fn read_bounds(r: &mut Reader<'_>) -> Result<BoxSphereBounds, Ue3Error> {
    Ok(BoxSphereBounds {
        origin: read_vec3(r)?,
        box_extent: read_vec3(r)?,
        sphere_radius: r.read_f32()?,
    })
}

/// Read one `FBspNode`.
pub fn read_bsp_node(r: &mut Reader<'_>) -> Result<BspNode, Ue3Error> {
    Ok(BspNode {
        plane: read_plane(r)?,
        vert_pool: r.read_i32()?,
        surf: r.read_i32()?,
        vertex_index: r.read_i32()?,
        component_index: r.read_u16()?,
        component_node_index: r.read_u16()?,
        component_element_index: r.read_i32()?,
        back: r.read_i32()?,
        front: r.read_i32()?,
        coplanar: r.read_i32()?,
        collision_bound: r.read_i32()?,
        zone: [r.read_u8()?, r.read_u8()?],
        num_vertices: r.read_u8()?,
        node_flags: r.read_u8()?,
        leaf: [r.read_i32()?, r.read_i32()?],
    })
}

/// Read one `FBspSurf`.
pub fn read_bsp_surf(r: &mut Reader<'_>) -> Result<BspSurf, Ue3Error> {
    Ok(BspSurf {
        material: r.read_package_index()?,
        poly_flags: r.read_u32()?,
        base: r.read_i32()?,
        normal: r.read_i32()?,
        texture_u: r.read_i32()?,
        texture_v: r.read_i32()?,
        brush_poly: r.read_i32()?,
        actor: r.read_package_index()?,
        plane: read_plane(r)?,
        shadow_map_scale: r.read_f32()?,
        lighting_channels: r.read_u32()?,
        lightmass_index: r.read_i32()?,
    })
}

/// Read one `FVert` of the given serialized size (24 or 16).
pub fn read_vert(r: &mut Reader<'_>, size: usize) -> Result<Vert, Ue3Error> {
    Ok(Vert {
        point: r.read_i32()?,
        side: r.read_i32()?,
        shadow_uv: read_vec2(r)?,
        backface_shadow_uv: if size >= VERT_SIZE {
            Some(read_vec2(r)?)
        } else {
            None
        },
    })
}

/// Read one `FZoneProperties`.
pub fn read_zone(r: &mut Reader<'_>) -> Result<ZoneProperties, Ue3Error> {
    Ok(ZoneProperties {
        zone_actor: r.read_package_index()?,
        connectivity: r.read_u64()?,
        visibility: r.read_u64()?,
        last_render_time: r.read_f32()?,
    })
}

/// Read one `FModelVertex`.
pub fn read_model_vertex(r: &mut Reader<'_>) -> Result<ModelVertex, Ue3Error> {
    Ok(ModelVertex {
        position: read_vec3(r)?,
        tangent_x: r.read_u32()?,
        tangent_z: r.read_u32()?,
        uv: read_vec2(r)?,
        shadow_uv: read_vec2(r)?,
    })
}

/// Read `FLightmassPrimitiveSettings` (v868).
pub fn read_lightmass_settings(r: &mut Reader<'_>) -> Result<LightmassPrimitiveSettings, Ue3Error> {
    Ok(LightmassPrimitiveSettings {
        use_two_sided_lighting: r.read_u32()?,
        shadow_indirect_only: r.read_u32()?,
        fully_occluded_samples_fraction: r.read_f32()?,
        use_emissive_for_static_lighting: r.read_u32()?,
        emissive_light_falloff_exponent: r.read_f32()?,
        emissive_light_explicit_influence_radius: r.read_f32()?,
        emissive_boost: r.read_f32()?,
        diffuse_boost: r.read_f32()?,
        specular_boost: r.read_f32()?,
    })
}

fn bulk_i32(r: &mut Reader<'_>, what: &'static str) -> ObjResult<Vec<i32>> {
    Ok(read_bulk(r, what, &[4], |r, _| r.read_i32())?.1)
}

/// Read the `UModel` native data that follows the tagged properties.
pub fn read_model(r: &mut Reader<'_>) -> ObjResult<Model> {
    let bounds = read_bounds(r)?;
    let vectors = read_bulk(r, "Model.Vectors", &[12], |r, _| read_vec3(r))?.1;
    let points = read_bulk(r, "Model.Points", &[12], |r, _| read_vec3(r))?.1;
    let nodes = read_bulk(r, "Model.Nodes", &[BSP_NODE_SIZE], |r, _| read_bsp_node(r))?.1;
    let surfs_owner = r.read_package_index()?;
    let surfs = read_array(r, "Model.Surfs", BSP_SURF_SIZE, |r| Ok(read_bsp_surf(r)?))?;
    let (vert_element_size, verts) =
        read_bulk(r, "Model.Verts", &[VERT_SIZE, VERT_SIZE_SHORT], read_vert)?;
    let num_shared_sides = r.read_i32()?;
    let at = r.position();
    let num_zones = r.read_i32()?;
    let num_zones = usize::try_from(num_zones)
        .ok()
        .filter(|n| *n <= MAX_ZONES)
        .ok_or_else(|| {
            malformed(
                "Model.NumZones",
                at,
                format!("{num_zones} zones (allowed 0..={MAX_ZONES})"),
            )
        })?;
    r.check_count("Model.Zones", r.position(), num_zones, ZONE_PROPERTIES_SIZE)?;
    let mut zones = Vec::with_capacity(num_zones);
    for _ in 0..num_zones {
        zones.push(read_zone(r)?);
    }
    let polys = r.read_package_index()?;
    let leaf_hulls = bulk_i32(r, "Model.LeafHulls")?;
    let leaves = bulk_i32(r, "Model.Leaves")?;
    let root_outside = r.read_u32()?;
    let linked = r.read_u32()?;
    let portal_nodes = bulk_i32(r, "Model.PortalNodes")?;
    let num_vertices = r.read_u32()?;
    let vertex_buffer = read_bulk(r, "Model.VertexBuffer", &[MODEL_VERTEX_SIZE], |r, _| {
        read_model_vertex(r)
    })?
    .1;
    let lighting_guid = r.read_guid()?;
    let lightmass_settings =
        read_array(r, "Model.LightmassSettings", LIGHTMASS_SETTINGS_SIZE, |r| {
            Ok(read_lightmass_settings(r)?)
        })?;
    Ok(Model {
        bounds,
        vectors,
        points,
        nodes,
        surfs_owner,
        surfs,
        vert_element_size,
        verts,
        num_shared_sides,
        zones,
        polys,
        leaf_hulls,
        leaves,
        root_outside,
        linked,
        portal_nodes,
        num_vertices,
        vertex_buffer,
        lighting_guid,
        lightmass_settings,
    })
}

/// Read one `FPoly`.
pub fn read_poly(r: &mut Reader<'_>) -> ObjResult<Poly> {
    let base = read_vec3(r)?;
    let normal = read_vec3(r)?;
    let texture_u = read_vec3(r)?;
    let texture_v = read_vec3(r)?;
    let vertices = read_array(r, "Poly.Vertices", 12, |r| Ok(read_vec3(r)?))?;
    Ok(Poly {
        base,
        normal,
        texture_u,
        texture_v,
        vertices,
        poly_flags: r.read_u32()?,
        actor: r.read_package_index()?,
        item_name: r.read_fname()?,
        material: r.read_package_index()?,
        link: r.read_i32()?,
        brush_poly: r.read_i32()?,
        shadow_map_scale: r.read_f32()?,
        lighting_channels: r.read_u32()?,
        lightmass: read_lightmass_settings(r)?,
        ruleset_variation: r.read_fname()?,
    })
}

/// Read the `UPolys` native data that follows the tagged properties.
pub fn read_polys(r: &mut Reader<'_>) -> ObjResult<Polys> {
    let num = r.read_count("Polys.Num", POLY_MIN_SIZE)?;
    let at = r.position();
    let max = r.read_i32()?;
    if usize::try_from(max).map_or(true, |m| m < num) {
        return Err(malformed(
            "Polys.Max",
            at,
            format!("Max {max} is smaller than Num {num}"),
        ));
    }
    let owner = r.read_package_index()?;
    let mut polys = Vec::with_capacity(num.min(MAX_PREALLOC));
    for _ in 0..num {
        polys.push(read_poly(r)?);
    }
    Ok(Polys { max, owner, polys })
}

/// Read the `BrushComponent` native data (cached physics convex data): the
/// byte size of each cooked convex element.
pub fn read_brush_component_tail(r: &mut Reader<'_>) -> ObjResult<Vec<usize>> {
    read_array(r, "BrushComponent.CachedPhysBrushData", 8, |r| {
        Ok(
            read_bulk(r, "CachedPhysBrushData.Element", &[1], |r, _| r.read_u8())?
                .1
                .len(),
        )
    })
}

/// Decode the prelude and tagged properties of export `index`, then the
/// native tail with `tail`, requiring exact consumption of `SerialSize`.
pub fn decode_with_tail<T>(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
    kind: &str,
    tail: impl FnOnce(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<(DecodedObject, T)> {
    let obj = decode_object(pkg, own_name, index, schema)?;
    let data = pkg.export_data(index)?;
    let mut r = Reader::at(data, obj.properties_end)?;
    let value = tail(&mut r)?;
    if r.remaining() != 0 {
        return Err(ObjectError::SizeMismatch {
            export: index,
            kind: kind.to_owned(),
            consumed: r.position(),
            size: data.len(),
        });
    }
    Ok((obj, value))
}

/// Decode a `Model` export strictly (exact consumption).
pub fn decode_model(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<(DecodedObject, Model)> {
    decode_with_tail(pkg, own_name, index, schema, "Model", read_model)
}

/// Decode a `Polys` export strictly (exact consumption).
pub fn decode_polys(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<(DecodedObject, Polys)> {
    decode_with_tail(pkg, own_name, index, schema, "Polys", read_polys)
}

/// Decode a `BrushComponent` export strictly (exact consumption).
pub fn decode_brush_component(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<(DecodedObject, Vec<usize>)> {
    decode_with_tail(
        pkg,
        own_name,
        index,
        schema,
        "BrushComponent",
        read_brush_component_tail,
    )
}

// ------------------------------------------------------------ validation

fn in_range(i: i32, len: usize) -> bool {
    usize::try_from(i).is_ok_and(|i| i < len)
}

fn opt_in_range(i: i32, len: usize) -> bool {
    i == -1 || in_range(i, len)
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    f64::from(a[0]) * f64::from(b[0])
        + f64::from(a[1]) * f64::from(b[1])
        + f64::from(a[2]) * f64::from(b[2])
}

fn length(a: Vec3) -> f64 {
    dot(a, a).sqrt()
}

/// Structural and geometric consistency of a decoded [`Model`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ModelCheck {
    /// Index fields checked (node, surface and vertex references).
    pub references: usize,
    /// References out of range (should be 0).
    pub bad_references: usize,
    /// Node planes with a non-unit normal (|len − 1| > 1e-3).
    pub non_unit_planes: usize,
    /// Polygon vertices checked against their node plane.
    pub plane_points: usize,
    /// Polygon vertices farther than the tolerance from their node plane.
    pub off_plane_points: usize,
    /// Largest point-to-plane distance seen (UU).
    pub max_plane_distance: f64,
    /// Polygons whose vertex winding normal agrees with the node normal.
    pub winding_agrees: usize,
    /// Polygons whose winding normal opposes the node normal.
    pub winding_opposes: usize,
    /// First few problems, for diagnostics.
    pub examples: Vec<String>,
}

impl ModelCheck {
    fn note(&mut self, msg: String) {
        if self.examples.len() < 16 {
            self.examples.push(msg);
        }
    }
}

/// Distance tolerance for a polygon point on its node plane: 0.1 UU plus a
/// relative term for the f32 precision of large coordinates.
fn plane_tolerance(p: Vec3) -> f64 {
    0.1 + 1e-5 * length(p)
}

impl Model {
    /// Check every index reference and the polygon/plane geometry.
    pub fn check(&self) -> ModelCheck {
        let mut c = ModelCheck::default();
        let np = self.points.len();
        let nv = self.vectors.len();
        let check = |ok: bool, c: &mut ModelCheck, what: &dyn Fn() -> String| {
            c.references += 1;
            if !ok {
                c.bad_references += 1;
                c.note(what());
            }
        };
        // Only vertex-pool entries that a node uses are meaningful: the pool
        // can hold stale entries past the last node's range.
        for n in &self.nodes {
            let Ok(start) = usize::try_from(n.vert_pool) else {
                continue;
            };
            let end = start.saturating_add(usize::from(n.num_vertices));
            for (k, v) in self.verts.get(start..end).unwrap_or(&[]).iter().enumerate() {
                check(in_range(v.point, np), &mut c, &|| {
                    format!("vert {}: point {} out of range", start + k, v.point)
                });
            }
        }
        for (i, s) in self.surfs.iter().enumerate() {
            check(in_range(s.base, np), &mut c, &|| {
                format!("surf {i}: base {}", s.base)
            });
            check(in_range(s.normal, nv), &mut c, &|| {
                format!("surf {i}: normal {}", s.normal)
            });
            check(in_range(s.texture_u, nv), &mut c, &|| {
                format!("surf {i}: texture_u {}", s.texture_u)
            });
            check(in_range(s.texture_v, nv), &mut c, &|| {
                format!("surf {i}: texture_v {}", s.texture_v)
            });
        }
        let nn = self.nodes.len();
        for (i, n) in self.nodes.iter().enumerate() {
            check(in_range(n.surf, self.surfs.len()), &mut c, &|| {
                format!("node {i}: surf {}", n.surf)
            });
            for (what, child) in [
                ("back", n.back),
                ("front", n.front),
                ("coplanar", n.coplanar),
            ] {
                check(opt_in_range(child, nn), &mut c, &|| {
                    format!("node {i}: {what} {child}")
                });
            }
            for leaf in n.leaf {
                check(opt_in_range(leaf, self.leaves.len()), &mut c, &|| {
                    format!("node {i}: leaf {leaf}")
                });
            }
            check(
                opt_in_range(n.collision_bound, self.leaf_hulls.len()),
                &mut c,
                &|| format!("node {i}: collision bound {}", n.collision_bound),
            );
            let end = i64::from(n.vert_pool) + i64::from(n.num_vertices);
            check(
                n.vert_pool >= 0 && end <= self.verts.len() as i64,
                &mut c,
                &|| format!("node {i}: verts {}+{}", n.vert_pool, n.num_vertices),
            );
            let normal = [n.plane[0], n.plane[1], n.plane[2]];
            if (length(normal) - 1.0).abs() > 1e-3 {
                c.non_unit_planes += 1;
                c.note(format!("node {i}: plane normal length {}", length(normal)));
            }
            if let Some(poly) = self.node_polygon(i) {
                for p in &poly {
                    let d = (dot(normal, *p) - f64::from(n.plane[3])).abs();
                    c.plane_points += 1;
                    c.max_plane_distance = c.max_plane_distance.max(d);
                    if d > plane_tolerance(*p) {
                        c.off_plane_points += 1;
                        c.note(format!("node {i}: point {p:?} is {d} from its plane"));
                    }
                }
                let w = polygon_normal(&poly);
                let s = dot(w, normal);
                if s > 0.0 {
                    c.winding_agrees += 1;
                } else if s < 0.0 {
                    c.winding_opposes += 1;
                }
            }
        }
        c
    }

    /// Vertex positions of node `index`'s polygon, or `None` when the node has
    /// no polygon or a reference is out of range.
    pub fn node_polygon(&self, index: usize) -> Option<Vec<Vec3>> {
        let n = self.nodes.get(index)?;
        if n.num_vertices == 0 {
            return None;
        }
        let start = usize::try_from(n.vert_pool).ok()?;
        let end = start.checked_add(usize::from(n.num_vertices))?;
        let verts = self.verts.get(start..end)?;
        verts
            .iter()
            .map(|v| {
                usize::try_from(v.point)
                    .ok()
                    .and_then(|p| self.points.get(p).copied())
            })
            .collect()
    }

    /// Surface of node `index`, if in range.
    pub fn node_surf(&self, index: usize) -> Option<&BspSurf> {
        let n = self.nodes.get(index)?;
        self.surfs.get(usize::try_from(n.surf).ok()?)
    }

    /// Fan-triangulate every node polygon accepted by `select`. Triangles keep
    /// the node polygon's winding; each triangle is tagged with its surface
    /// index.
    pub fn triangulate(&self, select: impl Fn(&BspNode, &BspSurf) -> bool) -> TriangleMesh {
        let mut mesh = TriangleMesh::default();
        for (i, n) in self.nodes.iter().enumerate() {
            let Some(surf) = self.node_surf(i) else {
                continue;
            };
            if !select(n, surf) {
                continue;
            }
            let Some(poly) = self.node_polygon(i) else {
                continue;
            };
            let tag = u32::try_from(n.surf).unwrap_or(u32::MAX);
            mesh.add_polygon(&poly, tag);
        }
        mesh
    }
}

/// Newell normal of a polygon (not normalized).
pub fn polygon_normal(poly: &[Vec3]) -> Vec3 {
    let mut n = [0.0f64; 3];
    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let (ax, ay, az) = (f64::from(a[0]), f64::from(a[1]), f64::from(a[2]));
        let (bx, by, bz) = (f64::from(b[0]), f64::from(b[1]), f64::from(b[2]));
        n[0] += (ay - by) * (az + bz);
        n[1] += (az - bz) * (ax + bx);
        n[2] += (ax - bx) * (ay + by);
    }
    [n[0] as f32, n[1] as f32, n[2] as f32]
}

/// True for a BSP surface that is drawn (not invisible, not a portal).
/// TENTATIVE classification (UE3 flag meanings).
pub fn is_visible_surface(s: &BspSurf) -> bool {
    s.poly_flags & (poly_flags::INVISIBLE | poly_flags::PORTAL) == 0
}

/// True for a BSP surface that blocks (not non-solid, not a portal).
/// TENTATIVE classification (UE3 flag meanings).
pub fn is_collision_surface(s: &BspSurf) -> bool {
    s.poly_flags & (poly_flags::NOT_SOLID | poly_flags::PORTAL) == 0
}

/// An indexed triangle set.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TriangleMesh {
    /// Vertex positions.
    pub positions: Vec<Vec3>,
    /// Triangles (indices into `positions`).
    pub indices: Vec<[u32; 3]>,
    /// One tag per triangle (surface or polygon index).
    pub tags: Vec<u32>,
}

impl TriangleMesh {
    /// Add a convex polygon as a triangle fan. Returns false (and adds
    /// nothing) for fewer than three vertices or when the index space is full.
    pub fn add_polygon(&mut self, poly: &[Vec3], tag: u32) -> bool {
        if poly.len() < 3 {
            return false;
        }
        let Ok(base) = u32::try_from(self.positions.len()) else {
            return false;
        };
        let Some(_) = u32::try_from(poly.len())
            .ok()
            .and_then(|n| base.checked_add(n))
        else {
            return false;
        };
        self.positions.extend_from_slice(poly);
        for k in 1..poly.len() - 1 {
            // k < poly.len() <= u32::MAX - base, checked above.
            let k = k as u32;
            self.indices.push([base, base + k, base + k + 1]);
            self.tags.push(tag);
        }
        true
    }

    /// Append another mesh.
    pub fn append(&mut self, other: &TriangleMesh) -> bool {
        let Ok(base) = u32::try_from(self.positions.len()) else {
            return false;
        };
        let Some(_) = u32::try_from(other.positions.len())
            .ok()
            .and_then(|n| base.checked_add(n))
        else {
            return false;
        };
        self.positions.extend_from_slice(&other.positions);
        for t in &other.indices {
            self.indices.push([
                t[0].saturating_add(base),
                t[1].saturating_add(base),
                t[2].saturating_add(base),
            ]);
        }
        self.tags.extend_from_slice(&other.tags);
        true
    }

    /// Number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len()
    }

    /// Axis-aligned bounds `(min, max)`, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let first = *self.positions.first()?;
        let mut lo = first;
        let mut hi = first;
        for p in &self.positions {
            for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *l = l.min(*v);
                *h = h.max(*v);
            }
        }
        Some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_triangulation_keeps_winding() {
        let mut m = TriangleMesh::default();
        let quad = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        assert!(m.add_polygon(&quad, 7));
        assert_eq!(m.indices, vec![[0, 1, 2], [0, 2, 3]]);
        assert_eq!(m.tags, vec![7, 7]);
        assert!(!m.add_polygon(&quad[..2], 1));
        let n = polygon_normal(&quad);
        assert!(n[2] > 0.0);
        let mut other = TriangleMesh::default();
        other.add_polygon(&quad[..3], 9);
        assert!(m.append(&other));
        assert_eq!(m.indices.last(), Some(&[4, 5, 6]));
        assert_eq!(m.bounds(), Some(([0.0; 3], [1.0, 1.0, 0.0])));
    }

    #[test]
    fn bulk_rejects_unexpected_element_size() {
        let mut data = 13i32.to_le_bytes().to_vec();
        data.extend_from_slice(&0i32.to_le_bytes());
        let mut r = Reader::new(&data);
        assert!(read_bulk(&mut r, "t", &[12], |r, _| read_vec3(r)).is_err());
        // Count larger than the data.
        let mut data = 12i32.to_le_bytes().to_vec();
        data.extend_from_slice(&1000i32.to_le_bytes());
        let mut r = Reader::new(&data);
        assert!(read_bulk(&mut r, "t", &[12], |r, _| read_vec3(r)).is_err());
    }
}
