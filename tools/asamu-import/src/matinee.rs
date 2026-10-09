//! `asamu-import matinee`: per-map Matinee (`InterpData`) export.
//!
//! For every map package (or the ones named with `--map`) that holds any
//! Matinee data, this writes into `<out>/matinee/` (user-local; never the
//! repository or the game install):
//!
//! - `<map>.matinee.json` — [`asamu_ue3::matinee::MatineeMap`]: every
//!   `SeqAct_Interp` with its effective settings (play rate, looping,
//!   rewind, forced start), its `InterpData`, its group bindings (the
//!   actors each group drives, by object path and class) and property
//!   links; every `InterpData` with its groups and tracks (move curves and
//!   split axis sub-tracks, event keys, director cuts, sounds, property /
//!   material / skeletal-control curves, fades, anim keys, toggles,
//!   visibility keys, ...); `CameraAnim` assets; publishable coverage
//!   counts.
//! - `manifest.json` — per-map counts.
//!
//! Curve keys are written as stored (`in`, `out`, `arrive`, `leave`,
//! `mode`); evaluate them with `asamu_ue3::matinee::InterpCurve::eval`,
//! which follows the original engine's arithmetic.
//!
//! All of this is derived from copyrighted game data: keep it local and do
//! not redistribute it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::matinee::{self, MatineeCoverage, MatineeMap};
use asamu_ue3::model::PackageSet;
use serde::Serialize;

use crate::levels::{prepare_dir, select_maps};
use crate::safety;

/// `format` of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "asamu-matinee-manifest";

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive, e.g. AG-IceCave). Repeat
    /// for several; default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
    /// Also write maps without any Matinee data (an empty document).
    #[arg(long)]
    include_empty: bool,
}

/// One map in `manifest.json` (counts only).
#[derive(Debug, Serialize)]
pub struct ManifestEntry {
    /// Map package name.
    pub map: String,
    /// File written (none when the map has no Matinee data).
    pub file: Option<String>,
    /// Coverage counts.
    pub coverage: MatineeCoverage,
}

/// `manifest.json`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: &'static str,
    /// [`matinee::MATINEE_VERSION`] of the map files.
    pub matinee_version: u32,
    /// Maps, in file-name order.
    pub maps: Vec<ManifestEntry>,
}

/// Cooked package folder and root of the install.
fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.root))
}

fn to_json<T: Serialize>(v: &T, pretty: bool) -> Result<Vec<u8>> {
    Ok(if pretty {
        serde_json::to_vec_pretty(v)?
    } else {
        serde_json::to_vec(v)?
    })
}

fn write_file(dir: &Path, name: &str, data: &[u8], input: &Path, force: bool) -> Result<PathBuf> {
    let target = safety::check_output_path(&dir.join(name), input, force)?;
    safety::write_output(&target, data, force)?;
    Ok(target)
}

/// True when the map holds anything worth a file.
pub fn has_content(m: &MatineeMap) -> bool {
    !m.actions.is_empty() || !m.interp_data.is_empty() || !m.camera_anims.is_empty()
}

/// Decode one map (a fresh package set per map keeps memory bounded).
pub fn convert_map(file: &Path, cooked: &Path) -> Result<MatineeMap> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    Ok(matinee::extract_for(&set, &lp))
}

