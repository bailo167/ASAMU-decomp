//! Level BSP geometry (`levels/<map>.bsp.json` + `.bsp.bin`) for rendering.
//!
//! Written by `asamu-import levels` (format `asamu-bsp`, version 1; see
//! `LEVEL_FORMAT.md`): a surface table (material, flags, texture base point
//! and axes, normal) and, in the binary file, triangle sets of `f32` vec3
//! positions, `u32` index triples and one `u32` surface index per triangle,
//! all in UE3 world space. Triangles wind so that `(b − a) × (c − a)`
//! (computed on the raw UE3 coordinates) points along the surface normal.
//!
//! [`BspGeometry::render_meshes`] builds one flat-shaded mesh per material in
//! render space:
//!
//! - positions with `asamu_core::coords::ue_pos_to_bevy`, normals with
//!   `ue_dir_to_bevy` (unit surface normal);
//! - the basis change has determinant −1, so each triangle is emitted as
//!   `(a, c, b)` to keep counter-clockwise front faces on the normal side;
//! - texture coordinates are `u = (p − base) · TextureU / 128` and
//!   `v = (p − base) · TextureV / 128` ([`BSP_UV_SCALE`]; CONFIRMED in the
//!   original executable: `UModel::BuildVertexBuffers` multiplies both dot
//!   products by the `f32` literal `0.0078125` = 1/128; the texture size is
//!   not involved).
//!
//! The binary is untrusted input: every span is bounds-checked with checked
//! arithmetic, and triangles with an out-of-range index or surface are
//! skipped and counted.

use std::collections::BTreeMap;
use std::path::Path;

use asamu_core::WorldScale;
use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};
use asamu_core::glam::Vec3;
use serde::Deserialize;

use crate::error::{AssetError, AssetResult};
use crate::files::{MAX_SCENE_BYTES, parse_json, read_bounded};

/// `format` of a BSP index.
pub const BSP_FORMAT: &str = "asamu-bsp";
/// BSP index versions this reader understands.
pub const BSP_VERSION: u32 = 1;
/// Factor applied to `(p − base) · TextureU/V` to get BSP texture
/// coordinates: 1/128 (CONFIRMED: the `f32` literal `0x3C000000` that
/// `UModel::BuildVertexBuffers` in the original Mac executable multiplies
/// both coordinates by; reproduce by disassembling that symbol).
pub const BSP_UV_SCALE: f32 = 1.0 / 128.0;

#[derive(Debug, Clone, Copy, Deserialize)]
struct RawSpan {
    offset: usize,
    count: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RawMeshSpans {
    positions: RawSpan,
    triangles: RawSpan,
    surfaces: RawSpan,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSurface {
    #[serde(default)]
    material: Option<String>,
    #[serde(default)]
    poly_flags: u32,
    #[serde(default)]
    base: [f32; 3],
    #[serde(default)]
    normal: [f32; 3],
    #[serde(default)]
    texture_u: [f32; 3],
    #[serde(default)]
    texture_v: [f32; 3],
}

#[derive(Debug, Deserialize)]
struct RawBsp {
    format: String,
    version: u32,
    bin: String,
    #[serde(default)]
    surfaces: Vec<RawSurface>,
    #[serde(default)]
    meshes: BTreeMap<String, RawMeshSpans>,
}

/// One BSP surface.
#[derive(Debug, Clone, PartialEq)]
pub struct BspSurface {
    /// Material path.
    pub material: Option<String>,
    /// `PolyFlags`.
    pub poly_flags: u32,
    /// Texture origin (UE3, UU).
    pub base: Vec3,
    /// Surface normal (UE3).
    pub normal: Vec3,
    /// Texture U axis (UE3; texels per UU along the axis).
    pub texture_u: Vec3,
    /// Texture V axis.
    pub texture_v: Vec3,
}

/// The drawn triangle set of a level BSP.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BspGeometry {
    /// Surfaces.
    pub surfaces: Vec<BspSurface>,
    /// Positions (UE3, UU).
    pub positions: Vec<Vec3>,
    /// Triangles with their surface index (only valid ones are kept).
    pub triangles: Vec<([u32; 3], u32)>,
    /// Triangles dropped (index or surface out of range, or non-finite).
    pub dropped: usize,
}

/// A flat-shaded render mesh of all BSP triangles sharing a material.
#[derive(Debug, Clone, PartialEq)]
pub struct BspRenderMesh {
    /// Material path.
    pub material: Option<String>,
    /// Render-space positions.
    pub positions: Vec<[f32; 3]>,
    /// Render-space unit normals.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices (counter-clockwise front faces).
    pub indices: Vec<u32>,
}

fn take<'a>(
    bin: &'a [u8],
    span: RawSpan,
    elem: usize,
    what: &str,
    path: &Path,
) -> AssetResult<&'a [u8]> {
    let bad = || AssetError::Format {
        path: path.to_path_buf(),
        expected: format!("{what} inside the binary ({} bytes)", bin.len()),
        found: format!("offset {} count {}", span.offset, span.count),
    };
    let len = span.count.checked_mul(elem).ok_or_else(bad)?;
    let end = span.offset.checked_add(len).ok_or_else(bad)?;
    bin.get(span.offset..end).ok_or_else(bad)
}

