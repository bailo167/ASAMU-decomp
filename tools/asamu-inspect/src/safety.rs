//! Guard rails for writing derived data (e.g. a decompressed stream).
//!
//! Decompressed packages are original game data and must never land in the
//! public repository outside its git-ignored `research/` subdirectories, nor
//! inside the game install.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Repository root as known at compile time (`tools/asamu-inspect/../..`).
fn compile_time_repo_root() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent()?.parent()?.canonicalize().ok()
}

/// `research/` subdirectories that the repository's `.gitignore` ignores.
/// Derived game data may only be written inside the repository under one of these.
pub const IGNORED_RESEARCH_DIRS: &[&str] = &[
    "local",
    "raw",
    "ghidra",
    "decompiled",
    "extracted",
    "symbol-dumps",
    "decompressed",
    "strings",
];

/// True if `dir` looks like the root of this repository.
fn is_repo_root(dir: &Path) -> bool {
    dir.join("crates")
        .join("asamu-ue3")
        .join("Cargo.toml")
        .is_file()
        && dir.join("Cargo.toml").is_file()
}

/// Validate an output path for derived game data and return its resolved form.
///
/// Refuses: a missing parent directory, the input file itself, anything inside
/// an `.app` bundle or a `steamapps` tree, anything inside this repository
/// except under a git-ignored `research/` subdirectory (see
/// [`IGNORED_RESEARCH_DIRS`]), an existing symlink or non-regular file at the
/// target (a link could redirect the write into the game install), and
/// existing files unless `force`.
pub fn check_output_path(out: &Path, input: &Path, force: bool) -> Result<PathBuf> {
    let file_name = out
        .file_name()
        .with_context(|| format!("output path {} has no file name", out.display()))?;
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let parent = parent.canonicalize().with_context(|| {
        format!(
            "output directory {} does not exist (create it first)",
            parent.display()
        )
    })?;
    let target = parent.join(file_name);

    if let Ok(input) = input.canonicalize()
        && input == target
    {
        bail!("refusing to overwrite the input file");
    }
    for anc in parent.ancestors() {
        let name = anc
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.ends_with(".app") || name == "steamapps" {
            bail!(
                "refusing to write inside {} (looks like a game install)",
                anc.display()
            );
        }
    }
    let mut roots: Vec<PathBuf> = compile_time_repo_root().into_iter().collect();
    roots.extend(
        parent
            .ancestors()
            .filter(|a| is_repo_root(a))
            .map(Path::to_path_buf),
    );
    for root in &roots {
        let allowed = IGNORED_RESEARCH_DIRS
            .iter()
            .any(|d| target.starts_with(root.join("research").join(d)));
        if target.starts_with(root) && !allowed {
            bail!(
                "refusing to write original-game-derived data inside the repository ({}); \
                 use a git-ignored path such as research/local/ or a path outside the repo",
                root.display()
            );
        }
    }
    match std::fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => bail!(
            "{} exists and is not a regular file (symlinks are refused)",
            target.display()
        ),
        Ok(_) if !force => bail!(
            "{} already exists (pass --force to overwrite)",
            target.display()
        ),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("checking {}", target.display()));
        }
    }
    Ok(target)
}

