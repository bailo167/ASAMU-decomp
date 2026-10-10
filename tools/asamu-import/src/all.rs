//! `asamu-import all`: every conversion end to end.
//!
//! 1. **Locate** the install (`--original`, `ASAMU_ORIGINAL_DIR`, or Steam
//!    discovery through `asamu-locate`).
//! 2. **Hash** the conversions' input files (the packages and texture file
//!    caches in the cooked folder and its `Maps` folder) with SHA-256. Hashes
//!    are cached by size and modification time in the state file, so only new
//!    or changed files are read again.
//! 3. **Verify** them against the committed inventory
//!    (`docs/reverse-engineering/data/inventory/*.json`, embedded at build
//!    time): missing files refuse the run, different sizes or hashes only warn
//!    (`--no-verify` skips the comparison).
//! 4. **Convert**: textures, meshes (`--collision`), materials, levels, audio,
//!    matinee, skeletal, kismet, lightmaps, particles, decals and
//!    localization, each through its own module's `run` with arguments built
//!    here. A stage whose module is still a stub is reported as unavailable,
//!    not as a failure.
//!
//! # Output (all user-local; never the repository or the install)
//!
//! ```text
//! <out>/<stage>/...               each stage's own output (see the stage modules)
//! <out>/asamu-import-run.json     run manifest (deterministic, see below)
//! <out>/asamu-import-state.json   hash cache and timings of the last run (not deterministic)
//! <out>/.package-cache/           decompressed packages (only with --package-cache)
//! ```
//!
//! The **run manifest** records the importer version (and the SHA-256 of its
//! executable), the install identity (layout, Steam build and depots, cooked
//! folder, a fingerprint over the input files' paths, sizes and hashes), the
//! inventory verification, and per stage its status, arguments, fingerprint
//! and output totals. It holds no timestamps, mtimes or absolute paths (a
//! failed stage's error has the output root, the install root and the home
//! folder replaced by `<out>`, `<install>` and `~`), so the same importer on
//! the same install writes it byte for byte identically and it can be
//! attached to a bug report; timestamps and durations live in the state file.
//!
//! Because every stage folder is replaced wholesale, the output root must be
//! a folder `all` owns: new, empty, holding a run manifest or state file, or
//! holding nothing but stage folders. Any other folder is refused.
//!
//! # Resumability
//!
//! A stage is skipped when the manifest records it as finished (`ok`) with the
//! same fingerprint (importer build + stage + arguments + input fingerprint),
//! its key output file still exists and its folder still holds the recorded
//! number of files and bytes (so a deleted or added file is noticed; a
//! same-size edit is not, `--force` covers that). Otherwise it runs again:
//!
//! Every stage reads only the install and writes only its own folder
//! (`localization` also reads, when present, two Steam metadata files next to
//! the install: the app manifest and the cached stats schema). It runs with `<out>/.staging/` as its output root, and its folder is swapped
//! into place when the stage ends, so an interrupted run never leaves a
//! half-written stage folder behind (the old output stays usable until the
//! new one is complete) and stale files of an older run never linger. A stage
//! that fails after writing its key file (some objects failed, the rest
//! converted) is still swapped in and recorded as `failed`; a stage that
//! writes outside its folder fails and its output is discarded. Because each
//! stage starts from an empty folder, no module needs `--force`.
//!
//! `--force` reruns every selected stage; `--only` / `--skip` select stages.
//!
//! # Package cache
//!
//! `--package-cache` keeps decompressed copies of the packages under
//! `<out>/.package-cache` (bounded by `--package-cache-max-mib`) and points the
//! stages that read nothing but packages at them; see [`pkgcache`] for the
//! measured effect.

mod inventory;
mod pkgcache;
mod sha256;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use asamu_locate::Install;
use serde::{Deserialize, Serialize};

use crate::safety;
pub use inventory::{Inventory, Verification, VerifyStatus};

/// Run manifest file name (in the output root).
pub const RUN_MANIFEST: &str = "asamu-import-run.json";
/// State file name (hash cache + timings).
pub const STATE_FILE: &str = "asamu-import-state.json";
/// Staging folder for stages that are swapped into place.
const STAGING_DIR: &str = ".staging";
/// Where a replaced stage folder waits for its removal.
const REPLACED_DIR: &str = ".replaced";
const MANIFEST_FORMAT: &str = "asamu-import-run";
const MANIFEST_VERSION: u32 = 1;
const STATE_FORMAT: &str = "asamu-import-state";
const STATE_VERSION: u32 = 1;
/// Bumped when the meaning of a stage's arguments or the fingerprint changes.
const PIPELINE_VERSION: &str = "asamu-import-all/1";
const NOTICE: &str = "Converted locally from the user's own copy of A Story About My Uncle. \
     Copyrighted game data: do not redistribute.";

#[derive(clap::Args, Debug, Clone)]
pub struct Args {
    /// Run only these stages (comma-separated or repeated): textures, meshes,
    /// materials, levels, audio, matinee, skeletal, kismet, lightmaps,
    /// particles, decals, localization.
    #[arg(long, value_delimiter = ',', value_parser = parse_stage)]
    only: Vec<StageId>,
    /// Do not run these stages (comma-separated or repeated).
    #[arg(long, value_delimiter = ',', value_parser = parse_stage)]
    skip: Vec<StageId>,
    /// Run every selected stage again, even when its output is up to date.
    #[arg(long)]
    force: bool,
    /// Show the verification and what would run; write nothing.
    #[arg(long)]
    plan: bool,
    /// Do not compare the install with the committed inventory (and do not
    /// refuse when inventoried files are missing).
    #[arg(long)]
    no_verify: bool,
    /// Stop at the first failed stage (default: run the others, fail at the end).
    #[arg(long)]
    fail_fast: bool,
    /// textures (and lightmaps): also write PNG previews.
    #[arg(long)]
    png: bool,
    /// audio: language of the localized audio and subtitles (INT, DEU, FRA, ...).
    #[arg(long, default_value = "INT")]
    lang: String,
    /// Keep decompressed packages in <out>/.package-cache and let the stages
    /// that read only packages use them (about 1.5 GB for the Mac build; saves
    /// about 3 s per forced re-import).
    #[arg(long)]
    package_cache: bool,
    /// Upper bound of the package cache in MiB; when the packages need more,
    /// the cache is removed and not used.
    #[arg(long, default_value_t = 2048)]
    package_cache_max_mib: u64,
}

/// The conversions, in run order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageId {
    Textures,
    Meshes,
    Materials,
    Levels,
    Audio,
    Matinee,
    Skeletal,
    Kismet,
    Lightmaps,
    Particles,
    Decals,
    Localization,
}

