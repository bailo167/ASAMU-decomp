//! Tests for `asamu-import all` with synthetic installs and stub stages
//! (no game data), plus one real-data check that skips without
//! `ASAMU_ORIGINAL_DIR`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    out: PathBuf,
    install: Install,
    inventories: Vec<Inventory>,
}

impl Fixture {
    fn cooked(&self) -> PathBuf {
        self.root.join("ASAMU").join("CookedMac")
    }

    fn env(&self) -> Env<'_> {
        Env {
            install: &self.install,
            out: &self.out,
            inventories: &self.inventories,
            tool: tool(),
        }
    }

    fn manifest_bytes(&self) -> Vec<u8> {
        std::fs::read(self.out.join(RUN_MANIFEST)).unwrap()
    }

    fn manifest(&self) -> RunManifest {
        serde_json::from_slice(&self.manifest_bytes()).unwrap()
    }

    fn record(&self, name: &str) -> StageRecord {
        self.manifest()
            .stages
            .into_iter()
            .find(|r| r.name == name)
            .unwrap()
    }

    fn state(&self) -> State {
        serde_json::from_slice(&std::fs::read(self.out.join(STATE_FILE)).unwrap()).unwrap()
    }
}

/// A loose install with two packages, a map and a texture file cache.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Game");
    let cooked = root.join("ASAMU").join("CookedMac");
    std::fs::create_dir_all(cooked.join("Maps")).unwrap();
    std::fs::create_dir_all(root.join("Engine")).unwrap();
    std::fs::write(cooked.join("Core.u"), b"core v1").unwrap();
    std::fs::write(cooked.join("Startup.upk"), b"startup").unwrap();
    std::fs::write(cooked.join("Textures.tfc"), b"texture cache").unwrap();
    std::fs::write(cooked.join("Maps").join("M1.asamu"), b"map one").unwrap();
    std::fs::write(cooked.join("PCTOC.txt"), b"not an input").unwrap();
    let install = asamu_locate::from_original_dir(&root).unwrap();
    let inventories = vec![inventory_of(&cooked, &[])];
    let out = tmp.path().join("converted");
    Fixture {
        _tmp: tmp,
        root,
        out,
        install,
        inventories,
    }
}

/// An inventory describing the input files currently in `cooked`, plus
/// `extra` (path, size, sha256) rows.
fn inventory_of(cooked: &Path, extra: &[(&str, u64, &str)]) -> Inventory {
    let mut files = Vec::new();
    for (dir, prefix) in [(cooked.to_path_buf(), ""), (cooked.join("Maps"), "Maps/")] {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && inventory::is_input_name(&name) {
                files.push(inventory::InventoryFile {
                    cooked_rel: format!("{prefix}{name}"),
                    size: e.metadata().unwrap().len(),
                    sha256: sha256::file_hex(&e.path()).unwrap(),
                });
            }
        }
    }
    for (p, s, h) in extra {
        files.push(inventory::InventoryFile {
            cooked_rel: (*p).to_owned(),
            size: *s,
            sha256: (*h).to_owned(),
        });
    }
    files.sort_by(|a, b| a.cooked_rel.cmp(&b.cooked_rel));
    Inventory {
        id: "test-inventory".to_owned(),
        layout: "loose".to_owned(),
        build_id: None,
        cooked_dir: "CookedMac".to_owned(),
        files,
    }
}

fn tool() -> ToolInfo {
    ToolInfo {
        name: "asamu-import".to_owned(),
        version: "test".to_owned(),
        build_sha256: Some("build-1".to_owned()),
    }
}

