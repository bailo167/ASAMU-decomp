//! `asamu-import meshes`: StaticMesh → glTF 2.0 (`.gltf` JSON + `.bin`), with a
//! JSON manifest for the runtime.
//!
//! Everything written is derived from the user's own copy of the game: it goes
//! to the user-local output directory only (never the repository, except a
//! git-ignored `research/` subfolder, and never the install), and must not be
//! redistributed. `--check` writes nothing: it decodes, converts and validates
//! every mesh in memory and prints a report.
//!
//! The native layout decoded here is documented in
//! `docs/reverse-engineering/MESHES.md`.
//!
//! Output layout under `<out>/meshes/`:
//!
//! ```text
//! manifest.json                       object path -> files, LODs, sections, ...
//! <Package>/<Path...>/<Name>.gltf     LOD 0 (+ <Name>_LOD<n>.gltf with --all-lods)
//! <Package>/<Path...>/<Name>.bin      binary buffer of the .gltf next to it
//! <Package>/<Path...>/<Name>.collision.json   simple collision shapes (meshes with a body setup)
//! ```
//!
//! An object path cooked into several packages (the same mesh in several
//! maps) is written once, from the first package that contains it; the
//! manifest lists the other packages under `also_in` when the converted
//! content is identical, and under `differs_in` (with a separate
//! `<Name>@<Package>.gltf`) when it is not.
//!
//! # Conversion
//!
//! - **Axes.** UE3 is left-handed with X forward, Y right, Z up; glTF (and the
//!   Bevy runtime, see `asamu_core::coords`) is right-handed with -Z forward,
//!   X right, Y up. Points and directions map as `(x, y, z)_ue → (y, z, -x)`,
//!   the same mapping as `asamu_core::coords::ue_dir_to_bevy`. Its
//!   determinant is -1.
//! - **Scale.** `--scale` glTF units per Unreal unit (default 1: positions stay
//!   in UU; the runtime applies its own world scale).
//! - **Winding.** In the shipped meshes `cross(b - a, c - a)` points *against*
//!   the stored vertex normal (UE3 data, CONFIRMED in MESHES.md). The
//!   determinant -1 mapping flips that, so the triangle order is kept as is
//!   and glTF's counter-clockwise front faces agree with the normals.
//! - **Normals** are `TangentZ`; **tangents** are `TangentX` with
//!   `w = -sign(TangentZ.W)`: UE3's bitangent is `cross(Z, X) * sign(W)`
//!   (CONFIRMED in MESHES.md) and the mirror negates cross products. Normals
//!   are renormalised; the few zero normals in the data are replaced by the
//!   area-weighted face normal; tangents are orthogonalised against the
//!   normal.
//! - **UVs** keep UE3's top-left origin, which is glTF's. Every stored channel
//!   is written (`TEXCOORD_0`, `TEXCOORD_1`, ...). The lightmap channel index
//!   (`LightMapCoordinateIndex`) is recorded in the manifest.
//! - **Vertex colors** (`COLOR_0`, normalized RGBA bytes) are the stored
//!   `FColor` values unchanged apart from the BGRA → RGBA order.
//! - **Sections** become one primitive each, with a placeholder material named
//!   after the UE3 material path (`None` for an unassigned slot).
//! - **Collision** (`--collision`): the kDOP collision triangles (which index
//!   LOD 0's vertices) become a second mesh on a node named `UCX_<Name>`.
//!
//! # Simple collision
//!
//! Every manifest entry carries `simple_collision`: the mesh's three
//! simple-collision switches (the stored value, else the native default
//! `true`), whether the mesh has a body setup, and for a mesh with one the
//! `.collision.json` file with its shapes ([`SimpleCollisionDoc`]: convex
//! elements, boxes, spheres and capsules).
//!
//! The shapes are **not** converted to glTF axes: they are in the mesh's
//! local space, UE3 axes (X forward, Y right, Z up, left-handed) and Unreal
//! units, whatever `--scale` is. It is the same local space the mesh's
//! vertices and collision triangles are in, so a shape point `(x, y, z)`
//! corresponds to the glTF point `(y, z, -x) * scale` of the mesh's files.
//! A component's scale and placement apply to the shapes as they do to the
//! triangles. See `docs/reverse-engineering/MESHES.md`, "Simple collision".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::PackageSet;
use asamu_ue3::bodysetup::{
    BodyProperty, BodySetup, Matrix, ScalarValue, decode_body_setup, validate_body_setup,
};
use asamu_ue3::model::LoadedPackage;
use asamu_ue3::staticmesh::{
    CollisionTriangle, LodModel, StaticMesh, ValidationContext, decode_static_mesh, is_static_mesh,
    validate_static_mesh,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::safety;

/// Manifest format version. Version 2 added `simple_collision` to every
/// entry (and the `.collision.json` files), and the content hash covers them.
const MANIFEST_VERSION: u32 = 2;

/// `format` of a `.collision.json` document.
pub const SIMPLE_COLLISION_FORMAT: &str = "asamu-simple-collision";
/// `version` of a `.collision.json` document.
pub const SIMPLE_COLLISION_VERSION: u32 = 1;
/// Coordinate system note stored in every `.collision.json` document.
const SIMPLE_COLLISION_SPACE: &str = "mesh local space, UE3 axes (x forward, y right, z up, \
     left-handed), Unreal units, unscaled; the mesh's glTF files hold the same points as \
     (y, z, -x) * scale";

/// Notice stored in every manifest and glTF file.
const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
                      Copyrighted game data: keep it local, never redistribute.";

/// Coordinate system note stored in the manifest.
const COORDINATES: &str =
    "glTF 2.0: right-handed, +Y up, -Z forward; UE3 (x, y, z) -> (y, z, -x) * scale";

/// glTF component types.
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const UNSIGNED_BYTE: u32 = 5121;
const BYTE: u32 = 5120;
const SHORT: u32 = 5122;
/// glTF buffer-view targets.
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
/// Unit-length tolerance for normals and tangents (the glTF validator uses
/// 0.00005 on the squared length; this is looser on purpose and still far
/// tighter than the packed source precision).
const UNIT_TOLERANCE: f32 = 1e-3;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only packages whose file name contains this text (case-insensitive;
    /// repeatable).
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Only meshes whose object path contains this text (case-insensitive).
    #[arg(long)]
    name: Option<String>,
    /// Stop after converting this many meshes.
    #[arg(long)]
    limit: Option<usize>,
    /// Also write LODs 1.. (`<Name>_LOD<n>.gltf`); by default only LOD 0.
    #[arg(long)]
    all_lods: bool,
    /// Add the kDOP collision triangles as a `UCX_<Name>` node.
    #[arg(long)]
    collision: bool,
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
// Coordinate conversion and vector helpers
// ---------------------------------------------------------------------------

/// UE3 axes → glTF axes (no scale): `(x, y, z) → (y, z, -x)`. Same mapping as
/// `asamu_core::coords::ue_dir_to_bevy`.
pub fn ue_to_gltf(v: [f32; 3]) -> [f32; 3] {
    [v[1], v[2], -v[0]]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len3(a: [f32; 3]) -> f32 {
    dot3(a, a).sqrt()
}

fn normalize3(a: [f32; 3]) -> Option<[f32; 3]> {
    let l = len3(a);
    (l.is_finite() && l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// Any unit vector perpendicular to unit `n`.
fn perpendicular(n: [f32; 3]) -> [f32; 3] {
    let axis = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    normalize3(cross3(n, axis)).unwrap_or([1.0, 0.0, 0.0])
}

// ---------------------------------------------------------------------------
// glTF building
// ---------------------------------------------------------------------------

/// Conversion settings.
#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    /// glTF units per Unreal unit.
    pub scale: f32,
}

/// One section's material and triangles, as given to [`build_gltf`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionEntry {
    /// UE3 material path (`None` for an unassigned slot).
    pub material: Option<String>,
    /// First index in the LOD's index buffer.
    pub first_index: u32,
    /// Triangles.
    pub triangles: u32,
    /// `EnableCollision`.
    pub collision: bool,
    /// `bEnableShadowCasting`.
    pub cast_shadow: bool,
}

/// Statistics of one conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetStats {
    /// Vertices written.
    pub vertices: usize,
    /// Triangles written (render primitives only).
    pub triangles: u64,
    /// Primitives written.
    pub primitives: usize,
    /// UV channels written.
    pub uv_channels: usize,
    /// `COLOR_0` written.
    pub colors: bool,
    /// Zero normals replaced by face normals.
    pub normals_fixed: usize,
    /// Tangents parallel to the normal replaced by a perpendicular.
    pub tangents_fixed: usize,
    /// Triangles whose glTF face normal agrees with the vertex normals.
    pub winding_agree: u64,
    /// Triangles whose glTF face normal opposes the vertex normals.
    pub winding_disagree: u64,
    /// Collision triangles written.
    pub collision_triangles: usize,
}

/// A converted glTF asset: JSON document and binary buffer.
#[derive(Debug, Clone)]
pub struct GltfAsset {
    /// The `.gltf` document.
    pub json: Value,
    /// The `.bin` buffer it references.
    pub bin: Vec<u8>,
    /// Statistics.
    pub stats: AssetStats,
}

/// Accumulates buffer views and accessors over one binary buffer.
#[derive(Default)]
struct BinBuilder {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl BinBuilder {
    /// Append `bytes` as a new 4-byte-aligned buffer view.
    fn view(&mut self, bytes: &[u8], target: u32) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views.push(json!({
            "buffer": 0,
            "byteOffset": offset,
            "byteLength": bytes.len(),
            "target": target,
        }));
        self.views.len() - 1
    }

    fn accessor(&mut self, mut a: Value) -> usize {
        if let Some(obj) = a.as_object_mut()
            && obj.get("byteOffset").and_then(Value::as_u64) == Some(0)
        {
            obj.remove("byteOffset");
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn vec3_f32(&mut self, data: &[[f32; 3]], with_bounds: bool) -> usize {
        let mut bytes = Vec::with_capacity(data.len() * 12);
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in data {
            for k in 0..3 {
                bytes.extend_from_slice(&v[k].to_le_bytes());
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        let view = self.view(&bytes, ARRAY_BUFFER);
        let mut a = json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": data.len(),
            "type": "VEC3",
        });
        if with_bounds {
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessor(a)
    }

    fn vec4_f32(&mut self, data: &[[f32; 4]]) -> usize {
        let mut bytes = Vec::with_capacity(data.len() * 16);
        for v in data {
            for c in v {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        let view = self.view(&bytes, ARRAY_BUFFER);
        self.accessor(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": data.len(),
            "type": "VEC4",
        }))
    }

    fn vec2_f32(&mut self, data: &[[f32; 2]]) -> usize {
        let mut bytes = Vec::with_capacity(data.len() * 8);
        for v in data {
            for c in v {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        let view = self.view(&bytes, ARRAY_BUFFER);
        self.accessor(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": data.len(),
            "type": "VEC2",
        }))
    }

    fn rgba_u8(&mut self, data: &[[u8; 4]]) -> usize {
        let bytes: Vec<u8> = data.iter().flatten().copied().collect();
        let view = self.view(&bytes, ARRAY_BUFFER);
        self.accessor(json!({
            "bufferView": view,
            "componentType": UNSIGNED_BYTE,
            "normalized": true,
            "count": data.len(),
            "type": "VEC4",
        }))
    }

    /// One index buffer view; returns its index.
    fn indices_view(&mut self, indices: &[u16]) -> usize {
        let bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        self.view(&bytes, ELEMENT_ARRAY_BUFFER)
    }

    fn index_accessor(&mut self, view: usize, first: usize, count: usize) -> usize {
        self.accessor(json!({
            "bufferView": view,
            "byteOffset": first * 2,
            "componentType": UNSIGNED_SHORT,
            "count": count,
            "type": "SCALAR",
        }))
    }
}

/// Convert one LOD to glTF. `sections[i]` describes `lod.sections[i]`
/// (material path etc.); `collision` adds the kDOP triangles (which index LOD
/// 0's vertices, so only pass them for LOD 0). `bin_uri` is the buffer's
/// relative file name.
pub fn build_gltf(
    name: &str,
    lod: &LodModel,
    sections: &[SectionEntry],
    collision: Option<&[CollisionTriangle]>,
    opts: &ConvertOptions,
    bin_uri: &str,
) -> Result<GltfAsset> {
    let nv = lod.positions.positions.len();
    let vb = &lod.vertices;
    if nv == 0 {
        bail!("{name}: no vertices");
    }
    if vb.tangent_z.len() != nv || vb.tangent_x.len() != nv || vb.uvs.iter().any(|c| c.len() != nv)
    {
        bail!("{name}: vertex buffer size differs from the position buffer");
    }
    if sections.len() != lod.sections.len() {
        bail!("{name}: section descriptions do not match the LOD");
    }
    if !(opts.scale.is_finite() && opts.scale > 0.0) {
        bail!("scale must be a positive finite number");
    }
    let mut stats = AssetStats {
        vertices: nv,
        ..AssetStats::default()
    };

    // Positions.
    let mut positions = Vec::with_capacity(nv);
    for p in &lod.positions.positions {
        let g = ue_to_gltf(*p).map(|c| c * opts.scale);
        if g.iter().any(|c| !c.is_finite()) {
            bail!("{name}: non-finite vertex position");
        }
        positions.push(g);
    }
    // Index ranges.
    for (si, s) in lod.sections.iter().enumerate() {
        let first = usize::try_from(s.first_index)?;
        let count = usize::try_from(s.num_triangles)?
            .checked_mul(3)
            .context("index count overflow")?;
        let end = first.checked_add(count).context("index range overflow")?;
        if end > lod.indices.len() {
            bail!("{name}: section {si} indices exceed the index buffer");
        }
    }
    if let Some(&bad) = lod.indices.iter().find(|&&i| usize::from(i) >= nv) {
        bail!("{name}: index {bad} out of range ({nv} vertices)");
    }
    // Area-weighted face normals in glTF space (kept winding), used for
    // vertices whose stored normal is zero.
    let mut face_acc = vec![[0.0f32; 3]; nv];
    for t in lod.indices.as_chunks::<3>().0 {
        let (a, b, c) = (usize::from(t[0]), usize::from(t[1]), usize::from(t[2]));
        let f = cross3(
            sub3(positions[b], positions[a]),
            sub3(positions[c], positions[a]),
        );
        for v in [a, b, c] {
            for k in 0..3 {
                face_acc[v][k] += f[k];
            }
        }
    }
    // Normals and tangents.
    let mut normals = Vec::with_capacity(nv);
    let mut tangents = Vec::with_capacity(nv);
    for (v, face) in face_acc.iter().enumerate() {
        let z = vb.tangent_z[v].unpack();
        let x = vb.tangent_x[v].unpack();
        let stored = ue_to_gltf([z[0], z[1], z[2]]);
        let n = if len3(stored) >= 0.5 {
            normalize3(stored).unwrap_or([0.0, 1.0, 0.0])
        } else {
            stats.normals_fixed += 1;
            normalize3(*face).unwrap_or([0.0, 1.0, 0.0])
        };
        let t_raw = ue_to_gltf([x[0], x[1], x[2]]);
        let d = dot3(n, t_raw);
        let t_ortho = [
            t_raw[0] - n[0] * d,
            t_raw[1] - n[1] * d,
            t_raw[2] - n[2] * d,
        ];
        let t = match normalize3(t_ortho) {
            Some(t) if len3(t_ortho) > 1e-3 => t,
            _ => {
                stats.tangents_fixed += 1;
                perpendicular(n)
            }
        };
        // UE3 bitangent = cross(Z, X) * sign(W); the mirror negates cross products.
        let w = if vb.tangent_z[v].0[3] >= 128 {
            -1.0
        } else {
            1.0
        };
        normals.push(n);
        tangents.push([t[0], t[1], t[2], w]);
    }

    let mut b = BinBuilder::default();
    let pos_acc = b.vec3_f32(&positions, true);
    let nrm_acc = b.vec3_f32(&normals, false);
    let tan_acc = b.vec4_f32(&tangents);
    let mut attributes = serde_json::Map::new();
    attributes.insert("POSITION".to_owned(), json!(pos_acc));
    attributes.insert("NORMAL".to_owned(), json!(nrm_acc));
    attributes.insert("TANGENT".to_owned(), json!(tan_acc));
    for (ch, uvs) in vb.uvs.iter().enumerate() {
        if uvs.iter().flatten().any(|c| !c.is_finite()) {
            bail!("{name}: non-finite UV in channel {ch}");
        }
        let acc = b.vec2_f32(uvs);
        attributes.insert(format!("TEXCOORD_{ch}"), json!(acc));
    }
    stats.uv_channels = vb.uvs.len();
    if lod.colors.num_vertices != 0 && lod.colors.colors_bgra.len() == nv {
        let rgba: Vec<[u8; 4]> = lod
            .colors
            .colors_bgra
            .iter()
            .map(|c| [c[2], c[1], c[0], c[3]])
            .collect();
        let acc = b.rgba_u8(&rgba);
        attributes.insert("COLOR_0".to_owned(), json!(acc));
        stats.colors = true;
    }

    // Materials (deduplicated by path, in first-use order).
    let mut material_names: Vec<Option<String>> = Vec::new();
    let mut primitives = Vec::new();
    let idx_view = if lod.indices.is_empty() {
        None
    } else {
        Some(b.indices_view(&lod.indices))
    };
    for (si, s) in lod.sections.iter().enumerate() {
        if s.num_triangles == 0 {
            continue;
        }
        let Some(view) = idx_view else { continue };
        let first = usize::try_from(s.first_index)?;
        let count = usize::try_from(s.num_triangles)? * 3;
        let acc = b.index_accessor(view, first, count);
        let mat = &sections[si].material;
        let mi = match material_names.iter().position(|m| m == mat) {
            Some(i) => i,
            None => {
                material_names.push(mat.clone());
                material_names.len() - 1
            }
        };
        for t in lod.indices[first..first + count].as_chunks::<3>().0 {
            let (a, bb, c) = (usize::from(t[0]), usize::from(t[1]), usize::from(t[2]));
            let f = cross3(
                sub3(positions[bb], positions[a]),
                sub3(positions[c], positions[a]),
            );
            let vn = [0, 1, 2].map(|k| normals[a][k] + normals[bb][k] + normals[c][k]);
            let d = dot3(f, vn);
            if len3(f) > 1e-12 {
                if d > 0.0 {
                    stats.winding_agree += 1;
                } else if d < 0.0 {
                    stats.winding_disagree += 1;
                }
            }
        }
        stats.triangles += u64::from(s.num_triangles);
        primitives.push(json!({
            "attributes": Value::Object(attributes.clone()),
            "indices": acc,
            "material": mi,
            "mode": 4,
        }));
    }
    if primitives.is_empty() {
        bail!("{name}: no triangles");
    }
    stats.primitives = primitives.len();

    let mut meshes = vec![json!({ "name": name, "primitives": primitives })];
    let mut nodes = vec![json!({ "name": name, "mesh": 0 })];
    if let Some(tris) = collision.filter(|t| !t.is_empty()) {
        let mut idx = Vec::with_capacity(tris.len() * 3);
        for t in tris {
            if t.vertices.iter().any(|&v| usize::from(v) >= nv) {
                bail!("{name}: collision triangle references a missing vertex");
            }
            idx.extend_from_slice(&t.vertices);
        }
        let view = b.indices_view(&idx);
        let acc = b.index_accessor(view, 0, idx.len());
        let cname = format!("UCX_{name}");
        meshes.push(json!({
            "name": cname,
            "primitives": [{ "attributes": { "POSITION": pos_acc }, "indices": acc, "mode": 4 }],
        }));
        nodes.push(json!({ "name": cname, "mesh": 1, "extras": { "asamu_collision": true } }));
        stats.collision_triangles = tris.len();
    }
    let materials: Vec<Value> = material_names
        .iter()
        .map(|m| {
            json!({
                "name": m.clone().unwrap_or_else(|| "None".to_owned()),
                "pbrMetallicRoughness": {
                    "baseColorFactor": [0.8, 0.8, 0.8, 1.0],
                    "metallicFactor": 0.0,
                    "roughnessFactor": 1.0,
                },
                "extras": { "ue3_material": m },
            })
        })
        .collect();
    let scene_nodes: Vec<usize> = (0..nodes.len()).collect();
    while !b.bin.len().is_multiple_of(4) {
        b.bin.push(0);
    }
    let doc = json!({
        "asset": {
            "version": "2.0",
            "generator": concat!("asamu-import meshes ", env!("CARGO_PKG_VERSION")),
            "extras": {
                "notice": NOTICE,
                "source": name,
                "coordinates": COORDINATES,
                "scale": opts.scale,
            },
        },
        "scene": 0,
        "scenes": [{ "name": name, "nodes": scene_nodes }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [{ "uri": bin_uri, "byteLength": b.bin.len() }],
    });
    Ok(GltfAsset {
        json: doc,
        bin: b.bin,
        stats,
    })
}

// ---------------------------------------------------------------------------
// Structural validation of a written glTF
// ---------------------------------------------------------------------------

fn component_size(ct: u64) -> Option<usize> {
    match u32::try_from(ct).ok()? {
        BYTE | UNSIGNED_BYTE => Some(1),
        SHORT | UNSIGNED_SHORT => Some(2),
        UNSIGNED_INT | FLOAT => Some(4),
        _ => None,
    }
}

fn type_components(t: &str) -> Option<usize> {
    match t {
        "SCALAR" => Some(1),
        "VEC2" => Some(2),
        "VEC3" => Some(3),
        "VEC4" => Some(4),
        _ => None,
    }
}

/// A resolved accessor: its bytes and shape.
struct AccessorData<'a> {
    bytes: &'a [u8],
    component_type: u32,
    components: usize,
    count: usize,
}

impl AccessorData<'_> {
    fn f32_at(&self, i: usize, k: usize) -> Option<f32> {
        let at = (i * self.components + k) * 4;
        let b = self.bytes.get(at..at + 4)?;
        Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn index_at(&self, i: usize) -> Option<u32> {
        match self.component_type {
            UNSIGNED_SHORT => {
                let b = self.bytes.get(i * 2..i * 2 + 2)?;
                Some(u32::from(u16::from_le_bytes([b[0], b[1]])))
            }
            UNSIGNED_INT => {
                let b = self.bytes.get(i * 4..i * 4 + 4)?;
                Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            }
            UNSIGNED_BYTE => self.bytes.get(i).map(|&b| u32::from(b)),
            _ => None,
        }
    }
}

/// Structural checks of a glTF document against its binary buffer: buffer and
/// view bounds, accessor ranges and alignment, `POSITION` min/max equal to the
/// data, equal attribute counts per primitive, index ranges, unit normals and
/// tangents with `w = ±1`, and valid material/mesh/node/scene references.
/// Returns one message per problem (empty when the asset is consistent).
pub fn validate_gltf(doc: &Value, bin: &[u8]) -> Vec<String> {
    let mut issues = Vec::new();
    if doc["asset"]["version"] != "2.0" {
        issues.push("asset.version is not 2.0".to_owned());
    }
    let buffers = doc["buffers"].as_array().cloned().unwrap_or_default();
    if buffers.len() != 1 {
        issues.push(format!("{} buffers (expected 1)", buffers.len()));
        return issues;
    }
    if buffers[0]["byteLength"].as_u64() != u64::try_from(bin.len()).ok() {
        issues.push(format!(
            "buffer byteLength {} != {} bytes of .bin",
            buffers[0]["byteLength"],
            bin.len()
        ));
    }
    let views = doc["bufferViews"].as_array().cloned().unwrap_or_default();
    let mut view_bytes: Vec<Option<&[u8]>> = Vec::with_capacity(views.len());
    for (i, v) in views.iter().enumerate() {
        let off = v["byteOffset"].as_u64().unwrap_or(0);
        let len = v["byteLength"].as_u64().unwrap_or(0);
        let range = usize::try_from(off)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(o, l)| Some(o..o.checked_add(l)?));
        let slice = range.and_then(|r| bin.get(r));
        if v["buffer"].as_u64() != Some(0) || slice.is_none() || len == 0 {
            issues.push(format!("bufferView {i} is outside the buffer or empty"));
        }
        if !off.is_multiple_of(4) {
            issues.push(format!("bufferView {i} offset {off} is not 4-byte aligned"));
        }
        if v.get("byteStride").is_some() {
            issues.push(format!("bufferView {i} has a byteStride (not expected)"));
        }
        view_bytes.push(slice);
    }
    let accessors = doc["accessors"].as_array().cloned().unwrap_or_default();
    let mut acc: Vec<Option<AccessorData<'_>>> = Vec::with_capacity(accessors.len());
    for (i, a) in accessors.iter().enumerate() {
        let resolved = (|| {
            let view = view_bytes.get(usize::try_from(a["bufferView"].as_u64()?).ok()?)?;
            let view = (*view)?;
            let ct = a["componentType"].as_u64()?;
            let cs = component_size(ct)?;
            let comps = type_components(a["type"].as_str()?)?;
            let count = usize::try_from(a["count"].as_u64()?).ok()?;
            let off = usize::try_from(a["byteOffset"].as_u64().unwrap_or(0)).ok()?;
            if count == 0 || !off.is_multiple_of(cs) {
                return None;
            }
            let len = count.checked_mul(cs)?.checked_mul(comps)?;
            let bytes = view.get(off..off.checked_add(len)?)?;
            Some(AccessorData {
                bytes,
                component_type: u32::try_from(ct).ok()?,
                components: comps,
                count,
            })
        })();
        if resolved.is_none() {
            issues.push(format!(
                "accessor {i} is invalid (view, type, alignment or range)"
            ));
        }
        acc.push(resolved);
    }
    // Float accessors: finite values; min/max (when present) equal the data.
    for (i, a) in accessors.iter().enumerate() {
        let Some(d) = acc.get(i).and_then(Option::as_ref) else {
            continue;
        };
        if d.component_type != FLOAT {
            continue;
        }
        let mut lo = vec![f32::INFINITY; d.components];
        let mut hi = vec![f32::NEG_INFINITY; d.components];
        let mut finite = true;
        for v in 0..d.count {
            for k in 0..d.components {
                let x = d.f32_at(v, k).unwrap_or(f32::NAN);
                finite &= x.is_finite();
                lo[k] = lo[k].min(x);
                hi[k] = hi[k].max(x);
            }
        }
        if !finite {
            issues.push(format!("accessor {i} holds non-finite floats"));
        }
        for (key, want) in [("min", &lo), ("max", &hi)] {
            if let Some(got) = a.get(key) {
                let got: Vec<f32> = got
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                if &got != want {
                    issues.push(format!("accessor {i} {key} {got:?} != data {want:?}"));
                }
            }
        }
    }
    let materials = doc["materials"].as_array().map_or(0, Vec::len);
    let meshes = doc["meshes"].as_array().cloned().unwrap_or_default();
    for (mi, m) in meshes.iter().enumerate() {
        let prims = m["primitives"].as_array().cloned().unwrap_or_default();
        if prims.is_empty() {
            issues.push(format!("mesh {mi} has no primitives"));
        }
        for (pi, p) in prims.iter().enumerate() {
            let at = format!("mesh {mi} primitive {pi}");
            check_primitive(p, &acc, materials, &at, &mut issues);
        }
    }
    let nodes = doc["nodes"].as_array().cloned().unwrap_or_default();
    for (ni, n) in nodes.iter().enumerate() {
        if let Some(m) = n.get("mesh")
            && as_index(m).is_none_or(|m| m >= meshes.len())
        {
            issues.push(format!("node {ni} references a missing mesh"));
        }
    }
    let scenes = doc["scenes"].as_array().cloned().unwrap_or_default();
    for (si, s) in scenes.iter().enumerate() {
        for n in s["nodes"].as_array().cloned().unwrap_or_default() {
            if as_index(&n).is_none_or(|n| n >= nodes.len()) {
                issues.push(format!("scene {si} references a missing node"));
            }
        }
    }
    if as_index(&doc["scene"]).is_none_or(|s| s >= scenes.len()) {
        issues.push("default scene is missing".to_owned());
    }
    issues
}

fn as_index(v: &Value) -> Option<usize> {
    usize::try_from(v.as_u64()?).ok()
}

fn check_primitive(
    p: &Value,
    acc: &[Option<AccessorData<'_>>],
    materials: usize,
    at: &str,
    issues: &mut Vec<String>,
) {
    let get = |v: &Value| -> Option<&AccessorData<'_>> {
        acc.get(usize::try_from(v.as_u64()?).ok()?)?.as_ref()
    };
    if p["mode"].as_u64().unwrap_or(4) != 4 {
        issues.push(format!("{at}: not a triangle list"));
    }
    let attrs = p["attributes"].as_object().cloned().unwrap_or_default();
    let Some(pos) = attrs.get("POSITION").and_then(get) else {
        issues.push(format!("{at}: missing or invalid POSITION"));
        return;
    };
    if pos.component_type != FLOAT || pos.components != 3 {
        issues.push(format!("{at}: POSITION is not a float VEC3"));
    }
    let nv = pos.count;
    for (name, v) in &attrs {
        let Some(d) = get(v) else {
            issues.push(format!("{at}: invalid accessor for {name}"));
            continue;
        };
        if d.count != nv {
            issues.push(format!(
                "{at}: {name} has {} elements, POSITION {nv}",
                d.count
            ));
        }
        let shape_ok = match name.as_str() {
            "POSITION" | "NORMAL" => d.component_type == FLOAT && d.components == 3,
            "TANGENT" => d.component_type == FLOAT && d.components == 4,
            "COLOR_0" => d.component_type == UNSIGNED_BYTE && d.components == 4,
            n if n.starts_with("TEXCOORD_") => d.component_type == FLOAT && d.components == 2,
            _ => true,
        };
        if !shape_ok {
            issues.push(format!(
                "{at}: {name} has the wrong component type or shape"
            ));
        }
    }
    if let Some(n) = attrs.get("NORMAL").and_then(get)
        && n.component_type == FLOAT
        && n.components == 3
    {
        let bad = (0..n.count)
            .filter(|&i| {
                let v = [0, 1, 2].map(|k| n.f32_at(i, k).unwrap_or(0.0));
                (len3(v) - 1.0).abs() > UNIT_TOLERANCE
            })
            .count();
        if bad > 0 {
            issues.push(format!("{at}: {bad} normals are not unit length"));
        }
    }
    if let Some(t) = attrs.get("TANGENT").and_then(get)
        && t.component_type == FLOAT
        && t.components == 4
    {
        let bad = (0..t.count)
            .filter(|&i| {
                let v = [0, 1, 2].map(|k| t.f32_at(i, k).unwrap_or(0.0));
                let w = t.f32_at(i, 3).unwrap_or(0.0);
                (len3(v) - 1.0).abs() > UNIT_TOLERANCE || (w != 1.0 && w != -1.0)
            })
            .count();
        if bad > 0 {
            issues.push(format!(
                "{at}: {bad} tangents are not unit length with w = ±1"
            ));
        }
    }
    match p.get("indices") {
        Some(v) => match get(v) {
            Some(ix) => {
                if ix.components != 1
                    || !matches!(
                        ix.component_type,
                        UNSIGNED_SHORT | UNSIGNED_INT | UNSIGNED_BYTE
                    )
                {
                    issues.push(format!("{at}: indices are not unsigned scalars"));
                }
                if !ix.count.is_multiple_of(3) {
                    issues.push(format!("{at}: {} indices is not a multiple of 3", ix.count));
                }
                let nv32 = u32::try_from(nv).unwrap_or(u32::MAX);
                if (0..ix.count).any(|i| ix.index_at(i).is_none_or(|x| x >= nv32)) {
                    issues.push(format!("{at}: index out of range ({nv} vertices)"));
                }
            }
            None => issues.push(format!("{at}: invalid indices accessor")),
        },
        None => issues.push(format!("{at}: no indices")),
    }
    if let Some(m) = p.get("material")
        && as_index(m).is_none_or(|m| m >= materials)
    {
        issues.push(format!("{at}: material index out of range"));
    }
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// One exported LOD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LodEntry {
    /// LOD index (0 = finest).
    pub lod: usize,
    /// `.gltf` file, relative to the manifest.
    pub gltf: String,
    /// `.bin` file, relative to the manifest.
    pub bin: String,
    /// Sections (one glTF primitive each, unless empty).
    pub sections: Vec<SectionEntry>,
    /// Conversion statistics.
    pub stats: AssetStats,
}

/// UE3 bounds of a mesh (UE3 axes, UU).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundsEntry {
    /// Box centre.
    pub origin: [f32; 3],
    /// Box half-size.
    pub box_extent: [f32; 3],
    /// Sphere radius.
    pub sphere_radius: f32,
}

/// One mesh in the manifest, keyed by its object path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshEntry {
    /// Package the files were converted from.
    pub package: String,
    /// Export index in that package.
    pub export_index: usize,
    /// Other packages containing an identical copy.
    #[serde(default)]
    pub also_in: Vec<String>,
    /// Other packages whose copy differs (converted as `<Name>@<Package>`).
    #[serde(default)]
    pub differs_in: Vec<String>,
    /// LOD models in the source mesh.
    pub lod_count: usize,
    /// Exported LODs.
    pub lods: Vec<LodEntry>,
    /// `LightMapCoordinateIndex` (UV channel of the lightmap), when tagged.
    pub light_map_coordinate_index: Option<i32>,
    /// `LightMapResolution`, when tagged.
    pub light_map_resolution: Option<i32>,
    /// `BodySetup` object path (an `RB_BodySetup`), when set.
    pub body_setup: Option<String>,
    /// kDOP collision triangles (indices into LOD 0).
    pub collision_triangles: usize,
    /// Simple collision: the switches, and the shapes file of a mesh with a
    /// body setup.
    pub simple_collision: SimpleCollisionEntry,
    /// UE3 bounds.
    pub bounds_ue: BoundsEntry,
    /// glTF units per Unreal unit used for the files.
    pub scale: f32,
    /// FNV-1a 64 of the decoded source geometry of every LOD, the section
    /// material paths, the simple-collision switches and the body setup's
    /// shapes (independent of the conversion options; detects differing
    /// copies of the same path in different packages).
    pub content_hash: String,
}

/// Simple collision of one mesh in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleCollisionEntry {
    /// `UseSimpleBoxCollision`: swept (non-zero-extent) traces, the pawn's
    /// among them, use the simple shapes instead of the triangles. The
    /// stored value, else the native default `true`.
    pub use_simple_box_collision: bool,
    /// `UseSimpleLineCollision`: line (zero-extent) traces that are not
    /// forced to the triangles use the simple shapes. Stored, else `true`.
    pub use_simple_line_collision: bool,
    /// `UseSimpleRigidBodyCollision`. Stored, else `true`.
    pub use_simple_rigid_body_collision: bool,
    /// The mesh has a body setup (`body_setup` of the entry names it). A
    /// mesh without one has no simple shapes at all.
    pub has_body_setup: bool,
    /// The `.collision.json` file with the shapes, relative to the manifest
    /// (present exactly when the mesh has a body setup).
    pub shapes: Option<String>,
    /// Convex elements in that file.
    pub convex: usize,
    /// Boxes.
    pub boxes: usize,
    /// Spheres.
    pub spheres: usize,
    /// Capsules.
    pub sphyls: usize,
}

/// One convex element (`KConvexElem`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvexShape {
    /// `VertexData`.
    pub vertices: Vec<[f32; 3]>,
    /// `FacePlaneData` as `[nx, ny, nz, d]`: the plane `n . p = d`, with the
    /// normal pointing out of the element.
    pub planes: Vec<[f32; 4]>,
    /// `FaceTriData`: the surface as triangles of vertex indices.
    pub face_triangles: Vec<[u32; 3]>,
    /// `EdgeDirections`.
    pub edge_directions: Vec<[f32; 3]>,
    /// `FaceNormalDirections`.
    pub face_normal_directions: Vec<[f32; 3]>,
    /// `ElemBox.Min` (the vertices' bounding box).
    pub bounds_min: [f32; 3],
    /// `ElemBox.Max`.
    pub bounds_max: [f32; 3],
}