impl StageId {
    /// Every stage in run order.
    pub const ALL: [StageId; 12] = [
        StageId::Textures,
        StageId::Meshes,
        StageId::Materials,
        StageId::Levels,
        StageId::Audio,
        StageId::Matinee,
        StageId::Skeletal,
        StageId::Kismet,
        StageId::Lightmaps,
        StageId::Particles,
        StageId::Decals,
        StageId::Localization,
    ];

    /// Subcommand / folder name.
    pub fn name(self) -> &'static str {
        match self {
            StageId::Textures => "textures",
            StageId::Meshes => "meshes",
            StageId::Materials => "materials",
            StageId::Levels => "levels",
            StageId::Audio => "audio",
            StageId::Matinee => "matinee",
            StageId::Skeletal => "skeletal",
            StageId::Kismet => "kismet",
            StageId::Lightmaps => "lightmaps",
            StageId::Particles => "particles",
            StageId::Decals => "decals",
            StageId::Localization => "localization",
        }
    }
}

fn parse_stage(s: &str) -> Result<StageId, String> {
    StageId::ALL
        .into_iter()
        .find(|id| id.name().eq_ignore_ascii_case(s.trim()))
        .ok_or_else(|| {
            let names: Vec<&str> = StageId::ALL.iter().map(|i| i.name()).collect();
            format!("unknown stage {s:?} (one of {})", names.join(", "))
        })
}

/// Runs one stage's module with an argument vector.
pub type Runner = Box<dyn Fn(&crate::Ctx, &[String]) -> Result<()>>;

/// One conversion. Its module must read only the install and write only
/// `<out>/<dir>/` (the staged runner checks the second part).
pub struct Stage {
    /// Which conversion.
    pub id: StageId,
    /// Output folder under the converted root.
    pub dir: &'static str,
    /// File (inside `dir`) whose presence marks a finished stage; `None`:
    /// any entry in `dir`.
    pub key_file: Option<&'static str>,
    /// Reads nothing but packages (may use the package cache).
    pub packages_only: bool,
    /// Canonical arguments (part of the fingerprint).
    pub args: Vec<String>,
    /// The module entry point.
    pub runner: Runner,
}

/// Parse a module's `Args` from an argument vector, exactly as the
/// subcommand would (so the other modules' private fields stay private).
pub fn parse_stage_args<A: clap::Args + clap::FromArgMatches>(argv: &[String]) -> Result<A> {
    let cmd = A::augment_args(clap::Command::new("stage").no_binary_name(true));
    let matches = cmd
        .try_get_matches_from(argv)
        .map_err(|e| anyhow!("invalid stage arguments {argv:?}: {e}"))?;
    A::from_arg_matches(&matches).map_err(|e| anyhow!("invalid stage arguments {argv:?}: {e}"))
}

/// Does the module's `Args` define `--<long>`?
pub fn supports_flag<A: clap::Args>(long: &str) -> bool {
    A::augment_args(clap::Command::new("stage"))
        .get_arguments()
        .any(|a| a.get_long() == Some(long))
}

fn runner<A>(f: fn(&crate::Ctx, A) -> Result<()>) -> Runner
where
    A: clap::Args + clap::FromArgMatches + 'static,
{
    Box::new(move |ctx, argv| f(ctx, parse_stage_args::<A>(argv)?))
}

fn stage<A>(
    id: StageId,
    key_file: Option<&'static str>,
    packages_only: bool,
    args: Vec<String>,
    f: fn(&crate::Ctx, A) -> Result<()>,
) -> Stage
where
    A: clap::Args + clap::FromArgMatches + 'static,
{
    Stage {
        id,
        dir: id.name(),
        key_file,
        packages_only,
        args,
        runner: runner(f),
    }
}

/// The real conversions with the arguments `all` gives them.
pub fn real_stages(args: &Args) -> Vec<Stage> {
    let png = if args.png {
        vec!["--png".to_owned()]
    } else {
        Vec::new()
    };
    let manifest = Some("manifest.json");
    vec![
        stage(
            StageId::Textures,
            manifest,
            false,
            png,
            crate::textures::run,
        ),
        stage(
            StageId::Meshes,
            manifest,
            true,
            vec!["--collision".to_owned()],
            crate::meshes::run,
        ),
        stage(
            StageId::Materials,
            Some("materials.json"),
            true,
            Vec::new(),
            crate::materials::run,
        ),
        stage(
            StageId::Levels,
            manifest,
            true,
            Vec::new(),
            crate::levels::run,
        ),
        stage(
            StageId::Audio,
            manifest,
            true,
            vec!["--lang".to_owned(), args.lang.clone()],
            crate::audio::run,
        ),
        stage(
            StageId::Matinee,
            manifest,
            true,
            Vec::new(),
            crate::matinee::run,
        ),
        stage(
            StageId::Skeletal,
            manifest,
            true,
            Vec::new(),
            crate::skeletal::run,
        ),
        // Reads only the install and writes only `kismet/` (module docs).
        // Kept on the original install rather than the package cache.
        stage(
            StageId::Kismet,
            manifest,
            false,
            Vec::new(),
            crate::kismet::run,
        ),
        // Reads only the install (packages and texture caches) and writes
        // only `lightmaps/` (module docs), so it is staged too.
        stage(
            StageId::Lightmaps,
            None,
            false,
            if args.png && supports_flag::<crate::lightmaps::Args>("png") {
                vec!["--png".to_owned()]
            } else {
                Vec::new()
            },
            crate::lightmaps::run,
        ),
        // The three below read the install directly (packages; decals also
        // the texture caches for their masks) and write only their own folder.
        stage(
            StageId::Particles,
            Some("particles.json"),
            false,
            Vec::new(),
            crate::particles::run,
        ),
        stage(
            StageId::Decals,
            manifest,
            false,
            Vec::new(),
            crate::decals::run,
        ),
        // Every language of the install. Also reads Steam's app manifest and
        // cached stats schema when they are there (default language,
        // achievement names); without them it converts the install's text.
        stage(
            StageId::Localization,
            manifest,
            false,
            Vec::new(),
            crate::localization::run,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// `asamu-import-run.json` (deterministic).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunManifest {
    /// `"asamu-import-run"`.
    pub format: String,
    /// Format version.
    pub version: u32,
    /// Origin notice.
    pub notice: String,
    /// The importer.
    pub tool: ToolInfo,
    /// The install the data came from.
    pub install: InstallRecord,
    /// Inventory comparison.
    pub verification: Verification,
    /// Stages in run order (stages never run are absent).
    pub stages: Vec<StageRecord>,
}

/// The importer that wrote a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// `"asamu-import"`.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// SHA-256 of the running executable (changes with every build).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_sha256: Option<String>,
}

impl ToolInfo {
    /// This executable.
    pub fn current() -> ToolInfo {
        ToolInfo {
            name: "asamu-import".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build_sha256: std::env::current_exe()
                .ok()
                .and_then(|p| sha256::file_hex(&p).ok()),
        }
    }
}

/// Install identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// Layout label (`mac-app`, `windows`, `linux`, `loose`).
    pub layout: String,
    /// Steam App ID.
    pub app_id: u32,
    /// Steam build id, when an app manifest was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<u64>,
    /// Installed depots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depots: Vec<DepotRecord>,
    /// Cooked folder name.
    pub cooked_dir: String,
    /// The input files.
    pub inputs: InputsRecord,
}

