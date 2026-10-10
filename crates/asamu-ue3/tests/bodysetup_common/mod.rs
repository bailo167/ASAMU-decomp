//! A hand-written `RB_BodySetup` payload for the body setup tests. Every byte
//! is written here by a writer of our own (a second implementation of the
//! layout, independent of the library's encoder); nothing comes from the
//! original game.
//!
//! The body: a few scalar tags, two pre-cooked scales, and an aggregate with
//! one sphere, one box, one capsule and one convex element (a square pyramid
//! with five vertices, so the permuted vertex data has a padded group).

#![allow(dead_code)]

/// Name table of the fixture.
pub const NAMES: &[&str] = &[
    "None",
    "BoneName",
    "NameProperty",
    "Pelvis",
    "bNoCollision",
    "BoolProperty",
    "PhysMaterial",
    "ObjectProperty",
    "MassScale",
    "FloatProperty",
    "PreCachedPhysScale",
    "ArrayProperty",
    "PreCachedPhysDataVersion",
    "IntProperty",
    "AggGeom",
    "StructProperty",
    "KAggregateGeom",
    "SphereElems",
    "TM",
    "Matrix",
    "Radius",
    "bNoRBCollision",
    "bPerPolyShape",
    "BoxElems",
    "X",
    "Y",
    "Z",
    "SphylElems",
    "Length",
    "ConvexElems",
    "VertexData",
    "PermutedVertexData",
    "FaceTriData",
    "EdgeDirections",
    "FaceNormalDirections",
    "FacePlaneData",
    "ElemBox",
    "Box",
    "SleepFamily",
    "ByteProperty",
    "ESleepFamily",
    "SF_Sensitive",
    "COMNudge",
    "Vector",
    "bSkipCloseAndParallelChecks",
    "Padding",
];

/// The fixture's name table as owned strings.
pub fn names() -> Vec<String> {
    NAMES.iter().map(|s| (*s).to_owned()).collect()
}

/// Index of `s` in [`NAMES`].
pub fn n(s: &str) -> i32 {
    NAMES
        .iter()
        .position(|x| *x == s)
        .unwrap_or_else(|| panic!("{s} is not a fixture name")) as i32
}

