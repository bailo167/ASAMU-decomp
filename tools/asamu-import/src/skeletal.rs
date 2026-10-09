//! `asamu-import skeletal`: SkeletalMesh + AnimSet/AnimSequence → glTF 2.0
//! with a skin (joints, inverse bind matrices) and one animation per
//! sequence, plus a JSON manifest.
//!
//! Everything written is derived from the user's own copy of the game: it goes
//! to the user-local output directory only (never the repository, except a
//! git-ignored `research/` subfolder, and never the install), and must not be
//! redistributed. `--check` writes nothing: it decodes, converts and validates
//! every mesh in memory and prints a report.
//!
//! The native layouts decoded here are documented in
//! `docs/reverse-engineering/SKELETAL.md`.
//!
//! Output layout under `<out>/skeletal/`:
//!
//! ```text
//! manifest.json                       object path -> files, skeleton, anim sets, ...
//! <Package>/<Path...>/<Name>.gltf     LOD 0 + skin + animations
//! <Package>/<Path...>/<Name>.bin      binary buffer of the .gltf next to it
//! <Package>/<Path...>/<Name>_LOD<n>.gltf/.bin   coarser LODs (--all-lods; skin only)
//! ```
//!
//! # Conversion
//!
//! - **Axes and scale** as in `meshes`: UE3 `(x, y, z)` → glTF `(y, z, -x)`
//!   (determinant -1), times `--scale`. A rotation `(x, y, z, w)` becomes
//!   `(-y, -z, x, w)` (the mirror conjugates the axis map onto the rotation).
//! - **Node tree.** A root node carries the mesh-to-component transform the
//!   engine applies (`v → RotOrigin · (v + Origin)`); under it the bone nodes
//!   (reference pose, local TRS), sockets as children of their bones, and the
//!   skinned mesh node.
//! - **Skin.** One joint per reference-skeleton bone (same order); inverse
//!   bind matrices from the composed reference pose. Vertices carry
//!   `JOINTS_0` (chunk bone map → skeleton bone) and `WEIGHTS_0` (the stored
//!   bytes, normalised; every shipped vertex sums to 255).
//! - **Geometry** as in `meshes` (normals from `TangentZ`, tangents from
//!   `TangentX` with `w = -sign(TangentZ.W)`, every UV channel, winding kept).
//! - **Animations.** Every AnimSet paired with the mesh (by the
//!   SkeletalMeshComponents that use both, else `PreviewSkelMeshName`, else
//!   the best bone-name match) contributes one animation per sequence, named
//!   `<AnimSet>/<SequenceName>`. Tracks map to bones by name. Rotations use
//!   the engine's pose rule (W negated on every bone but the root); a bone
//!   gets a translation channel only when the engine uses the animation's
//!   translation (root, or `bAnimRotationOnly` off / `UseTranslationBoneNames`,
//!   and not `ForceMeshTranslationBoneNames`). Keys are written at their
//!   stored times (engine's non-looping mapping) with LINEAR interpolation;
//!   keys that share a time (repeated final frames) are split so the engine's
//!   frame-table search is reproduced ([`sampler_keys`]). `--resample` instead
//!   evaluates every frame with the engine's interpolation. glTF blends
//!   rotations by slerp where the engine uses nlerp: identical on keys,
//!   slightly different between them. `RateScale`, looping flags and
//!   notifies go to the manifest.
//! - **Manifest.** An existing manifest's entries and file names are kept, so
//!   a filtered run updates only its own meshes. A document that fails
//!   [`validate_gltf`] is never written.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::PackageSet;
use asamu_ue3::anim::{
    AnimSequence, AnimSetInfo, CompressedTrack, bone_to_track, decode_anim_sequence,
    decode_anim_set, is_anim_sequence, is_anim_set, pose_rotation, sample_rotation,
    sample_translation,
};
use asamu_ue3::model::LoadedPackage;
use asamu_ue3::skeletal::{
    SkelLodModel, SkeletalMesh, ValidationContext, bone_names, decode_skeletal_mesh,
    is_skeletal_mesh, validate_skeletal_mesh,
};
use asamu_ue3::{PackageIndex, Property, Value, decode_object};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::safety;

/// Manifest format version.
const MANIFEST_VERSION: u32 = 1;

/// Notice stored in every manifest and glTF file.
const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
                      Copyrighted game data: keep it local, never redistribute.";

/// Coordinate system note stored in the manifest.
const COORDINATES: &str = "glTF 2.0: right-handed, +Y up, -Z forward; UE3 (x, y, z) -> (y, z, -x) * scale; \
     rotation (x, y, z, w) -> (-y, -z, x, w)";

/// glTF component types.
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const UNSIGNED_BYTE: u32 = 5121;
/// glTF buffer-view targets.
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
/// Unit-length tolerance for normals, tangents and rotations.
const UNIT_TOLERANCE: f32 = 1e-3;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only meshes from packages whose file name contains this text
    /// (case-insensitive; repeatable). AnimSets and pairings are still read
    /// from every package.
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Only meshes whose object path contains this text (case-insensitive).
    #[arg(long)]
    name: Option<String>,
    /// Stop after converting this many meshes.
    #[arg(long)]
    limit: Option<usize>,
    /// Also write LODs 1.. (`<Name>_LOD<n>.gltf`, skin only).
    #[arg(long)]
    all_lods: bool,
    /// Evaluate every track at each frame with the engine's interpolation
    /// instead of writing the stored keys.
    #[arg(long)]
    resample: bool,
    /// Do not write animations.
    #[arg(long)]
    no_animations: bool,
    /// glTF units per Unreal unit (1 keeps UU).
    #[arg(long, default_value_t = 1.0)]
    scale: f32,
    /// Overwrite existing files (otherwise they are kept).
    #[arg(long)]
    force: bool,
    /// Decode, convert and validate every mesh in memory; write nothing.
    #[arg(long)]
    check: bool,
}

// ---------------------------------------------------------------------------
// Math (f64; glTF output in f32)
// ---------------------------------------------------------------------------

type V3 = [f64; 3];
type Q = [f64; 4];

/// UE3 axes → glTF axes: `(x, y, z) → (y, z, -x)`.
pub fn ue_to_gltf(v: [f32; 3]) -> [f32; 3] {
    [v[1], v[2], -v[0]]
}

/// UE3 rotation `(x, y, z, w)` → glTF rotation for the axis map above.
pub fn ue_quat_to_gltf(q: [f32; 4]) -> [f32; 4] {
    [-q[1], -q[2], q[0], q[3]]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}

fn normalize(a: V3) -> Option<V3> {
    let l = len(a);
    (l > 1e-12 && l.is_finite()).then(|| a.map(|c| c / l))
}

fn qnormalize(q: Q) -> Q {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l > 1e-12 && l.is_finite() {
        q.map(|c| c / l)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn qmul(a: Q, b: Q) -> Q {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

fn qconj(q: Q) -> Q {
    [-q[0], -q[1], -q[2], q[3]]
}

fn rotate(q: Q, v: V3) -> V3 {
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c2 = cross(u, t);
    std::array::from_fn(|k| v[k] + q[3] * t[k] + c2[k])
}

fn f64v(v: [f32; 3]) -> V3 {
    v.map(f64::from)
}

fn f64q(q: [f32; 4]) -> Q {
    q.map(f64::from)
}

/// A rigid transform `v → r·v + t`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rigid {
    t: V3,
    r: Q,
}

impl Rigid {
    const IDENTITY: Rigid = Rigid {
        t: [0.0; 3],
        r: [0.0, 0.0, 0.0, 1.0],
    };

    fn then_child(&self, child: &Rigid) -> Rigid {
        let rt = rotate(self.r, child.t);
        Rigid {
            t: [self.t[0] + rt[0], self.t[1] + rt[1], self.t[2] + rt[2]],
            r: qnormalize(qmul(self.r, child.r)),
        }
    }

    fn inverse(&self) -> Rigid {
        let ri = qconj(self.r);
        let t = rotate(ri, self.t);
        Rigid {
            t: [-t[0], -t[1], -t[2]],
            r: ri,
        }
    }

    /// Column-major 4x4 matrix (glTF layout).
    fn to_matrix(self) -> [f64; 16] {
        let [x, y, z, w] = self.r;
        let (x2, y2, z2) = (x + x, y + y, z + z);
        let (xx, xy, xz) = (x * x2, x * y2, x * z2);
        let (yy, yz, zz) = (y * y2, y * z2, z * z2);
        let (wx, wy, wz) = (w * x2, w * y2, w * z2);
        [
            1.0 - (yy + zz),
            xy + wz,
            xz - wy,
            0.0,
            xy - wz,
            1.0 - (xx + zz),
            yz + wx,
            0.0,
            xz + wy,
            yz - wx,
            1.0 - (xx + yy),
            0.0,
            self.t[0],
            self.t[1],
            self.t[2],
            1.0,
        ]
    }
}

/// Unit quaternion of a proper rotation matrix given as rows `m[row][col]`
/// (column-vector convention: `v' = m · v`).
fn quat_from_rows(m: [[f64; 3]; 3]) -> Q {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    qnormalize(q)
}

/// UE3 `FRotator` (pitch, yaw, roll in 65536ths of a turn) → UE3 rotation
/// quaternion, through the engine's `FRotationMatrix` rows (row-vector
/// convention `v' = v · M`).
pub fn rotator_to_quat(pitch: i32, yaw: i32, roll: i32) -> [f64; 4] {
    let a = |u: i32| f64::from(u) * std::f64::consts::TAU / 65536.0;
    let (sp, cp) = a(pitch).sin_cos();
    let (sy, cy) = a(yaw).sin_cos();
    let (sr, cr) = a(roll).sin_cos();
    let rows = [
        [cp * cy, cp * sy, sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp],
        [-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp],
    ];
    // Column convention: R = M^T.
    let r = [
        [rows[0][0], rows[1][0], rows[2][0]],
        [rows[0][1], rows[1][1], rows[2][1]],
        [rows[0][2], rows[1][2], rows[2][2]],
    ];
    quat_from_rows(r)
}

/// UE3 rigid transform → glTF space (with `scale` on translations).
fn rigid_to_gltf(t: V3, r: Q, scale: f64) -> Rigid {
    Rigid {
        t: [t[1] * scale, t[2] * scale, -t[0] * scale],
        r: qnormalize([-r[1], -r[2], r[0], r[3]]),
    }
}

// ---------------------------------------------------------------------------
// Binary buffer
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Bin {
    bytes: Vec<u8>,
    views: Vec<Json>,
    accessors: Vec<Json>,
}

impl Bin {
    fn view(&mut self, data: &[u8], target: Option<u32>) -> usize {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        let offset = self.bytes.len();
        self.bytes.extend_from_slice(data);
        let mut v = json!({"buffer": 0, "byteOffset": offset, "byteLength": data.len()});
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.views.push(v);
        self.views.len() - 1
    }

    fn floats(
        &mut self,
        data: &[f32],
        ty: &str,
        comps: usize,
        target: Option<u32>,
        minmax: bool,
    ) -> usize {
        let mut b = Vec::with_capacity(data.len() * 4);
        for f in data {
            b.extend_from_slice(&f.to_le_bytes());
        }
        let view = self.view(&b, target);
        let count = data.len() / comps.max(1);
        let mut a = json!({"bufferView": view, "componentType": FLOAT, "count": count, "type": ty});
        if minmax {
            let mut lo = vec![f32::INFINITY; comps];
            let mut hi = vec![f32::NEG_INFINITY; comps];
            for c in data.chunks_exact(comps) {
                for k in 0..comps {
                    lo[k] = lo[k].min(c[k]);
                    hi[k] = hi[k].max(c[k]);
                }
            }
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn bytes_accessor(&mut self, data: &[u8], ty: &str, normalized: bool) -> usize {
        let view = self.view(data, Some(ARRAY_BUFFER));
        let comps = if ty == "VEC4" { 4 } else { 1 };
        let mut a = json!({"bufferView": view, "componentType": UNSIGNED_BYTE,
                           "count": data.len() / comps, "type": ty});
        if normalized {
            a["normalized"] = json!(true);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn u16_accessor(&mut self, data: &[u16], ty: &str, target: Option<u32>) -> usize {
        let mut b = Vec::with_capacity(data.len() * 2);
        for v in data {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let view = self.view(&b, target);
        let comps = if ty == "VEC4" { 4 } else { 1 };
        self.accessors
            .push(json!({"bufferView": view, "componentType": UNSIGNED_SHORT,
                                   "count": data.len() / comps, "type": ty}));
        self.accessors.len() - 1
    }

    fn u32_accessor(&mut self, data: &[u32]) -> usize {
        let mut b = Vec::with_capacity(data.len() * 4);
        for v in data {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let view = self.view(&b, Some(ELEMENT_ARRAY_BUFFER));
        self.accessors
            .push(json!({"bufferView": view, "componentType": UNSIGNED_INT,
                                   "count": data.len(), "type": "SCALAR"}));
        self.accessors.len() - 1
    }

    fn mat4_accessor(&mut self, mats: &[[f32; 16]]) -> usize {
        let mut b = Vec::with_capacity(mats.len() * 64);
        for m in mats {
            for f in m {
                b.extend_from_slice(&f.to_le_bytes());
            }
        }
        let view = self.view(&b, None);
        self.accessors
            .push(json!({"bufferView": view, "componentType": FLOAT,
                                   "count": mats.len(), "type": "MAT4"}));
        self.accessors.len() - 1
    }
}

// ---------------------------------------------------------------------------
// Inputs gathered from all packages
// ---------------------------------------------------------------------------

/// A socket (`SkeletalMeshSocket`) resolved from its tagged properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SocketEntry {
    /// `SocketName`.
    pub name: String,
    /// `BoneName`.
    pub bone: String,
    /// `RelativeLocation` (UE3).
    pub location: [f32; 3],
    /// `RelativeRotation` (pitch, yaw, roll; UE3 rotator units).
    pub rotation: [i32; 3],
    /// `RelativeScale`.
    pub scale: [f32; 3],
}

fn struct_fields(v: &Value) -> Option<&[Property]> {
    match v {
        Value::Struct { fields, .. } => Some(fields),
        _ => None,
    }
}

fn field<'a>(fields: &'a [Property], name: &str) -> Option<&'a Value> {
    fields
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

fn as_f32(v: Option<&Value>) -> Option<f32> {
    match v {
        Some(Value::Float(f)) => Some(*f),
        _ => None,
    }
}

fn as_i32(v: Option<&Value>) -> Option<i32> {
    match v {
        Some(Value::Int(i)) => Some(*i),
        _ => None,
    }
}

fn socket_from(props: &[Property]) -> Option<SocketEntry> {
    let name = match field(props, "SocketName") {
        Some(Value::Name(n)) => n.clone(),
        _ => return None,
    };
    let bone = match field(props, "BoneName") {
        Some(Value::Name(n)) => n.clone(),
        _ => return None,
    };
    let vec3 = |key: &str, default: f32| -> [f32; 3] {
        field(props, key)
            .and_then(struct_fields)
            .map_or([default; 3], |f| {
                [
                    as_f32(field(f, "X")).unwrap_or(default),
                    as_f32(field(f, "Y")).unwrap_or(default),
                    as_f32(field(f, "Z")).unwrap_or(default),
                ]
            })
    };
    let rotation = field(props, "RelativeRotation")
        .and_then(struct_fields)
        .map_or([0; 3], |f| {
            [
                as_i32(field(f, "Pitch")).unwrap_or(0),
                as_i32(field(f, "Yaw")).unwrap_or(0),
                as_i32(field(f, "Roll")).unwrap_or(0),
            ]
        });
    Some(SocketEntry {
        name,
        bone,
        location: vec3("RelativeLocation", 0.0),
        rotation,
        scale: vec3("RelativeScale", 1.0),
    })
}

struct MeshInput {
    package: String,
    vctx: ValidationContext,
    export_index: usize,
    mesh: SkeletalMesh,
    bone_names: Vec<String>,
    materials: Vec<Option<String>>,
    sockets: Vec<SocketEntry>,
    also_in: Vec<String>,
}

struct SequenceInput {
    seq: AnimSequence,
}

#[derive(Default)]
struct Gathered {
    meshes: BTreeMap<String, MeshInput>,
    sets: BTreeMap<String, AnimSetInfo>,
    sequences: BTreeMap<String, SequenceInput>,
    /// AnimSet path -> mesh paths used together by a SkeletalMeshComponent.
    component_pairs: BTreeMap<String, BTreeSet<String>>,
    failures: Vec<String>,
}

fn obj_path(v: &Value) -> Option<String> {
    match v {
        Value::Object(o) if o.index != 0 => Some(o.path.clone()),
        _ => None,
    }
}

fn gather_package(g: &mut Gathered, lp: &LoadedPackage, set: &PackageSet) {
    let pkg = &lp.package;
    for i in 0..pkg.exports.len() {
        if is_skeletal_mesh(pkg, i) {
            match decode_skeletal_mesh(pkg, Some(&lp.name), i, set) {
                Ok(mesh) => {
                    let path = mesh.object.path.clone();
                    if let Some(existing) = g.meshes.get_mut(&path) {
                        existing.also_in.push(lp.name.clone());
                        continue;
                    }
                    let names = bone_names(pkg, &mesh.native);
                    let materials = mesh
                        .native
                        .materials
                        .iter()
                        .map(|m| lp.ref_path(*m).ok().flatten())
                        .collect();
                    let sockets = mesh
                        .socket_refs()
                        .iter()
                        .filter_map(|r| PackageIndex(r.index).export_index())
                        .filter_map(|e| decode_object(pkg, Some(&lp.name), e, set).ok())
                        .filter_map(|o| socket_from(&o.properties))
                        .collect();
                    let vctx = ValidationContext {
                        payload_stream_offset: pkg
                            .export(i)
                            .ok()
                            .map(|e| i64::from(e.serial_offset)),
                        imports: pkg.imports.len(),
                        exports: pkg.exports.len(),
                    };
                    g.meshes.insert(
                        path,
                        MeshInput {
                            package: lp.name.clone(),
                            vctx,
                            export_index: i,
                            mesh,
                            bone_names: names,
                            materials,
                            sockets,
                            also_in: Vec::new(),
                        },
                    );
                }
                Err(e) => g.failures.push(format!("{} export {i}: {e}", lp.name)),
            }
        } else if is_anim_set(pkg, i) {
            match decode_anim_set(pkg, Some(&lp.name), i, set) {
                Ok(s) => {
                    g.sets.entry(s.path.clone()).or_insert(s);
                }
                Err(e) => g.failures.push(format!("{} export {i}: {e}", lp.name)),
            }
        } else if is_anim_sequence(pkg, i) {
            let path = pkg
                .export_ref(i)
                .ok()
                .and_then(|r| lp.ref_path(r).ok().flatten());
            if path.as_ref().is_some_and(|p| g.sequences.contains_key(p)) {
                continue;
            }
            match decode_anim_sequence(pkg, Some(&lp.name), i, set) {
                Ok(mut seq) => {
                    // Editor source keys are not exported; free them.
                    seq.native.raw_tracks = Vec::new();
                    g.sequences
                        .insert(seq.object.path.clone(), SequenceInput { seq });
                }
                Err(e) => g.failures.push(format!("{} export {i}: {e}", lp.name)),
            }
        } else if pkg
            .export_class_name(i)
            .is_ok_and(|c| c == "SkeletalMeshComponent")
            && let Ok(obj) = decode_object(pkg, Some(&lp.name), i, set)
        {
            let mesh = field(&obj.properties, "SkeletalMesh").and_then(obj_path);
            let sets: Vec<String> = match field(&obj.properties, "AnimSets") {
                Some(Value::Array(items)) => items.iter().filter_map(obj_path).collect(),
                _ => Vec::new(),
            };
            if let Some(mesh) = mesh {
                for s in sets {
                    g.component_pairs.entry(s).or_default().insert(mesh.clone());
                }
            }
        }
    }
}

/// How an AnimSet was paired with a mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingSource {
    /// A SkeletalMeshComponent uses both.
    Component,
    /// The set's `PreviewSkelMeshName`.
    Preview,
    /// The mesh whose bones match the most track names.
    BoneNames,
}

fn match_ratio(set: &AnimSetInfo, names: &[String]) -> f64 {
    if set.track_bone_names.is_empty() {
        return 0.0;
    }
    let found = set
        .track_bone_names
        .iter()
        .filter(|t| names.iter().any(|n| n.eq_ignore_ascii_case(t)))
        .count();
    found as f64 / set.track_bone_names.len() as f64
}

/// Mesh path -> (AnimSet path, source) list.
type Pairings = BTreeMap<String, Vec<(String, PairingSource)>>;

/// Pair every AnimSet with meshes; returns the pairings and the unpaired sets.
fn pair_sets(g: &Gathered) -> (Pairings, Vec<String>) {
    let mut out: Pairings = BTreeMap::new();
    let mut unpaired = Vec::new();
    for (path, set) in &g.sets {
        let mut chosen: Vec<(String, PairingSource)> = Vec::new();
        if let Some(meshes) = g.component_pairs.get(path) {
            for m in meshes {
                if let Some(mi) = g.meshes.get(m)
                    && match_ratio(set, &mi.bone_names) >= 0.5
                {
                    chosen.push((m.clone(), PairingSource::Component));
                }
            }
        }
        if chosen.is_empty()
            && let Some(preview) = &set.preview_skel_mesh_name
            && let Some(mi) = g.meshes.get(preview)
            && match_ratio(set, &mi.bone_names) >= 0.5
        {
            chosen.push((preview.clone(), PairingSource::Preview));
        }
        if chosen.is_empty() {
            let best = g
                .meshes
                .iter()
                .map(|(p, mi)| (match_ratio(set, &mi.bone_names), p))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((ratio, p)) = best
                && ratio >= 0.9
            {
                chosen.push((p.clone(), PairingSource::BoneNames));
            }
        }
        if chosen.is_empty() {
            unpaired.push(path.clone());
        }
        for (m, src) in chosen {
            out.entry(m).or_default().push((path.clone(), src));
        }
    }
    (out, unpaired)
}

// ---------------------------------------------------------------------------
// glTF construction
// ---------------------------------------------------------------------------

/// Per-sequence manifest record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceEntry {
    /// glTF animation index.
    pub animation: usize,
    /// glTF animation name (`<AnimSet>/<SequenceName>`).
    pub name: String,
    /// Object path.
    pub path: String,
    /// `SequenceName`.
    pub sequence_name: String,
    /// `NumFrames`.
    pub num_frames: i32,
    /// `SequenceLength` (seconds).
    pub length: f32,
    /// `RateScale`.
    pub rate_scale: f32,
    /// `bNoLoopingInterpolation`.
    pub no_looping_interpolation: bool,
    /// `bIsAdditive`.
    pub additive: bool,
    /// `KeyEncodingFormat`.
    pub key_encoding: String,
    /// Channels written.
    pub channels: usize,
    /// Tracks whose bone is not in the mesh skeleton.
    pub unmatched_tracks: usize,
    /// Notifies: time (s), notify object path, comment, duration.
    pub notifies: Vec<(f32, Option<String>, String, f32)>,
}

/// Per-AnimSet manifest record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimSetEntry {
    /// Object path.
    pub path: String,
    /// How it was paired with this mesh.
    pub source: PairingSource,
    /// Fraction of tracks whose bone exists on the mesh.
    pub match_ratio: f64,
    /// `bAnimRotationOnly`.
    pub anim_rotation_only: bool,
    /// Sequences exported as animations.
    pub sequences: Vec<SequenceEntry>,
}

/// Statistics of one converted LOD.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LodStats {
    /// Vertices.
    pub vertices: usize,
    /// Triangles.
    pub triangles: u64,
    /// Zero normals replaced by the face normal.
    pub normals_fixed: usize,
    /// Tangents replaced by a perpendicular.
    pub tangents_fixed: usize,
    /// Vertices whose weights did not sum to 255 (written as floats).
    pub weights_renormalised: usize,
}

/// One exported LOD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LodEntry {
    /// LOD index.
    pub lod: usize,
    /// `.gltf` path relative to `skeletal/`.
    pub gltf: String,
    /// `.bin` path relative to `skeletal/`.
    pub bin: String,
    /// Statistics.
    pub stats: LodStats,
}

/// One mesh in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshEntry {
    /// Package the mesh was converted from.
    pub package: String,
    /// Export index there.
    pub export_index: usize,
    /// Other packages holding the same object path.
    pub also_in: Vec<String>,
    /// glTF units per UU.
    pub scale: f32,
    /// Bone names (joint order).
    pub bones: Vec<String>,
    /// Material paths per slot.
    pub materials: Vec<Option<String>>,
    /// `Origin` (UE3).
    pub origin: [f32; 3],
    /// `RotOrigin` (pitch, yaw, roll).
    pub rot_origin: [i32; 3],
    /// UE3 bounds: origin, box extent, sphere radius.
    pub bounds_ue: ([f32; 3], [f32; 3], f32),
    /// Sockets.
    pub sockets: Vec<SocketEntry>,
    /// LOD models in the source.
    pub lod_count: usize,
    /// Exported LODs.
    pub lods: Vec<LodEntry>,
    /// AnimSets exported with LOD 0.
    pub anim_sets: Vec<AnimSetEntry>,
    /// FNV-1a of the written files.
    pub content_hash: String,
}