fn args(list: &[&str]) -> Args {
    let v: Vec<String> = list.iter().map(|s| (*s).to_owned()).collect();
    parse_stage_args::<Args>(&v).unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behave {
    /// Write `<dir>/manifest.json` (+ a data file copying Core.u).
    Write,
    /// Fail without writing.
    Fail,
    /// Write everything, then fail (some objects failed).
    FailAfterWrite,
    /// Write a half file, then fail (no key file).
    FailMidway,
    /// The stub module error.
    Stub,
    /// Write a sibling folder next to its own.
    WriteOutside,
    /// Fail with an error naming absolute paths (output and install).
    FailWithPaths,
}

#[derive(Clone, Debug)]
struct Call {
    id: StageId,
    argv: Vec<String>,
    out: PathBuf,
    original: PathBuf,
}

type Log = Rc<RefCell<Vec<Call>>>;

fn stub(id: StageId, behave: Behave, log: &Log) -> Stage {
    let log = Rc::clone(log);
    let dir = id.name();
    let runner: Runner = Box::new(move |ctx: &crate::Ctx, argv: &[String]| {
        let original = ctx.original.clone().unwrap();
        log.borrow_mut().push(Call {
            id,
            argv: argv.to_vec(),
            out: ctx.out.clone(),
            original: original.clone(),
        });
        let target = ctx.out.join(dir);
        let write = |target: &Path| {
            std::fs::create_dir_all(target).unwrap();
            let core = std::fs::read(original.join("ASAMU/CookedMac/Core.u")).unwrap();
            std::fs::write(target.join("data.bin"), core).unwrap();
            std::fs::write(
                target.join("manifest.json"),
                format!("{{\"stage\":\"{dir}\"}}"),
            )
            .unwrap();
        };
        match behave {
            Behave::Write => {
                write(&target);
                Ok(())
            }
            Behave::Fail => bail!("{dir}: everything failed"),
            Behave::FailAfterWrite => {
                write(&target);
                bail!("{dir}: 2 objects failed to convert")
            }
            Behave::FailMidway => {
                std::fs::create_dir_all(&target).unwrap();
                std::fs::write(target.join("half.bin"), b"half").unwrap();
                bail!("{dir}: interrupted")
            }
            Behave::Stub => bail!("{dir} is not implemented yet"),
            Behave::WriteOutside => {
                write(&target);
                write(&ctx.out.join("levels"));
                Ok(())
            }
            Behave::FailWithPaths => Err(anyhow!(
                "cannot read {}",
                original.join("ASAMU/CookedMac/Core.u").display()
            )
            .context(format!(
                "writing {}",
                target.join("manifest.json").display()
            ))),
        }
    });
    Stage {
        id,
        dir,
        // Kismet's stub stands for a stage without a single key file.
        key_file: (id != StageId::Kismet).then_some("manifest.json"),
        packages_only: id == StageId::Meshes,
        args: vec![format!("--{dir}-option")],
        runner,
    }
}

/// textures, meshes and kismet stubs.
fn stages(log: &Log, behave: &[(StageId, Behave)]) -> Vec<Stage> {
    let b = |id: StageId| {
        behave
            .iter()
            .find(|(i, _)| *i == id)
            .map_or(Behave::Write, |(_, b)| *b)
    };
    vec![
        stub(StageId::Textures, b(StageId::Textures), log),
        stub(StageId::Meshes, b(StageId::Meshes), log),
        stub(StageId::Kismet, b(StageId::Kismet), log),
    ]
}

fn ran(log: &Log) -> Vec<StageId> {
    log.borrow().iter().map(|c| c.id).collect()
}

fn actions(r: &Report) -> BTreeMap<StageId, Action> {
    r.actions.iter().cloned().collect()
}

// ---------------------------------------------------------------------------
// Pipeline behaviour
// ---------------------------------------------------------------------------

#[test]
fn first_run_converts_everything_and_rerun_skips() {
    let f = fixture();
    let log = Log::default();
    let r = execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(
        ran(&log),
        vec![StageId::Textures, StageId::Meshes, StageId::Kismet]
    );
    assert!(r.failed().is_empty());
    // Every stage runs with the staging folder as its output root.
    let calls = log.borrow().clone();
    let staging = f.out.canonicalize().unwrap().join(STAGING_DIR);
    assert!(calls.iter().all(|c| c.out == staging));
    assert_eq!(calls[0].argv, vec!["--textures-option".to_owned()]);
    assert_eq!(calls[0].original, f.install.root);
    for d in ["textures", "meshes", "kismet"] {
        assert!(f.out.join(d).join("manifest.json").is_file(), "{d}");
    }
    assert!(!f.out.join(STAGING_DIR).exists());
    assert!(!f.out.join(REPLACED_DIR).exists());

    let m = f.manifest();
    assert_eq!(m.format, "asamu-import-run");
    assert_eq!(m.tool.version, "test");
    assert_eq!(m.install.layout, "loose");
    assert_eq!(m.install.cooked_dir, "CookedMac");
    assert_eq!(
        m.install.inputs.files, 4,
        "packages, map and tfc; not the TOC"
    );
    assert_eq!(m.verification.status, VerifyStatus::Match);
    assert_eq!(m.stages.len(), 3);
    for s in &m.stages {
        assert_eq!(s.status, StageStatus::Ok);
        let o = s.outputs.as_ref().unwrap();
        let manifest = format!("{{\"stage\":\"{}\"}}", s.name);
        assert_eq!((o.files, o.bytes), (2, 7 + manifest.len() as u64));
        // The digest covers paths, sizes and contents.
        let expected = sha256::hex_digest(
            format!(
                "data.bin\t7\t{}\nmanifest.json\t{}\t{}\n",
                sha256::hex_digest(b"core v1"),
                manifest.len(),
                sha256::hex_digest(manifest.as_bytes())
            )
            .as_bytes(),
        );
        assert_eq!(o.sha256, expected);
    }
    let text = String::from_utf8(f.manifest_bytes()).unwrap();
    assert!(
        !text.contains(&f.root.display().to_string()),
        "no absolute paths"
    );
    let first = f.manifest_bytes();
    let state = f.state();
    assert_eq!(state.last_run.as_ref().unwrap().hashed_files, 4);

    // Second run: nothing to do, identical manifest, nothing hashed again.
    let log2 = Log::default();
    let r2 = execute(&f.env(), stages(&log2, &[]), &args(&[])).unwrap();
    assert!(ran(&log2).is_empty());
    assert!(actions(&r2).values().all(|a| *a == Action::UpToDate));
    assert_eq!(f.manifest_bytes(), first, "run manifest is deterministic");
    assert_eq!(f.state().last_run.unwrap().hashed_files, 0);
}

#[test]
fn changed_input_reruns_every_stage_and_replaces_stale_files() {
    let f = fixture();
    let log = Log::default();
    execute(&f.env(), stages(&log, &[]), &args(&["--no-verify"])).unwrap();
    let before = f.manifest().install.inputs.sha256;
    std::fs::write(f.out.join("kismet/stale.txt"), b"from an older importer").unwrap();
    std::fs::write(f.cooked().join("Core.u"), b"core v2 (patched)").unwrap();
    let log2 = Log::default();
    execute(&f.env(), stages(&log2, &[]), &args(&["--no-verify"])).unwrap();
    assert_eq!(
        ran(&log2),
        vec![StageId::Textures, StageId::Meshes, StageId::Kismet]
    );
    // Each stage starts from an empty folder: no stale file survives.
    assert!(!f.out.join("kismet/stale.txt").exists());
    assert_ne!(f.manifest().install.inputs.sha256, before);
    assert_eq!(
        std::fs::read(f.out.join("textures/data.bin")).unwrap(),
        b"core v2 (patched)"
    );
}

#[test]
fn changed_importer_build_or_arguments_rerun() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    // Another importer build.
    let mut env = f.env();
    env.tool.build_sha256 = Some("build-2".to_owned());
    let log = Log::default();
    execute(&env, stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log).len(), 3);
    // Other stage arguments.
    let log = Log::default();
    let mut st = stages(&log, &[]);
    st[1].args.push("--extra".to_owned());
    execute(&env, st, &args(&[])).unwrap();
    assert_eq!(ran(&log), vec![StageId::Meshes]);
}

