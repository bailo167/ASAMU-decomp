//! `asamu-import textures`: Texture2D (and lightmaps, shadow maps, flip books,
//! cube maps) → DDS, with optional PNG previews and a JSON manifest for the
//! runtime.
//!
//! Everything written is derived from the user's own copy of the game: it goes
//! to the user-local output directory only (never the repository, except a
//! git-ignored `research/` subfolder, and never the install), and must not be
//! redistributed. `--check` writes nothing: it decodes every texture, loads
//! and decompresses every mip, and prints the coverage report described in
//! `docs/reverse-engineering/TEXTURES.md`.
//!
//! Output layout under `<out>/textures/`:
//!
//! ```text
//! manifest.json                         object path -> file, format, size, ...
//! <Package>/<Object>/<Path>.dds         every mip that is stored, largest first
//! <Package>/<Object>/<Path>.png         preview (with --png)
//! ```
//!
//! An object path that occurs in several packages (a texture cooked into more
//! than one map) is written once, from the first package that contains it;
//! the manifest lists the other packages under `also_in`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::PackageSet;
use asamu_ue3::bulkdata::TextureFileCaches;
use asamu_ue3::model::LoadedPackage;
use asamu_ue3::texture::{
    PixelFormat, Texture, TextureClass, TextureCoverage, TextureDecoder, TextureNative,
    decode_to_rgba8, mip_dims,
};
use serde::{Deserialize, Serialize};

use crate::safety;

/// Manifest format version.
const MANIFEST_VERSION: u32 = 1;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only packages whose file name contains this text (case-insensitive;
    /// repeatable).
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Only textures whose object path contains this text (case-insensitive).
    #[arg(long)]
    name: Option<String>,
    /// Stop after converting this many textures.
    #[arg(long)]
    limit: Option<usize>,
    /// Also write PNG previews.
    #[arg(long)]
    png: bool,
    /// Largest preview edge in pixels: the preview uses the largest stored mip
    /// that fits (0 = always the largest stored mip).
    #[arg(long, default_value_t = 512)]
    png_max: u32,
    /// Skip lightmaps and shadow maps (LightMapTexture2D, ShadowMapTexture2D).
    #[arg(long)]
    skip_lighting: bool,
    /// Overwrite files that already exist (otherwise they are kept and skipped).
    #[arg(long)]
    force: bool,
    /// Write nothing: decode every texture, load and decompress every mip, and
    /// print the coverage report.
    #[arg(long)]
    check: bool,
    /// With --check: print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Convert everything in memory (DDS and, with --png, PNG encoding) but
    /// write nothing, not even the output directory or the manifest.
    #[arg(long)]
    dry_run: bool,
}

/// One converted texture in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestEntry {
    /// Package file stem the data was taken from.
    pub package: String,
    /// Texture class name.
    pub class: String,
    /// DDS file, relative to the `textures/` folder.
    pub file: String,
    /// PNG preview, relative to the `textures/` folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
    /// Original `EPixelFormat` name.
    pub format: String,
    /// `SizeX` x `SizeY` of the texture.
    pub size: [u32; 2],
    /// Size of the largest mip written (smaller than `size` when the cooker
    /// stripped the top mips).
    pub written_size: [u32; 2],
    /// Mips written.
    pub mips: u32,
    /// True for a cube map (six faces in the DDS).
    #[serde(default)]
    pub cube: bool,
    /// `SRGB` (absent when unknown: `LightMapTexture2D` has no default
    /// object in the data, so an untagged value cannot be resolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub srgb: Option<bool>,
    /// `AddressX` / `AddressY`.
    pub address: [String; 2],
    /// `Filter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// `LODGroup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lod_group: Option<String>,
    /// `CompressionSettings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_settings: Option<String>,
    /// `LightmapFlags` (lightmaps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lightmap_flags: Option<u32>,
    /// `ShadowmapFlags` (shadow maps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadowmap_flags: Option<i32>,
    /// Other packages holding the same object path (not written again).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
}

/// `manifest.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    /// Format version.
    pub version: u32,
    /// Notice about the origin of the data.
    pub notice: String,
    /// Textures by qualified object path.
    pub textures: BTreeMap<String, ManifestEntry>,
}