/// The manifest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version.
    pub version: u32,
    /// Legal notice.
    pub notice: String,
    /// Coordinate convention.
    pub coordinates: String,
    /// Meshes by object path.
    pub meshes: BTreeMap<String, MeshEntry>,
    /// AnimSets not paired with any mesh.
    pub unpaired_anim_sets: Vec<String>,
}

/// A converted glTF asset.
pub struct GltfAsset {
    /// The document.
    pub json: Json,
    /// Its binary buffer.
    pub bin: Vec<u8>,
    /// Statistics.
    pub stats: LodStats,
}

/// Animations to add to a document: (AnimSet, sequences in set order).
pub struct AnimInput<'a> {
    /// The set.
    pub set: &'a AnimSetInfo,
    /// Its sequences.
    pub sequences: Vec<&'a AnimSequence>,
}

/// Options of [`build_gltf`].
#[derive(Debug, Clone, Copy)]
pub struct BuildOptions {
    /// glTF units per UU.
    pub scale: f32,
    /// Evaluate every frame instead of writing stored keys.
    pub resample: bool,
}

fn unpack_normal(b: [u8; 4]) -> [f64; 4] {
    b.map(|x| f64::from(x) / 127.5 - 1.0)
}

fn perpendicular(n: V3) -> V3 {
    let a = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    normalize(cross(n, a)).unwrap_or([1.0, 0.0, 0.0])
}

fn short_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Mesh-to-component transform in UE3: `v → R·(v + Origin)`.
fn mesh_origin_rigid(mesh: &SkeletalMesh) -> Rigid {
    let n = &mesh.native;
    let r = rotator_to_quat(n.rot_origin[0], n.rot_origin[1], n.rot_origin[2]);
    let t = rotate(r, f64v(n.origin));
    Rigid { t, r }
}

