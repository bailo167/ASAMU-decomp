//! `asamu-import levels`: per-map scene descriptions and BSP geometry.
//!
//! For every map package (or the ones named with `--map`) this writes, into
//! `<out>/levels/` (user-local; never the repository or the game install):
//!
//! - `<map>.scene.json` — [`asamu_ue3::level::Scene`]: every actor of the
//!   persistent level with transform, collision flags, tags, components
//!   (static meshes + material overrides, lights, cylinders, brushes),
//!   gameplay parameters, Matinee references and world-space volume geometry;
//!   `WorldInfo` settings and streaming sub-levels.
//! - `<map>.bsp.json` + `<map>.bsp.bin` — the level BSP as triangle sets
//!   (drawn surfaces and blocking surfaces) in UE3 world space, with a
//!   surface table (material, flags, texture axes). The JSON gives the byte
//!   offset and count of every array in the `.bin` file.
//! - `<map>.bsp.glb` (with `--gltf`) — the same triangles as a glTF 2.0
//!   binary for viewing, converted to glTF axes (`(y, z, -x)`, unscaled UU).
//! - `manifest.json` — per-map counts and streaming relationships.
//!
//! All of this is derived from copyrighted game data: keep it local and do
//! not redistribute it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::bsp::TriangleMesh;
use asamu_ue3::level::{self, BspGeometry, Scene, SceneOptions};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use serde::Serialize;

use crate::safety;

/// `format` of `<map>.bsp.json`.
pub const BSP_FORMAT: &str = "asamu-bsp";
/// `version` of `<map>.bsp.json`.
pub const BSP_VERSION: u32 = 1;
/// `format` of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "asamu-levels-manifest";

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive, e.g. AG-IceCave). Repeat
    /// for several; default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Skip BSP geometry.
    #[arg(long)]
    no_bsp: bool,
    /// Also write `<map>.bsp.glb` (glTF 2.0 binary, glTF axes) for viewing.
    #[arg(long)]
    gltf: bool,
    /// Include effective class parameters for every actor (default: only
    /// for actors that are not plain static meshes, lights or CSG brushes).
    #[arg(long)]
    all_params: bool,
    /// Skip world-space geometry for brushes and volumes.
    #[arg(long)]
    no_volume_geometry: bool,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
}

/// Cooked package folder and root of the install.
fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.root))
}

/// Map package files in `maps_dir`, optionally filtered by stem.
pub fn select_maps(maps_dir: &Path, wanted: &[String]) -> Result<Vec<PathBuf>> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(maps_dir)
        .with_context(|| format!("reading {}", maps_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("asamu"))
        })
        .collect();
    all.sort();
    if wanted.is_empty() {
        return Ok(all);
    }
    let mut out = Vec::new();
    for w in wanted {
        let hit = all.iter().find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(w))
        });
        match hit {
            Some(p) => out.push(p.clone()),
            None => bail!("map {w} not found in {}", maps_dir.display()),
        }
    }
    Ok(out)
}

/// Create `dir` (and missing parents) after validating every new component
/// as an output location (refuses the repository outside its ignored
/// `research/` folders, `.app` bundles, `steamapps` trees and symlinks) and
/// refusing anything inside `install` (the game install root, which for a
/// copy outside Steam has neither an `.app` bundle nor a `steamapps` parent
/// on Windows/Linux layouts).
pub fn prepare_dir(dir: &Path, input: &Path, install: &Path) -> Result<PathBuf> {
    let abs = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let mut existing = abs.clone();
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(ToOwned::to_owned) else {
            bail!("cannot create {}", abs.display());
        };
        missing.push(name);
        if !existing.pop() {
            bail!("cannot create {}", abs.display());
        }
    }
    let mut cur = existing
        .canonicalize()
        .with_context(|| format!("resolving {}", existing.display()))?;
    if !cur.is_dir() {
        bail!("{} is not a directory", cur.display());
    }
    // The install root resolved the same way (symlinks followed); a missing
    // or unreadable root cannot contain anything we create.
    if let Ok(root) = install.canonicalize()
        && is_within(&cur, &root)
    {
        bail!(
            "refusing to write inside the game install {} (choose an output directory \
             outside it)",
            root.display()
        );
    }
    // Validate the existing directory itself as a place to write into.
    safety::check_output_path(&cur.join(".asamu-import-probe"), input, false)?;
    for name in missing.into_iter().rev() {
        let next = safety::check_output_path(&cur.join(&name), input, false)?;
        std::fs::create_dir(&next).with_context(|| format!("creating {}", next.display()))?;
        cur = next;
    }
    Ok(cur)
}