/// One box (`KBoxElem`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoxShape {
    /// `TM`: rows X axis, Y axis, Z axis, origin, each `[x, y, z, w]`
    /// (row-vector convention: a local point `p` is at
    /// `p.x * row0 + p.y * row1 + p.z * row2 + row3`).
    pub transform: Matrix,
    /// `X`, `Y`, `Z`: the full edge lengths along the local axes.
    pub size: [f32; 3],
    /// `bNoRBCollision`.
    pub no_rb_collision: bool,
    /// `bPerPolyShape`.
    pub per_poly_shape: bool,
}

/// One sphere (`KSphereElem`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SphereShape {
    /// `TM` (see [`BoxShape::transform`]); the centre is its fourth row.
    pub transform: Matrix,
    /// `Radius`.
    pub radius: f32,
    /// `bNoRBCollision`.
    pub no_rb_collision: bool,
    /// `bPerPolyShape`.
    pub per_poly_shape: bool,
}

/// One capsule (`KSphylElem`): a segment of `length` along the local Z axis,
/// centred on the origin, swept by a sphere of `radius`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SphylShape {
    /// `TM` (see [`BoxShape::transform`]).
    pub transform: Matrix,
    /// `Radius`.
    pub radius: f32,
    /// `Length` of the segment (the capsule is `length + 2 * radius` long).
    pub length: f32,
    /// `bNoRBCollision`.
    pub no_rb_collision: bool,
    /// `bPerPolyShape`.
    pub per_poly_shape: bool,
}