/// Build one glTF document (one LOD) with its skin and, for LOD 0, the given
/// animations. `sequences_out` receives one manifest record per animation.
#[allow(clippy::too_many_arguments)]
pub fn build_gltf(
    mesh: &SkeletalMesh,
    names: &[String],
    materials: &[Option<String>],
    sockets: &[SocketEntry],
    lod: &SkelLodModel,
    anims: &[AnimInput<'_>],
    opts: &BuildOptions,
    bin_name: &str,
    sequences_out: &mut Vec<Vec<SequenceEntry>>,
) -> Result<GltfAsset> {
    let n = &mesh.native;
    let scale = f64::from(opts.scale);
    let nb = n.ref_skeleton.len();
    if nb == 0 {
        bail!("empty skeleton");
    }
    if nb > usize::from(u16::MAX) + 1 {
        bail!("{nb} bones exceed the 16-bit glTF joint indices");
    }
    if opts.resample {
        for seq in anims.iter().flat_map(|a| a.sequences.iter()) {
            if usize::try_from(seq.info.num_frames).is_ok_and(|f| f > MAX_RESAMPLE_FRAMES) {
                bail!(
                    "{}: NumFrames {} exceeds the resampling limit {MAX_RESAMPLE_FRAMES}",
                    seq.object.path,
                    seq.info.num_frames
                );
            }
        }
    }
    let mut bin = Bin::default();
    let mut stats = LodStats::default();

    // --- Skeleton nodes ---------------------------------------------------
    // Node 0: root (mesh origin); nodes 1..=nb: bones; then sockets; then the
    // skinned mesh node.
    let locals: Vec<Rigid> = n
        .ref_skeleton
        .iter()
        .map(|b| rigid_to_gltf(f64v(b.position), qnormalize(f64q(b.orientation)), scale))
        .collect();
    let mut globals: Vec<Rigid> = Vec::with_capacity(nb);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (i, b) in n.ref_skeleton.iter().enumerate() {
        if i == 0 {
            globals.push(locals[0]);
            continue;
        }
        let p = usize::try_from(b.parent_index)
            .ok()
            .filter(|&p| p < i)
            .with_context(|| format!("bone {i} has parent {}", b.parent_index))?;
        children[p].push(i);
        globals.push(globals[p].then_child(&locals[i]));
    }
    let bone_node = |b: usize| b + 1;
    let socket_base = nb + 1;
    let mut socket_children: Vec<Vec<usize>> = vec![Vec::new(); nb];
    let mut socket_nodes = Vec::new();
    for (si, s) in sockets.iter().enumerate() {
        let Some(b) = names.iter().position(|n| n.eq_ignore_ascii_case(&s.bone)) else {
            continue;
        };
        let r = rotator_to_quat(s.rotation[0], s.rotation[1], s.rotation[2]);
        let local = rigid_to_gltf(f64v(s.location), r, scale);
        socket_children[b].push(socket_base + socket_nodes.len());
        socket_nodes.push(json!({
            "name": format!("SOCKET_{}", s.name),
            "translation": local.t.map(|c| c as f32),
            "rotation": local.r.map(|c| c as f32),
            "scale": ue_to_gltf(s.scale).map(f32::abs),
            "extras": {"socket": si, "bone": s.bone},
        }));
    }
    let mesh_node = socket_base + socket_nodes.len();
    let root = rigid_to_gltf(mesh_origin_rigid(mesh).t, mesh_origin_rigid(mesh).r, scale);
    let mut nodes = vec![json!({
        "name": short_name(&mesh.object.path),
        "translation": root.t.map(|c| c as f32),
        "rotation": root.r.map(|c| c as f32),
        "children": [bone_node(0)],
    })];
    for (i, l) in locals.iter().enumerate() {
        let mut node = json!({
            "name": names.get(i).cloned().unwrap_or_else(|| format!("bone{i}")),
            "translation": l.t.map(|c| c as f32),
            "rotation": l.r.map(|c| c as f32),
        });
        let mut ch: Vec<usize> = children[i].iter().map(|&c| bone_node(c)).collect();
        ch.extend(&socket_children[i]);
        if !ch.is_empty() {
            node["children"] = json!(ch);
        }
        nodes.push(node);
    }
    nodes.extend(socket_nodes);

    // --- Skin --------------------------------------------------------------
    let ibms: Vec<[f32; 16]> = globals
        .iter()
        .map(|g| g.inverse().to_matrix().map(|c| c as f32))
        .collect();
    let ibm = bin.mat4_accessor(&ibms);
    let skin = json!({
        "name": format!("{}_Skin", short_name(&mesh.object.path)),
        "inverseBindMatrices": ibm,
        "joints": (0..nb).map(bone_node).collect::<Vec<_>>(),
        "skeleton": bone_node(0),
    });

    // --- Vertices ----------------------------------------------------------
    let vb = &lod.vertex_buffer;
    let nv = vb.vertices.len();
    if u32::try_from(nv).ok() != Some(lod.num_vertices) {
        bail!(
            "vertex buffer holds {nv} vertices, NumVertices {}",
            lod.num_vertices
        );
    }
    let indices: Vec<u32> = lod.indices.indices.clone();
    if indices
        .iter()
        .any(|&i| usize::try_from(i).map_or(true, |i| i >= nv))
    {
        bail!("index out of range");
    }
    // Face normals (fallback for zero stored normals).
    let mut face_normal_sum = vec![[0f64; 3]; nv];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| usize::try_from(i).unwrap_or(0));
        let pa = f64v(ue_to_gltf(vb.vertices[a].position));
        let pb = f64v(ue_to_gltf(vb.vertices[b].position));
        let pc = f64v(ue_to_gltf(vb.vertices[c].position));
        let fnm = cross(
            [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]],
            [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]],
        );
        for v in [a, b, c] {
            for k in 0..3 {
                face_normal_sum[v][k] += fnm[k];
            }
        }
    }
    let mut positions = Vec::with_capacity(nv * 3);
    let mut normals = Vec::with_capacity(nv * 3);
    let mut tangents = Vec::with_capacity(nv * 4);
    let channels = usize::try_from(vb.num_tex_coords).unwrap_or(0).min(4);
    let mut uvs: Vec<Vec<f32>> = vec![Vec::with_capacity(nv * 2); channels];
    let wide_joints = nb > 256;
    let mut joints8: Vec<u8> = Vec::with_capacity(nv * 4);
    let mut joints16: Vec<u16> = Vec::with_capacity(nv * 4);
    let mut weights8: Vec<u8> = Vec::with_capacity(nv * 4);
    let mut weightsf: Vec<f32> = Vec::with_capacity(nv * 4);
    // Vertex -> chunk.
    let mut chunk_of = vec![usize::MAX; nv];
    for (ci, c) in lod.chunks.iter().enumerate() {
        let lo = usize::try_from(c.base_vertex_index).unwrap_or(usize::MAX);
        let count = c
            .vertex_count()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        for slot in chunk_of.iter_mut().skip(lo).take(count) {
            *slot = ci;
        }
    }
    for (vi, v) in vb.vertices.iter().enumerate() {
        let p = ue_to_gltf(v.position);
        positions.extend(p.map(|c| (f64::from(c) * scale) as f32));
        let z = unpack_normal(v.tangent_z.0);
        let x = unpack_normal(v.tangent_x.0);
        let nz = [z[1], z[2], -z[0]];
        let normal = match normalize(nz) {
            Some(n) if len(nz) > 0.5 => n,
            _ => {
                stats.normals_fixed += 1;
                normalize(face_normal_sum[vi]).unwrap_or([0.0, 1.0, 0.0])
            }
        };
        normals.extend(normal.map(|c| c as f32));
        let tx = [x[1], x[2], -x[0]];
        let d = dot(tx, normal);
        let ortho = [
            tx[0] - normal[0] * d,
            tx[1] - normal[1] * d,
            tx[2] - normal[2] * d,
        ];
        let tangent = match normalize(ortho) {
            Some(t) if len(ortho) > 0.1 => t,
            _ => {
                stats.tangents_fixed += 1;
                perpendicular(normal)
            }
        };
        let w = if z[3] >= 0.0 { -1.0f32 } else { 1.0 };
        tangents.extend(tangent.map(|c| c as f32));
        tangents.push(w);
        for (ch, out) in uvs.iter_mut().enumerate() {
            out.extend(v.uvs[ch]);
        }
        let chunk = lod
            .chunks
            .get(chunk_of[vi])
            .with_context(|| format!("vertex {vi} lies in no chunk"))?;
        let sum: u32 = v.influence_weights.iter().map(|&w| u32::from(w)).sum();
        if sum != 255 {
            stats.weights_renormalised += 1;
        }
        for k in 0..4 {
            let w = v.influence_weights[k];
            let bone = if w == 0 {
                0
            } else {
                let local = usize::from(v.influence_bones[k]);
                let b = usize::from(*chunk.bone_map.get(local).with_context(|| {
                    format!("vertex {vi} bone {local} outside its chunk's bone map")
                })?);
                if b >= nb {
                    bail!("vertex {vi} references bone {b} of {nb}");
                }
                b
            };
            if wide_joints {
                joints16.push(u16::try_from(bone).unwrap_or(0));
            } else {
                joints8.push(u8::try_from(bone).unwrap_or(0));
            }
            weights8.push(w);
            weightsf.push(if sum == 0 {
                0.0
            } else {
                f32::from(w) / sum as f32
            });
        }
    }
    let pos_acc = bin.floats(&positions, "VEC3", 3, Some(ARRAY_BUFFER), true);
    let nrm_acc = bin.floats(&normals, "VEC3", 3, Some(ARRAY_BUFFER), false);
    let tan_acc = bin.floats(&tangents, "VEC4", 4, Some(ARRAY_BUFFER), false);
    let uv_accs: Vec<usize> = uvs
        .iter()
        .map(|u| bin.floats(u, "VEC2", 2, Some(ARRAY_BUFFER), false))
        .collect();
    let joints_acc = if wide_joints {
        bin.u16_accessor(&joints16, "VEC4", Some(ARRAY_BUFFER))
    } else {
        bin.bytes_accessor(&joints8, "VEC4", false)
    };
    let weights_acc = if stats.weights_renormalised == 0 {
        bin.bytes_accessor(&weights8, "VEC4", true)
    } else {
        bin.floats(&weightsf, "VEC4", 4, Some(ARRAY_BUFFER), false)
    };
    let mut attributes = json!({
        "POSITION": pos_acc,
        "NORMAL": nrm_acc,
        "TANGENT": tan_acc,
        "JOINTS_0": joints_acc,
        "WEIGHTS_0": weights_acc,
    });
    for (ch, acc) in uv_accs.iter().enumerate() {
        attributes[format!("TEXCOORD_{ch}")] = json!(acc);
    }
    stats.vertices = nv;

    // --- Primitives (one per section) ----------------------------------------
    let wide_indices = nv > usize::from(u16::MAX);
    let mut primitives = Vec::new();
    for s in &lod.sections {
        let first = usize::try_from(s.base_index).unwrap_or(usize::MAX);
        let count = usize::try_from(s.num_triangles)
            .ok()
            .and_then(|t| t.checked_mul(3))
            .unwrap_or(usize::MAX);
        let slice = first
            .checked_add(count)
            .and_then(|end| indices.get(first..end))
            .context("section outside the index buffer")?;
        if slice.is_empty() {
            continue;
        }
        let acc = if wide_indices {
            bin.u32_accessor(slice)
        } else {
            let v: Vec<u16> = slice
                .iter()
                .map(|&i| u16::try_from(i).unwrap_or(0))
                .collect();
            bin.u16_accessor(&v, "SCALAR", Some(ELEMENT_ARRAY_BUFFER))
        };
        stats.triangles += u64::from(s.num_triangles);
        primitives.push(json!({
            "attributes": attributes,
            "indices": acc,
            "material": usize::from(s.material_index),
            "mode": 4,
        }));
    }
    if primitives.is_empty() {
        bail!("LOD has no triangles");
    }
    let slot_count = materials.len().max(
        lod.sections
            .iter()
            .map(|s| usize::from(s.material_index) + 1)
            .max()
            .unwrap_or(0),
    );
    let gltf_materials: Vec<Json> = (0..slot_count)
        .map(|i| {
            let name = materials
                .get(i)
                .cloned()
                .flatten()
                .unwrap_or_else(|| "None".to_owned());
            json!({"name": name, "pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.8, 0.8, 1.0],
                   "metallicFactor": 0.0, "roughnessFactor": 1.0}})
        })
        .collect();
    nodes.push(json!({
        "name": format!("{}_Mesh", short_name(&mesh.object.path)),
        "mesh": 0,
        "skin": 0,
    }));

    // --- Animations ----------------------------------------------------------
    let mut animations = Vec::new();
    for a in anims {
        let mut entries = Vec::new();
        let table = bone_to_track(&a.set.track_bone_names, names);
        for seq in &a.sequences {
            let info = &seq.info;
            let mut samplers = Vec::new();
            let mut channels = Vec::new();
            let mut matched = vec![false; seq.tracks.len()];
            for (bone, track) in table.iter().enumerate() {
                let Some(ti) = *track else { continue };
                let Some(bt) = seq.tracks.get(ti) else {
                    continue;
                };
                if let Some(m) = matched.get_mut(ti) {
                    *m = true;
                }
                let (times, values) = rotation_keys(
                    bt.rotation.as_ref(),
                    bone,
                    info.sequence_length,
                    info.num_frames,
                    opts.resample,
                );
                let input = bin.floats(&times, "SCALAR", 1, None, true);
                let output = bin.floats(&values, "VEC4", 4, None, false);
                samplers.push(json!({"input": input, "output": output, "interpolation": "LINEAR"}));
                channels.push(json!({"sampler": samplers.len() - 1,
                                     "target": {"node": bone_node(bone), "path": "rotation"}}));
                if a.set.uses_anim_translation(ti, bone == 0) {
                    let (times, values) = translation_keys(
                        bt.translation.as_ref(),
                        info.sequence_length,
                        info.num_frames,
                        opts.resample,
                        scale,
                    );
                    let input = bin.floats(&times, "SCALAR", 1, None, true);
                    let output = bin.floats(&values, "VEC3", 3, None, false);
                    samplers
                        .push(json!({"input": input, "output": output, "interpolation": "LINEAR"}));
                    channels.push(json!({"sampler": samplers.len() - 1,
                                         "target": {"node": bone_node(bone), "path": "translation"}}));
                }
            }
            if channels.is_empty() {
                continue;
            }
            let name = format!("{}/{}", short_name(&a.set.path), info.sequence_name);
            entries.push(SequenceEntry {
                animation: animations.len(),
                name: name.clone(),
                path: seq.object.path.clone(),
                sequence_name: info.sequence_name.clone(),
                num_frames: info.num_frames,
                length: info.sequence_length,
                rate_scale: info.rate_scale,
                no_looping_interpolation: info.no_looping_interpolation,
                additive: info.is_additive,
                key_encoding: info.key_encoding.name().to_owned(),
                channels: channels.len(),
                unmatched_tracks: matched.iter().filter(|m| !**m).count(),
                notifies: info
                    .notifies
                    .iter()
                    .map(|n| (n.time, n.notify.clone(), n.comment.clone(), n.duration))
                    .collect(),
            });
            animations.push(json!({"name": name, "samplers": samplers, "channels": channels}));
        }
        sequences_out.push(entries);
    }

    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "asamu-import skeletal", "copyright": NOTICE},
        "scene": 0,
        "scenes": [{"name": short_name(&mesh.object.path), "nodes": [0, mesh_node]}],
        "nodes": nodes,
        "meshes": [{"name": short_name(&mesh.object.path), "primitives": primitives}],
        "skins": [skin],
        "materials": gltf_materials,
        "extras": {"source": mesh.object.path, "ue3_origin": n.origin, "ue3_rot_origin": n.rot_origin},
    });
    if !animations.is_empty() {
        doc["animations"] = json!(animations);
    }
    while !bin.bytes.len().is_multiple_of(4) {
        bin.bytes.push(0);
    }
    doc["buffers"] = json!([{"uri": bin_name, "byteLength": bin.bytes.len()}]);
    doc["bufferViews"] = json!(bin.views);
    doc["accessors"] = json!(bin.accessors);
    Ok(GltfAsset {
        json: doc,
        bin: bin.bytes,
        stats,
    })
}

/// Push a key, keeping times strictly increasing: a key that does not come
/// after the previous one replaces its value (the later key wins).
fn push_key<T: Copy>(out: &mut Vec<(f32, T)>, time: f32, value: T) {
    match out.last_mut() {
        Some(last) if time <= last.0 => last.1 = value,
        _ => out.push((time, value)),
    }
}

/// Turn engine key times into a glTF sampler input (finite, non-negative,
/// strictly increasing) without changing what the engine shows.
///
/// Frame tables may give several keys the same frame (in the shipped data
/// only the final frame, on 515 tracks, and the two keys differ on most of
/// them). The engine's frame-table search interpolates *towards the first*
/// key of such a run and samples the *last* one from the shared time on (at
/// time 0, where the engine returns the first key, the other way round); keys
/// inside a run are never sampled. glTF cannot hold two keys at one time, so
/// the first key of a run is written one `f32` step before the shared time
/// (one step after it at time 0) and the last key at it. Non-finite or
/// negative times (only possible with malformed tags) become 0, and times that
/// go backwards are clamped to the previous time.
fn sampler_keys<T: Copy>(keys: Vec<(f32, T)>) -> Vec<(f32, T)> {
    let mut clamped: Vec<(f32, T)> = Vec::with_capacity(keys.len());
    let mut prev = 0.0f32;
    for (t, v) in keys {
        let t = if t.is_finite() { t.max(prev) } else { prev };
        prev = t;
        clamped.push((t, v));
    }
    let mut out: Vec<(f32, T)> = Vec::with_capacity(clamped.len().saturating_add(1));
    let mut i = 0;
    while let Some(&(t, first)) = clamped.get(i) {
        let mut j = i;
        while clamped.get(j + 1).is_some_and(|k| k.0 == t) {
            j += 1;
        }
        let last = clamped.get(j).map_or(first, |k| k.1);
        if i == j {
            push_key(&mut out, t, first);
        } else if t == 0.0 {
            push_key(&mut out, 0.0, first);
            push_key(&mut out, t.next_up(), last);
        } else {
            let before = t.next_down();
            // Without room before the shared time the previous key stays.
            if out.last().is_none_or(|k| k.0 < before) {
                out.push((before, first));
            }
            push_key(&mut out, t, last);
        }
        i = j + 1;
    }
    out
}

/// Most frames `--resample` evaluates per sequence (sanity limit, not a format
/// value: the longest shipped sequence has 3,091 frames).
const MAX_RESAMPLE_FRAMES: usize = 1 << 16;

fn sample_times(length: f32, num_frames: i32) -> Vec<f32> {
    let n = usize::try_from(num_frames)
        .unwrap_or(0)
        .clamp(1, MAX_RESAMPLE_FRAMES);
    if n == 1 {
        return vec![0.0];
    }
    (0..n).map(|f| f as f32 / (n - 1) as f32 * length).collect()
}

/// Rotation keys of one bone in glTF space (engine pose rule applied,
/// hemisphere-continuous for LINEAR interpolation).
fn rotation_keys(
    track: Option<&CompressedTrack>,
    bone: usize,
    length: f32,
    num_frames: i32,
    resample: bool,
) -> (Vec<f32>, Vec<f32>) {
    let keys: Vec<(f32, [f32; 4])> = match track {
        None => vec![(0.0, [0.0, 0.0, 0.0, 1.0])],
        Some(t) if resample => sample_times(length, num_frames)
            .into_iter()
            .map(|time| (time, sample_rotation(t, time, length, num_frames)))
            .collect(),
        // A track without keys (only possible in malformed data) reads as
        // its format's missing-data value, like an absent track.
        Some(t) if t.num_keys == 0 => vec![(0.0, t.rotation_key(0))],
        Some(t) => (0..t.num_keys)
            .map(|k| (t.key_time(k, length, num_frames), t.rotation_key(k)))
            .collect(),
    };
    let keys = sampler_keys(keys);
    let mut times = Vec::with_capacity(keys.len());
    let mut values = Vec::with_capacity(keys.len().saturating_mul(4));
    let mut prev: Option<Q> = None;
    for (time, q) in keys {
        let q = qnormalize(f64q(ue_quat_to_gltf(pose_rotation(q, bone))));
        let q = match prev {
            Some(p) if p[0] * q[0] + p[1] * q[1] + p[2] * q[2] + p[3] * q[3] < 0.0 => q.map(|c| -c),
            _ => q,
        };
        prev = Some(q);
        times.push(time);
        values.extend(q.map(|c| c as f32));
    }
    (times, values)
}

fn translation_keys(
    track: Option<&CompressedTrack>,
    length: f32,
    num_frames: i32,
    resample: bool,
    scale: f64,
) -> (Vec<f32>, Vec<f32>) {
    let keys: Vec<(f32, [f32; 3])> = match track {
        None => vec![(0.0, [0.0; 3])],
        Some(t) if resample => sample_times(length, num_frames)
            .into_iter()
            .map(|time| (time, sample_translation(t, time, length, num_frames)))
            .collect(),
        Some(t) if t.num_keys == 0 => vec![(0.0, t.translation_key(0))],
        Some(t) => (0..t.num_keys)
            .map(|k| (t.key_time(k, length, num_frames), t.translation_key(k)))
            .collect(),
    };
    let keys = sampler_keys(keys);
    let mut times = Vec::with_capacity(keys.len());
    let mut values = Vec::with_capacity(keys.len().saturating_mul(3));
    for (time, v) in keys {
        times.push(time);
        values.extend(ue_to_gltf(v).map(|c| (f64::from(c) * scale) as f32));
    }
    (times, values)
}

// ---------------------------------------------------------------------------
// Validation of a written document
// ---------------------------------------------------------------------------

