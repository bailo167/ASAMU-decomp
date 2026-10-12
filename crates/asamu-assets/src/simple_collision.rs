//! Reader for the simple-collision shapes of a mesh (`<Name>.collision.json`).
//!
//! `asamu-import meshes` writes one such document beside the glTF files of
//! every mesh that has a body setup (mesh manifest version 2; the manifest
//! entry's [`crate::manifest::SimpleCollisionEntry::shapes`] names it). The
//! format is described in `docs/reverse-engineering/MESHES.md`, "Simple
//! collision"; the writer is `tools/asamu-import/src/meshes.rs`.
//!
//! Every point and direction is in the mesh's local space, in UE3 axes and
//! Unreal units, exactly as the original stores it: unlike the glTF files the
//! shapes are neither converted to glTF axes nor scaled.
//!
//! The document is untrusted input like every converted file: the format and
//! version are checked, every float must be finite, every triangle index must
//! name a vertex, sizes must not be negative and every transform must be
//! affine. Nothing here panics. What the reader does **not** promise is that a
//! convex element is convex: 584 of the original's 3,446 elements are not
//! (MESHES.md), and a consumer must use each list the way the original's
//! check uses it.

use std::path::Path;

use serde::Deserialize;

use crate::error::{AssetError, AssetResult};

/// `format` of a shapes document.
pub const SIMPLE_COLLISION_FORMAT: &str = "asamu-simple-collision";
/// `version` this reader understands.
pub const SIMPLE_COLLISION_VERSION: u32 = 1;
/// Largest shapes document accepted. The largest of the shipped game's 316
/// is well under one megabyte.
pub const MAX_SIMPLE_COLLISION_BYTES: u64 = 64 * 1024 * 1024;

/// A row-vector 4×4 matrix as stored: rows are the X axis, the Y axis, the Z
/// axis and the origin, each `[x, y, z, w]` (a local point `p` is at
/// `p.x * row0 + p.y * row1 + p.z * row2 + row3`).
pub type ShapeMatrix = [[f32; 4]; 4];

/// One convex element (`KConvexElem`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConvexShape {
    /// `VertexData`: the points whose range the original's swept-box check
    /// projects on every axis.
    pub vertices: Vec<[f32; 3]>,
    /// `FacePlaneData` as `[nx, ny, nz, d]`: the plane `n . p = d`, normal
    /// pointing out of the element.
    #[serde(default)]
    pub planes: Vec<[f32; 4]>,
    /// `FaceTriData`: the surface as triangles of vertex indices.
    #[serde(default)]
    pub face_triangles: Vec<[u32; 3]>,
    /// `EdgeDirections` (unit vectors; the original crosses each with the
    /// swept box's three edges).
    #[serde(default)]
    pub edge_directions: Vec<[f32; 3]>,
    /// `FaceNormalDirections` (unit vectors; one per face direction).
    #[serde(default)]
    pub face_normal_directions: Vec<[f32; 3]>,
    /// `ElemBox.Min`: the vertices' bounding box.
    pub bounds_min: [f32; 3],
    /// `ElemBox.Max`.
    pub bounds_max: [f32; 3],
}

/// One box (`KBoxElem`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BoxShape {
    /// `TM`.
    pub transform: ShapeMatrix,
    /// `X`, `Y`, `Z`: full edge lengths along the local axes.
    pub size: [f32; 3],
}

/// One sphere (`KSphereElem`), centred on the fourth row of its transform.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SphereShape {
    /// `TM`.
    pub transform: ShapeMatrix,
    /// `Radius`.
    pub radius: f32,
}

/// One capsule (`KSphylElem`): a segment of `length` along the local Z axis,
/// centred on the origin, swept by a sphere of `radius`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SphylShape {
    /// `TM`.
    pub transform: ShapeMatrix,
    /// `Radius`.
    pub radius: f32,
    /// `Length` of the segment.
    pub length: f32,
}

#[derive(Debug, Deserialize)]
struct RawDoc {
    #[serde(default)]
    format: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    mesh: String,
    #[serde(default)]
    body_setup: String,
    #[serde(default)]
    convex: Vec<ConvexShape>,
    #[serde(default)]
    boxes: Vec<BoxShape>,
    #[serde(default)]
    spheres: Vec<SphereShape>,
    #[serde(default)]
    sphyls: Vec<SphylShape>,
}

/// The simple-collision shapes of one mesh's body setup.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SimpleCollision {
    /// Object path of the mesh.
    pub mesh: String,
    /// Object path of the body setup.
    pub body_setup: String,
    /// Convex elements, in the order the original tests them.
    pub convex: Vec<ConvexShape>,
    /// Boxes.
    pub boxes: Vec<BoxShape>,
    /// Spheres.
    pub spheres: Vec<SphereShape>,
    /// Capsules.
    pub sphyls: Vec<SphylShape>,
}

