//! `RB_BodySetup` decoding for UE3 v868 / licensee 0: the simple collision
//! shapes of a static mesh (and of the bodies of a physics asset).
//!
//! An `RB_BodySetup` export is `i32 NetIndex`, tagged properties, and the
//! native data written by `URB_BodySetup::Serialize`. Evidence, counts and
//! confidence of every statement here: `docs/reverse-engineering/MESHES.md`,
//! section "Simple collision".
//!
//! ```text
//! i32 NetIndex
//! tagged properties (each: FName Name, FName Type, i32 Size, i32 ArrayIndex, [extra], value)
//!   scalar tags of RB_BodySetup           BoneName, bNoCollision, PhysMaterial, MassScale, ...
//!   PreCachedPhysScale  ArrayProperty     i32 Count, Count x FVector
//!   PreCachedPhysDataVersion IntProperty
//!   COMNudge            StructProperty Vector (never stored in the shipped data)
//!   AggGeom             StructProperty KAggregateGeom: a tagged stream
//!     SphereElems  ArrayProperty  i32 Count, Count x tagged KSphereElem {TM, Radius, bNoRBCollision, bPerPolyShape}
//!     BoxElems     ArrayProperty  i32 Count, Count x tagged KBoxElem    {TM, X, Y, Z, bNoRBCollision, bPerPolyShape}
//!     SphylElems   ArrayProperty  i32 Count, Count x tagged KSphylElem  {TM, Radius, Length, bNoRBCollision, bPerPolyShape}
//!     ConvexElems  ArrayProperty  i32 Count, Count x tagged KConvexElem:
//!       VertexData            ArrayProperty  i32 Count, Count x FVector
//!       PermutedVertexData    ArrayProperty  i32 Count, Count x FPlane
//!       FaceTriData           ArrayProperty  i32 Count, Count x i32
//!       EdgeDirections        ArrayProperty  i32 Count, Count x FVector
//!       FaceNormalDirections  ArrayProperty  i32 Count, Count x FVector
//!       FacePlaneData         ArrayProperty  i32 Count, Count x FPlane
//!       ElemBox               StructProperty Box: FVector Min, FVector Max, u8 IsValid
//!     bSkipCloseAndParallelChecks BoolProperty (never stored in the shipped data)
//!   FName "None"
//! native data: TArray<FKCachedConvexData> PreCachedPhysData
//!   i32 Count, Count x { i32 ElementCount, ElementCount x bulk TArray<u8> }
//!   bulk TArray<u8> = i32 ElementSize (1), i32 ByteCount, ByteCount bytes
//! ```
//!
//! A class default object (`Default__RB_BodySetup`) ends after its tagged
//! properties: class default objects store no native data
//! (`OBJECT_FORMAT.md`).
//!
//! Binary structs are written member by member in property-link order, in
//! which a struct's own members precede the inherited ones: an `FPlane` is
//! stored as **W, X, Y, Z** and an `FMatrix` (`TM`) as its four planes, each
//! in that order. [`Plane`] and [`Matrix`] hold them in the usual X, Y, Z, W
//! order.
//!
//! A struct tag stores a `BoolProperty` value as one byte in the tag header
//! (the tag size is 0).
//!
//! The decoder here is written against that layout only: it does not use a
//! [`crate::schema::Schema`], so it also works on a package without script
//! classes, and it is checked against the schema-driven generic decoder
//! ([`crate::object::decode_object`]) and by a byte-for-byte re-encode
//! ([`encode_body_setup`]) on every shipped export.
//!
//! Strictness: every tag must be one this module knows (a scalar tag of any
//! name is kept generically; struct and array tags must be the ones listed
//! above, with exactly the listed type), members of a struct must come in
//! declaration order and at most once, every value must end exactly at its
//! tag size, booleans must be 0 or 1, counts are checked against the
//! remaining bytes before anything is allocated, and the decoder must end
//! exactly at the end of the payload. Malformed input yields an
//! [`ObjectError`]; nothing here panics.
//!
//! The pre-cooked physics data (`PreCachedPhysData`) is kept as opaque bytes:
//! it is the physics middleware's own cooked form of the convex elements and
//! the game's own line and box checks do not read it.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Serialize, Serializer};

use crate::flags;
use crate::model::LoadedPackage;
use crate::object::{ObjResult, ObjectError};
use crate::package::Package;
use crate::reader::Reader;
use crate::types::{FName, PackageIndex};
use crate::writer::Writer;

/// Serialized size of an `FVector`.
pub const VECTOR_SIZE: usize = 12;
/// Serialized size of an `FPlane`.
pub const PLANE_SIZE: usize = 16;
/// Serialized size of an `FMatrix`.
pub const MATRIX_SIZE: usize = 64;
/// Serialized size of an `FBox` (two vectors and the `IsValid` byte).
pub const BOX_SIZE: usize = 25;
/// Smallest tag: two names, size and array index.
pub const TAG_MIN_SIZE: usize = 24;
/// Smallest tagged struct: the terminating `None` name.
pub const STRUCT_MIN_SIZE: usize = 8;
/// Element size of the bulk byte arrays in the pre-cooked physics data.
pub const CACHED_ELEMENT_SIZE: usize = 1;
/// Largest element count reserved before the elements have decoded.
const MAX_PREALLOC: usize = 4096;

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// A package name table, as far as this decoder needs it.
pub trait NameTable {
    /// Number of names.
    fn name_count(&self) -> usize;
    /// Name `index`, without an instance number.
    fn name_at(&self, index: usize) -> Option<&str>;
}

impl NameTable for Package {
    fn name_count(&self) -> usize {
        self.names.len()
    }
    fn name_at(&self, index: usize) -> Option<&str> {
        self.names.get(index).map(|n| n.name.as_str())
    }
}

impl<S: AsRef<str>> NameTable for Vec<S> {
    fn name_count(&self) -> usize {
        self.len()
    }
    fn name_at(&self, index: usize) -> Option<&str> {
        self.get(index).map(AsRef::as_ref)
    }
}

impl<S: AsRef<str>> NameTable for &[S] {
    fn name_count(&self) -> usize {
        self.len()
    }
    fn name_at(&self, index: usize) -> Option<&str> {
        self.get(index).map(AsRef::as_ref)
    }
}

/// Name → index lookup for the encoder (names compare without regard to
/// ASCII case; the first entry wins).
#[derive(Debug, Clone, Default)]
pub struct NameIndex(HashMap<String, i32>);

impl NameIndex {
    /// Index every name of `names`.
    pub fn new(names: &dyn NameTable) -> Self {
        let mut map = HashMap::new();
        for i in 0..names.name_count() {
            let (Some(n), Ok(idx)) = (names.name_at(i), i32::try_from(i)) else {
                continue;
            };
            map.entry(n.to_ascii_lowercase()).or_insert(idx);
        }
        NameIndex(map)
    }

    /// The `FName` (instance number 0) of `name`.
    pub fn get(&self, name: &str) -> Option<FName> {
        self.0
            .get(&name.to_ascii_lowercase())
            .map(|&index| FName { index, number: 0 })
    }
}

// ---------------------------------------------------------------------------
// Decoded types
// ---------------------------------------------------------------------------

/// `FPlane` in the usual order (`x`, `y`, `z` the normal, `w` the distance).
/// On disk the order is W, X, Y, Z.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Plane {
    /// X.
    pub x: f32,
    /// Y.
    pub y: f32,
    /// Z.
    pub z: f32,
    /// W.
    pub w: f32,
}

