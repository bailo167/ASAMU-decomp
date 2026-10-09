//! `asamu-import lightmaps`: the original's baked lighting, per map.
//!
//! For every map (or the ones named with `--map`) this writes, into
//! `<out>/lightmaps/` (user-local; never the repository or the game install):
//!
//! - `<map>/atlas_<n>.dds` — one HDR atlas per pair of shipped coefficient
//!   textures (`NormalizedAverageColor*` + `DirectionalMaxComponent*`,
//!   `LightMapTexture2D`, DXT1). Each light map region of the atlas is
//!   combined with **its own** scale vectors into linear RGB irradiance (UE3
//!   light units) by [`asamu_ue3::lightmap::directional_irradiance`] — an
//!   APPROXIMATION of the original's directional light maps, see
//!   `docs/reverse-engineering/LIGHTMAPS.md` — and stored as
//!   `DXGI_FORMAT_R9G9B9E5_SHAREDEXP` (DX10 DDS header, one mip). Regions
//!   are grown by one texel so bilinear filtering does not bleed the unused
//!   atlas texels in. With `--png`, `atlas_<n>.png` previews (Reinhard
//!   tone-mapped, sRGB) are written next to them.
//! - `<map>/vertex.dds` — vertex light maps (`FLightMap1D`) reduced to one
//!   constant per component: the mean of the component's simple coefficient
//!   samples, in a 4 x 4 texel cell each (sample the cell centre).
//! - `<map>.bsp.bin` — BSP surfaces with their light map coordinates: per
//!   `ModelComponent` element, the polygons of its nodes from the level
//!   model's render vertex buffer (`FModelVertex`: position, texture UV,
//!   shadow-map UV), fan-triangulated, UE3 world space. Every element is
//!   written, also those without a texture light map (they carry no atlas),
//!   so the elements together cover the whole visible BSP and can replace
//!   the flat BSP of `asamu-import levels` (same triangle count on every
//!   shipped map).
//! - `<map>.lightmaps.json` — `format` `asamu-lightmaps`, version 1: atlases,
//!   one entry per lit component (actor, component, static mesh, light map
//!   kind, atlas and UV rectangle, light map UV channel, scale vectors,
//!   baked light count; instanced static mesh components are counted but
//!   not written, see [`MapStats::skipped`]), the BSP elements (material,
//!   atlas and rectangle when lit, the byte spans of their arrays in the
//!   `.bin`), and every light component
//!   with its GUIDs and whether it is baked into a light map (the renderer
//!   must not light light-mapped surfaces with it again) or casts a static
//!   shadow map.
//!
//! `--check` decodes and combines everything in memory, prints coverage and
//! statistics and writes nothing.
//!
//! All of this is derived from copyrighted game data: keep it local and do
//! not redistribute it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::bsp::{self, Model};
use asamu_ue3::bulkdata::TextureFileCaches;
use asamu_ue3::level::{self, ComponentKind, Scene, SceneOptions};
use asamu_ue3::lightmap::{
    self, LightMap, LightMap1D, LightMap2D, LightingLayout, LightingNative,
    NUM_STORED_LIGHTMAP_COEF,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::staticmesh::decode_static_mesh;
use asamu_ue3::texture::{TextureDecoder, decode_to_rgba8};
use asamu_ue3::types::PackageIndex;
use serde::Serialize;

use crate::levels::{prepare_dir, select_maps};
use crate::safety;

/// `format` of `<map>.lightmaps.json`.
pub const FORMAT: &str = "asamu-lightmaps";
/// `version` of `<map>.lightmaps.json`.
pub const VERSION: u32 = 1;
/// `DXGI_FORMAT_R9G9B9E5_SHAREDEXP`.
const DXGI_R9G9B9E5: u32 = 67;
/// Texels each light map region is grown by (filtering guard band).
const GUARD_TEXELS: i64 = 1;
/// Edge of one vertex-light-map cell in `vertex.dds`.
const VERTEX_CELL: u32 = 4;
/// Largest atlas edge accepted (the shipped light maps are at most 1024).
const MAX_ATLAS_EDGE: u32 = 4096;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive). Repeat for several;
    /// default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Decode and combine everything in memory; write nothing.
    #[arg(long)]
    check: bool,
    /// With `--check`: print the statistics as JSON.
    #[arg(long)]
    json: bool,
    /// Also write tone-mapped PNG previews of the atlases.
    #[arg(long)]
    png: bool,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
}

// ------------------------------------------------------------ JSON

/// Location of one array in the `.bin` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Byte offset.
    pub offset: usize,
    /// Element count.
    pub count: usize,
}

/// One HDR atlas.
#[derive(Debug, Clone, Serialize)]
pub struct AtlasEntry {
    /// DDS file relative to `lightmaps/`.
    pub file: String,
    /// PNG preview relative to `lightmaps/` (`--png`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
    /// Coefficient 0 texture (`NormalizedAverageColor*`).
    pub color_texture: String,
    /// Coefficient 1 texture (`DirectionalMaxComponent*`).
    pub max_components_texture: String,
    /// Distinct light map regions combined.
    pub regions: usize,
    /// Interior texels claimed by two regions with different scales.
    pub overlap_texels: usize,
    /// Largest irradiance component written.
    pub max_irradiance: f32,
}

/// The vertex-light-map atlas.
#[derive(Debug, Clone, Serialize)]
pub struct VertexAtlasEntry {
    /// DDS file relative to `lightmaps/`.
    pub file: String,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
    /// Cell edge in texels.
    pub cell: u32,
}

/// One lit component.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentEntry {
    /// Owning actor object name.
    pub actor: String,
    /// Owning actor's slot in `ULevel::Actors`.
    pub actor_slot: usize,
    /// Component object name.
    pub component: String,
    /// Component class (`StaticMeshComponent`, ...).
    pub class: String,
    /// Static mesh path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    /// `texture` (2D light map) or `vertex` (1D, reduced to a constant).
    pub kind: &'static str,
    /// Index into `atlases` (texture kind; the vertex kind uses
    /// `vertex_atlas`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas: Option<usize>,
    /// Texture rectangle `[min_u, min_v, max_u, max_v]` the light map UVs
    /// `[0, 1]` map to; for `vertex` a degenerate rectangle on the cell
    /// centre.
    pub uv_rect: [f32; 4],
    /// Light map UV channel of the mesh (`LightMapCoordinateIndex`; 0 when
    /// untagged), texture kind only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_channel: Option<i32>,
    /// UV channels of the mesh's LOD 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh_uv_channels: Option<u32>,
    /// Scale vectors (coefficient 0, 1, 2).
    pub scale_vectors: [[f32; 3]; NUM_STORED_LIGHTMAP_COEF],
    /// Mean irradiance (vertex kind) — linear RGB, UE3 light units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_irradiance: Option<[f32; 3]>,
    /// Vertex samples (vertex kind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_samples: Option<usize>,
    /// Lights baked into the light map.
    pub baked_lights: usize,
    /// Static shadow maps of the component (lights rendered dynamically with
    /// precomputed shadows).
    pub shadow_maps: usize,
}