struct Acc<'a> {
    bytes: &'a [u8],
    component_type: u64,
    components: usize,
    count: usize,
    normalized: bool,
}

impl Acc<'_> {
    fn component_size(&self) -> usize {
        match self.component_type {
            5121 | 5120 => 1,
            5123 | 5122 => 2,
            _ => 4,
        }
    }

    fn f32_at(&self, i: usize, k: usize) -> Option<f32> {
        let at = (i * self.components + k) * 4;
        let b = self.bytes.get(at..at + 4)?;
        Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn uint_at(&self, i: usize, k: usize) -> Option<u32> {
        let cs = self.component_size();
        let at = (i * self.components + k) * cs;
        let b = self.bytes.get(at..at + cs)?;
        Some(match cs {
            1 => u32::from(b[0]),
            2 => u32::from(u16::from_le_bytes([b[0], b[1]])),
            _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        })
    }

    fn value(&self, i: usize, k: usize) -> Option<f64> {
        if self.component_type == u64::from(FLOAT) {
            return self.f32_at(i, k).map(f64::from);
        }
        let v = f64::from(self.uint_at(i, k)?);
        Some(if self.normalized {
            v / if self.component_size() == 1 {
                255.0
            } else {
                65535.0
            }
        } else {
            v
        })
    }
}

fn idx(v: &Json) -> Option<usize> {
    usize::try_from(v.as_u64()?).ok()
}

fn json_vec(v: &Json) -> Option<Vec<f64>> {
    v.as_array()?.iter().map(Json::as_f64).collect()
}

/// Structural checks of a skinned, animated glTF document against its
/// buffer. Returns one message per problem (empty when consistent).
pub fn validate_gltf(doc: &Json, bin: &[u8]) -> Vec<String> {
    let mut issues = Vec::new();
    if doc["asset"]["version"] != "2.0" {
        issues.push("asset.version is not 2.0".to_owned());
    }
    let buffers = doc["buffers"].as_array().cloned().unwrap_or_default();
    if buffers.len() != 1 || buffers[0]["byteLength"].as_u64() != u64::try_from(bin.len()).ok() {
        issues.push("expected one buffer matching the .bin length".to_owned());
        return issues;
    }
    let views = doc["bufferViews"].as_array().cloned().unwrap_or_default();
    let view_bytes: Vec<Option<&[u8]>> = views
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let off = v["byteOffset"]
                .as_u64()
                .and_then(|o| usize::try_from(o).ok());
            let l = v["byteLength"]
                .as_u64()
                .and_then(|o| usize::try_from(o).ok());
            let s = off
                .zip(l)
                .and_then(|(o, l)| bin.get(o..o.checked_add(l)?))
                .filter(|s| !s.is_empty());
            if s.is_none() || off.is_some_and(|o| !o.is_multiple_of(4)) {
                issues.push(format!(
                    "bufferView {i} is outside the buffer, empty or unaligned"
                ));
            }
            s
        })
        .collect();
    let accessors = doc["accessors"].as_array().cloned().unwrap_or_default();
    let acc: Vec<Option<Acc<'_>>> = accessors
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let r = (|| {
                let view = (*view_bytes.get(idx(&a["bufferView"])?)?)?;
                let ct = a["componentType"].as_u64()?;
                let components = match a["type"].as_str()? {
                    "SCALAR" => 1,
                    "VEC2" => 2,
                    "VEC3" => 3,
                    "VEC4" => 4,
                    "MAT4" => 16,
                    _ => return None,
                };
                let cs = match ct {
                    5121 | 5120 => 1,
                    5123 | 5122 => 2,
                    5125 | 5126 => 4,
                    _ => return None,
                };
                let count = idx(&a["count"])?;
                let len = count.checked_mul(cs)?.checked_mul(components)?;
                if count == 0 {
                    return None;
                }
                Some(Acc {
                    bytes: view.get(..len)?,
                    component_type: ct,
                    components,
                    count,
                    normalized: a["normalized"].as_bool().unwrap_or(false),
                })
            })();
            if r.is_none() {
                issues.push(format!("accessor {i} is invalid"));
            }
            r
        })
        .collect();
    let get = |v: &Json| -> Option<&Acc<'_>> { acc.get(idx(v)?)?.as_ref() };
    // Float accessors are finite; min/max equal the data.
    for (i, a) in accessors.iter().enumerate() {
        let Some(d) = acc.get(i).and_then(Option::as_ref) else {
            continue;
        };
        if d.component_type != u64::from(FLOAT) {
            continue;
        }
        let mut lo = vec![f64::INFINITY; d.components];
        let mut hi = vec![f64::NEG_INFINITY; d.components];
        let mut finite = true;
        for v in 0..d.count {
            for k in 0..d.components {
                let x = d.f32_at(v, k).map_or(f64::NAN, f64::from);
                finite &= x.is_finite();
                lo[k] = lo[k].min(x);
                hi[k] = hi[k].max(x);
            }
        }
        if !finite {
            issues.push(format!("accessor {i} holds non-finite floats"));
        }
        for (key, want) in [("min", &lo), ("max", &hi)] {
            if let Some(got) = a.get(key).and_then(json_vec)
                && got
                    .iter()
                    .zip(want.iter())
                    .any(|(g, w)| (*g as f32) != (*w as f32))
            {
                issues.push(format!("accessor {i} {key} does not match the data"));
            }
        }
    }
    // Nodes: a tree.
    let nodes = doc["nodes"].as_array().cloned().unwrap_or_default();
    let mut parent: Vec<Option<usize>> = vec![None; nodes.len()];
    for (ni, n) in nodes.iter().enumerate() {
        for c in n["children"].as_array().cloned().unwrap_or_default() {
            match idx(&c) {
                Some(c) if c < nodes.len() && parent[c].is_none() && c != ni => {
                    parent[c] = Some(ni)
                }
                _ => issues.push(format!("node {ni} has an invalid or shared child")),
            }
        }
        // TRS values must be finite numbers of the right arity (a non-finite
        // float would have been written as JSON null).
        for (key, arity) in [("translation", 3), ("rotation", 4), ("scale", 3)] {
            if let Some(v) = n.get(key) {
                let ok = json_vec(v)
                    .is_some_and(|c| c.len() == arity && c.iter().all(|x| x.is_finite()));
                if !ok {
                    issues.push(format!("node {ni} {key} is not {arity} finite numbers"));
                }
            }
        }
        if let Some(r) = n.get("rotation").and_then(json_vec) {
            let l = r.iter().map(|c| c * c).sum::<f64>().sqrt();
            if r.len() != 4 || (l - 1.0).abs() > f64::from(UNIT_TOLERANCE) {
                issues.push(format!("node {ni} rotation is not a unit quaternion"));
            }
        }
        let mesh_count = doc["meshes"].as_array().map_or(0, Vec::len);
        let skin_count = doc["skins"].as_array().map_or(0, Vec::len);
        if n.get("mesh")
            .is_some_and(|m| idx(m).is_none_or(|m| m >= mesh_count))
        {
            issues.push(format!("node {ni} references a missing mesh"));
        }
        if let Some(s) = n.get("skin") {
            if idx(s).is_none_or(|s| s >= skin_count) {
                issues.push(format!("node {ni} references a missing skin"));
            }
            if n.get("mesh").is_none() {
                issues.push(format!("node {ni} has a skin but no mesh"));
            }
        }
    }
    for start in 0..nodes.len() {
        let mut cur = Some(start);
        let mut steps = 0;
        while let Some(c) = cur {
            steps += 1;
            if steps > nodes.len() {
                issues.push(format!("node {start} is in a cycle"));
                break;
            }
            cur = parent[c];
        }
    }
    let scene_roots: Vec<usize> = doc["scenes"][0]["nodes"]
        .as_array()
        .map(|a| a.iter().filter_map(idx).collect())
        .unwrap_or_default();
    if scene_roots.is_empty()
        || scene_roots
            .iter()
            .any(|&r| r >= nodes.len() || parent[r].is_some())
    {
        issues.push("scene roots are missing or not roots".to_owned());
    }
    // Global transforms of the nodes.
    let local = |n: &Json| -> Rigid {
        let t = n.get("translation").and_then(json_vec).unwrap_or_default();
        let r = n.get("rotation").and_then(json_vec).unwrap_or_default();
        Rigid {
            t: [0, 1, 2].map(|k| t.get(k).copied().unwrap_or(0.0)),
            r: if r.len() == 4 {
                [r[0], r[1], r[2], r[3]]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            },
        }
    };
    let global = |mut i: usize| -> Rigid {
        let mut chain = vec![i];
        while let Some(p) = parent.get(i).copied().flatten() {
            if chain.len() > nodes.len() {
                break;
            }
            chain.push(p);
            i = p;
        }
        chain
            .iter()
            .rev()
            .fold(Rigid::IDENTITY, |acc, &n| acc.then_child(&local(&nodes[n])))
    };
    // Skins.
    let skins = doc["skins"].as_array().cloned().unwrap_or_default();
    let mut joint_counts = Vec::new();
    for (si, s) in skins.iter().enumerate() {
        let joints: Vec<usize> = s["joints"]
            .as_array()
            .map(|a| a.iter().filter_map(idx).collect())
            .unwrap_or_default();
        joint_counts.push(joints.len());
        if joints.is_empty() || joints.iter().any(|&j| j >= nodes.len()) {
            issues.push(format!("skin {si} has missing or invalid joints"));
            continue;
        }
        if joints.iter().collect::<BTreeSet<_>>().len() != joints.len() {
            issues.push(format!("skin {si} repeats a joint"));
        }
        if let Some(sk) = s.get("skeleton") {
            // The skeleton node must be an ancestor of (or equal to) every joint.
            let ok = idx(sk).is_some_and(|sk| {
                joints.iter().all(|&j| {
                    let mut cur = Some(j);
                    let mut steps = 0;
                    while let Some(c) = cur {
                        if c == sk {
                            return true;
                        }
                        steps += 1;
                        if steps > nodes.len() {
                            break;
                        }
                        cur = parent.get(c).copied().flatten();
                    }
                    false
                })
            });
            if !ok {
                issues.push(format!("skin {si} skeleton is not a root of its joints"));
            }
        }
        let Some(ibm) = get(&s["inverseBindMatrices"]) else {
            issues.push(format!("skin {si} has no inverse bind matrices"));
            continue;
        };
        if ibm.components != 16 || ibm.count != joints.len() {
            issues.push(format!(
                "skin {si} inverse bind matrices do not match the joints"
            ));
            continue;
        }
        // IBM * global(joint) relative to the skeleton root must be identity:
        // compare against the joint's transform below the skin's parent.
        let base = joints
            .first()
            .and_then(|&j| parent[j])
            .map_or(Rigid::IDENTITY, global);
        let base_inv = base.inverse();
        for (k, &j) in joints.iter().enumerate() {
            let g = base_inv.then_child(&global(j)).to_matrix();
            let m: Vec<f64> = (0..16)
                .map(|c| ibm.f32_at(k, c).map_or(f64::NAN, f64::from))
                .collect();
            // (IBM · G) for column-major 4x4 matrices.
            let mut worst = 0.0f64;
            for col in 0..4 {
                for row in 0..4 {
                    let v: f64 = (0..4).map(|x| m[x * 4 + row] * g[col * 4 + x]).sum();
                    let want = if row == col { 1.0 } else { 0.0 };
                    worst = worst.max((v - want).abs());
                }
            }
            let limit = 1e-3 * (1.0 + len(global(j).t));
            if worst.is_nan() || worst >= limit {
                issues.push(format!(
                    "skin {si} joint {k}: inverse bind matrix does not invert the bind pose"
                ));
                break;
            }
        }
    }
    // Meshes.
    let materials = doc["materials"].as_array().map_or(0, Vec::len);
    for (mi, m) in doc["meshes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let joints = joint_counts.first().copied().unwrap_or(0);
        let prims = m["primitives"].as_array().cloned().unwrap_or_default();
        if prims.is_empty() {
            issues.push(format!("mesh {mi} has no primitives"));
        }
        for (pi, p) in prims.iter().enumerate() {
            let at = format!("mesh {mi} primitive {pi}");
            let attrs = p["attributes"].as_object().cloned().unwrap_or_default();
            let Some(pos) = attrs.get("POSITION").and_then(get) else {
                issues.push(format!("{at}: missing POSITION"));
                continue;
            };
            if accessors
                .get(idx(&attrs["POSITION"]).unwrap_or(usize::MAX))
                .is_none_or(|a| a.get("min").is_none())
            {
                issues.push(format!("{at}: POSITION has no min/max"));
            }
            for (name, v) in &attrs {
                match get(v) {
                    Some(d) if d.count == pos.count => {}
                    _ => issues.push(format!("{at}: {name} count differs from POSITION")),
                }
            }
            for (name, comps) in [("NORMAL", 3usize), ("TANGENT", 3)] {
                if let Some(d) = attrs.get(name).and_then(get) {
                    let bad = (0..d.count)
                        .filter(|&i| {
                            let l = (0..comps)
                                .map(|k| d.value(i, k).unwrap_or(0.0).powi(2))
                                .sum::<f64>()
                                .sqrt();
                            (l - 1.0).abs() > f64::from(UNIT_TOLERANCE)
                        })
                        .count();
                    if bad > 0 {
                        issues.push(format!("{at}: {bad} {name} vectors are not unit length"));
                    }
                }
            }
            if let (Some(j), Some(w)) = (
                attrs.get("JOINTS_0").and_then(get),
                attrs.get("WEIGHTS_0").and_then(get),
            ) {
                for i in 0..j.count {
                    let sum: f64 = (0..4).map(|k| w.value(i, k).unwrap_or(0.0)).sum();
                    if (sum - 1.0).abs() > 2e-3 {
                        issues.push(format!("{at}: vertex {i} weights sum to {sum}"));
                        break;
                    }
                    if (0..4).any(|k| {
                        j.uint_at(i, k)
                            .is_none_or(|b| usize::try_from(b).map_or(true, |b| b >= joints))
                    }) {
                        issues.push(format!("{at}: vertex {i} uses a joint outside the skin"));
                        break;
                    }
                }
            } else {
                issues.push(format!("{at}: missing JOINTS_0 / WEIGHTS_0"));
            }
            match p.get("indices").and_then(get) {
                Some(ix) => {
                    if !ix.count.is_multiple_of(3) {
                        issues.push(format!("{at}: index count not a multiple of 3"));
                    }
                    let nv = u32::try_from(pos.count).unwrap_or(u32::MAX);
                    if (0..ix.count).any(|i| ix.uint_at(i, 0).is_none_or(|x| x >= nv)) {
                        issues.push(format!("{at}: index out of range"));
                    }
                }
                None => issues.push(format!("{at}: no indices")),
            }
            if p.get("material")
                .and_then(idx)
                .is_none_or(|m| m >= materials)
            {
                issues.push(format!("{at}: material index out of range"));
            }
        }
    }
    // Animations.
    for (ai, a) in doc["animations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let samplers = a["samplers"].as_array().cloned().unwrap_or_default();
        for (si, s) in samplers.iter().enumerate() {
            let (Some(input), Some(output)) = (get(&s["input"]), get(&s["output"])) else {
                issues.push(format!("animation {ai} sampler {si} has invalid accessors"));
                continue;
            };
            let times: Vec<f32> = (0..input.count)
                .filter_map(|i| input.f32_at(i, 0))
                .collect();
            if times.len() != input.count
                || times.iter().any(|t| !t.is_finite())
                || times.windows(2).any(|w| w[0] >= w[1])
                || times.first().is_some_and(|t| *t < 0.0)
            {
                issues.push(format!(
                    "animation {ai} sampler {si}: times not strictly increasing"
                ));
            }
            if accessors
                .get(idx(&s["input"]).unwrap_or(usize::MAX))
                .is_none_or(|a| a.get("min").is_none())
            {
                issues.push(format!("animation {ai} sampler {si}: input has no min/max"));
            }
            if output.count != input.count {
                issues.push(format!(
                    "animation {ai} sampler {si}: {} outputs for {} times",
                    output.count, input.count
                ));
            }
            if output.components == 4
                && (0..output.count).any(|i| {
                    let l = (0..4)
                        .map(|k| output.value(i, k).unwrap_or(0.0).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    (l - 1.0).abs() > f64::from(UNIT_TOLERANCE)
                })
            {
                issues.push(format!(
                    "animation {ai} sampler {si}: rotation not unit length"
                ));
            }
        }
        let mut targets = BTreeSet::new();
        for (ci, c) in a["channels"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let node = idx(&c["target"]["node"]);
            let path = c["target"]["path"].as_str().unwrap_or("");
            let sampler = idx(&c["sampler"]).and_then(|s| samplers.get(s));
            let comps = sampler
                .and_then(|s| get(&s["output"]))
                .map_or(0, |o| o.components);
            let ok = node.is_some_and(|n| n < nodes.len())
                && matches!((path, comps), ("rotation", 4) | ("translation", 3));
            if !ok {
                issues.push(format!("animation {ai} channel {ci} is invalid"));
            }
            if !targets.insert((node, path.to_owned())) {
                issues.push(format!("animation {ai} channel {ci} repeats a target"));
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

const MAX_COMPONENT: usize = 96;

/// A file-system-safe path component.
fn sanitize(component: &str) -> String {
    let s: String = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        return "_".to_owned();
    }
    let s = if s.len() > MAX_COMPONENT {
        let h = fnv1a64(&[component.as_bytes()]);
        format!("{}_{h:016x}", &s[..MAX_COMPONENT - 17])
    } else {
        s
    };
    let upper = s.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved { format!("_{s}") } else { s }
}

fn relative_stem(package: &str, object_path: &str) -> PathBuf {
    let mut p = PathBuf::from(sanitize(package));
    for part in object_path.split('.') {
        p.push(sanitize(part));
    }
    p
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn lod_rel_paths(stem: &Path, li: usize) -> (PathBuf, PathBuf) {
    let name = stem
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mesh".to_owned());
    let base = if li == 0 {
        name
    } else {
        format!("{name}_LOD{li}")
    };
    (
        stem.with_file_name(format!("{base}.gltf")),
        stem.with_file_name(format!("{base}.bin")),
    )
}

/// Output files already claimed: lower-cased path relative to `skeletal/` ->
/// object path of the mesh that owns it.
type Claims = BTreeMap<String, String>;

/// Files listed by an earlier run's manifest.
fn claims_from_manifest(m: &Manifest) -> Claims {
    let mut c = Claims::new();
    for (key, e) in &m.meshes {
        for l in &e.lods {
            for f in [&l.gltf, &l.bin] {
                c.entry(f.to_ascii_lowercase())
                    .or_insert_with(|| key.clone());
            }
        }
    }
    c
}

/// Largest manifest read back from an earlier run (sanity limit).
const MAX_MANIFEST_BYTES: u64 = 256 << 20;

/// The manifest of an earlier run, or an empty one when there is none.
fn read_manifest(path: &Path) -> Result<Manifest> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_file() => {
            bail!("{} is not a regular file", path.display())
        }
        Ok(m) if m.len() > MAX_MANIFEST_BYTES => {
            bail!(
                "{} is larger than {MAX_MANIFEST_BYTES} bytes",
                path.display()
            )
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Manifest::default()),
        Err(e) => return Err(e).with_context(|| format!("checking {}", path.display())),
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a skeletal manifest", path.display()))?;
    if m.version != MANIFEST_VERSION {
        bail!(
            "{} has manifest version {}, expected {MANIFEST_VERSION} (convert into a fresh directory)",
            path.display(),
            m.version
        );
    }
    Ok(m)
}

/// Output stem of a mesh whose files (`.gltf`/`.bin` of LODs `0..lod_count`)
/// collide with no file claimed by another mesh (in this run or an earlier
/// run's manifest), compared case-insensitively (case-insensitive file
/// systems; sanitising can also merge names; a mesh can be named like another
/// mesh's `_LOD<n>` file). A colliding mesh gets a hash of its object path
/// appended.
fn claim_stem(claimed: &mut Claims, package: &str, object_path: &str, lod_count: usize) -> PathBuf {
    let files = |stem: &Path| -> Vec<String> {
        (0..lod_count.max(1))
            .flat_map(|li| {
                let (g, b) = lod_rel_paths(stem, li);
                [g, b]
            })
            .map(|p| rel_string(&p).to_ascii_lowercase())
            .collect()
    };
    let free = |claimed: &Claims, files: &[String]| {
        files
            .iter()
            .all(|f| claimed.get(f).is_none_or(|owner| owner == object_path))
    };
    let mut stem = relative_stem(package, object_path);
    let name = stem
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let h = fnv1a64(&[object_path.to_ascii_lowercase().as_bytes()]);
    let mut salt = 0u64;
    while !free(claimed, &files(&stem)) {
        let tag = h.wrapping_add(salt);
        stem.set_file_name(format!("{name}~{:08x}", tag >> 32));
        salt = salt.wrapping_add(1 << 32);
    }
    for f in files(&stem) {
        claimed.insert(f, object_path.to_owned());
    }
    stem
}

fn cooked_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.maps_dir))
}