#[test]
fn deleted_output_reruns_only_that_stage() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    std::fs::remove_file(f.out.join("meshes/manifest.json")).unwrap();
    let log = Log::default();
    let r = execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log), vec![StageId::Meshes]);
    assert_eq!(
        actions(&r).get(&StageId::Meshes),
        Some(&Action::Ran(StageStatus::Ok))
    );
    assert!(f.out.join("meshes/manifest.json").is_file());
}

#[test]
fn failed_stage_is_recorded_others_continue_and_rerun_retries_it() {
    let f = fixture();
    let log = Log::default();
    let r = execute(
        &f.env(),
        stages(&log, &[(StageId::Meshes, Behave::Fail)]),
        &args(&[]),
    )
    .unwrap();
    assert_eq!(r.failed(), vec!["meshes"]);
    assert_eq!(ran(&log).len(), 3, "later stages still ran");
    let rec = f.record("meshes");
    assert_eq!(rec.status, StageStatus::Failed);
    assert!(rec.error.unwrap().contains("everything failed"));
    assert!(rec.outputs.is_none());
    assert!(!f.out.join("meshes").exists());
    assert!(!f.out.join(STAGING_DIR).exists());

    let log2 = Log::default();
    let r2 = execute(&f.env(), stages(&log2, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log2), vec![StageId::Meshes]);
    assert!(r2.failed().is_empty());
    assert_eq!(f.record("meshes").status, StageStatus::Ok);
}

#[test]
fn fail_fast_stops_after_the_first_failure() {
    let f = fixture();
    let log = Log::default();
    let r = execute(
        &f.env(),
        stages(&log, &[(StageId::Textures, Behave::Fail)]),
        &args(&["--fail-fast"]),
    )
    .unwrap();
    assert_eq!(ran(&log), vec![StageId::Textures]);
    assert_eq!(r.failed(), vec!["textures"]);
    let logs = f.state().last_run.unwrap().stages;
    let meshes = logs.iter().find(|l| l.name == "meshes").unwrap();
    assert_eq!(meshes.action, "not-run-fail-fast");
}

#[test]
fn hashes_survive_an_aborted_run() {
    let mut f = fixture();
    // The run stops after hashing (here: a refused verification).
    f.inventories = vec![inventory_of(&f.cooked(), &[("Maps/M2.asamu", 1, "00")])];
    assert!(execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).is_err());
    assert_eq!(
        f.state().hashes.len(),
        4,
        "hash cache written before the stages"
    );
    f.inventories = vec![inventory_of(&f.cooked(), &[])];
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    assert_eq!(f.state().last_run.unwrap().hashed_files, 0);
}

#[test]
fn refuses_a_run_manifest_that_is_not_a_file() {
    let f = fixture();
    std::fs::create_dir_all(f.out.join(RUN_MANIFEST)).unwrap();
    let log = Log::default();
    assert!(execute(&f.env(), stages(&log, &[]), &args(&[])).is_err());
    assert!(ran(&log).is_empty());
}

#[test]
fn partial_failure_keeps_the_written_output() {
    let f = fixture();
    let r = execute(
        &f.env(),
        stages(
            &Log::default(),
            &[(StageId::Meshes, Behave::FailAfterWrite)],
        ),
        &args(&[]),
    )
    .unwrap();
    assert_eq!(r.failed(), vec!["meshes"]);
    let rec = f.record("meshes");
    assert_eq!(rec.status, StageStatus::Failed);
    assert!(rec.error.unwrap().contains("partial output kept"));
    assert!(f.out.join("meshes/manifest.json").is_file());
    assert_eq!(rec.outputs.map(|o| o.files), Some(2));
}

#[test]
fn stub_module_is_unavailable_not_a_failure() {
    let f = fixture();
    let r = execute(
        &f.env(),
        stages(&Log::default(), &[(StageId::Kismet, Behave::Stub)]),
        &args(&[]),
    )
    .unwrap();
    assert!(r.failed().is_empty());
    assert_eq!(f.record("kismet").status, StageStatus::Unavailable);
    // Tried again next time (the module may exist by then).
    let log = Log::default();
    execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log), vec![StageId::Kismet]);
    assert_eq!(f.record("kismet").status, StageStatus::Ok);
}

#[test]
fn interrupted_run_is_redone_from_a_clean_staging_folder() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    // Simulate a run killed while textures was being converted.
    let mut m = f.manifest();
    m.stages[0].status = StageStatus::Running;
    std::fs::write(
        f.out.join(RUN_MANIFEST),
        serde_json::to_string_pretty(&m).unwrap(),
    )
    .unwrap();
    let staged = f.out.join(STAGING_DIR).join("textures");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("truncated.dds"), b"DD").unwrap();
    std::fs::create_dir_all(f.out.join(REPLACED_DIR).join("textures")).unwrap();

    let log = Log::default();
    execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log), vec![StageId::Textures]);
    assert!(!f.out.join(STAGING_DIR).exists());
    assert!(!f.out.join(REPLACED_DIR).exists());
    assert!(!f.out.join("textures/truncated.dds").exists());
    assert_eq!(f.record("textures").status, StageStatus::Ok);
}