/// Write `data` to a target validated by [`check_output_path`] without ever
/// writing through an existing directory entry.
///
/// Without `force` the file must not exist (`create_new`, which also refuses a
/// symlink, even a dangling one). With `force` the data goes to a fresh
/// temporary file in the same directory that is then renamed over the target:
/// the rename replaces the directory entry, so a symlink or hard link that
/// appeared at the target cannot redirect the write into another file (such as
/// a package in the game install).
pub fn write_output(target: &Path, data: &[u8], force: bool) -> Result<()> {
    if !force {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .with_context(|| format!("creating {}", target.display()))?;
        return f
            .write_all(data)
            .with_context(|| format!("writing {}", target.display()));
    }
    let file_name = target
        .file_name()
        .with_context(|| format!("output path {} has no file name", target.display()))?;
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = parent.join(format!(
        ".{}.asamu-inspect-{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .with_context(|| format!("creating temporary file {}", tmp.display()))?;
    // From here on `tmp` is a file this call created; it is the only thing the
    // error path ever removes.
    let written = f
        .write_all(data)
        .and_then(|()| f.sync_all())
        .and_then(|()| {
            drop(f);
            std::fs::rename(&tmp, target)
        });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", target.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_repo_paths_outside_research() {
        let Some(root) = compile_time_repo_root() else {
            return;
        };
        let input = root.join("Cargo.toml");
        assert!(check_output_path(&root.join("decompressed.bin"), &input, false).is_err());
        assert!(check_output_path(&root.join("docs").join("x.bin"), &input, false).is_err());
        // research/ itself is tracked: refused. research/local/ is ignored: allowed.
        let research = root.join("research");
        if research.is_dir() {
            let name = "asamu-inspect-safety-test-nonexistent.bin";
            assert!(check_output_path(&research.join(name), &input, false).is_err());
            let local = research.join("local");
            if local.is_dir() {
                let ok = check_output_path(&local.join(name), &input, false);
                assert!(ok.is_ok(), "{ok:?}");
            }
        }
    }

    #[test]
    fn refuses_app_bundles_and_missing_dirs() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "asamu-inspect-safety-{}-{nanos}",
            std::process::id()
        ));
        let app = dir.join("Game.app").join("Contents");
        std::fs::create_dir_all(&app).unwrap();
        let input = dir.join("in.u");
        assert!(check_output_path(&app.join("out.bin"), &input, false).is_err());
        assert!(check_output_path(&dir.join("nope").join("out.bin"), &input, false).is_err());
        let fine = dir.join("out.bin");
        assert!(check_output_path(&fine, &input, false).is_ok());
        std::fs::write(&fine, b"x").unwrap();
        assert!(check_output_path(&fine, &input, false).is_err());
        assert!(check_output_path(&fine, &input, true).is_ok());
        assert!(check_output_path(&fine, &fine, true).is_err());
        // A directory at the target is refused even with --force.
        let sub = dir.join("subdir");
        std::fs::create_dir(&sub).unwrap();
        assert!(check_output_path(&sub, &input, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "asamu-inspect-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_output_never_writes_through_links() {
        let dir = scratch_dir("write");
        let input = dir.join("in.u");

        // New file without --force; a second write without --force fails.
        let out = dir.join("out.bin");
        write_output(&out, b"one", false).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"one");
        assert!(write_output(&out, b"two", false).is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"one");

        // --force replaces the directory entry: a hard link to a protected
        // file keeps the protected content.
        let protected = dir.join("protected.u");
        std::fs::write(&protected, b"original").unwrap();
        let linked = dir.join("linked.bin");
        std::fs::hard_link(&protected, &linked).unwrap();
        let target = check_output_path(&linked, &input, true).unwrap();
        write_output(&target, b"derived", true).unwrap();
        assert_eq!(std::fs::read(&linked).unwrap(), b"derived");
        assert_eq!(std::fs::read(&protected).unwrap(), b"original");
        // No temporary files are left behind.
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_targets_are_refused() {
        let dir = scratch_dir("symlink");
        let input = dir.join("in.u");
        let protected = dir.join("protected.u");
        std::fs::write(&protected, b"original").unwrap();

        // Existing link to an existing file: refused with and without --force.
        let link = dir.join("link.bin");
        std::os::unix::fs::symlink(&protected, &link).unwrap();
        assert!(check_output_path(&link, &input, false).is_err());
        assert!(check_output_path(&link, &input, true).is_err());

        // Dangling link (exists() is false for it): still refused, and the
        // non-force writer will not create the link's destination either.
        let dangling = dir.join("dangling.bin");
        let dest = dir.join("would-be-created.u");
        std::os::unix::fs::symlink(&dest, &dangling).unwrap();
        assert!(check_output_path(&dangling, &input, false).is_err());
        assert!(check_output_path(&dangling, &input, true).is_err());
        assert!(write_output(&dangling, b"x", false).is_err());
        assert!(!dest.exists());

        // Even if a link appears after validation, --force replaces the link
        // itself rather than writing through it.
        let late = dir.join("late.bin");
        let target = check_output_path(&late, &input, true).unwrap();
        std::os::unix::fs::symlink(&protected, &late).unwrap();
        write_output(&target, b"derived", true).unwrap();
        assert_eq!(std::fs::read(&protected).unwrap(), b"original");
        assert!(
            !std::fs::symlink_metadata(&late)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
