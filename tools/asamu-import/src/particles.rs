//! `asamu-import particles`: particle systems (emitters, LOD levels, modules
//! and their distributions) and per-map emitter placements.
//!
//! Every `ParticleSystem` of every package is decoded through
//! `asamu_ue3::particle` (strict: every particle export's tagged properties
//! must end at `SerialSize`; see `docs/reverse-engineering/PARTICLES.md`,
//! which documents the schema). Map packages are read directly: every placed
//! actor's `ParticleSystemComponent` (in the shipped maps all of them belong
//! to `Emitter` actors) with its effective template, `bAutoActivate`, world
//! transform and instance parameters. Components that live in class default
//! objects or archetypes (effects that script spawns, e.g. weapon and pawn
//! effects) are listed as templates.
//!
//! Output (user-local only; never the repository or the game install):
//!
//! ```text
//! <out>/particles/particles.json        format "asamu-particles", version 1
//! <out>/particles/maps/<Map>.json       format "asamu-particle-placements", version 1
//! ```
//!
//! `--check` writes nothing and prints the coverage report. The data is
//! derived from copyrighted game files: keep it local, do not redistribute.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::level::{self, ComponentKind, Mat4, SceneOptions};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::particle::{
    InstanceParameter, PARTICLES_FORMAT, PARTICLES_VERSION, ParticleCensus, ParticleDecoder,
    ParticleRole, ParticleSystem, system_exports,
};
use serde::Serialize;

use crate::safety;

/// `format` of a map's placement file.
pub const PLACEMENTS_FORMAT: &str = "asamu-particle-placements";
/// `version` of a map's placement file.
pub const PLACEMENTS_VERSION: u32 = 1;

const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
     Copyrighted game data: do not redistribute.";

const CONVENTIONS: &str = "Positions in Unreal units, UE3 axes (X forward, Y right, Z up, \
     left-handed); rotators in 65536 units per turn; matrices are UE3 row-vector matrices \
     (p' = p * M, row 3 the translation). Module parameters are the effective tagged properties \
     (own over archetype or class defaults) by name; a RawDistribution property is an object \
     with \"dist\" (float|vector), the decoded distribution object in \"value\" (kind constant, \
     uniform, constant_curve, uniform_curve, parameter, lookup, unsupported) and a \"baked\" \
     summary of the engine's lookup table (op, chunk, range). The shipped game evaluates the \
     distribution object when one is set and the table otherwise. Curves: keys {t, v, arrive, \
     leave, mode}; tangents are multiplied by the segment span unless broken_tangents.";

const SHADER_CACHE_PREFIXES: &[&str] = &["refshadercache", "globalshadercache"];

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Only packages whose file name contains this text (case-insensitive;
    /// repeatable).
    #[arg(long = "package")]
    packages: Vec<String>,
    /// Write nothing: decode everything and print the coverage report.
    #[arg(long)]
    check: bool,
    /// With --check: print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Pretty-print the JSON files.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing files.
    #[arg(long)]
    force: bool,
}

/// One particle system in `particles.json`.
#[derive(Debug, Clone, Serialize)]
pub struct SystemEntry {
    /// The decoded system.
    #[serde(flatten)]
    pub system: ParticleSystem,
    /// Other packages holding a copy of the same object path.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
}

/// A particle system component outside any level (a class default object's
/// or an archetype's component, spawned by script at run time).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComponentTemplate {
    /// Component object path.
    pub path: String,
    /// Package file stem.
    pub package: String,
    /// The object that owns the component (class default object or archetype).
    pub owner: String,
    /// Component class name.
    pub class: String,
    /// `Template`.
    pub template: Option<String>,
    /// `bAutoActivate`.
    pub auto_activate: bool,
    /// The owner is a class default object.
    pub in_class_default: bool,
}