#[test]
fn old_output_stays_until_the_new_output_is_complete() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    let r = execute(
        &f.env(),
        stages(&Log::default(), &[(StageId::Textures, Behave::FailMidway)]),
        &args(&["--force"]),
    )
    .unwrap();
    assert_eq!(r.failed(), vec!["textures"]);
    // The complete output of the first run is untouched.
    assert!(f.out.join("textures/manifest.json").is_file());
    assert!(!f.out.join("textures/half.bin").exists());
    assert_eq!(f.record("textures").status, StageStatus::Failed);
    assert!(!f.out.join(STAGING_DIR).exists());
}

#[test]
fn writing_outside_the_stage_folder_is_rejected() {
    let f = fixture();
    let r = execute(
        &f.env(),
        stages(&Log::default(), &[(StageId::Meshes, Behave::WriteOutside)]),
        &args(&[]),
    )
    .unwrap();
    assert_eq!(r.failed(), vec!["meshes"]);
    assert!(
        f.record("meshes")
            .error
            .unwrap()
            .contains("outside its folder")
    );
    assert!(!f.out.join("levels").exists());
    assert!(!f.out.join("meshes").exists());
}

#[test]
fn deleted_or_added_output_files_rerun_that_stage() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    // A data file disappears behind an intact manifest.
    std::fs::remove_file(f.out.join("textures/data.bin")).unwrap();
    // A stray file appears in another stage folder.
    std::fs::write(f.out.join("kismet/stray.txt"), b"x").unwrap();
    let log = Log::default();
    let r = execute(&f.env(), stages(&log, &[]), &args(&["--plan"])).unwrap();
    assert!(ran(&log).is_empty());
    assert_eq!(
        actions(&r).get(&StageId::Textures),
        Some(&Action::WouldRun(
            "output files changed since the last run".to_owned()
        ))
    );
    assert_eq!(actions(&r).get(&StageId::Meshes), Some(&Action::UpToDate));
    execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log), vec![StageId::Textures, StageId::Kismet]);
    assert!(f.out.join("textures/data.bin").is_file());
    assert!(!f.out.join("kismet/stray.txt").exists());
    // And then it is up to date again.
    let log = Log::default();
    execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert!(ran(&log).is_empty());
}

#[test]
fn refuses_an_output_folder_holding_other_data() {
    let f = fixture();
    std::fs::create_dir_all(f.out.join("audio")).unwrap();
    std::fs::write(f.out.join("audio/my-song.flac"), b"user data").unwrap();
    std::fs::write(f.out.join("notes.txt"), b"user data").unwrap();
    let log = Log::default();
    for extra in [&[][..], &["--plan"][..]] {
        let err = execute(&f.env(), stages(&log, &[]), &args(extra)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("already holds other files"), "{msg}");
        assert!(msg.contains("notes.txt"), "{msg}");
    }
    assert!(ran(&log).is_empty());
    assert!(
        f.out.join("audio/my-song.flac").is_file(),
        "nothing deleted"
    );
    assert!(!f.out.join(RUN_MANIFEST).exists());
    assert!(!f.out.join(STATE_FILE).exists());
}

#[test]
fn refuses_stage_named_folders_that_are_not_converter_output() {
    let f = fixture();
    // `--out ~/Music` holding only a user's `audio` folder: named like a
    // stage, but no converter wrote it.
    std::fs::create_dir_all(f.out.join("audio")).unwrap();
    std::fs::write(f.out.join("audio/song.flac"), b"user data").unwrap();
    let err = execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap_err();
    assert!(format!("{err:#}").contains("audio"), "{err:#}");
    assert!(f.out.join("audio/song.flac").is_file());
    // A file named like a stage is not stage output either.
    let g = fixture();
    std::fs::create_dir_all(&g.out).unwrap();
    std::fs::write(g.out.join("textures"), b"x").unwrap();
    assert!(execute(&g.env(), stages(&Log::default(), &[]), &args(&[])).is_err());
    // Converter output of a stage without a key file: a top-level JSON file.
    assert!(!looks_like_stage_output(&f.out.join("nope"), None));
    let lm = f.out.join("lm");
    std::fs::create_dir_all(lm.join("Map")).unwrap();
    assert!(!looks_like_stage_output(&lm, None), "no JSON");
    std::fs::write(lm.join("Map.lightmaps.json"), b"{}").unwrap();
    assert!(looks_like_stage_output(&lm, None));
}

#[test]
fn accepts_folders_with_only_stage_output_or_a_run_record() {
    // Single-stage commands ran here before: only stage folders.
    let f = fixture();
    std::fs::create_dir_all(f.out.join("textures")).unwrap();
    std::fs::write(f.out.join("textures/manifest.json"), b"{}").unwrap();
    std::fs::write(f.out.join(".DS_Store"), b"").unwrap();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    // A folder with a run manifest is ours even with other entries.
    std::fs::write(f.out.join("readme.txt"), b"mine").unwrap();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    assert!(f.out.join("readme.txt").is_file());
}