/// A `.collision.json` document: the simple collision shapes of one mesh's
/// body setup, in the mesh's local space, UE3 axes and Unreal units (never
/// scaled, never converted to glTF axes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimpleCollisionDoc {
    /// [`SIMPLE_COLLISION_FORMAT`].
    pub format: String,
    /// [`SIMPLE_COLLISION_VERSION`].
    pub version: u32,
    /// Redistribution notice.
    pub notice: String,
    /// Coordinate system of every point and direction in the document.
    pub space: String,
    /// Object path of the mesh.
    pub mesh: String,
    /// Object path of the body setup.
    pub body_setup: String,
    /// Scalar properties stored on the body setup (name → value; object
    /// references as object paths).
    pub body_properties: BTreeMap<String, Value>,
    /// Convex elements.
    pub convex: Vec<ConvexShape>,
    /// Boxes.
    pub boxes: Vec<BoxShape>,
    /// Spheres.
    pub spheres: Vec<SphereShape>,
    /// Capsules.
    pub sphyls: Vec<SphylShape>,
}

fn require<T: Clone>(v: Option<&T>, what: &str) -> Result<T> {
    v.cloned()
        .with_context(|| format!("{what} is not stored (the struct default would apply)"))
}

/// Build the shapes document of `body` (the body setup of mesh `mesh_path`).
/// `object_path` resolves the object references among its scalar
/// properties. The body must pass `validate_body_setup`: every member
/// present, every value finite, valid triangle indices.
pub fn simple_collision_doc(
    mesh_path: &str,
    body_path: &str,
    body: &BodySetup,
    object_path: &dyn Fn(asamu_ue3::PackageIndex) -> Option<String>,
) -> Result<SimpleCollisionDoc> {
    let issues = validate_body_setup(body);
    if !issues.is_empty() {
        bail!("inconsistent body setup: {}", issues.join("; "));
    }
    let mut body_properties = BTreeMap::new();
    for p in &body.properties {
        let BodyProperty::Scalar(s) = p else { continue };
        let value = match &s.value {
            ScalarValue::Bool(v) => json!(v),
            ScalarValue::Int(v) => json!(v),
            ScalarValue::Float(v) if v.is_finite() => json!(v),
            ScalarValue::Float(v) => bail!("{} is {v}", s.name),
            ScalarValue::Name { text, .. } => json!(text),
            ScalarValue::Object(o) => json!(object_path(*o)),
            ScalarValue::Byte { value, .. } => json!(value),
            ScalarValue::Enum { value, .. } => json!(value),
        };
        body_properties.insert(s.name.clone(), value);
    }
    let geom = body.agg_geom().context("the body setup has no AggGeom")?;
    let mut convex = Vec::with_capacity(geom.convex().len());
    for (i, c) in geom.convex().iter().enumerate() {
        let w = |m: &str| format!("convex {i}: {m}");
        let tris = require(c.face_tri_data.as_ref(), &w("FaceTriData"))?;
        let mut face_triangles = Vec::with_capacity(tris.len() / 3);
        for t in tris.as_chunks::<3>().0 {
            let idx =
                |k: usize| u32::try_from(t[k]).with_context(|| w("negative FaceTriData index"));
            face_triangles.push([idx(0)?, idx(1)?, idx(2)?]);
        }
        let bx = require(c.elem_box.as_ref(), &w("ElemBox"))?;
        convex.push(ConvexShape {
            vertices: require(c.vertex_data.as_ref(), &w("VertexData"))?,
            planes: require(c.face_plane_data.as_ref(), &w("FacePlaneData"))?
                .iter()
                .map(|p| [p.x, p.y, p.z, p.w])
                .collect(),
            face_triangles,
            edge_directions: require(c.edge_directions.as_ref(), &w("EdgeDirections"))?,
            face_normal_directions: require(
                c.face_normal_directions.as_ref(),
                &w("FaceNormalDirections"),
            )?,
            bounds_min: bx.min,
            bounds_max: bx.max,
        });
    }
    let mut boxes = Vec::with_capacity(geom.boxes().len());
    for (i, e) in geom.boxes().iter().enumerate() {
        let w = |m: &str| format!("box {i}: {m}");
        boxes.push(BoxShape {
            transform: require(e.tm.as_ref(), &w("TM"))?,
            size: [
                require(e.x.as_ref(), &w("X"))?,
                require(e.y.as_ref(), &w("Y"))?,
                require(e.z.as_ref(), &w("Z"))?,
            ],
            no_rb_collision: require(e.no_rb_collision.as_ref(), &w("bNoRBCollision"))?,
            per_poly_shape: require(e.per_poly_shape.as_ref(), &w("bPerPolyShape"))?,
        });
    }
    let mut spheres = Vec::with_capacity(geom.spheres().len());
    for (i, e) in geom.spheres().iter().enumerate() {
        let w = |m: &str| format!("sphere {i}: {m}");
        spheres.push(SphereShape {
            transform: require(e.tm.as_ref(), &w("TM"))?,
            radius: require(e.radius.as_ref(), &w("Radius"))?,
            no_rb_collision: require(e.no_rb_collision.as_ref(), &w("bNoRBCollision"))?,
            per_poly_shape: require(e.per_poly_shape.as_ref(), &w("bPerPolyShape"))?,
        });
    }
    let mut sphyls = Vec::with_capacity(geom.sphyls().len());
    for (i, e) in geom.sphyls().iter().enumerate() {
        let w = |m: &str| format!("sphyl {i}: {m}");
        sphyls.push(SphylShape {
            transform: require(e.tm.as_ref(), &w("TM"))?,
            radius: require(e.radius.as_ref(), &w("Radius"))?,
            length: require(e.length.as_ref(), &w("Length"))?,
            no_rb_collision: require(e.no_rb_collision.as_ref(), &w("bNoRBCollision"))?,
            per_poly_shape: require(e.per_poly_shape.as_ref(), &w("bPerPolyShape"))?,
        });
    }
    Ok(SimpleCollisionDoc {
        format: SIMPLE_COLLISION_FORMAT.to_owned(),
        version: SIMPLE_COLLISION_VERSION,
        notice: NOTICE.to_owned(),
        space: SIMPLE_COLLISION_SPACE.to_owned(),
        mesh: mesh_path.to_owned(),
        body_setup: body_path.to_owned(),
        body_properties,
        convex,
        boxes,
        spheres,
        sphyls,
    })
}