/// `FMatrix`: rows `XPlane`, `YPlane`, `ZPlane`, `WPlane`, each `[x, y, z, w]`.
/// UE3 multiplies row vectors: a local point `p` maps to
/// `p.x * row0 + p.y * row1 + p.z * row2 + row3`.
pub type Matrix = [[f32; 4]; 4];

/// `FBox` (`ElemBox`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ElemBox {
    /// Minimum corner.
    pub min: [f32; 3],
    /// Maximum corner.
    pub max: [f32; 3],
    /// `IsValid` byte.
    pub is_valid: u8,
}

/// `KConvexElem`: one convex hull. A member is `None` when its tag is absent
/// (never in the shipped data).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ConvexElem {
    /// `VertexData`: the hull's vertices.
    pub vertex_data: Option<Vec<[f32; 3]>>,
    /// `PermutedVertexData`: the vertices regrouped four at a time for SIMD
    /// (see [`permute_vertex_data`]).
    pub permuted_vertex_data: Option<Vec<Plane>>,
    /// `FaceTriData`: vertex indices, three per triangle of the hull surface.
    pub face_tri_data: Option<Vec<i32>>,
    /// `EdgeDirections`: the distinct edge directions.
    pub edge_directions: Option<Vec<[f32; 3]>>,
    /// `FaceNormalDirections`: the distinct face normal directions.
    pub face_normal_directions: Option<Vec<[f32; 3]>>,
    /// `FacePlaneData`: the hull's planes (outward normal, distance).
    pub face_plane_data: Option<Vec<Plane>>,
    /// `ElemBox`: the vertices' bounding box.
    pub elem_box: Option<ElemBox>,
}

/// `KSphereElem`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct SphereElem {
    /// `TM`.
    pub tm: Option<Matrix>,
    /// `Radius`.
    pub radius: Option<f32>,
    /// `bNoRBCollision`.
    pub no_rb_collision: Option<bool>,
    /// `bPerPolyShape`.
    pub per_poly_shape: Option<bool>,
}

/// `KBoxElem`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct BoxElem {
    /// `TM`.
    pub tm: Option<Matrix>,
    /// `X`.
    pub x: Option<f32>,
    /// `Y`.
    pub y: Option<f32>,
    /// `Z`.
    pub z: Option<f32>,
    /// `bNoRBCollision`.
    pub no_rb_collision: Option<bool>,
    /// `bPerPolyShape`.
    pub per_poly_shape: Option<bool>,
}

/// `KSphylElem` (a capsule).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct SphylElem {
    /// `TM`.
    pub tm: Option<Matrix>,
    /// `Radius`.
    pub radius: Option<f32>,
    /// `Length`.
    pub length: Option<f32>,
    /// `bNoRBCollision`.
    pub no_rb_collision: Option<bool>,
    /// `bPerPolyShape`.
    pub per_poly_shape: Option<bool>,
}

/// `KAggregateGeom`. A member is `None` when its tag is absent.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct AggGeom {
    /// `SphereElems`.
    pub sphere_elems: Option<Vec<SphereElem>>,
    /// `BoxElems`.
    pub box_elems: Option<Vec<BoxElem>>,
    /// `SphylElems`.
    pub sphyl_elems: Option<Vec<SphylElem>>,
    /// `ConvexElems`.
    pub convex_elems: Option<Vec<ConvexElem>>,
    /// `bSkipCloseAndParallelChecks`.
    pub skip_close_and_parallel_checks: Option<bool>,
}

impl AggGeom {
    /// The spheres (empty when the tag is absent).
    pub fn spheres(&self) -> &[SphereElem] {
        self.sphere_elems.as_deref().unwrap_or(&[])
    }
    /// The boxes (empty when the tag is absent).
    pub fn boxes(&self) -> &[BoxElem] {
        self.box_elems.as_deref().unwrap_or(&[])
    }
    /// The capsules (empty when the tag is absent).
    pub fn sphyls(&self) -> &[SphylElem] {
        self.sphyl_elems.as_deref().unwrap_or(&[])
    }
    /// The convex hulls (empty when the tag is absent).
    pub fn convex(&self) -> &[ConvexElem] {
        self.convex_elems.as_deref().unwrap_or(&[])
    }
    /// Number of shapes of all four kinds.
    pub fn element_count(&self) -> usize {
        self.spheres()
            .len()
            .saturating_add(self.boxes().len())
            .saturating_add(self.sphyls().len())
            .saturating_add(self.convex().len())
    }
}

/// Value of a scalar tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ScalarValue {
    /// `BoolProperty`.
    Bool(bool),
    /// `IntProperty`.
    Int(i32),
    /// `FloatProperty`.
    Float(f32),
    /// `NameProperty`.
    Name {
        /// Resolved text.
        text: String,
        /// Stored name reference.
        raw: FName,
    },
    /// `ObjectProperty`.
    Object(PackageIndex),
    /// `ByteProperty` stored as one byte.
    Byte {
        /// Enum name of the tag (`None` for a plain byte).
        enum_name: String,
        /// Stored enum name reference.
        raw_enum: FName,
        /// The byte.
        value: u8,
    },
    /// `ByteProperty` stored as an enumerator name.
    Enum {
        /// Enum name of the tag.
        enum_name: String,
        /// Stored enum name reference.
        raw_enum: FName,
        /// Enumerator.
        value: String,
        /// Stored enumerator name reference.
        raw_value: FName,
    },
}

/// A scalar tag of the body setup itself (`BoneName`, `bNoCollision`, ...).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScalarProperty {
    /// Property name.
    pub name: String,
    /// Stored name reference.
    pub raw_name: FName,
    /// Value.
    pub value: ScalarValue,
}

/// One tagged property of an `RB_BodySetup`, in stream order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum BodyProperty {
    /// A scalar tag.
    Scalar(ScalarProperty),
    /// `PreCachedPhysScale`: the scales the physics data was pre-cooked for.
    PreCachedPhysScale(Vec<[f32; 3]>),
    /// `COMNudge`.
    ComNudge([f32; 3]),
    /// `AggGeom`: the simple collision shapes.
    AggGeom(AggGeom),
}

/// Opaque cooked bytes; serialized as their length only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CookedBlob(pub Vec<u8>);

impl Serialize for CookedBlob {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(self.0.len()).unwrap_or(u64::MAX))
    }
}

/// `FKCachedConvexData`: the pre-cooked convex elements for one scale.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CachedConvexData {
    /// `CachedConvexElements`: one cooked blob per convex element.
    pub elements: Vec<CookedBlob>,
}

/// A decoded `RB_BodySetup` export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BodySetup {
    /// `NetIndex`.
    pub net_index: i32,
    /// Tagged properties in stream order.
    pub properties: Vec<BodyProperty>,
    /// Payload offset where the native data starts (end of the tagged
    /// properties).
    pub native_start: usize,
    /// `PreCachedPhysData` (native data): one entry per pre-cooked scale.
    /// `None` for a class default object, which stores no native data.
    pub pre_cached_phys_data: Option<Vec<CachedConvexData>>,
}

impl BodySetup {
    /// `AggGeom`, when tagged.
    pub fn agg_geom(&self) -> Option<&AggGeom> {
        self.properties.iter().find_map(|p| match p {
            BodyProperty::AggGeom(g) => Some(g),
            _ => None,
        })
    }

    /// `PreCachedPhysScale`, when tagged.
    pub fn pre_cached_phys_scale(&self) -> Option<&[[f32; 3]]> {
        self.properties.iter().find_map(|p| match p {
            BodyProperty::PreCachedPhysScale(v) => Some(v.as_slice()),
            _ => None,
        })
    }