/// One placed particle system component.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Placement {
    /// Owning actor's object name (`Emitter_3`).
    pub actor: String,
    /// Owning actor's qualified path (the key Kismet uses for the actor).
    pub actor_path: String,
    /// Owning actor's class path.
    pub actor_class: String,
    /// `ULevel::Actors` index.
    pub slot: usize,
    /// Actor `Tag`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Actor `bHidden`.
    pub actor_hidden: bool,
    /// Actor `Base` (attachment parent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Matinee actions that move the actor.
    pub moved_by_matinee: bool,
    /// Actor `Location`.
    pub location: [f32; 3],
    /// Actor `Rotation`.
    pub rotation: [i32; 3],
    /// Component object name.
    pub component: String,
    /// Component class name.
    pub component_class: String,
    /// Component world transform (row-vector matrix).
    pub local_to_world: Mat4,
    /// `Template` (the key into `particles.json`).
    pub template: Option<String>,
    /// `bAutoActivate`.
    pub auto_activate: bool,
    /// `HiddenGame`.
    pub hidden_game: bool,
    /// `bKillOnDeactivate`.
    pub kill_on_deactivate: bool,
    /// `bKillOnCompleted`.
    pub kill_on_completed: bool,
    /// `WarmupTime` (0: the template's).
    pub warmup_time: f32,
    /// `SecondsBeforeInactive`.
    pub seconds_before_inactive: f32,
    /// `EmitterDelay`.
    pub emitter_delay: f32,
    /// `InstanceParameters`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub instance_parameters: Vec<InstanceParameter>,
}

/// `particles/maps/<Map>.json`.
#[derive(Debug, Serialize)]
struct PlacementsFile<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    conventions: &'static str,
    map: &'a str,
    placements: &'a [Placement],
}

/// Coverage totals written into `particles.json` and printed by `--check`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// Census over every selected package.
    pub census: ParticleCensus,
    /// Distinct system paths written.
    pub distinct_systems: usize,
    /// System paths found in more than one package.
    pub repeated_systems: usize,
    /// Placed components per map.
    pub placements: BTreeMap<String, usize>,
    /// Placed components whose template is a system in `particles.json`.
    pub placements_resolved: usize,
    /// Placed components without a template.
    pub placements_without_template: usize,
    /// Placed components whose template was not found.
    pub placements_unresolved: Vec<String>,
    /// `bAutoActivate` false / true over the placements.
    pub auto_activate: [usize; 2],
    /// Component templates outside levels.
    pub component_templates: usize,
}

#[derive(Debug, Serialize)]
struct ParticlesFile<'a> {
    format: &'static str,
    version: u32,
    notice: &'static str,
    conventions: &'static str,
    systems: &'a BTreeMap<String, SystemEntry>,
    component_templates: &'a [ComponentTemplate],
    coverage: &'a Report,
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

fn is_map(p: &Path) -> bool {
    p.extension()
        .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("asamu"))
}