/// Check the text that will be written against the document it was made
/// from: it must parse back to the same values and serialize to the same
/// text again (so every float survives bit for bit, the sign of zero
/// included), and hold valid triangles.
pub fn validate_simple_collision(text: &str, doc: &SimpleCollisionDoc) -> Vec<String> {
    let mut issues = Vec::new();
    match serde_json::from_str::<SimpleCollisionDoc>(text) {
        Ok(back) => {
            // Value equality treats 0.0 and -0.0 alike; the text does not.
            let canonical = serde_json::to_string(doc).unwrap_or_default();
            let again = serde_json::to_string(&back).unwrap_or_default();
            if back != *doc || again != canonical || text.trim_end_matches('\n') != canonical {
                issues.push("the written shapes do not read back as decoded".to_owned());
            }
        }
        Err(e) => issues.push(format!("the written shapes do not parse: {e}")),
    }
    if doc.format != SIMPLE_COLLISION_FORMAT || doc.version != SIMPLE_COLLISION_VERSION {
        issues.push("wrong format or version".to_owned());
    }
    for (i, c) in doc.convex.iter().enumerate() {
        let n = c.vertices.len();
        if c.face_triangles
            .iter()
            .flatten()
            .any(|&v| usize::try_from(v).map_or(true, |v| v >= n))
        {
            issues.push(format!(
                "convex {i}: a triangle index is outside the {n} vertices"
            ));
        }
    }
    issues
}

/// The simple collision of one mesh: its manifest entry, the shapes file of
/// a mesh with a body setup, and what the validation found.
struct SimpleCollision {
    entry: SimpleCollisionEntry,
    file: Option<(PathBuf, Vec<u8>)>,
    issues: Vec<String>,
}

/// `<stem>.collision.json`.
fn collision_rel_path(stem: &Path) -> PathBuf {
    let name = stem
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mesh".to_owned());
    stem.with_file_name(format!("{name}.collision.json"))
}

/// The shapes document of `mesh`'s body setup (`None` without one).
fn mesh_shapes(lp: &LoadedPackage, mesh: &StaticMesh) -> Result<Option<SimpleCollisionDoc>> {
    let body_ref = mesh.native.body_setup;
    if body_ref.is_null() {
        return Ok(None);
    }
    let index = body_ref
        .export_index()
        .context("the body setup is not an export of the mesh's package")?;
    let body = decode_body_setup(&lp.package, index).context("decoding the body setup")?;
    let body_path = lp
        .ref_path(body_ref)
        .ok()
        .flatten()
        .context("the body setup has no object path")?;
    simple_collision_doc(&mesh.object.path, &body_path, &body, &|o| {
        lp.ref_path(o).ok().flatten()
    })
    .map(Some)
}

/// Convert the simple collision of `mesh`; the shapes file goes next to the
/// mesh's glTF files under `stem`.
fn convert_simple_collision(
    lp: &LoadedPackage,
    mesh: &StaticMesh,
    stem: &Path,
) -> Result<SimpleCollision> {
    let flags = mesh.simple_collision_flags();
    let doc = mesh_shapes(lp, mesh)?;
    let mut entry = SimpleCollisionEntry {
        use_simple_box_collision: flags.box_,
        use_simple_line_collision: flags.line,
        use_simple_rigid_body_collision: flags.rigid_body,
        has_body_setup: doc.is_some(),
        shapes: None,
        convex: 0,
        boxes: 0,
        spheres: 0,
        sphyls: 0,
    };
    let Some(doc) = doc else {
        return Ok(SimpleCollision {
            entry,
            file: None,
            issues: Vec::new(),
        });
    };
    // Compact on purpose: the documents are arrays of numbers.
    let mut text = serde_json::to_string(&doc)?;
    text.push('\n');
    let issues = validate_simple_collision(&text, &doc);
    let rel = collision_rel_path(stem);
    entry.shapes = Some(rel_string(&rel));
    entry.convex = doc.convex.len();
    entry.boxes = doc.boxes.len();
    entry.spheres = doc.spheres.len();
    entry.sphyls = doc.sphyls.len();
    Ok(SimpleCollision {
        entry,
        file: Some((rel, text.into_bytes())),
        issues,
    })
}

/// The manifest written to `<out>/meshes/manifest.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version.
    pub version: u32,
    /// Redistribution notice.
    pub notice: String,
    /// Axis convention of the files.
    pub coordinates: String,
    /// glTF units per Unreal unit of the last run.
    pub scale: f32,
    /// Meshes by object path.
    pub meshes: BTreeMap<String, MeshEntry>,
}

/// FNV-1a 64-bit hash (content comparison only, not security).
fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hash of a mesh's decoded source content: every LOD's positions, tangent
/// bytes, UVs, colors, indices and sections (with material paths), the
/// collision triangles, the simple-collision switches and the body setup's
/// shapes. Package indices are replaced by resolved paths, so identical
/// meshes cooked into different packages hash alike.
fn content_hash(lp: &LoadedPackage, mesh: &StaticMesh) -> String {
    let n = &mesh.native;
    let mut data: Vec<u8> = Vec::new();
    for lod in &n.lods {
        for p in &lod.positions.positions {
            p.iter()
                .for_each(|c| data.extend_from_slice(&c.to_bits().to_le_bytes()));
        }
        for (x, z) in lod.vertices.tangent_x.iter().zip(&lod.vertices.tangent_z) {
            data.extend_from_slice(&x.0);
            data.extend_from_slice(&z.0);
        }
        for ch in &lod.vertices.uvs {
            for uv in ch {
                uv.iter()
                    .for_each(|c| data.extend_from_slice(&c.to_bits().to_le_bytes()));
            }
        }
        lod.colors
            .colors_bgra
            .iter()
            .for_each(|c| data.extend_from_slice(c));
        lod.indices
            .iter()
            .for_each(|i| data.extend_from_slice(&i.to_le_bytes()));
        for s in &lod.sections {
            data.extend_from_slice(&s.first_index.to_le_bytes());
            data.extend_from_slice(&s.num_triangles.to_le_bytes());
            data.extend_from_slice(&s.enable_collision.to_le_bytes());
            let m = lp.ref_path(s.material).ok().flatten().unwrap_or_default();
            data.extend_from_slice(m.as_bytes());
            data.push(0);
        }
        data.push(0xff);
    }
    for t in &n.kdop.triangles {
        t.vertices
            .iter()
            .for_each(|v| data.extend_from_slice(&v.to_le_bytes()));
        data.extend_from_slice(&t.material_index.to_le_bytes());
    }
    // Simple collision: the switches and the body setup's shapes (as the
    // document that is written; it holds object paths, not package indices).
    let flags = mesh.simple_collision_flags();
    let shapes = match mesh_shapes(lp, mesh) {
        Ok(None) => Vec::new(),
        Ok(Some(doc)) => serde_json::to_vec(&doc).unwrap_or_else(|_| b"!unserializable".to_vec()),
        // The conversion reports the error; two failing copies compare equal.
        Err(_) => b"!undecodable".to_vec(),
    };
    let switches = [
        u8::from(flags.line),
        u8::from(flags.box_),
        u8::from(flags.rigid_body),
        u8::from(!n.body_setup.is_null()),
    ];
    format!("{:016x}", fnv1a64(&[&data, &switches, &shapes]))
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// A decoded mesh converted in memory (all requested LODs).
struct Converted {
    entry: MeshEntry,
    files: Vec<(PathBuf, Vec<u8>)>,
    issues: Vec<String>,
}

fn section_entries(lp: &LoadedPackage, lod: &LodModel) -> Vec<SectionEntry> {
    lod.sections
        .iter()
        .map(|s| SectionEntry {
            material: lp.ref_path(s.material).ok().flatten(),
            first_index: s.first_index,
            triangles: s.num_triangles,
            collision: s.enable_collision != 0,
            cast_shadow: s.enable_shadow_casting != 0,
        })
        .collect()
}

/// Convert `mesh` (export `index` of `lp`) into glTF files under `stem`
/// (relative path without extension).
fn convert_mesh(
    lp: &LoadedPackage,
    index: usize,
    mesh: &StaticMesh,
    stem: &Path,
    args: &Args,
) -> Result<Converted> {
    let n = &mesh.native;
    let opts = ConvertOptions { scale: args.scale };
    let lod_count = exported_lods(mesh, args);
    let mut lods = Vec::new();
    let mut files = Vec::new();
    let mut issues = Vec::new();
    for (li, lod) in n.lods.iter().take(lod_count).enumerate() {
        let (gltf_rel, bin_rel) = lod_rel_paths(stem, li);
        let bin_name = bin_rel
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sections = section_entries(lp, lod);
        let collision = (args.collision && li == 0).then_some(n.kdop.triangles.as_slice());
        let asset = build_gltf(
            &mesh.object.path,
            lod,
            &sections,
            collision,
            &opts,
            &bin_name,
        )?;
        let text = serde_json::to_string_pretty(&asset.json)?;
        // Validate what will be written: re-parse the text and check it
        // against the buffer.
        let reparsed: Value = serde_json::from_str(&text)?;
        for m in validate_gltf(&reparsed, &asset.bin) {
            issues.push(format!("LOD {li}: {m}"));
        }
        lods.push(LodEntry {
            lod: li,
            gltf: rel_string(&gltf_rel),
            bin: rel_string(&bin_rel),
            sections,
            stats: asset.stats,
        });
        files.push((gltf_rel, text.into_bytes()));
        files.push((bin_rel, asset.bin));
    }
    let simple = convert_simple_collision(lp, mesh, stem).context("simple collision")?;
    issues.extend(
        simple
            .issues
            .iter()
            .map(|m| format!("simple collision: {m}")),
    );
    files.extend(simple.file);
    let hash = content_hash(lp, mesh);
    let entry = MeshEntry {
        package: lp.name.clone(),
        export_index: index,
        scale: args.scale,
        also_in: Vec::new(),
        differs_in: Vec::new(),
        lod_count: n.lods.len(),
        lods,
        light_map_coordinate_index: mesh.light_map_coordinate_index(),
        light_map_resolution: mesh.light_map_resolution(),
        body_setup: lp.ref_path(n.body_setup).ok().flatten(),
        collision_triangles: n.kdop.triangles.len(),
        simple_collision: simple.entry,
        bounds_ue: BoundsEntry {
            origin: n.bounds.origin,
            box_extent: n.bounds.box_extent,
            sphere_radius: n.bounds.sphere_radius,
        },
        content_hash: hash,
    };
    Ok(Converted {
        entry,
        files,
        issues,
    })
}

#[derive(Debug, Default)]
struct RunStats {
    meshes: usize,
    written: usize,
    kept: usize,
    duplicates: usize,
    variants: usize,
    failed: usize,
    invalid: usize,
    lods: usize,
    vertices: u64,
    triangles: u64,
    normals_fixed: u64,
    tangents_fixed: u64,
    winding_agree: u64,
    winding_disagree: u64,
    /// Meshes with a body setup (a shapes file).
    bodies: usize,
    convex: usize,
    boxes: usize,
    spheres: usize,
    sphyls: usize,
    /// Meshes whose box / line switch is off.
    box_off: usize,
    line_off: usize,
    /// Meshes without a body setup whose box switch is on: swept traces
    /// find nothing to hit on them.
    no_body_box_on: usize,
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    if !(args.scale.is_finite() && args.scale > 0.0) {
        bail!("--scale must be a positive finite number");
    }
    let (cooked, maps) = cooked_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps];
    let files = package_files(&dirs, &args.packages);
    if files.is_empty() {
        bail!("no package matches the --package filters");
    }
    let root = if args.check {
        None
    } else {
        Some(prepare_out_dir(
            &ctx.out,
            files.first().map(PathBuf::as_path),
        )?)
    };
    let mut manifest = match &root {
        Some(r) => read_manifest(&r.join("manifest.json"))?,
        None => Manifest::default(),
    };
    let mut stats = RunStats::default();
    let mut claims = Claims::from_manifest(&manifest);
    // Content hash of each path converted in this run.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    'packages: for file in &files {
        // A fresh set per package keeps memory bounded (maps are large).
        let set = PackageSet::new(&dirs);
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        for index in 0..lp.package.exports.len() {
            if !is_static_mesh(&lp.package, index) {
                continue;
            }
            if args.limit.is_some_and(|l| stats.meshes >= l) {
                break 'packages;
            }
            let mesh = match decode_static_mesh(&lp.package, Some(&lp.name), index, &set) {
                Ok(m) => m,
                Err(e) => {
                    stats.failed += 1;
                    eprintln!("asamu-import: {} export {index}: {e}", lp.name);
                    continue;
                }
            };
            let path = mesh.object.path.clone();
            if let Some(filter) = &args.name
                && !path
                    .to_ascii_lowercase()
                    .contains(&filter.to_ascii_lowercase())
            {
                continue;
            }
            let vctx = ValidationContext {
                payload_stream_offset: lp
                    .package
                    .export(index)
                    .ok()
                    .map(|e| i64::from(e.serial_offset)),
                imports: lp.package.imports.len(),
                exports: lp.package.exports.len(),
            };
            let structural = validate_static_mesh(&mesh.native, &vctx);
            if !structural.is_empty() {
                stats.failed += 1;
                eprintln!(
                    "asamu-import: {path} ({}): {}",
                    lp.name,
                    structural.join("; ")
                );
                continue;
            }
            let key = path.to_ascii_lowercase();
            let mut stem = relative_stem(&lp.name, &path);
            let previous = manifest.meshes.get(&path).cloned();
            let duplicate_of_other_package = previous
                .as_ref()
                .is_some_and(|e| !e.package.eq_ignore_ascii_case(&lp.name));
            if duplicate_of_other_package && let Some(prev) = previous.as_ref() {
                let hash = content_hash(&lp, &mesh);
                let first_hash = seen.get(&key).unwrap_or(&prev.content_hash);
                let entry = manifest.meshes.get_mut(&path);
                if *first_hash == hash {
                    if let Some(e) = entry
                        && !e.also_in.iter().any(|p| p.eq_ignore_ascii_case(&lp.name))
                    {
                        e.also_in.push(lp.name.clone());
                    }
                    stats.duplicates += 1;
                    continue;
                }
                // Same path, different content: write a variant next to it.
                if let Some(e) = entry
                    && !e
                        .differs_in
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&lp.name))
                {
                    e.differs_in.push(lp.name.clone());
                }
                stats.variants += 1;
                let name = stem
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                stem.set_file_name(format!("{name}@{}", sanitize(&lp.name)));
                let vkey = format!("{path}@{}", lp.name);
                let Some(stem) = claims.choose(&vkey, stem, exported_lods(&mesh, &args)) else {
                    stats.failed += 1;
                    eprintln!(
                        "asamu-import: {path} ({}): no free output file name",
                        lp.name
                    );
                    continue;
                };
                let variant = match convert_mesh(&lp, index, &mesh, &stem, &args) {
                    Ok(v) => v,
                    Err(e) => {
                        stats.failed += 1;
                        eprintln!("asamu-import: {path} ({}): {e:#}", lp.name);
                        continue;
                    }
                };
                if !variant.issues.is_empty() {
                    stats.invalid += 1;
                    eprintln!(
                        "asamu-import: {path} ({}): glTF validation: {}",
                        lp.name,
                        variant.issues.join("; ")
                    );
                }
                write_converted(&root, &variant, file, &args, &mut stats)?;
                account(&mut stats, &variant);
                claims.claim(&vkey, &variant.files);
                manifest.meshes.insert(vkey, variant.entry);
                continue;
            }
            let Some(stem) = claims.choose(&path, stem, exported_lods(&mesh, &args)) else {
                stats.failed += 1;
                eprintln!(
                    "asamu-import: {path} ({}): no free output file name",
                    lp.name
                );
                continue;
            };
            let converted = match convert_mesh(&lp, index, &mesh, &stem, &args) {
                Ok(c) => c,
                Err(e) => {
                    stats.failed += 1;
                    eprintln!("asamu-import: {path} ({}): {e:#}", lp.name);
                    continue;
                }
            };
            if !converted.issues.is_empty() {
                stats.invalid += 1;
                eprintln!(
                    "asamu-import: {path} ({}): glTF validation: {}",
                    lp.name,
                    converted.issues.join("; ")
                );
            }
            stats.meshes += 1;
            account(&mut stats, &converted);
            write_converted(&root, &converted, file, &args, &mut stats)?;
            claims.claim(&path, &converted.files);
            seen.insert(key, converted.entry.content_hash.clone());
            let mut entry = converted.entry;
            if let Some(prev) = previous
                && prev.package.eq_ignore_ascii_case(&lp.name)
            {
                entry.also_in = prev.also_in;
                entry.differs_in = prev.differs_in;
            }
            manifest.meshes.insert(path, entry);
        }
    }
    print_summary(&stats, &args, manifest.meshes.len());
    if let Some(root) = &root {
        manifest.version = MANIFEST_VERSION;
        manifest.notice = NOTICE.to_owned();
        manifest.coordinates = COORDINATES.to_owned();
        manifest.scale = args.scale;
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
            "{} meshes failed to convert, {} failed glTF validation",
            stats.failed,
            stats.invalid
        );
    }
    Ok(())
}

