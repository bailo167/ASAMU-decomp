//! `asamu-import materials`: Material / MaterialInstance → approximate PBR
//! descriptions (`materials.json`).
//!
//! Every `Material`, `DecalMaterial`, `MaterialInstanceConstant` and
//! `MaterialInstanceTimeVarying` of every package is decoded (tagged
//! properties, expression graph, cooked native tail), its instance chain is
//! resolved to the base material with parameter overrides applied, and the
//! graph of each material input is reduced to an approximate description for
//! a modern renderer (see `asamu_ue3::material` and
//! `docs/reverse-engineering/MATERIALS.md`, which documents the schema).
//!
//! Output (user-local only; never the repository or the game install):
//!
//! ```text
//! <out>/materials/materials.json     format "asamu-materials", version 1
//! ```
//!
//! keyed by material object path exactly as mesh sections
//! (`meshes/manifest.json`), level actors' material overrides and BSP
//! surfaces (`levels/*.json`) reference it. Texture references are texture
//! object paths, the keys of `textures/manifest.json`.
//!
//! `--check` writes nothing and prints the coverage report. The data is
//! derived from copyrighted game files: keep it local, do not redistribute.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::material::{
    ApproxMaterial, ClassCoverage, MaterialCoverage, MaterialDecoder, count_guid_occurrences,
    guid_hex, scan_package,
};
use asamu_ue3::model::PackageSet;
use asamu_ue3::types::Guid;
use serde::Serialize;

use crate::safety;

/// `format` of `materials.json`.
pub const FORMAT: &str = "asamu-materials";
/// `version` of `materials.json`.
pub const VERSION: u32 = 1;

const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
     Copyrighted game data: do not redistribute.";

const CONVENTIONS: &str = "Colours are linear RGBA (UE3 Color constants converted with (c/255)^2.2, \
     alpha linear). A channel's value is texture.channels * value + bias when a texture is bound, \
     else value; a single texture channel is broadcast to every component. Texture keys are \
     texture object paths (textures/manifest.json). UV transforms: uv' = uv[channel] * scale + \
     offset + panning * seconds, then a rotation by rotation_angle + rotation * seconds radians \
     about rotation_center (rotation_angle is omitted when 0). Normal maps are tangent space in \
     UE3 convention.";

/// Shader cache packages (one huge `ShaderCache` export each); skipped by the
/// material scan and only searched with `--shader-caches`.
const SHADER_CACHE_PREFIXES: &[&str] = &["refshadercache", "globalshadercache"];

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only packages whose file name contains this text (case-insensitive;
    /// repeatable).
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Only materials whose object path contains this text (case-insensitive).
    #[arg(long)]
    name: Option<String>,
    /// Write nothing: decode and approximate everything and print the
    /// coverage report.
    #[arg(long)]
    check: bool,
    /// With --check: print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Also search the decompressed `RefShaderCache-*.upk` streams for every
    /// material resource Id (about 130 MB each; slower).
    #[arg(long)]
    shader_caches: bool,
    /// Pretty-print materials.json.
    #[arg(long)]
    pretty: bool,
    /// Overwrite an existing materials.json.
    #[arg(long)]
    force: bool,
}

/// One material in `materials.json`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MaterialEntry {
    /// Package file stem the entry was taken from.
    pub package: String,
    /// Export index in that package.
    pub export_index: usize,
    /// The approximation.
    #[serde(flatten)]
    pub approx: ApproxMaterial,
    /// Other packages holding an identical copy of the object.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
    /// Other packages holding a copy whose approximation differs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differs_in: Vec<String>,
}