    /// The scalar tags in stream order.
    pub fn scalars(&self) -> impl Iterator<Item = &ScalarProperty> {
        self.properties.iter().filter_map(|p| match p {
            BodyProperty::Scalar(s) => Some(s),
            _ => None,
        })
    }

    /// The scalar tag `name` (ASCII case ignored), when present.
    pub fn scalar(&self, name: &str) -> Option<&ScalarValue> {
        self.scalars()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .map(|s| &s.value)
    }

    /// The boolean tag `name`, when present.
    pub fn bool_property(&self, name: &str) -> Option<bool> {
        match self.scalar(name) {
            Some(ScalarValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// Number of shapes (0 without an `AggGeom`).
    pub fn element_count(&self) -> usize {
        self.agg_geom().map_or(0, AggGeom::element_count)
    }

    /// The pre-cooked physics data (empty for a class default object).
    pub fn cached_data(&self) -> &[CachedConvexData] {
        self.pre_cached_phys_data.as_deref().unwrap_or(&[])
    }

    /// Bytes of pre-cooked physics data.
    pub fn cached_bytes(&self) -> u64 {
        self.cached_data()
            .iter()
            .flat_map(|d| d.elements.iter())
            .map(|e| u64::try_from(e.0.len()).unwrap_or(u64::MAX))
            .fold(0, u64::saturating_add)
    }
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

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn resolve(
    names: &dyn NameTable,
    n: FName,
    what: &'static str,
    offset: usize,
) -> ObjResult<String> {
    let base = usize::try_from(n.index)
        .ok()
        .and_then(|i| names.name_at(i))
        .ok_or_else(|| {
            malformed(
                what,
                offset,
                format!(
                    "name index {} outside the name table ({} names)",
                    n.index,
                    names.name_count()
                ),
            )
        })?;
    if n.number < 0 {
        return Err(malformed(
            what,
            offset,
            format!("negative name instance number {}", n.number),
        ));
    }
    Ok(n.display_with(base))
}

fn read_name(
    r: &mut Reader<'_>,
    names: &dyn NameTable,
    what: &'static str,
) -> ObjResult<(FName, String)> {
    let at = r.position();
    let n = r.read_fname()?;
    let text = resolve(names, n, what, at)?;
    Ok((n, text))
}

/// The property types this decoder reads.
const TAG_TYPES: [&str; 9] = [
    "StructProperty",
    "ArrayProperty",
    "BoolProperty",
    "IntProperty",
    "FloatProperty",
    "NameProperty",
    "ObjectProperty",
    "ByteProperty",
    "StrProperty",
];

/// A tag header and the byte range of its value.
struct Tag {
    offset: usize,
    raw_name: FName,
    name: String,
    ty: String,
    struct_name: Option<String>,
    enum_name: Option<(FName, String)>,
    bool_value: Option<bool>,
    start: usize,
    end: usize,
}

impl Tag {
    fn size(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn wrong(&self, expected: &str) -> ObjectError {
        let found = match &self.struct_name {
            Some(s) => format!("{} {s}", self.ty),
            None => self.ty.clone(),
        };
        malformed(
            "property tag type",
            self.offset,
            format!("{} is a {found}, expected {expected}", self.name),
        )
    }

    fn expect(&self, ty: &str) -> ObjResult<()> {
        if self.ty == ty {
            Ok(())
        } else {
            Err(self.wrong(ty))
        }
    }

    fn expect_struct(&self, struct_name: &str) -> ObjResult<()> {
        if self.ty == "StructProperty"
            && self
                .struct_name
                .as_deref()
                .is_some_and(|s| eq(s, struct_name))
        {
            Ok(())
        } else {
            Err(self.wrong(&format!("StructProperty {struct_name}")))
        }
    }

    fn bool(&self) -> ObjResult<bool> {
        self.expect("BoolProperty")?;
        self.bool_value.ok_or_else(|| self.wrong("BoolProperty"))
    }

    /// A cursor over exactly this tag's value.
    fn value<'a>(&self, r: &Reader<'a>) -> ObjResult<Reader<'a>> {
        let data = r.data().get(..self.end).ok_or_else(|| {
            malformed(
                "property tag size",
                self.offset,
                "value runs past the end".to_owned(),
            )
        })?;
        Ok(Reader::at(data, self.start)?)
    }

    /// Require that `vr` consumed the whole value.
    fn finish(&self, vr: &Reader<'_>) -> ObjResult<()> {
        if vr.position() == self.end {
            Ok(())
        } else {
            Err(malformed(
                "property value",
                self.start,
                format!(
                    "{} ({}): decoded {} of {} bytes",
                    self.name,
                    self.ty,
                    vr.position().saturating_sub(self.start),
                    self.size()
                ),
            ))
        }
    }
}

/// Read one tag header; `None` at the terminating `None` name. The cursor is
/// left at the start of the value.
fn read_tag(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<Option<Tag>> {
    let offset = r.position();
    let (raw_name, name) = read_name(r, names, "property tag name")?;
    if eq(&name, "None") {
        return Ok(None);
    }
    let (_, ty) = read_name(r, names, "property tag type")?;
    // Names compare without regard to case; use the canonical spelling.
    let ty = TAG_TYPES
        .iter()
        .find(|t| eq(t, &ty))
        .map_or(ty, |t| (*t).to_owned());
    let size_at = r.position();
    let size = r.read_i32()?;
    let size = usize::try_from(size).map_err(|_| {
        malformed(
            "property tag size",
            size_at,
            format!("negative size {size}"),
        )
    })?;
    let idx_at = r.position();
    let array_index = r.read_i32()?;
    if array_index != 0 {
        return Err(malformed(
            "property tag array index",
            idx_at,
            format!(
                "{name}: static array index {array_index} (no body setup property is a static array)"
            ),
        ));
    }
    let mut struct_name = None;
    let mut enum_name = None;
    let mut bool_value = None;
    match ty.as_str() {
        "StructProperty" => struct_name = Some(read_name(r, names, "struct tag name")?.1),
        "BoolProperty" => {
            let at = r.position();
            bool_value = Some(match r.read_u8()? {
                0 => false,
                1 => true,
                v => {
                    return Err(malformed(
                        "boolean tag value",
                        at,
                        format!("{name}: boolean is {v}, not 0 or 1"),
                    ));
                }
            });
        }
        "ByteProperty" => enum_name = Some(read_name(r, names, "byte tag enum name")?),
        _ => {}
    }
    let start = r.position();
    let end = start
        .checked_add(size)
        .filter(|e| *e <= r.len())
        .ok_or_else(|| {
            malformed(
                "property tag size",
                size_at,
                format!(
                    "{name}: value of {size} bytes runs past the end ({} left)",
                    r.remaining()
                ),
            )
        })?;
    Ok(Some(Tag {
        offset,
        raw_name,
        name,
        ty,
        struct_name,
        enum_name,
        bool_value,
        start,
        end,
    }))
}

/// Position of `tag` among `members` (declaration order), which must lie
/// after the previous member's: a member may be absent but never repeated or
/// out of order.
fn member_index(
    tag: &Tag,
    owner: &'static str,
    members: &[&str],
    last: &mut Option<usize>,
) -> ObjResult<usize> {
    let idx = members
        .iter()
        .position(|m| eq(m, &tag.name))
        .ok_or_else(|| {
            malformed(
                "property tag name",
                tag.offset,
                format!("{} is not a member of {owner}", tag.name),
            )
        })?;
    if last.is_some_and(|l| idx <= l) {
        return Err(malformed(
            "property tag order",
            tag.offset,
            format!(
                "{} of {owner} is repeated or out of declaration order",
                tag.name
            ),
        ));
    }
    *last = Some(idx);
    Ok(idx)
}

fn read_vector(r: &mut Reader<'_>) -> ObjResult<[f32; 3]> {
    Ok([r.read_f32()?, r.read_f32()?, r.read_f32()?])
}

/// `FPlane` in its stored order W, X, Y, Z.
fn read_plane(r: &mut Reader<'_>) -> ObjResult<Plane> {
    let w = r.read_f32()?;
    let x = r.read_f32()?;
    let y = r.read_f32()?;
    let z = r.read_f32()?;
    Ok(Plane { x, y, z, w })
}

fn read_matrix(r: &mut Reader<'_>) -> ObjResult<Matrix> {
    let mut m = [[0.0f32; 4]; 4];
    for row in &mut m {
        let p = read_plane(r)?;
        *row = [p.x, p.y, p.z, p.w];
    }
    Ok(m)
}

fn read_elem_box(r: &mut Reader<'_>) -> ObjResult<ElemBox> {
    Ok(ElemBox {
        min: read_vector(r)?,
        max: read_vector(r)?,
        is_valid: r.read_u8()?,
    })
}

/// `i32 Count` then `Count` fixed-size items.
fn read_items<T>(
    r: &mut Reader<'_>,
    what: &'static str,
    item_size: usize,
    mut f: impl FnMut(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<Vec<T>> {
    let n = r.read_count(what, item_size)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(f(r)?);
    }
    Ok(out)
}

/// `i32 Count` then `Count` tagged structs.
fn read_structs<T>(
    r: &mut Reader<'_>,
    names: &dyn NameTable,
    what: &'static str,
    mut f: impl FnMut(&mut Reader<'_>, &dyn NameTable) -> ObjResult<T>,
) -> ObjResult<Vec<T>> {
    let n = r.read_count(what, STRUCT_MIN_SIZE)?;
    let mut out = Vec::with_capacity(n.min(MAX_PREALLOC));
    for _ in 0..n {
        out.push(f(r, names)?);
    }
    Ok(out)
}

fn array_value<T>(
    tag: &Tag,
    r: &Reader<'_>,
    read: impl FnOnce(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<T> {
    tag.expect("ArrayProperty")?;
    let mut vr = tag.value(r)?;
    let v = read(&mut vr)?;
    tag.finish(&vr)?;
    Ok(v)
}

fn struct_value<T>(
    tag: &Tag,
    r: &Reader<'_>,
    struct_name: &str,
    read: impl FnOnce(&mut Reader<'_>) -> ObjResult<T>,
) -> ObjResult<T> {
    tag.expect_struct(struct_name)?;
    let mut vr = tag.value(r)?;
    let v = read(&mut vr)?;
    tag.finish(&vr)?;
    Ok(v)
}

fn float_value(tag: &Tag, r: &Reader<'_>) -> ObjResult<f32> {
    tag.expect("FloatProperty")?;
    let mut vr = tag.value(r)?;
    let v = vr.read_f32()?;
    tag.finish(&vr)?;
    Ok(v)
}

fn bool_value(tag: &Tag, r: &Reader<'_>) -> ObjResult<bool> {
    let v = tag.bool()?;
    tag.finish(&tag.value(r)?)?;
    Ok(v)
}

const CONVEX_MEMBERS: [&str; 7] = [
    "VertexData",
    "PermutedVertexData",
    "FaceTriData",
    "EdgeDirections",
    "FaceNormalDirections",
    "FacePlaneData",
    "ElemBox",
];
const SPHERE_MEMBERS: [&str; 4] = ["TM", "Radius", "bNoRBCollision", "bPerPolyShape"];
const BOX_MEMBERS: [&str; 6] = ["TM", "X", "Y", "Z", "bNoRBCollision", "bPerPolyShape"];
const SPHYL_MEMBERS: [&str; 5] = ["TM", "Radius", "Length", "bNoRBCollision", "bPerPolyShape"];
const AGG_MEMBERS: [&str; 5] = [
    "SphereElems",
    "BoxElems",
    "SphylElems",
    "ConvexElems",
    "bSkipCloseAndParallelChecks",
];

fn read_convex(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<ConvexElem> {
    let mut e = ConvexElem::default();
    let mut last = None;
    while let Some(tag) = read_tag(r, names)? {
        match member_index(&tag, "KConvexElem", &CONVEX_MEMBERS, &mut last)? {
            0 => {
                e.vertex_data = Some(array_value(&tag, r, |v| {
                    read_items(v, "VertexData", VECTOR_SIZE, read_vector)
                })?);
            }
            1 => {
                e.permuted_vertex_data = Some(array_value(&tag, r, |v| {
                    read_items(v, "PermutedVertexData", PLANE_SIZE, read_plane)
                })?);
            }
            2 => {
                e.face_tri_data = Some(array_value(&tag, r, |v| {
                    read_items(v, "FaceTriData", 4, |x| Ok(x.read_i32()?))
                })?);
            }
            3 => {
                e.edge_directions = Some(array_value(&tag, r, |v| {
                    read_items(v, "EdgeDirections", VECTOR_SIZE, read_vector)
                })?);
            }
            4 => {
                e.face_normal_directions = Some(array_value(&tag, r, |v| {
                    read_items(v, "FaceNormalDirections", VECTOR_SIZE, read_vector)
                })?);
            }
            5 => {
                e.face_plane_data = Some(array_value(&tag, r, |v| {
                    read_items(v, "FacePlaneData", PLANE_SIZE, read_plane)
                })?);
            }
            _ => e.elem_box = Some(struct_value(&tag, r, "Box", read_elem_box)?),
        }
        r.seek(tag.end)?;
    }
    Ok(e)
}

fn read_sphere(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<SphereElem> {
    let mut e = SphereElem::default();
    let mut last = None;
    while let Some(tag) = read_tag(r, names)? {
        match member_index(&tag, "KSphereElem", &SPHERE_MEMBERS, &mut last)? {
            0 => e.tm = Some(struct_value(&tag, r, "Matrix", read_matrix)?),
            1 => e.radius = Some(float_value(&tag, r)?),
            2 => e.no_rb_collision = Some(bool_value(&tag, r)?),
            _ => e.per_poly_shape = Some(bool_value(&tag, r)?),
        }
        r.seek(tag.end)?;
    }
    Ok(e)
}

fn read_box(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<BoxElem> {
    let mut e = BoxElem::default();
    let mut last = None;
    while let Some(tag) = read_tag(r, names)? {
        match member_index(&tag, "KBoxElem", &BOX_MEMBERS, &mut last)? {
            0 => e.tm = Some(struct_value(&tag, r, "Matrix", read_matrix)?),
            1 => e.x = Some(float_value(&tag, r)?),
            2 => e.y = Some(float_value(&tag, r)?),
            3 => e.z = Some(float_value(&tag, r)?),
            4 => e.no_rb_collision = Some(bool_value(&tag, r)?),
            _ => e.per_poly_shape = Some(bool_value(&tag, r)?),
        }
        r.seek(tag.end)?;
    }
    Ok(e)
}

fn read_sphyl(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<SphylElem> {
    let mut e = SphylElem::default();
    let mut last = None;
    while let Some(tag) = read_tag(r, names)? {
        match member_index(&tag, "KSphylElem", &SPHYL_MEMBERS, &mut last)? {
            0 => e.tm = Some(struct_value(&tag, r, "Matrix", read_matrix)?),
            1 => e.radius = Some(float_value(&tag, r)?),
            2 => e.length = Some(float_value(&tag, r)?),
            3 => e.no_rb_collision = Some(bool_value(&tag, r)?),
            _ => e.per_poly_shape = Some(bool_value(&tag, r)?),
        }
        r.seek(tag.end)?;
    }
    Ok(e)
}

fn read_agg_geom(r: &mut Reader<'_>, names: &dyn NameTable) -> ObjResult<AggGeom> {
    let mut g = AggGeom::default();
    let mut last = None;
    while let Some(tag) = read_tag(r, names)? {
        match member_index(&tag, "KAggregateGeom", &AGG_MEMBERS, &mut last)? {
            0 => {
                g.sphere_elems = Some(array_value(&tag, r, |v| {
                    read_structs(v, names, "SphereElems", read_sphere)
                })?);
            }
            1 => {
                g.box_elems = Some(array_value(&tag, r, |v| {
                    read_structs(v, names, "BoxElems", read_box)
                })?);
            }
            2 => {
                g.sphyl_elems = Some(array_value(&tag, r, |v| {
                    read_structs(v, names, "SphylElems", read_sphyl)
                })?);
            }
            3 => {
                g.convex_elems = Some(array_value(&tag, r, |v| {
                    read_structs(v, names, "ConvexElems", read_convex)
                })?);
            }
            _ => g.skip_close_and_parallel_checks = Some(bool_value(&tag, r)?),
        }
        r.seek(tag.end)?;
    }
    Ok(g)
}

fn read_scalar(tag: &Tag, r: &Reader<'_>, names: &dyn NameTable) -> ObjResult<ScalarValue> {
    let mut vr = tag.value(r)?;
    let value = match tag.ty.as_str() {
        "BoolProperty" => ScalarValue::Bool(tag.bool()?),
        "IntProperty" => ScalarValue::Int(vr.read_i32()?),
        "FloatProperty" => ScalarValue::Float(vr.read_f32()?),
        "NameProperty" => {
            let (raw, text) = read_name(&mut vr, names, "name value")?;
            ScalarValue::Name { text, raw }
        }
        "ObjectProperty" => ScalarValue::Object(vr.read_package_index()?),
        "ByteProperty" => {
            let (raw_enum, enum_name) = tag
                .enum_name
                .clone()
                .ok_or_else(|| tag.wrong("ByteProperty"))?;
            if tag.size() == 8 {
                let (raw_value, value) = read_name(&mut vr, names, "enum value")?;
                ScalarValue::Enum {
                    enum_name,
                    raw_enum,
                    value,
                    raw_value,
                }
            } else {
                ScalarValue::Byte {
                    enum_name,
                    raw_enum,
                    value: vr.read_u8()?,
                }
            }
        }
        _ => {
            return Err(malformed(
                "property tag type",
                tag.offset,
                format!(
                    "{} ({}): not a property this decoder knows",
                    tag.name, tag.ty
                ),
            ));
        }
    };
    tag.finish(&vr)?;
    Ok(value)
}

fn read_cached_phys_data(r: &mut Reader<'_>) -> ObjResult<Vec<CachedConvexData>> {
    // Every entry holds at least its element count; every element at least
    // the two words of a bulk array header.
    let n = r.read_count("PreCachedPhysData", 4)?;
    let mut out = Vec::with_capacity(n.min(MAX_PREALLOC));
    for _ in 0..n {
        let m = r.read_count("CachedConvexElements", 8)?;
        let mut elements = Vec::with_capacity(m.min(MAX_PREALLOC));
        for _ in 0..m {
            let at = r.position();
            let elem = r.read_i32()?;
            if usize::try_from(elem).ok() != Some(CACHED_ELEMENT_SIZE) {
                return Err(malformed(
                    "ConvexElementData",
                    at,
                    format!("bulk element size {elem}, expected {CACHED_ELEMENT_SIZE}"),
                ));
            }
            let len = r.read_count("ConvexElementData", CACHED_ELEMENT_SIZE)?;
            elements.push(CookedBlob(r.read_bytes(len)?.to_vec()));
        }
        out.push(CachedConvexData { elements });
    }
    Ok(out)
}

/// Decode an `RB_BodySetup` payload (`NetIndex`, tagged properties, native
/// data). The export must not carry a state frame. A class default object
/// (`class_default_object`) has no native data. The decoder must end exactly
/// at the end of `data`.
pub fn decode_body_setup_payload(
    data: &[u8],
    names: &dyn NameTable,
    class_default_object: bool,
) -> ObjResult<BodySetup> {
    let mut r = Reader::new(data);
    let net_index = r.read_i32()?;
    let mut properties = Vec::new();
    let (mut agg, mut scale, mut nudge) = (false, false, false);
    let mut seen: HashSet<String> = HashSet::new();
    while let Some(tag) = read_tag(&mut r, names)? {
        let once = |flag: &mut bool| -> ObjResult<()> {
            if *flag {
                return Err(malformed(
                    "property tag name",
                    tag.offset,
                    format!("{} is tagged twice", tag.name),
                ));
            }
            *flag = true;
            Ok(())
        };
        let prop = if eq(&tag.name, "AggGeom") {
            once(&mut agg)?;
            BodyProperty::AggGeom(struct_value(&tag, &r, "KAggregateGeom", |v| {
                read_agg_geom(v, names)
            })?)
        } else if eq(&tag.name, "PreCachedPhysScale") {
            once(&mut scale)?;
            BodyProperty::PreCachedPhysScale(array_value(&tag, &r, |v| {
                read_items(v, "PreCachedPhysScale", VECTOR_SIZE, read_vector)
            })?)
        } else if eq(&tag.name, "COMNudge") {
            once(&mut nudge)?;
            BodyProperty::ComNudge(struct_value(&tag, &r, "Vector", read_vector)?)
        } else {
            if !seen.insert(tag.name.to_ascii_lowercase()) {
                return Err(malformed(
                    "property tag name",
                    tag.offset,
                    format!("{} is tagged twice", tag.name),
                ));
            }
            BodyProperty::Scalar(ScalarProperty {
                name: tag.name.clone(),
                raw_name: tag.raw_name,
                value: read_scalar(&tag, &r, names)?,
            })
        };
        properties.push(prop);
        r.seek(tag.end)?;
    }
    let native_start = r.position();
    let pre_cached_phys_data = if class_default_object {
        None
    } else {
        Some(read_cached_phys_data(&mut r)?)
    };
    if r.remaining() != 0 {
        return Err(malformed(
            "RB_BodySetup native data",
            r.position(),
            format!(
                "{} bytes left after the last known field (payload {} bytes)",
                r.remaining(),
                data.len()
            ),
        ));
    }
    Ok(BodySetup {
        net_index,
        properties,
        native_start,
        pre_cached_phys_data,
    })
}

/// True when export `index` is an `Engine.RB_BodySetup` (exactly; the class
/// has no subclass in the shipped packages). Inside `Engine.u` the class is
/// one of the package's own top-level exports.
pub fn is_body_setup(pkg: &Package, index: usize) -> bool {
    if !pkg
        .export_class_name(index)
        .is_ok_and(|c| c == "RB_BodySetup")
    {
        return false;
    }
    match pkg.export_class_package(index) {
        Ok(Some(p)) => p.eq_ignore_ascii_case("Engine"),
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Decode export `index` as an `RB_BodySetup`.
pub fn decode_body_setup(pkg: &Package, index: usize) -> ObjResult<BodySetup> {
    if !is_body_setup(pkg, index) {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Engine.RB_BodySetup",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    }
    let object_flags = pkg.export(index)?.object_flags;
    if object_flags & flags::object::HAS_STACK != 0 {
        return Err(malformed(
            "RB_BodySetup prelude",
            0,
            "the export carries a state frame (never the case in the shipped packages)",
        ));
    }
    decode_body_setup_payload(
        pkg.export_data(index)?,
        pkg,
        object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0,
    )
}

// ---------------------------------------------------------------------------
// Encoder (exact inverse of the decoder)
// ---------------------------------------------------------------------------

fn put_count(w: &mut Writer, n: usize) -> Option<()> {
    w.i32(i32::try_from(n).ok()?);
    Some(())
}

fn put_f32(w: &mut Writer, v: f32) {
    w.u32(v.to_bits());
}

fn put_vector(w: &mut Writer, v: [f32; 3]) {
    v.iter().for_each(|c| put_f32(w, *c));
}

fn put_plane(w: &mut Writer, p: Plane) {
    [p.w, p.x, p.y, p.z].iter().for_each(|c| put_f32(w, *c));
}

fn put_matrix(w: &mut Writer, m: &Matrix) {
    for row in m {
        put_plane(
            w,
            Plane {
                x: row[0],
                y: row[1],
                z: row[2],
                w: row[3],
            },
        );
    }
}

enum Extra<'a> {
    None,
    Struct(&'a str),
    Bool(bool),
    Enum(FName),
}

struct Encoder<'n> {
    names: &'n NameIndex,
}

impl Encoder<'_> {
    fn tag_raw(
        &self,
        w: &mut Writer,
        name: FName,
        ty: &str,
        extra: &Extra<'_>,
        value: &[u8],
    ) -> Option<()> {
        w.fname(name);
        w.fname(self.names.get(ty)?);
        put_count(w, value.len())?;
        w.i32(0);
        match extra {
            Extra::None => {}
            Extra::Struct(s) => w.fname(self.names.get(s)?),
            Extra::Bool(b) => w.u8(u8::from(*b)),
            Extra::Enum(e) => w.fname(*e),
        }
        w.bytes(value);
        Some(())
    }

    fn tag(
        &self,
        w: &mut Writer,
        name: &str,
        ty: &str,
        extra: &Extra<'_>,
        value: &[u8],
    ) -> Option<()> {
        self.tag_raw(w, self.names.get(name)?, ty, extra, value)
    }

    fn none(&self, w: &mut Writer) -> Option<()> {
        w.fname(self.names.get("None")?);
        Some(())
    }

    fn array<T>(
        &self,
        w: &mut Writer,
        name: &str,
        items: Option<&Vec<T>>,
        mut put: impl FnMut(&mut Writer, &T) -> Option<()>,
    ) -> Option<()> {
        let Some(items) = items else { return Some(()) };
        let mut v = Writer::new();
        put_count(&mut v, items.len())?;
        for it in items {
            put(&mut v, it)?;
        }
        self.tag(w, name, "ArrayProperty", &Extra::None, &v.into_bytes())
    }

    fn float(&self, w: &mut Writer, name: &str, v: Option<f32>) -> Option<()> {
        let Some(v) = v else { return Some(()) };
        self.tag(
            w,
            name,
            "FloatProperty",
            &Extra::None,
            &v.to_bits().to_le_bytes(),
        )
    }

    fn bool(&self, w: &mut Writer, name: &str, v: Option<bool>) -> Option<()> {
        let Some(v) = v else { return Some(()) };
        self.tag(w, name, "BoolProperty", &Extra::Bool(v), &[])
    }

    fn matrix(&self, w: &mut Writer, name: &str, m: Option<&Matrix>) -> Option<()> {
        let Some(m) = m else { return Some(()) };
        let mut v = Writer::new();
        put_matrix(&mut v, m);
        self.tag(
            w,
            name,
            "StructProperty",
            &Extra::Struct("Matrix"),
            &v.into_bytes(),
        )
    }

    fn convex(&self, w: &mut Writer, e: &ConvexElem) -> Option<()> {
        let vectors = |w: &mut Writer, v: &[f32; 3]| {
            put_vector(w, *v);
            Some(())
        };
        let planes = |w: &mut Writer, p: &Plane| {
            put_plane(w, *p);
            Some(())
        };
        self.array(w, "VertexData", e.vertex_data.as_ref(), vectors)?;
        self.array(
            w,
            "PermutedVertexData",
            e.permuted_vertex_data.as_ref(),
            planes,
        )?;
        self.array(w, "FaceTriData", e.face_tri_data.as_ref(), |w, i| {
            w.i32(*i);
            Some(())
        })?;
        self.array(w, "EdgeDirections", e.edge_directions.as_ref(), vectors)?;
        self.array(
            w,
            "FaceNormalDirections",
            e.face_normal_directions.as_ref(),
            vectors,
        )?;
        self.array(w, "FacePlaneData", e.face_plane_data.as_ref(), planes)?;
        if let Some(b) = &e.elem_box {
            let mut v = Writer::new();
            put_vector(&mut v, b.min);
            put_vector(&mut v, b.max);
            v.u8(b.is_valid);
            self.tag(
                w,
                "ElemBox",
                "StructProperty",
                &Extra::Struct("Box"),
                &v.into_bytes(),
            )?;
        }
        self.none(w)
    }

    fn agg_geom(&self, g: &AggGeom) -> Option<Vec<u8>> {
        let mut w = Writer::new();
        self.array(&mut w, "SphereElems", g.sphere_elems.as_ref(), |w, e| {
            self.matrix(w, "TM", e.tm.as_ref())?;
            self.float(w, "Radius", e.radius)?;
            self.bool(w, "bNoRBCollision", e.no_rb_collision)?;
            self.bool(w, "bPerPolyShape", e.per_poly_shape)?;
            self.none(w)
        })?;
        self.array(&mut w, "BoxElems", g.box_elems.as_ref(), |w, e| {
            self.matrix(w, "TM", e.tm.as_ref())?;
            self.float(w, "X", e.x)?;
            self.float(w, "Y", e.y)?;
            self.float(w, "Z", e.z)?;
            self.bool(w, "bNoRBCollision", e.no_rb_collision)?;
            self.bool(w, "bPerPolyShape", e.per_poly_shape)?;
            self.none(w)
        })?;
        self.array(&mut w, "SphylElems", g.sphyl_elems.as_ref(), |w, e| {
            self.matrix(w, "TM", e.tm.as_ref())?;
            self.float(w, "Radius", e.radius)?;
            self.float(w, "Length", e.length)?;
            self.bool(w, "bNoRBCollision", e.no_rb_collision)?;
            self.bool(w, "bPerPolyShape", e.per_poly_shape)?;
            self.none(w)
        })?;
        self.array(&mut w, "ConvexElems", g.convex_elems.as_ref(), |w, e| {
            self.convex(w, e)
        })?;
        self.bool(
            &mut w,
            "bSkipCloseAndParallelChecks",
            g.skip_close_and_parallel_checks,
        )?;
        self.none(&mut w)?;
        Some(w.into_bytes())
    }

    fn scalar(&self, w: &mut Writer, p: &ScalarProperty) -> Option<()> {
        match &p.value {
            ScalarValue::Bool(b) => {
                self.tag_raw(w, p.raw_name, "BoolProperty", &Extra::Bool(*b), &[])
            }
            ScalarValue::Int(v) => {
                self.tag_raw(w, p.raw_name, "IntProperty", &Extra::None, &v.to_le_bytes())
            }
            ScalarValue::Float(v) => self.tag_raw(
                w,
                p.raw_name,
                "FloatProperty",
                &Extra::None,
                &v.to_bits().to_le_bytes(),
            ),
            ScalarValue::Name { raw, .. } => {
                let mut v = Writer::new();
                v.fname(*raw);
                self.tag_raw(w, p.raw_name, "NameProperty", &Extra::None, &v.into_bytes())
            }
            ScalarValue::Object(o) => self.tag_raw(
                w,
                p.raw_name,
                "ObjectProperty",
                &Extra::None,
                &o.0.to_le_bytes(),
            ),
            ScalarValue::Byte {
                raw_enum, value, ..
            } => self.tag_raw(
                w,
                p.raw_name,
                "ByteProperty",
                &Extra::Enum(*raw_enum),
                &[*value],
            ),
            ScalarValue::Enum {
                raw_enum,
                raw_value,
                ..
            } => {
                let mut v = Writer::new();
                v.fname(*raw_value);
                self.tag_raw(
                    w,
                    p.raw_name,
                    "ByteProperty",
                    &Extra::Enum(*raw_enum),
                    &v.into_bytes(),
                )
            }
        }
    }
}

/// Encode a body setup into the payload [`decode_body_setup_payload`] reads
/// (`native_start` is recomputed, not used). Names that are not stored as
/// raw references (tag names of the known members, type and struct names)
/// are looked up in `names`. `None` when a name is missing or a count does
/// not fit an `i32`.
pub fn encode_body_setup(b: &BodySetup, names: &NameIndex) -> Option<Vec<u8>> {
    let enc = Encoder { names };
    let mut w = Writer::new();
    w.i32(b.net_index);
    for p in &b.properties {
        match p {
            BodyProperty::Scalar(s) => enc.scalar(&mut w, s)?,
            BodyProperty::PreCachedPhysScale(v) => {
                enc.array(&mut w, "PreCachedPhysScale", Some(v), |w, s| {
                    put_vector(w, *s);
                    Some(())
                })?;
            }
            BodyProperty::ComNudge(v) => {
                let mut x = Writer::new();
                put_vector(&mut x, *v);
                enc.tag(
                    &mut w,
                    "COMNudge",
                    "StructProperty",
                    &Extra::Struct("Vector"),
                    &x.into_bytes(),
                )?;
            }
            BodyProperty::AggGeom(g) => {
                let v = enc.agg_geom(g)?;
                enc.tag(
                    &mut w,
                    "AggGeom",
                    "StructProperty",
                    &Extra::Struct("KAggregateGeom"),
                    &v,
                )?;
            }
        }
    }
    enc.none(&mut w)?;
    if let Some(data) = &b.pre_cached_phys_data {
        put_count(&mut w, data.len())?;
        for d in data {
            put_count(&mut w, d.elements.len())?;
            for e in &d.elements {
                put_count(&mut w, CACHED_ELEMENT_SIZE)?;
                put_count(&mut w, e.0.len())?;
                w.bytes(&e.0);
            }
        }
    }
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Derived data and structural cross-checks
// ---------------------------------------------------------------------------

/// `PermutedVertexData` of `vertices`: the vertices are taken four at a time
/// and each group is stored as three planes holding the four X, the four Y
/// and the four Z coordinates (vertex `4k + i` in component `i` of
/// `x, y, z, w`). A last group of fewer than four is filled by repeating
/// that group's first vertex. Every shipped convex element stores exactly
/// this, bit for bit.
pub fn permute_vertex_data(vertices: &[[f32; 3]]) -> Vec<Plane> {
    let mut out = Vec::with_capacity(vertices.len().div_ceil(4).saturating_mul(3));
    for group in vertices.chunks(4) {
        let Some(first) = group.first() else { continue };
        for axis in 0..3 {
            let at = |i: usize| group.get(i).unwrap_or(first)[axis];
            out.push(Plane {
                x: at(0),
                y: at(1),
                z: at(2),
                w: at(3),
            });
        }
    }
    out
}

fn finite3(v: &[f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

fn finite_plane(p: &Plane) -> bool {
    [p.x, p.y, p.z, p.w].iter().all(|c| c.is_finite())
}

fn finite_matrix(m: &Matrix) -> bool {
    m.iter().flatten().all(|c| c.is_finite())
}

/// True when the matrix's fourth column is exactly `(0, 0, 0, 1)`: an affine
/// transform whose translation is the fourth row.
pub fn is_affine(m: &Matrix) -> bool {
    m[0][3] == 0.0 && m[1][3] == 0.0 && m[2][3] == 0.0 && m[3][3] == 1.0
}

/// Structural cross-checks of a decoded body setup (empty = consistent):
///
/// - every float is finite;
/// - every shape has all of its members;
/// - `FaceTriData` holds whole triangles whose indices are valid vertices;
/// - `PermutedVertexData` is [`permute_vertex_data`] of `VertexData`, bit
///   for bit;
/// - `ElemBox` is valid and is exactly the vertices' bounding box;
/// - every `TM` is affine ([`is_affine`]);
/// - radii, lengths and box sizes are not negative;
/// - the pre-cooked physics data has one entry per `PreCachedPhysScale` and
///   each entry one blob per convex element.
pub fn validate_body_setup(b: &BodySetup) -> Vec<String> {
    let mut issues = Vec::new();
    let geom = b.agg_geom();
    if let Some(scales) = b.pre_cached_phys_scale()
        && !scales.iter().all(finite3)
    {
        issues.push("PreCachedPhysScale has a non-finite value".to_owned());
    }
    let scales = b.pre_cached_phys_scale().map_or(0, <[_]>::len);
    if b.cached_data().len() != scales {
        issues.push(format!(
            "{} pre-cooked entries for {scales} pre-cooked scales",
            b.cached_data().len()
        ));
    }
    let convex = geom.map_or(0, |g| g.convex().len());
    for (i, d) in b.cached_data().iter().enumerate() {
        if d.elements.len() != convex {
            issues.push(format!(
                "pre-cooked entry {i} has {} blobs for {convex} convex elements",
                d.elements.len()
            ));
        }
    }
    let Some(g) = geom else { return issues };
    let tm_issue = |kind: &str, i: usize, tm: Option<&Matrix>, issues: &mut Vec<String>| match tm {
        None => issues.push(format!("{kind} {i}: TM is absent")),
        Some(m) if !finite_matrix(m) => issues.push(format!("{kind} {i}: TM is not finite")),
        Some(m) if !is_affine(m) => issues.push(format!("{kind} {i}: TM is not affine")),
        Some(_) => {}
    };
    let size_issue =
        |kind: &str, i: usize, name: &str, v: Option<f32>, issues: &mut Vec<String>| match v {
            None => issues.push(format!("{kind} {i}: {name} is absent")),
            Some(v) if !v.is_finite() || v < 0.0 => {
                issues.push(format!("{kind} {i}: {name} is {v}"));
            }
            Some(_) => {}
        };
    for (i, e) in g.spheres().iter().enumerate() {
        tm_issue("sphere", i, e.tm.as_ref(), &mut issues);
        size_issue("sphere", i, "Radius", e.radius, &mut issues);
    }
    for (i, e) in g.boxes().iter().enumerate() {
        tm_issue("box", i, e.tm.as_ref(), &mut issues);
        size_issue("box", i, "X", e.x, &mut issues);
        size_issue("box", i, "Y", e.y, &mut issues);
        size_issue("box", i, "Z", e.z, &mut issues);
    }
    for (i, e) in g.sphyls().iter().enumerate() {
        tm_issue("sphyl", i, e.tm.as_ref(), &mut issues);
        size_issue("sphyl", i, "Radius", e.radius, &mut issues);
        size_issue("sphyl", i, "Length", e.length, &mut issues);
    }
    for (i, e) in g.convex().iter().enumerate() {
        validate_convex(i, e, &mut issues);
    }
    issues
}

fn validate_convex(i: usize, e: &ConvexElem, issues: &mut Vec<String>) {
    let (
        Some(verts),
        Some(permuted),
        Some(tris),
        Some(edges),
        Some(normals),
        Some(planes),
        Some(bx),
    ) = (
        &e.vertex_data,
        &e.permuted_vertex_data,
        &e.face_tri_data,
        &e.edge_directions,
        &e.face_normal_directions,
        &e.face_plane_data,
        &e.elem_box,
    )
    else {
        issues.push(format!("convex {i}: a member is absent"));
        return;
    };
    if !(verts.iter().all(finite3)
        && edges.iter().all(finite3)
        && normals.iter().all(finite3)
        && planes.iter().all(finite_plane)
        && permuted.iter().all(finite_plane)
        && finite3(&bx.min)
        && finite3(&bx.max))
    {
        issues.push(format!("convex {i}: a value is not finite"));
        return;
    }
    if tris.len() % 3 != 0 {
        issues.push(format!(
            "convex {i}: FaceTriData has {} indices, not whole triangles",
            tris.len()
        ));
    }
    if let Some(bad) = tris
        .iter()
        .find(|&&t| usize::try_from(t).map_or(true, |t| t >= verts.len()))
    {
        issues.push(format!(
            "convex {i}: FaceTriData index {bad} outside the {} vertices",
            verts.len()
        ));
    }
    let expected = permute_vertex_data(verts);
    let same = expected.len() == permuted.len()
        && expected.iter().zip(permuted).all(|(a, b)| {
            [a.x, a.y, a.z, a.w]
                .iter()
                .zip([b.x, b.y, b.z, b.w])
                .all(|(p, q)| p.to_bits() == q.to_bits())
        });
    if !same {
        issues.push(format!(
            "convex {i}: PermutedVertexData is not the permutation of VertexData"
        ));
    }
    if bx.is_valid != 1 {
        issues.push(format!("convex {i}: ElemBox.IsValid is {}", bx.is_valid));
    }
    if let Some(first) = verts.first() {
        let (mut lo, mut hi) = (*first, *first);
        for v in verts {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        if lo != bx.min || hi != bx.max {
            issues.push(format!(
                "convex {i}: ElemBox is not the vertices' bounding box"
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage over a package
// ---------------------------------------------------------------------------

/// Most failure samples kept per package.
const MAX_FAILURE_SAMPLES: usize = 16;

/// `RB_BodySetup` coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BodySetupCoverage {
    /// Package name.
    pub package: String,
    /// `RB_BodySetup` exports found.
    pub total: usize,
    /// Exports decoded with every payload byte consumed.
    pub exact: usize,
    /// Exports whose decoded fields re-encode ([`encode_body_setup`]) to
    /// exactly the original payload.
    pub round_trip: usize,
    /// Exports that also pass [`validate_body_setup`].
    pub valid: usize,
    /// Payload bytes of the exact exports.
    pub payload_bytes: u64,
    /// Class name of the export's outer (`StaticMesh`, `PhysicsAsset`, or
    /// `<none>` for the class default object) → exports.
    pub owners: BTreeMap<String, usize>,
    /// Exports with an `AggGeom` tag.
    pub with_agg_geom: usize,
    /// Exports with an `AggGeom` that holds no shape at all.
    pub empty_agg_geom: usize,
    /// Convex elements.
    pub convex: usize,
    /// Boxes.
    pub boxes: usize,
    /// Spheres.
    pub spheres: usize,
    /// Capsules.
    pub sphyls: usize,
    /// Convex vertices.
    pub convex_vertices: u64,
    /// Convex planes.
    pub convex_planes: u64,
    /// Exports with pre-cooked physics data.
    pub with_cached_data: usize,
    /// Pre-cooked blobs.
    pub cached_blobs: usize,
    /// Bytes of pre-cooked blobs.
    pub cached_bytes: u64,
    /// Scalar tag name → exports carrying it.
    pub scalar_tags: BTreeMap<String, usize>,
    /// First decode failures (`export: error`).
    pub failures: Vec<String>,
    /// First validation issues (`export: issue`).
    pub issues: Vec<String>,
}

/// Decode every `RB_BodySetup` export of `lp` and gather statistics.
pub fn body_setup_coverage(lp: &LoadedPackage) -> BodySetupCoverage {
    let pkg = &lp.package;
    let mut cov = BodySetupCoverage {
        package: lp.name.clone(),
        ..BodySetupCoverage::default()
    };
    let mut index = None;
    for i in 0..pkg.exports.len() {
        if !is_body_setup(pkg, i) {
            continue;
        }
        cov.total += 1;
        let b = match decode_body_setup(pkg, i) {
            Ok(b) => b,
            Err(e) => {
                if cov.failures.len() < MAX_FAILURE_SAMPLES {
                    cov.failures.push(format!("{i}: {e}"));
                }
                continue;
            }
        };
        cov.exact += 1;
        let data = pkg.export_data(i).unwrap_or(&[]);
        cov.payload_bytes += u64::try_from(data.len()).unwrap_or(0);
        let names = index.get_or_insert_with(|| NameIndex::new(pkg));
        if encode_body_setup(&b, names).as_deref() == Some(data) {
            cov.round_trip += 1;
        }
        let issues = validate_body_setup(&b);
        if issues.is_empty() {
            cov.valid += 1;
        } else {
            for m in issues {
                if cov.issues.len() < MAX_FAILURE_SAMPLES {
                    cov.issues.push(format!("{i}: {m}"));
                }
            }
        }
        let owner = pkg
            .export(i)
            .ok()
            .and_then(|e| e.outer_index.export_index())
            .and_then(|o| pkg.export_class_name(o).ok())
            .unwrap_or_else(|| "<none>".to_owned());
        *cov.owners.entry(owner).or_insert(0) += 1;
        if let Some(g) = b.agg_geom() {
            cov.with_agg_geom += 1;
            if g.element_count() == 0 {
                cov.empty_agg_geom += 1;
            }
            cov.convex += g.convex().len();
            cov.boxes += g.boxes().len();
            cov.spheres += g.spheres().len();
            cov.sphyls += g.sphyls().len();
            for c in g.convex() {
                let len = |n: Option<usize>| u64::try_from(n.unwrap_or(0)).unwrap_or(0);
                cov.convex_vertices += len(c.vertex_data.as_ref().map(Vec::len));
                cov.convex_planes += len(c.face_plane_data.as_ref().map(Vec::len));
            }
        }
        if !b.cached_data().is_empty() {
            cov.with_cached_data += 1;
        }
        cov.cached_blobs += b
            .cached_data()
            .iter()
            .map(|d| d.elements.len())
            .sum::<usize>();
        cov.cached_bytes += b.cached_bytes();
        for s in b.scalars() {
            *cov.scalar_tags.entry(s.name.clone()).or_insert(0) += 1;
        }
    }
    cov
}