/// One installed depot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepotRecord {
    /// Depot id.
    pub depot_id: u32,
    /// Depot manifest id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_id: Option<String>,
}

/// Summary of the input files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputsRecord {
    /// Number of files.
    pub files: usize,
    /// Total bytes.
    pub bytes: u64,
    /// SHA-256 over the sorted `path \t size \t sha256 \n` lines (paths
    /// relative to the cooked folder).
    pub sha256: String,
}

/// Status of a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageStatus {
    /// Finished without errors.
    Ok,
    /// Finished with errors (output may be partial).
    Failed,
    /// The module is not implemented in this build.
    Unavailable,
    /// Started and not finished (an interrupted run).
    Running,
}

impl StageStatus {
    fn label(self) -> &'static str {
        match self {
            StageStatus::Ok => "ok",
            StageStatus::Failed => "FAILED",
            StageStatus::Unavailable => "unavailable",
            StageStatus::Running => "running",
        }
    }
}

/// File count, bytes and content digest of a stage folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTotals {
    /// Files.
    pub files: u64,
    /// Bytes.
    pub bytes: u64,
    /// SHA-256 over the sorted `path \t size \t sha256 \n` lines of every
    /// file (paths relative to the stage folder): equal digests mean
    /// byte-identical output trees.
    pub sha256: String,
}

/// One stage in the run manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageRecord {
    /// Stage name.
    pub name: String,
    /// Status of the last attempt.
    pub status: StageStatus,
    /// Arguments the module was given (without `--force`).
    pub args: Vec<String>,
    /// Fingerprint of importer build + stage + arguments + inputs.
    pub fingerprint: String,
    /// Totals of the stage folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<OutputTotals>,
    /// Error of a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `asamu-import-state.json` (machine-local, not deterministic).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct State {
    format: String,
    version: u32,
    /// Install root the hashes belong to.
    install_root: String,
    /// Hash cache, sorted by path.
    hashes: Vec<HashEntry>,
    /// The last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<RunLog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HashEntry {
    /// Path relative to the cooked folder.
    path: String,
    size: u64,
    mtime_ns: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RunLog {
    started_unix_s: u64,
    finished_unix_s: u64,
    seconds: f64,
    hashing_seconds: f64,
    hashed_files: usize,
    #[serde(default)]
    output_digest_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_cache: Option<CacheLog>,
    stages: Vec<StageLog>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CacheLog {
    used: bool,
    reused: usize,
    written: usize,
    bytes: u64,
    seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StageLog {
    name: String,
    /// `ran`, `up-to-date`, `not-selected` or `not-run-fail-fast`.
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<StageStatus>,
    seconds: f64,
    package_cache: bool,
}

/// One input file of the conversions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    /// Path relative to the cooked folder, `/`-separated (`Core.u`, `Maps/X.asamu`).
    pub cooked_rel: String,
    /// Full path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Modification time in ns since the Unix epoch (0 when unknown).
    pub mtime_ns: u64,
    /// SHA-256, lower-case hex.
    pub sha256: String,
}

/// What a stage did in this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ran (with the resulting status).
    Ran(StageStatus),
    /// Skipped: finished earlier with the same fingerprint.
    UpToDate,
    /// Would run (`--plan`), with the reason.
    WouldRun(String),
}

/// Result of [`execute`].
#[derive(Debug, Clone)]
pub struct Report {
    /// The manifest as written (or as it would be, with `--plan`).
    pub manifest: RunManifest,
    /// Per selected stage, what happened.
    pub actions: Vec<(StageId, Action)>,
}

impl Report {
    /// Names of the stages that failed in this run.
    pub fn failed(&self) -> Vec<&'static str> {
        self.actions
            .iter()
            .filter(|(_, a)| *a == Action::Ran(StageStatus::Failed))
            .map(|(id, _)| id.name())
            .collect()
    }
}

/// Everything [`execute`] needs besides the stages.
pub struct Env<'a> {
    /// The located install.
    pub install: &'a Install,
    /// Output root as given.
    pub out: &'a Path,
    /// Committed inventories.
    pub inventories: &'a [Inventory],
    /// The importer.
    pub tool: ToolInfo,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let install = match &ctx.original {
        Some(dir) => asamu_locate::from_original_dir(dir)?,
        None => asamu_locate::locate()?,
    };
    let inventories = Inventory::embedded()?;
    let env = Env {
        install: &install,
        out: &ctx.out,
        inventories: &inventories,
        tool: ToolInfo::current(),
    };
    let report = execute(&env, real_stages(&args), &args)?;
    if report.manifest.verification.status == VerifyStatus::Mismatch {
        println!(
            "note: this install differs from the verified build; when reporting a conversion \
             problem, include {RUN_MANIFEST} (it holds no personal paths)"
        );
    }
    let failed = report.failed();
    if !failed.is_empty() {
        bail!("{} stage(s) failed: {}", failed.len(), failed.join(", "));
    }
    Ok(())
}