/// One BSP element (surfaces sharing a material and a light map).
#[derive(Debug, Clone, Serialize)]
pub struct BspElementEntry {
    /// `ModelComponent` object name.
    pub component: String,
    /// Element index in the component.
    pub element: usize,
    /// Material path.
    pub material: Option<String>,
    /// Atlas index; absent for an element without a texture light map (the
    /// original draws it without static lighting; the runtime draws it
    /// without a light map).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas: Option<usize>,
    /// Texture rectangle (as for components); absent with `atlas`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv_rect: Option<[f32; 4]>,
    /// `f32 x, y, z` per vertex (UE3 world space).
    pub positions: Span,
    /// `f32 x, y, z` unit normal per vertex (UE3 axes).
    pub normals: Span,
    /// `f32 u, v` texture coordinates per vertex.
    pub uv0: Span,
    /// `f32 u, v` light map coordinates per vertex.
    pub uv1: Span,
    /// `u32 a, b, c` per triangle; `(b - a) x (c - a)` points along the
    /// surface normal (UE3 axes).
    pub triangles: Span,
}

/// BSP light map geometry.
#[derive(Debug, Clone, Serialize)]
pub struct BspEntry {
    /// Binary file relative to `lightmaps/`.
    pub bin: String,
    /// Elements.
    pub elements: Vec<BspElementEntry>,
}

/// One light component.
#[derive(Debug, Clone, Serialize)]
pub struct LightEntry {
    /// Owning actor object name.
    pub actor: String,
    /// Owning actor's slot.
    pub actor_slot: usize,
    /// Component object name.
    pub component: String,
    /// Component class.
    pub class: String,
    /// `LightGuid` (named by shadow maps).
    pub light_guid: Option<String>,
    /// `LightmapGuid` (named by light maps).
    pub lightmap_guid: Option<String>,
    /// Baked into at least one light map of this map or the levels it
    /// streams.
    pub baked: bool,
    /// Named by a static shadow map.
    pub shadow_mapped: bool,
}

/// Counts of one map.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MapStats {
    /// Lighting exports by layout.
    pub layouts: BTreeMap<String, usize>,
    /// Lit components written (texture kind).
    pub texture_components: usize,
    /// Lit components written (vertex kind).
    pub vertex_components: usize,
    /// Static mesh components without a light map.
    pub unlit_components: usize,
    /// Components and BSP elements skipped, by reason: instanced static
    /// mesh components (their light map rectangle is offset per instance,
    /// which one draw cannot express), SpeedTree and fluid surfaces (not
    /// rendered), unresolved coefficient textures, decode failures.
    pub skipped: BTreeMap<String, usize>,
    /// Texture light maps whose mesh lacks the light map UV channel.
    pub missing_uv_channel: usize,
    /// BSP elements written.
    pub bsp_elements: usize,
    /// BSP elements written without a texture light map.
    pub bsp_elements_unlit: usize,
    /// BSP triangles written.
    pub bsp_triangles: usize,
    /// BSP light map UVs outside `[0, 1]` (beyond 1e-3).
    pub bsp_uv_outside: usize,
    /// Lights; baked; shadow-mapped.
    pub lights: usize,
    /// Lights baked into light maps.
    pub lights_baked: usize,
    /// Lights with static shadow maps.
    pub lights_shadow_mapped: usize,
    /// Texels written over all atlases.
    pub atlas_texels: u64,
    /// Histogram (8 bins over 0..=255) of the largest colour channel of
    /// coefficient-0 texels inside light map regions.
    pub color_max_channel_histogram: [u64; 9],
}

/// `<map>.lightmaps.json`.
#[derive(Debug, Clone, Serialize)]
pub struct MapFile {
    /// [`FORMAT`].
    pub format: &'static str,
    /// [`VERSION`].
    pub version: u32,
    /// Map package name.
    pub map: String,
    /// How the stored values were produced.
    pub encoding: &'static str,
    /// Sub-levels this map streams (their lights may be baked here).
    pub sublevels: Vec<String>,
    /// HDR atlases.
    pub atlases: Vec<AtlasEntry>,
    /// Vertex light map atlas.
    pub vertex_atlas: Option<VertexAtlasEntry>,
    /// Lit components.
    pub components: Vec<ComponentEntry>,
    /// BSP light map geometry.
    pub bsp: Option<BspEntry>,
    /// Light components.
    pub lights: Vec<LightEntry>,
    /// Counts.
    pub stats: MapStats,
}

const ENCODING: &str = "atlas texels: linear RGB irradiance in UE3 light units (1.0 = a light of \
    brightness 1 at normal incidence), RGB9E5; per light map region = srgb(coefficient 0) * \
    scale[0] * mean_i(srgb(coefficient 1)_i * scale[1]_i) (approximation of the directional \
    light map, see LIGHTMAPS.md); vertex cells: mean of srgb(simple sample) * scale[2]; texture \
    UV = uv_rect.min + light map UV * (uv_rect.max - uv_rect.min)";

// ------------------------------------------------------------ encoders

/// Encode linear RGB as `R9G9B9E5` (the shared-exponent algorithm of the
/// `EXT_texture_shared_exponent` specification). Negative and non-finite
/// inputs become 0; values beyond the format's maximum saturate.
pub fn rgb9e5(rgb: [f32; 3]) -> u32 {
    const N: i32 = 9;
    const B: i32 = 15;
    const MAX: f64 = 65_408.0; // (2^9 - 1) / 2^9 * 2^16
    let c = |v: f32| {
        let v = f64::from(v);
        if v.is_finite() {
            v.clamp(0.0, MAX)
        } else {
            0.0
        }
    };
    let (r, g, b) = (c(rgb[0]), c(rgb[1]), c(rgb[2]));
    let max = r.max(g).max(b);
    if max <= 0.0 {
        return 0;
    }
    let floor_log2 = max.log2().floor() as i32;
    let mut exp = floor_log2.max(-B - 1) + 1 + B;
    let denom = |e: i32| 2f64.powi(e - B - N);
    let maxm = (max / denom(exp) + 0.5).floor() as i32;
    if maxm == 1 << N {
        exp += 1;
    }
    let exp = exp.clamp(0, 31);
    let q = |v: f64| ((v / denom(exp) + 0.5).floor() as u32).min(511);
    q(r) | (q(g) << 9) | (q(b) << 18) | ((exp as u32) << 27)
}

/// Decode `R9G9B9E5` (tests).
#[cfg(test)]
pub fn rgb9e5_decode(v: u32) -> [f32; 3] {
    let exp = i32::try_from(v >> 27).unwrap_or(0);
    let s = 2f32.powi(exp - 15 - 9);
    [
        (v & 511) as f32 * s,
        ((v >> 9) & 511) as f32 * s,
        ((v >> 18) & 511) as f32 * s,
    ]
}