fn f32_at(b: &[u8], i: usize) -> f32 {
    let mut a = [0u8; 4];
    if let Some(s) = b.get(i..i + 4) {
        a.copy_from_slice(s);
    }
    f32::from_le_bytes(a)
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    let mut a = [0u8; 4];
    if let Some(s) = b.get(i..i + 4) {
        a.copy_from_slice(s);
    }
    u32::from_le_bytes(a)
}

impl BspGeometry {
    /// Parses a BSP index and its binary (the drawn, `visible`, set).
    ///
    /// # Errors
    /// Malformed JSON, wrong format/version, or spans outside the binary.
    pub fn from_parts(path: &Path, json: &[u8], bin: &[u8]) -> AssetResult<Self> {
        let raw: RawBsp = parse_json(path, json)?;
        Self::from_raw(path, raw, bin)
    }

    fn from_raw(path: &Path, raw: RawBsp, bin: &[u8]) -> AssetResult<Self> {
        if raw.format != BSP_FORMAT || raw.version != BSP_VERSION {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("{BSP_FORMAT} version {BSP_VERSION}"),
                found: format!("{} version {}", raw.format, raw.version),
            });
        }
        let surfaces: Vec<BspSurface> = raw
            .surfaces
            .iter()
            .map(|s| BspSurface {
                material: s.material.clone(),
                poly_flags: s.poly_flags,
                base: Vec3::from_array(s.base),
                normal: Vec3::from_array(s.normal),
                texture_u: Vec3::from_array(s.texture_u),
                texture_v: Vec3::from_array(s.texture_v),
            })
            .collect();
        let Some(spans) = raw.meshes.get("visible").copied() else {
            return Ok(Self {
                surfaces,
                ..Self::default()
            });
        };
        let pos = take(bin, spans.positions, 12, "positions", path)?;
        let tri = take(bin, spans.triangles, 12, "triangles", path)?;
        let tags = take(bin, spans.surfaces, 4, "surface tags", path)?;
        let positions: Vec<Vec3> = (0..spans.positions.count)
            .map(|i| {
                let o = i * 12;
                Vec3::new(f32_at(pos, o), f32_at(pos, o + 4), f32_at(pos, o + 8))
            })
            .collect();
        let n_pos = positions.len();
        let mut triangles = Vec::with_capacity(spans.triangles.count.min(1 << 20));
        let mut dropped = 0;
        for t in 0..spans.triangles.count {
            let o = t * 12;
            let idx = [u32_at(tri, o), u32_at(tri, o + 4), u32_at(tri, o + 8)];
            let surface = if t < spans.surfaces.count {
                u32_at(tags, t * 4)
            } else {
                u32::MAX
            };
            let ok = idx.iter().all(|&i| {
                usize::try_from(i)
                    .ok()
                    .and_then(|i| positions.get(i))
                    .is_some_and(|p| p.is_finite())
            }) && usize::try_from(surface).is_ok_and(|s| s < surfaces.len());
            if ok && n_pos > 0 {
                triangles.push((idx, surface));
            } else {
                dropped += 1;
            }
        }
        Ok(Self {
            surfaces,
            positions,
            triangles,
            dropped,
        })
    }

    /// Reads `<map>.bsp.json` and the binary it names (which must be a plain
    /// file name in the same directory).
    ///
    /// # Errors
    /// I/O, parse or validation errors.
    pub fn load(json_path: &Path) -> AssetResult<Self> {
        let json = read_bounded(json_path, MAX_SCENE_BYTES)?;
        let raw: RawBsp = parse_json(json_path, &json)?;
        let name = raw.bin.as_str();
        if name.is_empty() || name.contains(['/', '\\', ':', '\0']) || name == "." || name == ".." {
            return Err(AssetError::UnsafePath {
                path: name.to_owned(),
                reason: "the BSP binary must be a file name next to its index",
            });
        }
        let bin_path = json_path
            .parent()
            .map_or_else(|| Path::new(name).to_path_buf(), |d| d.join(name));
        let bin = read_bounded(&bin_path, MAX_SCENE_BYTES)?;
        Self::from_raw(json_path, raw, &bin)
    }

    /// Moves every position and texture base point by `offset` (a streaming
    /// sub-level's `Offset`).
    pub fn translate(&mut self, offset: Vec3) {
        if offset == Vec3::ZERO || !offset.is_finite() {
            return;
        }
        for p in &mut self.positions {
            *p += offset;
        }
        for s in &mut self.surfaces {
            s.base += offset;
        }
    }

    /// Appends another BSP (its triangles keep their own surfaces).
    pub fn append(&mut self, other: BspGeometry) {
        let (Ok(p0), Ok(s0)) = (
            u32::try_from(self.positions.len()),
            u32::try_from(self.surfaces.len()),
        ) else {
            self.dropped += other.triangles.len();
            return;
        };
        self.positions.extend(other.positions);
        self.surfaces.extend(other.surfaces);
        for (idx, s) in other.triangles {
            match (
                idx[0].checked_add(p0),
                idx[1].checked_add(p0),
                idx[2].checked_add(p0),
                s.checked_add(s0),
            ) {
                (Some(a), Some(b), Some(c), Some(s)) => self.triangles.push(([a, b, c], s)),
                _ => self.dropped += 1,
            }
        }
        self.dropped += other.dropped;
    }

    /// Flat-shaded render meshes, one per material, in material order of
    /// first use.
    #[must_use]
    pub fn render_meshes(&self, scale: WorldScale) -> Vec<BspRenderMesh> {
        let mut out: Vec<BspRenderMesh> = Vec::new();
        let mut by_material: BTreeMap<Option<String>, usize> = BTreeMap::new();
        for (idx, surface) in &self.triangles {
            let Some(s) = usize::try_from(*surface)
                .ok()
                .and_then(|i| self.surfaces.get(i))
            else {
                continue;
            };
            let mi = *by_material.entry(s.material.clone()).or_insert_with(|| {
                out.push(BspRenderMesh {
                    material: s.material.clone(),
                    positions: Vec::new(),
                    normals: Vec::new(),
                    uvs: Vec::new(),
                    indices: Vec::new(),
                });
                out.len() - 1
            });
            let Some(mesh) = out.get_mut(mi) else {
                continue;
            };
            let n = s.normal.try_normalize().unwrap_or(Vec3::Z);
            let normal = ue_dir_to_bevy(n).to_array();
            // Three new vertices; their indices must fit in `u32`.
            let Some(first) = u32::try_from(mesh.positions.len())
                .ok()
                .filter(|f| f.checked_add(2).is_some())
            else {
                continue;
            };
            // (a, c, b): the basis change mirrors, see the module docs.
            for &k in &[idx[0], idx[2], idx[1]] {
                let p = usize::try_from(k)
                    .ok()
                    .and_then(|i| self.positions.get(i))
                    .copied()
                    .unwrap_or(Vec3::ZERO);
                let d = p - s.base;
                let uv = [
                    d.dot(s.texture_u) * BSP_UV_SCALE,
                    d.dot(s.texture_v) * BSP_UV_SCALE,
                ];
                mesh.positions.push(ue_pos_to_bevy(p, scale).to_array());
                mesh.normals.push(normal);
                mesh.uvs.push(if uv.iter().all(|c| c.is_finite()) {
                    uv
                } else {
                    [0.0; 2]
                });
            }
            mesh.indices.extend_from_slice(&[
                first,
                first.saturating_add(1),
                first.saturating_add(2),
            ]);
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic BSP: one quad (two triangles) on the floor facing up (+Z),
    /// surface 0, plus one triangle with a bad index.
    pub(crate) fn synthetic_bsp() -> (String, Vec<u8>) {
        let mut bin = Vec::new();
        let pts: [[f32; 3]; 4] = [
            [0.0, 0.0, 0.0],
            [100.0, 0.0, 0.0],
            [100.0, 100.0, 0.0],
            [0.0, 100.0, 0.0],
        ];
        for p in pts {
            for c in p {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        // UE3 left-handed: X forward, Y right, Z up. (b-a) x (c-a) with
        // a=(0,0,0), b=(100,0,0), c=(100,100,0) = +Z.
        let tris: [[u32; 3]; 3] = [[0, 1, 2], [0, 2, 3], [0, 2, 9]];
        let tri_off = bin.len();
        for t in tris {
            for i in t {
                bin.extend_from_slice(&i.to_le_bytes());
            }
        }
        let tag_off = bin.len();
        for _ in 0..3 {
            bin.extend_from_slice(&0u32.to_le_bytes());
        }
        let json = format!(
            r#"{{"format": "asamu-bsp", "version": 1, "coordinates": "UE3", "winding": "w",
                "bin": "Test.bsp.bin", "model": "M", "bounds": {{}}, "counts": {{}}, "check": {{}},
                "surfaces": [{{"material": "Pkg.M_Floor", "poly_flags": 3584, "base": [0, 0, 0],
                               "normal": [0, 0, 1], "texture_u": [1, 0, 0], "texture_v": [0, 1, 0],
                               "actor": null, "brush_poly": 0, "plane": [0, 0, 1, 0]}}],
                "meshes": {{
                  "visible": {{"positions": {{"offset": 0, "count": 4}},
                               "triangles": {{"offset": {tri_off}, "count": 3}},
                               "surfaces": {{"offset": {tag_off}, "count": 3}}}},
                  "collision": {{"positions": {{"offset": 0, "count": 4}},
                               "triangles": {{"offset": {tri_off}, "count": 3}},
                               "surfaces": {{"offset": {tag_off}, "count": 3}}}}
                }}}}"#
        );
        (json, bin)
    }

    #[test]
    fn synthetic_bsp_builds_front_facing_render_meshes() {
        let (json, bin) = synthetic_bsp();
        let g = BspGeometry::from_parts(Path::new("Test.bsp.json"), json.as_bytes(), &bin).unwrap();
        assert_eq!(g.triangles.len(), 2);
        assert_eq!(g.dropped, 1);
        let meshes = g.render_meshes(WorldScale::IDENTITY);
        assert_eq!(meshes.len(), 1);
        let m = &meshes[0];
        assert_eq!(m.material.as_deref(), Some("Pkg.M_Floor"));
        assert_eq!(m.positions.len(), 6);
        assert_eq!(m.indices, vec![0, 1, 2, 3, 4, 5]);
        // Up in UE3 is +Y in render space.
        assert_eq!(m.normals[0], [0.0, 1.0, 0.0]);
        // Each triangle's right-handed face normal points along the normal.
        for t in m.indices.chunks(3) {
            let p = |i: u32| Vec3::from_array(m.positions[i as usize]);
            let f = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0]));
            assert!(f.dot(Vec3::Y) > 0.0, "{f}");
        }
        // UV of UE3 point (100, 100, 0): (100/128, 100/128).
        let i = m
            .positions
            .iter()
            .position(|p| {
                *p == ue_pos_to_bevy(Vec3::new(100.0, 100.0, 0.0), WorldScale::IDENTITY).to_array()
            })
            .unwrap();
        assert_eq!(m.uvs[i], [100.0 / 128.0, 100.0 / 128.0]);
    }

    #[test]
    fn translate_and_append() {
        let (json, bin) = synthetic_bsp();
        let mut a = BspGeometry::from_parts(Path::new("x"), json.as_bytes(), &bin).unwrap();
        let mut b = a.clone();
        b.translate(Vec3::new(0.0, 0.0, 10.0));
        assert_eq!(b.positions[0], Vec3::new(0.0, 0.0, 10.0));
        assert_eq!(b.surfaces[0].base, Vec3::new(0.0, 0.0, 10.0));
        a.append(b);
        assert_eq!(a.triangles.len(), 4);
        assert_eq!(a.surfaces.len(), 2);
        assert_eq!(a.triangles[2], ([4, 5, 6], 1));
        assert_eq!(a.dropped, 2);
        let meshes = a.render_meshes(WorldScale::IDENTITY);
        assert_eq!(meshes.len(), 1, "same material");
        assert_eq!(meshes[0].indices.len(), 12);
    }

    #[test]
    fn spans_outside_the_binary_are_errors() {
        let (json, bin) = synthetic_bsp();
        assert!(
            BspGeometry::from_parts(Path::new("x"), json.as_bytes(), &bin[..bin.len() - 1])
                .is_err()
        );
        let huge = json.replacen("\"count\": 4", "\"count\": 18446744073709551615", 1);
        assert!(BspGeometry::from_parts(Path::new("x"), huge.as_bytes(), &bin).is_err());
        let wrong = json.replacen("asamu-bsp", "asamu-scene", 1);
        assert!(BspGeometry::from_parts(Path::new("x"), wrong.as_bytes(), &bin).is_err());
    }

    #[test]
    fn corrupted_binaries_never_panic() {
        let (json, bin) = synthetic_bsp();
        for i in 0..bin.len() {
            for v in [0x00, 0x7f, 0xff] {
                let mut b = bin.clone();
                b[i] = v;
                if let Ok(g) = BspGeometry::from_parts(Path::new("x"), json.as_bytes(), &b) {
                    let _ = g.render_meshes(WorldScale::IDENTITY);
                }
            }
        }
    }

    #[test]
    fn load_refuses_binaries_outside_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (json, bin) = synthetic_bsp();
        std::fs::write(tmp.path().join("Test.bsp.bin"), &bin).unwrap();
        std::fs::write(tmp.path().join("Test.bsp.json"), &json).unwrap();
        let g = BspGeometry::load(&tmp.path().join("Test.bsp.json")).unwrap();
        assert_eq!(g.triangles.len(), 2);
        let evil = json.replacen("Test.bsp.bin", "../Test.bsp.bin", 1);
        std::fs::write(tmp.path().join("Evil.bsp.json"), evil).unwrap();
        assert!(matches!(
            BspGeometry::load(&tmp.path().join("Evil.bsp.json")),
            Err(AssetError::UnsafePath { .. })
        ));
    }
}