/// Run (or with `--plan`, describe) the pipeline.
pub fn execute(env: &Env<'_>, stages: Vec<Stage>, args: &Args) -> Result<Report> {
    let run_start = Instant::now();
    let started_unix_s = unix_now();
    let install = env.install;
    println!(
        "asamu-import all: {} install{} at {}",
        install.layout.label(),
        install
            .build_id
            .map(|b| format!(" (Steam build {b})"))
            .unwrap_or_default(),
        install.root.display()
    );
    for w in &install.warnings {
        println!("  warning: {w}");
    }

    let out = resolve_out(env.out, &install.root, !args.plan)?;
    println!("  output: {}", out.display());
    // `all` replaces whole stage folders: never in a folder holding other data.
    check_output_root_is_ours(&out, &stages)?;
    let redactions = path_redactions(&out, &install.root);

    // Inputs and their hashes.
    let raw = gather_inputs(install)?;
    if raw.is_empty() {
        bail!(
            "no packages found in {} (is this the game install?)",
            install.cooked_dir.display()
        );
    }
    let previous_state = read_json::<State>(&out.join(STATE_FILE)).unwrap_or_default();
    let root_key = install.root.display().to_string();
    let cache: BTreeMap<String, HashEntry> = if previous_state.install_root == root_key {
        previous_state
            .hashes
            .iter()
            .map(|h| (h.path.clone(), h.clone()))
            .collect()
    } else {
        BTreeMap::new()
    };
    let hash_start = Instant::now();
    let (inputs, hashed) = hash_inputs(raw, &cache)?;
    let hashing_seconds = hash_start.elapsed().as_secs_f64();
    let input_bytes: u64 = inputs.iter().map(|f| f.size).sum();
    println!(
        "  inputs: {} files, {} ({} hashed now in {:.1} s, {} from the hash cache)",
        inputs.len(),
        fmt_bytes(input_bytes),
        hashed,
        hashing_seconds,
        inputs.len() - hashed
    );
    let hash_entries: Vec<HashEntry> = inputs
        .iter()
        .map(|f| HashEntry {
            path: f.cooked_rel.clone(),
            size: f.size,
            mtime_ns: f.mtime_ns,
            sha256: f.sha256.clone(),
        })
        .collect();
    if !args.plan && hashed > 0 {
        // Keep the new hashes even if this run is interrupted.
        let early = State {
            format: STATE_FORMAT.to_owned(),
            version: STATE_VERSION,
            install_root: root_key.clone(),
            hashes: hash_entries.clone(),
            last_run: previous_state.last_run.clone(),
        };
        write_json(&out, STATE_FILE, &early, &install.root)?;
    }

    // Verification.
    let cooked_name = dir_name(&install.cooked_dir);
    let verification = if args.no_verify {
        Verification::skipped()
    } else {
        let disk: Vec<inventory::DiskFile> = inputs
            .iter()
            .map(|f| inventory::DiskFile {
                cooked_rel: f.cooked_rel.clone(),
                size: f.size,
                sha256: f.sha256.clone(),
            })
            .collect();
        inventory::verify(env.inventories, &cooked_name, install.build_id, &disk)
    };
    print_verification(&verification);
    if verification.status == VerifyStatus::MissingFiles {
        bail!(
            "{} inventoried input file(s) are missing from {}: {}. Repair the install (Steam: \
             Properties > Installed Files > Verify integrity) or pass --no-verify to convert \
             what is there",
            verification.missing.len(),
            install.cooked_dir.display(),
            verification.missing.join(", ")
        );
    }

    let install_record = InstallRecord {
        layout: install.layout.label().to_owned(),
        app_id: install.app_id,
        build_id: install.build_id,
        depots: install
            .depots
            .iter()
            .map(|d| DepotRecord {
                depot_id: d.depot_id,
                manifest_id: d.manifest_id.map(|m| m.to_string()),
            })
            .collect(),
        cooked_dir: cooked_name.clone(),
        inputs: InputsRecord {
            files: inputs.len(),
            bytes: input_bytes,
            sha256: inputs_fingerprint(&inputs),
        },
    };

    // Previous manifest and decisions.
    let previous = read_json::<RunManifest>(&out.join(RUN_MANIFEST))
        .filter(|m| m.format == MANIFEST_FORMAT && m.version == MANIFEST_VERSION);
    let mut records: BTreeMap<StageId, StageRecord> = BTreeMap::new();
    if let Some(prev) = &previous {
        for r in &prev.stages {
            if let Ok(id) = parse_stage(&r.name) {
                records.insert(id, r.clone());
            }
        }
    }
    let selected: Vec<&Stage> = stages
        .iter()
        .filter(|s| args.only.is_empty() || args.only.contains(&s.id))
        .filter(|s| !args.skip.contains(&s.id))
        .collect();
    let mut decisions = Vec::with_capacity(selected.len());
    for s in &selected {
        let fp = stage_fingerprint(&env.tool, s, &install_record.inputs.sha256);
        let found = OutputsFound {
            key_present: outputs_present(&out, s),
            totals: {
                let dir = out.join(s.dir);
                dir_exists(&dir).then(|| dir_totals(&dir))
            },
        };
        let d = decide(records.get(&s.id), &fp, &found, args.force);
        decisions.push((*s, fp, d));
    }

    let mut manifest = RunManifest {
        format: MANIFEST_FORMAT.to_owned(),
        version: MANIFEST_VERSION,
        notice: NOTICE.to_owned(),
        tool: env.tool.clone(),
        install: install_record,
        verification,
        stages: Vec::new(),
    };

    if args.plan {
        let total = decisions.len();
        let mut actions = Vec::new();
        for (i, (s, _, d)) in decisions.iter().enumerate() {
            let what = match d {
                Decision::UpToDate => {
                    actions.push((s.id, Action::UpToDate));
                    "up to date".to_owned()
                }
                Decision::Run(reason) => {
                    actions.push((s.id, Action::WouldRun(reason.clone())));
                    format!("would run ({reason})")
                }
            };
            println!("[{}/{total}] {:<10} {what}", i + 1, s.id.name());
        }
        manifest.stages = ordered_records(&records);
        println!("plan only: nothing written");
        return Ok(Report { manifest, actions });
    }

    clean_leftovers(&out)?;

    // Package cache.
    let wants_cache = decisions
        .iter()
        .any(|(s, _, d)| matches!(d, Decision::Run(_)) && s.packages_only);
    let mut cache_log = None;
    let mut cache_root: Option<PathBuf> = None;
    if args.package_cache && wants_cache {
        let limit = args.package_cache_max_mib.saturating_mul(1024 * 1024);
        let maps_name = dir_name(&install.maps_dir);
        // Streams written by another importer build (another decompressor)
        // are not reused.
        let writer = format!(
            "{}+{}",
            env.tool.version,
            env.tool.build_sha256.as_deref().unwrap_or("-")
        );
        match pkgcache::prepare(&out, &inputs, &cooked_name, &maps_name, limit, &writer) {
            Ok(pkgcache::Outcome::Ready(p)) => {
                println!(
                    "  package cache: {} ({} reused, {} decompressed now, {:.1} s)",
                    fmt_bytes(p.bytes),
                    p.reused,
                    p.written,
                    p.seconds
                );
                cache_log = Some(CacheLog {
                    used: true,
                    reused: p.reused,
                    written: p.written,
                    bytes: p.bytes,
                    seconds: p.seconds,
                    note: None,
                });
                cache_root = Some(p.root);
            }
            Ok(pkgcache::Outcome::TooLarge { needed, limit }) => {
                let note = format!(
                    "the packages need {} but --package-cache-max-mib allows {}; cache removed, \
                     not used",
                    fmt_bytes(needed),
                    fmt_bytes(limit)
                );
                println!("  package cache: {note}");
                cache_log = Some(CacheLog {
                    used: false,
                    reused: 0,
                    written: 0,
                    bytes: 0,
                    seconds: 0.0,
                    note: Some(note),
                });
            }
            Err(e) => {
                let note = format!("not used: {e:#}");
                println!("  package cache: {note}");
                cache_log = Some(CacheLog {
                    used: false,
                    reused: 0,
                    written: 0,
                    bytes: 0,
                    seconds: 0.0,
                    note: Some(note),
                });
            }
        }
    } else if !args.package_cache {
        let bytes = pkgcache::existing_bytes(&out);
        if bytes > 0 {
            println!(
                "  note: {} holds {} of decompressed packages that this run does not use \
                 (pass --package-cache, or delete the folder)",
                out.join(pkgcache::DIR).display(),
                fmt_bytes(bytes)
            );
        }
    }

    // Stages.
    let total = decisions.len();
    let mut actions = Vec::with_capacity(total);
    let mut logs = Vec::with_capacity(StageId::ALL.len());
    let mut stop = false;
    for (i, (s, fp, d)) in decisions.iter().enumerate() {
        let n = i + 1;
        let reason = match d {
            Decision::UpToDate => {
                println!("[{n}/{total}] {:<10} up to date", s.id.name());
                actions.push((s.id, Action::UpToDate));
                logs.push(StageLog {
                    name: s.id.name().to_owned(),
                    action: "up-to-date".to_owned(),
                    status: records.get(&s.id).map(|r| r.status),
                    seconds: 0.0,
                    package_cache: false,
                });
                continue;
            }
            Decision::Run(_) if stop => {
                println!("[{n}/{total}] {:<10} not run (--fail-fast)", s.id.name());
                logs.push(StageLog {
                    name: s.id.name().to_owned(),
                    action: "not-run-fail-fast".to_owned(),
                    status: None,
                    seconds: 0.0,
                    package_cache: false,
                });
                continue;
            }
            Decision::Run(reason) => reason,
        };
        let use_cache = s.packages_only && cache_root.is_some();
        let original = match (&cache_root, use_cache) {
            (Some(c), true) => c.clone(),
            _ => install.root.clone(),
        };
        println!(
            "[{n}/{total}] {:<10} running ({reason}){}",
            s.id.name(),
            if use_cache { " [package cache]" } else { "" }
        );
        records.insert(
            s.id,
            StageRecord {
                name: s.id.name().to_owned(),
                status: StageStatus::Running,
                args: s.args.clone(),
                fingerprint: fp.clone(),
                outputs: None,
                error: None,
            },
        );
        manifest.stages = ordered_records(&records);
        write_json(&out, RUN_MANIFEST, &manifest, &install.root)?;

        let t = Instant::now();
        let (status, error) = match run_staged(&out, s, &original) {
            Ok(r) => r,
            Err(e) => (StageStatus::Failed, Some(format!("{e:#}"))),
        };
        let seconds = t.elapsed().as_secs_f64();
        let totals = dir_exists(&out.join(s.dir)).then(|| dir_totals(&out.join(s.dir)));
        println!(
            "[{n}/{total}] {:<10} {} in {seconds:.1} s{}",
            s.id.name(),
            status.label(),
            totals
                .map(|(f, b)| format!(", {f} files, {}", fmt_bytes(b)))
                .unwrap_or_default()
        );
        if let Some(e) = &error {
            println!("    {e}");
        }
        if status == StageStatus::Failed && args.fail_fast {
            stop = true;
        }
        records.insert(
            s.id,
            StageRecord {
                name: s.id.name().to_owned(),
                status,
                args: s.args.clone(),
                fingerprint: fp.clone(),
                outputs: None,
                // The manifest is meant to be shared in bug reports: no
                // absolute paths (module errors name their files).
                error: error.map(|e| redact_paths(&e, &redactions)),
            },
        );
        manifest.stages = ordered_records(&records);
        write_json(&out, RUN_MANIFEST, &manifest, &install.root)?;
        actions.push((s.id, Action::Ran(status)));
        logs.push(StageLog {
            name: s.id.name().to_owned(),
            action: "ran".to_owned(),
            status: Some(status),
            seconds,
            package_cache: use_cache,
        });
    }

    // Final totals and digests. A stage that did not run keeps its recorded
    // digest while its folder still has the recorded file count and size.
    let ran_now: Vec<StageId> = actions
        .iter()
        .filter(|(_, a)| matches!(a, Action::Ran(_)))
        .map(|(id, _)| *id)
        .collect();
    let digest_start = Instant::now();
    for (id, r) in records.iter_mut() {
        let dir = out.join(id.name());
        if !dir_exists(&dir) {
            r.outputs = None;
            continue;
        }
        let (files, bytes) = dir_totals(&dir);
        let keep = !ran_now.contains(id)
            && r.outputs
                .as_ref()
                .is_some_and(|o| o.files == files && o.bytes == bytes);
        if !keep {
            r.outputs = Some(tree_digest(&dir)?);
        }
    }
    let digest_seconds = digest_start.elapsed().as_secs_f64();
    manifest.stages = ordered_records(&records);
    write_json(&out, RUN_MANIFEST, &manifest, &install.root)?;

    // State: hash cache + timings.
    for id in StageId::ALL {
        if !logs.iter().any(|l| l.name == id.name()) {
            logs.push(StageLog {
                name: id.name().to_owned(),
                action: "not-selected".to_owned(),
                status: None,
                seconds: 0.0,
                package_cache: false,
            });
        }
    }
    let state = State {
        format: STATE_FORMAT.to_owned(),
        version: STATE_VERSION,
        install_root: root_key,
        hashes: hash_entries,
        last_run: Some(RunLog {
            started_unix_s,
            finished_unix_s: unix_now(),
            seconds: run_start.elapsed().as_secs_f64(),
            hashing_seconds,
            hashed_files: hashed,
            output_digest_seconds: digest_seconds,
            package_cache: cache_log,
            stages: logs,
        }),
    };
    write_json(&out, STATE_FILE, &state, &install.root)?;

    print_summary(&manifest, &actions, &state);
    println!(
        "run manifest: {}\nnote: converted data is copyrighted game data; keep it local, never \
         redistribute",
        out.join(RUN_MANIFEST).display()
    );
    Ok(Report { manifest, actions })
}