/// Totals over all packages.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    /// Per material class (exports in all packages, copies included).
    pub classes: BTreeMap<String, ClassCoverage>,
    /// Material expression exports.
    pub expression_exports: usize,
    /// Of which ending exactly at `SerialSize`.
    pub expression_exact: usize,
    /// Native tail bytes consumed.
    pub native_bytes: u64,
    /// Materials and instances approximated (every copy).
    pub approximated: usize,
    /// Of which lossless.
    pub lossless: usize,
    /// Fallbacks (every copy).
    pub fallback: usize,
    /// Distinct object paths written.
    pub distinct_paths: usize,
    /// Distinct paths approximated / lossless / fallback.
    pub distinct_approximated: usize,
    /// Distinct lossless.
    pub distinct_lossless: usize,
    /// Distinct fallbacks.
    pub distinct_fallback: usize,
    /// Repeated paths identical to the first copy / differing.
    pub repeats_identical: usize,
    /// Repeated paths whose approximation differs.
    pub repeats_differing: usize,
    /// Alpha modes (distinct paths).
    pub alpha_modes: BTreeMap<String, usize>,
    /// Lighting models (distinct paths).
    pub lighting_models: BTreeMap<String, usize>,
    /// Base colour sources (distinct paths).
    pub base_color: BTreeMap<String, usize>,
    /// Distinct paths with a normal map.
    pub normal_maps: usize,
    /// Distinct paths with an emissive texture.
    pub emissive_textures: usize,
    /// Distinct paths with panning/rotating UVs.
    pub animated_uvs: usize,
    /// Distinct paths with tiled/offset UVs or a UV channel other than 0.
    pub transformed_uvs: usize,
    /// Unsupported expression kinds (distinct paths).
    pub unsupported: BTreeMap<String, usize>,
    /// Shader cache search (with --shader-caches).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shader_caches: Vec<ShaderCacheCheck>,
    /// Shader cache packages present but not decoded.
    pub shader_cache_packages: Vec<String>,
}

/// Result of searching one shader cache for material resource Ids.
#[derive(Debug, Clone, Serialize)]
pub struct ShaderCacheCheck {
    /// Package file name.
    pub package: String,
    /// Decompressed stream length.
    pub stream_bytes: usize,
    /// Distinct resource Ids searched.
    pub ids: usize,
    /// Ids found at least once.
    pub found: usize,
    /// Total occurrences.
    pub occurrences: usize,
}

/// The coverage report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// Per package.
    pub packages: Vec<MaterialCoverage>,
    /// Totals.
    pub totals: Totals,
}

/// `materials.json`.
#[derive(Debug, Serialize)]
pub struct MaterialsFile<'a> {
    /// [`FORMAT`].
    pub format: &'static str,
    /// [`VERSION`].
    pub version: u32,
    /// Origin notice.
    pub notice: &'static str,
    /// Value conventions.
    pub conventions: &'static str,
    /// Materials by object path.
    pub materials: &'a BTreeMap<String, MaterialEntry>,
    /// Coverage totals.
    pub coverage: &'a Totals,
}

fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.maps_dir, install.root))
}

fn is_package(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .map(|x| x.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
}

fn is_shader_cache(p: &Path) -> bool {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    SHADER_CACHE_PREFIXES.iter().any(|x| stem.starts_with(x))
}

/// Package files of `dirs` (cooked folder first, then maps), sorted per
/// folder, split into material-bearing packages and shader caches.
fn package_files(dirs: &[PathBuf], filters: &[String]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files = Vec::new();
    let mut caches = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_package(p))
            .collect();
        v.sort();
        for p in v {
            if is_shader_cache(&p) {
                caches.push(p);
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if filters.is_empty()
                || filters
                    .iter()
                    .any(|f| name.contains(&f.to_ascii_lowercase()))
            {
                files.push(p);
            }
        }
    }
    (files, caches)
}