/// A DDS file with a DX10 header holding one `R9G9B9E5` mip.
pub fn dds_rgb9e5(width: u32, height: u32, texels: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 124 + 20 + texels.len() * 4);
    let mut u = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    u(0x2053_4444); // "DDS "
    u(124);
    u(0x1 | 0x2 | 0x4 | 0x8 | 0x1000); // caps, height, width, pitch, pixel format
    u(height);
    u(width);
    u(width.saturating_mul(4)); // pitch
    u(0); // depth
    u(1); // mip count
    for _ in 0..11 {
        u(0);
    }
    u(32); // pixel format size
    u(0x4); // DDPF_FOURCC
    u(0x3031_5844); // "DX10"
    for _ in 0..5 {
        u(0);
    }
    u(0x1000); // DDSCAPS_TEXTURE
    for _ in 0..4 {
        u(0);
    }
    u(DXGI_R9G9B9E5);
    u(3); // D3D10_RESOURCE_DIMENSION_TEXTURE2D
    u(0);
    u(1); // array size
    u(0);
    for t in texels {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// A minimal RGB8 PNG (stored deflate blocks).
pub fn png_rgb8(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = usize::try_from(width).unwrap_or(0) * 3;
    let mut raw = Vec::with_capacity((row + 1) * usize::try_from(height).unwrap_or(0));
    for y in 0..usize::try_from(height).unwrap_or(0) {
        raw.push(0);
        raw.extend_from_slice(rgb.get(y * row..(y + 1) * row).unwrap_or(&[]));
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        let len = u16::try_from(c.len()).unwrap_or(u16::MAX);
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn linear_to_srgb_byte(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

// ------------------------------------------------------------ atlas combine

/// One light map region placed in an atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    /// Texture rectangle `[min_u, min_v, max_u, max_v]`.
    pub uv_rect: [f32; 4],
    /// Scale vectors.
    pub scale: [[f32; 3]; NUM_STORED_LIGHTMAP_COEF],
}

/// Result of combining one atlas.
#[derive(Debug, Clone, PartialEq)]
pub struct Combined {
    /// Linear RGB irradiance per texel (row-major, top row first).
    pub texels: Vec<[f32; 3]>,
    /// Interior texels claimed by two regions with different scales.
    pub overlap_texels: usize,
    /// Coefficient-0 max-channel histogram inside regions.
    pub color_histogram: [u64; 9],
}

/// Texel range `[a, b)` covered by `[lo, hi]` on an axis of `n` texels,
/// clamped to `[0, n]` (non-finite bounds become 0; `b >= a`), so callers can
/// grow it by a guard band without overflowing.
fn texel_range(lo: f32, hi: f32, n: u32) -> (i64, i64) {
    let n = f64::from(n);
    let t = |v: f32| {
        let x = (f64::from(v) * n).round();
        if x.is_finite() {
            x.clamp(0.0, n) as i64
        } else {
            0
        }
    };
    let (a, b) = (t(lo), t(hi));
    (a, b.max(a))
}

/// Combine the two coefficient textures (`color` = coefficient 0, `maxc` =
/// coefficient 1, RGBA8 as decoded, `w` x `h`) into irradiance, region by
/// region: first each region grown by [`GUARD_TEXELS`], then the interiors,
/// so interiors always win over a neighbour's guard band. Texels outside
/// every region stay 0.
pub fn combine_atlas(color: &[u8], maxc: &[u8], w: u32, h: u32, regions: &[Region]) -> Combined {
    let n = usize::try_from(u64::from(w) * u64::from(h)).unwrap_or(0);
    let mut texels = vec![[0.0f32; 3]; n];
    let mut owner: Vec<u32> = vec![0; n];
    let mut overlap = 0usize;
    let mut hist = [0u64; 9];
    let texel = |buf: &[u8], i: usize| -> [u8; 3] {
        buf.get(i * 4..i * 4 + 3)
            .map_or([0; 3], |s| [s[0], s[1], s[2]])
    };
    for pass in 0..2 {
        for (ri, r) in regions.iter().enumerate() {
            let (x0, x1) = texel_range(r.uv_rect[0], r.uv_rect[2], w);
            let (y0, y1) = texel_range(r.uv_rect[1], r.uv_rect[3], h);
            let g = if pass == 0 { GUARD_TEXELS } else { 0 };
            let xs = (x0 - g).max(0)..(x1 + g).min(i64::from(w));
            let ys = (y0 - g).max(0)..(y1 + g).min(i64::from(h));
            for y in ys {
                for x in xs.clone() {
                    let interior = (x0..x1).contains(&x) && (y0..y1).contains(&y);
                    if pass == 0 && interior {
                        continue;
                    }
                    let Ok(i) = usize::try_from(y * i64::from(w) + x) else {
                        continue;
                    };
                    let c = texel(color, i);
                    let m = texel(maxc, i);
                    let e = lightmap::directional_irradiance(c, m, &r.scale);
                    if pass == 1 {
                        let id = u32::try_from(ri + 1).unwrap_or(u32::MAX);
                        if let Some(o) = owner.get_mut(i) {
                            if *o != 0
                                && regions
                                    .get(usize::try_from(*o - 1).unwrap_or(0))
                                    .is_some_and(|p| p.scale != r.scale)
                            {
                                overlap += 1;
                            }
                            *o = id;
                        }
                        let mx = c[0].max(c[1]).max(c[2]);
                        hist[usize::from(mx) * 8 / 255] += 1;
                    }
                    if let Some(t) = texels.get_mut(i) {
                        *t = e;
                    }
                }
            }
        }
    }
    Combined {
        texels,
        overlap_texels: overlap,
        color_histogram: hist,
    }
}

/// Vertex light map constants packed into one atlas of
/// [`VERTEX_CELL`]-texel square cells.
#[derive(Debug, Clone, PartialEq)]
pub struct VertexCells {
    /// Atlas width in texels.
    pub width: u32,
    /// Atlas height in texels.
    pub height: u32,
    /// `R9G9B9E5` texels, row-major.
    pub texels: Vec<u32>,
    /// UV of each constant's cell centre, in input order.
    pub centres: Vec<[f32; 2]>,
}

/// Pack one constant per vertex light map into a near-square grid of cells
/// (row-major). `None` when there are none, or when the atlas would exceed
/// [`MAX_ATLAS_EDGE`] (every size is computed with checked arithmetic).
pub fn pack_vertex_cells(means: &[[f32; 3]]) -> Option<VertexCells> {
    let n = u32::try_from(means.len()).ok().filter(|n| *n > 0)?;
    let mut cols = 1u32;
    while cols.checked_mul(cols)? < n {
        cols = cols.checked_add(1)?;
    }
    let rows = n.div_ceil(cols);
    let width = cols.checked_mul(VERTEX_CELL)?;
    let height = rows.checked_mul(VERTEX_CELL)?;
    if width > MAX_ATLAS_EDGE || height > MAX_ATLAS_EDGE {
        return None;
    }
    let len = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
    let mut texels = vec![0u32; len];
    let mut centres = Vec::with_capacity(means.len());
    for (k, mean) in (0u32..).zip(means) {
        let (cx, cy) = ((k % cols) * VERTEX_CELL, (k / cols) * VERTEX_CELL);
        let packed = rgb9e5(*mean);
        for y in cy..cy + VERTEX_CELL {
            let row = usize::try_from(u64::from(y) * u64::from(width)).ok()?;
            let start = row.checked_add(usize::try_from(cx).ok()?)?;
            let end = start.checked_add(usize::try_from(VERTEX_CELL).ok()?)?;
            texels.get_mut(start..end)?.fill(packed);
        }
        let half = VERTEX_CELL as f32 / 2.0;
        centres.push([
            (cx as f32 + half) / width as f32,
            (cy as f32 + half) / height as f32,
        ]);
    }
    Some(VertexCells {
        width,
        height,
        texels,
        centres,
    })
}

// ------------------------------------------------------------ conversion

/// Everything decoded for one map, before files are written.
struct MapData {
    file: MapFile,
    /// (relative path, bytes).
    outputs: Vec<(PathBuf, Vec<u8>)>,
}

fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.root))
}