// ---------------------------------------------------------------------------
// Decisions and fingerprints
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    UpToDate,
    Run(String),
}

/// What is in a stage folder now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputsFound {
    /// The stage's key file (or, without one, any entry) exists.
    key_present: bool,
    /// Files and bytes in the folder (`None`: no folder).
    totals: Option<(u64, u64)>,
}

fn decide(
    prev: Option<&StageRecord>,
    fingerprint: &str,
    found: &OutputsFound,
    force: bool,
) -> Decision {
    let run = |r: &str| Decision::Run(r.to_owned());
    if force {
        return run("--force");
    }
    // Files deleted, added or resized since the run that recorded the totals
    // (a deleted texture would otherwise stay missing behind an existing
    // manifest). Same-size content edits are not detected: use --force.
    let changed = |p: &StageRecord| {
        p.outputs
            .as_ref()
            .is_some_and(|o| found.totals != Some((o.files, o.bytes)))
    };
    match prev {
        None if found.key_present => run("output without a run record; regenerating"),
        None => run("not converted yet"),
        Some(p) => match p.status {
            StageStatus::Running => run("an earlier run was interrupted"),
            StageStatus::Failed => run("failed last time"),
            StageStatus::Unavailable => run("was unavailable last time"),
            StageStatus::Ok if p.fingerprint != fingerprint => {
                run("inputs, arguments or importer changed")
            }
            StageStatus::Ok if !found.key_present => run("output missing"),
            StageStatus::Ok if changed(p) => run("output files changed since the last run"),
            StageStatus::Ok => Decision::UpToDate,
        },
    }
}