fn package_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

fn prepare_out_dir(out: &Path, input: &Path) -> Result<PathBuf> {
    let root = out.join("skeletal");
    let mut existing = root.clone();
    while !existing.exists() {
        match existing.parent() {
            Some(p) if !p.as_os_str().is_empty() => existing = p.to_path_buf(),
            _ => {
                existing = PathBuf::from(".");
                break;
            }
        }
    }
    safety::check_output_path(&existing.join(".asamu-import-skeletal-probe"), input, false)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    safety::check_output_path(&root.join("manifest.json"), input, true)
        .with_context(|| format!("refusing output directory {}", root.display()))?;
    Ok(root)
}

/// Create `root/rel_dir` one component at a time without following links.
fn ensure_dirs(root: &Path, rel_dir: &Path, input: &Path) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for comp in rel_dir.components() {
        let std::path::Component::Normal(name) = comp else {
            bail!("refusing output path component {:?}", comp.as_os_str());
        };
        let next = cur.join(name);
        match std::fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => bail!(
                "{} exists and is not a directory (links inside the output tree are refused)",
                next.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let checked = safety::check_output_path(&next, input, false)?;
                if let Err(e) = std::fs::create_dir(&checked)
                    && !(e.kind() == std::io::ErrorKind::AlreadyExists
                        && std::fs::symlink_metadata(&checked)
                            .is_ok_and(|m| m.file_type().is_dir()))
                {
                    return Err(e).with_context(|| format!("creating {}", checked.display()));
                }
            }
            Err(e) => return Err(e).with_context(|| format!("checking {}", next.display())),
        }
        cur = next;
    }
    Ok(cur)
}