/// True when `path` is `root` or below it, comparing components without
/// regard to ASCII case (macOS and Windows file systems usually ignore case;
/// on a case-sensitive one this only refuses more).
fn is_within(path: &Path, root: &Path) -> bool {
    let mut p = path.components();
    root.components().all(|r| {
        p.next().is_some_and(|c| {
            c.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&r.as_os_str().to_string_lossy())
        })
    })
}

fn write_file(dir: &Path, name: &str, data: &[u8], input: &Path, force: bool) -> Result<PathBuf> {
    let target = safety::check_output_path(&dir.join(name), input, force)?;
    safety::write_output(&target, data, force)?;
    Ok(target)
}

fn to_json<T: Serialize>(v: &T, pretty: bool) -> Result<Vec<u8>> {
    Ok(if pretty {
        serde_json::to_vec_pretty(v)?
    } else {
        serde_json::to_vec(v)?
    })
}

// ------------------------------------------------------------ BSP binary

/// Location of one array in the `.bin` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Byte offset.
    pub offset: usize,
    /// Element count (vec3 / triangle / tag).
    pub count: usize,
}

/// One triangle set in the `.bin` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MeshSpans {
    /// `f32 x, y, z` per vertex.
    pub positions: Span,
    /// `u32 a, b, c` per triangle.
    pub triangles: Span,
    /// `u32` surface index per triangle.
    pub surfaces: Span,
}

/// `<map>.bsp.json`.
#[derive(Debug, Serialize)]
pub struct BspIndex<'a> {
    /// [`BSP_FORMAT`].
    pub format: &'static str,
    /// [`BSP_VERSION`].
    pub version: u32,
    /// Units and axes.
    pub coordinates: &'static str,
    /// Winding rule.
    pub winding: &'static str,
    /// Name of the binary file next to this one.
    pub bin: String,
    /// Model, bounds, counts, consistency check and surfaces.
    #[serde(flatten)]
    pub geometry: BspMeta<'a>,
    /// Arrays of the drawn and blocking triangle sets.
    pub meshes: BTreeMap<&'static str, MeshSpans>,
}

/// The parts of [`BspGeometry`] that go into the JSON.
#[derive(Debug, Serialize)]
pub struct BspMeta<'a> {
    /// Model path.
    pub model: &'a str,
    /// Bounds.
    pub bounds: &'a asamu_ue3::bsp::BoxSphereBounds,
    /// Counts.
    pub counts: &'a level::BspCounts,
    /// Consistency check.
    pub check: &'a asamu_ue3::bsp::ModelCheck,
    /// Surfaces (indexed by the per-triangle surface tags).
    pub surfaces: &'a [level::SurfaceInfo],
}

