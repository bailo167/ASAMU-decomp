//! `asamu-import decals`: per-map decal export.
//!
//! For every map package (or the ones named with `--map`) this reads the
//! map's `DecalActor`s and their `DecalComponent`s directly from the user's
//! own install (`asamu_ue3::decal`) and writes into `<out>/decals/`
//! (user-local; never the repository or the game install):
//!
//! - `<map>.decals.json` (format [`asamu_ue3::decal::DECALS_FORMAT`],
//!   version [`asamu_ue3::decal::DECALS_VERSION`]): per decal actor its
//!   world actor slot, path, class, hidden flag, material, box (width,
//!   height, near/far planes), tiling/offset, in-plane rotation, depth
//!   bias, sort order, blend range, frame (origin and the projection,
//!   width and height axes) and the decal geometry per receiving component
//!   in world space: positions (UU, rounded to 0.01), normals, decal
//!   texture coordinates and triangle indices. Decals are the decal
//!   components of placed actors: `DecalActor` / `DecalActorMovable` and the
//!   checkpoint markers' decal (`ASAMUCheckpointVisuals`). Static decals
//!   keep the cooker's own receiver geometry (clipped to the decal box
//!   where the cooker stored whole triangles); every movable decal is
//!   projected here onto the collision triangles of the components the
//!   engine's run-time receiver query returns (the editor's receiver list
//!   plus the other colliding static mesh components in the decal's box;
//!   the same projection reproduces the cooked receivers, VFX_DECALS.md
//!   §8.5). A decal on a mirrored owner projects along the reversed
//!   direction (`mirrored`).
//! - `masks/<material>.dds`: for each decal material that keeps its shape in
//!   an opacity texture instead of the displayed one (the cave paintings
//!   and runes: a constant glow, the picture only in the mask), a white
//!   image whose alpha is that opacity channel, with mips. The shared
//!   render-material model shows one texture per material and would draw
//!   such a decal as a solid patch; the runtime binds the mask as the
//!   material's texture instead (VFX_DECALS.md §9).
//! - `manifest.json`: per-map counts and totals, and the mask of every
//!   decal material that has one.
//!
//! `--check` prints the coverage table and writes nothing.
//!
//! All of this is derived from copyrighted game data: keep it local and do
//! not redistribute it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::bulkdata::TextureFileCaches;
use asamu_ue3::decal::{DecalStats, MapDecal, MapDecals, ReceiverMesh, extract_map_decals};
use asamu_ue3::material::{ApproxMaterial, Channel, MaterialDecoder};
use asamu_ue3::model::PackageSet;
use asamu_ue3::texture::{PixelFormat, TextureDecoder, decode_to_rgba8, mip_dims};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::levels::{prepare_dir, select_maps};
use crate::safety;

/// `format` of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "asamu-decals-manifest";
/// Warnings kept per map file (the rest are counted).
const MAX_WARNINGS: usize = 64;
/// Folder of the decal masks inside `decals/`.
pub const MASKS_DIR: &str = "masks";
/// Longest edge of a decal mask, texels: the largest stored mip of the
/// opacity texture that fits is used (the shipped ones are at most 1,024).
pub const MASK_MAX_EDGE: u32 = 1024;
/// Most decal masks written per run (hostile-input bound; the shipped
/// decals use 44).
const MAX_MASKS: usize = 1024;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive, e.g. AG-Darkcave).
    /// Repeat for several; default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
    /// Also write maps without decals (an empty document).
    #[arg(long)]
    include_empty: bool,
    /// Print the coverage table only; write nothing.
    #[arg(long)]
    check: bool,
}

/// One map in `manifest.json` (counts only).
#[derive(Debug, Serialize)]
pub struct ManifestEntry {
    /// Map package name.
    pub map: String,
    /// File written (none when the map has no decals).
    pub file: Option<String>,
    /// Counts.
    pub stats: DecalStats,
}

/// One decal mask in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaskEntry {
    /// File inside `decals/` (`masks/<name>.dds`): white, alpha = the mask.
    pub file: String,
    /// The opacity texture it was made from.
    pub texture: String,
    /// The channel of that texture (`r`, `g`, `b` or `a`).
    pub channel: &'static str,
    /// Constant the material multiplies the mask by (already applied).
    pub scale: f32,
    /// Width and height of the largest mip.
    pub size: [u32; 2],
    /// Mips written.
    pub mips: u32,
}

/// `manifest.json`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: &'static str,
    /// Version of the map files.
    pub decals_version: u32,
    /// Maps, in file-name order.
    pub maps: Vec<ManifestEntry>,
    /// Totals over the maps.
    pub totals: DecalStats,
    /// Decal masks by material path (the materials of the maps converted
    /// in this run whose shape is in an opacity texture only).
    pub masks: BTreeMap<String, MaskEntry>,
}