fn stage_fingerprint(tool: &ToolInfo, stage: &Stage, inputs_sha256: &str) -> String {
    let mut h = sha256::Sha256::new();
    for part in [
        PIPELINE_VERSION,
        tool.name.as_str(),
        tool.version.as_str(),
        tool.build_sha256.as_deref().unwrap_or("-"),
        stage.id.name(),
        inputs_sha256,
    ] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    for a in &stage.args {
        h.update(a.as_bytes());
        h.update(&[0x1f]);
    }
    h.finalize_hex()
}

fn inputs_fingerprint(inputs: &[InputFile]) -> String {
    let mut h = sha256::Sha256::new();
    for f in inputs {
        h.update(format!("{}\t{}\t{}\n", f.cooked_rel, f.size, f.sha256).as_bytes());
    }
    h.finalize_hex()
}

fn ordered_records(records: &BTreeMap<StageId, StageRecord>) -> Vec<StageRecord> {
    // BTreeMap over StageId iterates in run order.
    records.values().cloned().collect()
}

fn outputs_present(out: &Path, stage: &Stage) -> bool {
    let dir = out.join(stage.dir);
    match stage.key_file {
        Some(f) => std::fs::symlink_metadata(dir.join(f)).is_ok_and(|m| m.is_file()),
        None => std::fs::read_dir(&dir).is_ok_and(|mut rd| rd.next().is_some()),
    }
}

// ---------------------------------------------------------------------------
// Running a stage
// ---------------------------------------------------------------------------

/// The scaffold stub's error: a bare `"<stage> is not implemented yet"`
/// (no context chain). A real conversion error that merely mentions an
/// unimplemented feature somewhere in its chain stays a failure.
fn is_stub_error(e: &anyhow::Error) -> bool {
    e.chain().count() == 1 && e.to_string().trim_end().ends_with("not implemented yet")
}

/// Entries `all` itself creates in the output root (plus files the desktop
/// adds when a folder is opened).
fn is_known_root_entry(name: &str) -> bool {
    const OS_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];
    StageId::ALL.iter().any(|s| s.name() == name)
        || [
            RUN_MANIFEST,
            STATE_FILE,
            STAGING_DIR,
            REPLACED_DIR,
            pkgcache::DIR,
        ]
        .contains(&name)
        || OS_FILES.contains(&name)
        || name.starts_with(&format!(".{RUN_MANIFEST}."))
        || name.starts_with(&format!(".{STATE_FILE}."))
}

/// Does `<out>/<name>` (a folder named like a stage, in a root without a run
/// record) look like converter output: an empty folder, or one holding the
/// stage's key file, or (stages without one) a top-level JSON file?
fn looks_like_stage_output(dir: &Path, key_file: Option<&str>) -> bool {
    if !dir_exists(dir) {
        return false; // a file or a symbolic link with a stage's name
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    if entries.is_empty() {
        return true;
    }
    match key_file {
        Some(f) => std::fs::symlink_metadata(dir.join(f)).is_ok_and(|m| m.is_file()),
        None => entries.iter().any(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.file_name().to_string_lossy().ends_with(".json")
        }),
    }
}

/// Refuse an output root that holds data `all` did not write: every stage
/// folder is replaced wholesale, so `--out ~/Documents` must not delete a
/// `~/Documents/audio` folder. A root with a run manifest or state file is
/// ours; otherwise every entry must be one `all` (or a single-stage command)
/// creates, and every stage-named folder must look like converter output.
fn check_output_root_is_ours(out: &Path, stages: &[Stage]) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(out) else {
        return Ok(()); // not created yet (--plan)
    };
    if [RUN_MANIFEST, STATE_FILE]
        .iter()
        .any(|f| std::fs::symlink_metadata(out.join(f)).is_ok_and(|m| m.is_file()))
    {
        return Ok(());
    }
    let mut foreign: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| match parse_stage(n) {
            Ok(id) if id.name() == n => {
                let key = stages
                    .iter()
                    .find(|s| s.id == id)
                    .map_or(Some("manifest.json"), |s| s.key_file);
                !looks_like_stage_output(&out.join(n), key)
            }
            _ => !is_known_root_entry(n),
        })
        .collect();
    if foreign.is_empty() {
        return Ok(());
    }
    foreign.sort();
    let more = foreign.len().saturating_sub(5);
    foreign.truncate(5);
    bail!(
        "{} already holds other files ({}{}); `asamu-import all` replaces its stage folders, so \
         choose a new or empty output folder with --out",
        out.display(),
        foreign.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    )
}

/// (path prefix, placeholder) pairs for [`redact_paths`], longest first.
fn path_redactions(out: &Path, install_root: &Path) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = vec![
        (out.display().to_string(), "<out>"),
        (install_root.display().to_string(), "<install>"),
    ];
    if let Ok(c) = install_root.canonicalize() {
        v.push((c.display().to_string(), "<install>"));
    }
    if let Ok(c) = out.canonicalize() {
        v.push((c.display().to_string(), "<out>"));
    }
    for var in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(var) {
            v.push((PathBuf::from(h).display().to_string(), "~"));
        }
    }
    // Never a bare root ("/", "C:\") or an empty prefix.
    v.retain(|(p, _)| p.trim_end_matches(['/', '\\']).len() > 3);
    v.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