fn account(stats: &mut RunStats, c: &Converted) {
    let sc = &c.entry.simple_collision;
    stats.bodies += usize::from(sc.has_body_setup);
    stats.convex += sc.convex;
    stats.boxes += sc.boxes;
    stats.spheres += sc.spheres;
    stats.sphyls += sc.sphyls;
    stats.box_off += usize::from(!sc.use_simple_box_collision);
    stats.line_off += usize::from(!sc.use_simple_line_collision);
    stats.no_body_box_on += usize::from(!sc.has_body_setup && sc.use_simple_box_collision);
    for l in &c.entry.lods {
        stats.lods += 1;
        stats.vertices += u64::try_from(l.stats.vertices).unwrap_or(0);
        stats.triangles += l.stats.triangles;
        stats.normals_fixed += u64::try_from(l.stats.normals_fixed).unwrap_or(0);
        stats.tangents_fixed += u64::try_from(l.stats.tangents_fixed).unwrap_or(0);
        stats.winding_agree += l.stats.winding_agree;
        stats.winding_disagree += l.stats.winding_disagree;
    }
}

fn write_converted(
    root: &Option<PathBuf>,
    c: &Converted,
    input: &Path,
    args: &Args,
    stats: &mut RunStats,
) -> Result<()> {
    let Some(root) = root else { return Ok(()) };
    let mut wrote_any = false;
    for (rel, data) in &c.files {
        wrote_any |= write_file(root, rel, data, input, args.force)?;
    }
    if wrote_any {
        stats.written += 1;
    } else {
        stats.kept += 1;
    }
    Ok(())
}

fn print_summary(s: &RunStats, args: &Args, manifest_len: usize) {
    let mode = if args.check { "checked" } else { "converted" };
    println!(
        "meshes: {} {mode} ({} LODs, {} vertices, {} triangles); {} written, {} kept; \
         {} identical duplicates, {} differing variants; {} failed, {} invalid glTF",
        s.meshes,
        s.lods,
        s.vertices,
        s.triangles,
        s.written,
        s.kept,
        s.duplicates,
        s.variants,
        s.failed,
        s.invalid
    );
    println!(
        "normals replaced (zero in source): {}; tangents replaced: {}; winding vs normals: \
         {} agree, {} disagree; manifest entries: {manifest_len}",
        s.normals_fixed, s.tangents_fixed, s.winding_agree, s.winding_disagree
    );
    println!(
        "simple collision: {} meshes with a body setup ({} convex elements, {} boxes, {} spheres, \
         {} capsules); box switch off on {}, line switch off on {}; no body setup and box \
         switch on: {}",
        s.bodies, s.convex, s.boxes, s.spheres, s.sphyls, s.box_off, s.line_off, s.no_body_box_on
    );
}

// ---------------------------------------------------------------------------
// Files and paths
// ---------------------------------------------------------------------------

/// `CookedMac` (or the equivalent) and its `Maps` folder.
fn cooked_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.maps_dir))
}