#[test]
fn failed_stage_errors_hold_no_absolute_paths() {
    let f = fixture();
    let r = execute(
        &f.env(),
        stages(&Log::default(), &[(StageId::Meshes, Behave::FailWithPaths)]),
        &args(&[]),
    )
    .unwrap();
    assert_eq!(r.failed(), vec!["meshes"]);
    // `/`-normalised so the check holds with Windows separators too.
    let err = f.record("meshes").error.unwrap().replace('\\', "/");
    assert!(err.contains("<out>/.staging/meshes/manifest.json"), "{err}");
    assert!(err.contains("<install>/ASAMU/CookedMac/Core.u"), "{err}");
    let text = String::from_utf8(f.manifest_bytes()).unwrap();
    let tmp_root = f.root.parent().unwrap().display().to_string();
    assert!(!text.contains(&tmp_root), "{text}");
    let canonical = f.root.canonicalize().unwrap();
    assert!(!text.contains(&canonical.display().to_string()), "{text}");
}

#[test]
fn only_and_skip_select_stages_and_keep_other_records() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    let log = Log::default();
    let r = execute(
        &f.env(),
        stages(&log, &[]),
        &args(&["--only", "meshes,kismet", "--skip", "kismet", "--force"]),
    )
    .unwrap();
    assert_eq!(ran(&log), vec![StageId::Meshes]);
    assert_eq!(r.actions.len(), 1);
    assert_eq!(f.manifest().stages.len(), 3, "other records carried over");
    let state = f.state();
    let logs = state.last_run.unwrap().stages;
    assert_eq!(logs.len(), StageId::ALL.len());
    assert_eq!(
        logs.iter().find(|l| l.name == "textures").unwrap().action,
        "not-selected"
    );
}

#[test]
fn plan_writes_nothing() {
    let f = fixture();
    let log = Log::default();
    let r = execute(&f.env(), stages(&log, &[]), &args(&["--plan"])).unwrap();
    assert!(ran(&log).is_empty());
    assert!(!f.out.exists(), "the output folder is not even created");
    assert!(
        r.actions
            .iter()
            .all(|(_, a)| matches!(a, Action::WouldRun(reason) if reason == "not converted yet"))
    );
    // After a real run, the plan reports up to date.
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    let before = f.manifest_bytes();
    let r = execute(&f.env(), stages(&log, &[]), &args(&["--plan"])).unwrap();
    assert!(r.actions.iter().all(|(_, a)| *a == Action::UpToDate));
    assert_eq!(f.manifest_bytes(), before);
}

#[test]
fn missing_inventoried_file_refuses_before_any_stage() {
    let mut f = fixture();
    f.inventories = vec![inventory_of(&f.cooked(), &[("Maps/M2.asamu", 10, "00")])];
    let log = Log::default();
    let err = execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Maps/M2.asamu"), "{msg}");
    assert!(ran(&log).is_empty());
    // --no-verify converts what is there.
    execute(&f.env(), stages(&log, &[]), &args(&["--no-verify"])).unwrap();
    assert_eq!(ran(&log).len(), 3);
    assert_eq!(f.manifest().verification.status, VerifyStatus::Skipped);
}

#[test]
fn mismatched_files_only_warn() {
    let mut f = fixture();
    let mut inv = inventory_of(&f.cooked(), &[]);
    inv.files[0].sha256 = "0".repeat(64);
    f.inventories = vec![inv];
    let log = Log::default();
    execute(&f.env(), stages(&log, &[]), &args(&[])).unwrap();
    assert_eq!(ran(&log).len(), 3);
    let v = f.manifest().verification;
    assert_eq!(v.status, VerifyStatus::Mismatch);
    assert_eq!(v.mismatched.len(), 1);
}

#[test]
fn other_cooks_have_no_inventory_and_still_convert() {
    let mut f = fixture();
    let mut inv = inventory_of(&f.cooked(), &[]);
    inv.cooked_dir = "CookedPC".to_owned();
    f.inventories = vec![inv];
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    assert_eq!(f.manifest().verification.status, VerifyStatus::NoInventory);
}