fn guid_prop(props: &[asamu_ue3::Property], name: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| lightmap::guid_of(&p.value))
}

/// Light map GUIDs referenced by the lit components of `lp`, and the light
/// GUIDs named by its shadow maps.
fn referenced_guids(set: &PackageSet, lp: &LoadedPackage) -> (BTreeSet<String>, BTreeSet<String>) {
    let pkg = &lp.package;
    let mut lm = BTreeSet::new();
    let mut sm = BTreeSet::new();
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class_name(i).unwrap_or_default();
        if class == "ShadowMap2D" {
            if let Ok(o) = set.decode(lp, i)
                && let Some(g) = lightmap::shadow_map_2d_info(&o.properties).light_guid
            {
                sm.insert(g);
            }
            continue;
        }
        if lightmap::lighting_layout(pkg, i).is_none() {
            continue;
        }
        let Ok(d) = lightmap::decode_lighting(pkg, Some(&lp.name), i, set) else {
            continue;
        };
        if let LightingNative::ShadowMap1D(s) = &d.native {
            sm.insert(s.light_guid.to_string());
        }
        for m in d.native.light_maps() {
            for g in m.light_guids() {
                lm.insert(g.to_string());
            }
        }
    }
    (lm, sm)
}

/// `LightMapCoordinateIndex` (0 when untagged) and LOD-0 UV channel count of
/// the static mesh at `path`.
fn mesh_uv_info(set: &PackageSet, path: &str) -> Option<(i32, u32)> {
    let (lp, i) = set.locate(path)?;
    let mesh = decode_static_mesh(&lp.package, Some(&lp.name), i, set).ok()?;
    let channels = mesh.native.lods.first()?.vertices.num_tex_coords;
    Some((mesh.light_map_coordinate_index().unwrap_or(0), channels))
}

/// Resolve a texture reference of `lp` to (package, export).
fn resolve(
    set: &PackageSet,
    lp: &Arc<LoadedPackage>,
    idx: PackageIndex,
) -> Option<(Arc<LoadedPackage>, usize)> {
    if let Some(i) = idx.export_index() {
        return Some((Arc::clone(lp), i));
    }
    let path = lp.ref_path(idx).ok()??;
    set.locate(&path)
}

/// Decode the first stored mip of a texture to RGBA8: (texels, w, h, path).
fn load_rgba(
    dec: &TextureDecoder<'_>,
    caches: &TextureFileCaches,
    lp: &LoadedPackage,
    index: usize,
) -> Result<(Vec<u8>, u32, u32, String)> {
    let t = dec
        .decode(lp, index)
        .with_context(|| format!("decoding texture export {index} of {}", lp.name))?;
    let m = t
        .first_stored_mip()
        .with_context(|| format!("{} has no stored mip", t.path))?;
    let mip0 = t.mips().first().context("no mips")?;
    let w = (u32::try_from(mip0.size_x).unwrap_or(1) >> m).max(1);
    let h = (u32::try_from(mip0.size_y).unwrap_or(1) >> m).max(1);
    if w > MAX_ATLAS_EDGE || h > MAX_ATLAS_EDGE {
        bail!("{} is {w}x{h}, larger than {MAX_ATLAS_EDGE}", t.path);
    }
    let payload = lp.package.export_data(index)?;
    let bytes = t.load_mip(payload, Some(caches), m)?;
    let format = t.format.context("unknown pixel format")?;
    let rgba = decode_to_rgba8(format, w, h, &bytes)?;
    Ok((rgba, w, h, t.path.clone()))
}

/// Atlas key: the two coefficient textures.
type AtlasKey = ((String, usize), (String, usize));

struct AtlasBuilder {
    keys: BTreeMap<AtlasKey, usize>,
    jobs: Vec<(AtlasKey, Vec<Region>)>,
}

impl AtlasBuilder {
    fn add(&mut self, set: &PackageSet, lp: &Arc<LoadedPackage>, m: &LightMap2D) -> Option<usize> {
        let (a, ai) = resolve(set, lp, m.textures[0])?;
        let (b, bi) = resolve(set, lp, m.textures[1])?;
        let key = ((a.name.clone(), ai), (b.name.clone(), bi));
        let idx = *self.keys.entry(key.clone()).or_insert_with(|| {
            self.jobs.push((key, Vec::new()));
            self.jobs.len() - 1
        });
        let region = Region {
            uv_rect: m.uv_rect(),
            scale: m.scale_vectors,
        };
        if let Some((_, regions)) = self.jobs.get_mut(idx)
            && !regions.contains(&region)
        {
            regions.push(region);
        }
        Some(idx)
    }
}

fn mean_vertex_irradiance(m: &LightMap1D) -> [f32; 3] {
    let n = m.simple_samples.len();
    let mut sum = [0.0f64; 3];
    for i in 0..n {
        if let Some(e) = lightmap::vertex_irradiance(m, i) {
            for (s, v) in sum.iter_mut().zip(e) {
                *s += f64::from(v);
            }
        }
    }
    let d = n.max(1) as f64;
    [
        (sum[0] / d) as f32,
        (sum[1] / d) as f32,
        (sum[2] / d) as f32,
    ]
}

fn push_f32s(bin: &mut Vec<u8>, v: &[f32]) {
    for x in v {
        bin.extend_from_slice(&x.to_le_bytes());
    }
}

/// Fan-triangulated polygons of `nodes` with light map coordinates, wound
/// so that `(b - a) x (c - a)` points along each node's plane normal.
#[allow(clippy::type_complexity)]
fn bsp_element_geometry(
    model: &Model,
    nodes: &[u16],
) -> (
    Vec<[f32; 3]>,
    Vec<[f32; 3]>,
    Vec<[f32; 2]>,
    Vec<[f32; 2]>,
    Vec<[u32; 3]>,
) {
    let (mut pos, mut nrm, mut uv0, mut uv1, mut tri) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for &n in nodes {
        let Some(node) = model.nodes.get(usize::from(n)) else {
            continue;
        };
        let count = usize::from(node.num_vertices);
        let Ok(first) = usize::try_from(node.vertex_index) else {
            continue;
        };
        let Some(verts) = first
            .checked_add(count)
            .and_then(|end| model.vertex_buffer.get(first..end))
        else {
            continue;
        };
        if count < 3 {
            continue;
        }
        let normal = [node.plane[0], node.plane[1], node.plane[2]];
        let poly: Vec<bsp::Vec3> = verts.iter().map(|v| v.position).collect();
        let newell = bsp::polygon_normal(&poly);
        let dot = newell[0] * normal[0] + newell[1] * normal[1] + newell[2] * normal[2];
        let Ok(base) = u32::try_from(pos.len()) else {
            break;
        };
        for v in verts {
            pos.push(v.position);
            nrm.push(normal);
            uv0.push(v.uv);
            uv1.push(v.shadow_uv);
        }
        for k in 1..count - 1 {
            let (Ok(b), Ok(c)) = (u32::try_from(k), u32::try_from(k + 1)) else {
                break;
            };
            if dot >= 0.0 {
                tri.push([base, base + b, base + c]);
            } else {
                tri.push([base, base + c, base + b]);
            }
        }
    }
    (pos, nrm, uv0, uv1, tri)
}