/// Create `dir` and missing parents, validating each new component as an
/// output location (no repository paths outside ignored `research/`, no
/// `.app`/`steamapps`, no symlinks) and refusing anything inside `install`.
fn prepare_dir(dir: &Path, input: &Path, install: &Path) -> Result<PathBuf> {
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
    if let Ok(root) = install.canonicalize()
        && is_within(&cur, &root)
    {
        bail!(
            "refusing to write inside the game install {} (choose an output directory outside it)",
            root.display()
        );
    }
    safety::check_output_path(&cur.join(".asamu-import-probe"), input, false)?;
    for name in missing.into_iter().rev() {
        let next = safety::check_output_path(&cur.join(&name), input, false)?;
        std::fs::create_dir(&next).with_context(|| format!("creating {}", next.display()))?;
        cur = next;
    }
    Ok(cur)
}

/// `path` is `root` or below it (components compared without ASCII case).
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

fn bump(m: &mut BTreeMap<String, usize>, k: &str) {
    *m.entry(k.to_owned()).or_insert(0) += 1;
}

/// Fold one distinct entry into the totals (distinct-path statistics).
fn count_distinct(t: &mut Totals, a: &ApproxMaterial) {
    let mut one = MaterialCoverage::default();
    one.add_approximation(a);
    t.distinct_approximated += one.approximated;
    t.distinct_lossless += one.lossless;
    t.distinct_fallback += one.fallback;
    for (k, v) in one.alpha_modes {
        *t.alpha_modes.entry(k).or_insert(0) += v;
    }
    for (k, v) in one.lighting_models {
        *t.lighting_models.entry(k).or_insert(0) += v;
    }
    for (k, v) in one.base_color {
        *t.base_color.entry(k).or_insert(0) += v;
    }
    t.normal_maps += one.normal_maps;
    t.emissive_textures += one.emissive_textures;
    t.animated_uvs += one.animated_uvs;
    t.transformed_uvs += one.transformed_uvs;
    for k in one.unsupported.keys() {
        bump(&mut t.unsupported, k);
    }
}

/// Decode and approximate every selected package. Returns the entries by
/// object path, the report and every distinct resource Id.
pub fn convert(
    cooked: &Path,
    maps: &Path,
    files: &[PathBuf],
    name_filter: Option<&str>,
) -> Result<(BTreeMap<String, MaterialEntry>, Report, Vec<Guid>)> {
    let dirs = [cooked.to_path_buf(), maps.to_path_buf()];
    let mut entries: BTreeMap<String, MaterialEntry> = BTreeMap::new();
    let mut lower_index: HashMap<String, String> = HashMap::new();
    let mut report = Report::default();
    let mut ids: Vec<Guid> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let filter = name_filter.map(str::to_ascii_lowercase);
    for file in files {
        // A fresh set per package keeps memory bounded (maps are large);
        // the script packages it needs are reopened on demand.
        let set = PackageSet::new(&dirs);
        let lp: Arc<_> = set
            .open_file(file)
            .with_context(|| format!("opening {}", file.display()))?;
        let dec = MaterialDecoder::new(&set);
        let package = lp.name.clone();
        let cov = scan_package(&dec, &lp, &mut |m, res| {
            for r in &m.native.resources {
                if seen_ids.insert(r.resource.id) {
                    ids.push(r.resource.id);
                }
            }
            let Ok(a) = res else {
                return;
            };
            if filter
                .as_deref()
                .is_some_and(|f| !m.path.to_ascii_lowercase().contains(f))
            {
                return;
            }
            let key = m.path.to_ascii_lowercase();
            match lower_index.get(&key).and_then(|k| entries.get_mut(k)) {
                Some(first) => {
                    if &first.approx == a {
                        first.also_in.push(package.clone());
                    } else {
                        first.differs_in.push(package.clone());
                    }
                }
                None => {
                    lower_index.insert(key, m.path.clone());
                    entries.insert(
                        m.path.clone(),
                        MaterialEntry {
                            package: package.clone(),
                            export_index: m.export_index,
                            approx: a.clone(),
                            also_in: Vec::new(),
                            differs_in: Vec::new(),
                        },
                    );
                }
            }
        });
        if !cov.classes.is_empty() || !cov.expression_exports.is_empty() {
            report.packages.push(cov);
        }
    }
    let t = &mut report.totals;
    for c in &report.packages {
        for (k, v) in &c.classes {
            let e = t.classes.entry(k.clone()).or_default();
            e.total += v.total;
            e.default_objects += v.default_objects;
            e.decoded += v.decoded;
            e.round_trip += v.round_trip;
        }
        t.expression_exports += c.expression_exports.values().sum::<usize>();
        t.expression_exact += c.expression_exact;
        t.native_bytes += c.native_bytes;
        t.approximated += c.approximated;
        t.lossless += c.lossless;
        t.fallback += c.fallback;
    }
    t.distinct_paths = entries.len();
    for e in entries.values() {
        count_distinct(t, &e.approx);
        if e.also_in.is_empty() && e.differs_in.is_empty() {
            continue;
        }
        t.repeats_identical += e.also_in.len();
        t.repeats_differing += e.differs_in.len();
    }
    Ok((entries, report, ids))
}