/// One summary line per map (counts only).
pub fn summary_line(m: &MatineeMap) -> String {
    let c = &m.coverage;
    format!(
        "{:<20} actions {:>3}  data {:>3}  groups {:>4}  tracks {:>4} (decoded {:>4}, unknown {})  \
         bindings {:>3}  camera anims {}  warnings {}",
        m.package,
        c.actions,
        c.interp_data,
        c.groups.values().sum::<usize>(),
        c.tracks_total,
        c.tracks_decoded,
        c.tracks_unknown,
        c.bindings,
        c.camera_anims,
        c.warnings
    )
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install_root) = install_dirs(ctx)?;
    let maps_dir = cooked.join("Maps");
    let maps = select_maps(&maps_dir, &args.maps)?;
    if maps.is_empty() {
        bail!("no map packages in {}", maps_dir.display());
    }
    let out_dir = prepare_dir(&ctx.out.join("matinee"), &maps_dir, &install_root)?;
    eprintln!(
        "writing Matinee data derived from your own install to {} (do not redistribute)",
        out_dir.display()
    );
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        matinee_version: matinee::MATINEE_VERSION,
        maps: Vec::new(),
    };
    for file in &maps {
        let m = convert_map(file, &cooked)?;
        println!("{}", summary_line(&m));
        let file_name = if has_content(&m) || args.include_empty {
            let name = format!("{}.matinee.json", m.package);
            write_file(
                &out_dir,
                &name,
                &to_json(&m, args.pretty)?,
                file,
                args.force,
            )?;
            Some(name)
        } else {
            None
        };
        manifest.maps.push(ManifestEntry {
            map: m.package.clone(),
            file: file_name,
            coverage: m.coverage,
        });
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
    use asamu_ue3::matinee::{MATINEE_FORMAT, MATINEE_VERSION};

    fn empty(name: &str) -> MatineeMap {
        MatineeMap {
            format: MATINEE_FORMAT.to_owned(),
            version: MATINEE_VERSION,
            package: name.to_owned(),
            actions: Vec::new(),
            interp_data: Vec::new(),
            camera_anims: Vec::new(),
            coverage: MatineeCoverage::default(),
            orphans: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn empty_maps_have_no_content_and_summarise() {
        let m = empty("ASAMULegal");
        assert!(!has_content(&m));
        let line = summary_line(&m);
        assert!(line.starts_with("ASAMULegal"));
        assert!(line.contains("tracks    0"));
    }

    #[test]
    fn manifest_json_shape_is_stable() {
        let manifest = Manifest {
            format: MANIFEST_FORMAT,
            matinee_version: MATINEE_VERSION,
            maps: vec![ManifestEntry {
                map: "AG-Test".to_owned(),
                file: None,
                coverage: MatineeCoverage::default(),
            }],
        };
        let v: serde_json::Value =
            serde_json::from_slice(&to_json(&manifest, false).unwrap()).unwrap();
        assert_eq!(v["format"], MANIFEST_FORMAT);
        assert_eq!(v["matinee_version"], MATINEE_VERSION);
        assert_eq!(v["maps"][0]["map"], "AG-Test");
        assert!(v["maps"][0]["file"].is_null());
        assert_eq!(v["maps"][0]["coverage"]["tracks_total"], 0);
        let doc: serde_json::Value =
            serde_json::from_slice(&to_json(&empty("AG-Test"), true).unwrap()).unwrap();
        assert_eq!(doc["format"], MATINEE_FORMAT);
        assert_eq!(doc["version"], MATINEE_VERSION);
        assert!(doc["actions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn output_is_refused_inside_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        let input = root.join("Cargo.toml");
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("no-install");
        assert!(prepare_dir(&root.join("matinee-test-out"), &input, &install).is_err());
        assert!(!root.join("matinee-test-out").exists());
        let ok = prepare_dir(&tmp.path().join("conv").join("matinee"), &input, &install).unwrap();
        let f = write_file(&ok, "m.matinee.json", b"{}", &input, false).unwrap();
        assert!(write_file(&ok, "m.matinee.json", b"{}", &input, false).is_err());
        assert!(write_file(&ok, "m.matinee.json", b"[]", &input, true).is_ok());
        assert_eq!(std::fs::read(f).unwrap(), b"[]");
    }

    /// The writer never lands in the game install or follows a symlink:
    /// an output folder inside the install, a symlinked output folder that
    /// leads into it, and a symlink planted at a target file are refused
    /// (the last even with `--force`), and the input stays untouched.
    #[cfg(unix)]
    #[test]
    fn output_refuses_the_install_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("Game");
        std::fs::create_dir_all(install.join("Maps")).unwrap();
        let input = install.join("Maps").join("AG-Test.asamu");
        std::fs::write(&input, b"x").unwrap();
        assert!(prepare_dir(&install.join("out").join("matinee"), &input, &install).is_err());
        assert!(!install.join("out").exists());
        let conv = tmp.path().join("conv");
        std::fs::create_dir_all(&conv).unwrap();
        std::os::unix::fs::symlink(&install, conv.join("matinee")).unwrap();
        assert!(prepare_dir(&conv.join("matinee"), &input, &install).is_err());
        let ok = prepare_dir(&tmp.path().join("conv2").join("matinee"), &input, &install).unwrap();
        std::os::unix::fs::symlink(&input, ok.join("AG-Test.matinee.json")).unwrap();
        assert!(write_file(&ok, "AG-Test.matinee.json", b"{}", &input, true).is_err());
        assert!(write_file(&ok, "AG-Test.matinee.json", b"{}", &input, false).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), b"x");
        // The manifest is rewritten in place (force) without following links.
        let m = write_file(&ok, "manifest.json", b"{}", &input, true).unwrap();
        assert!(write_file(&ok, "manifest.json", b"[]", &input, true).is_ok());
        assert_eq!(std::fs::read(m).unwrap(), b"[]");
    }

    /// Gated on the user's install (skips without it): one map converts to
    /// JSON whose actions all name a decoded `InterpData`, converting twice
    /// gives identical bytes, and the run writes only into the temporary
    /// output directory.
    #[test]
    fn real_map_converts_to_consistent_json() {
        let Ok(install) = asamu_locate::locate() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let cooked = install.cooked_dir.clone();
        let Ok(maps) = select_maps(&cooked.join("Maps"), &["AG-IceCave".to_owned()]) else {
            eprintln!("SKIP: AG-IceCave not found");
            return;
        };
        let m = convert_map(&maps[0], &cooked).unwrap();
        assert!(has_content(&m));
        let tmp = tempfile::tempdir().unwrap();
        let dir = prepare_dir(&tmp.path().join("matinee"), &maps[0], &install.root).unwrap();
        let bytes = to_json(&m, false).unwrap();
        // Deterministic: a second, independent conversion is byte-identical.
        let again = convert_map(&maps[0], &cooked).unwrap();
        assert_eq!(to_json(&again, false).unwrap(), bytes);
        let f = write_file(&dir, "AG-IceCave.matinee.json", &bytes, &maps[0], false).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap();
        assert_eq!(doc["format"], matinee::MATINEE_FORMAT);
        let data: Vec<&str> = doc["interp_data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["path"].as_str().unwrap())
            .collect();
        let actions = doc["actions"].as_array().unwrap();
        assert_eq!(actions.len(), m.coverage.actions);
        for a in actions {
            assert!(data.contains(&a["interp_data"].as_str().unwrap()));
        }
        // Curve keys carry the documented field names.
        let first_point =
            &doc["interp_data"][0]["groups"][0]["tracks"][0]["data"]["pos"]["points"][0];
        for k in ["in", "out", "arrive", "leave", "mode"] {
            assert!(!first_point[k].is_null(), "{k}");
        }
    }
}