/// Replace the absolute paths of `redactions` in `msg` by placeholders.
fn redact_paths(msg: &str, redactions: &[(String, &'static str)]) -> String {
    let mut s = msg.to_owned();
    for (prefix, placeholder) in redactions {
        s = s.replace(prefix.as_str(), placeholder);
    }
    s
}

fn run_staged(out: &Path, stage: &Stage, original: &Path) -> Result<(StageStatus, Option<String>)> {
    let staging = out.join(STAGING_DIR);
    remove_tree(&staging)?;
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    let ctx = crate::Ctx {
        original: Some(original.to_path_buf()),
        out: staging.clone(),
    };
    let result = (stage.runner)(&ctx, &stage.args);
    let produced = staging.join(stage.dir);
    let unexpected: Vec<String> = std::fs::read_dir(&staging)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != stage.dir)
                .collect()
        })
        .unwrap_or_default();
    if !unexpected.is_empty() {
        remove_tree(&staging)?;
        let mut msg = format!(
            "the stage wrote outside its folder {}/ ({}); output discarded",
            stage.dir,
            unexpected.join(", ")
        );
        if let Err(e) = result {
            msg.push_str(&format!("; stage error: {e:#}"));
        }
        return Ok((StageStatus::Failed, Some(msg)));
    }
    let key_present = match stage.key_file {
        Some(f) => produced.join(f).is_file(),
        None => std::fs::read_dir(&produced).is_ok_and(|mut rd| rd.next().is_some()),
    };
    let outcome = match result {
        Ok(()) => {
            if dir_exists(&produced) {
                swap_in(out, stage.dir, &produced)?;
            } else {
                // Finished with no output: the old output is stale too.
                remove_tree(&out.join(stage.dir))?;
            }
            (StageStatus::Ok, None)
        }
        Err(e) if is_stub_error(&e) => (StageStatus::Unavailable, Some(format!("{e:#}"))),
        Err(e) => {
            let mut msg = format!("{e:#}");
            if key_present {
                swap_in(out, stage.dir, &produced)?;
                msg.push_str(" (partial output kept)");
            }
            (StageStatus::Failed, Some(msg))
        }
    };
    remove_tree(&staging)?;
    Ok(outcome)
}

/// Replace `<out>/<dir>` with `produced`.
fn swap_in(out: &Path, dir: &str, produced: &Path) -> Result<()> {
    let live = out.join(dir);
    if std::fs::symlink_metadata(&live).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!(
            "{} is a symbolic link; refusing to replace it (make it a folder, or run `asamu-import \
             {dir}` directly)",
            live.display()
        );
    }
    let replaced = out.join(REPLACED_DIR);
    remove_tree(&replaced)?;
    if std::fs::symlink_metadata(&live).is_ok() {
        std::fs::create_dir_all(&replaced)
            .with_context(|| format!("creating {}", replaced.display()))?;
        std::fs::rename(&live, replaced.join(dir))
            .with_context(|| format!("moving the old {} aside", live.display()))?;
    }
    std::fs::rename(produced, &live)
        .with_context(|| format!("moving the new output to {}", live.display()))?;
    remove_tree(&replaced)
}

/// Remove `p` (a folder this command created, or a stale entry). A symbolic
/// link is removed itself, never followed.
fn remove_tree(p: &Path) -> Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))
        }
        Ok(_) => std::fs::remove_file(p).with_context(|| format!("removing {}", p.display())),
        Err(_) => Ok(()),
    }
}

fn clean_leftovers(out: &Path) -> Result<()> {
    remove_tree(&out.join(STAGING_DIR))?;
    remove_tree(&out.join(REPLACED_DIR))
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

struct RawInput {
    cooked_rel: String,
    path: PathBuf,
    size: u64,
    mtime_ns: u64,
}

fn gather_inputs(install: &Install) -> Result<Vec<RawInput>> {
    let maps_prefix = install
        .maps_dir
        .strip_prefix(&install.cooked_dir)
        .ok()
        .map(rel_string)
        .unwrap_or_else(|| dir_name(&install.maps_dir));
    let mut out = Vec::new();
    for (dir, prefix) in [
        (&install.cooked_dir, String::new()),
        (&install.maps_dir, format!("{maps_prefix}/")),
    ] {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !inventory::is_input_name(&name) {
                continue;
            }
            let path = e.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
            out.push(RawInput {
                cooked_rel: format!("{prefix}{name}"),
                path,
                size: meta.len(),
                mtime_ns,
            });
        }
    }
    out.sort_by(|a, b| a.cooked_rel.cmp(&b.cooked_rel));
    out.dedup_by(|a, b| a.cooked_rel == b.cooked_rel);
    Ok(out)
}

/// Hash every input, reusing `cache` entries whose size and mtime match.
/// Returns the inputs and how many were hashed now.
fn hash_inputs(
    raw: Vec<RawInput>,
    cache: &BTreeMap<String, HashEntry>,
) -> Result<(Vec<InputFile>, usize)> {
    let mut hashes: Vec<Option<String>> = raw
        .iter()
        .map(|r| {
            cache
                .get(&r.cooked_rel)
                .filter(|h| h.size == r.size && h.mtime_ns == r.mtime_ns && r.mtime_ns != 0)
                .map(|h| h.sha256.clone())
        })
        .collect();
    let todo: Vec<usize> = (0..raw.len()).filter(|&i| hashes[i].is_none()).collect();
    let paths: Vec<&Path> = todo.iter().map(|&i| raw[i].path.as_path()).collect();
    for (&i, r) in todo.iter().zip(parallel_file_hashes(&paths)) {
        let h = r.with_context(|| format!("hashing {}", raw[i].path.display()))?;
        hashes[i] = Some(h);
    }
    let inputs = raw
        .into_iter()
        .zip(hashes)
        .map(|(r, h)| InputFile {
            cooked_rel: r.cooked_rel,
            path: r.path,
            size: r.size,
            mtime_ns: r.mtime_ns,
            sha256: h.unwrap_or_default(),
        })
        .collect();
    Ok((inputs, todo.len()))
}

/// SHA-256 of every file in `paths` (same order), on up to 8 threads.
fn parallel_file_hashes(paths: &[&Path]) -> Vec<std::io::Result<String>> {
    let results: Mutex<Vec<(usize, std::io::Result<String>)>> = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(paths.len())
        .max(1);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = paths.get(k) else {
                        break;
                    };
                    let r = sha256::file_hex(p);
                    match results.lock() {
                        Ok(mut g) => g.push((k, r)),
                        Err(poisoned) => poisoned.into_inner().push((k, r)),
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|p| p.into_inner());
    results.sort_by_key(|(k, _)| *k);
    results.into_iter().map(|(_, r)| r).collect()
}