/// Package files of `dirs` (cooked folder first, then maps), sorted per
/// folder, shader caches left out.
fn package_files(dirs: &[PathBuf], filters: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_package(p) && !is_shader_cache(p))
            .collect();
        v.sort();
        for p in v {
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
    files
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

fn add_census(into: &mut ParticleCensus, c: ParticleCensus) {
    into.packages += c.packages;
    for (k, v) in c.classes {
        let e = into.classes.entry(k).or_default();
        e.role = v.role;
        e.exports += v.exports;
        e.default_objects += v.default_objects;
        e.exact += v.exact;
        e.failed += v.failed;
    }
    for (k, v) in c.per_package {
        *into.per_package.entry(k).or_default() += v;
    }
    into.systems += c.systems;
    into.systems_failed += c.systems_failed;
    into.emitters += c.emitters;
    into.lod_levels += c.lod_levels;
    into.modules += c.modules;
    into.raw_distributions += c.raw_distributions;
    into.raw_without_object += c.raw_without_object;
    for (k, v) in c.distribution_kinds {
        *into.distribution_kinds.entry(k).or_default() += v;
    }
    for (k, v) in c.module_classes {
        *into.module_classes.entry(k).or_default() += v;
    }
    for (k, v) in c.emitter_kinds {
        *into.emitter_kinds.entry(k).or_default() += v;
    }
    into.payload_bytes = into.payload_bytes.saturating_add(c.payload_bytes);
    for f in c.failures {
        if into.failures.len() < asamu_ue3::particle::MAX_NOTES {
            into.failures.push(f);
        }
    }
    for n in c.notes {
        if into.notes.len() < asamu_ue3::particle::MAX_NOTES {
            into.notes.push(n);
        }
    }
}

/// True when export `index` lies inside a level (a `Level` or `World` outer).
fn in_level(lp: &LoadedPackage, index: usize) -> bool {
    let pkg = &lp.package;
    let Some(idx) = asamu_ue3::types::PackageIndex::from_export(index) else {
        return false;
    };
    pkg.outer_chain(idx).is_ok_and(|chain| {
        chain.iter().any(|o| {
            o.export_index()
                .and_then(|i| pkg.export_class_name(i).ok())
                .is_some_and(|c| c.eq_ignore_ascii_case("Level") || c.eq_ignore_ascii_case("World"))
        })
    })
}

/// Placements of one map package.
fn map_placements(
    set: &PackageSet,
    lp: &Arc<LoadedPackage>,
    dec: &ParticleDecoder<'_>,
    warnings: &mut Vec<String>,
) -> Vec<Placement> {
    let mut out = Vec::new();
    for lvl in level::level_exports(&lp.package) {
        let scene = match level::extract_scene(set, lp, lvl, &SceneOptions::default()) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("{}: level {lvl}: {e}", lp.name));
                continue;
            }
        };
        for a in &scene.actors {
            for c in &a.components {
                if c.kind != ComponentKind::ParticleSystem {
                    continue;
                }
                let info = match dec.component(lp, c.export_index) {
                    Ok(i) => i,
                    Err(e) => {
                        warnings.push(format!("{}: {}: {e}", lp.name, c.name));
                        continue;
                    }
                };
                out.push(Placement {
                    actor: a.name.clone(),
                    actor_path: lp
                        .qualified(a.export_index)
                        .unwrap_or_else(|_| a.name.clone()),
                    actor_class: a.class.clone(),
                    slot: a.slot,
                    tag: a.tag.clone(),
                    actor_hidden: a.hidden,
                    base: a.base.clone(),
                    moved_by_matinee: !a.matinee.is_empty(),
                    location: a.location,
                    rotation: a.rotation,
                    component: c.name.clone(),
                    component_class: info.class.clone(),
                    local_to_world: c.local_to_world,
                    template: info.template.clone(),
                    auto_activate: info.auto_activate,
                    hidden_game: info.hidden_game,
                    kill_on_deactivate: info.kill_on_deactivate,
                    kill_on_completed: info.kill_on_completed,
                    warmup_time: info.warmup_time,
                    seconds_before_inactive: info.seconds_before_inactive,
                    emitter_delay: info.emitter_delay,
                    instance_parameters: info.instance_parameters,
                });
            }
        }
    }
    out
}