#[allow(clippy::too_many_lines)]
fn convert_map(
    file: &Path,
    cooked: &Path,
    args: &Args,
    caches: &TextureFileCaches,
) -> Result<MapData> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp: Arc<LoadedPackage> = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    let map = lp.name.clone();
    let Some(&level_export) = level::level_exports(&lp.package).first() else {
        bail!("{} has no Level export", file.display());
    };
    let opts = SceneOptions {
        volume_geometry: false,
        ..SceneOptions::default()
    };
    let scene: Scene = level::extract_scene(&set, &lp, level_export, &opts)
        .with_context(|| format!("extracting the scene of {map}"))?;
    let pkg = &lp.package;
    let mut stats = MapStats::default();
    for i in 0..pkg.exports.len() {
        if let Some(l) = lightmap::lighting_layout(pkg, i) {
            *stats.layouts.entry(l.class_name().to_owned()).or_default() += 1;
        }
    }

    // GUIDs referenced here and in the sub-levels this map streams.
    let sublevels: Vec<String> = scene
        .streaming_levels
        .iter()
        .filter_map(|s| s.package_name.clone())
        .collect();
    let (mut lm_guids, mut sm_guids) = referenced_guids(&set, &lp);
    for s in &sublevels {
        if let Some(sub) = set.package(s) {
            let (a, b) = referenced_guids(&set, &sub);
            lm_guids.extend(a);
            sm_guids.extend(b);
        }
    }

    let mut atlases = AtlasBuilder {
        keys: BTreeMap::new(),
        jobs: Vec::new(),
    };
    let mut components = Vec::new();
    let mut vertex_means: Vec<(usize, [f32; 3])> = Vec::new();
    let mut lights = Vec::new();
    let mut uv_cache: BTreeMap<String, Option<(i32, u32)>> = BTreeMap::new();
    for actor in &scene.actors {
        for comp in &actor.components {
            if comp.kind == ComponentKind::Light {
                let Ok(o) = set.decode(&lp, comp.export_index) else {
                    continue;
                };
                let light_guid = guid_prop(&o.properties, "LightGuid");
                let lightmap_guid = guid_prop(&o.properties, "LightmapGuid");
                let baked = lightmap_guid.as_ref().is_some_and(|g| lm_guids.contains(g));
                let shadow_mapped = light_guid.as_ref().is_some_and(|g| sm_guids.contains(g));
                stats.lights += 1;
                stats.lights_baked += usize::from(baked);
                stats.lights_shadow_mapped += usize::from(shadow_mapped);
                lights.push(LightEntry {
                    actor: actor.name.clone(),
                    actor_slot: actor.slot,
                    component: comp.name.clone(),
                    class: comp
                        .class
                        .rsplit('.')
                        .next()
                        .unwrap_or(&comp.class)
                        .to_owned(),
                    light_guid,
                    lightmap_guid,
                    baked,
                    shadow_mapped,
                });
                continue;
            }
            let layout = lightmap::lighting_layout(pkg, comp.export_index)
                .or_else(|| lightmap::inherited_lighting_layout(pkg, comp.export_index, &set));
            let Some(layout) = layout else {
                continue;
            };
            let decoded = match lightmap::decode_lighting_as(
                pkg,
                Some(&lp.name),
                comp.export_index,
                &set,
                layout,
            ) {
                Ok(d) => d,
                Err(_) => {
                    *stats
                        .skipped
                        .entry("decode failure".to_owned())
                        .or_default() += 1;
                    continue;
                }
            };
            let LightingNative::StaticMesh(s) = &decoded.native else {
                *stats
                    .skipped
                    .entry(format!("{} (not rendered)", layout.class_name()))
                    .or_default() += 1;
                continue;
            };
            let Some(lod0) = s.lods.first() else {
                stats.unlit_components += 1;
                continue;
            };
            if layout == LightingLayout::InstancedStaticMeshComponent {
                // The shared light map rectangle is offset per instance
                // (`LightmapUVBias`), which a single draw cannot express:
                // counted, not written (the runtime draws these without a
                // light map).
                *stats
                    .skipped
                    .entry("InstancedStaticMeshComponent (not rendered)".to_owned())
                    .or_default() += 1;
                continue;
            }
            let mesh = comp.static_mesh.clone();
            let base = ComponentEntry {
                actor: actor.name.clone(),
                actor_slot: actor.slot,
                component: comp.name.clone(),
                class: layout.class_name().to_owned(),
                mesh: mesh.clone(),
                kind: "texture",
                atlas: None,
                uv_rect: [0.0; 4],
                uv_channel: None,
                mesh_uv_channels: None,
                scale_vectors: [[0.0; 3]; NUM_STORED_LIGHTMAP_COEF],
                mean_irradiance: None,
                vertex_samples: None,
                baked_lights: lod0.light_map.light_guids().len(),
                shadow_maps: lod0.shadow_maps.len() + lod0.shadow_vertex_buffers.len(),
            };
            match &lod0.light_map {
                LightMap::None => stats.unlit_components += 1,
                LightMap::TwoD(m) => {
                    let Some(atlas) = atlases.add(&set, &lp, m) else {
                        *stats
                            .skipped
                            .entry("unresolved coefficient texture".to_owned())
                            .or_default() += 1;
                        continue;
                    };
                    let uv = mesh.as_ref().and_then(|p| {
                        uv_cache
                            .entry(p.clone())
                            .or_insert_with(|| mesh_uv_info(&set, p))
                            .to_owned()
                    });
                    if let Some((ch, n)) = uv
                        && u32::try_from(ch).is_ok_and(|c| c >= n)
                    {
                        stats.missing_uv_channel += 1;
                    }
                    stats.texture_components += 1;
                    components.push(ComponentEntry {
                        atlas: Some(atlas),
                        uv_rect: m.uv_rect(),
                        uv_channel: uv.map(|u| u.0),
                        mesh_uv_channels: uv.map(|u| u.1),
                        scale_vectors: m.scale_vectors,
                        ..base
                    });
                }
                LightMap::OneD(m) => {
                    stats.vertex_components += 1;
                    let mean = mean_vertex_irradiance(m);
                    vertex_means.push((components.len(), mean));
                    components.push(ComponentEntry {
                        kind: "vertex",
                        scale_vectors: m.scale_vectors,
                        mean_irradiance: Some(mean),
                        vertex_samples: Some(m.simple_samples.len()),
                        ..base
                    });
                }
            }
        }
    }

    // BSP elements.
    let mut bsp_bin = Vec::new();
    let mut bsp_elements = Vec::new();
    if let Some(model_export) = scene.tail.model.export_index()
        && let Ok((_, model)) = bsp::decode_model(pkg, Some(&lp.name), model_export, &set)
    {
        for mc in &scene.tail.model_components {
            let Some(mi) = mc.export_index() else {
                continue;
            };
            let Ok(d) = lightmap::decode_lighting(pkg, Some(&lp.name), mi, &set) else {
                continue;
            };
            let LightingNative::Model(m) = &d.native else {
                continue;
            };
            let comp_name = pkg
                .export_path(mi)
                .ok()
                .and_then(|p| p.rsplit('.').next().map(str::to_owned))
                .unwrap_or_default();
            for (ei, e) in m.elements.iter().enumerate() {
                // Every element is written, so that the light-mapped BSP can
                // replace the flat BSP completely; elements without a texture
                // light map carry no atlas.
                let lit = match &e.light_map {
                    LightMap::TwoD(lm) => match atlases.add(&set, &lp, lm) {
                        Some(atlas) => Some((atlas, lm.uv_rect())),
                        None => {
                            *stats
                                .skipped
                                .entry("unresolved BSP coefficient texture".to_owned())
                                .or_default() += 1;
                            None
                        }
                    },
                    _ => None,
                };
                let (p, n, a, b, t) = bsp_element_geometry(&model, &e.nodes);
                if t.is_empty() {
                    continue;
                }
                if lit.is_some() {
                    stats.bsp_uv_outside += b
                        .iter()
                        .flatten()
                        .filter(|v| !(-1e-3..=1.0 + 1e-3).contains(*v))
                        .count();
                } else {
                    stats.bsp_elements_unlit += 1;
                }
                let span = |bin: &Vec<u8>, count: usize| Span {
                    offset: bin.len(),
                    count,
                };
                let positions = span(&bsp_bin, p.len());
                push_f32s(&mut bsp_bin, &p.concat());
                let normals = span(&bsp_bin, n.len());
                push_f32s(&mut bsp_bin, &n.concat());
                let uv0 = span(&bsp_bin, a.len());
                push_f32s(&mut bsp_bin, &a.concat());
                let uv1 = span(&bsp_bin, b.len());
                push_f32s(&mut bsp_bin, &b.concat());
                let triangles = span(&bsp_bin, t.len());
                for tr in &t {
                    for i in tr {
                        bsp_bin.extend_from_slice(&i.to_le_bytes());
                    }
                }
                stats.bsp_elements += 1;
                stats.bsp_triangles += t.len();
                bsp_elements.push(BspElementEntry {
                    component: comp_name.clone(),
                    element: ei,
                    material: lp.ref_path(e.material).ok().flatten(),
                    atlas: lit.map(|l| l.0),
                    uv_rect: lit.map(|l| l.1),
                    positions,
                    normals,
                    uv0,
                    uv1,
                    triangles,
                });
            }
        }
    }

    // Atlases.
    let dec = TextureDecoder::new(&set);
    let mut outputs: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut atlas_entries = Vec::new();
    for (ai, (key, regions)) in atlases.jobs.iter().enumerate() {
        let ((pa, ia), (pb, ib)) = key;
        let la = set.package(pa).context("coefficient texture package")?;
        let lb = set.package(pb).context("coefficient texture package")?;
        let (ca, w, h, path_a) = load_rgba(&dec, caches, &la, *ia)?;
        let (cb, w2, h2, path_b) = load_rgba(&dec, caches, &lb, *ib)?;
        if (w, h) != (w2, h2) {
            bail!("{path_a} is {w}x{h} but {path_b} is {w2}x{h2}");
        }
        let combined = combine_atlas(&ca, &cb, w, h, regions);
        for (s, v) in stats
            .color_max_channel_histogram
            .iter_mut()
            .zip(combined.color_histogram)
        {
            *s += v;
        }
        stats.atlas_texels += u64::from(w) * u64::from(h);
        let max = combined
            .texels
            .iter()
            .flatten()
            .fold(0.0f32, |a, b| a.max(*b));
        let rel = PathBuf::from(&map).join(format!("atlas_{ai}.dds"));
        let png_rel = PathBuf::from(&map).join(format!("atlas_{ai}.png"));
        if !args.check {
            let packed: Vec<u32> = combined.texels.iter().map(|t| rgb9e5(*t)).collect();
            outputs.push((rel.clone(), dds_rgb9e5(w, h, &packed)));
            if args.png {
                let rgb: Vec<u8> = combined
                    .texels
                    .iter()
                    .flat_map(|t| t.map(|c| linear_to_srgb_byte(c / (1.0 + c))))
                    .collect();
                outputs.push((png_rel.clone(), png_rgb8(w, h, &rgb)));
            }
        }
        atlas_entries.push(AtlasEntry {
            file: rel_string(&rel),
            png: args.png.then(|| rel_string(&png_rel)),
            width: w,
            height: h,
            color_texture: path_a,
            max_components_texture: path_b,
            regions: regions.len(),
            overlap_texels: combined.overlap_texels,
            max_irradiance: max,
        });
    }

    // Vertex atlas.
    let mut vertex_atlas = None;
    if !vertex_means.is_empty() {
        let means: Vec<[f32; 3]> = vertex_means.iter().map(|(_, m)| *m).collect();
        let Some(cells) = pack_vertex_cells(&means) else {
            bail!(
                "{map}: {} vertex light maps do not fit one {MAX_ATLAS_EDGE}-texel atlas",
                means.len()
            );
        };
        let (w, h, texels) = (cells.width, cells.height, cells.texels);
        for ((ci, _), centre) in vertex_means.iter().zip(cells.centres) {
            if let Some(c) = components.get_mut(*ci) {
                c.uv_rect = [centre[0], centre[1], centre[0], centre[1]];
            }
        }
        let rel = PathBuf::from(&map).join("vertex.dds");
        if !args.check {
            outputs.push((rel.clone(), dds_rgb9e5(w, h, &texels)));
        }
        vertex_atlas = Some(VertexAtlasEntry {
            file: rel_string(&rel),
            width: w,
            height: h,
            cell: VERTEX_CELL,
        });
    }

    let bsp = (!bsp_elements.is_empty()).then(|| {
        let rel = PathBuf::from(format!("{map}.bsp.bin"));
        let entry = BspEntry {
            bin: rel_string(&rel),
            elements: bsp_elements,
        };
        if !args.check {
            outputs.push((rel, std::mem::take(&mut bsp_bin)));
        }
        entry
    });

    Ok(MapData {
        file: MapFile {
            format: FORMAT,
            version: VERSION,
            map,
            encoding: ENCODING,
            sublevels,
            atlases: atlas_entries,
            vertex_atlas,
            components,
            bsp,
            lights,
            stats,
        },
        outputs,
    })
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn write_rel(
    root: &Path,
    rel: &Path,
    data: &[u8],
    input: &Path,
    install: &Path,
    force: bool,
) -> Result<()> {
    let dir = match rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => prepare_dir(&root.join(p), input, install)?,
        _ => root.to_path_buf(),
    };
    let name = rel.file_name().context("output file name")?;
    let target = safety::check_output_path(&dir.join(name), input, force)?;
    safety::write_output(&target, data, force)?;
    Ok(())
}