const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
     Copyrighted game data: do not redistribute.";

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, maps, install_root) = cooked_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps.clone()];
    let files = package_files(&dirs, &args.packages)?;
    if files.is_empty() {
        bail!("no package matches the --package filters");
    }
    let set = PackageSet::new(&dirs);
    let caches = TextureFileCaches::discover(&dirs);
    let decoder = TextureDecoder::new(&set);

    if args.check {
        return check(&set, &decoder, &caches, &files, args.json);
    }

    let root = if args.dry_run {
        ctx.out.join("textures")
    } else {
        prepare_out_dir(
            &ctx.out,
            files.first().map(PathBuf::as_path),
            Some(&install_root),
        )?
    };
    let manifest_path = root.join("manifest.json");
    let mut manifest = if args.dry_run {
        Manifest::default()
    } else {
        read_manifest(&manifest_path)?
    };
    let mut stats = RunStats::default();
    let mut names = OutputNames::default();
    'packages: for file in &files {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        for index in 0..lp.package.exports.len() {
            if args.limit.is_some_and(|l| stats.written + stats.kept >= l) {
                break 'packages;
            }
            let Some((_, class)) = decoder.export_class(&lp, index) else {
                continue;
            };
            if args.skip_lighting
                && matches!(
                    class,
                    TextureClass::LightMapTexture2D | TextureClass::ShadowMapTexture2D
                )
            {
                continue;
            }
            let tex = match decoder.decode(&lp, index) {
                Ok(t) => t,
                Err(e) => {
                    stats.failed += 1;
                    eprintln!("asamu-import: export {index} of {}: {e}", lp.name);
                    continue;
                }
            };
            if tex.is_default_object {
                continue;
            }
            if let Some(filter) = &args.name
                && !tex
                    .path
                    .to_ascii_lowercase()
                    .contains(&filter.to_ascii_lowercase())
            {
                continue;
            }
            if let Some(existing) = manifest.textures.get_mut(&tex.path)
                && !existing.package.eq_ignore_ascii_case(&lp.name)
            {
                if !existing.also_in.iter().any(|p| p == &lp.name) {
                    existing.also_in.push(lp.name.clone());
                }
                stats.duplicates += 1;
                if !same_header(existing, &tex) {
                    stats.variant_duplicates += 1;
                    eprintln!(
                        "asamu-import: {} in {} differs from the copy in {} (kept the first)",
                        tex.path, lp.name, existing.package
                    );
                }
                continue;
            }
            let stem = relative_stem(&lp.name, &tex.path);
            if let Err(e) = names.check(&stem, &tex.path) {
                stats.failed += 1;
                eprintln!("asamu-import: {} ({}): {e:#}", tex.path, lp.name);
                continue;
            }
            match convert(&lp, &tex, &decoder, &caches, &root, file, &args) {
                Ok(Some((entry, wrote))) => {
                    names.claim(&stem, &tex.path);
                    if wrote {
                        stats.written += 1;
                    } else {
                        stats.kept += 1;
                    }
                    record_entry(&mut manifest, &tex.path, entry);
                }
                Ok(None) => stats.skipped += 1,
                Err(e) => {
                    stats.failed += 1;
                    eprintln!("asamu-import: {} ({}): {e:#}", tex.path, lp.name);
                }
            }
        }
    }
    manifest.version = MANIFEST_VERSION;
    manifest.notice = NOTICE.to_owned();
    let json = serde_json::to_string_pretty(&manifest)?;
    if args.dry_run {
        println!("dry run: nothing written ({} manifest bytes)", json.len());
    } else {
        let target = safety::check_output_path(&manifest_path, &cooked, true)?;
        safety::write_output(&target, json.as_bytes(), true)?;
    }
    println!(
        "textures: {} {}, {} kept (already present), {} duplicates of an earlier package \
         ({} with a different format, size or mip count), {} without stored texels, {} failed; \
         manifest: {} entries at {}",
        stats.written,
        if args.dry_run {
            "converted (dry run)"
        } else {
            "written"
        },
        stats.kept,
        stats.duplicates,
        stats.variant_duplicates,
        stats.skipped,
        stats.failed,
        manifest.textures.len(),
        manifest_path.display()
    );
    println!("note: converted data is copyrighted game data; keep it local, never redistribute");
    if stats.failed > 0 {
        bail!("{} textures failed to convert", stats.failed);
    }
    Ok(())
}

/// Insert or replace the manifest entry of `path`. A rerun replaces the
/// entry but keeps the other packages recorded for it by earlier runs.
fn record_entry(manifest: &mut Manifest, path: &str, mut entry: ManifestEntry) {
    if let Some(old) = manifest.textures.get(path) {
        for p in &old.also_in {
            if !entry.also_in.contains(p) && !p.eq_ignore_ascii_case(&entry.package) {
                entry.also_in.push(p.clone());
            }
        }
    }
    manifest.textures.insert(path.to_owned(), entry);
}

/// Output file stems claimed during one run. Different object paths can
/// sanitize to the same stem (`A B` and `A_B`), or differ only in case on a
/// case-insensitive file system; the second one is refused instead of
/// silently sharing (or, with `--force`, overwriting) the first one's files.
#[derive(Debug, Default)]
struct OutputNames {
    /// Lower-case stem -> the object path that claimed it.
    claimed: HashMap<String, String>,
}

impl OutputNames {
    fn key(stem: &Path) -> String {
        rel_string(stem).to_ascii_lowercase()
    }

    /// Ok when `stem` is free or already claimed by `path` itself.
    fn check(&self, stem: &Path, path: &str) -> Result<()> {
        match self.claimed.get(&Self::key(stem)) {
            Some(owner) if owner != path => bail!(
                "output name {} is already used by {owner}",
                rel_string(stem)
            ),
            _ => Ok(()),
        }
    }

    fn claim(&mut self, stem: &Path, path: &str) {
        self.claimed.insert(Self::key(stem), path.to_owned());
    }
}

#[derive(Debug, Default)]
struct RunStats {
    written: usize,
    kept: usize,
    duplicates: usize,
    variant_duplicates: usize,
    skipped: usize,
    failed: usize,
}

/// Does `tex` have the format, size and number of stored mips recorded in
/// `entry` (for the object path cooked into another package)?
fn same_header(entry: &ManifestEntry, tex: &Texture) -> bool {
    if entry.cube || tex.class == TextureClass::TextureCube {
        return entry.cube && tex.class == TextureClass::TextureCube;
    }
    let size = [
        u32::try_from(tex.props.size_x.unwrap_or(0)).unwrap_or(0),
        u32::try_from(tex.props.size_y.unwrap_or(0)).unwrap_or(0),
    ];
    let stored = tex
        .first_stored_mip()
        .map_or(0, |first| tex.mips().len().saturating_sub(first));
    tex.format.map(PixelFormat::enum_name) == Some(entry.format.as_str())
        && size == entry.size
        && u32::try_from(stored).ok() == Some(entry.mips)
}

/// `CookedMac` (or the equivalent), its `Maps` folder, and the install root.
fn cooked_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.maps_dir, install.root))
}

/// Package files of `dirs` (cooked folder first, then maps), filtered.
fn package_files(dirs: &[PathBuf], filters: &[String]) -> Result<Vec<PathBuf>> {
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
    Ok(out)
}

/// Refuse `dir` (an existing, canonical directory) when it lies inside the
/// game install. The shared safety rules recognise an install only by an
/// `.app` bundle or a `steamapps` folder among the ancestors; an install
/// given with `--original` / `ASAMU_ORIGINAL_DIR` can live anywhere.
fn refuse_install(dir: &Path, install_root: Option<&Path>) -> Result<()> {
    let Some(root) = install_root else {
        return Ok(());
    };
    let Ok(root) = root.canonicalize() else {
        return Ok(());
    };
    if dir.starts_with(&root) {
        bail!(
            "refusing to write inside the game install {} (choose an output directory outside it)",
            root.display()
        );
    }
    Ok(())
}