/// Everything the conversion produces.
pub struct Converted {
    /// Systems by object path.
    pub systems: BTreeMap<String, SystemEntry>,
    /// Components outside levels.
    pub templates: Vec<ComponentTemplate>,
    /// Placements per map package stem.
    pub maps: BTreeMap<String, Vec<Placement>>,
    /// Coverage.
    pub report: Report,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

/// Decode every selected package.
pub fn convert(cooked: &Path, maps_dir: &Path, files: &[PathBuf]) -> Result<Converted> {
    let mut systems: BTreeMap<String, SystemEntry> = BTreeMap::new();
    let mut folded: BTreeMap<String, String> = BTreeMap::new();
    let mut templates = Vec::new();
    let mut maps = BTreeMap::new();
    let mut report = Report::default();
    let mut warnings = Vec::new();
    for path in files {
        let set = PackageSet::new(&[cooked.to_path_buf(), maps_dir.to_path_buf()]);
        let lp = set
            .open_file(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let dec = ParticleDecoder::new(&set);
        add_census(&mut report.census, dec.census(std::slice::from_ref(&lp)));
        for i in system_exports(&lp.package) {
            match dec.system(&lp, i) {
                Ok(s) => {
                    let key = s.path.to_ascii_lowercase();
                    if let Some(existing) = folded.get(&key) {
                        report.repeated_systems += 1;
                        if let Some(e) = systems.get_mut(existing) {
                            e.also_in.push(lp.name.clone());
                        }
                    } else {
                        folded.insert(key, s.path.clone());
                        systems.insert(
                            s.path.clone(),
                            SystemEntry {
                                system: s,
                                also_in: Vec::new(),
                            },
                        );
                    }
                }
                Err(e) => warnings.push(format!("{}#{i}: {e}", lp.name)),
            }
        }
        for i in 0..lp.package.exports.len() {
            let Some((_, ParticleRole::Component)) = dec.export_role(&lp, i) else {
                continue;
            };
            let is_cdo_itself = lp.package.export(i).is_ok_and(|e| {
                e.object_flags & asamu_ue3::flags::object::CLASS_DEFAULT_OBJECT != 0
            });
            if is_cdo_itself || in_level(&lp, i) {
                continue;
            }
            match dec.component(&lp, i) {
                Ok(info) => {
                    let owner = info
                        .path
                        .rsplit_once('.')
                        .map_or_else(String::new, |(o, _)| o.to_owned());
                    let in_class_default = owner
                        .rsplit('.')
                        .next()
                        .is_some_and(|n| n.starts_with("Default__"));
                    templates.push(ComponentTemplate {
                        path: info.path,
                        package: lp.name.clone(),
                        owner,
                        class: info.class,
                        template: info.template,
                        auto_activate: info.auto_activate,
                        in_class_default,
                    });
                }
                Err(e) => warnings.push(format!("{}#{i}: {e}", lp.name)),
            }
        }
        if is_map(path) {
            let p = map_placements(&set, &lp, &dec, &mut warnings);
            maps.insert(lp.name.clone(), p);
        }
    }
    report.distinct_systems = systems.len();
    report.component_templates = templates.len();
    for (map, list) in &maps {
        report.placements.insert(map.clone(), list.len());
        for p in list {
            report.auto_activate[usize::from(p.auto_activate)] += 1;
            match &p.template {
                None => report.placements_without_template += 1,
                Some(t) if folded.contains_key(&t.to_ascii_lowercase()) => {
                    report.placements_resolved += 1;
                }
                Some(t) => report
                    .placements_unresolved
                    .push(format!("{map}: {} -> {t}", p.actor)),
            }
        }
    }
    Ok(Converted {
        systems,
        templates,
        maps,
        report,
        warnings,
    })
}

fn print_report(c: &Converted) {
    let r = &c.report;
    let cen = &r.census;
    let exports: usize = cen.classes.values().map(|x| x.exports).sum();
    let exact: usize = cen.classes.values().map(|x| x.exact).sum();
    println!(
        "particle exports: {exports} in {} packages, {exact} with tagged properties ending exactly \
         at SerialSize ({} payload bytes)",
        cen.packages, cen.payload_bytes
    );
    println!(
        "systems: {} decoded ({} distinct paths, {} repeats, {} failed); emitters {}, LOD levels \
         {}, modules {}, raw distributions {} ({} without an object)",
        cen.systems,
        r.distinct_systems,
        r.repeated_systems,
        cen.systems_failed,
        cen.emitters,
        cen.lod_levels,
        cen.modules,
        cen.raw_distributions,
        cen.raw_without_object
    );
    println!("emitter kinds: {:?}", cen.emitter_kinds);
    println!("distribution kinds: {:?}", cen.distribution_kinds);
    println!("module classes:");
    for (k, v) in &cen.module_classes {
        println!("  {k:44} {v}");
    }
    println!("placements per map:");
    for (k, v) in &r.placements {
        println!("  {k:28} {v}");
    }
    println!(
        "placements: {} resolved, {} without template, {} unresolved; bAutoActivate [false, \
         true] = {:?}; component templates outside levels {}",
        r.placements_resolved,
        r.placements_without_template,
        r.placements_unresolved.len(),
        r.auto_activate,
        r.component_templates
    );
    for f in cen.failures.iter().take(20) {
        println!("FAIL {f}");
    }
    for w in c.warnings.iter().take(20) {
        println!("WARN {w}");
    }
}

/// Map package stem → a safe file name (`AG-Workshop.json`).
fn map_file_name(map: &str) -> Option<String> {
    let ok = !map.is_empty()
        && map
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    ok.then(|| format!("{map}.json"))
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, maps_dir, install_root) = install_dirs(ctx)?;
    let dirs = vec![cooked.clone(), maps_dir.clone()];
    let files = package_files(&dirs, &args.packages);
    if files.is_empty() {
        bail!("no package matches the --package filters");
    }
    let converted = convert(&cooked, &maps_dir, &files)?;
    if args.check {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&converted.report)?);
        } else {
            print_report(&converted);
        }
        return Ok(());
    }
    let input = files.first().map(PathBuf::as_path).unwrap_or(&cooked);
    let dir = prepare_dir(&ctx.out.join("particles"), input, &install_root)?;
    let to_bytes = |v: &dyn erased::Json| -> Result<Vec<u8>> {
        if args.pretty { v.pretty() } else { v.compact() }
    };
    let doc = ParticlesFile {
        format: PARTICLES_FORMAT,
        version: PARTICLES_VERSION,
        notice: NOTICE,
        conventions: CONVENTIONS,
        systems: &converted.systems,
        component_templates: &converted.templates,
        coverage: &converted.report,
    };
    let target = safety::check_output_path(&dir.join("particles.json"), input, args.force)?;
    safety::write_output(&target, &to_bytes(&doc)?, args.force)?;
    let maps_out = prepare_dir(&dir.join("maps"), input, &install_root)?;
    let mut written = 0usize;
    for (map, placements) in &converted.maps {
        let Some(name) = map_file_name(map) else {
            eprintln!("skipping map with an unusual name: {map:?}");
            continue;
        };
        let file = PlacementsFile {
            format: PLACEMENTS_FORMAT,
            version: PLACEMENTS_VERSION,
            notice: NOTICE,
            conventions: CONVENTIONS,
            map,
            placements,
        };
        let t = safety::check_output_path(&maps_out.join(name), input, args.force)?;
        safety::write_output(&t, &to_bytes(&file)?, args.force)?;
        written += 1;
    }
    println!(
        "wrote {} particle systems and {} component templates to {}, placements of {written} maps \
         ({} components) to {}",
        converted.systems.len(),
        converted.templates.len(),
        target.display(),
        converted.report.placements.values().sum::<usize>(),
        maps_out.display()
    );
    if !converted.warnings.is_empty() {
        println!("{} warnings (see --check)", converted.warnings.len());
    }
    Ok(())
}