#[test]
fn empty_install_is_refused() {
    let f = fixture();
    for e in std::fs::read_dir(f.cooked()).unwrap().flatten() {
        if e.path().is_file() {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    std::fs::remove_dir_all(f.cooked().join("Maps")).unwrap();
    let err = execute(
        &f.env(),
        stages(&Log::default(), &[]),
        &args(&["--no-verify"]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("no packages"));
}

#[test]
fn refuses_output_inside_the_install() {
    let mut f = fixture();
    f.out = f.root.join("ASAMU").join("converted");
    let err = execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap_err();
    assert!(format!("{err:#}").contains("inside the game install"));
    assert!(!f.out.exists());
}

#[test]
fn refuses_output_inside_the_repository() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let f = fixture();
    let inside = repo.join("asamu-import-all-test-output").join("x");
    assert!(resolve_out(&inside, &f.root, false).is_err());
    assert!(!repo.join("asamu-import-all-test-output").exists());
    let docs = repo.join("docs").join("converted");
    assert!(resolve_out(&docs, &f.root, false).is_err());
    // research/local/ is git-ignored and allowed.
    let local = repo.join("research").join("local");
    if local.is_dir() {
        let ok = resolve_out(&local.join("asamu-import-all-test-x"), &f.root, false);
        assert!(ok.is_ok(), "{ok:?}");
        assert!(!local.join("asamu-import-all-test-x").exists());
    }
}

#[cfg(unix)]
#[test]
fn refuses_to_replace_a_symlinked_stage_folder() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    let elsewhere = f.root.parent().unwrap().join("elsewhere");
    std::fs::rename(f.out.join("meshes"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.out.join("meshes")).unwrap();
    let r = execute(&f.env(), stages(&Log::default(), &[]), &args(&["--force"])).unwrap();
    assert_eq!(r.failed(), vec!["meshes"]);
    assert!(f.record("meshes").error.unwrap().contains("symbolic link"));
    assert!(
        elsewhere.join("manifest.json").is_file(),
        "target untouched"
    );
}

#[test]
fn hash_cache_is_dropped_for_another_install_root() {
    let f = fixture();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    let mut state = f.state();
    state.install_root = "/elsewhere".to_owned();
    std::fs::write(
        f.out.join(STATE_FILE),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    execute(&f.env(), stages(&Log::default(), &[]), &args(&[])).unwrap();
    assert_eq!(f.state().last_run.unwrap().hashed_files, 4);
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

#[test]
fn decisions() {
    let rec = |status, fp: &str| StageRecord {
        name: "textures".to_owned(),
        status,
        args: Vec::new(),
        fingerprint: fp.to_owned(),
        outputs: Some(OutputTotals {
            files: 2,
            bytes: 10,
            sha256: String::new(),
        }),
        error: None,
    };
    let found = |key_present: bool, totals: Option<(u64, u64)>| OutputsFound {
        key_present,
        totals,
    };
    let same = found(true, Some((2, 10)));
    let ok = rec(StageStatus::Ok, "a");
    assert_eq!(decide(Some(&ok), "a", &same, false), Decision::UpToDate);
    assert!(matches!(
        decide(Some(&ok), "a", &same, true),
        Decision::Run(_)
    ));
    assert!(matches!(
        decide(Some(&ok), "b", &same, false),
        Decision::Run(_)
    ));
    assert!(matches!(
        decide(Some(&ok), "a", &found(false, Some((2, 10))), false),
        Decision::Run(_)
    ));
    // A file deleted, added or resized behind an intact key file.
    for totals in [Some((1, 4)), Some((3, 12)), Some((2, 11)), None] {
        assert_eq!(
            decide(Some(&ok), "a", &found(true, totals), false),
            Decision::Run("output files changed since the last run".to_owned()),
            "{totals:?}"
        );
    }
    // A record without totals (killed before the final digest) is trusted.
    let mut no_totals = rec(StageStatus::Ok, "a");
    no_totals.outputs = None;
    assert_eq!(
        decide(Some(&no_totals), "a", &found(true, Some((9, 9))), false),
        Decision::UpToDate
    );
    assert!(matches!(
        decide(None, "a", &found(false, None), false),
        Decision::Run(_)
    ));
    for s in [
        StageStatus::Failed,
        StageStatus::Running,
        StageStatus::Unavailable,
    ] {
        assert!(matches!(
            decide(Some(&rec(s, "a")), "a", &same, false),
            Decision::Run(_)
        ));
    }
}

#[test]
fn stub_errors_are_recognised_only_as_bare_stub_messages() {
    assert!(is_stub_error(&anyhow!("kismet is not implemented yet")));
    // A real failure whose chain mentions an unimplemented feature fails.
    let deep = anyhow!("zlib bulk data is not implemented yet").context("decoding X.upk");
    assert!(!is_stub_error(&deep));
    assert!(!is_stub_error(&anyhow!("kismet: 3 maps failed")));
}

#[test]
fn redaction_removes_output_install_and_home_paths() {
    let out = Path::new("/data/users/someone/asamu/converted");
    let install = Path::new("/data/games/A Story About My Uncle");
    let r = path_redactions(out, install);
    let msg = format!(
        "writing {}/.staging/textures/x.dds: denied; reading {}/A Story About My Uncle.app/y.upk",
        out.display(),
        install.display()
    );
    let red = redact_paths(&msg, &r);
    assert_eq!(
        red,
        "writing <out>/.staging/textures/x.dds: denied; reading <install>/A Story About My \
         Uncle.app/y.upk"
    );
    // Bare roots are never replaced.
    let r = path_redactions(Path::new("/"), Path::new("/"));
    assert!(r.iter().all(|(p, _)| p.len() > 3), "{r:?}");
    assert_eq!(redact_paths("/a/b", &r), "/a/b");
}

#[test]
fn stage_names() {
    assert_eq!(parse_stage("Textures"), Ok(StageId::Textures));
    assert_eq!(parse_stage(" lightmaps "), Ok(StageId::Lightmaps));
    assert_eq!(parse_stage("localization"), Ok(StageId::Localization));
    assert!(parse_stage("shaders").is_err());
    let names: Vec<&str> = StageId::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        [
            "textures",
            "meshes",
            "materials",
            "levels",
            "audio",
            "matinee",
            "skeletal",
            "kismet",
            "lightmaps",
            "particles",
            "decals",
            "localization"
        ]
    );
    let a = args(&["--only", "audio,levels", "--skip", "levels"]);
    assert_eq!(a.only, vec![StageId::Audio, StageId::Levels]);
    assert_eq!(a.skip, vec![StageId::Levels]);
    assert_eq!(a.lang, "INT");
    assert_eq!(a.package_cache_max_mib, 2048);
}

/// The arguments `all` gives the real modules must parse with their `Args`.
#[test]
fn real_stage_arguments_parse_with_the_module_args() {
    let a = args(&["--png", "--lang", "DEU"]);
    let st = real_stages(&a);
    assert_eq!(st.len(), StageId::ALL.len());
    let order: Vec<StageId> = st.iter().map(|s| s.id).collect();
    assert_eq!(order, StageId::ALL);
    let argv = |id: StageId| st.iter().find(|s| s.id == id).unwrap().args.clone();
    assert_eq!(argv(StageId::Textures), vec!["--png".to_owned()]);
    assert_eq!(argv(StageId::Meshes), vec!["--collision".to_owned()]);
    assert_eq!(
        argv(StageId::Audio),
        vec!["--lang".to_owned(), "DEU".to_owned()]
    );
    parse_stage_args::<crate::textures::Args>(&argv(StageId::Textures)).unwrap();
    parse_stage_args::<crate::meshes::Args>(&argv(StageId::Meshes)).unwrap();
    parse_stage_args::<crate::materials::Args>(&argv(StageId::Materials)).unwrap();
    parse_stage_args::<crate::levels::Args>(&argv(StageId::Levels)).unwrap();
    parse_stage_args::<crate::audio::Args>(&argv(StageId::Audio)).unwrap();
    parse_stage_args::<crate::matinee::Args>(&argv(StageId::Matinee)).unwrap();
    parse_stage_args::<crate::skeletal::Args>(&argv(StageId::Skeletal)).unwrap();
    parse_stage_args::<crate::kismet::Args>(&argv(StageId::Kismet)).unwrap();
    parse_stage_args::<crate::lightmaps::Args>(&argv(StageId::Lightmaps)).unwrap();
    parse_stage_args::<crate::particles::Args>(&argv(StageId::Particles)).unwrap();
    parse_stage_args::<crate::decals::Args>(&argv(StageId::Decals)).unwrap();
    parse_stage_args::<crate::localization::Args>(&argv(StageId::Localization)).unwrap();
    // Only stages that read nothing but packages may use the package cache.
    let cache_ok: Vec<StageId> = st
        .iter()
        .filter(|s| s.packages_only)
        .map(|s| s.id)
        .collect();
    assert_eq!(
        cache_ok,
        [
            StageId::Meshes,
            StageId::Materials,
            StageId::Levels,
            StageId::Audio,
            StageId::Matinee,
            StageId::Skeletal
        ]
    );
    assert!(supports_flag::<crate::textures::Args>("png"));
    assert!(!supports_flag::<crate::textures::Args>("no-such-flag"));
    // The key files `all` checks are the ones the modules document.
    let key = |id: StageId| st.iter().find(|s| s.id == id).unwrap().key_file;
    assert_eq!(key(StageId::Materials), Some("materials.json"));
    assert_eq!(key(StageId::Kismet), Some("manifest.json"));
    assert_eq!(key(StageId::Lightmaps), None);
    assert_eq!(key(StageId::Particles), Some("particles.json"));
    assert_eq!(key(StageId::Decals), Some("manifest.json"));
    assert_eq!(key(StageId::Localization), Some("manifest.json"));
    assert!(parse_stage_args::<crate::textures::Args>(&["--bogus".to_owned()]).is_err());
}

#[test]
fn byte_formatting() {
    assert_eq!(fmt_bytes(0), "0 B");
    assert_eq!(fmt_bytes(1023), "1023 B");
    assert_eq!(fmt_bytes(1536), "1.5 KiB");
    assert_eq!(fmt_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
}

// ---------------------------------------------------------------------------
// Package cache
// ---------------------------------------------------------------------------

/// A minimal uncompressed v868 package: summary, empty tables, `payload`.
fn minimal_package(payload: &[u8]) -> Vec<u8> {
    fn summary(offset: i32) -> Vec<u8> {
        let mut v = Vec::new();
        let i32le = |v: &mut Vec<u8>, x: i32| v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&0x9E2A_83C1_u32.to_le_bytes());
        v.extend_from_slice(&868u16.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        i32le(&mut v, offset); // TotalHeaderSize
        i32le(&mut v, 5); // FolderName "None"
        v.extend_from_slice(b"None\0");
        v.extend_from_slice(&0u32.to_le_bytes()); // PackageFlags
        for _ in 0..3 {
            i32le(&mut v, 0); // count
            i32le(&mut v, offset); // offset
        }
        for _ in 0..5 {
            i32le(&mut v, 0); // depends, guids offset/counts, thumbnails
        }
        v.extend_from_slice(&[7u8; 16]); // Guid
        i32le(&mut v, 1); // Generations
        for _ in 0..3 {
            i32le(&mut v, 0);
        }
        i32le(&mut v, 868); // EngineVersion
        i32le(&mut v, 0); // CookerVersion
        v.extend_from_slice(&0u32.to_le_bytes()); // CompressionFlags
        i32le(&mut v, 0); // CompressedChunks
        v.extend_from_slice(&0u32.to_le_bytes()); // PackageSource
        i32le(&mut v, 0); // AdditionalPackagesToCook
        i32le(&mut v, 0); // TextureAllocations
        v
    }
    let len = i32::try_from(summary(0).len()).unwrap();
    let mut bytes = summary(len);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn minimal_package_parses() {
    let p = asamu_ue3::Package::from_bytes(minimal_package(b"payload")).unwrap();
    assert!(p.exports.is_empty());
}

#[test]
fn package_cache_writes_reuses_invalidates_and_respects_its_bound() {
    let f = fixture();
    let cooked = f.cooked();
    std::fs::write(cooked.join("Core.u"), minimal_package(b"core")).unwrap();
    std::fs::write(cooked.join("Startup.upk"), minimal_package(b"startup")).unwrap();
    std::fs::write(cooked.join("Maps/M1.asamu"), minimal_package(b"map")).unwrap();
    let raw = gather_inputs(&f.install).unwrap();
    let (inputs, _) = hash_inputs(raw, &BTreeMap::new()).unwrap();
    std::fs::create_dir_all(&f.out).unwrap();
    let out = f.out.canonicalize().unwrap();
    let prep = |inputs: &[InputFile], limit: u64| {
        pkgcache::prepare(&out, inputs, "CookedMac", "Maps", limit, "test").unwrap()
    };

    let pkgcache::Outcome::Ready(p) = prep(&inputs, u64::MAX) else {
        panic!("cache not ready")
    };
    assert_eq!((p.written, p.reused), (3, 0), "the tfc is not cached");
    let cache_cooked = p.root.join("ASAMU/CookedMac");
    assert_eq!(
        std::fs::read(cache_cooked.join("Core.u")).unwrap(),
        std::fs::read(cooked.join("Core.u")).unwrap(),
        "an uncompressed package's stream is the file"
    );
    assert!(cache_cooked.join("Maps/M1.asamu").is_file());
    assert!(!cache_cooked.join("Textures.tfc").exists());
    // The tree is a loose install root for the stages.
    let fake = asamu_locate::from_original_dir(&p.root).unwrap();
    assert_eq!(
        fake.cooked_dir.canonicalize().unwrap(),
        cache_cooked.canonicalize().unwrap()
    );
    assert!(fake.maps_dir.join("M1.asamu").is_file());

    // Unchanged: reused.
    let pkgcache::Outcome::Ready(p2) = prep(&inputs, u64::MAX) else {
        panic!("cache not ready")
    };
    assert_eq!((p2.written, p2.reused), (0, 3));
    assert_eq!(p2.bytes, p.bytes);

    // A changed source and a removed package.
    std::fs::write(cooked.join("Core.u"), minimal_package(b"core v2")).unwrap();
    std::fs::remove_file(cooked.join("Startup.upk")).unwrap();
    let (inputs2, _) = hash_inputs(gather_inputs(&f.install).unwrap(), &BTreeMap::new()).unwrap();
    let pkgcache::Outcome::Ready(p3) = prep(&inputs2, u64::MAX) else {
        panic!("cache not ready")
    };
    assert_eq!((p3.written, p3.reused), (1, 1));
    assert!(
        !cache_cooked.join("Startup.upk").exists(),
        "stale entry removed"
    );
    assert_eq!(
        std::fs::read(cache_cooked.join("Core.u")).unwrap(),
        minimal_package(b"core v2")
    );

    // Streams of another importer build are decompressed again.
    let pkgcache::Outcome::Ready(p4) =
        pkgcache::prepare(&out, &inputs2, "CookedMac", "Maps", u64::MAX, "test+other").unwrap()
    else {
        panic!("cache not ready")
    };
    assert_eq!((p4.written, p4.reused), (2, 0));
    assert_eq!(p4.bytes, p3.bytes);

    // Over the bound: removed, not used.
    let out_of_bound = prep(&inputs2, 10);
    assert!(matches!(
        out_of_bound,
        pkgcache::Outcome::TooLarge { limit: 10, .. }
    ));
    assert!(!out.join(pkgcache::DIR).exists());
}

#[test]
fn package_cache_feeds_package_only_stages() {
    let f = fixture();
    let cooked = f.cooked();
    std::fs::write(cooked.join("Core.u"), minimal_package(b"core")).unwrap();
    std::fs::write(cooked.join("Startup.upk"), minimal_package(b"startup")).unwrap();
    std::fs::write(cooked.join("Maps/M1.asamu"), minimal_package(b"map")).unwrap();
    let log = Log::default();
    execute(
        &f.env(),
        stages(&log, &[]),
        &args(&["--no-verify", "--package-cache"]),
    )
    .unwrap();
    let calls = log.borrow().clone();
    let cache = f.out.canonicalize().unwrap().join(pkgcache::DIR);
    let original = |id: StageId| calls.iter().find(|c| c.id == id).unwrap().original.clone();
    assert_eq!(
        original(StageId::Meshes),
        cache,
        "package-only staged stage uses the cache"
    );
    assert_eq!(
        original(StageId::Textures),
        f.install.root,
        "textures reads the install"
    );
    assert_eq!(
        original(StageId::Kismet),
        f.install.root,
        "stages that are not package-only read the install"
    );
    // Same output either way.
    assert_eq!(
        std::fs::read(f.out.join("meshes/data.bin")).unwrap(),
        std::fs::read(cooked.join("Core.u")).unwrap()
    );
    let state = f.state();
    let cache_log = state.last_run.unwrap().package_cache.unwrap();
    assert!(cache_log.used);
    assert_eq!(cache_log.written, 3);
}

// ---------------------------------------------------------------------------
// Real data (skips without ASAMU_ORIGINAL_DIR)
// ---------------------------------------------------------------------------

/// The real install verifies against the committed inventory (`--plan`
/// writes nothing).
#[test]
fn real_install_matches_the_committed_inventory() {
    let Some(dir) = std::env::var_os("ASAMU_ORIGINAL_DIR") else {
        eprintln!("skipping: ASAMU_ORIGINAL_DIR not set");
        return;
    };
    let install = asamu_locate::from_original_dir(Path::new(&dir)).unwrap();
    let inventories = Inventory::embedded().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("converted");
    let env = Env {
        install: &install,
        out: &out,
        inventories: &inventories,
        tool: tool(),
    };
    let a = args(&["--plan"]);
    let r = execute(&env, real_stages(&a), &a).unwrap();
    assert!(!out.exists());
    let v = &r.manifest.verification;
    if install.cooked_dir.ends_with("CookedMac") {
        assert_eq!(v.status, VerifyStatus::Match, "{v:?}");
        assert_eq!(v.checked, 45);
        assert_eq!(r.manifest.install.inputs.files, 45);
    }
    assert_eq!(r.actions.len(), StageId::ALL.len());
}