/// Validate `<out>/textures` with the safety rules before creating anything,
/// then create it.
fn prepare_out_dir(
    out: &Path,
    input: Option<&Path>,
    install_root: Option<&Path>,
) -> Result<PathBuf> {
    let root = out.join("textures");
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
    // The probe name never exists; the check covers the repository and
    // install rules for the deepest existing ancestor.
    safety::check_output_path(&existing.join(".asamu-import-textures-probe"), input, false)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    let existing = existing.canonicalize()?;
    refuse_install(&existing, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    refuse_install(&root, install_root)
        .with_context(|| format!("refusing output directory {}", out.display()))?;
    safety::check_output_path(&root.join("manifest.json"), input, true)
        .with_context(|| format!("refusing output directory {}", root.display()))?;
    Ok(root)
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a texture manifest", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Windows device names, which name a device rather than a file in any
/// folder and with any extension.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A file-system-safe path component: ASCII letters, digits, `-` and `_`
/// only (so never empty, `.`, `..` or a separator), and never a Windows
/// device name.
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
        "_".to_owned()
    } else if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(&s)) {
        format!("_{s}")
    } else {
        s
    }
}

/// Relative output path (without extension) for `object_path` of `package`.
fn relative_stem(package: &str, object_path: &str) -> PathBuf {
    let mut p = PathBuf::from(sanitize(package));
    for part in object_path.split('.') {
        p.push(sanitize(part));
    }
    p
}

/// Largest written mip's width and height, and the mips (largest first).
type MipChain = (u32, u32, Vec<Vec<u8>>);

/// The stored mips of `tex` from the first stored one, loaded and checked
/// against the format math for their natural size. Returns (width, height,
/// mips).
fn load_chain(
    tex: &Texture,
    payload: &[u8],
    caches: &TextureFileCaches,
) -> Result<Option<MipChain>> {
    let Some(format) = tex.format else {
        bail!("unknown pixel format {:?}", tex.props.format);
    };
    let Some(first) = tex.first_stored_mip() else {
        return Ok(None);
    };
    let mips = tex.mips();
    let mip0 = mips.first().context("texture without mips")?;
    // Stored sizes of small block-compressed mips are clamped to 4: the
    // natural size of every mip follows from mip 0 and the mip index.
    let (w, h) = mip_dims(
        u32::try_from(mip0.size_x).context("negative mip width")?,
        u32::try_from(mip0.size_y).context("negative mip height")?,
        u32::try_from(first)?,
    );
    let mut out = Vec::new();
    for (k, level) in (first..mips.len()).enumerate() {
        let bytes = tex.load_mip(payload, Some(caches), level)?;
        let (mw, mh) = mip_dims(w, h, u32::try_from(k)?);
        let need = format
            .mip_bytes(mw, mh)
            .with_context(|| format!("no texel layout for {}", format.enum_name()))?;
        if bytes.len() as u64 != need {
            bail!(
                "mip {level}: {} bytes for a {mw}x{mh} {} mip, expected {need}",
                bytes.len(),
                format.enum_name()
            );
        }
        out.push(bytes);
    }
    Ok(Some((w, h, out)))
}