/// Files, bytes and the content digest of the tree under `dir` (symbolic
/// links are not followed).
fn tree_digest(dir: &Path) -> Result<OutputTotals> {
    let mut files: Vec<(String, PathBuf, u64)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).with_context(|| format!("listing {}", d.display()))?;
        for e in rd {
            // An unreadable entry must not silently drop out of the digest.
            let e = e.with_context(|| format!("listing {}", d.display()))?;
            let ft = e
                .file_type()
                .with_context(|| format!("reading the type of {}", e.path().display()))?;
            let path = e.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path.strip_prefix(dir).map(rel_string).unwrap_or_default();
                let size = e.metadata().map_or(0, |m| m.len());
                files.push((rel, path, size));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let paths: Vec<&Path> = files.iter().map(|f| f.1.as_path()).collect();
    let mut h = sha256::Sha256::new();
    let mut bytes = 0u64;
    for ((rel, path, size), r) in files.iter().zip(parallel_file_hashes(&paths)) {
        let digest = r.with_context(|| format!("hashing {}", path.display()))?;
        h.update(format!("{rel}\t{size}\t{digest}\n").as_bytes());
        bytes = bytes.saturating_add(*size);
    }
    Ok(OutputTotals {
        files: files.len() as u64,
        bytes,
        sha256: h.finalize_hex(),
    })
}

// ---------------------------------------------------------------------------
// Output folder safety and files
// ---------------------------------------------------------------------------

/// Resolve the output root to an absolute path and refuse the repository
/// (outside its git-ignored `research/` folders), the game install and
/// anything that looks like an install. Creates it when `create`.
fn resolve_out(out: &Path, install_root: &Path, create: bool) -> Result<PathBuf> {
    let abs = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()
            .context("reading the current directory")?
            .join(out)
    };
    let mut existing = abs.as_path();
    let mut missing: Vec<OsString> = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => bail!("cannot resolve the output directory {}", out.display()),
        }
    }
    let existing = existing
        .canonicalize()
        .with_context(|| format!("resolving {}", existing.display()))?;
    if !existing.is_dir() {
        bail!("{} is not a directory", existing.display());
    }
    let mut resolved = existing.clone();
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    refuse_inside_install(&resolved, install_root)?;
    // The shared rules: no repository paths outside research/<ignored>, no
    // `.app` bundles or `steamapps` trees, no symlinked targets.
    let probe = match missing.last() {
        Some(first_missing) => existing.join(first_missing),
        None => resolved.join(RUN_MANIFEST),
    };
    safety::check_output_path(&probe, install_root, true)?;
    if create {
        std::fs::create_dir_all(&resolved)
            .with_context(|| format!("creating {}", resolved.display()))?;
        let canonical = resolved
            .canonicalize()
            .with_context(|| format!("resolving {}", resolved.display()))?;
        refuse_inside_install(&canonical, install_root)?;
        return Ok(canonical);
    }
    Ok(resolved)
}

fn refuse_inside_install(dir: &Path, install_root: &Path) -> Result<()> {
    let Ok(root) = install_root.canonicalize() else {
        return Ok(());
    };
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    // Case-insensitive file systems (macOS and Windows defaults) see the same
    // folder under another spelling.
    let inside =
        dir.starts_with(&root) || Path::new(&lower(dir)).starts_with(Path::new(&lower(&root)));
    if inside {
        bail!(
            "refusing to write inside the game install {} (choose an output directory outside it)",
            root.display()
        );
    }
    Ok(())
}

fn write_json<T: Serialize>(out: &Path, name: &str, value: &T, install_root: &Path) -> Result<()> {
    let mut json = serde_json::to_string_pretty(value)?;
    json.push('\n');
    let target = safety::check_output_path(&out.join(name), install_root, true)?;
    safety::write_output(&target, json.as_bytes(), true)
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

fn dir_exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Files and bytes under `dir` (symbolic links are not followed).
pub(crate) fn dir_totals(dir: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(e.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    (files, bytes)
}

fn dir_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn rel_string(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64 / 1024.0;
    let mut unit = UNITS[0];
    for u in &UNITS[1..] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = u;
    }
    format!("{v:.1} {unit}")
}

fn print_verification(v: &Verification) {
    match v.status {
        VerifyStatus::Match => println!(
            "  inventory: all {} input files match {} (size and SHA-256)",
            v.checked,
            v.inventory.as_deref().unwrap_or("?")
        ),
        VerifyStatus::Mismatch => {
            println!(
                "  inventory: warning: {} of {} input files differ from {} (continuing; the \
                 conversions are verified on that build only)",
                v.mismatched.len(),
                v.checked,
                v.inventory.as_deref().unwrap_or("?")
            );
            for m in &v.mismatched {
                println!(
                    "    {}: {} bytes (inventory {})",
                    m.path, m.size, m.expected_size
                );
            }
        }
        VerifyStatus::MissingFiles => println!(
            "  inventory: {} of {} input files are missing",
            v.missing.len(),
            v.checked
        ),
        VerifyStatus::NoInventory | VerifyStatus::Skipped => {}
    }
    for n in &v.notes {
        println!("  inventory: {n}");
    }
    if !v.unlisted.is_empty() {
        println!(
            "  inventory: note: {} input file(s) not in the inventory: {}",
            v.unlisted.len(),
            v.unlisted.join(", ")
        );
    }
}

fn print_summary(manifest: &RunManifest, actions: &[(StageId, Action)], state: &State) {
    println!("summary:");
    println!(
        "  {:<10} {:<12} {:>8} {:>8} {:>10}",
        "stage", "result", "time", "files", "size"
    );
    let logs = state
        .last_run
        .as_ref()
        .map(|r| r.stages.as_slice())
        .unwrap_or_default();
    for (id, action) in actions {
        let rec = manifest.stages.iter().find(|r| r.name == id.name());
        let result = match action {
            Action::Ran(s) => s.label().to_owned(),
            Action::UpToDate => "up to date".to_owned(),
            Action::WouldRun(_) => "planned".to_owned(),
        };
        let secs = logs
            .iter()
            .find(|l| l.name == id.name())
            .map_or(0.0, |l| l.seconds);
        let (files, size) = rec
            .and_then(|r| r.outputs.as_ref())
            .map_or(("-".to_owned(), "-".to_owned()), |o| {
                (o.files.to_string(), fmt_bytes(o.bytes))
            });
        println!(
            "  {:<10} {:<12} {:>7.1}s {:>8} {:>10}",
            id.name(),
            result,
            secs,
            files,
            size
        );
    }
    if let Some(r) = &state.last_run {
        println!(
            "  total {:.1} s (hashing {:.1} s)",
            r.seconds, r.hashing_seconds
        );
    }
}

#[cfg(test)]
mod tests;