fn push_mesh(bin: &mut Vec<u8>, m: &TriangleMesh) -> MeshSpans {
    let positions = Span {
        offset: bin.len(),
        count: m.positions.len(),
    };
    for p in &m.positions {
        for c in p {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let triangles = Span {
        offset: bin.len(),
        count: m.indices.len(),
    };
    for t in &m.indices {
        for i in t {
            bin.extend_from_slice(&i.to_le_bytes());
        }
    }
    let surfaces = Span {
        offset: bin.len(),
        count: m.tags.len(),
    };
    for t in &m.tags {
        bin.extend_from_slice(&t.to_le_bytes());
    }
    MeshSpans {
        positions,
        triangles,
        surfaces,
    }
}

/// Serialize BSP geometry as (`.bsp.json`, `.bsp.bin`).
pub fn bsp_files(g: &BspGeometry, bin_name: &str, pretty: bool) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut bin = Vec::new();
    let mut meshes = BTreeMap::new();
    meshes.insert("visible", push_mesh(&mut bin, &g.visible));
    meshes.insert("collision", push_mesh(&mut bin, &g.collision));
    let index = BspIndex {
        format: BSP_FORMAT,
        version: BSP_VERSION,
        coordinates: level::COORDINATES,
        winding: "vertex order a, b, c gives (b - a) x (c - a) (computed on the raw coordinates) \
                  along the surface normal",
        bin: bin_name.to_owned(),
        geometry: BspMeta {
            model: &g.model,
            bounds: &g.bounds,
            counts: &g.counts,
            check: &g.check,
            surfaces: &g.surfaces,
        },
        meshes,
    };
    Ok((to_json(&index, pretty)?, bin))
}

// ------------------------------------------------------------ glTF

/// UE3 → glTF axes (the `asamu-core` Bevy mapping `(y, z, -x)`, unscaled).
pub fn ue_to_gltf(p: [f32; 3]) -> [f32; 3] {
    [p[1], p[2], -p[0]]
}

fn pad4(v: &mut Vec<u8>, byte: u8) {
    while !v.len().is_multiple_of(4) {
        v.push(byte);
    }
}

/// A minimal glTF 2.0 binary with one node per non-empty mesh. Positions are
/// converted with [`ue_to_gltf`]; the mapping flips handedness, so triangle
/// winding is reversed to keep front faces on the surface-normal side.
pub fn glb(meshes: &[(&str, &TriangleMesh)]) -> Result<Vec<u8>> {
    let mut bin: Vec<u8> = Vec::new();
    let mut accessors = Vec::new();
    let mut views = Vec::new();
    let mut gl_meshes = Vec::new();
    let mut nodes = Vec::new();
    for (name, m) in meshes {
        if m.indices.is_empty() || m.positions.is_empty() {
            continue;
        }
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        let pos_off = bin.len();
        for p in &m.positions {
            let g = ue_to_gltf(*p);
            for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(g) {
                *l = l.min(v);
                *h = h.max(v);
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let pos_len = bin.len() - pos_off;
        let idx_off = bin.len();
        for t in &m.indices {
            for i in [t[0], t[2], t[1]] {
                bin.extend_from_slice(&i.to_le_bytes());
            }
        }
        let idx_len = bin.len() - idx_off;
        let pv = views.len();
        views.push(serde_json::json!({
            "buffer": 0, "byteOffset": pos_off, "byteLength": pos_len, "target": 34962
        }));
        views.push(serde_json::json!({
            "buffer": 0, "byteOffset": idx_off, "byteLength": idx_len, "target": 34963
        }));
        let pa = accessors.len();
        accessors.push(serde_json::json!({
            "bufferView": pv, "componentType": 5126, "count": m.positions.len(),
            "type": "VEC3", "min": lo, "max": hi
        }));
        accessors.push(serde_json::json!({
            "bufferView": pv + 1, "componentType": 5125, "count": m.indices.len() * 3,
            "type": "SCALAR"
        }));
        let mi = gl_meshes.len();
        gl_meshes.push(serde_json::json!({
            "name": name,
            "primitives": [{ "attributes": { "POSITION": pa }, "indices": pa + 1, "mode": 4 }]
        }));
        nodes.push(serde_json::json!({ "name": name, "mesh": mi }));
    }
    let node_ids: Vec<usize> = (0..nodes.len()).collect();
    let doc = serde_json::json!({
        "asset": {
            "version": "2.0",
            "generator": "asamu-import levels",
            "extras": {
                "note": "Derived from the user's own game install; do not redistribute.",
                "axes": "glTF = (ue.y, ue.z, -ue.x); units: Unreal units (unscaled)"
            }
        },
        "scene": 0,
        "scenes": [{ "nodes": node_ids }],
        "nodes": nodes,
        "meshes": gl_meshes,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": bin.len() }]
    });
    let mut json = serde_json::to_vec(&doc)?;
    pad4(&mut json, b' ');
    pad4(&mut bin, 0);
    let total = 12 + 8 + json.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let u32_of = |n: usize| u32::try_from(n).context("glTF binary larger than 4 GiB");
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&u32_of(total)?.to_le_bytes());
    out.extend_from_slice(&u32_of(json.len())?.to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    if !bin.is_empty() {
        out.extend_from_slice(&u32_of(bin.len())?.to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
    }
    Ok(out)
}

// ------------------------------------------------------------ manifest

/// One map in `manifest.json` (counts only).
#[derive(Debug, Serialize)]
pub struct ManifestEntry {
    /// Map package name.
    pub map: String,
    /// Files written.
    pub files: Vec<String>,
    /// `WorldInfo.Title`.
    pub title: Option<String>,
    /// Streaming sub-level package names.
    pub streaming_levels: Vec<String>,
    /// Actors extracted.
    pub actors: usize,
    /// Actors per kind.
    pub kinds: BTreeMap<String, usize>,
    /// Components.
    pub components: usize,
    /// BSP triangles (drawn, blocking).
    pub bsp_triangles: Option<[usize; 2]>,
    /// Warnings recorded in the scene.
    pub warnings: usize,
}

/// `manifest.json`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: &'static str,
    /// Scene format version.
    pub scene_version: u32,
    /// BSP format version.
    pub bsp_version: u32,
    /// Maps.
    pub maps: Vec<ManifestEntry>,
}

fn kinds_of(scene: &Scene) -> BTreeMap<String, usize> {
    scene
        .stats
        .kinds
        .iter()
        .map(|(k, v)| {
            let name = serde_json::to_value(k)
                .ok()
                .and_then(|j| j.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{k:?}"));
            (name, *v)
        })
        .collect()
}

fn convert_map(file: &Path, cooked: &Path, out_dir: &Path, args: &Args) -> Result<ManifestEntry> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp: Arc<LoadedPackage> = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    let levels = level::level_exports(&lp.package);
    let Some(&level_export) = levels.first() else {
        bail!("{} has no Level export", file.display());
    };
    let opts = SceneOptions {
        volume_geometry: !args.no_volume_geometry,
        all_params: args.all_params,
        ..SceneOptions::default()
    };
    let scene = level::extract_scene(&set, &lp, level_export, &opts)
        .with_context(|| format!("extracting the scene of {}", lp.name))?;
    let map = lp.name.clone();
    let mut files = Vec::new();
    let name = format!("{map}.scene.json");
    write_file(
        out_dir,
        &name,
        &to_json(&scene, args.pretty)?,
        file,
        args.force,
    )?;
    files.push(name);

    let mut bsp_triangles = None;
    if !args.no_bsp
        && let Some(model) = scene.tail.model.export_index()
    {
        let g = level::extract_bsp(&set, &lp, model)
            .with_context(|| format!("decoding the BSP of {map}"))?;
        let bin_name = format!("{map}.bsp.bin");
        let (json, bin) = bsp_files(&g, &bin_name, args.pretty)?;
        let json_name = format!("{map}.bsp.json");
        write_file(out_dir, &json_name, &json, file, args.force)?;
        write_file(out_dir, &bin_name, &bin, file, args.force)?;
        files.push(json_name);
        files.push(bin_name);
        if args.gltf {
            let glb_name = format!("{map}.bsp.glb");
            let data = glb(&[("visible", &g.visible), ("collision", &g.collision)])?;
            write_file(out_dir, &glb_name, &data, file, args.force)?;
            files.push(glb_name);
        }
        bsp_triangles = Some([g.visible.triangle_count(), g.collision.triangle_count()]);
    }
    Ok(ManifestEntry {
        map,
        files,
        title: scene.world_info.as_ref().and_then(|w| w.title.clone()),
        streaming_levels: scene
            .streaming_levels
            .iter()
            .filter_map(|s| s.package_name.clone())
            .collect(),
        actors: scene.stats.actors,
        kinds: kinds_of(&scene),
        components: scene.stats.components,
        bsp_triangles,
        warnings: scene.warnings.len(),
    })
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install_root) = install_dirs(ctx)?;
    let maps_dir = cooked.join("Maps");
    let maps = select_maps(&maps_dir, &args.maps)?;
    if maps.is_empty() {
        bail!("no map packages in {}", maps_dir.display());
    }
    let out_dir = prepare_dir(&ctx.out.join("levels"), &maps_dir, &install_root)?;
    eprintln!(
        "writing level data derived from your own install to {} (do not redistribute)",
        out_dir.display()
    );
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        scene_version: level::SCENE_VERSION,
        bsp_version: BSP_VERSION,
        maps: Vec::new(),
    };
    for file in &maps {
        let entry = convert_map(file, &cooked, &out_dir, &args)?;
        println!(
            "{:<20} actors {:>5}  components {:>5}  bsp triangles {:>12}  streaming {:?}  warnings {}",
            entry.map,
            entry.actors,
            entry.components,
            entry
                .bsp_triangles
                .map(|[v, c]| format!("{v}/{c}"))
                .unwrap_or_else(|| "-".to_owned()),
            entry.streaming_levels,
            entry.warnings
        );
        manifest.maps.push(entry);
    }
    write_file(
        &out_dir,
        "manifest.json",
        &to_json(&manifest, true)?,
        &maps_dir,
        true,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> TriangleMesh {
        let mut m = TriangleMesh::default();
        m.add_polygon(
            &[
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            3,
        );
        m
    }

    #[test]
    fn bsp_binary_layout_matches_index() {
        let m = quad();
        let mut bin = Vec::new();
        let spans = push_mesh(&mut bin, &m);
        assert_eq!(
            spans.positions,
            Span {
                offset: 0,
                count: 4
            }
        );
        assert_eq!(
            spans.triangles,
            Span {
                offset: 48,
                count: 2
            }
        );
        assert_eq!(
            spans.surfaces,
            Span {
                offset: 72,
                count: 2
            }
        );
        assert_eq!(bin.len(), 80);
        // Second triangle (0, 2, 3), little-endian u32.
        assert_eq!(&bin[60..72], &[0, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&bin[72..76], &3u32.to_le_bytes());
        // x of vertex 1 = 1.0
        assert_eq!(&bin[12..16], &1.0f32.to_le_bytes());
    }

    #[test]
    fn glb_is_well_formed_and_flips_winding() {
        let m = quad();
        let empty = TriangleMesh::default();
        let data = glb(&[("visible", &m), ("collision", &empty)]).unwrap();
        assert_eq!(&data[0..4], b"glTF");
        let total = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
        assert_eq!(total, data.len());
        let json_len = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
        assert_eq!(&data[16..20], b"JSON");
        assert_eq!(json_len % 4, 0);
        let doc: serde_json::Value = serde_json::from_slice(&data[20..20 + json_len]).unwrap();
        assert_eq!(
            doc["meshes"].as_array().unwrap().len(),
            1,
            "empty meshes are skipped"
        );
        assert_eq!(doc["accessors"][0]["count"], 4);
        let bin_off = 20 + json_len;
        let bin_len = u32::from_le_bytes(data[bin_off..bin_off + 4].try_into().unwrap()) as usize;
        assert_eq!(&data[bin_off + 4..bin_off + 8], b"BIN\0");
        assert_eq!(bin_off + 8 + bin_len, data.len());
        assert_eq!(doc["buffers"][0]["byteLength"].as_u64().unwrap(), 72);
        // First index triple is (0, 2, 1): winding reversed.
        let idx = bin_off + 8 + 48;
        let first: Vec<u32> = (0..3)
            .map(|k| u32::from_le_bytes(data[idx + 4 * k..idx + 4 * k + 4].try_into().unwrap()))
            .collect();
        assert_eq!(first, vec![0, 2, 1]);
        // Axis mapping: UE +X (forward) → glTF −Z.
        assert_eq!(ue_to_gltf([1.0, 2.0, 3.0]), [2.0, 3.0, -1.0]);
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn prepare_dir_refuses_the_repository_and_creates_local_dirs() {
        let root = repo_root();
        let input = root.join("Cargo.toml");
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("no-install-here");
        assert!(prepare_dir(&root.join("asamu-import-test-out"), &input, &install).is_err());
        assert!(!root.join("asamu-import-test-out").exists());
        assert!(prepare_dir(&root.join("docs").join("x"), &input, &install).is_err());
        let app = tmp.path().join("Game.app").join("Contents");
        std::fs::create_dir_all(&app).unwrap();
        assert!(prepare_dir(&app.join("levels"), &input, &install).is_err());
        let ok = prepare_dir(
            &tmp.path().join("a").join("b").join("levels"),
            &input,
            &install,
        )
        .unwrap();
        assert!(ok.is_dir());
        let f = write_file(&ok, "x.json", b"{}", &input, false).unwrap();
        assert!(write_file(&ok, "x.json", b"{}", &input, false).is_err());
        assert!(write_file(&ok, "x.json", b"[]", &input, true).is_ok());
        assert_eq!(std::fs::read(f).unwrap(), b"[]");
    }

    #[test]
    fn prepare_dir_refuses_the_install_and_escapes_into_the_repository() {
        let root = repo_root();
        let input = root.join("Cargo.toml");
        let tmp = tempfile::tempdir().unwrap();
        // A copy of the game outside Steam (Windows/Linux layout: no .app,
        // no steamapps): nothing may be written inside its root, whatever
        // the case of the path.
        let install = tmp.path().join("Games").join("ASAMU");
        std::fs::create_dir_all(install.join("ASAMU").join("CookedPC")).unwrap();
        assert!(prepare_dir(&install.join("levels"), &input, &install).is_err());
        assert!(!install.join("levels").exists());
        assert!(prepare_dir(&install.join("ASAMU").join("out"), &input, &install).is_err());
        // On a case-insensitive file system (macOS, Windows defaults) the
        // same directory spelled in another case is the install too.
        if tmp.path().join("GAMES").exists() {
            let upper = tmp.path().join("GAMES").join("asamu").join("levels");
            assert!(prepare_dir(&upper, &input, &install).is_err());
        }
        // Next to the install is fine.
        let beside = prepare_dir(&tmp.path().join("Games").join("out"), &input, &install);
        assert!(beside.is_ok(), "{beside:?}");
        // `..` cannot climb out of a missing directory into the repository.
        let climb = tmp
            .path()
            .join("missing")
            .join("..")
            .join("..")
            .join("levels");
        assert!(prepare_dir(&climb, &input, &install).is_err());
        let research = root.join("research").join("local");
        if research.is_dir() {
            let up = research.join("..").join("..").join("docs").join("levels");
            assert!(prepare_dir(&up, &input, &install).is_err());
            assert!(!root.join("docs").join("levels").exists());
        }
        // A symlink that leads into the repository is resolved and refused.
        #[cfg(unix)]
        {
            let link = tmp.path().join("to-repo");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            assert!(prepare_dir(&link.join("levels"), &input, &install).is_err());
            assert!(!root.join("levels").exists());
            let link = tmp.path().join("to-install");
            std::os::unix::fs::symlink(&install, &link).unwrap();
            assert!(prepare_dir(&link.join("levels"), &input, &install).is_err());
        }
    }

    #[test]
    fn containment_ignores_case_but_not_prefixes() {
        assert!(is_within(Path::new("/a/B/c"), Path::new("/a/b")));
        assert!(is_within(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a"), Path::new("/a/b")));
    }

    #[test]
    fn map_selection_is_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        for n in [
            "AG-IceCave.asamu",
            "TheCore.asamu",
            "AG-IceCave_LOC_INT.upk",
        ] {
            std::fs::write(tmp.path().join(n), b"").unwrap();
        }
        let all = select_maps(tmp.path(), &[]).unwrap();
        assert_eq!(all.len(), 2);
        let one = select_maps(tmp.path(), &["ag-icecave".to_owned()]).unwrap();
        assert_eq!(one.len(), 1);
        assert!(select_maps(tmp.path(), &["Nope".to_owned()]).is_err());
    }
}