fn write_file(root: &Path, rel: &Path, data: &[u8], input: &Path, force: bool) -> Result<bool> {
    let Some(name) = rel.file_name() else {
        bail!("output path {} has no file name", rel.display());
    };
    let dir = ensure_dirs(root, rel.parent().unwrap_or(Path::new("")), input)?;
    let target = dir.join(name);
    if !force && std::fs::symlink_metadata(&target).is_ok() {
        return Ok(false);
    }
    let checked = safety::check_output_path(&target, input, force)?;
    safety::write_output(&checked, data, force)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Print how many distinct sequences use each key encoding and how many
/// tracks use each (codec, kind, format).
fn print_format_coverage(g: &Gathered) {
    let mut encodings: BTreeMap<&str, usize> = BTreeMap::new();
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for s in g.sequences.values() {
        let enc = s.seq.info.key_encoding.name();
        *encodings.entry(enc).or_insert(0) += 1;
        for t in &s.seq.tracks {
            for (kind, part) in [("translation", &t.translation), ("rotation", &t.rotation)] {
                let key = match part {
                    Some(p) => format!("{enc} {kind} {}", p.format.name()),
                    None => format!("{enc} {kind} identity (no data)"),
                };
                *formats.entry(key).or_insert(0) += 1;
            }
        }
    }
    println!("sequences by key encoding: {encodings:?}");
    for (k, v) in &formats {
        println!("  tracks {k}: {v}");
    }
}

#[derive(Debug, Default)]
struct RunStats {
    meshes: usize,
    lods: usize,
    vertices: u64,
    triangles: u64,
    animations: usize,
    channels: usize,
    written: usize,
    kept: usize,
    failed: usize,
    invalid: usize,
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    if !(args.scale.is_finite() && args.scale > 0.0) {
        bail!("--scale must be a positive finite number");
    }
    let (cooked, maps) = cooked_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps];
    let files = package_files(&dirs);
    if files.is_empty() {
        bail!("no packages found under {}", cooked.display());
    }
    let mut g = Gathered::default();
    for file in &files {
        // A fresh set per package keeps memory bounded (maps are large).
        let set = PackageSet::new(&dirs);
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        gather_package(&mut g, &lp, &set);
    }
    for f in &g.failures {
        eprintln!("asamu-import: decode failure: {f}");
    }
    let (pairs, unpaired) = pair_sets(&g);
    print_format_coverage(&g);
    let root = if args.check {
        None
    } else {
        Some(prepare_out_dir(&ctx.out, &cooked)?)
    };
    // A filtered run (--package/--name/--limit) updates its meshes and keeps
    // the other entries of an earlier run's manifest.
    let mut manifest = match &root {
        Some(r) => read_manifest(&r.join("manifest.json"))?,
        None => Manifest::default(),
    };
    let mut stats = RunStats {
        failed: g.failures.len(),
        ..RunStats::default()
    };
    let opts = BuildOptions {
        scale: args.scale,
        resample: args.resample,
    };
    let mut claimed = claims_from_manifest(&manifest);
    for (path, mi) in &g.meshes {
        if !args.packages.is_empty()
            && !args.packages.iter().any(|f| {
                mi.package
                    .to_ascii_lowercase()
                    .contains(&f.to_ascii_lowercase())
            })
        {
            continue;
        }
        if let Some(filter) = &args.name
            && !path
                .to_ascii_lowercase()
                .contains(&filter.to_ascii_lowercase())
        {
            continue;
        }
        if args.limit.is_some_and(|l| stats.meshes >= l) {
            break;
        }
        let structural = validate_skeletal_mesh(&mi.mesh.native, Some(&mi.bone_names), &mi.vctx);
        if !structural.is_empty() {
            stats.failed += 1;
            eprintln!("asamu-import: {path}: {}", structural.join("; "));
            continue;
        }
        let lod_count = if args.all_lods {
            mi.mesh.native.lods.len()
        } else {
            1
        };
        let stem = claim_stem(&mut claimed, &mi.package, path, lod_count);
        let set_list: Vec<(String, PairingSource)> = if args.no_animations {
            Vec::new()
        } else {
            pairs.get(path).cloned().unwrap_or_default()
        };
        let anim_inputs: Vec<AnimInput<'_>> = set_list
            .iter()
            .filter_map(|(sp, _)| g.sets.get(sp))
            .map(|set| AnimInput {
                set,
                sequences: set
                    .sequences
                    .iter()
                    .flatten()
                    .filter_map(|p| g.sequences.get(p))
                    .map(|s| &s.seq)
                    .collect(),
            })
            .collect();
        let mut lods = Vec::new();
        let mut written_files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let mut anim_entries: Vec<AnimSetEntry> = Vec::new();
        let mut ok = true;
        let mut invalid = false;
        for (li, lod) in mi.mesh.native.lods.iter().take(lod_count).enumerate() {
            let (gltf_rel, bin_rel) = lod_rel_paths(&stem, li);
            let bin_name = bin_rel
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut seq_entries = Vec::new();
            let anims: &[AnimInput<'_>] = if li == 0 { &anim_inputs } else { &[] };
            let asset = match build_gltf(
                &mi.mesh,
                &mi.bone_names,
                &mi.materials,
                &mi.sockets,
                lod,
                anims,
                &opts,
                &bin_name,
                &mut seq_entries,
            ) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("asamu-import: {path} LOD {li}: {e:#}");
                    ok = false;
                    break;
                }
            };
            let text = serde_json::to_string_pretty(&asset.json)?;
            let reparsed: Json = serde_json::from_str(&text)?;
            let issues = validate_gltf(&reparsed, &asset.bin);
            if !issues.is_empty() {
                // Never write a document that fails validation.
                eprintln!(
                    "asamu-import: {path} LOD {li}: glTF validation: {}",
                    issues.join("; ")
                );
                invalid = true;
                break;
            }
            if li == 0 {
                for ((sp, src), entries) in set_list.iter().zip(seq_entries) {
                    let set = g.sets.get(sp);
                    stats.animations += entries.len();
                    stats.channels += entries.iter().map(|e| e.channels).sum::<usize>();
                    anim_entries.push(AnimSetEntry {
                        path: sp.clone(),
                        source: *src,
                        match_ratio: set.map_or(0.0, |s| match_ratio(s, &mi.bone_names)),
                        anim_rotation_only: set.is_some_and(|s| s.anim_rotation_only),
                        sequences: entries,
                    });
                }
            }
            stats.lods += 1;
            stats.vertices += u64::try_from(asset.stats.vertices).unwrap_or(0);
            stats.triangles += asset.stats.triangles;
            lods.push(LodEntry {
                lod: li,
                gltf: rel_string(&gltf_rel),
                bin: rel_string(&bin_rel),
                stats: asset.stats,
            });
            written_files.push((gltf_rel, text.into_bytes()));
            written_files.push((bin_rel, asset.bin));
        }
        if invalid {
            stats.invalid += 1;
            continue;
        }
        if !ok {
            stats.failed += 1;
            continue;
        }
        stats.meshes += 1;
        let parts: Vec<&[u8]> = written_files.iter().map(|(_, d)| d.as_slice()).collect();
        let hash = format!("{:016x}", fnv1a64(&parts));
        if let Some(root) = &root {
            let mut wrote = false;
            for (rel, data) in &written_files {
                wrote |= write_file(root, rel, data, &cooked, args.force)?;
            }
            if wrote {
                stats.written += 1;
            } else {
                stats.kept += 1;
            }
        }
        let n = &mi.mesh.native;
        manifest.meshes.insert(
            path.clone(),
            MeshEntry {
                package: mi.package.clone(),
                export_index: mi.export_index,
                also_in: mi.also_in.clone(),
                scale: args.scale,
                bones: mi.bone_names.clone(),
                materials: mi.materials.clone(),
                origin: n.origin,
                rot_origin: n.rot_origin,
                bounds_ue: (n.bounds.origin, n.bounds.box_extent, n.bounds.sphere_radius),
                sockets: mi.sockets.clone(),
                lod_count: n.lods.len(),
                lods,
                anim_sets: anim_entries,
                content_hash: hash,
            },
        );
    }
    manifest.unpaired_anim_sets = unpaired;
    let mode = if args.check { "checked" } else { "converted" };
    println!(
        "skeletal meshes: {} {mode} ({} LODs, {} vertices, {} triangles, {} animations, {} channels); \
         {} written, {} kept; {} failed, {} invalid glTF; anim sets {} ({} unpaired), sequences {}",
        stats.meshes,
        stats.lods,
        stats.vertices,
        stats.triangles,
        stats.animations,
        stats.channels,
        stats.written,
        stats.kept,
        stats.failed,
        stats.invalid,
        g.sets.len(),
        manifest.unpaired_anim_sets.len(),
        g.sequences.len()
    );
    if let Some(root) = &root {
        manifest.version = MANIFEST_VERSION;
        manifest.notice = NOTICE.to_owned();
        manifest.coordinates = COORDINATES.to_owned();
        let json = serde_json::to_string_pretty(&manifest)?;
        let manifest_path = root.join("manifest.json");
        let target = safety::check_output_path(&manifest_path, &cooked, true)?;
        safety::write_output(&target, json.as_bytes(), true)?;
        println!(
            "manifest: {} meshes at {}",
            manifest.meshes.len(),
            manifest_path.display()
        );
        println!(
            "note: converted data is copyrighted game data; keep it local, never redistribute"
        );
    }
    if stats.failed > 0 || stats.invalid > 0 {
        bail!(
            "{} skeletal meshes failed to convert, {} failed glTF validation",
            stats.failed,
            stats.invalid
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaternion_axis_map_matches_the_vector_axis_map() {
        // Rotating in UE3 space then mapping equals mapping then rotating.
        let q = qnormalize([0.3, -0.5, 0.2, 0.75]);
        let v = [1.5, -2.0, 0.25];
        let ue = rotate(q, v);
        let mapped = [ue[1], ue[2], -ue[0]];
        let gq = f64q(ue_quat_to_gltf(q.map(|c| c as f32)));
        let gv = rotate(gq, [v[1], v[2], -v[0]]);
        for k in 0..3 {
            assert!((mapped[k] - gv[k]).abs() < 1e-6, "{mapped:?} vs {gv:?}");
        }
    }

    #[test]
    fn rotator_conversion_follows_the_engine_matrix() {
        // Yaw +90 degrees turns +X into +Y in UE3.
        let q = rotator_to_quat(0, 16384, 0);
        let v = rotate(q, [1.0, 0.0, 0.0]);
        assert!((v[0]).abs() < 1e-9 && (v[1] - 1.0).abs() < 1e-9, "{v:?}");
        // Pitch +90 degrees turns +X into +Z.
        let q = rotator_to_quat(16384, 0, 0);
        let v = rotate(q, [1.0, 0.0, 0.0]);
        assert!((v[2] - 1.0).abs() < 1e-9, "{v:?}");
        // Roll +90 degrees turns +Y into +Z? Per FRotationMatrix row 1 =
        // (0, CR, -SR)... the Y axis maps to row 1 of M: (0, 0, -1)·... check
        // against the matrix directly.
        let q = rotator_to_quat(0, 0, 16384);
        let v = rotate(q, [0.0, 1.0, 0.0]);
        assert!((v[2] + 1.0).abs() < 1e-9, "{v:?}");
    }

    #[test]
    fn rigid_inverse_and_composition() {
        let a = Rigid {
            t: [1.0, 2.0, 3.0],
            r: qnormalize([0.1, 0.2, 0.3, 0.9]),
        };
        let id = a.then_child(&a.inverse());
        assert!(len(id.t) < 1e-9);
        assert!((id.r[3].abs() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sampler_keys_keep_the_engine_behaviour_of_shared_times() {
        // A run at the final time: the first key just before it (the engine
        // interpolates towards it), the last key at it.
        let k = sampler_keys(vec![(0.0, 1), (1.0, 2), (2.0, 3), (2.0, 4)]);
        assert_eq!(
            k,
            vec![(0.0, 1), (1.0, 2), (2.0f32.next_down(), 3), (2.0, 4)]
        );
        // Keys inside a run are never sampled by the engine.
        let k = sampler_keys(vec![(0.0, 1), (1.0, 2), (1.0, 3), (1.0, 4), (2.0, 5)]);
        assert_eq!(
            k,
            vec![(0.0, 1), (1.0f32.next_down(), 2), (1.0, 4), (2.0, 5)]
        );
        // A run at time 0: the engine returns the first key at 0 exactly.
        let k = sampler_keys(vec![(0.0, 1), (0.0, 2), (1.0, 3)]);
        assert_eq!(k, vec![(0.0, 1), (0.0f32.next_up(), 2), (1.0, 3)]);
        // Malformed times: non-finite or negative become 0 (later key wins),
        // backwards times are clamped; the result is always a valid input.
        let k = sampler_keys(vec![(f32::NAN, 1), (-1.0, 2), (0.5, 3), (0.25, 4)]);
        assert!(k.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(k.iter().all(|x| x.0.is_finite() && x.0 >= 0.0));
        assert_eq!(k.last().map(|x| x.1), Some(4));
        assert!(sampler_keys::<u8>(Vec::new()).is_empty());
        // No room before the shared time: the previous key is kept.
        let t = 1.0f32;
        let k = sampler_keys(vec![(t.next_down(), 1), (t, 2), (t, 3)]);
        assert_eq!(k, vec![(t.next_down(), 1), (t, 3)]);
    }

    #[test]
    fn resampling_is_bounded() {
        assert_eq!(sample_times(1.0, i32::MAX).len(), MAX_RESAMPLE_FRAMES);
        assert_eq!(sample_times(1.0, i32::MIN), vec![0.0]);
        assert_eq!(sample_times(f32::NAN, 1), vec![0.0]);
    }

    #[test]
    fn output_names_never_collide() {
        let mut claimed = Claims::new();
        let a = claim_stem(&mut claimed, "Pkg", "A.Mesh", 2);
        // Same path in another case, and a name that sanitises the same.
        let b = claim_stem(&mut claimed, "Pkg", "a.mesh", 1);
        let c = claim_stem(&mut claimed, "Pkg", "A.Me sh", 1);
        let c2 = claim_stem(&mut claimed, "Pkg", "A.Me?sh", 1);
        // A mesh literally named like another mesh's LOD file.
        let d = claim_stem(&mut claimed, "Pkg", "A.Mesh_LOD1", 1);
        let all = [&a, &b, &c, &c2, &d];
        let mut files = BTreeSet::new();
        for (i, stem) in all.iter().enumerate() {
            let lods = if i == 0 { 2 } else { 1 };
            for li in 0..lods {
                let (g, bin) = lod_rel_paths(stem, li);
                assert!(files.insert(rel_string(&g).to_ascii_lowercase()), "{g:?}");
                assert!(
                    files.insert(rel_string(&bin).to_ascii_lowercase()),
                    "{bin:?}"
                );
            }
        }
        assert_eq!(rel_string(&a), "Pkg/A/Mesh");
        // A later run keeps each mesh's own files (claims from the manifest).
        assert_eq!(claim_stem(&mut claimed, "Pkg", "A.Mesh", 2), a);
        assert_eq!(claim_stem(&mut claimed, "Pkg", "a.mesh", 1), b);
    }

    #[test]
    fn manifests_merge_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        assert_eq!(read_manifest(&path).unwrap(), Manifest::default());
        let entry = |gltf: &str| MeshEntry {
            package: "Pkg".to_owned(),
            export_index: 1,
            also_in: Vec::new(),
            scale: 1.0,
            bones: vec!["Root".to_owned()],
            materials: Vec::new(),
            origin: [0.0; 3],
            rot_origin: [0; 3],
            bounds_ue: ([0.0; 3], [1.0; 3], 1.0),
            sockets: Vec::new(),
            lod_count: 1,
            lods: vec![LodEntry {
                lod: 0,
                gltf: format!("{gltf}.gltf"),
                bin: format!("{gltf}.bin"),
                stats: LodStats::default(),
            }],
            anim_sets: Vec::new(),
            content_hash: String::new(),
        };
        let mut m = Manifest {
            version: MANIFEST_VERSION,
            ..Manifest::default()
        };
        m.meshes.insert("A.Mesh".to_owned(), entry("Pkg/A/Mesh"));
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let back = read_manifest(&path).unwrap();
        assert_eq!(back, m);
        // Another mesh cannot take A.Mesh's files; A.Mesh keeps them.
        let mut claims = claims_from_manifest(&back);
        assert_ne!(
            rel_string(&claim_stem(&mut claims, "Pkg", "a.mesh", 1)),
            "Pkg/A/Mesh"
        );
        assert_eq!(
            rel_string(&claim_stem(&mut claims, "Pkg", "A.Mesh", 1)),
            "Pkg/A/Mesh"
        );
        // Not a manifest, another version, or not a regular file: refused.
        std::fs::write(&path, b"[1, 2]").unwrap();
        assert!(read_manifest(&path).is_err());
        m.version = MANIFEST_VERSION + 1;
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(read_manifest(&path).is_err());
        assert!(read_manifest(dir.path()).is_err());
    }

    #[test]
    fn sanitize_rejects_device_names_and_separators() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("a/b.c"), "a_b_c");
        assert_eq!(sanitize(""), "_");
        assert!(sanitize(&"x".repeat(300)).len() <= MAX_COMPONENT);
    }

    // -----------------------------------------------------------------------
    // Behavioural check: an independent glTF skinning evaluator against the
    // engine's pose rules evaluated in UE3 space.
    // -----------------------------------------------------------------------

    use asamu_ue3::FName;
    use asamu_ue3::anim::{
        AnimSequenceInfo, AnimSequenceNative, BoneTrack, Codec, CompressionFormat, KeyData,
        KeyEncoding, TrackKind,
    };
    use asamu_ue3::object::{DecodedObject, ObjectPrelude};
    use asamu_ue3::skeletal::{
        GpuSkinVertex, GpuSkinVertexBuffer, MeshBone, MultiSizeIndices, SkelChunk, SkelSection,
        SkeletalMeshNative,
    };
    use asamu_ue3::staticmesh::{BoxSphereBounds, PackedNormal};
    use std::f64::consts::FRAC_1_SQRT_2;

    fn object(path: &str) -> DecodedObject {
        DecodedObject {
            export_index: 0,
            path: path.to_owned(),
            class: "Engine.Test".to_owned(),
            prelude: ObjectPrelude {
                shadow_map_len: None,
                state_frame: None,
                component: None,
                net_index: 0,
            },
            properties: Vec::new(),
            properties_end: 0,
            payload_size: 0,
            warnings: Vec::new(),
        }
    }

    fn accessor(doc: &Json, bin: &[u8], i: usize) -> Vec<Vec<f64>> {
        let a = &doc["accessors"][i];
        let v = &doc["bufferViews"][idx(&a["bufferView"]).unwrap()];
        let off = idx(&v["byteOffset"]).unwrap();
        let comps = match a["type"].as_str().unwrap() {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => 16,
        };
        let ct = a["componentType"].as_u64().unwrap();
        let cs = match ct {
            5121 => 1,
            5123 => 2,
            _ => 4,
        };
        let norm = a["normalized"].as_bool().unwrap_or(false);
        let count = idx(&a["count"]).unwrap();
        (0..count)
            .map(|e| {
                (0..comps)
                    .map(|k| {
                        let at = off + (e * comps + k) * cs;
                        let b = &bin[at..at + cs];
                        match (ct, cs) {
                            (5126, _) => f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                            (_, 1) => f64::from(b[0]) / if norm { 255.0 } else { 1.0 },
                            (_, 2) => f64::from(u16::from_le_bytes([b[0], b[1]])),
                            _ => f64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// How rotation keys are blended when sampling a document.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Blend {
        /// glTF 2.0 LINEAR rotation semantics: spherical linear interpolation.
        Slerp,
        /// The engine's normalised lerp (isolates key placement from the
        /// slerp-vs-nlerp difference).
        Nlerp,
    }

    fn slerp(a: Q, b: Q, t: f64) -> Q {
        let mut b = b;
        let mut d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
        if d < 0.0 {
            d = -d;
            b = b.map(|c| -c);
        }
        if d > 0.9999 {
            return qnormalize(std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t));
        }
        let th = d.min(1.0).acos();
        let (wa, wb) = (((1.0 - t) * th).sin() / th.sin(), (t * th).sin() / th.sin());
        std::array::from_fn(|k| a[k] * wa + b[k] * wb)
    }

    fn eval_gltf(doc: &Json, bin: &[u8], anim: Option<usize>, t: f32) -> Vec<V3> {
        eval_gltf_with(doc, bin, anim, t, Blend::Slerp)
    }

    /// Skinned positions of the first primitive at time `t` of animation
    /// `anim`, evaluated with glTF semantics (translations lerp, rotations as
    /// `blend`; clamped outside the key range; the skinned mesh node's own
    /// transform ignored; `joint global x inverse bind`).
    fn eval_gltf_with(
        doc: &Json,
        bin: &[u8],
        anim: Option<usize>,
        t: f32,
        blend: Blend,
    ) -> Vec<V3> {
        let nodes = doc["nodes"].as_array().unwrap();
        let mut local: Vec<Rigid> = nodes
            .iter()
            .map(|n| {
                let tr = n
                    .get("translation")
                    .and_then(json_vec)
                    .unwrap_or(vec![0.0; 3]);
                let r = n
                    .get("rotation")
                    .and_then(json_vec)
                    .unwrap_or(vec![0.0, 0.0, 0.0, 1.0]);
                Rigid {
                    t: [tr[0], tr[1], tr[2]],
                    r: [r[0], r[1], r[2], r[3]],
                }
            })
            .collect();
        if let Some(ai) = anim {
            let a = &doc["animations"][ai];
            for c in a["channels"].as_array().unwrap() {
                let s = &a["samplers"][idx(&c["sampler"]).unwrap()];
                let times: Vec<f64> = accessor(doc, bin, idx(&s["input"]).unwrap())
                    .into_iter()
                    .map(|v| v[0])
                    .collect();
                let vals = accessor(doc, bin, idx(&s["output"]).unwrap());
                let t = f64::from(t);
                let (k0, k1, alpha) = if t <= times[0] {
                    (0, 0, 0.0)
                } else if t >= times[times.len() - 1] {
                    (times.len() - 1, times.len() - 1, 0.0)
                } else {
                    let k = times.iter().rposition(|&x| x <= t).unwrap();
                    (k, k + 1, (t - times[k]) / (times[k + 1] - times[k]))
                };
                let node = idx(&c["target"]["node"]).unwrap();
                let lerp: Vec<f64> = (0..vals[k0].len())
                    .map(|i| vals[k0][i] * (1.0 - alpha) + vals[k1][i] * alpha)
                    .collect();
                match c["target"]["path"].as_str().unwrap() {
                    "rotation" if blend == Blend::Slerp => {
                        let q = |v: &[f64]| [v[0], v[1], v[2], v[3]];
                        local[node].r = qnormalize(slerp(q(&vals[k0]), q(&vals[k1]), alpha));
                    }
                    "rotation" => local[node].r = qnormalize([lerp[0], lerp[1], lerp[2], lerp[3]]),
                    _ => local[node].t = [lerp[0], lerp[1], lerp[2]],
                }
            }
        }
        let mut parent = vec![None; nodes.len()];
        for (ni, n) in nodes.iter().enumerate() {
            for c in n["children"].as_array().cloned().unwrap_or_default() {
                parent[idx(&c).unwrap()] = Some(ni);
            }
        }
        let global = |mut i: usize| {
            let mut chain = vec![i];
            while let Some(p) = parent[i] {
                chain.push(p);
                i = p;
            }
            chain
                .iter()
                .rev()
                .fold(Rigid::IDENTITY, |acc, &n| acc.then_child(&local[n]))
        };
        let skin = &doc["skins"][0];
        let joints: Vec<usize> = skin["joints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| idx(j).unwrap())
            .collect();
        let ibm = accessor(doc, bin, idx(&skin["inverseBindMatrices"]).unwrap());
        let prim = &doc["meshes"][0]["primitives"][0]["attributes"];
        let pos = accessor(doc, bin, idx(&prim["POSITION"]).unwrap());
        let jts = accessor(doc, bin, idx(&prim["JOINTS_0"]).unwrap());
        let wts = accessor(doc, bin, idx(&prim["WEIGHTS_0"]).unwrap());
        let mats: Vec<[f64; 16]> = joints
            .iter()
            .enumerate()
            .map(|(k, &j)| {
                let g = global(j).to_matrix();
                let m = &ibm[k];
                let mut out = [0.0; 16];
                for col in 0..4 {
                    for row in 0..4 {
                        out[col * 4 + row] = (0..4).map(|x| g[x * 4 + row] * m[col * 4 + x]).sum();
                    }
                }
                out
            })
            .collect();
        pos.iter()
            .enumerate()
            .map(|(vi, p)| {
                let mut acc = [0.0; 3];
                for k in 0..4 {
                    let w = wts[vi][k];
                    let m = &mats[jts[vi][k] as usize];
                    for row in 0..3 {
                        acc[row] += w
                            * (m[row] * p[0] + m[4 + row] * p[1] + m[8 + row] * p[2] + m[12 + row]);
                    }
                }
                acc
            })
            .collect()
    }

    /// Engine-side evaluation in UE3 space, mapped to glTF axes.
    fn eval_engine(
        mesh: &SkeletalMesh,
        names: &[String],
        set: Option<(&AnimSetInfo, &AnimSequence)>,
        t: f32,
        scale: f64,
    ) -> Vec<V3> {
        let n = &mesh.native;
        let reference: Vec<Rigid> = n
            .ref_skeleton
            .iter()
            .map(|b| Rigid {
                t: f64v(b.position),
                r: qnormalize(f64q(b.orientation)),
            })
            .collect();
        let pose: Vec<Rigid> = (0..reference.len())
            .map(|bi| {
                let Some((s, seq)) = set else {
                    return reference[bi];
                };
                let Some(ti) = s
                    .track_bone_names
                    .iter()
                    .position(|x| x.eq_ignore_ascii_case(&names[bi]))
                else {
                    return reference[bi];
                };
                let tr = &seq.tracks[ti];
                let len = seq.info.sequence_length;
                let nf = seq.info.num_frames;
                let mut q = tr
                    .rotation
                    .as_ref()
                    .map_or([0.0, 0.0, 0.0, 1.0], |r| sample_rotation(r, t, len, nf));
                if bi != 0 {
                    q[3] = -q[3];
                }
                let translation = if s.uses_anim_translation(ti, bi == 0) {
                    f64v(
                        tr.translation
                            .as_ref()
                            .map_or([0.0; 3], |x| sample_translation(x, t, len, nf)),
                    )
                } else {
                    reference[bi].t
                };
                Rigid {
                    t: translation,
                    r: qnormalize(f64q(q)),
                }
            })
            .collect();
        let compose = |locals: &[Rigid]| {
            let mut g: Vec<Rigid> = Vec::new();
            for (i, l) in locals.iter().enumerate() {
                let parent = n.ref_skeleton[i].parent_index as usize;
                g.push(if i == 0 { *l } else { g[parent].then_child(l) });
            }
            g
        };
        let bind = compose(&reference);
        let posed = compose(&pose);
        let origin = mesh_origin_rigid(mesh);
        let lod = &n.lods[0];
        lod.vertex_buffer
            .vertices
            .iter()
            .enumerate()
            .map(|(vi, v)| {
                let chunk = lod.chunks.iter().find(|c| {
                    let lo = c.base_vertex_index as usize;
                    vi >= lo && vi < lo + c.vertex_count().unwrap() as usize
                });
                let chunk = chunk.unwrap();
                let p = f64v(v.position);
                let mut acc = [0.0; 3];
                for k in 0..4 {
                    let w = f64::from(v.influence_weights[k]) / 255.0;
                    if w == 0.0 {
                        continue;
                    }
                    let b = usize::from(chunk.bone_map[usize::from(v.influence_bones[k])]);
                    let local = bind[b]
                        .inverse()
                        .then_child(&Rigid {
                            t: p,
                            r: [0.0, 0.0, 0.0, 1.0],
                        })
                        .t;
                    let world = posed[b]
                        .then_child(&Rigid {
                            t: local,
                            r: [0.0, 0.0, 0.0, 1.0],
                        })
                        .t;
                    for c in 0..3 {
                        acc[c] += w * world[c];
                    }
                }
                let placed = origin
                    .then_child(&Rigid {
                        t: acc,
                        r: [0.0, 0.0, 0.0, 1.0],
                    })
                    .t;
                [placed[1] * scale, placed[2] * scale, -placed[0] * scale]
            })
            .collect()
    }

    fn max_dist(a: &[V3], b: &[V3]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| len([x[0] - y[0], x[1] - y[1], x[2] - y[2]]))
            .fold(0.0, f64::max)
    }

    fn qn(q: [f64; 4]) -> [f32; 4] {
        qnormalize(q).map(|c| c as f32)
    }

    /// A three-bone mesh with non-trivial reference rotations, two chunks
    /// worth of vertices in one chunk, and a yawed mesh origin.
    fn synthetic_mesh() -> (SkeletalMesh, Vec<String>) {
        let bone = |name: i32, parent: i32, children: i32, r: [f64; 4], t: [f32; 3]| MeshBone {
            name: FName {
                index: name,
                number: 0,
            },
            flags: 0,
            orientation: qn(r),
            position: t,
            num_children: children,
            parent_index: parent,
            bone_color: [0; 4],
        };
        let bones = vec![
            bone(
                0,
                0,
                1,
                [0.0, 0.0, -FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
                [0.0, 0.0, 0.0],
            ),
            bone(1, 0, 1, [0.1, -0.2, 0.3, -0.9], [0.0, 0.0, 40.0]),
            bone(2, 1, 0, [0.5, 0.1, -0.2, -0.8], [25.0, 3.0, 0.0]),
        ];
        let vert = |p: [f32; 3], bones: [u8; 4], weights: [u8; 4]| GpuSkinVertex {
            tangent_x: PackedNormal([255, 128, 128, 128]),
            tangent_z: PackedNormal([128, 128, 255, 255]),
            influence_bones: bones,
            influence_weights: weights,
            position: p,
            uvs: [[0.25, 0.5], [0.0; 2], [0.0; 2], [0.0; 2]],
        };
        let vertices = vec![
            vert([0.0, 0.0, 0.0], [0, 0, 0, 0], [255, 0, 0, 0]),
            vert([5.0, 0.0, 35.0], [0, 1, 0, 0], [128, 127, 0, 0]),
            vert([10.0, 5.0, 45.0], [1, 0, 0, 0], [255, 0, 0, 0]),
            vert([20.0, -5.0, 60.0], [1, 2, 0, 0], [200, 55, 0, 0]),
            vert([30.0, 0.0, 70.0], [2, 1, 0, 0], [230, 25, 0, 0]),
            vert([25.0, 10.0, 65.0], [2, 0, 0, 0], [255, 0, 0, 0]),
        ];
        let lod = SkelLodModel {
            sections: vec![SkelSection {
                material_index: 0,
                chunk_index: 0,
                base_index: 0,
                num_triangles: 4,
                triangle_sorting: 0,
            }],
            indices: MultiSizeIndices {
                needs_cpu_access: 0,
                data_type_size: 2,
                indices: vec![0, 1, 2, 1, 3, 2, 3, 4, 5, 2, 3, 5],
            },
            active_bone_indices: vec![0, 1, 2],
            chunks: vec![SkelChunk {
                base_vertex_index: 0,
                rigid_vertices: Vec::new(),
                soft_vertices: Vec::new(),
                bone_map: vec![0, 1, 2],
                num_rigid_vertices: 3,
                num_soft_vertices: 3,
                max_bone_influences: 2,
            }],
            size: 0,
            num_vertices: 6,
            required_bones: vec![0, 1, 2],
            raw_point_indices_record: asamu_ue3::bulkdata::BulkDataRecord {
                flags: 0,
                element_count: 0,
                size_on_disk: 0,
                offset_in_file: 0,
                header_offset: 0,
            },
            raw_point_indices: Vec::new(),
            num_tex_coords: 1,
            vertex_buffer: GpuSkinVertexBuffer {
                num_tex_coords: 1,
                use_full_precision_uvs: false,
                use_packed_position: false,
                mesh_extension: [0.0; 3],
                mesh_origin: [0.0; 3],
                vertices,
            },
            colors: None,
            vertex_influences: Vec::new(),
            adjacency: MultiSizeIndices {
                needs_cpu_access: 0,
                data_type_size: 2,
                indices: Vec::new(),
            },
        };
        let native = SkeletalMeshNative {
            start: 0,
            bounds: BoxSphereBounds {
                origin: [0.0; 3],
                box_extent: [100.0; 3],
                sphere_radius: 173.0,
            },
            materials: vec![PackageIndex(0)],
            origin: [1.0, -2.0, 3.0],
            rot_origin: [0, -16384, 0],
            ref_skeleton: bones,
            skeletal_depth: 3,
            lods: vec![lod],
            name_index_map: Vec::new(),
            per_poly_bone_kdops: Vec::new(),
            bone_break_names: Vec::new(),
            bone_break_options: Vec::new(),
            clothing_assets: vec![PackageIndex(0)],
            cached_streaming_texture_factors: Vec::new(),
            source_data: None,
        };
        let mesh = SkeletalMesh {
            object: object("Test.Mesh"),
            has_vertex_colors: false,
            native,
        };
        (
            mesh,
            vec!["Root".to_owned(), "Spine".to_owned(), "Arm".to_owned()],
        )
    }

    fn rot_track(keys: &[[f64; 4]], frames: Option<Vec<u16>>) -> CompressedTrack {
        // Float96NoW stores x, y, z with w >= 0.
        let data: Vec<f32> = keys
            .iter()
            .flat_map(|q| {
                let q = qnormalize(*q);
                let q = if q[3] < 0.0 { q.map(|c| -c) } else { q };
                [q[0] as f32, q[1] as f32, q[2] as f32]
            })
            .collect();
        CompressedTrack {
            kind: TrackKind::Rotation,
            codec: Codec::PerTrack,
            offset: 0,
            format: CompressionFormat::Float96NoW,
            component_mask: 7,
            num_keys: keys.len(),
            header: Vec::new(),
            components_per_key: 3,
            data: KeyData::F32(data),
            has_frame_table: frames.is_some(),
            frames: frames.unwrap_or_default(),
            end: 0,
            padding: Vec::new(),
        }
    }

    fn trans_track(keys: &[[f32; 3]]) -> CompressedTrack {
        CompressedTrack {
            kind: TrackKind::Translation,
            codec: Codec::Legacy,
            offset: 0,
            format: CompressionFormat::None,
            component_mask: 0,
            num_keys: keys.len(),
            header: Vec::new(),
            components_per_key: 3,
            data: KeyData::F32(keys.iter().flatten().copied().collect()),
            has_frame_table: false,
            frames: Vec::new(),
            end: 0,
            padding: Vec::new(),
        }
    }

    fn synthetic_anim(rotation_only: bool) -> (AnimSetInfo, AnimSequence) {
        let set = AnimSetInfo {
            path: "Test.Set".to_owned(),
            track_bone_names: vec!["Spine".to_owned(), "Root".to_owned(), "Arm".to_owned()],
            sequences: vec![Some("Test.Set.Seq".to_owned())],
            sequence_indices: vec![1],
            anim_rotation_only: rotation_only,
            use_translation_bone_names: Vec::new(),
            force_mesh_translation_bone_names: Vec::new(),
            preview_skel_mesh_name: None,
            best_ratio_skel_mesh_name: None,
        };
        let tracks = vec![
            BoneTrack {
                translation: Some(trans_track(&[
                    [0.0, 0.0, 40.0],
                    [0.0, 2.0, 41.0],
                    [0.0, 4.0, 39.0],
                ])),
                rotation: Some(rot_track(
                    &[
                        [0.1, -0.2, 0.3, 0.9],
                        [0.3, -0.1, 0.2, 0.9],
                        [0.0, 0.2, 0.1, 0.95],
                    ],
                    None,
                )),
            },
            BoneTrack {
                translation: Some(trans_track(&[
                    [0.0, 0.0, 0.0],
                    [5.0, 0.0, 1.0],
                    [10.0, 0.0, 0.0],
                ])),
                rotation: Some(rot_track(
                    &[
                        [0.0, 0.0, -FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
                        [0.0, 0.1, -0.7, -0.7],
                    ],
                    None,
                )),
            },
            BoneTrack {
                translation: None,
                rotation: Some(rot_track(
                    &[
                        [0.5, 0.1, -0.2, 0.8],
                        [0.4, 0.3, -0.1, 0.8],
                        [0.6, 0.0, -0.3, 0.7],
                    ],
                    Some(vec![0, 7, 10]),
                )),
            },
        ];
        let info = AnimSequenceInfo {
            sequence_name: "Wave".to_owned(),
            num_frames: 11,
            sequence_length: 1.0,
            rate_scale: 1.0,
            no_looping_interpolation: false,
            is_additive: false,
            translation_format: CompressionFormat::None,
            rotation_format: CompressionFormat::Float96NoW,
            key_encoding: KeyEncoding::PerTrackCompression,
            compressed_track_offsets: Vec::new(),
            notifies: Vec::new(),
            compression_scheme: None,
            additive_ref_name: None,
            encoding_pkg_version: 0,
        };
        let seq = AnimSequence {
            object: object("Test.Set.Seq"),
            info,
            native: AnimSequenceNative {
                start: 0,
                raw_tracks: Vec::new(),
                compressed: Vec::new(),
            },
            tracks,
        };
        (set, seq)
    }

    /// How [`check_mesh_against_engine`] exports and samples.
    #[derive(Debug, Clone, Copy)]
    struct Mode {
        /// `--resample` (every frame) instead of the stored keys.
        resample: bool,
        /// Rotation blending of the glTF evaluator.
        blend: Blend,
        /// Also compare at times between frames.
        between_frames: bool,
    }

    /// Export `mesh` with `anims`, validate the document and compare the
    /// glTF-evaluated skin with the engine-rule evaluation: at the reference
    /// pose, at up to `max_frames` frames of every animation (always the
    /// first, the last and the one before it, which lies in the final
    /// interval) and, with `between_frames`, at four times between frames.
    /// Returns the largest distance seen.
    #[allow(clippy::too_many_arguments)]
    fn check_mesh_against_engine(
        mesh: &SkeletalMesh,
        names: &[String],
        anims: &[(AnimSetInfo, AnimSequence)],
        scale: f32,
        tolerance: f64,
        max_frames: i32,
        mode: Mode,
    ) -> f64 {
        let inputs: Vec<AnimInput<'_>> = anims
            .iter()
            .map(|(s, q)| AnimInput {
                set: s,
                sequences: vec![q],
            })
            .collect();
        let opts = BuildOptions {
            scale,
            resample: mode.resample,
        };
        let mut entries = Vec::new();
        let asset = build_gltf(
            mesh,
            names,
            &[],
            &[],
            &mesh.native.lods[0],
            &inputs,
            &opts,
            "x.bin",
            &mut entries,
        )
        .unwrap();
        let text = serde_json::to_string(&asset.json).unwrap();
        let doc: Json = serde_json::from_str(&text).unwrap();
        let issues = validate_gltf(&doc, &asset.bin);
        assert!(issues.is_empty(), "{issues:?}");
        let scale = f64::from(scale);
        // Reference pose.
        let a = eval_gltf_with(&doc, &asset.bin, None, 0.0, mode.blend);
        let b = eval_engine(mesh, names, None, 0.0, scale);
        let mut worst = max_dist(&a, &b);
        assert!(worst < tolerance, "reference pose differs by {worst}");
        for (ai, (set, seq)) in anims.iter().enumerate() {
            let nf = seq.info.num_frames.max(1);
            let len = seq.info.sequence_length;
            let step = (nf / max_frames.max(1)).max(1);
            let mut rels: Vec<f32> = (0..nf)
                .step_by(usize::try_from(step).unwrap())
                .chain([nf - 2, nf - 1])
                .filter(|&f| f >= 0)
                .map(|f| {
                    if nf > 1 {
                        f as f32 / (nf - 1) as f32
                    } else {
                        0.0
                    }
                })
                .collect();
            if mode.between_frames {
                rels.extend([0.137, 0.5013, 0.871, 0.9993]);
            }
            for rel in rels {
                let t = rel * len;
                let a = eval_gltf_with(&doc, &asset.bin, Some(ai), t, mode.blend);
                let b = eval_engine(mesh, names, Some((set, seq)), t, scale);
                let d = max_dist(&a, &b);
                worst = worst.max(d);
                assert!(
                    d < tolerance,
                    "{} ({mode:?}) at {rel} of the sequence: {d}",
                    seq.info.sequence_name
                );
            }
        }
        worst
    }

    #[test]
    fn synthetic_skinning_matches_the_engine_rules() {
        let (mesh, names) = synthetic_mesh();
        let anims = vec![synthetic_anim(false), synthetic_anim(true)];
        let frames = Mode {
            resample: true,
            blend: Blend::Slerp,
            between_frames: false,
        };
        check_mesh_against_engine(&mesh, &names, &anims, 1.0, 1e-3, i32::MAX, frames);
        check_mesh_against_engine(&mesh, &names, &anims, 0.02, 1e-5, i32::MAX, frames);
        // Stored keys sampled with the engine's blend agree everywhere, also
        // between frames and inside the frame-table track's key intervals.
        let keyed = Mode {
            resample: false,
            blend: Blend::Nlerp,
            between_frames: true,
        };
        check_mesh_against_engine(&mesh, &names, &anims, 1.0, 1e-3, i32::MAX, keyed);
    }

    #[test]
    fn keyed_export_matches_at_key_times() {
        let (mesh, names) = synthetic_mesh();
        let (set, seq) = synthetic_anim(false);
        let inputs = vec![AnimInput {
            set: &set,
            sequences: vec![&seq],
        }];
        let opts = BuildOptions {
            scale: 1.0,
            resample: false,
        };
        let mut entries = Vec::new();
        let asset = build_gltf(
            &mesh,
            &names,
            &[],
            &[],
            &mesh.native.lods[0],
            &inputs,
            &opts,
            "x.bin",
            &mut entries,
        )
        .unwrap();
        assert!(validate_gltf(&asset.json, &asset.bin).is_empty());
        assert_eq!(entries.len(), 1);
        // Three rotations; with bAnimRotationOnly off every tracked bone also
        // takes the animation's translation (the Arm track's absent
        // translation is the engine's zero vector).
        assert_eq!(entries[0][0].channels, 3 + 3);
        // At t = 0 and 1 every track is on a key; at 0.5 and 0.7 some tracks
        // are between keys (the frame-table track has keys at frames 0, 7, 10).
        for t in [0.0, 0.5, 0.7, 1.0] {
            let b = eval_engine(&mesh, &names, Some((&set, &seq)), t, 1.0);
            let slerped = eval_gltf(&asset.json, &asset.bin, Some(0), t);
            let nlerped = eval_gltf_with(&asset.json, &asset.bin, Some(0), t, Blend::Nlerp);
            // With the engine's blend the stored keys reproduce the engine
            // everywhere.
            assert!(
                max_dist(&nlerped, &b) < 1e-3,
                "t {t}: {}",
                max_dist(&nlerped, &b)
            );
            if t == 0.0 || t == 1.0 {
                assert!(
                    max_dist(&slerped, &b) < 1e-3,
                    "t {t}: {}",
                    max_dist(&slerped, &b)
                );
            } else {
                // Some tracks are between keys: glTF's slerp differs slightly
                // from the engine's nlerp there.
                let d = max_dist(&slerped, &b);
                assert!(d > 1e-3 && d < 0.1, "t {t}: {d}");
            }
        }
    }

    /// The synthetic sequence with the frame-table track's last two keys on
    /// the same (final) frame and different values, as on 399 shipped tracks.
    fn duplicated_final_frame_anim() -> (AnimSetInfo, AnimSequence) {
        let (set, mut seq) = synthetic_anim(false);
        seq.tracks[2].rotation = Some(rot_track(
            &[
                [0.5, 0.1, -0.2, 0.8],
                [0.4, 0.3, -0.1, 0.8],
                [0.2, 0.5, -0.3, 0.7],
                [0.7, -0.2, 0.1, 0.6],
            ],
            Some(vec![0, 6, 10, 10]),
        ));
        (set, seq)
    }

    #[test]
    fn duplicated_final_frame_follows_the_engine() {
        let (mesh, names) = synthetic_mesh();
        let anims = vec![duplicated_final_frame_anim()];
        let keyed = Mode {
            resample: false,
            blend: Blend::Nlerp,
            between_frames: true,
        };
        check_mesh_against_engine(&mesh, &names, &anims, 1.0, 1e-3, i32::MAX, keyed);
        let resampled = Mode {
            resample: true,
            blend: Blend::Slerp,
            between_frames: false,
        };
        check_mesh_against_engine(&mesh, &names, &anims, 1.0, 1e-3, i32::MAX, resampled);
        // The check is sensitive: letting the last key win the whole final
        // interval (the earlier export rule) moves the mesh visibly.
        let (set, seq) = &anims[0];
        let mut last_wins = seq.clone();
        if let Some(KeyData::F32(v)) = last_wins.tracks[2].rotation.as_mut().map(|t| &mut t.data) {
            let tail: Vec<f32> = v[9..12].to_vec();
            v[6..9].copy_from_slice(&tail);
        }
        let a = eval_engine(&mesh, &names, Some((set, seq)), 0.9, 1.0);
        let b = eval_engine(&mesh, &names, Some((set, &last_wins)), 0.9, 1.0);
        assert!(max_dist(&a, &b) > 0.1, "{}", max_dist(&a, &b));
    }

    fn build(
        mesh: &SkeletalMesh,
        names: &[String],
        anims: &[(AnimSetInfo, AnimSequence)],
        resample: bool,
    ) -> Result<GltfAsset> {
        let inputs: Vec<AnimInput<'_>> = anims
            .iter()
            .map(|(s, q)| AnimInput {
                set: s,
                sequences: vec![q],
            })
            .collect();
        let opts = BuildOptions {
            scale: 1.0,
            resample,
        };
        let mut entries = Vec::new();
        build_gltf(
            mesh,
            names,
            &[],
            &[],
            &mesh.native.lods[0],
            &inputs,
            &opts,
            "x.bin",
            &mut entries,
        )
    }

    #[test]
    fn hostile_sequence_tags_give_valid_documents_or_errors() {
        let (mesh, names) = synthetic_mesh();
        // Non-finite / negative SequenceLength and extreme NumFrames: key
        // times are sanitised, the document stays valid, nothing panics.
        for (len, frames) in [
            (f32::NAN, 11),
            (f32::INFINITY, 11),
            (-1.0, 11),
            (0.0, 11),
            (1.0, i32::MIN),
            (1.0, 0),
            (1.0, 1),
            (f32::MAX, i32::MAX),
        ] {
            let (set, mut seq) = synthetic_anim(false);
            seq.info.sequence_length = len;
            seq.info.num_frames = frames;
            let anims = vec![(set, seq)];
            let asset = build(&mesh, &names, &anims, false).unwrap();
            let doc: Json = serde_json::from_str(&asset.json.to_string()).unwrap();
            let issues = validate_gltf(&doc, &asset.bin);
            assert!(issues.is_empty(), "{len} {frames}: {issues:?}");
            // Resampling refuses absurd frame counts instead of allocating.
            let r = build(&mesh, &names, &anims, true);
            if frames == i32::MAX {
                assert!(r.is_err());
            } else {
                let asset = r.unwrap();
                assert!(
                    validate_gltf(&asset.json, &asset.bin).is_empty(),
                    "{len} {frames}"
                );
            }
        }
        // A LOD without triangles is refused (glTF meshes need a primitive).
        let (mut mesh, names) = synthetic_mesh();
        mesh.native.lods[0].sections.clear();
        assert!(build(&mesh, &names, &[], false).is_err());
    }

    /// Hostile input end to end: mutated mesh payloads and animation streams
    /// that still decode (and, for meshes, pass the structural checks the
    /// driver runs first) never make the exporter panic; what it builds
    /// validates (animation keys may be non-finite, which the validator
    /// reports and the driver refuses to write).
    #[test]
    fn mutated_inputs_never_panic_the_exporter() {
        use asamu_ue3::anim::{decode_tracks, encode_tracks};
        use asamu_ue3::skeletal::{decode_skeletal_mesh_native, encode_skeletal_mesh_native};
        let (mesh, names) = synthetic_mesh();
        let (set, base_seq) = synthetic_anim(true);
        // A per-track-only stream: every track rewritten in the per-track
        // codec, offsets at the sequential positions.
        let mut seq = base_seq.clone();
        for bt in &mut seq.tracks {
            if let Some(t) = bt.translation.as_mut() {
                t.codec = Codec::PerTrack;
                t.format = CompressionFormat::Float96NoW;
                t.component_mask = 7;
            }
        }
        let mut offsets = Vec::new();
        let mut at = 0i32;
        for bt in &seq.tracks {
            for part in [&bt.translation, &bt.rotation] {
                match part {
                    Some(t) => {
                        offsets.push(at);
                        let one = BoneTrack {
                            translation: Some(t.clone()),
                            rotation: None,
                        };
                        let len = encode_tracks(&[one], seq.info.num_frames, 0x55)
                            .unwrap()
                            .len();
                        at += i32::try_from(len).unwrap();
                    }
                    None => offsets.push(-1),
                }
            }
        }
        seq.info.compressed_track_offsets = offsets;
        let stream = encode_tracks(&seq.tracks, seq.info.num_frames, 0x55).unwrap();
        seq.tracks = decode_tracks(&seq.info, &stream).unwrap();
        let payload = encode_skeletal_mesh_native(&mesh.native, false).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            seed >> 33
        };
        let (mut meshes_built, mut anims_built) = (0, 0);
        for round in 0..1500 {
            // Mesh mutation.
            let mut m = payload.clone();
            for _ in 0..1 + next() % 3 {
                let at = (next() as usize) % m.len();
                m[at] = next() as u8;
            }
            if let Ok(native) = decode_skeletal_mesh_native(&m, 0, false) {
                let candidate = SkeletalMesh {
                    object: object("Test.Mesh"),
                    has_vertex_colors: false,
                    native,
                };
                let ctx = ValidationContext::default();
                let structural = validate_skeletal_mesh(&candidate.native, Some(&names), &ctx);
                if structural.is_empty()
                    && let Ok(asset) =
                        build(&candidate, &names, &[(set.clone(), seq.clone())], false)
                {
                    meshes_built += 1;
                    let issues = validate_gltf(&asset.json, &asset.bin);
                    assert!(issues.is_empty(), "round {round}: {issues:?}");
                }
            }
            // Stream mutation (and hostile tags every few rounds).
            let mut st = stream.clone();
            for _ in 0..1 + next() % 3 {
                let at = (next() as usize) % st.len();
                st[at] = next() as u8;
            }
            let mut info = seq.info.clone();
            if round % 5 == 0 {
                info.num_frames = [i32::MIN, -1, 0, 1, 300, i32::MAX][(next() % 6) as usize];
                info.sequence_length = [f32::NAN, -2.0, 0.0, 1e30][(next() % 4) as usize];
            }
            if let Ok(tracks) = decode_tracks(&info, &st) {
                let mut s2 = seq.clone();
                s2.info = info;
                s2.tracks = tracks;
                for resample in [false, true] {
                    if let Ok(asset) = build(&mesh, &names, &[(set.clone(), s2.clone())], resample)
                    {
                        anims_built += 1;
                        // Only non-finite key values (refused by the driver,
                        // which never writes an invalid document) may fail.
                        let issues = validate_gltf(&asset.json, &asset.bin);
                        assert!(
                            issues.iter().all(|m| m.contains("non-finite")),
                            "round {round}: {issues:?}"
                        );
                    }
                }
            }
        }
        assert!(meshes_built > 20, "{meshes_built}");
        assert!(anims_built > 100, "{anims_built}");
    }

    #[test]
    fn validator_reports_broken_documents() {
        let (mesh, names) = synthetic_mesh();
        let (set, seq) = synthetic_anim(false);
        let inputs = vec![AnimInput {
            set: &set,
            sequences: vec![&seq],
        }];
        let opts = BuildOptions {
            scale: 1.0,
            resample: false,
        };
        let mut entries = Vec::new();
        let asset = build_gltf(
            &mesh,
            &names,
            &[],
            &[],
            &mesh.native.lods[0],
            &inputs,
            &opts,
            "x.bin",
            &mut entries,
        )
        .unwrap();
        // Corrupt one inverse bind matrix.
        let mut bin = asset.bin.clone();
        let ibm =
            &asset.json["accessors"][idx(&asset.json["skins"][0]["inverseBindMatrices"]).unwrap()];
        let off = idx(&asset.json["bufferViews"][idx(&ibm["bufferView"]).unwrap()]["byteOffset"])
            .unwrap();
        bin[off + 48..off + 52].copy_from_slice(&123.0f32.to_le_bytes());
        assert!(
            validate_gltf(&asset.json, &bin)
                .iter()
                .any(|m| m.contains("inverse bind"))
        );
        // Non-increasing times.
        let mut doc = asset.json.clone();
        let input = idx(&doc["animations"][0]["samplers"][0]["input"]).unwrap();
        let view = idx(&doc["accessors"][input]["bufferView"]).unwrap();
        let off = idx(&doc["bufferViews"][view]["byteOffset"]).unwrap();
        let mut bin = asset.bin.clone();
        bin[off + 4..off + 8].copy_from_slice(&0.0f32.to_le_bytes());
        doc["accessors"][input]["max"] = json!([1.0]);
        assert!(
            validate_gltf(&doc, &bin)
                .iter()
                .any(|m| m.contains("strictly increasing"))
        );
        // A joint index outside the skin.
        let mut doc = asset.json.clone();
        doc["skins"][0]["joints"] = json!([1, 2]);
        assert!(!validate_gltf(&doc, &asset.bin).is_empty());
        // A node that is its own child.
        let mut doc = asset.json.clone();
        doc["nodes"][1]["children"] = json!([1]);
        assert!(!validate_gltf(&doc, &asset.bin).is_empty());
        // No primitives; a node pointing at a missing mesh or skin; a skin
        // whose skeleton is not above its joints.
        let mut doc = asset.json.clone();
        doc["meshes"][0]["primitives"] = json!([]);
        assert!(
            validate_gltf(&doc, &asset.bin)
                .iter()
                .any(|m| m.contains("no primitives"))
        );
        let mut doc = asset.json.clone();
        let last = doc["nodes"].as_array().unwrap().len() - 1;
        doc["nodes"][last]["mesh"] = json!(7);
        doc["nodes"][last]["skin"] = json!(3);
        let issues = validate_gltf(&doc, &asset.bin);
        assert!(
            issues.iter().any(|m| m.contains("missing mesh")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|m| m.contains("missing skin")),
            "{issues:?}"
        );
        let mut doc = asset.json.clone();
        doc["skins"][0]["skeleton"] = json!(last);
        assert!(
            validate_gltf(&doc, &asset.bin)
                .iter()
                .any(|m| m.contains("skeleton"))
        );
        // A non-finite node translation (written as JSON null).
        let mut doc = asset.json.clone();
        doc["nodes"][1]["translation"] = json!([null, 0.0, 0.0]);
        assert!(
            validate_gltf(&doc, &asset.bin)
                .iter()
                .any(|m| m.contains("finite numbers"))
        );
        // A NaN key time.
        let mut doc = asset.json.clone();
        let input = idx(&doc["animations"][0]["samplers"][0]["input"]).unwrap();
        let view = idx(&doc["accessors"][input]["bufferView"]).unwrap();
        let off = idx(&doc["bufferViews"][view]["byteOffset"]).unwrap();
        let mut bin = asset.bin.clone();
        bin[off + 4..off + 8].copy_from_slice(&f32::NAN.to_le_bytes());
        doc["accessors"][input]
            .as_object_mut()
            .unwrap()
            .remove("max");
        assert!(
            validate_gltf(&doc, &bin)
                .iter()
                .any(|m| m.contains("strictly increasing"))
        );
    }

    /// Real data (skips without the install): every mesh with animations,
    /// first sequence of each set, evaluated at every frame.
    #[test]
    fn real_data_skinning_matches_the_engine_rules() {
        let install = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
            Some(d) => asamu_locate::from_original_dir(Path::new(&d)).ok(),
            None => asamu_locate::locate().ok(),
        };
        let Some(install) = install else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let dirs = vec![install.cooked_dir.clone(), install.maps_dir.clone()];
        let mut g = Gathered::default();
        for file in package_files(&dirs) {
            let set = PackageSet::new(&dirs);
            let lp = set.open_file(&file).unwrap();
            gather_package(&mut g, &lp, &set);
        }
        assert!(g.failures.is_empty(), "{:?}", g.failures);
        let (pairs, unpaired) = pair_sets(&g);
        assert!(unpaired.is_empty());
        let mut checked = 0;
        let (mut keyed_worst, mut slerp_worst) = (0.0f64, 0.0f64);
        for (path, sets) in &pairs {
            let mi = &g.meshes[path];
            let anims: Vec<(AnimSetInfo, AnimSequence)> = sets
                .iter()
                .filter_map(|(sp, _)| {
                    let set = g.sets.get(sp)?;
                    let seq = set
                        .sequences
                        .iter()
                        .flatten()
                        .find_map(|p| g.sequences.get(p))?;
                    Some((set.clone(), seq.seq.clone()))
                })
                .collect();
            // Every frame resampled, read back with glTF's slerp.
            let resampled = Mode {
                resample: true,
                blend: Blend::Slerp,
                between_frames: false,
            };
            check_mesh_against_engine(&mi.mesh, &mi.bone_names, &anims, 1.0, 0.05, 8, resampled);
            // Stored keys with the engine's nlerp: key times, frame tables
            // (including duplicated final frames) and translation rules.
            let keyed = Mode {
                resample: false,
                blend: Blend::Nlerp,
                between_frames: true,
            };
            let d =
                check_mesh_against_engine(&mi.mesh, &mi.bone_names, &anims, 1.0, 0.05, 8, keyed);
            keyed_worst = keyed_worst.max(d);
            // The same document read with glTF's slerp: only the blend
            // between keys differs from the engine.
            let slerped = Mode {
                resample: false,
                blend: Blend::Slerp,
                between_frames: true,
            };
            let d =
                check_mesh_against_engine(&mi.mesh, &mi.bone_names, &anims, 1.0, 0.5, 8, slerped);
            slerp_worst = slerp_worst.max(d);
            checked += 1;
        }
        eprintln!(
            "real-data skinning check: {checked} meshes; keyed export with the engine's nlerp \
             within {keyed_worst:.5} uu, with glTF slerp within {slerp_worst:.5} uu"
        );
        assert!(checked >= 15);
    }
}