/// Package files of `dirs` (cooked folder first, then maps), filtered by
/// case-insensitive substrings of the file name.
fn package_files(dirs: &[PathBuf], filters: &[String]) -> Vec<PathBuf> {
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
            .filter(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                filters.is_empty()
                    || filters
                        .iter()
                        .any(|f| name.contains(&f.to_ascii_lowercase()))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// Validate `<out>/meshes` with the safety rules before creating anything,
/// then create it.
fn prepare_out_dir(out: &Path, input: Option<&Path>) -> Result<PathBuf> {
    let root = out.join("meshes");
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
    let input = input.unwrap_or(out);
    // The probe name never exists; the check applies the repository and
    // install rules to the deepest existing ancestor.
    safety::check_output_path(&existing.join(".asamu-import-meshes-probe"), input, false)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    safety::check_output_path(&root.join("manifest.json"), input, true)
        .with_context(|| format!("refusing output directory {}", root.display()))?;
    Ok(root)
}

/// The existing manifest of an output folder (empty when there is none). A
/// manifest of another format version is refused instead of being merged:
/// its entries lack fields this version writes and its content hashes are
/// not comparable.
fn read_manifest(path: &Path) -> Result<Manifest> {
    match std::fs::read(path) {
        Ok(bytes) => {
            #[derive(Deserialize)]
            struct Version {
                #[serde(default)]
                version: u32,
            }
            let found: Version = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a mesh manifest", path.display()))?;
            if found.version != MANIFEST_VERSION {
                bail!(
                    "{} has manifest version {}, expected {MANIFEST_VERSION} (convert into a \
                     fresh directory, or let `asamu-import all` rebuild the stage)",
                    path.display(),
                    found.version
                );
            }
            serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a mesh manifest", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Longest output path component made from a name.
const MAX_COMPONENT: usize = 96;

/// A file-system-safe path component (also avoids Windows device names and
/// over-long names).
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
    // Keep components well below the common 255-byte limit, leaving room for
    // `_LOD<n>`, `@<Package>`, `~<hash>` and the extension.
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

/// Relative output path (without extension) for `object_path` of `package`.
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

/// Number of LODs written for `mesh`.
fn exported_lods(mesh: &StaticMesh, args: &Args) -> usize {
    if args.all_lods {
        mesh.native.lods.len()
    } else {
        1
    }
}

/// `.gltf` and `.bin` paths of LOD `li` for `stem` (`<Name>`, `<Name>_LOD<n>`).
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

/// Output files in use: lower-case relative path -> manifest key. Lower case
/// because the default file systems of macOS and Windows ignore case, so two
/// object paths that differ only in case (or that sanitise to the same name,
/// or a mesh called `X_LOD1` next to LOD 1 of `X`) would otherwise share a
/// file and the manifest would point both entries at one mesh.
#[derive(Debug, Default)]
struct Claims(BTreeMap<String, String>);

impl Claims {
    /// Files listed by an existing manifest (earlier runs).
    fn from_manifest(m: &Manifest) -> Self {
        let mut c = Claims::default();
        for (key, e) in &m.meshes {
            for l in &e.lods {
                for f in [&l.gltf, &l.bin] {
                    c.0.entry(f.to_ascii_lowercase())
                        .or_insert_with(|| key.clone());
                }
            }
            if let Some(f) = &e.simple_collision.shapes {
                c.0.entry(f.to_ascii_lowercase())
                    .or_insert_with(|| key.clone());
            }
        }
        c
    }

    /// Every file a mesh under `stem` may write: the LOD files and the
    /// shapes file (whether or not the mesh turns out to have a body setup).
    fn planned(stem: &Path, lods: usize) -> Vec<String> {
        (0..lods)
            .flat_map(|li| {
                let (g, b) = lod_rel_paths(stem, li);
                [rel_string(&g), rel_string(&b)]
            })
            .chain([rel_string(&collision_rel_path(stem))])
            .map(|f| f.to_ascii_lowercase())
            .collect()
    }

    fn free_for(&self, key: &str, files: &[String]) -> bool {
        files
            .iter()
            .all(|f| self.0.get(f).is_none_or(|owner| owner == key))
    }

    /// `stem` when its files are free (or already `key`'s), otherwise the
    /// stem with a `~<hash of key>` suffix; `None` when both are taken.
    fn choose(&self, key: &str, stem: PathBuf, lods: usize) -> Option<PathBuf> {
        if self.free_for(key, &Self::planned(&stem, lods)) {
            return Some(stem);
        }
        let name = stem
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let h = fnv1a64(&[key.to_ascii_lowercase().as_bytes()]);
        let alt = stem.with_file_name(format!("{name}~{:08x}", h >> 32));
        self.free_for(key, &Self::planned(&alt, lods))
            .then_some(alt)
    }

    fn claim(&mut self, key: &str, files: &[(PathBuf, Vec<u8>)]) {
        for (rel, _) in files {
            self.0
                .insert(rel_string(rel).to_ascii_lowercase(), key.to_owned());
        }
    }
}

/// Create `root/rel_dir` one component at a time without following links:
/// every component must be a plain name, an existing component must be a
/// real directory (a symlink is refused even when it points to one), and
/// each new directory passes the safety rules before it is created. `root`
/// is the canonical output root from [`prepare_out_dir`].
fn ensure_dirs(root: &Path, rel_dir: &Path, input: &Path) -> Result<PathBuf> {
    let mut cur = root.to_path_buf();
    for comp in rel_dir.components() {
        let std::path::Component::Normal(name) = comp else {
            bail!(
                "refusing output path component {:?} in {}",
                comp.as_os_str(),
                rel_dir.display()
            );
        };
        let next = cur.join(name);
        let is_real_dir =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        match std::fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => bail!(
                "{} exists and is not a directory (links inside the output tree are refused)",
                next.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let checked = safety::check_output_path(&next, input, false)?;
                if let Err(e) = std::fs::create_dir(&checked)
                    && !(e.kind() == std::io::ErrorKind::AlreadyExists && is_real_dir(&checked))
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

/// Write `data` to `root/rel` through the safety checks. Returns false when
/// the file exists and `force` is off (it is kept, even when it is a link:
/// nothing is written through it).
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

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_ue3::PackageIndex;
    use asamu_ue3::bulkdata::BulkDataRecord;
    use asamu_ue3::staticmesh::{
        BoxSphereBounds, ColorBuffer, FragmentRange, KdopBounds, KdopTree, MeshSection,
        PackedNormal, PositionBuffer, StaticMeshNative, VertexBuffer, decode_static_mesh_native,
        encode_static_mesh_native,
    };
    use asamu_ue3::types::Guid;

    /// Unit quad in the UE3 XY plane, normal +Z (UE3), UVs spanning [0, 1],
    /// wound like the shipped data (cross(b - a, c - a) opposes the normal).
    fn quad(colors: bool, w_byte: u8) -> LodModel {
        let positions = vec![
            [0.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [10.0, 10.0, 0.0],
            [10.0, 0.0, 0.0],
        ];
        // UV u grows along +Y (UE3), v grows along +X: TangentX = +Y. With
        // Z = +Z and X = +Y, cross(Z, X) = -X, so dP/dv = +X needs W = -1.
        let uvs = vec![vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]];
        LodModel {
            raw_triangles: BulkDataRecord {
                flags: 0,
                element_count: 0,
                size_on_disk: 0,
                offset_in_file: 0,
                header_offset: 0,
            },
            sections: vec![
                MeshSection {
                    material: PackageIndex(-1),
                    enable_collision: 1,
                    old_enable_collision: 1,
                    enable_shadow_casting: 1,
                    first_index: 0,
                    num_triangles: 1,
                    min_vertex_index: 0,
                    max_vertex_index: 2,
                    material_index: 0,
                    fragments: vec![FragmentRange {
                        base_index: 0,
                        num_primitives: 1,
                    }],
                },
                MeshSection {
                    material: PackageIndex(-1),
                    enable_collision: 0,
                    old_enable_collision: 0,
                    enable_shadow_casting: 1,
                    first_index: 3,
                    num_triangles: 1,
                    min_vertex_index: 0,
                    max_vertex_index: 3,
                    material_index: 1,
                    fragments: vec![],
                },
            ],
            positions: PositionBuffer {
                stride: 12,
                num_vertices: 4,
                positions,
            },
            vertices: VertexBuffer {
                num_tex_coords: 1,
                stride: 12,
                num_vertices: 4,
                full_precision_uvs: false,
                tangent_x: vec![PackedNormal([128, 255, 128, 128]); 4],
                tangent_z: vec![PackedNormal([128, 128, 255, w_byte]); 4],
                uvs,
            },
            colors: ColorBuffer {
                stride: if colors { 4 } else { 0 },
                num_vertices: if colors { 4 } else { 0 },
                colors_bgra: if colors {
                    vec![[10, 20, 30, 40]; 4]
                } else {
                    vec![]
                },
            },
            num_vertices: 4,
            // cross(b - a, c - a) for (0, 1, 2) = (0,10,0) x (10,10,0) = -Z: opposes +Z.
            indices: vec![0, 1, 2, 0, 2, 3],
            wireframe_indices: vec![],
            adjacency_indices: vec![],
        }
    }

    fn sections(lod: &LodModel, names: &[Option<&str>]) -> Vec<SectionEntry> {
        lod.sections
            .iter()
            .zip(names)
            .map(|(s, n)| SectionEntry {
                material: n.map(str::to_owned),
                first_index: s.first_index,
                triangles: s.num_triangles,
                collision: s.enable_collision != 0,
                cast_shadow: true,
            })
            .collect()
    }

    fn read_f32s(asset: &GltfAsset, accessor: usize) -> Vec<f32> {
        let a = &asset.json["accessors"][accessor];
        let v = &asset.json["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
        let off = v["byteOffset"].as_u64().unwrap() as usize
            + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let n = a["count"].as_u64().unwrap() as usize
            * type_components(a["type"].as_str().unwrap()).unwrap();
        (0..n)
            .map(|i| {
                let b = &asset.bin[off + 4 * i..off + 4 * i + 4];
                f32::from_le_bytes([b[0], b[1], b[2], b[3]])
            })
            .collect()
    }

    fn attr(asset: &GltfAsset, name: &str) -> usize {
        asset.json["meshes"][0]["primitives"][0]["attributes"][name]
            .as_u64()
            .unwrap() as usize
    }

    #[test]
    fn axes_map_like_asamu_core() {
        // (x, y, z)_ue -> (y, z, -x): forward +X -> -Z, right +Y -> +X, up +Z -> +Y.
        assert_eq!(ue_to_gltf([1.0, 0.0, 0.0]), [0.0, 0.0, -1.0]);
        assert_eq!(ue_to_gltf([0.0, 1.0, 0.0]), [1.0, 0.0, 0.0]);
        assert_eq!(ue_to_gltf([0.0, 0.0, 1.0]), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn quad_converts_and_validates() {
        let lod = quad(true, 0);
        let secs = sections(&lod, &[Some("Pkg.Mat_A"), None]);
        let asset = build_gltf(
            "Pkg.Quad",
            &lod,
            &secs,
            None,
            &ConvertOptions { scale: 2.0 },
            "Quad.bin",
        )
        .unwrap();
        assert!(validate_gltf(&asset.json, &asset.bin).is_empty());
        // Positions: UE (10, 0, 0) * 2 -> glTF (0, 0, -20).
        let p = read_f32s(&asset, attr(&asset, "POSITION"));
        assert_eq!(&p[9..12], &[0.0, 0.0, -20.0]);
        assert_eq!(asset.json["accessors"][0]["min"], json!([0.0, 0.0, -20.0]));
        assert_eq!(asset.json["accessors"][0]["max"], json!([20.0, 0.0, 0.0]));
        // Normal UE +Z -> glTF +Y (byte 128 unpacks to 1/255, not 0).
        let n = read_f32s(&asset, attr(&asset, "NORMAL"));
        assert!(n[1] > 0.9999 && n[0].abs() < 0.005 && n[2].abs() < 0.005);
        // Winding: kept order, and it agrees with the normals in glTF space.
        assert_eq!(asset.stats.winding_agree, 2);
        assert_eq!(asset.stats.winding_disagree, 0);
        // Two primitives, two materials named after the UE3 paths.
        assert_eq!(asset.stats.primitives, 2);
        assert_eq!(asset.json["materials"][0]["name"], "Pkg.Mat_A");
        assert_eq!(asset.json["materials"][1]["name"], "None");
        // Colors are RGBA.
        let c = attr(&asset, "COLOR_0");
        let a = &asset.json["accessors"][c];
        let v = &asset.json["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
        let off = v["byteOffset"].as_u64().unwrap() as usize;
        assert_eq!(&asset.bin[off..off + 4], &[30, 20, 10, 40]);
        assert_eq!(asset.json["buffers"][0]["uri"], "Quad.bin");
    }

    /// The tangent frame written to glTF matches the UV derivatives: tangent
    /// along dP/du and bitangent cross(N, T) * w along dP/dv.
    #[test]
    fn tangent_handedness_follows_the_uvs() {
        for (w_byte, expect_ok) in [(0u8, true), (255u8, false)] {
            let lod = quad(false, w_byte);
            let secs = sections(&lod, &[None, None]);
            let asset = build_gltf(
                "Q",
                &lod,
                &secs,
                None,
                &ConvertOptions { scale: 1.0 },
                "Q.bin",
            )
            .unwrap();
            let p = read_f32s(&asset, attr(&asset, "POSITION"));
            let n = read_f32s(&asset, attr(&asset, "NORMAL"));
            let t = read_f32s(&asset, attr(&asset, "TANGENT"));
            let pos = |i: usize| [p[3 * i], p[3 * i + 1], p[3 * i + 2]];
            // UVs: v0 (0,0), v1 (1,0), v3 (0,1): dP/du = P1 - P0, dP/dv = P3 - P0.
            let dpdu = sub3(pos(1), pos(0));
            let dpdv = sub3(pos(3), pos(0));
            let nn = [n[0], n[1], n[2]];
            let tt = [t[0], t[1], t[2]];
            assert!(dot3(tt, dpdu) > 0.0, "tangent follows dP/du");
            let b = cross3(nn, tt).map(|c| c * t[3]);
            assert_eq!(dot3(b, dpdv) > 0.0, expect_ok, "w byte {w_byte}");
        }
    }

    #[test]
    fn collision_node_and_zero_normals() {
        let mut lod = quad(false, 0);
        lod.vertices.tangent_z[2] = PackedNormal([127, 128, 128, 0]); // ~zero normal
        lod.vertices.tangent_x[3] = PackedNormal([128, 128, 255, 128]); // parallel to normal
        let secs = sections(&lod, &[None, None]);
        let coll = [CollisionTriangle {
            vertices: [0, 1, 2],
            material_index: 0,
        }];
        let asset = build_gltf(
            "Q",
            &lod,
            &secs,
            Some(&coll),
            &ConvertOptions { scale: 1.0 },
            "Q.bin",
        )
        .unwrap();
        assert!(validate_gltf(&asset.json, &asset.bin).is_empty());
        assert_eq!(asset.stats.normals_fixed, 1);
        assert_eq!(asset.stats.tangents_fixed, 1);
        assert_eq!(asset.json["nodes"][1]["name"], "UCX_Q");
        assert_eq!(asset.json["scenes"][0]["nodes"], json!([0, 1]));
        assert_eq!(asset.stats.collision_triangles, 1);
        // The replacement normal is the face normal: glTF +Y.
        let n = read_f32s(&asset, attr(&asset, "NORMAL"));
        assert_eq!(&n[6..9], &[0.0, 1.0, 0.0]);
        // A collision triangle with a missing vertex is refused.
        let bad = [CollisionTriangle {
            vertices: [0, 1, 9],
            material_index: 0,
        }];
        assert!(
            build_gltf(
                "Q",
                &lod,
                &secs,
                Some(&bad),
                &ConvertOptions { scale: 1.0 },
                "Q.bin"
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_lods_are_refused() {
        let lod = quad(false, 0);
        let secs = sections(&lod, &[None, None]);
        let o = ConvertOptions { scale: 1.0 };
        let mut l = lod.clone();
        l.indices[4] = 7;
        assert!(build_gltf("Q", &l, &secs, None, &o, "Q.bin").is_err());
        let mut l = lod.clone();
        l.sections[1].num_triangles = 4;
        assert!(build_gltf("Q", &l, &sections(&l, &[None, None]), None, &o, "Q.bin").is_err());
        let mut l = lod.clone();
        l.positions.positions[0][0] = f32::NAN;
        assert!(build_gltf("Q", &l, &secs, None, &o, "Q.bin").is_err());
        let mut l = lod.clone();
        l.vertices.uvs[0].pop();
        assert!(build_gltf("Q", &l, &secs, None, &o, "Q.bin").is_err());
        let mut l = lod.clone();
        for s in &mut l.sections {
            s.num_triangles = 0;
        }
        assert!(build_gltf("Q", &l, &sections(&l, &[None, None]), None, &o, "Q.bin").is_err());
        assert!(build_gltf("Q", &lod, &secs[..1], None, &o, "Q.bin").is_err());
        assert!(
            build_gltf(
                "Q",
                &lod,
                &secs,
                None,
                &ConvertOptions { scale: 0.0 },
                "Q.bin"
            )
            .is_err()
        );
    }

    #[test]
    fn validator_catches_corruption() {
        let lod = quad(true, 0);
        let secs = sections(&lod, &[None, None]);
        let good = build_gltf(
            "Q",
            &lod,
            &secs,
            None,
            &ConvertOptions { scale: 1.0 },
            "Q.bin",
        )
        .unwrap();
        let expect = |f: &dyn Fn(&mut Value, &mut Vec<u8>), needle: &str| {
            let mut doc = good.json.clone();
            let mut bin = good.bin.clone();
            f(&mut doc, &mut bin);
            let issues = validate_gltf(&doc, &bin);
            assert!(
                issues.iter().any(|i| i.contains(needle)),
                "'{needle}' not in {issues:?}"
            );
        };
        expect(&|_, b| b.truncate(b.len() - 4), "byteLength");
        expect(&|d, _| d["accessors"][0]["max"][0] = json!(99.0), "max");
        expect(&|d, _| d["accessors"][1]["count"] = json!(3), "elements");
        expect(
            &|d, _| d["accessors"][0]["count"] = json!(1000),
            "accessor 0 is invalid",
        );
        expect(
            &|d, _| d["meshes"][0]["primitives"][0]["material"] = json!(9),
            "material",
        );
        expect(&|d, _| d["nodes"][0]["mesh"] = json!(5), "missing mesh");
        expect(&|d, _| d["asset"]["version"] = json!("1.0"), "2.0");
        // An index past the vertex count.
        let idx_acc = good.json["meshes"][0]["primitives"][0]["indices"]
            .as_u64()
            .unwrap() as usize;
        let view = good.json["accessors"][idx_acc]["bufferView"]
            .as_u64()
            .unwrap() as usize;
        let off = good.json["bufferViews"][view]["byteOffset"]
            .as_u64()
            .unwrap() as usize;
        expect(
            &|_, b| b[off..off + 2].copy_from_slice(&40u16.to_le_bytes()),
            "index out of range",
        );
        // A non-unit normal.
        let nacc = attr(&good, "NORMAL");
        let view = good.json["accessors"][nacc]["bufferView"].as_u64().unwrap() as usize;
        let off = good.json["bufferViews"][view]["byteOffset"]
            .as_u64()
            .unwrap() as usize;
        expect(
            &|_, b| b[off..off + 4].copy_from_slice(&3.0f32.to_le_bytes()),
            "normals are not unit",
        );
        // A tangent with w = 0.
        let tacc = attr(&good, "TANGENT");
        let view = good.json["accessors"][tacc]["bufferView"].as_u64().unwrap() as usize;
        let off = good.json["bufferViews"][view]["byteOffset"]
            .as_u64()
            .unwrap() as usize;
        expect(
            &|_, b| b[off + 12..off + 16].copy_from_slice(&0.0f32.to_le_bytes()),
            "tangents",
        );
        // Hostile JSON never panics.
        for doc in [
            json!(null),
            json!({}),
            json!({"buffers": [{}], "accessors": [{"bufferView": 99}]}),
        ] {
            let _ = validate_gltf(&doc, &[]);
        }
    }

    #[test]
    fn output_paths_are_sanitized() {
        let p = relative_stem("AG-Darkcave", "Foo.Bar baz.../x");
        assert_eq!(rel_string(&p), "AG-Darkcave/Foo/Bar_baz/_/_/_x");
        assert!(!rel_string(&relative_stem("..", "..")).contains(".."));
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("COM1"), "_COM1");
        assert_eq!(sanitize("Console"), "Console");
    }

    fn native_from(lod: LodModel) -> StaticMeshNative {
        StaticMeshNative {
            start: 0,
            bounds: BoxSphereBounds {
                origin: [5.0, 5.0, 0.0],
                box_extent: [5.0, 5.0, 0.0],
                sphere_radius: 8.0,
            },
            body_setup: PackageIndex(0),
            kdop: KdopTree {
                root_bounds: KdopBounds {
                    min: [0.0; 3],
                    max: [10.0, 10.0, 0.0],
                },
                nodes: vec![],
                triangles: vec![CollisionTriangle {
                    vertices: [0, 1, 2],
                    material_index: 0,
                }],
            },
            internal_version: 18,
            source_data: None,
            optimization_settings: vec![],
            has_been_simplified: 0,
            is_mesh_proxy: 0,
            lods: vec![lod.clone(), lod],
            lod_info_count: 2,
            thumbnail_angle: [0; 3],
            thumbnail_distance: 0.0,
            high_res_source_mesh_name: String::new(),
            high_res_source_mesh_crc: 0,
            lighting_guid: Guid {
                a: 1,
                b: 2,
                c: 3,
                d: 4,
            },
            vertex_position_version: 0,
            cached_streaming_texture_factors: vec![],
            remove_degenerates: 1,
            per_lod_static_lighting_for_instancing: 0,
            console_prealloc_instance_count: 0,
        }
    }

    fn plain_sections(lod: &LodModel) -> Vec<SectionEntry> {
        lod.sections
            .iter()
            .map(|s| SectionEntry {
                material: None,
                first_index: s.first_index,
                triangles: s.num_triangles,
                collision: s.enable_collision != 0,
                cast_shadow: s.enable_shadow_casting != 0,
            })
            .collect()
    }

    /// Mutated synthetic payloads go through the importer's pipeline
    /// (decode, structural validation, glTF build, re-parse, glTF
    /// validation). Nothing panics, and every glTF the converter accepts to
    /// build passes the validator, as in `convert_mesh`.
    #[test]
    fn hostile_payloads_never_break_the_converter() {
        let base = encode_static_mesh_native(&native_from(quad(true, 255))).unwrap();
        let ctx = ValidationContext {
            payload_stream_offset: None,
            imports: 4,
            exports: 4,
        };
        let n = decode_static_mesh_native(&base, 0).unwrap();
        assert!(validate_static_mesh(&n, &ctx).is_empty());
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let opts = ConvertOptions { scale: 1.0 };
        let (mut built, mut skipped) = (0usize, 0usize);
        for _ in 0..20_000 {
            let mut b = base.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() % b.len() as u64) as usize;
                match next() % 3 {
                    0 => b[i] = next() as u8,
                    1 => b[i] ^= 1 << (next() % 8),
                    _ => {
                        if i + 4 <= b.len() {
                            let v =
                                [i32::MAX, i32::MIN, -1, 0, 1, 0x7f80_0000][(next() % 6) as usize];
                            b[i..i + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
            }
            let Ok(n) = decode_static_mesh_native(&b, 0) else {
                continue;
            };
            let valid = validate_static_mesh(&n, &ctx).is_empty();
            for (li, lod) in n.lods.iter().enumerate() {
                let coll = (li == 0).then_some(n.kdop.triangles.as_slice());
                let Ok(asset) = build_gltf("Q", lod, &plain_sections(lod), coll, &opts, "Q.bin")
                else {
                    skipped += 1;
                    continue;
                };
                if !valid {
                    // The importer never converts these; only "no panic" matters.
                    continue;
                }
                built += 1;
                let text = serde_json::to_string(&asset.json).unwrap();
                let reparsed: Value = serde_json::from_str(&text).unwrap();
                let issues = validate_gltf(&reparsed, &asset.bin);
                assert!(issues.is_empty(), "{issues:?}");
            }
        }
        assert!(
            built > 2_000,
            "only {built} assets built ({skipped} refused)"
        );
    }

    #[test]
    fn extreme_section_values_are_refused() {
        let o = ConvertOptions { scale: 1.0 };
        for (first, tris) in [
            (u32::MAX, u32::MAX),
            (0, u32::MAX),
            (u32::MAX, 1),
            (u32::MAX - 2, 1),
            (5, 1),
        ] {
            let mut l = quad(false, 0);
            l.sections[0].first_index = first;
            l.sections[0].num_triangles = tris;
            let secs = plain_sections(&l);
            assert!(
                build_gltf("Q", &l, &secs, None, &o, "Q.bin").is_err(),
                "{first} {tris}"
            );
        }
        // Huge but finite coordinates and scale: refused when the product
        // overflows, converted (with a valid document) otherwise.
        let mut l = quad(false, 0);
        l.positions.positions[2] = [f32::MAX, -f32::MAX, f32::MAX];
        let secs = plain_sections(&l);
        let a = build_gltf("Q", &l, &secs, None, &o, "Q.bin").unwrap();
        assert!(validate_gltf(&a.json, &a.bin).is_empty());
        let big = ConvertOptions { scale: 4.0 };
        assert!(build_gltf("Q", &l, &secs, None, &big, "Q.bin").is_err());
        for scale in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            let bad = ConvertOptions { scale };
            assert!(build_gltf("Q", &quad(false, 0), &secs, None, &bad, "Q.bin").is_err());
        }
    }

    #[test]
    fn long_names_are_capped() {
        let long_a = format!("{}A", "x".repeat(300));
        let long_b = format!("{}B", "x".repeat(300));
        let (a, b) = (sanitize(&long_a), sanitize(&long_b));
        assert!(a.len() <= MAX_COMPONENT && b.len() <= MAX_COMPONENT);
        assert_ne!(a, b);
        assert_eq!(a, sanitize(&long_a));
        assert_eq!(
            sanitize(&"y".repeat(MAX_COMPONENT)),
            "y".repeat(MAX_COMPONENT)
        );
        // Worst case: capped name, variant suffix, LOD suffix, hash and extension.
        let stem = relative_stem(&long_a, &format!("{long_a}.{long_b}"));
        let name = format!("{}@{}", sanitize(&long_b), sanitize(&long_a));
        let (g, _) = lod_rel_paths(&stem.with_file_name(format!("{name}~0123abcd")), 63);
        assert!(g.file_name().unwrap().len() < 255);
    }

    #[test]
    fn colliding_output_names_are_disambiguated() {
        let files = |stem: &Path, lods: usize| -> Vec<(PathBuf, Vec<u8>)> {
            (0..lods)
                .flat_map(|li| {
                    let (g, b) = lod_rel_paths(stem, li);
                    [(g, vec![]), (b, vec![])]
                })
                .collect()
        };
        let mut c = Claims::default();
        // Two object paths that sanitise to the same stem.
        let s1 = relative_stem("Pkg", "Pkg.A B");
        assert_eq!(s1, relative_stem("Pkg", "Pkg.A_B"));
        let got = c.choose("Pkg.A B", s1.clone(), 1).unwrap();
        assert_eq!(got, s1);
        c.claim("Pkg.A B", &files(&got, 1));
        let got2 = c.choose("Pkg.A_B", s1.clone(), 1).unwrap();
        assert_ne!(got2, s1);
        assert!(rel_string(&got2).starts_with("Pkg/Pkg/A_B~"));
        c.claim("Pkg.A_B", &files(&got2, 1));
        // The same key keeps its files on a re-run.
        assert_eq!(c.choose("Pkg.A B", s1.clone(), 1).unwrap(), s1);
        assert_eq!(c.choose("Pkg.A_B", s1.clone(), 1).unwrap(), got2);
        // Paths that differ only in case collide on case-insensitive file systems.
        let m1 = relative_stem("Pkg", "Pkg.Mesh");
        c.claim("Pkg.Mesh", &files(&m1, 1));
        let m2 = relative_stem("Pkg", "Pkg.MESH");
        assert_ne!(c.choose("Pkg.MESH", m2.clone(), 1).unwrap(), m2);
        // A mesh named like another mesh's LOD file.
        let x = relative_stem("Pkg", "Pkg.X");
        c.claim("Pkg.X", &files(&x, 2));
        let xl = relative_stem("Pkg", "Pkg.X_LOD1");
        assert_ne!(c.choose("Pkg.X_LOD1", xl.clone(), 1).unwrap(), xl);
        // Both the stem and its hashed alternative taken: refused.
        let y = relative_stem("Pkg", "Pkg.Y");
        c.claim("other", &files(&y, 1));
        let alt = c.choose("Pkg.Y", y.clone(), 1).unwrap();
        c.claim("third", &files(&alt, 1));
        assert!(c.choose("Pkg.Y", y, 1).is_none());
        // Claims are restored from an existing manifest.
        let mut m = Manifest::default();
        let (g, b) = lod_rel_paths(&s1, 0);
        m.meshes.insert(
            "Pkg.A B".to_owned(),
            MeshEntry {
                package: "Pkg".to_owned(),
                export_index: 0,
                also_in: vec![],
                differs_in: vec![],
                lod_count: 1,
                lods: vec![LodEntry {
                    lod: 0,
                    gltf: rel_string(&g),
                    bin: rel_string(&b),
                    sections: vec![],
                    stats: AssetStats::default(),
                }],
                light_map_coordinate_index: None,
                light_map_resolution: None,
                body_setup: None,
                collision_triangles: 0,
                simple_collision: SimpleCollisionEntry {
                    use_simple_box_collision: true,
                    use_simple_line_collision: true,
                    use_simple_rigid_body_collision: true,
                    has_body_setup: true,
                    shapes: Some(rel_string(&collision_rel_path(&s1))),
                    convex: 1,
                    boxes: 0,
                    spheres: 0,
                    sphyls: 0,
                },
                bounds_ue: BoundsEntry {
                    origin: [0.0; 3],
                    box_extent: [0.0; 3],
                    sphere_radius: 0.0,
                },
                scale: 1.0,
                content_hash: String::new(),
            },
        );
        let c = Claims::from_manifest(&m);
        assert_eq!(c.choose("Pkg.A B", s1.clone(), 1).unwrap(), s1);
        assert_ne!(c.choose("Pkg.A_B", s1.clone(), 1).unwrap(), s1);
    }

    #[test]
    fn content_hash_is_order_sensitive() {
        assert_ne!(fnv1a64(&[b"ab", b"c"]), fnv1a64(&[b"a", b"bc"]));
        assert_eq!(fnv1a64(&[b"x"]), fnv1a64(&[b"x"]));
    }

    #[test]
    fn refuses_repo_output() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        assert!(prepare_out_dir(&repo.join("docs"), None).is_err());
        assert!(prepare_out_dir(&repo, None).is_err());
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Game.app").join("Contents");
        std::fs::create_dir_all(&app).unwrap();
        assert!(prepare_out_dir(&app, None).is_err());
        let ok = prepare_out_dir(tmp.path(), None).unwrap();
        assert!(ok.ends_with("meshes"));
        // Existing files are kept without --force, replaced with it.
        let rel = Path::new("P").join("M.gltf");
        assert!(write_file(&ok, &rel, b"one", tmp.path(), false).unwrap());
        assert!(!write_file(&ok, &rel, b"two", tmp.path(), false).unwrap());
        assert_eq!(std::fs::read(ok.join(&rel)).unwrap(), b"one");
        assert!(write_file(&ok, &rel, b"two", tmp.path(), true).unwrap());
        assert_eq!(std::fs::read(ok.join(&rel)).unwrap(), b"two");
    }

    /// Links inside the output tree are never followed: a directory link (to
    /// a game install or anywhere else) is refused before anything is created
    /// behind it, a file link is kept without `--force` and refused with it,
    /// and relative paths cannot leave the root.
    #[cfg(unix)]
    #[test]
    fn links_inside_the_output_tree_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = prepare_out_dir(tmp.path(), None).unwrap();
        let input = tmp.path().join("in.u");
        let install = tmp.path().join("Game.app").join("Contents");
        std::fs::create_dir_all(&install).unwrap();
        std::os::unix::fs::symlink(&install, root.join("Engine")).unwrap();
        let rel = Path::new("Engine").join("Sub").join("M.gltf");
        assert!(write_file(&root, &rel, b"x", &input, false).is_err());
        assert!(write_file(&root, &rel, b"x", &input, true).is_err());
        assert_eq!(std::fs::read_dir(&install).unwrap().count(), 0);
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("Other")).unwrap();
        let rel = Path::new("Other").join("M.gltf");
        assert!(write_file(&root, &rel, b"x", &input, false).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        let victim = tmp.path().join("victim.u");
        std::fs::write(&victim, b"original").unwrap();
        std::fs::create_dir(root.join("P")).unwrap();
        std::os::unix::fs::symlink(&victim, root.join("P").join("M.gltf")).unwrap();
        let rel = Path::new("P").join("M.gltf");
        assert!(!write_file(&root, &rel, b"x", &input, false).unwrap());
        assert!(write_file(&root, &rel, b"x", &input, true).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");
        assert!(write_file(&root, Path::new("../escape.gltf"), b"x", &input, false).is_err());
        assert!(!tmp.path().join("escape.gltf").exists());
        let abs = tmp.path().join("abs.gltf");
        assert!(write_file(&root, &abs, b"x", &input, false).is_err());
        assert!(!abs.exists());
    }

    // ---------------------------------------------------------------- simple collision

    use asamu_ue3::bodysetup::{
        AggGeom, BoxElem, ConvexElem, ElemBox, Plane, ScalarProperty, SphereElem, SphylElem,
        permute_vertex_data,
    };
    use asamu_ue3::types::FName;

    /// A body written here: a square pyramid (base at z = 0.5, apex at
    /// z = 2.5), a box, a sphere and a capsule, with two scalar properties.
    fn pyramid_body() -> BodySetup {
        let verts = vec![
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [1.0, 1.0, 0.5],
            [-1.0, 1.0, 0.5],
            [0.0, 0.0, 2.5],
        ];
        let plane = |x, y, z, w| Plane { x, y, z, w };
        let convex = ConvexElem {
            permuted_vertex_data: Some(permute_vertex_data(&verts)),
            vertex_data: Some(verts),
            face_tri_data: Some(vec![0, 1, 2, 0, 2, 3, 1, 4, 2, 2, 4, 3, 3, 4, 0, 0, 4, 1]),
            edge_directions: Some(vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
            face_normal_directions: Some(vec![[0.0, 0.0, -1.0]]),
            face_plane_data: Some(vec![
                plane(0.0, 0.0, -1.0, -0.5),
                plane(0.894_427_2, 0.0, 0.447_213_6, 1.118_034),
            ]),
            elem_box: Some(ElemBox {
                min: [-1.0, -1.0, 0.5],
                max: [1.0, 1.0, 2.5],
                is_valid: 1,
            }),
        };
        let tm = |origin: [f32; 3]| {
            Some([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [origin[0], origin[1], origin[2], 1.0],
            ])
        };
        let scalar = |name: &str, value| {
            BodyProperty::Scalar(ScalarProperty {
                name: name.to_owned(),
                raw_name: FName::default(),
                value,
            })
        };
        BodySetup {
            net_index: 0,
            properties: vec![
                scalar("bNoCollision", ScalarValue::Bool(true)),
                scalar("PhysMaterial", ScalarValue::Object(PackageIndex(-3))),
                scalar("PreCachedPhysDataVersion", ScalarValue::Int(7)),
                BodyProperty::AggGeom(AggGeom {
                    sphere_elems: Some(vec![SphereElem {
                        tm: tm([10.0, 20.0, 30.0]),
                        radius: Some(4.0),
                        no_rb_collision: Some(false),
                        per_poly_shape: Some(true),
                    }]),
                    box_elems: Some(vec![BoxElem {
                        tm: tm([1.0, 2.0, 3.0]),
                        x: Some(2.0),
                        y: Some(4.0),
                        z: Some(6.0),
                        no_rb_collision: Some(true),
                        per_poly_shape: Some(false),
                    }]),
                    sphyl_elems: Some(vec![SphylElem {
                        tm: tm([0.0; 3]),
                        radius: Some(1.5),
                        length: Some(8.0),
                        no_rb_collision: Some(false),
                        per_poly_shape: Some(false),
                    }]),
                    convex_elems: Some(vec![convex]),
                    skip_close_and_parallel_checks: None,
                }),
            ],
            native_start: 0,
            pre_cached_phys_data: Some(Vec::new()),
        }
    }

    fn pyramid_doc(body: &BodySetup) -> Result<SimpleCollisionDoc> {
        simple_collision_doc(
            "Pkg.Meshes.Pyramid",
            "Pkg.Meshes.Pyramid.Body",
            body,
            &|o| (o.0 == -3).then(|| "Pkg.Phys.Stone".to_owned()),
        )
    }

    fn geom_mut(body: &mut BodySetup) -> &mut AggGeom {
        body.properties
            .iter_mut()
            .find_map(|p| match p {
                BodyProperty::AggGeom(g) => Some(g),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn simple_collision_document_keeps_ue3_space_and_values() {
        let doc = pyramid_doc(&pyramid_body()).unwrap();
        assert_eq!(doc.format, "asamu-simple-collision");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.mesh, "Pkg.Meshes.Pyramid");
        assert_eq!(doc.body_setup, "Pkg.Meshes.Pyramid.Body");
        assert!(doc.space.contains("UE3 axes") && doc.space.contains("(y, z, -x) * scale"));
        assert_eq!(doc.body_properties.len(), 3);
        assert_eq!(doc.body_properties["bNoCollision"], json!(true));
        assert_eq!(doc.body_properties["PhysMaterial"], json!("Pkg.Phys.Stone"));
        assert_eq!(doc.body_properties["PreCachedPhysDataVersion"], json!(7));
        // UE3 coordinates as stored: nothing is swapped, mirrored or scaled.
        let c = &doc.convex[0];
        assert_eq!(c.vertices[1], [1.0, -1.0, 0.5]);
        assert_eq!(c.vertices[4], [0.0, 0.0, 2.5]);
        assert_eq!(c.planes[0], [0.0, 0.0, -1.0, -0.5]);
        assert_eq!(c.planes[1], [0.894_427_2, 0.0, 0.447_213_6, 1.118_034]);
        assert_eq!(c.face_triangles.len(), 6);
        assert_eq!(c.face_triangles[2], [1, 4, 2]);
        assert_eq!(c.edge_directions, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        assert_eq!(c.face_normal_directions, [[0.0, 0.0, -1.0]]);
        assert_eq!(
            (c.bounds_min, c.bounds_max),
            ([-1.0, -1.0, 0.5], [1.0, 1.0, 2.5])
        );
        assert_eq!(doc.boxes[0].size, [2.0, 4.0, 6.0]);
        assert_eq!(doc.boxes[0].transform[3], [1.0, 2.0, 3.0, 1.0]);
        assert!(doc.boxes[0].no_rb_collision && !doc.boxes[0].per_poly_shape);
        assert_eq!(doc.spheres[0].radius, 4.0);
        assert_eq!(doc.spheres[0].transform[3], [10.0, 20.0, 30.0, 1.0]);
        assert!(doc.spheres[0].per_poly_shape);
        assert_eq!((doc.sphyls[0].radius, doc.sphyls[0].length), (1.5, 8.0));
        // An unresolvable object reference is null, not an invented path.
        let unresolved = simple_collision_doc("M", "B", &pyramid_body(), &|_| None).unwrap();
        assert_eq!(unresolved.body_properties["PhysMaterial"], Value::Null);

        // The text that is written reads back exactly and deterministically.
        let text = serde_json::to_string(&doc).unwrap();
        assert_eq!(validate_simple_collision(&text, &doc), Vec::<String>::new());
        assert_eq!(
            text,
            serde_json::to_string(&pyramid_doc(&pyramid_body()).unwrap()).unwrap()
        );
        assert!(!text.contains('\n') && !text.contains("  "));
        // A changed value, a broken document and a bad index are reported.
        let mut other = doc.clone();
        other.convex[0].vertices[0][0] = -1.5;
        assert_eq!(validate_simple_collision(&text, &other).len(), 1);
        assert_eq!(
            validate_simple_collision(&text[..text.len() - 2], &doc).len(),
            1
        );
        let mut bad = doc.clone();
        bad.convex[0].face_triangles[0] = [0, 1, 5];
        let bad_text = serde_json::to_string(&bad).unwrap();
        assert!(
            validate_simple_collision(&bad_text, &bad)
                .iter()
                .any(|m| m.contains("outside the 5 vertices"))
        );
    }

    #[test]
    fn simple_collision_floats_survive_the_text_bit_for_bit() {
        // Awkward values: shortest decimal forms, subnormals, the largest and
        // smallest magnitudes, negative zero, and neighbours of 2^24.
        let awkward = [
            0.1f32,
            -0.3,
            1.0 / 3.0,
            120.8339,
            -6.513_884_5,
            f32::MIN_POSITIVE,
            1.0e-45,
            -1.0e-39,
            f32::MAX,
            f32::MIN,
            f32::EPSILON,
            -0.0,
            0.0,
            16_777_216.0,
            16_777_218.0,
            8_388_607.5,
            3.402_823_3e38,
            1.175_494_2e-38,
        ];
        let mut body = pyramid_body();
        {
            let c = &mut geom_mut(&mut body).convex_elems.as_mut().unwrap()[0];
            // Keep the body consistent: directions carry the values.
            c.edge_directions = Some(awkward.chunks(3).map(|v| [v[0], v[1], v[2]]).collect());
        }
        let doc = pyramid_doc(&body).unwrap();
        let text = serde_json::to_string(&doc).unwrap();
        assert_eq!(validate_simple_collision(&text, &doc), Vec::<String>::new());
        let back: SimpleCollisionDoc = serde_json::from_str(&text).unwrap();
        let bits = |d: &SimpleCollisionDoc| -> Vec<u32> {
            d.convex[0]
                .edge_directions
                .iter()
                .flatten()
                .map(|f| f.to_bits())
                .collect()
        };
        assert_eq!(bits(&back), awkward.map(f32::to_bits));
        // A sign-of-zero change alone is caught by the validation.
        let flipped = text.replacen("-0.0],[0.0,1677", "0.0],[0.0,1677", 1);
        assert_ne!(flipped, text);
        assert_eq!(validate_simple_collision(&flipped, &doc).len(), 1);
    }

    #[test]
    fn inconsistent_bodies_are_refused() {
        let refuse = |edit: &dyn Fn(&mut BodySetup), needle: &str| {
            let mut body = pyramid_body();
            edit(&mut body);
            let err = format!("{:#}", pyramid_doc(&body).unwrap_err());
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        };
        fn first(b: &mut BodySetup) -> &mut ConvexElem {
            &mut geom_mut(b).convex_elems.as_mut().unwrap()[0]
        }
        // Values JSON cannot hold.
        refuse(
            &|b| first(b).vertex_data.as_mut().unwrap()[0][0] = f32::NAN,
            "not finite",
        );
        refuse(
            &|b| geom_mut(b).sphere_elems.as_mut().unwrap()[0].radius = Some(f32::INFINITY),
            "Radius is inf",
        );
        // Members that are not stored: the struct default is not invented.
        refuse(&|b| first(b).face_plane_data = None, "a member is absent");
        refuse(
            &|b| geom_mut(b).box_elems.as_mut().unwrap()[0].tm = None,
            "TM is absent",
        );
        refuse(
            &|b| geom_mut(b).sphyl_elems.as_mut().unwrap()[0].length = None,
            "Length is absent",
        );
        refuse(
            &|b| geom_mut(b).box_elems.as_mut().unwrap()[0].per_poly_shape = None,
            "bPerPolyShape is not stored",
        );
        // Triangles that do not index the vertices.
        refuse(
            &|b| first(b).face_tri_data.as_mut().unwrap()[0] = 9,
            "index 9 outside",
        );
        refuse(
            &|b| first(b).face_tri_data.as_mut().unwrap()[0] = -1,
            "index -1",
        );
        // Stale derived data.
        refuse(
            &|b| first(b).elem_box.as_mut().unwrap().max[0] = 2.0,
            "ElemBox",
        );
        // No aggregate at all.
        refuse(
            &|b| {
                b.properties
                    .retain(|p| !matches!(p, BodyProperty::AggGeom(_)))
            },
            "no AggGeom",
        );
        // A non-finite scalar property.
        refuse(
            &|b| {
                b.properties.push(BodyProperty::Scalar(ScalarProperty {
                    name: "MassScale".to_owned(),
                    raw_name: FName::default(),
                    value: ScalarValue::Float(f32::NAN),
                }));
            },
            "MassScale is NaN",
        );
    }

    #[test]
    fn shapes_file_is_claimed_next_to_the_mesh() {
        let stem = relative_stem("Pkg", "Pkg.Meshes.Door");
        let rel = collision_rel_path(&stem);
        assert_eq!(rel_string(&rel), "Pkg/Pkg/Meshes/Door.collision.json");
        let planned = Claims::planned(&stem, 2);
        assert_eq!(
            planned,
            [
                "pkg/pkg/meshes/door.gltf",
                "pkg/pkg/meshes/door.bin",
                "pkg/pkg/meshes/door_lod1.gltf",
                "pkg/pkg/meshes/door_lod1.bin",
                "pkg/pkg/meshes/door.collision.json",
            ]
        );
        // A shapes file listed by a manifest blocks another key from the stem.
        let mut c = Claims::default();
        c.claim("Pkg.Meshes.Door", &[(rel, vec![])]);
        assert_eq!(c.choose("Pkg.Meshes.Door", stem.clone(), 1).unwrap(), stem);
        assert_ne!(c.choose("Pkg.Meshes.DOOR", stem.clone(), 1).unwrap(), stem);
    }

    #[test]
    fn manifests_of_another_version_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        // No manifest: an empty one.
        assert!(read_manifest(&path).unwrap().meshes.is_empty());
        // The current version is read.
        let current = Manifest {
            version: MANIFEST_VERSION,
            ..Manifest::default()
        };
        std::fs::write(&path, serde_json::to_string(&current).unwrap()).unwrap();
        assert_eq!(read_manifest(&path).unwrap().version, 2);
        // Version 1 (no `simple_collision`, another content hash) is not merged.
        for old in [
            r#"{"version": 1, "notice": "", "coordinates": "", "scale": 1.0, "meshes": {}}"#,
            r#"{"notice": "", "coordinates": "", "scale": 1.0, "meshes": {}}"#,
            r#"{"version": 3, "notice": "", "coordinates": "", "scale": 1.0, "meshes": {}}"#,
        ] {
            std::fs::write(&path, old).unwrap();
            let err = format!("{:#}", read_manifest(&path).unwrap_err());
            assert!(err.contains("expected 2"), "{err}");
        }
        std::fs::write(&path, "not json").unwrap();
        assert!(read_manifest(&path).is_err());
    }

    /// Gated on the original game data: the simple collision of every static
    /// mesh of the install, de-duplicated by object path as the importer
    /// does. Skips cleanly when the install is absent.
    #[test]
    fn real_simple_collision_counts() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let root = std::env::var_os("ASAMU_ORIGINAL_DIR").map_or_else(
            || {
                PathBuf::from(home).join(
                    "Library/Application Support/Steam/steamapps/common/A Story About My Uncle",
                )
            },
            PathBuf::from,
        );
        let cooked = root.join("A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac");
        if !cooked.is_dir() {
            eprintln!("SKIP: original game data not found");
            return;
        }
        let dirs = vec![cooked.clone(), cooked.join("Maps")];
        let files = package_files(&dirs, &[]);
        let mut exports = 0usize;
        let mut first: BTreeMap<String, (SimpleCollisionEntry, String)> = BTreeMap::new();
        let mut differing = 0usize;
        let mut bytes = 0usize;
        let mut properties: BTreeMap<String, usize> = BTreeMap::new();
        let mut no_collision_bodies = 0usize;
        for file in &files {
            let set = PackageSet::new(&dirs);
            let lp = set.open_file(file).unwrap();
            for i in 0..lp.package.exports.len() {
                if !is_static_mesh(&lp.package, i) {
                    continue;
                }
                exports += 1;
                let m = decode_static_mesh(&lp.package, Some(&lp.name), i, &set).unwrap();
                // Every copy of a path gets the same stem here, so equal
                // content gives equal entries and equal text.
                let stem = relative_stem("P", &m.object.path);
                let sc = convert_simple_collision(&lp, &m, &stem).unwrap();
                assert!(sc.issues.is_empty(), "{}: {:?}", m.object.path, sc.issues);
                assert_eq!(sc.entry.has_body_setup, sc.file.is_some());
                assert_eq!(sc.entry.has_body_setup, sc.entry.shapes.is_some());
                let text = sc
                    .file
                    .map(|(_, b)| String::from_utf8(b).unwrap())
                    .unwrap_or_default();
                match first.get(&m.object.path) {
                    Some((entry, t)) => {
                        if *entry != sc.entry || *t != text {
                            differing += 1;
                        }
                    }
                    None => {
                        bytes += text.len();
                        if let Ok(doc) = serde_json::from_str::<SimpleCollisionDoc>(&text) {
                            for k in doc.body_properties.keys() {
                                *properties.entry(k.clone()).or_insert(0) += 1;
                            }
                            if doc.body_properties.get("bNoCollision") == Some(&json!(true)) {
                                no_collision_bodies += 1;
                            }
                        }
                        first.insert(m.object.path.clone(), (sc.entry, text));
                    }
                }
            }
        }
        let count = |f: &dyn Fn(&SimpleCollisionEntry) -> bool| {
            first.values().filter(|(e, _)| f(e)).count()
        };
        let sum = |f: &dyn Fn(&SimpleCollisionEntry) -> usize| -> usize {
            first.values().map(|(e, _)| f(e)).sum()
        };
        eprintln!(
            "exports {exports} paths {} differing {differing} bodies {} convex {} boxes {} \
             spheres {} sphyls {} box off {} line off {} rb off {} no body+box on {} \
             body+box on {} shapes text {bytes} B properties {properties:?}",
            first.len(),
            count(&|e| e.has_body_setup),
            sum(&|e| e.convex),
            sum(&|e| e.boxes),
            sum(&|e| e.spheres),
            sum(&|e| e.sphyls),
            count(&|e| !e.use_simple_box_collision),
            count(&|e| !e.use_simple_line_collision),
            count(&|e| !e.use_simple_rigid_body_collision),
            count(&|e| !e.has_body_setup && e.use_simple_box_collision),
            count(&|e| e.has_body_setup && e.use_simple_box_collision),
        );
        // 1,512 exports are 882 distinct paths; every copy of a path has the
        // same switches and the same shapes.
        assert_eq!((exports, first.len(), differing), (1512, 882, 0));
        // PARITY_FINDINGS.md V5.
        assert_eq!(count(&|e| e.has_body_setup), 316);
        assert_eq!(
            count(&|e| !e.has_body_setup && e.use_simple_box_collision),
            515
        );
        assert_eq!(count(&|e| !e.use_simple_box_collision), 70);
        assert_eq!(
            count(&|e| !e.use_simple_box_collision && e.has_body_setup),
            19
        );
        assert_eq!(
            count(&|e| e.has_body_setup && e.use_simple_box_collision),
            297
        );
        assert_eq!(count(&|e| !e.use_simple_line_collision), 65);
        assert_eq!(count(&|e| !e.use_simple_rigid_body_collision), 57);
        // A body always has shapes: convex elements, or (one mesh) a sphere.
        assert_eq!(
            count(&|e| e.has_body_setup && e.convex + e.boxes + e.spheres + e.sphyls == 0),
            0
        );
        assert_eq!(count(&|e| e.convex > 0), 315);
        assert_eq!(count(&|e| e.spheres > 0), 1);
        assert_eq!((sum(&|e| e.boxes), sum(&|e| e.sphyls)), (0, 0));
        assert_eq!((sum(&|e| e.convex), sum(&|e| e.spheres)), (2439, 1));
        // Scalar properties stored on the mesh bodies.
        let stored: Vec<(&str, usize)> = properties.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            stored,
            [
                ("PhysMaterial", 9),
                ("PreCachedPhysDataVersion", 316),
                ("bNoCollision", 1),
            ]
        );
        assert_eq!(no_collision_bodies, 1);
        // The shapes documents of the 316 bodies, as written.
        assert_eq!(bytes, 4_119_188);
    }

    /// Gated on the original game data: converts a few packages in memory and
    /// validates every glTF. Skips cleanly when the install is absent.
    #[test]
    fn real_meshes_convert_and_validate() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let root = std::env::var_os("ASAMU_ORIGINAL_DIR").map_or_else(
            || {
                PathBuf::from(home).join(
                    "Library/Application Support/Steam/steamapps/common/A Story About My Uncle",
                )
            },
            PathBuf::from,
        );
        let cooked = root.join("A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac");
        if !cooked.is_dir() {
            eprintln!("SKIP: original game data not found");
            return;
        }
        let dirs = vec![cooked.clone(), cooked.join("Maps")];
        let files = package_files(&dirs, &["engine.u".to_owned(), "freds_place".to_owned()]);
        assert_eq!(files.len(), 2);
        let args = Args {
            packages: vec![],
            name: None,
            limit: None,
            all_lods: true,
            collision: true,
            scale: 1.0,
            force: false,
            check: true,
        };
        let mut meshes = 0;
        let (mut agree, mut disagree) = (0u64, 0u64);
        for file in &files {
            let set = PackageSet::new(&dirs);
            let lp = set.open_file(file).unwrap();
            for i in 0..lp.package.exports.len() {
                if !is_static_mesh(&lp.package, i) {
                    continue;
                }
                let m = decode_static_mesh(&lp.package, Some(&lp.name), i, &set).unwrap();
                let stem = relative_stem(&lp.name, &m.object.path);
                let c = convert_mesh(&lp, i, &m, &stem, &args).unwrap();
                assert!(c.issues.is_empty(), "{}: {:?}", m.object.path, c.issues);
                for l in &c.entry.lods {
                    agree += l.stats.winding_agree;
                    disagree += l.stats.winding_disagree;
                }
                meshes += 1;
            }
        }
        // Engine.u has 25 meshes, Freds_place 27.
        assert_eq!(meshes, 52);
        assert!(agree > 100 * disagree.max(1), "{agree} vs {disagree}");
    }
}