fn finite3(v: &[f32; 3]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Finite, with the fourth column `(0, 0, 0, 1)` (an affine row-vector
/// matrix).
fn affine(m: &ShapeMatrix) -> bool {
    m.iter().flatten().all(|x| x.is_finite())
        && m[0][3] == 0.0
        && m[1][3] == 0.0
        && m[2][3] == 0.0
        && m[3][3] == 1.0
}

impl SimpleCollision {
    /// Parses and checks a shapes document read from `path`.
    ///
    /// # Errors
    /// Malformed JSON, another format or version, a non-finite number, a
    /// triangle index outside its element's vertices, a negative size or a
    /// transform that is not affine.
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawDoc = crate::files::parse_json(path, data)?;
        let bad = |found: String| AssetError::Format {
            path: path.to_path_buf(),
            expected: format!("{SIMPLE_COLLISION_FORMAT} version {SIMPLE_COLLISION_VERSION}"),
            found,
        };
        if raw.format != SIMPLE_COLLISION_FORMAT || raw.version != SIMPLE_COLLISION_VERSION {
            return Err(bad(format!("{:?} version {}", raw.format, raw.version)));
        }
        for (i, c) in raw.convex.iter().enumerate() {
            let floats_ok = c.vertices.iter().all(finite3)
                && c.planes.iter().all(|p| p.iter().all(|x| x.is_finite()))
                && c.edge_directions.iter().all(finite3)
                && c.face_normal_directions.iter().all(finite3)
                && finite3(&c.bounds_min)
                && finite3(&c.bounds_max);
            if !floats_ok {
                return Err(bad(format!("convex element {i} with a non-finite number")));
            }
            let n = c.vertices.len();
            if c.face_triangles
                .iter()
                .flatten()
                .any(|&v| usize::try_from(v).map_or(true, |v| v >= n))
            {
                return Err(bad(format!(
                    "convex element {i} with a triangle index outside its {n} vertices"
                )));
            }
        }
        for (i, b) in raw.boxes.iter().enumerate() {
            if !(affine(&b.transform) && finite3(&b.size) && b.size.iter().all(|x| *x >= 0.0)) {
                return Err(bad(format!("box {i} with an unusable transform or size")));
            }
        }
        for (i, s) in raw.spheres.iter().enumerate() {
            if !(affine(&s.transform) && s.radius.is_finite() && s.radius >= 0.0) {
                return Err(bad(format!(
                    "sphere {i} with an unusable transform or radius"
                )));
            }
        }
        for (i, s) in raw.sphyls.iter().enumerate() {
            let sizes_ok =
                s.radius.is_finite() && s.radius >= 0.0 && s.length.is_finite() && s.length >= 0.0;
            if !(affine(&s.transform) && sizes_ok) {
                return Err(bad(format!(
                    "capsule {i} with an unusable transform or size"
                )));
            }
        }
        Ok(Self {
            mesh: raw.mesh,
            body_setup: raw.body_setup,
            convex: raw.convex,
            boxes: raw.boxes,
            spheres: raw.spheres,
            sphyls: raw.sphyls,
        })
    }

    /// How many of the shapes a swept (non-zero-extent) trace can hit: the
    /// convex elements and the boxes. The original's sphere and capsule
    /// checks return "no hit" at once for a non-zero extent
    /// (`FKSphereElem::LineCheck` and `FKSphylElem::LineCheck`, MESHES.md).
    #[must_use]
    pub fn swept_shapes(&self) -> usize {
        self.convex.len().saturating_add(self.boxes.len())
    }

    /// `true` without any shape.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.convex.is_empty()
            && self.boxes.is_empty()
            && self.spheres.is_empty()
            && self.sphyls.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document written by hand: one tetrahedron, one box, one sphere and
    /// one capsule.
    fn doc() -> serde_json::Value {
        serde_json::json!({
            "format": "asamu-simple-collision",
            "version": 1,
            "notice": "synthetic",
            "space": "mesh local space",
            "mesh": "Pkg.Meshes.Thing",
            "body_setup": "Pkg.Meshes.Thing.RB_BodySetup_0",
            "body_properties": {"PreCachedPhysDataVersion": 1},
            "convex": [{
                "vertices": [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
                "planes": [[0.0, 0.0, -1.0, 0.0]],
                "face_triangles": [[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
                "edge_directions": [[1.0, 0.0, 0.0]],
                "face_normal_directions": [[0.0, 0.0, 1.0]],
                "bounds_min": [0.0, 0.0, 0.0],
                "bounds_max": [10.0, 10.0, 10.0]
            }],
            "boxes": [{
                "transform": [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [5.0, 6.0, 7.0, 1.0]],
                "size": [2.0, 4.0, 6.0],
                "no_rb_collision": false,
                "per_poly_shape": false
            }],
            "spheres": [{
                "transform": [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
                "radius": 160.0,
                "no_rb_collision": false,
                "per_poly_shape": false
            }],
            "sphyls": [{
                "transform": [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
                "radius": 3.0,
                "length": 8.0,
                "no_rb_collision": false,
                "per_poly_shape": false
            }]
        })
    }

    fn parse(v: &serde_json::Value) -> AssetResult<SimpleCollision> {
        SimpleCollision::from_json(Path::new("x.collision.json"), v.to_string().as_bytes())
    }

    #[test]
    fn a_document_reads_back_field_by_field() {
        let s = parse(&doc()).unwrap();
        assert_eq!(s.mesh, "Pkg.Meshes.Thing");
        assert_eq!(s.body_setup, "Pkg.Meshes.Thing.RB_BodySetup_0");
        assert_eq!(
            (
                s.convex.len(),
                s.boxes.len(),
                s.spheres.len(),
                s.sphyls.len()
            ),
            (1, 1, 1, 1)
        );
        let c = &s.convex[0];
        assert_eq!(c.vertices[3], [0.0, 0.0, 10.0]);
        assert_eq!(c.planes, vec![[0.0, 0.0, -1.0, 0.0]]);
        assert_eq!(c.face_triangles[3], [1, 2, 3]);
        assert_eq!(c.edge_directions, vec![[1.0, 0.0, 0.0]]);
        assert_eq!(c.face_normal_directions, vec![[0.0, 0.0, 1.0]]);
        assert_eq!((c.bounds_min, c.bounds_max), ([0.0; 3], [10.0; 3]));
        assert_eq!(s.boxes[0].size, [2.0, 4.0, 6.0]);
        assert_eq!(s.boxes[0].transform[3], [5.0, 6.0, 7.0, 1.0]);
        assert_eq!(s.spheres[0].radius, 160.0);
        assert_eq!((s.sphyls[0].radius, s.sphyls[0].length), (3.0, 8.0));
        // Spheres and capsules never stop a swept trace.
        assert_eq!(s.swept_shapes(), 2);
        assert!(!s.is_empty());
    }

    #[test]
    fn missing_lists_are_empty_and_unknown_fields_are_ignored() {
        let s = parse(&serde_json::json!({
            "format": "asamu-simple-collision", "version": 1, "later_addition": [1, 2, 3]
        }))
        .unwrap();
        assert!(s.is_empty());
        assert_eq!(s.swept_shapes(), 0);
    }

    #[test]
    fn wrong_documents_are_refused() {
        let reject = |edit: &dyn Fn(&mut serde_json::Value), what: &str| {
            let mut v = doc();
            edit(&mut v);
            assert!(parse(&v).is_err(), "{what} accepted");
        };
        reject(&|v| v["format"] = "asamu-scene".into(), "another format");
        reject(&|v| v["version"] = 2.into(), "a later version");
        reject(
            &|v| v["convex"][0]["face_triangles"][0][1] = 4.into(),
            "an index past the vertices",
        );
        reject(
            &|v| v["convex"][0]["face_triangles"][0][1] = (-1).into(),
            "a negative index",
        );
        // 1e39 is a valid JSON number that does not fit an f32.
        reject(
            &|v| v["convex"][0]["vertices"][2][0] = 1.0e39.into(),
            "a vertex beyond f32",
        );
        reject(
            &|v| v["convex"][0]["edge_directions"][0][2] = 1.0e39.into(),
            "a direction beyond f32",
        );
        reject(
            &|v| v["convex"][0]["bounds_max"][0] = (-1.0e39).into(),
            "a bound beyond f32",
        );
        reject(
            &|v| v["boxes"][0]["size"][1] = (-4.0).into(),
            "a negative size",
        );
        reject(
            &|v| v["boxes"][0]["transform"][3][3] = 0.0.into(),
            "a projective matrix",
        );
        reject(
            &|v| v["spheres"][0]["radius"] = (-1.0).into(),
            "a negative radius",
        );
        reject(
            &|v| v["sphyls"][0]["length"] = 1.0e39.into(),
            "a length beyond f32",
        );
        reject(&|v| v["convex"][0]["vertices"] = 5.into(), "a wrong shape");
        reject(
            &|v| {
                v["convex"][0]
                    .as_object_mut()
                    .map(|o| o.remove("bounds_min"));
            },
            "a missing bound",
        );
        assert!(
            SimpleCollision::from_json(Path::new("x"), b"{\"format\": ").is_err(),
            "truncated JSON accepted"
        );
        assert!(SimpleCollision::from_json(Path::new("x"), b"[]").is_err());
    }
}