/// Serialization behind a trait object (the two file kinds share the
/// pretty/compact switch).
mod erased {
    use anyhow::Result;
    use serde::Serialize;

    pub trait Json {
        fn pretty(&self) -> Result<Vec<u8>>;
        fn compact(&self) -> Result<Vec<u8>>;
    }

    impl<T: Serialize> Json for T {
        fn pretty(&self) -> Result<Vec<u8>> {
            Ok(serde_json::to_vec_pretty(self)?)
        }
        fn compact(&self) -> Result<Vec<u8>> {
            Ok(serde_json::to_vec(self)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_file_names_are_safe() {
        assert_eq!(
            map_file_name("AG-Workshop").as_deref(),
            Some("AG-Workshop.json")
        );
        assert_eq!(
            map_file_name("Freds_place").as_deref(),
            Some("Freds_place.json")
        );
        assert_eq!(map_file_name("../x"), None);
        assert_eq!(map_file_name(""), None);
        assert_eq!(map_file_name("a/b"), None);
    }

    #[test]
    fn package_kinds() {
        assert!(is_shader_cache(Path::new(
            "/x/RefShaderCache-PC-OpenGL.upk"
        )));
        assert!(!is_shader_cache(Path::new("/x/Startup.upk")));
        assert!(is_map(Path::new("/x/AG-Workshop.asamu")));
        assert!(!is_map(Path::new("/x/Engine.u")));
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
        let ok = prepare_dir(&tmp.path().join("out/particles/maps"), &input, &install);
        assert!(ok.is_ok(), "{ok:?}");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/should-not-exist");
        assert!(prepare_dir(&repo, &input, &install).is_err());
    }

    /// Real data (skips without the install): every placement resolves to a
    /// written system and the coverage is complete.
    #[test]
    fn converts_the_install_when_present() {
        let Ok(install) = asamu_locate::locate() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let dirs = vec![install.cooked_dir.clone(), install.maps_dir.clone()];
        let files = package_files(&dirs, &[]);
        let c = convert(&install.cooked_dir, &install.maps_dir, &files)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            c.report.census.failures.is_empty(),
            "{:?}",
            c.report.census.failures
        );
        assert!(
            c.report.placements_unresolved.is_empty(),
            "{:?}",
            c.report.placements_unresolved
        );
        assert_eq!(c.report.placements_without_template, 0);
        assert_eq!(c.report.placements.values().sum::<usize>(), 162);
        assert_eq!(c.report.auto_activate, [6, 156]);
        assert_eq!(c.report.distinct_systems + c.report.repeated_systems, 110);
        assert_eq!(
            (c.report.distinct_systems, c.report.repeated_systems),
            (100, 10)
        );
        assert_eq!(c.systems.len(), 100);
        assert_eq!(c.templates.len(), 51);
        assert_eq!(
            c.templates.iter().filter(|t| t.template.is_some()).count(),
            30
        );
        assert!(
            c.templates.iter().all(|t| t.in_class_default),
            "all in class default objects"
        );
        let per_map: Vec<(&str, usize)> = c
            .report
            .placements
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(
            per_map,
            vec![
                ("AG-BeautifulCity", 7),
                ("AG-Darkcave", 43),
                ("AG-Epilogue", 19),
                ("AG-IceCave", 22),
                ("AG-ParadiseCave", 25),
                ("AG-StarHaven", 21),
                ("AG-Workshop", 11),
                ("ASAMUEntry", 0),
                ("ASAMUFrontEndMap", 12),
                ("ASAMULegal", 0),
                ("Freds_place", 0),
                ("TheCore", 2),
            ]
        );
        // No placement carries what the runtime would have to honour beyond
        // the template: no instance parameters, no base, one Matinee mover.
        let placed: Vec<&Placement> = c.maps.values().flatten().collect();
        assert!(
            placed
                .iter()
                .all(|p| p.instance_parameters.is_empty() && p.base.is_none())
        );
        assert_eq!(placed.iter().filter(|p| p.moved_by_matinee).count(), 1);
        assert!(placed.iter().all(|p| p.actor_class == "Engine.Emitter"));
        // The component has no kill flags of its own at v868.
        assert!(
            placed
                .iter()
                .all(|p| !p.kill_on_deactivate && !p.kill_on_completed)
        );
        // A system cooked into several packages is the same system: the
        // first copy is the one written, so the copies must agree in
        // everything the runtime reads. (`PeakActiveParticles`, an editor
        // statistic, does differ between the two copies of one system.)
        fn emitters_without_peaks(s: &ParticleSystem) -> serde_json::Value {
            let mut emitters = s.emitters.clone();
            for l in emitters.iter_mut().flat_map(|e| e.lods.iter_mut()) {
                l.peak_active_particles = 0;
            }
            serde_json::to_value(&emitters).unwrap_or_else(|e| panic!("{e}"))
        }
        let cooked = [install.cooked_dir.clone(), install.maps_dir.clone()];
        let mut copies = 0usize;
        for (path, entry) in c.systems.iter().filter(|(_, e)| !e.also_in.is_empty()) {
            let first = emitters_without_peaks(&entry.system);
            for other in &entry.also_in {
                let file = files
                    .iter()
                    .find(|f| f.file_stem().is_some_and(|s| s.to_string_lossy() == *other))
                    .unwrap_or_else(|| panic!("{other}"));
                let set = PackageSet::new(&cooked);
                let lp = set.open_file(file).unwrap_or_else(|e| panic!("{e}"));
                let dec = ParticleDecoder::new(&set);
                let copy = system_exports(&lp.package)
                    .into_iter()
                    .filter_map(|i| dec.system(&lp, i).ok())
                    .find(|s| s.path.eq_ignore_ascii_case(path))
                    .unwrap_or_else(|| panic!("{path} in {other}"));
                assert_eq!(
                    copy.params, entry.system.params,
                    "{path}: system values in {other}"
                );
                assert!(
                    first == emitters_without_peaks(&copy),
                    "{path}: the copy in {other} differs"
                );
                copies += 1;
            }
        }
        assert_eq!(copies, 10);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }
}