fn print_check(map: &MapFile) {
    let s = &map.stats;
    eprintln!(
        "{:18} layouts {:?}; components texture {} vertex {} unlit {}; skipped {:?}; missing UV \
         channel {}; atlases {} ({} texels, overlap {}); BSP elements {} ({} without a light map) \
         triangles {} (UV outside [0,1]: {}); lights {} baked {} shadow-mapped {}",
        map.map,
        s.layouts,
        s.texture_components,
        s.vertex_components,
        s.unlit_components,
        s.skipped,
        s.missing_uv_channel,
        map.atlases.len(),
        s.atlas_texels,
        map.atlases.iter().map(|a| a.overlap_texels).sum::<usize>(),
        s.bsp_elements,
        s.bsp_elements_unlit,
        s.bsp_triangles,
        s.bsp_uv_outside,
        s.lights,
        s.lights_baked,
        s.lights_shadow_mapped
    );
    eprintln!(
        "{:18} coefficient-0 max channel histogram (texels inside regions, 8 bins): {:?}",
        "", s.color_max_channel_histogram
    );
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install) = install_dirs(ctx)?;
    let maps = select_maps(&cooked.join("Maps"), &args.maps)?;
    let caches = TextureFileCaches::discover(std::slice::from_ref(&cooked));
    let out_root = if args.check {
        None
    } else {
        Some(prepare_dir(&ctx.out.join("lightmaps"), &cooked, &install)?)
    };
    let mut summaries = Vec::new();
    for file in &maps {
        let data = convert_map(file, &cooked, &args, &caches)
            .with_context(|| format!("converting {}", file.display()))?;
        if let Some(root) = &out_root {
            for (rel, bytes) in &data.outputs {
                write_rel(root, rel, bytes, file, &install, args.force)?;
            }
            let json = if args.pretty {
                serde_json::to_vec_pretty(&data.file)?
            } else {
                serde_json::to_vec(&data.file)?
            };
            let rel = PathBuf::from(format!("{}.lightmaps.json", data.file.map));
            write_rel(root, &rel, &json, file, &install, args.force)?;
            eprintln!(
                "{}: {} atlases, {} lit components, {} BSP elements, {} lights ({} baked)",
                data.file.map,
                data.file.atlases.len(),
                data.file.components.len(),
                data.file.bsp.as_ref().map_or(0, |b| b.elements.len()),
                data.file.stats.lights,
                data.file.stats.lights_baked
            );
        } else {
            print_check(&data.file);
        }
        summaries.push((data.file.map.clone(), data.file.stats.clone()));
    }
    if args.check && args.json {
        println!("{}", serde_json::to_string_pretty(&summaries)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb9e5_round_trips_within_precision() {
        for v in [
            [0.0, 0.0, 0.0],
            [1.0, 0.5, 0.25],
            [16.0, 0.001, 3.0],
            [1e-6, 2e-6, 0.0],
            [65_000.0, 1.0, 0.0],
        ] {
            let d = rgb9e5_decode(rgb9e5(v));
            let max = v.iter().fold(0.0f32, |a, b| a.max(*b));
            for (a, b) in v.iter().zip(d) {
                assert!((a - b).abs() <= max / 256.0 + 1e-7, "{v:?} -> {d:?}");
            }
        }
        assert_eq!(rgb9e5([-1.0, f32::NAN, f32::INFINITY]), 0);
        // Saturation.
        let d = rgb9e5_decode(rgb9e5([1e9, 0.0, 0.0]));
        assert!((d[0] - 65_408.0).abs() < 1.0);
        // Exact known encoding of 1.0 grey: mantissa 256, exponent 16.
        assert_eq!(
            rgb9e5([1.0; 3]),
            256 | (256 << 9) | (256 << 18) | (16 << 27)
        );
    }

    #[test]
    fn dds_header_is_dx10_rgb9e5() {
        let d = dds_rgb9e5(2, 1, &[1, 2]);
        assert_eq!(&d[0..4], b"DDS ");
        assert_eq!(d.len(), 4 + 124 + 20 + 8);
        assert_eq!(&d[84..88], b"DX10");
        assert_eq!(u32::from_le_bytes([d[128], d[129], d[130], d[131]]), 67);
        assert_eq!(u32::from_le_bytes([d[12], d[13], d[14], d[15]]), 1); // height
        assert_eq!(u32::from_le_bytes([d[16], d[17], d[18], d[19]]), 2); // width
    }

    #[test]
    fn png_has_valid_chunks() {
        let p = png_rgb8(2, 2, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
        assert_eq!(&p[1..4], b"PNG");
        assert_eq!(&p[12..16], b"IHDR");
        // IEND chunk CRC is the well-known constant.
        assert_eq!(&p[p.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    fn solid(w: u32, h: u32, c: [u8; 3]) -> Vec<u8> {
        (0..w * h).flat_map(|_| [c[0], c[1], c[2], 255]).collect()
    }

    #[test]
    fn regions_are_combined_with_their_own_scale_and_guard_band() {
        let (w, h) = (8, 4);
        let color = solid(w, h, [255, 255, 255]);
        let maxc = solid(w, h, [255, 255, 255]);
        let r = |x0: f32, x1: f32, s: f32| Region {
            uv_rect: [x0, 0.25, x1, 0.75],
            scale: [[1.0; 3], [s; 3], [1.0; 3]],
        };
        // Left region texels 1..3, right region texels 4..7 (adjacent).
        let c = combine_atlas(
            &color,
            &maxc,
            w,
            h,
            &[r(0.125, 0.375, 2.0), r(0.5, 0.875, 4.0)],
        );
        let at = |x: usize, y: usize| c.texels[y * 8 + x][0];
        assert!((at(1, 1) - 2.0).abs() < 1e-5);
        assert!((at(5, 2) - 4.0).abs() < 1e-5);
        // Guard band: texel 0 takes the left region, texel 7 the right one;
        // texel 3 is a guard of both (either value); rows 0 and 3 are guards.
        assert!((at(0, 1) - 2.0).abs() < 1e-5);
        assert!((at(3, 1) - 2.0).abs() < 1e-5 || (at(3, 1) - 4.0).abs() < 1e-5);
        assert!((at(7, 1) - 4.0).abs() < 1e-5);
        assert!((at(1, 0) - 2.0).abs() < 1e-5);
        assert!((at(1, 3) - 2.0).abs() < 1e-5);
        assert_eq!(c.overlap_texels, 0);
        // Overlapping interiors with different scales are counted.
        let c = combine_atlas(&color, &maxc, w, h, &[r(0.0, 0.5, 1.0), r(0.25, 0.75, 3.0)]);
        assert_eq!(c.overlap_texels, 2 * 2);
        // Identical rectangles and scales never conflict.
        let c = combine_atlas(&color, &maxc, w, h, &[r(0.0, 0.5, 1.0), r(0.0, 0.5, 1.0)]);
        assert_eq!(c.overlap_texels, 0);
    }

    #[test]
    fn hostile_rectangles_stay_in_bounds() {
        let (w, h) = (4, 4);
        let color = solid(w, h, [10, 20, 30]);
        let regions = [
            Region {
                uv_rect: [-5.0, -5.0, 10.0, 10.0],
                scale: [[1.0; 3]; 3],
            },
            Region {
                uv_rect: [f32::NAN, 0.0, f32::INFINITY, 1.0],
                scale: [[f32::MAX; 3]; 3],
            },
            Region {
                uv_rect: [0.9, 0.9, 0.1, 0.1],
                scale: [[1.0; 3]; 3],
            },
        ];
        let c = combine_atlas(&color, &[], w, h, &regions);
        assert_eq!(c.texels.len(), 16);
        let c = combine_atlas(&[], &[], 0, 0, &regions);
        assert!(c.texels.is_empty());
    }

    #[test]
    fn texel_ranges_are_clamped_to_the_atlas() {
        assert_eq!(texel_range(0.25, 0.5, 8), (2, 4));
        assert_eq!(texel_range(-5.0, 10.0, 8), (0, 8));
        assert_eq!(texel_range(f32::MAX, f32::MAX, 8), (8, 8));
        assert_eq!(texel_range(-f32::MAX, f32::MAX, 8), (0, 8));
        assert_eq!(texel_range(f32::NAN, f32::INFINITY, 8), (0, 0));
        assert_eq!(texel_range(0.75, 0.25, 8), (6, 6));
        // Huge finite rectangles (a corrupt light map) never overflow the
        // guard-band arithmetic.
        let color = solid(4, 4, [255, 255, 255]);
        let regions = [
            Region {
                uv_rect: [f32::MAX, f32::MAX, f32::MAX, f32::MAX],
                scale: [[1.0; 3]; 3],
            },
            Region {
                uv_rect: [-f32::MAX, -f32::MAX, f32::MAX, f32::MAX],
                scale: [[1.0; 3]; 3],
            },
        ];
        let c = combine_atlas(&color, &color, 4, 4, &regions);
        assert_eq!(c.texels.len(), 16);
        assert!(c.texels.iter().all(|t| (t[0] - 1.0).abs() < 1e-6));
    }

    #[test]
    fn vertex_cells_pack_row_major_with_centred_uvs() {
        assert_eq!(pack_vertex_cells(&[]), None);
        let one = pack_vertex_cells(&[[1.0; 3]]).unwrap();
        assert_eq!((one.width, one.height), (VERTEX_CELL, VERTEX_CELL));
        assert_eq!(one.centres, vec![[0.5, 0.5]]);
        assert!(one.texels.iter().all(|t| *t == rgb9e5([1.0; 3])));
        // Five constants: a 3 x 2 grid of cells.
        let means: Vec<[f32; 3]> = (0..5u8).map(|k| [f32::from(k), 0.0, 0.0]).collect();
        let c = pack_vertex_cells(&means).unwrap();
        assert_eq!((c.width, c.height), (3 * VERTEX_CELL, 2 * VERTEX_CELL));
        assert_eq!(c.texels.len(), 12 * 8);
        for (k, centre) in c.centres.iter().enumerate() {
            let x = (centre[0] * c.width as f32) as usize;
            let y = (centre[1] * c.height as f32) as usize;
            assert_eq!(c.texels[y * c.width as usize + x], rgb9e5(means[k]), "{k}");
        }
        // Cell 4 is the second cell of the second row; the unused sixth
        // cell stays 0.
        assert_eq!(c.centres[4], [0.5, 0.75]);
        assert_eq!(c.texels[7 * 12 + 11], 0);
        // Beyond one MAX_ATLAS_EDGE atlas: refused, nothing allocated for it.
        let cells_per_row = (MAX_ATLAS_EDGE / VERTEX_CELL) as usize;
        let too_many = vec![[0.0f32; 3]; cells_per_row * cells_per_row + 1];
        assert_eq!(pack_vertex_cells(&too_many), None);
    }

    #[test]
    fn fan_triangulation_follows_the_node_normal() {
        use asamu_ue3::bsp::{BspNode, ModelVertex};
        let v = |x: f32, y: f32| ModelVertex {
            position: [x, y, 0.0],
            tangent_x: 0,
            tangent_z: 0,
            uv: [x, y],
            shadow_uv: [x / 10.0, y / 10.0],
        };
        let node = |n: [f32; 4]| BspNode {
            plane: n,
            vert_pool: 0,
            surf: 0,
            vertex_index: 0,
            component_index: 0,
            component_node_index: 0,
            component_element_index: 0,
            back: -1,
            front: -1,
            coplanar: -1,
            collision_bound: -1,
            zone: [0, 0],
            num_vertices: 4,
            node_flags: 0,
            leaf: [-1, -1],
        };
        // Counter-clockwise square seen from +Z.
        let quad = vec![v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        for normal in [[0.0, 0.0, 1.0, 0.0], [0.0, 0.0, -1.0, 0.0]] {
            let model = test_model(vec![node(normal)], quad.clone());
            let (p, n, _, uv1, t) = bsp_element_geometry(&model, &[0, 7]);
            assert_eq!(p.len(), 4);
            assert_eq!(t.len(), 2);
            assert_eq!(n[0], [normal[0], normal[1], normal[2]]);
            assert_eq!(uv1[2], [1.0, 1.0]);
            for tr in &t {
                let [a, b, c] = tr.map(|i| p[i as usize]);
                let cross_z = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                assert!(cross_z * normal[2] > 0.0);
            }
        }
        // A node pointing outside the vertex buffer is skipped.
        let mut bad = node([0.0, 0.0, 1.0, 0.0]);
        bad.vertex_index = 3;
        let model = test_model(vec![bad], quad);
        assert!(bsp_element_geometry(&model, &[0]).4.is_empty());
    }

    fn test_model(
        nodes: Vec<asamu_ue3::bsp::BspNode>,
        vb: Vec<asamu_ue3::bsp::ModelVertex>,
    ) -> Model {
        Model {
            bounds: asamu_ue3::bsp::BoxSphereBounds {
                origin: [0.0; 3],
                box_extent: [0.0; 3],
                sphere_radius: 0.0,
            },
            vectors: vec![],
            points: vec![],
            nodes,
            surfs_owner: PackageIndex::NULL,
            surfs: vec![],
            vert_element_size: 24,
            verts: vec![],
            num_shared_sides: 0,
            zones: vec![],
            polys: PackageIndex::NULL,
            leaf_hulls: vec![],
            leaves: vec![],
            root_outside: 0,
            linked: 0,
            portal_nodes: vec![],
            num_vertices: u32::try_from(vb.len()).unwrap(),
            vertex_buffer: vb,
            lighting_guid: asamu_ue3::types::Guid::default(),
            lightmass_settings: vec![],
        }
    }
}