// ---------------------------------------------------------------------------
// Decal masks.
// ---------------------------------------------------------------------------

/// Where a decal material keeps its shape when the displayed channel has no
/// texture.
#[derive(Debug, Clone, PartialEq)]
pub struct MaskSource {
    /// Opacity texture path.
    pub texture: String,
    /// Channel index (0 red, 1 green, 2 blue, 3 alpha).
    pub channel: usize,
    /// Multiplier of the channel.
    pub scale: f32,
    /// Added after the multiplier.
    pub bias: f32,
}

impl MaskSource {
    /// Channel letter.
    #[must_use]
    pub fn channel_name(&self) -> &'static str {
        ["r", "g", "b", "a"]
            .get(self.channel)
            .copied()
            .unwrap_or("a")
    }
}

/// The mask a material needs, if any: it is masked or translucent, the
/// channel it displays (emissive when unlit, else the base colour) has no
/// texture, and its opacity comes from a texture. A colour output wired to
/// the scalar opacity input gives its first component (`rgb` reads red, as
/// the engine's cast to a scalar does; STRONG, UE3 convention).
#[must_use]
pub fn mask_source(a: &ApproxMaterial) -> Option<MaskSource> {
    let shown = if a.unlit { &a.emissive } else { &a.base_color };
    mask_source_of(a.alpha_mode, shown, a.opacity.as_ref())
}

/// [`mask_source`] over the parts it reads: the alpha mode, the displayed
/// channel and the opacity channel.
#[must_use]
pub fn mask_source_of(
    alpha_mode: &str,
    shown: &Channel,
    opacity: Option<&Channel>,
) -> Option<MaskSource> {
    if alpha_mode == "opaque" {
        return None;
    }
    if shown.texture.as_ref().is_some_and(|t| t.texture.is_some()) {
        return None;
    }
    let opacity = opacity.filter(|o| o.resolved)?;
    let binding = opacity.texture.as_ref()?;
    let texture = binding.texture.clone()?;
    if binding.sampler != "2d" {
        return None;
    }
    let channel = match binding.channels.chars().next()? {
        'r' => 0,
        'g' => 1,
        'b' => 2,
        'a' => 3,
        _ => return None,
    };
    let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
    Some(MaskSource {
        texture,
        channel,
        scale: finite(opacity.value[0], 1.0),
        bias: finite(opacity.bias[0], 0.0),
    })
}

/// The sRGB transfer function's inverse (texel value → linear), as the GPU
/// applies it to the colour channels of an sRGB texture.
#[must_use]
pub fn srgb_to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The mask values (0–255, linear) of tightly packed RGBA8 texels: channel
/// `source.channel` (a colour channel of an sRGB texture is linearised
/// first, alpha never is), times the material's multiplier plus its bias.
#[must_use]
pub fn mask_alpha(rgba: &[u8], source: &MaskSource, srgb: bool) -> Vec<u8> {
    let channel = source.channel.min(3);
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|px| {
            let raw = px[channel];
            let v = if srgb && channel < 3 {
                srgb_to_linear(raw)
            } else {
                f32::from(raw) / 255.0
            };
            let v = (v * source.scale + source.bias).clamp(0.0, 1.0);
            // The clamp makes the cast exact (NaN becomes 0).
            (v * 255.0).round() as u8
        })
        .collect()
}

/// The mip chain of a mask as `A8R8G8B8` texels (B, G, R, A: white with
/// `alpha`), largest first down to 1 × 1, each mip the 2 × 2 box average of
/// the one above. `alpha` holds `w × h` values (missing ones count as 0);
/// `w` and `h` are clamped to 1..=[`MASK_MAX_EDGE`].
#[must_use]
pub fn mask_mips(w: u32, h: u32, alpha: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let (mut w, mut h) = (
        w.clamp(1, MASK_MAX_EDGE) as usize,
        h.clamp(1, MASK_MAX_EDGE) as usize,
    );
    let mut level: Vec<u8> = (0..w * h)
        .map(|i| alpha.get(i).copied().unwrap_or(0))
        .collect();
    loop {
        out.push(level.iter().flat_map(|a| [255, 255, 255, *a]).collect());
        if w == 1 && h == 1 {
            break;
        }
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = Vec::with_capacity(nw * nh);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = 0u32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(w - 1);
                    let sy = (y * 2 + dy).min(h - 1);
                    sum += u32::from(level[sy * w + sx]);
                }
                next.push(u8::try_from((sum + 2) / 4).unwrap_or(u8::MAX));
            }
        }
        (w, h, level) = (nw, nh, next);
    }
    out
}