fn convert(
    lp: &LoadedPackage,
    tex: &Texture,
    decoder: &TextureDecoder<'_>,
    caches: &TextureFileCaches,
    root: &Path,
    input: &Path,
    args: &Args,
) -> Result<Option<(ManifestEntry, bool)>> {
    let payload = lp.package.export_data(tex.export_index)?;
    let Some(format) = tex.format else {
        bail!("unknown pixel format {:?}", tex.props.format);
    };
    let (cube, w, h, faces, format) = match &tex.native {
        TextureNative::Texture2D(_) => {
            let Some((w, h, mips)) = load_chain(tex, payload, caches)? else {
                return Ok(None);
            };
            (false, w, h, vec![mips], format)
        }
        TextureNative::SourceArtOnly { .. } if tex.class == TextureClass::TextureCube => {
            match cube_faces(lp, tex, decoder, caches)? {
                Some(c) => (true, c.w, c.h, c.faces, c.format),
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let mip_count = faces.first().map_or(0, Vec::len);
    let dds = dds::encode(format, w, h, cube, &faces)?;
    let stem = relative_stem(&lp.name, &tex.path);
    let dds_rel = stem.with_extension("dds");
    let wrote = write_file(root, &dds_rel, &dds, input, args)?;
    let mut png_rel = None;
    if args.png
        && let Some(face) = faces.first()
    {
        let (k, (pw, ph)) = preview_level(w, h, face.len(), args.png_max);
        if let Some(texels) = face.get(k) {
            let rgba = decode_to_rgba8(format, pw, ph, texels)?;
            let png = png::encode_rgba(pw, ph, &rgba)?;
            let rel = stem.with_extension("png");
            write_file(root, &rel, &png, input, args)?;
            png_rel = Some(rel_string(&rel));
        }
    }
    let p = &tex.props;
    let size = [
        u32::try_from(p.size_x.unwrap_or(0)).unwrap_or(0),
        u32::try_from(p.size_y.unwrap_or(0)).unwrap_or(0),
    ];
    let entry = ManifestEntry {
        package: lp.name.clone(),
        class: tex.class.name().to_owned(),
        file: rel_string(&dds_rel),
        png: png_rel,
        format: format.enum_name().to_owned(),
        size: if cube { [w, h] } else { size },
        written_size: [w, h],
        mips: u32::try_from(mip_count)?,
        cube,
        srgb: if tex.class == TextureClass::LightMapTexture2D {
            tex.tagged.srgb
        } else {
            p.srgb
        },
        address: [
            p.address_x.clone().unwrap_or_else(|| "TA_Wrap".to_owned()),
            p.address_y.clone().unwrap_or_else(|| "TA_Wrap".to_owned()),
        ],
        filter: p.filter.clone(),
        lod_group: p.lod_group.clone(),
        compression_settings: p.compression_settings.clone(),
        lightmap_flags: tex.texture2d().and_then(|t| t.lightmap_flags),
        shadowmap_flags: p.shadowmap_flags,
        also_in: Vec::new(),
    };
    Ok(Some((entry, wrote)))
}

/// Mip index and size used for a preview no larger than `max` (0 = largest).
fn preview_level(w: u32, h: u32, mips: usize, max: u32) -> (usize, (u32, u32)) {
    let mut k = 0usize;
    while max > 0 && k + 1 < mips {
        let (mw, mh) = mip_dims(w, h, u32::try_from(k).unwrap_or(u32::MAX));
        if mw.max(mh) <= max {
            break;
        }
        k += 1;
    }
    (k, mip_dims(w, h, u32::try_from(k).unwrap_or(u32::MAX)))
}

/// A cube map's faces in DDS order (+X, -X, +Y, -Y, +Z, -Z), each a full
/// mip chain, with their common format and largest mip size.
struct CubeFaces {
    w: u32,
    h: u32,
    format: PixelFormat,
    faces: Vec<Vec<Vec<u8>>>,
}

/// Load the faces of a cube map; all faces must agree on format, size and
/// mip count.
fn cube_faces(
    lp: &LoadedPackage,
    tex: &Texture,
    decoder: &TextureDecoder<'_>,
    caches: &TextureFileCaches,
) -> Result<Option<CubeFaces>> {
    let mut faces = Vec::with_capacity(6);
    let mut shape: Option<(u32, u32, usize, PixelFormat)> = None;
    for face in &tex.props.faces {
        let Some(path) = face else {
            return Ok(None);
        };
        let index = lp
            .export_by_qualified(path)
            .with_context(|| format!("cube face {path} is not in {}", lp.name))?;
        let face_tex = decoder.decode(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let Some((w, h, mips)) = load_chain(&face_tex, payload, caches)? else {
            return Ok(None);
        };
        let format = face_tex
            .format
            .context("cube face without a pixel format")?;
        let this = (w, h, mips.len(), format);
        if shape.is_some_and(|s| s != this) {
            bail!("cube faces differ in format, size or mip count");
        }
        shape = Some(this);
        faces.push(mips);
    }
    Ok(shape.map(|(w, h, _, format)| CubeFaces {
        w,
        h,
        format,
        faces,
    }))
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
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
/// the file exists and `--force` is off (it is kept). Writes nothing with
/// `--dry-run`.
fn write_file(root: &Path, rel: &Path, data: &[u8], input: &Path, args: &Args) -> Result<bool> {
    if args.dry_run {
        return Ok(true);
    }
    let force = args.force;
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

/// `--check`: decode everything, load every mip, print the report.
fn check(
    set: &PackageSet,
    decoder: &TextureDecoder<'_>,
    caches: &TextureFileCaches,
    files: &[PathBuf],
    json: bool,
) -> Result<()> {
    let mut cov = TextureCoverage::default();
    for file in files {
        let lp = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        cov.add_package(decoder, &lp, caches, true);
    }
    cov.finish(caches);
    if json {
        println!("{}", serde_json::to_string_pretty(&cov)?);
    } else {
        print_report(&cov);
    }
    if cov.failure_count > 0 {
        bail!("{} texture problems found", cov.failure_count);
    }
    Ok(())
}

fn print_report(cov: &TextureCoverage) {
    println!("texture classes (exports, class default objects, exact, failed):");
    for (k, c) in &cov.classes {
        println!(
            "  {k:<26} {:>6} {:>4} {:>6} {:>4}",
            c.total, c.default_objects, c.exact, c.failed
        );
    }
    println!(
        "pixel formats (textures, mips: inline/tfc/unused/lzo, size-ok, loaded-ok, texel bytes):"
    );
    for (k, f) in &cov.formats {
        println!(
            "  {k:<14} {:>5} {:>6}: {:>6}/{:>6}/{:>4}/{:>6} {:>6} {:>6} {:>12}",
            f.textures,
            f.mips,
            f.inline_mips,
            f.tfc_mips,
            f.unused_mips,
            f.lzo_mips,
            f.size_ok,
            f.loaded_ok,
            f.texel_bytes
        );
    }
    println!(
        "texture file caches (records, distinct ranges, uncovered bytes, gap payloads, out of range):"
    );
    for (k, f) in &cov.file_caches {
        println!(
            "  {k:<14} {:>6} {:>6} {:>9} {:>4} {:>3}",
            f.records, f.distinct_ranges, f.uncovered_bytes, f.gap_payloads, f.out_of_range
        );
    }
    println!("per package (texture exports, problems):");
    for p in &cov.packages {
        let total: usize = p.classes.values().map(|c| c.total).sum();
        if total > 0 {
            println!("  {:<32} {:>6} {:>4}", p.package, total, p.failure_count);
        }
        for f in &p.failures {
            println!("    ! {f}");
        }
    }
    println!(
        "mip sizes: {} natural, {} clamped to the 4x4 block, {} other; inline offsets: {} ok, {} bad",
        cov.mip_dims_natural,
        cov.mip_dims_block_clamped,
        cov.mip_dims_other,
        cov.inline_offset_ok,
        cov.inline_offset_bad
    );
    println!("problems: {}", cov.failure_count);
}

/// Minimal DDS writer (legacy header, plus the DX10 extension for formats
/// that have no legacy description).
pub mod dds {
    use anyhow::{Result, bail};
    use asamu_ue3::texture::{PixelFormat, mip_dims};

    const DDSD_CAPS: u32 = 0x1;
    const DDSD_HEIGHT: u32 = 0x2;
    const DDSD_WIDTH: u32 = 0x4;
    const DDSD_PITCH: u32 = 0x8;
    const DDSD_PIXELFORMAT: u32 = 0x1000;
    const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
    const DDSD_LINEARSIZE: u32 = 0x8_0000;
    const DDPF_ALPHAPIXELS: u32 = 0x1;
    const DDPF_FOURCC: u32 = 0x4;
    const DDPF_RGB: u32 = 0x40;
    const DDPF_LUMINANCE: u32 = 0x2_0000;
    const DDSCAPS_COMPLEX: u32 = 0x8;
    const DDSCAPS_TEXTURE: u32 = 0x1000;
    const DDSCAPS_MIPMAP: u32 = 0x40_0000;
    const DDSCAPS2_CUBEMAP_ALL_FACES: u32 = 0x200 | 0xFC00;
    const DXGI_FORMAT_R8G8_SNORM: u32 = 51;
    const DXGI_FORMAT_R16_UNORM: u32 = 56;
    const DXGI_FORMAT_BC5_UNORM: u32 = 83;
    const D3D10_RESOURCE_DIMENSION_TEXTURE2D: u32 = 3;
    const D3D10_RESOURCE_MISC_TEXTURECUBE: u32 = 0x4;

    fn fourcc(s: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*s)
    }

    /// Pixel-format block: flags, FourCC, bit count, R/G/B/A masks, and the
    /// DXGI format when the DX10 extension is needed.
    fn pixel_format(format: PixelFormat) -> Result<([u32; 7], Option<u32>)> {
        Ok(match format {
            PixelFormat::Dxt1 => ([DDPF_FOURCC, fourcc(b"DXT1"), 0, 0, 0, 0, 0], None),
            PixelFormat::Dxt3 => ([DDPF_FOURCC, fourcc(b"DXT3"), 0, 0, 0, 0, 0], None),
            PixelFormat::Dxt5 => ([DDPF_FOURCC, fourcc(b"DXT5"), 0, 0, 0, 0, 0], None),
            PixelFormat::A8R8G8B8 => (
                [
                    DDPF_RGB | DDPF_ALPHAPIXELS,
                    0,
                    32,
                    0x00FF_0000,
                    0x0000_FF00,
                    0x0000_00FF,
                    0xFF00_0000,
                ],
                None,
            ),
            PixelFormat::G8 => ([DDPF_LUMINANCE, 0, 8, 0xFF, 0, 0, 0], None),
            PixelFormat::Bc5 => (
                [DDPF_FOURCC, fourcc(b"DX10"), 0, 0, 0, 0, 0],
                Some(DXGI_FORMAT_BC5_UNORM),
            ),
            PixelFormat::V8U8 => (
                [DDPF_FOURCC, fourcc(b"DX10"), 0, 0, 0, 0, 0],
                Some(DXGI_FORMAT_R8G8_SNORM),
            ),
            PixelFormat::G16 => (
                [DDPF_FOURCC, fourcc(b"DX10"), 0, 0, 0, 0, 0],
                Some(DXGI_FORMAT_R16_UNORM),
            ),
            other => bail!("no DDS mapping for {}", other.enum_name()),
        })
    }

    /// Encode a 2D texture (`faces.len() == 1`) or a cube map (6 faces) whose
    /// largest mip is `w` x `h`. Every face holds the same number of mips,
    /// largest first, each exactly the format's size for its natural size.
    pub fn encode(
        format: PixelFormat,
        w: u32,
        h: u32,
        cube: bool,
        faces: &[Vec<Vec<u8>>],
    ) -> Result<Vec<u8>> {
        if faces.len() != if cube { 6 } else { 1 } {
            bail!(
                "{} faces for a {} texture",
                faces.len(),
                if cube { "cube" } else { "2D" }
            );
        }
        let mips = faces.first().map_or(0, Vec::len);
        if mips == 0 || faces.iter().any(|f| f.len() != mips) {
            bail!("faces need the same, non-zero number of mips");
        }
        if w == 0 || h == 0 {
            bail!("empty texture");
        }
        let (pf, dxgi) = pixel_format(format)?;
        let Some(layout) = format.layout() else {
            bail!("no texel layout for {}", format.enum_name());
        };
        for face in faces {
            for (k, data) in face.iter().enumerate() {
                let (mw, mh) = mip_dims(w, h, u32::try_from(k)?);
                let need = format.mip_bytes(mw, mh).unwrap_or(u64::MAX);
                if data.len() as u64 != need {
                    bail!(
                        "mip {k}: {} bytes, a {mw}x{mh} mip needs {need}",
                        data.len()
                    );
                }
            }
        }
        let compressed = format.is_block_compressed();
        let top = format.mip_bytes(w, h).unwrap_or(0);
        let pitch_or_linear = if compressed {
            u32::try_from(top)?
        } else {
            w.checked_mul(layout.block_bytes).unwrap_or(0)
        };
        let mut flags = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT;
        flags |= if compressed {
            DDSD_LINEARSIZE
        } else {
            DDSD_PITCH
        };
        let mut caps = DDSCAPS_TEXTURE;
        if mips > 1 {
            flags |= DDSD_MIPMAPCOUNT;
            caps |= DDSCAPS_COMPLEX | DDSCAPS_MIPMAP;
        }
        if cube {
            caps |= DDSCAPS_COMPLEX;
        }
        let total: usize = faces.iter().flatten().map(Vec::len).sum();
        let mut out = Vec::with_capacity(148 + total);
        let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
        put(fourcc(b"DDS "));
        put(124);
        put(flags);
        put(h);
        put(w);
        put(pitch_or_linear);
        put(0); // depth
        put(u32::try_from(mips)?);
        for _ in 0..11 {
            put(0);
        }
        put(32);
        for v in pf {
            put(v);
        }
        put(caps);
        put(if cube { DDSCAPS2_CUBEMAP_ALL_FACES } else { 0 });
        put(0);
        put(0);
        put(0);
        if let Some(dxgi) = dxgi {
            put(dxgi);
            put(D3D10_RESOURCE_DIMENSION_TEXTURE2D);
            put(if cube {
                D3D10_RESOURCE_MISC_TEXTURECUBE
            } else {
                0
            });
            put(1); // array size (a cube counts as one)
            put(0);
        }
        for face in faces {
            for data in face {
                out.extend_from_slice(data);
            }
        }
        Ok(out)
    }
}

/// Minimal PNG writer: 8-bit RGBA, no filtering, zlib stream of stored
/// (uncompressed) deflate blocks.
pub mod png {
    use anyhow::{Result, bail};

    /// CRC-32 (IEEE 802.3, reflected, as PNG requires).
    pub fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    /// Adler-32 (zlib trailer).
    pub fn adler32(data: &[u8]) -> u32 {
        const MOD: u32 = 65_521;
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            for &x in chunk {
                a += u32::from(x);
                b += a;
            }
            a %= MOD;
            b %= MOD;
        }
        (b << 16) | a
    }

    /// zlib stream of stored deflate blocks.
    pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 16);
        out.extend_from_slice(&[0x78, 0x01]);
        let mut blocks = data.chunks(65_535).peekable();
        if blocks.peek().is_none() {
            out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
        }
        while let Some(block) = blocks.next() {
            let last = blocks.peek().is_none();
            out.push(u8::from(last));
            let len = block.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
        }
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<()> {
        out.extend_from_slice(&u32::try_from(data.len())?.to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
        Ok(())
    }

    /// Encode `w` x `h` RGBA8 texels (row-major, top row first).
    pub fn encode_rgba(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
        let row = usize::try_from(w)?.saturating_mul(4);
        let rows = usize::try_from(h)?;
        if w == 0 || h == 0 || row.checked_mul(rows) != Some(rgba.len()) {
            bail!("{} bytes are not a {w}x{h} RGBA image", rgba.len());
        }
        let mut raw = Vec::with_capacity((row + 1) * rows);
        for line in rgba.chunks_exact(row) {
            raw.push(0); // filter: none
            raw.extend_from_slice(line);
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr)?;
        chunk(&mut out, b"IDAT", &zlib_stored(&raw))?;
        chunk(&mut out, b"IEND", &[])?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use asamu_ue3::bulkdata::BulkDataRecord;
    use asamu_ue3::texture::{MipMap, Texture2DNative, TextureProps};
    use asamu_ue3::types::Guid;

    #[test]
    fn crc_and_adler_known_answers() {
        assert_eq!(png::crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(png::crc32(b""), 0);
        assert_eq!(png::adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(png::adler32(b""), 1);
    }

    #[test]
    fn zlib_stored_blocks() {
        let empty = png::zlib_stored(&[]);
        assert_eq!(empty, vec![0x78, 0x01, 1, 0, 0, 0xFF, 0xFF, 0, 0, 0, 1]);
        let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let z = png::zlib_stored(&data);
        // Two blocks: 65,535 + 4,465 bytes.
        assert_eq!(z[2], 0);
        assert_eq!(u16::from_le_bytes([z[3], z[4]]), 65_535);
        let second = 2 + 5 + 65_535;
        assert_eq!(z[second], 1);
        assert_eq!(u16::from_le_bytes([z[second + 1], z[second + 2]]), 4_465);
        assert_eq!(z.len(), 2 + 5 + 65_535 + 5 + 4_465 + 4);
        assert_eq!(&z[z.len() - 4..], &png::adler32(&data).to_be_bytes());
    }

    #[test]
    fn png_structure() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 128];
        let p = png::encode_rgba(2, 1, &rgba).unwrap();
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[8..16], &[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        assert_eq!(&p[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
        assert_eq!(&p[24..29], &[8, 6, 0, 0, 0]);
        let crc = u32::from_be_bytes(p[29..33].try_into().unwrap());
        assert_eq!(crc, png::crc32(&p[12..29]));
        assert_eq!(
            &p[p.len() - 12..],
            &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
        );
        assert!(png::encode_rgba(2, 2, &rgba).is_err());
        assert!(png::encode_rgba(0, 1, &[]).is_err());
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn dds_dxt1_header() {
        // 8x8 DXT1 with three mips: 32 + 8 + 8 bytes.
        let mips = vec![vec![1u8; 32], vec![2u8; 8], vec![3u8; 8]];
        let d = dds::encode(PixelFormat::Dxt1, 8, 8, false, &[mips]).unwrap();
        assert_eq!(&d[..4], b"DDS ");
        assert_eq!(u32_at(&d, 4), 124);
        assert_eq!(
            u32_at(&d, 8),
            0x1 | 0x2 | 0x4 | 0x1000 | 0x2_0000 | 0x8_0000
        );
        assert_eq!(u32_at(&d, 12), 8); // height
        assert_eq!(u32_at(&d, 16), 8); // width
        assert_eq!(u32_at(&d, 20), 32); // linear size of mip 0
        assert_eq!(u32_at(&d, 28), 3); // mip count
        assert_eq!(u32_at(&d, 76), 32); // pixel format size
        assert_eq!(u32_at(&d, 80), 0x4); // FourCC
        assert_eq!(&d[84..88], b"DXT1");
        assert_eq!(u32_at(&d, 108), 0x1000 | 0x8 | 0x40_0000);
        assert_eq!(d.len(), 128 + 48);
        assert_eq!(d[128], 1);
        assert_eq!(d[128 + 32], 2);
        // Wrong mip size is refused.
        assert!(dds::encode(PixelFormat::Dxt1, 8, 8, false, &[vec![vec![0u8; 31]]]).is_err());
    }

    #[test]
    fn dds_uncompressed_and_dx10() {
        let d = dds::encode(PixelFormat::A8R8G8B8, 2, 1, false, &[vec![vec![0u8; 8]]]).unwrap();
        assert_eq!(u32_at(&d, 8) & 0x8, 0x8); // pitch
        assert_eq!(u32_at(&d, 20), 8);
        assert_eq!(u32_at(&d, 80), 0x41);
        assert_eq!(u32_at(&d, 88), 32);
        assert_eq!(u32_at(&d, 92), 0x00FF_0000);
        assert_eq!(u32_at(&d, 104), 0xFF00_0000);
        assert_eq!(d.len(), 128 + 8);

        let bc5 = dds::encode(PixelFormat::Bc5, 4, 4, false, &[vec![vec![0u8; 16]]]).unwrap();
        assert_eq!(&bc5[84..88], b"DX10");
        assert_eq!(u32_at(&bc5, 128), 83);
        assert_eq!(u32_at(&bc5, 132), 3);
        assert_eq!(bc5.len(), 148 + 16);

        let faces: Vec<Vec<Vec<u8>>> = (0..6u8).map(|f| vec![vec![f; 16]]).collect();
        let cube = dds::encode(PixelFormat::Dxt5, 4, 4, true, &faces).unwrap();
        assert_eq!(u32_at(&cube, 112), 0xFE00);
        assert_eq!(cube.len(), 128 + 96);
        assert_eq!(cube[128 + 5 * 16], 5);
        assert!(dds::encode(PixelFormat::Dxt5, 4, 4, true, &faces[..5]).is_err());
        assert!(dds::encode(PixelFormat::Unknown, 4, 4, false, &[vec![vec![0u8; 16]]]).is_err());
    }

    #[test]
    fn output_paths_are_sanitized() {
        let p = relative_stem("AG-Darkcave", "Foo.Bar baz.../x");
        assert_eq!(rel_string(&p), "AG-Darkcave/Foo/Bar_baz/_/_/_x");
        assert!(!rel_string(&relative_stem("..", "..")).contains(".."));
    }

    #[test]
    fn preview_level_picks_fitting_mip() {
        assert_eq!(preview_level(2048, 1024, 12, 512), (2, (512, 256)));
        assert_eq!(preview_level(256, 256, 9, 512), (0, (256, 256)));
        assert_eq!(preview_level(2048, 2048, 1, 512), (0, (2048, 2048)));
        assert_eq!(preview_level(2048, 2048, 12, 0), (0, (2048, 2048)));
    }

    fn record(flags: u32, count: i32, size: i32, header_offset: usize) -> BulkDataRecord {
        BulkDataRecord {
            flags,
            element_count: count,
            size_on_disk: size,
            offset_in_file: -1,
            header_offset,
        }
    }

    fn mip(data: BulkDataRecord, size_x: i32, size_y: i32) -> MipMap {
        MipMap {
            data,
            size_x,
            size_y,
        }
    }

    /// An 8x8 DXT1 texture whose two top mips were stripped: the first stored
    /// mip is 2x2 but recorded as 4x4 (block clamping), so its natural size
    /// must come from mip 0, not from the stored fields.
    fn stripped_dxt1() -> (Texture, Vec<u8>) {
        let mut payload = vec![0u8; 16];
        payload.extend_from_slice(&[0x11; 8]);
        payload.extend_from_slice(&[0u8; 16]);
        payload.extend_from_slice(&[0x22; 8]);
        let unused = record(0x21, 0, -1, 0);
        let mips = vec![
            mip(unused, 8, 8),
            mip(unused, 4, 4),
            mip(record(0, 8, 8, 0), 4, 4),
            mip(record(0, 8, 8, 24), 4, 4),
        ];
        let props = TextureProps {
            size_x: Some(8),
            size_y: Some(8),
            format: Some("PF_DXT1".to_owned()),
            ..TextureProps::default()
        };
        let tex = Texture {
            export_index: 0,
            path: "Pkg.T".to_owned(),
            class_path: "Engine.Texture2D".to_owned(),
            class: TextureClass::Texture2D,
            is_default_object: false,
            props: props.clone(),
            tagged: props,
            format: Some(PixelFormat::Dxt1),
            native: TextureNative::Texture2D(Texture2DNative {
                source_art: record(0, 0, 0, 0),
                mips,
                file_cache_guid: Guid::default(),
                cached_pvrtc_mips: Vec::new(),
                cached_flash_mips_max_resolution: 0,
                cached_atitc_mips: Vec::new(),
                cached_flash_mips: unused,
                cached_etc_mips: Vec::new(),
                lightmap_flags: None,
            }),
            serial_offset: 0,
            properties_end: 0,
        };
        (tex, payload)
    }

    #[test]
    fn natural_size_comes_from_mip0() {
        let (tex, payload) = stripped_dxt1();
        let (w, h, mips) = load_chain(&tex, &payload, &TextureFileCaches::default())
            .unwrap()
            .unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(mips, vec![vec![0x11; 8], vec![0x22; 8]]);
        let d = dds::encode(PixelFormat::Dxt1, w, h, false, &[mips]).unwrap();
        assert_eq!((u32_at(&d, 12), u32_at(&d, 16), u32_at(&d, 28)), (2, 2, 2));
        // A stored mip whose bytes do not fit its natural size is refused.
        let (mut bad, payload) = stripped_dxt1();
        if let TextureNative::Texture2D(t) = &mut bad.native {
            // Mip 0 recorded as 16x64 makes the first stored mip 4x16
            // (32 bytes), which its 8 stored bytes cannot be.
            t.mips[0].size_x = 16;
            t.mips[0].size_y = 64;
        }
        assert!(load_chain(&bad, &payload, &TextureFileCaches::default()).is_err());
    }

    #[test]
    fn reserved_and_colliding_output_names() {
        for (name, want) in [
            ("CON", "_CON"),
            ("nul", "_nul"),
            ("Com1", "_Com1"),
            ("LPT9", "_LPT9"),
            ("CONX", "CONX"),
            ("", "_"),
            ("..", "__"),
        ] {
            assert_eq!(sanitize(name), want);
        }
        let mut names = OutputNames::default();
        let a = relative_stem("Pkg", "Group.A B");
        let b = relative_stem("Pkg", "Group.A_B");
        let c = relative_stem("Pkg", "group.a_b");
        names.check(&a, "Group.A B").unwrap();
        names.claim(&a, "Group.A B");
        // The same object again is fine; another one with the same file name
        // (after sanitizing, or on a case-insensitive file system) is not.
        assert!(names.check(&a, "Group.A B").is_ok());
        assert!(names.check(&b, "Group.A_B").is_err());
        assert!(names.check(&c, "group.a_b").is_err());
        assert!(
            names
                .check(&relative_stem("Other", "Group.A_B"), "Group.A_B")
                .is_ok()
        );
    }

    fn entry(package: &str, also_in: &[&str]) -> ManifestEntry {
        ManifestEntry {
            package: package.to_owned(),
            class: "Texture2D".to_owned(),
            file: "x.dds".to_owned(),
            png: None,
            format: "PF_DXT1".to_owned(),
            size: [4, 4],
            written_size: [4, 4],
            mips: 1,
            cube: false,
            srgb: None,
            address: ["TA_Wrap".to_owned(), "TA_Wrap".to_owned()],
            filter: None,
            lod_group: None,
            compression_settings: None,
            lightmap_flags: None,
            shadowmap_flags: None,
            also_in: also_in.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn reruns_keep_recorded_copies() {
        let mut m = Manifest::default();
        m.textures.insert("P.T".to_owned(), entry("A", &["B", "C"]));
        record_entry(&mut m, "P.T", entry("A", &[]));
        assert_eq!(m.textures["P.T"].also_in, vec!["B", "C"]);
        // A new first package never lists itself as a copy.
        record_entry(&mut m, "P.T", entry("B", &[]));
        assert_eq!(m.textures["P.T"].also_in, vec!["C"]);
        record_entry(&mut m, "Q.T", entry("A", &[]));
        assert!(m.textures["Q.T"].also_in.is_empty());
    }

    /// An install given with --original need not sit in a `steamapps` tree
    /// or carry an `.app` bundle: writing inside it is refused anyway, and
    /// nothing is created.
    #[test]
    fn refuses_output_inside_any_install() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("Games").join("Uncle");
        std::fs::create_dir_all(install.join("ASAMU").join("CookedPC")).unwrap();
        let inside = install.join("ASAMU").join("converted");
        assert!(prepare_out_dir(&inside, None, Some(&install)).is_err());
        assert!(!inside.exists());
        assert!(prepare_out_dir(&install, None, Some(&install)).is_err());
        assert!(!install.join("textures").exists());
        let outside = tmp.path().join("out");
        let root = prepare_out_dir(&outside, None, Some(&install)).unwrap();
        assert!(root.ends_with("textures"));
        assert!(root.is_dir());
    }

    /// An output path that reaches the install through a link is refused too.
    #[cfg(unix)]
    #[test]
    fn refuses_output_linked_into_an_install() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("Uncle");
        std::fs::create_dir_all(&install).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&install, &link).unwrap();
        assert!(prepare_out_dir(&link, None, Some(&install)).is_err());
        assert_eq!(std::fs::read_dir(&install).unwrap().count(), 0);
    }

    /// Links inside the output tree are never followed: a directory link (to
    /// a game install or anywhere else) is refused before anything is created
    /// behind it, and a file link is kept without `--force` and refused with
    /// it.
    #[cfg(unix)]
    #[test]
    fn links_inside_the_output_tree_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = prepare_out_dir(tmp.path(), None, None).unwrap();
        let input = tmp.path().join("in.u");
        let mut args = Args {
            packages: Vec::new(),
            name: None,
            limit: None,
            png: false,
            png_max: 512,
            skip_lighting: false,
            force: false,
            check: false,
            json: false,
            dry_run: false,
        };
        let install = tmp.path().join("Game.app").join("Contents");
        std::fs::create_dir_all(&install).unwrap();
        std::os::unix::fs::symlink(&install, root.join("Engine")).unwrap();
        let rel = Path::new("Engine").join("Sub").join("T.dds");
        assert!(write_file(&root, &rel, b"x", &input, &args).is_err());
        args.force = true;
        assert!(write_file(&root, &rel, b"x", &input, &args).is_err());
        assert_eq!(std::fs::read_dir(&install).unwrap().count(), 0);

        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("Other")).unwrap();
        args.force = false;
        let rel = Path::new("Other").join("T.dds");
        assert!(write_file(&root, &rel, b"x", &input, &args).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);

        // A file link at the target: kept (nothing written through it)
        // without --force, refused with it.
        let victim = tmp.path().join("victim.bin");
        std::fs::write(&victim, b"original").unwrap();
        std::fs::create_dir(root.join("P")).unwrap();
        std::os::unix::fs::symlink(&victim, root.join("P").join("T.dds")).unwrap();
        let rel = Path::new("P").join("T.dds");
        assert!(!write_file(&root, &rel, b"new", &input, &args).unwrap());
        args.force = true;
        assert!(write_file(&root, &rel, b"new", &input, &args).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"original");

        // Ordinary writes work; reruns keep or replace.
        args.force = false;
        let rel = Path::new("Q").join("R").join("T.dds");
        assert!(write_file(&root, &rel, b"one", &input, &args).unwrap());
        assert!(!write_file(&root, &rel, b"two", &input, &args).unwrap());
        args.force = true;
        assert!(write_file(&root, &rel, b"two", &input, &args).unwrap());
        assert_eq!(std::fs::read(root.join(&rel)).unwrap(), b"two");
        // --dry-run writes nothing.
        args.dry_run = true;
        let rel = Path::new("S").join("T.dds");
        assert!(write_file(&root, &rel, b"x", &input, &args).unwrap());
        assert!(!root.join("S").exists());
    }

    #[test]
    fn refuses_repo_output() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        assert!(prepare_out_dir(&repo.join("docs"), None, None).is_err());
        assert!(prepare_out_dir(&repo, None, None).is_err());
    }
}