fn shader_cache_checks(caches: &[PathBuf], ids: &[Guid]) -> Result<Vec<ShaderCacheCheck>> {
    let mut out = Vec::new();
    for c in caches {
        let pkg =
            asamu_ue3::Package::open(c).with_context(|| format!("opening {}", c.display()))?;
        let hits = count_guid_occurrences(pkg.stream(), ids);
        out.push(ShaderCacheCheck {
            package: c
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            stream_bytes: pkg.stream().len(),
            ids: ids.len(),
            found: hits.values().filter(|n| **n > 0).count(),
            occurrences: hits.values().sum(),
        });
    }
    Ok(out)
}

fn print_report(r: &Report) {
    println!(
        "{:22} {:>9} {:>9} {:>9} {:>9} {:>6} {:>8} {:>5} {:>6}",
        "package",
        "materials",
        "instances",
        "functions",
        "round-trip",
        "appr",
        "lossless",
        "fb",
        "exprs"
    );
    for c in &r.packages {
        let sum = |pred: fn(&str) -> bool| -> (usize, usize) {
            c.classes
                .iter()
                .filter(|(k, _)| pred(k))
                .fold((0, 0), |(t, d), (_, v)| (t + v.total, d + v.decoded))
        };
        let (mt, md) =
            sum(|k| k.contains("Material") && !k.contains("Instance") && !k.contains("Function"));
        let (it, id) = sum(|k| k.contains("Instance"));
        let (ft, fd) = sum(|k| k.contains("Function"));
        let rt: usize = c.classes.values().map(|v| v.round_trip).sum();
        let tot: usize = c.classes.values().map(|v| v.total).sum();
        println!(
            "{:22} {:>4}/{:<4} {:>4}/{:<4} {:>4}/{:<4} {:>4}/{:<4} {:>6} {:>8} {:>5} {:>6}",
            c.package,
            md,
            mt,
            id,
            it,
            fd,
            ft,
            rt,
            tot,
            c.approximated,
            c.lossless,
            c.fallback,
            c.expression_exports.values().sum::<usize>()
        );
        for f in &c.failures {
            println!("    FAIL {f}");
        }
    }
    let t = &r.totals;
    println!();
    for (k, v) in &t.classes {
        println!(
            "{k:28} exports {:5} (default objects {}) decoded {:5} native round trip {:5}",
            v.total, v.default_objects, v.decoded, v.round_trip
        );
    }
    println!(
        "expression exports {} (tags end at SerialSize: {}), material native bytes {}",
        t.expression_exports, t.expression_exact, t.native_bytes
    );
    println!(
        "approximated {} (lossless {}), fallback {} over all copies",
        t.approximated, t.lossless, t.fallback
    );
    println!(
        "distinct paths {}: approximated {} (lossless {}), fallback {}; repeats identical {}, differing {}",
        t.distinct_paths,
        t.distinct_approximated,
        t.distinct_lossless,
        t.distinct_fallback,
        t.repeats_identical,
        t.repeats_differing
    );
    println!("alpha modes      {:?}", t.alpha_modes);
    println!("lighting models  {:?}", t.lighting_models);
    println!("base colour      {:?}", t.base_color);
    println!(
        "normal maps {}, emissive textures {}, animated UVs {}, transformed UVs {}",
        t.normal_maps, t.emissive_textures, t.animated_uvs, t.transformed_uvs
    );
    println!(
        "unsupported expression kinds (materials affected): {:?}",
        t.unsupported
    );
    for s in &t.shader_caches {
        println!(
            "shader cache {}: {} of {} resource Ids found ({} occurrences, {} stream bytes)",
            s.package, s.found, s.ids, s.occurrences, s.stream_bytes
        );
    }
    if t.shader_caches.is_empty() && !t.shader_cache_packages.is_empty() {
        println!(
            "shader caches not searched (pass --shader-caches): {}",
            t.shader_cache_packages.join(", ")
        );
    }
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, maps, install_root) = install_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps.clone()];
    let (files, caches) = package_files(&dirs, &args.packages);
    if files.is_empty() {
        bail!("no package matches the --package filters");
    }
    let (entries, mut report, ids) = convert(&cooked, &maps, &files, args.name.as_deref())?;
    report.totals.shader_cache_packages = caches
        .iter()
        .filter_map(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    if args.shader_caches {
        report.totals.shader_caches = shader_cache_checks(&caches, &ids)?;
    }
    if args.check {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_report(&report);
        }
        return Ok(());
    }
    let input = files.first().map(PathBuf::as_path).unwrap_or(&cooked);
    let dir = prepare_dir(&ctx.out.join("materials"), input, &install_root)?;
    let doc = MaterialsFile {
        format: FORMAT,
        version: VERSION,
        notice: NOTICE,
        conventions: CONVENTIONS,
        materials: &entries,
        coverage: &report.totals,
    };
    let bytes = if args.pretty {
        serde_json::to_vec_pretty(&doc)?
    } else {
        serde_json::to_vec(&doc)?
    };
    let target = safety::check_output_path(&dir.join("materials.json"), input, args.force)?;
    safety::write_output(&target, &bytes, args.force)?;
    println!(
        "wrote {} materials ({} approximated, {} fallback) to {}",
        entries.len(),
        report.totals.distinct_approximated,
        report.totals.distinct_fallback,
        target.display()
    );
    if !ids.is_empty() {
        let sample = ids.first().map(|g| guid_hex(*g)).unwrap_or_default();
        println!(
            "{} distinct material resource Ids (e.g. {sample})",
            ids.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_caches_are_recognised() {
        assert!(is_shader_cache(Path::new(
            "/x/RefShaderCache-PC-OpenGL.upk"
        )));
        assert!(is_shader_cache(Path::new(
            "GlobalShaderCache-PC-D3D-SM3.upk"
        )));
        assert!(!is_shader_cache(Path::new("/x/Startup.upk")));
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(
            !is_package(&manifest),
            "only .u/.upk/.asamu files are packages"
        );
    }

    #[test]
    fn within_ignores_ascii_case() {
        assert!(is_within(Path::new("/A/b/c"), Path::new("/a/B")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
    }

    #[test]
    fn output_dir_refuses_install_and_repository() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let install = tmp.path().join("Game");
        std::fs::create_dir_all(&install).unwrap_or_else(|e| panic!("{e}"));
        let input = install.join("x.upk");
        assert!(prepare_dir(&install.join("out"), &input, &install).is_err());
        let ok = prepare_dir(&tmp.path().join("out/materials"), &input, &install);
        assert!(ok.is_ok(), "{ok:?}");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/should-not-exist");
        assert!(prepare_dir(&repo, &input, &install).is_err());
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| panic!("repository root"))
    }

    /// Escapes through `..`, symlinks (to the repository, to the install,
    /// dangling) and a different spelling of the install are all refused,
    /// and nothing is created on the way.
    #[test]
    fn output_dir_refuses_escapes_and_links() {
        let root = repo_root();
        let input = root.join("Cargo.toml");
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let install = tmp.path().join("Games").join("ASAMU");
        std::fs::create_dir_all(install.join("ASAMU")).unwrap_or_else(|e| panic!("{e}"));
        // `..` out of a missing directory.
        let climb = tmp.path().join("missing").join("..").join("..").join("m");
        assert!(prepare_dir(&climb, &input, &install).is_err());
        let research = root.join("research").join("local");
        if research.is_dir() {
            let up = research
                .join("..")
                .join("..")
                .join("docs")
                .join("materials");
            assert!(prepare_dir(&up, &input, &install).is_err());
            assert!(!root.join("docs").join("materials").exists());
        }
        // Inside the install, also through another letter case where the
        // file system ignores case.
        assert!(prepare_dir(&install.join("ASAMU").join("materials"), &input, &install).is_err());
        if tmp.path().join("GAMES").exists() {
            let upper = tmp.path().join("GAMES").join("asamu").join("materials");
            assert!(prepare_dir(&upper, &input, &install).is_err());
        }
        #[cfg(unix)]
        {
            let link = tmp.path().join("to-repo");
            std::os::unix::fs::symlink(&root, &link).unwrap_or_else(|e| panic!("{e}"));
            assert!(prepare_dir(&link.join("materials"), &input, &install).is_err());
            assert!(!root.join("materials").exists());
            let link = tmp.path().join("to-install");
            std::os::unix::fs::symlink(&install, &link).unwrap_or_else(|e| panic!("{e}"));
            assert!(prepare_dir(&link.join("materials"), &input, &install).is_err());
            // A dangling link as a directory component to create.
            let target = tmp.path().join("not-yet");
            let dangling = tmp.path().join("dangling");
            std::os::unix::fs::symlink(&target, &dangling).unwrap_or_else(|e| panic!("{e}"));
            assert!(prepare_dir(&dangling.join("materials"), &input, &install).is_err());
            assert!(!target.exists());
        }
        // Next to the install is fine; an existing materials.json is kept
        // without --force and replaced with it.
        let dir = prepare_dir(&tmp.path().join("out").join("materials"), &input, &install)
            .unwrap_or_else(|e| panic!("{e}"));
        let target = safety::check_output_path(&dir.join("materials.json"), &input, false)
            .unwrap_or_else(|e| panic!("{e}"));
        safety::write_output(&target, b"{}", false).unwrap_or_else(|e| panic!("{e}"));
        assert!(safety::check_output_path(&dir.join("materials.json"), &input, false).is_err());
        let again = safety::check_output_path(&dir.join("materials.json"), &input, true)
            .unwrap_or_else(|e| panic!("{e}"));
        safety::write_output(&again, b"[]", true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            std::fs::read(dir.join("materials.json")).unwrap_or_default(),
            b"[]"
        );
    }

    /// `rotation_angle` (added by the verification pass) only appears when a
    /// fixed rotation is present, so files without one are unchanged.
    #[test]
    fn uv_rotation_angle_is_omitted_when_zero() {
        let uv = asamu_ue3::material::UvTransform::default();
        let v = serde_json::to_value(uv).unwrap_or_else(|e| panic!("{e}"));
        assert!(v.get("rotation_angle").is_none(), "{v}");
        assert!(v.get("rotation").is_some());
        let turned = asamu_ue3::material::UvTransform {
            rotation_angle: 1.5,
            ..uv
        };
        let v = serde_json::to_value(turned).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(v.get("rotation_angle").and_then(|x| x.as_f64()), Some(1.5));
    }
}