/// A file name for the mask of `material`: its path with everything but
/// ASCII letters, digits, `-` and `_` replaced, plus a hash of the exact path
/// (FNV-1a, lower-cased), so the name depends on nothing but the material.
#[must_use]
pub fn mask_file_name(material: &str) -> String {
    let stem: String = material
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    let mut hash = 0x811C_9DC5_u32;
    for b in material.to_ascii_lowercase().bytes() {
        hash = (hash ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("m_{stem}-{hash:08x}.dds")
}

/// Decodes the largest stored mip of the texture at `path` that fits
/// [`MASK_MAX_EDGE`] to RGBA8: (width, height, texels, sRGB).
fn load_mask_texels(
    set: &PackageSet,
    decoder: &TextureDecoder<'_>,
    caches: &TextureFileCaches,
    path: &str,
) -> Result<(u32, u32, Vec<u8>, bool)> {
    let (lp, index) = set
        .locate(path)
        .with_context(|| format!("texture {path} not found"))?;
    let tex = decoder.decode(&lp, index)?;
    let format = tex
        .format
        .with_context(|| format!("unknown pixel format {:?}", tex.props.format))?;
    let mips = tex.mips();
    let mip0 = mips.first().context("texture without mips")?;
    let (w0, h0) = (
        u32::try_from(mip0.size_x).context("negative mip width")?,
        u32::try_from(mip0.size_y).context("negative mip height")?,
    );
    let first = tex.first_stored_mip().context("no stored mip")?;
    let level = (first..mips.len())
        .find(|&l| {
            let (w, h) = mip_dims(w0, h0, u32::try_from(l).unwrap_or(u32::MAX));
            w.max(h) <= MASK_MAX_EDGE
        })
        .context("no mip small enough")?;
    let (w, h) = mip_dims(w0, h0, u32::try_from(level)?);
    let payload = lp.package.export_data(tex.export_index)?;
    let texels = tex.load_mip(payload, Some(caches), level)?;
    let rgba = decode_to_rgba8(format, w, h, &texels)?;
    Ok((w, h, rgba, tex.props.srgb.unwrap_or(false)))
}

/// Builds decal masks, once per material.
struct MaskBaker<'a> {
    set: &'a PackageSet,
    materials: MaterialDecoder<'a>,
    textures: TextureDecoder<'a>,
    caches: TextureFileCaches,
    /// Result per material path (lower case): `None` when it needs no mask
    /// or the mask could not be built.
    done: BTreeMap<String, Option<MaskEntry>>,
    /// Masks built and not yet written: (file, DDS bytes).
    pending: Vec<(String, Vec<u8>)>,
    built: usize,
}

impl<'a> MaskBaker<'a> {
    fn new(set: &'a PackageSet, cooked_dirs: &[PathBuf]) -> Self {
        Self {
            set,
            materials: MaterialDecoder::new(set),
            textures: TextureDecoder::new(set),
            caches: TextureFileCaches::discover(cooked_dirs),
            done: BTreeMap::new(),
            pending: Vec::new(),
            built: 0,
        }
    }

    /// The mask of `material`, built on first use (problems are reported
    /// once and leave the material without a mask).
    fn mask(&mut self, material: &str) -> Option<MaskEntry> {
        let key = material.to_ascii_lowercase();
        if let Some(known) = self.done.get(&key) {
            return known.clone();
        }
        let entry = self.build(material);
        self.done.insert(key, entry.clone());
        entry
    }

    fn build(&mut self, material: &str) -> Option<MaskEntry> {
        let (lp, index) = self.set.locate(material)?;
        let approx = self.materials.approximate(&lp, index).ok()?;
        let source = mask_source(&approx)?;
        if self.built >= MAX_MASKS {
            if self.built == MAX_MASKS {
                self.built += 1;
                eprintln!("asamu-import: more than {MAX_MASKS} decal masks; the rest left out");
            }
            return None;
        }
        let built = load_mask_texels(self.set, &self.textures, &self.caches, &source.texture)
            .and_then(|(w, h, rgba, srgb)| {
                let mips = mask_mips(w, h, &mask_alpha(&rgba, &source, srgb));
                let count = u32::try_from(mips.len())?;
                let dds =
                    crate::textures::dds::encode(PixelFormat::A8R8G8B8, w, h, false, &[mips])?;
                Ok((w, h, count, dds))
            });
        match built {
            Ok((w, h, mips, dds)) => {
                self.built += 1;
                let file = format!("{MASKS_DIR}/{}", mask_file_name(material));
                self.pending.push((file.clone(), dds));
                Some(MaskEntry {
                    file,
                    texture: source.texture.clone(),
                    channel: source.channel_name(),
                    scale: source.scale,
                    size: [w, h],
                    mips,
                })
            }
            Err(e) => {
                eprintln!(
                    "asamu-import: decal mask of {material} ({}): {e:#}",
                    source.texture
                );
                None
            }
        }
    }
}

/// A finite `f32` rounded to `decimals` places, as a JSON number (0 for
/// non-finite input: JSON cannot carry NaN or infinities).
fn num(v: f32, decimals: i32) -> Value {
    if !v.is_finite() {
        return json!(0.0);
    }
    let scale = 10f64.powi(decimals);
    let r = (f64::from(v) * scale).round() / scale;
    json!(if r == 0.0 { 0.0 } else { r })
}

fn vec3(v: [f32; 3], decimals: i32) -> Value {
    Value::Array(v.iter().map(|c| num(*c, decimals)).collect())
}

fn flat3(items: &[[f32; 3]], decimals: i32) -> Value {
    Value::Array(
        items
            .iter()
            .flat_map(|v| v.iter().map(|c| num(*c, decimals)))
            .collect(),
    )
}

fn receiver_json(r: &ReceiverMesh) -> Value {
    json!({
        "component": r.component,
        "component_class": r.component_class,
        "source": r.source,
        "positions": flat3(&r.positions, 2),
        "normals": flat3(&r.normals, 3),
        "uvs": Value::Array(r.uvs.iter().flat_map(|v| [num(v[0], 4), num(v[1], 4)]).collect()),
        "indices": Value::Array(r.triangles.iter().flatten().map(|i| json!(i)).collect()),
        "outside": r.outside,
        "has_light_map": r.has_light_map,
        "listed": r.listed,
    })
}

fn decal_json(d: &MapDecal) -> Value {
    let p = &d.params;
    let mut o = Map::new();
    o.insert("slot".into(), json!(d.slot));
    o.insert("name".into(), json!(d.name));
    o.insert("path".into(), json!(d.path));
    o.insert("class".into(), json!(d.class));
    o.insert("decal_actor".into(), json!(d.decal_actor));
    o.insert("component".into(), json!(d.component));
    o.insert("hidden".into(), json!(d.hidden || p.hidden_game));
    o.insert("movable".into(), json!(p.movable_decal));
    o.insert("static_decal".into(), json!(p.static_decal));
    o.insert("tag".into(), json!(d.tag));
    o.insert("base".into(), json!(d.base));
    o.insert("material".into(), json!(p.material));
    o.insert("location".into(), vec3(d.location, 3));
    o.insert("rotation".into(), json!(d.rotation));
    o.insert("draw_scale".into(), num(d.draw_scale, 4));
    o.insert("width".into(), num(p.width, 4));
    o.insert("height".into(), num(p.height, 4));
    o.insert("near_plane".into(), num(p.near_plane, 4));
    o.insert("far_plane".into(), num(p.far_plane, 4));
    o.insert("tile".into(), json!([num(p.tile_x, 4), num(p.tile_y, 4)]));
    o.insert(
        "offset".into(),
        json!([num(p.offset_x, 4), num(p.offset_y, 4)]),
    );
    o.insert("rotation_degrees".into(), num(p.rotation_degrees, 4));
    o.insert("depth_bias".into(), json!(p.depth_bias));
    o.insert(
        "slope_scale_depth_bias".into(),
        json!(p.slope_scale_depth_bias),
    );
    o.insert("sort_order".into(), json!(p.sort_order));
    o.insert(
        "blend_range".into(),
        json!([num(p.blend_range[0], 4), num(p.blend_range[1], 4)]),
    );
    o.insert("no_clip".into(), json!(p.no_clip));
    o.insert("project_on_backfaces".into(), json!(p.project_on_backfaces));
    o.insert("mirrored".into(), json!(d.mirrored));
    o.insert(
        "frame".into(),
        json!({
            "origin": vec3(d.frame.origin, 3),
            "direction": vec3(d.frame.direction, 6),
            "width_axis": vec3(d.frame.width_axis, 6),
            "height_axis": vec3(d.frame.height_axis, 6),
        }),
    );
    o.insert("static_receivers".into(), json!(d.static_receivers));
    o.insert("unclipped_receivers".into(), json!(d.unclipped_receivers));
    o.insert("hidden_receivers".into(), json!(d.hidden_receivers));
    o.insert(
        "receivers".into(),
        Value::Array(
            d.receivers
                .iter()
                .filter(|r| !r.triangles.is_empty())
                .map(receiver_json)
                .collect(),
        ),
    );
    o.insert("unresolved_receivers".into(), json!(d.unresolved_receivers));
    Value::Object(o)
}

/// The JSON document of one map (rounded, flat geometry arrays). `masks`
/// are the decal masks of the map's materials, by material path.
#[must_use]
pub fn map_json(m: &MapDecals, masks: &BTreeMap<String, MaskEntry>) -> Value {
    let mut warnings: Vec<&String> = m.warnings.iter().take(MAX_WARNINGS).collect();
    let dropped = m.warnings.len().saturating_sub(warnings.len());
    let note = format!("{dropped} more warnings");
    if dropped > 0 {
        warnings.push(&note);
    }
    json!({
        "format": m.format,
        "version": m.version,
        "package": m.package,
        "coordinates": m.coordinates,
        "notice": "derived from your own install of the original game: keep local, do not redistribute",
        "decals": Value::Array(m.decals.iter().map(decal_json).collect()),
        "masks": masks,
        "stats": m.stats,
        "warnings": warnings,
    })
}

/// Cooked package folder and root of the install.
fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.root))
}