/// Extra part of a tag header.
pub enum E<'a> {
    /// Nothing.
    No,
    /// Struct name.
    Struct(&'a str),
    /// Boolean value byte.
    Bool(u8),
    /// Enum name.
    Enum(&'a str),
}

/// Byte buffer with the positions of the fields the hostile tests patch.
#[derive(Default, Clone)]
pub struct B {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// Offsets of every element count (`i32`).
    pub counts: Vec<usize>,
    /// Offsets of every tag size (`i32`).
    pub sizes: Vec<usize>,
    /// Tag name and offset of the tag's first byte, in stream order.
    pub tags: Vec<(String, usize)>,
}

impl B {
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.bytes.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.bytes.push(v);
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn floats(&mut self, v: &[f32]) -> &mut Self {
        for x in v {
            self.f32(*x);
        }
        self
    }
    /// FName with instance number 0.
    pub fn name(&mut self, s: &str) -> &mut Self {
        self.name_num(s, 0)
    }
    pub fn name_num(&mut self, s: &str, number: i32) -> &mut Self {
        self.i32(n(s));
        self.i32(number)
    }
    /// An element count (remembered in `counts`).
    pub fn count(&mut self, v: i32) -> &mut Self {
        self.counts.push(self.bytes.len());
        self.i32(v)
    }
    /// Append `child`, shifting its remembered positions.
    pub fn append(&mut self, child: &B) -> &mut Self {
        let base = self.bytes.len();
        self.counts.extend(child.counts.iter().map(|o| o + base));
        self.sizes.extend(child.sizes.iter().map(|o| o + base));
        self.tags
            .extend(child.tags.iter().map(|(t, o)| (t.clone(), o + base)));
        self.bytes.extend_from_slice(&child.bytes);
        self
    }
    /// One tag: name, type, size, array index 0, the extra part and the value.
    pub fn tag(&mut self, name: &str, ty: &str, extra: E<'_>, value: &B) -> &mut Self {
        self.tags.push((name.to_owned(), self.bytes.len()));
        self.name(name);
        self.name(ty);
        self.sizes.push(self.bytes.len());
        self.i32(value.bytes.len() as i32);
        self.i32(0);
        match extra {
            E::No => {}
            E::Struct(s) => {
                self.name(s);
            }
            E::Bool(v) => {
                self.u8(v);
            }
            E::Enum(s) => {
                self.name(s);
            }
        }
        self.append(value)
    }
    pub fn float_tag(&mut self, name: &str, v: f32) -> &mut Self {
        let mut x = B::default();
        x.f32(v);
        self.tag(name, "FloatProperty", E::No, &x)
    }
    pub fn bool_tag(&mut self, name: &str, v: u8) -> &mut Self {
        self.tag(name, "BoolProperty", E::Bool(v), &B::default())
    }
    /// A `TM` tag. `rows` are the matrix rows in X, Y, Z, W order; each is
    /// stored W first.
    pub fn tm_tag(&mut self, rows: &[[f32; 4]; 4]) -> &mut Self {
        let mut x = B::default();
        for r in rows {
            x.floats(&[r[3], r[0], r[1], r[2]]);
        }
        self.tag("TM", "StructProperty", E::Struct("Matrix"), &x)
    }
    /// An array tag of fixed-size float items.
    pub fn float_array_tag(&mut self, name: &str, count: i32, floats: &[f32]) -> &mut Self {
        let mut x = B::default();
        x.count(count).floats(floats);
        self.tag(name, "ArrayProperty", E::No, &x)
    }
    /// Offset of the `k`-th tag called `name`.
    pub fn tag_at(&self, name: &str, k: usize) -> usize {
        self.tags
            .iter()
            .filter(|(t, _)| t == name)
            .nth(k)
            .unwrap_or_else(|| panic!("no tag {name} #{k}"))
            .1
    }
}

pub const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];
/// Translation (10, 20, 30).
pub const SPHERE_TM: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [10.0, 20.0, 30.0, 1.0],
];
/// A quarter turn about Z, then translation (1, 2, 3).
pub const BOX_TM: [[f32; 4]; 4] = [
    [0.0, 1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [1.0, 2.0, 3.0, 1.0],
];

/// The pyramid: a square base at z = 0.5 and an apex at z = 2.5.
pub const PYRAMID: [[f32; 3]; 5] = [
    [-1.0, -1.0, 0.5],
    [1.0, -1.0, 0.5],
    [1.0, 1.0, 0.5],
    [-1.0, 1.0, 0.5],
    [0.0, 0.0, 2.5],
];
/// Its planes as (normal x, y, z, distance): the base, then the +x, +y, -x
/// and -y sides (normal (2, 0, 1) / sqrt 5 and its rotations; distance
/// 2.5 / sqrt 5).
pub const S: f32 = 0.894_427_2;
pub const C: f32 = 0.447_213_6;
pub const D: f32 = 1.118_034;
pub const PYRAMID_PLANES: [[f32; 4]; 5] = [
    [0.0, 0.0, -1.0, -0.5],
    [S, 0.0, C, D],
    [0.0, S, C, D],
    [-S, 0.0, C, D],
    [0.0, -S, C, D],
];
/// Surface triangles, clockwise seen from outside.
pub const PYRAMID_TRIS: [i32; 18] = [0, 1, 2, 0, 2, 3, 1, 4, 2, 2, 4, 3, 3, 4, 0, 0, 4, 1];
/// Two base edge directions.
pub const PYRAMID_EDGES: [[f32; 3]; 2] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
/// The pre-cooked blob of the first scale.
pub const BLOB: [u8; 3] = [0xAA, 0xBB, 0xCC];

fn convex_elem() -> B {
    let mut e = B::default();
    e.float_array_tag("VertexData", 5, &PYRAMID.concat());
    // Two groups of four: the second repeats its first vertex (the apex).
    e.float_array_tag(
        "PermutedVertexData",
        6,
        // Each plane W first: (x3 | x0 x1 x2), (y3 | y0 y1 y2), (z3 | z0 z1 z2).
        &[
            -1.0, -1.0, 1.0, 1.0, //
            1.0, -1.0, -1.0, 1.0, //
            0.5, 0.5, 0.5, 0.5, //
            0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, //
            2.5, 2.5, 2.5, 2.5,
        ],
    );
    let mut tris = B::default();
    tris.count(18);
    for i in PYRAMID_TRIS {
        tris.i32(i);
    }
    e.tag("FaceTriData", "ArrayProperty", E::No, &tris);
    e.float_array_tag("EdgeDirections", 2, &PYRAMID_EDGES.concat());
    let normals: Vec<f32> = PYRAMID_PLANES
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    e.float_array_tag("FaceNormalDirections", 5, &normals);
    // Planes W first.
    let planes: Vec<f32> = PYRAMID_PLANES
        .iter()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    e.float_array_tag("FacePlaneData", 5, &planes);
    let mut bx = B::default();
    bx.floats(&[-1.0, -1.0, 0.5, 1.0, 1.0, 2.5]).u8(1);
    e.tag("ElemBox", "StructProperty", E::Struct("Box"), &bx);
    e.name("None");
    e
}

fn agg_geom() -> B {
    let mut g = B::default();

    let mut spheres = B::default();
    spheres.count(1);
    spheres
        .tm_tag(&SPHERE_TM)
        .float_tag("Radius", 4.0)
        .bool_tag("bNoRBCollision", 0)
        .bool_tag("bPerPolyShape", 1)
        .name("None");
    g.tag("SphereElems", "ArrayProperty", E::No, &spheres);

    let mut boxes = B::default();
    boxes.count(1);
    boxes
        .tm_tag(&BOX_TM)
        .float_tag("X", 2.0)
        .float_tag("Y", 4.0)
        .float_tag("Z", 6.0)
        .bool_tag("bNoRBCollision", 1)
        .bool_tag("bPerPolyShape", 0)
        .name("None");
    g.tag("BoxElems", "ArrayProperty", E::No, &boxes);

    let mut sphyls = B::default();
    sphyls.count(1);
    sphyls
        .tm_tag(&IDENTITY)
        .float_tag("Radius", 1.5)
        .float_tag("Length", 8.0)
        .bool_tag("bNoRBCollision", 0)
        .bool_tag("bPerPolyShape", 0)
        .name("None");
    g.tag("SphylElems", "ArrayProperty", E::No, &sphyls);

    let mut convex = B::default();
    convex.count(1);
    convex.append(&convex_elem());
    g.tag("ConvexElems", "ArrayProperty", E::No, &convex);

    g.bool_tag("bSkipCloseAndParallelChecks", 1);
    g.name("None");
    g
}

/// The tagged properties of the fixture, up to and including the final
/// `None` (no `NetIndex`, no native data).
pub fn tagged() -> B {
    let mut b = B::default();
    let mut v = B::default();
    v.name("SF_Sensitive");
    b.tag("SleepFamily", "ByteProperty", E::Enum("ESleepFamily"), &v);
    let mut v = B::default();
    v.name_num("Pelvis", 3);
    b.tag("BoneName", "NameProperty", E::No, &v);
    b.bool_tag("bNoCollision", 1);
    let mut v = B::default();
    v.i32(-5);
    b.tag("PhysMaterial", "ObjectProperty", E::No, &v);
    b.float_tag("MassScale", 2.5);
    b.float_array_tag("PreCachedPhysScale", 2, &[1.0, 1.0, 1.0, 2.0, 2.0, 0.5]);
    let mut v = B::default();
    v.i32(12345);
    b.tag("PreCachedPhysDataVersion", "IntProperty", E::No, &v);
    let mut v = B::default();
    v.floats(&[0.25, -0.5, 1.0]);
    b.tag("COMNudge", "StructProperty", E::Struct("Vector"), &v);
    b.tag(
        "AggGeom",
        "StructProperty",
        E::Struct("KAggregateGeom"),
        &agg_geom(),
    );
    b.name("None");
    b
}

/// The whole payload: `NetIndex` 7, the tagged properties, and pre-cooked
/// data for the two scales (a three-byte blob, then an empty one).
pub fn fixture() -> B {
    let mut b = B::default();
    b.i32(7);
    b.append(&tagged());
    b.count(2);
    b.count(1).i32(1).count(3);
    b.bytes.extend_from_slice(&BLOB);
    b.count(1).i32(1).count(0);
    b
}

/// A class default object's payload: `NetIndex`, two scalar tags, no native
/// data.
pub fn class_default_object() -> B {
    let mut b = B::default();
    b.i32(-1);
    b.bool_tag("bNoCollision", 0);
    b.float_tag("MassScale", 1.0);
    b.name("None");
    b
}
