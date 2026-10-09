//! `asamu-inventory` — sanitized metadata inventory of an original ASAMU install.
//!
//! ```text
//! asamu-inventory                       # locate via Steam / ASAMU_ORIGINAL_DIR, JSON to stdout
//! asamu-inventory --out inv.json --summary summary.md
//! asamu-inventory --root "/path/to/A Story About My Uncle" --summary
//! ```
//!
//! The install is only read. Outputs contain relative paths, sizes, hashes and types only.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use asamu_inventory::{DepotInfo, Inventory, ScanOptions, SteamInfo, scan, to_json, to_markdown};
use asamu_locate::Install;
use clap::Parser;

/// Inventory an original A Story About My Uncle install (read-only).
#[derive(Debug, Parser)]
#[command(name = "asamu-inventory", version)]
struct Cli {
    /// Install root to inventory. Default: located through ASAMU_ORIGINAL_DIR or Steam.
    #[arg(long, value_name = "PATH")]
    root: Option<PathBuf>,
    /// Write the JSON inventory to FILE.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Write a Markdown summary to FILE (or stdout when FILE is omitted or `-`).
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "-")]
    summary: Option<PathBuf>,
    /// Abort if the root holds more than this many files.
    #[arg(long, default_value_t = 50_000)]
    max_files: usize,
    /// Skip the cooker TOC cross-check.
    #[arg(long)]
    no_toc: bool,
}

fn steam_info(install: &Install) -> Option<SteamInfo> {
    install.manifest.as_ref()?;
    Some(SteamInfo {
        app_id: install.app_id,
        build_id: install.build_id,
        depots: install
            .depots
            .iter()
            .map(|d| DepotInfo {
                depot_id: d.depot_id,
                manifest_id: d.manifest_id.map(|m| m.to_string()),
            })
            .collect(),
    })
}

/// Refuse to write outputs inside the install: it must never be modified.
///
/// `roots` are every folder that belongs to the install (the scanned root and, when
/// `--root` named the `.app` bundle, the detected install folder around it). The output's
/// parent folder is resolved through symlinks, and an existing output path is resolved too,
/// so a symlink pointing into the install is refused as well. Canonical paths on macOS are
/// case-normalized, so case-insensitive spellings cannot slip past the prefix check.
fn ensure_outside(roots: &[&Path], out: &Path) -> Result<()> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let parent = fs::canonicalize(&parent).unwrap_or(parent);
    let target = fs::canonicalize(out).ok();
    for root in roots {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let inside =
            parent.starts_with(&root) || target.as_ref().is_some_and(|t| t.starts_with(&root));
        if inside {
            bail!(
                "refusing to write {} inside the original install ({})",
                out.display(),
                root.display()
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (root, install) = match &cli.root {
        Some(root) => (root.clone(), asamu_locate::from_original_dir(root).ok()),
        None => {
            let install = asamu_locate::locate().context("locating the original install")?;
            (install.root.clone(), Some(install))
        }
    };
    let mut protected: Vec<&Path> = vec![root.as_path()];
    if let Some(install) = &install {
        protected.push(install.root.as_path());
    }
    for path in [&cli.out, &cli.summary].into_iter().flatten() {
        if path.as_os_str() != "-" {
            ensure_outside(&protected, path)?;
        }
    }

    let started = Instant::now();
    let mut inventory: Inventory = scan(
        &root,
        &ScanOptions {
            max_files: cli.max_files,
            toc: !cli.no_toc,
        },
    )?;
    if let Some(install) = &install {
        inventory.layout = Some(install.layout.label().to_string());
        inventory.steam = steam_info(install);
    }
    eprintln!(
        "asamu-inventory: {} files, {} bytes in {:.1}s",
        inventory.files.len(),
        inventory.total_bytes(),
        started.elapsed().as_secs_f64()
    );

    let json = to_json(&inventory)?;
    match &cli.out {
        Some(out) => fs::write(out, &json).with_context(|| format!("write {}", out.display()))?,
        None if cli.summary.is_none() => print!("{json}"),
        None => {}
    }
    if let Some(summary) = &cli.summary {
        let md = to_markdown(&inventory);
        if summary.as_os_str() == "-" {
            print!("{md}");
        } else {
            fs::write(summary, md).with_context(|| format!("write {}", summary.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn outputs_inside_any_protected_root_are_refused() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("Game");
        let bundle = install.join("G.app");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        let outside = tmp.path().join("out");
        fs::create_dir_all(&outside).unwrap();

        assert!(ensure_outside(&[&bundle], &bundle.join("Contents/inv.json")).is_err());
        // `--root` named the bundle: the install folder around it is protected too.
        assert!(ensure_outside(&[&bundle], &install.join("inv.json")).is_ok());
        assert!(ensure_outside(&[&bundle, &install], &install.join("inv.json")).is_err());
        assert!(ensure_outside(&[&bundle, &install], &outside.join("inv.json")).is_ok());
        // A dotted path that resolves inside is still inside.
        let dotted = outside.join("..").join("Game").join("x.json");
        assert!(ensure_outside(&[&install], &dotted).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_into_the_install_are_refused() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("Game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("victim.txt"), b"original").unwrap();
        let outside = tmp.path().join("out");
        fs::create_dir_all(&outside).unwrap();
        // A symlinked output folder and a symlinked output file both point into the install.
        std::os::unix::fs::symlink(&install, outside.join("dir-link")).unwrap();
        std::os::unix::fs::symlink(install.join("victim.txt"), outside.join("file-link")).unwrap();
        assert!(ensure_outside(&[&install], &outside.join("dir-link/inv.json")).is_err());
        assert!(ensure_outside(&[&install], &outside.join("file-link")).is_err());
        assert!(ensure_outside(&[&install], &outside.join("fresh.json")).is_ok());
    }
}