fn write_file(dir: &Path, name: &str, data: &[u8], input: &Path, force: bool) -> Result<PathBuf> {
    let target = safety::check_output_path(&dir.join(name), input, force)?;
    safety::write_output(&target, data, force)?;
    Ok(target)
}

fn summary_line(s: &DecalStats, map: &str) -> String {
    format!(
        "{map:<18} components {:>3} (exact {:>3})  decals {:>3} (decal actors {:>3}, movable {:>3}, \
         hidden {:>1}, mirrored {:>1}, cooked {:>2}, with geometry {:>3})  receiver meshes {:>4} \
         (projected {:>4}, unlisted {:>3}, BSP {:>1})  triangles {:>5}  unresolved {:>1}",
        s.components,
        s.components_exact,
        s.actors,
        s.decal_actors,
        s.movable,
        s.hidden,
        s.mirrored,
        s.with_static_receivers,
        s.with_geometry,
        s.receiver_meshes,
        s.projected_receivers,
        s.world_receivers,
        s.bsp_receivers,
        s.triangles,
        s.unresolved_receivers
    )
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install_root) = install_dirs(ctx)?;
    let maps_dir = cooked.join("Maps");
    let maps = select_maps(&maps_dir, &args.maps)?;
    if maps.is_empty() {
        bail!("no map packages in {}", maps_dir.display());
    }
    let out_dir = if args.check {
        None
    } else {
        let d = prepare_dir(&ctx.out.join("decals"), &maps_dir, &install_root)?;
        eprintln!(
            "writing decals derived from your own install to {} (do not redistribute)",
            d.display()
        );
        Some(d)
    };
    let set = PackageSet::new(&[maps_dir.clone(), cooked.clone()]);
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        decals_version: asamu_ue3::decal::DECALS_VERSION,
        maps: Vec::new(),
        totals: DecalStats::default(),
        masks: BTreeMap::new(),
    };
    let mut materials = BTreeSet::new();
    let mut baker = MaskBaker::new(&set, &[cooked.clone(), maps_dir.clone()]);
    for file in &maps {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        let m = extract_map_decals(&set, &lp)
            .with_context(|| format!("decals of {}", file.display()))?;
        println!("{}", summary_line(&m.stats, &m.package));
        // The masks of this map's decal materials (built once per material;
        // always rewritten: a few dozen small images).
        let mut map_masks = BTreeMap::new();
        for material in m.decals.iter().filter_map(|d| d.params.material.as_ref()) {
            materials.insert(material.clone());
            if let Some(entry) = baker.mask(material) {
                map_masks.insert(material.clone(), entry);
            }
        }
        if let Some(dir) = &out_dir {
            if !baker.pending.is_empty() {
                prepare_dir(&dir.join(MASKS_DIR), &maps_dir, &install_root)?;
            }
            for (name, dds) in baker.pending.drain(..) {
                write_file(dir, &name, &dds, &maps_dir, true)?;
            }
        } else {
            baker.pending.clear();
        }
        manifest
            .masks
            .extend(map_masks.iter().map(|(k, v)| (k.clone(), v.clone())));
        if m.stats.components != m.stats.components_exact {
            eprintln!(
                "{}: {} decal components did not decode exactly",
                m.package,
                m.stats.components - m.stats.components_exact
            );
        }
        manifest.totals.add(&m.stats);
        let file_name = match &out_dir {
            Some(dir) if !m.decals.is_empty() || args.include_empty => {
                let name = format!("{}.decals.json", m.package);
                let doc = map_json(&m, &map_masks);
                let bytes = if args.pretty {
                    serde_json::to_vec_pretty(&doc)?
                } else {
                    serde_json::to_vec(&doc)?
                };
                write_file(dir, &name, &bytes, file, args.force)?;
                Some(name)
            }
            _ => None,
        };
        manifest.maps.push(ManifestEntry {
            map: m.package.clone(),
            file: file_name,
            stats: m.stats,
        });
    }
    println!("{}", summary_line(&manifest.totals, "total"));
    println!(
        "decal materials {:>3}  with a mask from an opacity texture {:>3}",
        materials.len(),
        manifest.masks.len()
    );
    if let Some(dir) = &out_dir {
        write_file(
            dir,
            "manifest.json",
            &serde_json::to_vec_pretty(&manifest)?,
            &maps_dir,
            true,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_ue3::decal::{
        DECAL_COORDINATES, DECALS_FORMAT, DECALS_VERSION, DecalFrame, ReceiverSource, decal_params,
    };

    fn sample() -> MapDecals {
        let params = decal_params(&[]);
        let frame = DecalFrame::new([1.0, 2.0, 3.0], [0, 16384, 0], 0.0);
        MapDecals {
            format: DECALS_FORMAT,
            version: DECALS_VERSION,
            package: "Test".into(),
            coordinates: DECAL_COORDINATES,
            decals: vec![MapDecal {
                slot: 7,
                name: "DecalActorMovable_0".into(),
                path: "Test.TheWorld.PersistentLevel.DecalActorMovable_0".into(),
                class: "Engine.DecalActorMovable".into(),
                decal_actor: true,
                component: None,
                location: [1.0, 2.0, 3.0],
                rotation: [0, 16384, 0],
                draw_scale: 1.0,
                draw_scale3d: [1.0; 3],
                hidden: true,
                tag: None,
                base: None,
                params,
                mirrored: false,
                frame,
                static_receivers: 0,
                unclipped_receivers: 0,
                hidden_receivers: 0,
                receivers: vec![
                    ReceiverMesh {
                        component: Some("Test.C".into()),
                        component_class: None,
                        source: ReceiverSource::Projected,
                        positions: vec![[1.234_567, 0.0, f32::NAN], [0.0; 3], [0.0; 3]],
                        normals: vec![[0.0, 0.0, 1.0]; 3],
                        uvs: vec![[0.123_456_7, 1.0]; 3],
                        triangles: vec![[0, 1, 2]],
                        outside: 0,
                        has_light_map: false,
                        listed: true,
                    },
                    ReceiverMesh {
                        component: Some("Test.Empty".into()),
                        component_class: None,
                        source: ReceiverSource::Projected,
                        positions: Vec::new(),
                        normals: Vec::new(),
                        uvs: Vec::new(),
                        triangles: Vec::new(),
                        outside: 0,
                        has_light_map: false,
                        listed: true,
                    },
                ],
                unresolved_receivers: vec!["Test.Missing".into()],
            }],
            stats: DecalStats::default(),
            warnings: (0..70).map(|i| format!("w{i}")).collect(),
        }
    }

    #[test]
    fn map_json_flattens_rounds_and_caps() {
        let mut masks = BTreeMap::new();
        masks.insert(
            "Pkg.M_Decal".to_owned(),
            MaskEntry {
                file: format!("{MASKS_DIR}/{}", mask_file_name("Pkg.M_Decal")),
                texture: "Pkg.T_Mask".into(),
                channel: "r",
                scale: 1.0,
                size: [4, 2],
                mips: 3,
            },
        );
        let v = map_json(&sample(), &masks);
        assert_eq!(v["masks"]["Pkg.M_Decal"]["channel"], "r");
        assert!(
            v["masks"]["Pkg.M_Decal"]["file"]
                .as_str()
                .unwrap()
                .starts_with("masks/m_Pkg_M_Decal-")
        );
        assert_eq!(v["format"], DECALS_FORMAT);
        let d = &v["decals"][0];
        assert_eq!(d["slot"], 7);
        assert_eq!(d["hidden"], true);
        // Empty receivers are dropped; geometry is flat and rounded.
        let recv = d["receivers"].as_array().unwrap();
        assert_eq!(recv.len(), 1);
        let pos = recv[0]["positions"].as_array().unwrap();
        assert_eq!(pos.len(), 9);
        assert_eq!(pos[0].as_f64().unwrap(), 1.23);
        assert_eq!(pos[2].as_f64().unwrap(), 0.0, "NaN becomes 0");
        assert_eq!(recv[0]["uvs"][0].as_f64().unwrap(), 0.1235);
        assert_eq!(recv[0]["indices"].as_array().unwrap().len(), 3);
        assert_eq!(recv[0]["source"], "projected");
        assert_eq!(recv[0]["listed"], true);
        assert_eq!(d["mirrored"], false);
        assert_eq!(d["unresolved_receivers"][0], "Test.Missing");
        // Warnings capped with a count line.
        let w = v["warnings"].as_array().unwrap();
        assert_eq!(w.len(), MAX_WARNINGS + 1);
        assert_eq!(w[MAX_WARNINGS], "6 more warnings");
        // Frame axes of yaw 90°: forward +Y.
        let dir = d["frame"]["direction"].as_array().unwrap();
        assert!((dir[1].as_f64().unwrap() - 1.0).abs() < 1e-6);
    }

    fn channel(texture: Option<(&str, &str)>, value: f32, bias: f32) -> Channel {
        use asamu_ue3::material::{ChannelSource, TextureBinding, UvTransform};
        Channel {
            source: ChannelSource::Expression,
            value: [value; 4],
            bias: [bias; 4],
            texture: texture.map(|(path, channels)| TextureBinding {
                texture: Some(path.to_owned()),
                parameter: None,
                sampler: "2d",
                channels: channels.to_owned(),
                uv: UvTransform {
                    channel: 0,
                    scale: [1.0, 1.0],
                    offset: [0.0, 0.0],
                    panning: [0.0, 0.0],
                    rotation: 0.0,
                    rotation_angle: 0.0,
                    rotation_center: [0.5, 0.5],
                },
            }),
            vertex_color: false,
            resolved: true,
        }
    }

    #[test]
    fn masks_are_for_materials_that_show_no_texture() {
        let constant = channel(None, 1.0, 0.0);
        let textured = channel(Some(("Pkg.T_Colour", "rgb")), 1.0, 0.0);
        // A constant glow whose shape is another texture's alpha.
        let alpha = channel(Some(("Pkg.T_Mask", "a")), 1.0, 0.0);
        let m = mask_source_of("mask", &constant, Some(&alpha)).unwrap();
        assert_eq!(
            (m.texture.as_str(), m.channel, m.channel_name()),
            ("Pkg.T_Mask", 3, "a")
        );
        assert_eq!((m.scale, m.bias), (1.0, 0.0));
        // A colour output wired to the opacity reads its first component.
        let rgb = channel(Some(("Pkg.T_Mask", "rgb")), 0.5, 0.25);
        let m = mask_source_of("blend", &constant, Some(&rgb)).unwrap();
        assert_eq!(
            (m.channel, m.channel_name(), m.scale, m.bias),
            (0, "r", 0.5, 0.25)
        );
        for (channels, index) in [("g", 1), ("b", 2), ("r", 0), ("rgba", 0)] {
            let c = channel(Some(("Pkg.T", channels)), 1.0, 0.0);
            assert_eq!(
                mask_source_of("mask", &constant, Some(&c)).unwrap().channel,
                index
            );
        }
        // No mask: opaque, a displayed texture (the shared model uses its
        // alpha), no opacity, a constant opacity, an unresolved graph, an
        // unknown channel, a cube sampler.
        assert_eq!(mask_source_of("opaque", &constant, Some(&alpha)), None);
        assert_eq!(mask_source_of("mask", &textured, Some(&alpha)), None);
        assert_eq!(mask_source_of("mask", &constant, None), None);
        assert_eq!(mask_source_of("mask", &constant, Some(&constant)), None);
        let mut unresolved = alpha.clone();
        unresolved.resolved = false;
        assert_eq!(mask_source_of("mask", &constant, Some(&unresolved)), None);
        let odd = channel(Some(("Pkg.T", "xyz")), 1.0, 0.0);
        assert_eq!(mask_source_of("mask", &constant, Some(&odd)), None);
        let empty = channel(Some(("Pkg.T", "")), 1.0, 0.0);
        assert_eq!(mask_source_of("mask", &constant, Some(&empty)), None);
        let mut cube = alpha.clone();
        if let Some(t) = cube.texture.as_mut() {
            t.sampler = "cube";
        }
        assert_eq!(mask_source_of("mask", &constant, Some(&cube)), None);
        // Non-finite factors fall back to the neutral ones.
        let nan = channel(Some(("Pkg.T", "a")), f32::NAN, f32::INFINITY);
        let m = mask_source_of("mask", &constant, Some(&nan)).unwrap();
        assert_eq!((m.scale, m.bias), (1.0, 0.0));
    }

    #[test]
    fn mask_values_follow_the_channel_and_the_colour_space() {
        let source = |channel| MaskSource {
            texture: "Pkg.T".into(),
            channel,
            scale: 1.0,
            bias: 0.0,
        };
        // Two texels: (r, g, b, a).
        let rgba = [0u8, 128, 255, 64, 255, 0, 10, 200];
        // Alpha is never linearised.
        assert_eq!(mask_alpha(&rgba, &source(3), true), vec![64, 200]);
        assert_eq!(mask_alpha(&rgba, &source(3), false), vec![64, 200]);
        // A colour channel of an sRGB texture is: 128 → 0.2158 → 55.
        assert_eq!(mask_alpha(&rgba, &source(1), true), vec![55, 0]);
        assert_eq!(mask_alpha(&rgba, &source(1), false), vec![128, 0]);
        assert_eq!(mask_alpha(&rgba, &source(0), true), vec![0, 255]);
        assert_eq!(mask_alpha(&rgba, &source(2), true), vec![255, 1]);
        // The transfer function's fixed points and its linear toe.
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
        assert!((srgb_to_linear(10) - 10.0 / 255.0 / 12.92).abs() < 1e-7);
        assert!((srgb_to_linear(188) - 0.5029).abs() < 1e-3);
        // Multiplier and bias, clamped; a wild channel index reads alpha;
        // NaN factors give 0, not a panic.
        let scaled = MaskSource {
            scale: 2.0,
            bias: -0.5,
            ..source(3)
        };
        assert_eq!(mask_alpha(&rgba, &scaled, false), vec![1, 255]);
        assert_eq!(mask_alpha(&rgba, &source(99), false), vec![64, 200]);
        let nan = MaskSource {
            scale: f32::NAN,
            ..source(3)
        };
        assert_eq!(mask_alpha(&rgba, &nan, false), vec![0, 0]);
        // A trailing partial texel is ignored.
        assert_eq!(mask_alpha(&rgba[..7], &source(3), false), vec![64]);
        assert!(mask_alpha(&[], &source(3), false).is_empty());
    }

    #[test]
    fn mask_mips_average_down_to_one_texel() {
        // 4 × 2: alpha 0, 100, 200, 255 / 255, 255, 0, 0.
        let alpha = [0u8, 100, 200, 255, 255, 255, 0, 0];
        let mips = mask_mips(4, 2, &alpha);
        assert_eq!(mips.len(), 3, "4x2, 2x1, 1x1");
        assert_eq!(mips[0].len(), 4 * 2 * 4);
        // White B, G, R with the alpha last.
        assert_eq!(&mips[0][..8], &[255, 255, 255, 0, 255, 255, 255, 100]);
        // 2 × 1: (0 + 100 + 255 + 255) / 4 = 152.5 → 153, (200 + 255) / 4 → 114.
        assert_eq!(mips[1], vec![255, 255, 255, 153, 255, 255, 255, 114]);
        // 1 × 1: the single row is sampled twice.
        assert_eq!(mips[2], vec![255, 255, 255, 134]);
        // Sizes follow the usual chain, also for odd sizes and one texel.
        let mips = mask_mips(5, 3, &[255; 15]);
        let sizes: Vec<usize> = mips.iter().map(|m| m.len() / 4).collect();
        assert_eq!(sizes, vec![15, 2, 1]);
        assert!(mips.iter().flatten().all(|v| *v == 255));
        assert_eq!(mask_mips(1, 1, &[7]), vec![vec![255, 255, 255, 7]]);
        // Missing texels count as transparent; a zero size is one texel.
        assert_eq!(
            mask_mips(2, 1, &[9]),
            vec![
                vec![255, 255, 255, 9, 255, 255, 255, 0],
                vec![255, 255, 255, 5]
            ]
        );
        assert_eq!(mask_mips(0, 0, &[]), vec![vec![255, 255, 255, 0]]);
        // Absurd sizes are clamped, not multiplied out.
        let huge = mask_mips(u32::MAX, u32::MAX, &[]);
        assert_eq!(huge[0].len(), (MASK_MAX_EDGE * MASK_MAX_EDGE * 4) as usize);
        assert_eq!(huge.len(), 11);
        // The encoder takes the chain as an A8R8G8B8 texture.
        let mips = mask_mips(4, 2, &alpha);
        let dds =
            crate::textures::dds::encode(PixelFormat::A8R8G8B8, 4, 2, false, &[mips]).unwrap();
        assert_eq!(&dds[..4], b"DDS ");
    }

    #[test]
    fn mask_file_names_are_plain_and_depend_on_the_material_only() {
        let a = mask_file_name("CavePaintings.Materials.CavePainting_Beak_INST");
        assert!(a.starts_with("m_CavePaintings_Materials_CavePainting_Beak_INST-"));
        assert!(a.ends_with(".dds"));
        assert_eq!(
            a,
            mask_file_name("CavePaintings.Materials.CavePainting_Beak_INST")
        );
        // Paths are case-insensitive: the hash is too.
        let hash = |s: &str| s.rsplit_once('-').unwrap().1.to_owned();
        assert_eq!(
            hash(&a),
            hash(&mask_file_name(
                "cavepaintings.materials.cavepainting_beak_inst"
            ))
        );
        // Names that sanitise alike still differ.
        assert_ne!(mask_file_name("A.B"), mask_file_name("A_B"));
        // Nothing that could leave the folder or confuse a loader.
        for hostile in [
            "../../etc/passwd",
            "a/b\\c:d",
            "",
            "x#y?z",
            "CON",
            "name\0nul",
            "é✓",
        ] {
            let f = mask_file_name(hostile);
            assert!(
                f.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{f}"
            );
            assert!(
                f.starts_with("m_") && f.ends_with(".dds") && !f.contains(".."),
                "{f}"
            );
        }
        assert!(mask_file_name(&"x".repeat(5000)).len() < 128);
    }

    #[test]
    fn num_handles_non_finite_and_negative_zero() {
        assert_eq!(num(f32::INFINITY, 2), json!(0.0));
        assert_eq!(num(-0.000_1, 2), json!(0.0));
        assert_eq!(num(2.005, 1), json!(2.0));
    }
}
