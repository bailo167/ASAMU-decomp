//! Optional decompressed-package cache for `asamu-import all --package-cache`.
//!
//! 38 of the 42 shipped packages are LZO-compressed; every stage that reads
//! packages decompresses them again (about 0.9 s per full pass on the
//! analysis machine, against about 0.1 s to open the same packages already
//! decompressed). The cache keeps each package's uncompressed stream (a valid
//! uncompressed package: `asamu_ue3` rewrites the summary with the
//! compression flags cleared) in
//!
//! ```text
//! <out>/.package-cache/
//!   index.json                     source SHA-256 + stream size per package
//!   Engine/                        (empty; marks the tree as an install root)
//!   ASAMU/<CookedDir>/<package>    one decompressed stream per package
//!   ASAMU/<CookedDir>/Maps/<map>
//! ```
//!
//! which `asamu_locate` accepts as a loose install root, so the stages that
//! read nothing but packages can be pointed at it unchanged. Texture file
//! caches are not copied: the textures stage keeps reading the original
//! install. The cache is bounded (`--package-cache-max-mib`): if every
//! package does not fit, the cache is removed and not used for the run.
//!
//! Measured on the analysis machine (Mac build 1822049, release build): the
//! six package-only stages (meshes, materials, levels, audio, matinee,
//! skeletal) finish 3.0-3.6 s sooner per forced run with a warm cache (for
//! example skeletal 1.9 s -> 1.0-1.4 s, matinee 1.25 s -> 0.9 s); building the
//! cache once takes about 1.6 s and 1.5 GB of disk. Reruns with nothing to do
//! skip every stage anyway, so the cache only pays off for repeated forced
//! or post-update imports, hence opt-in. The run manifest's output digests
//! are identical with and without the cache.
//!
//! This is decompressed original game data: it stays in the user-local
//! converted directory and must never be redistributed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use asamu_ue3::{Package, Summary};
use serde::{Deserialize, Serialize};

use super::InputFile;
use crate::safety;

/// Folder name under the converted root.
pub const DIR: &str = ".package-cache";
const INDEX: &str = "index.json";
const FORMAT: &str = "asamu-package-cache";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Index {
    format: String,
    version: u32,
    /// Importer version and build that wrote the streams (streams of another
    /// build, i.e. another decompressor, are not reused).
    tool_version: String,
    entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IndexEntry {
    /// Path relative to the cooked folder (`Core.u`, `Maps/X.asamu`).
    path: String,
    /// SHA-256 of the original package file.
    source_sha256: String,
    /// Bytes of the decompressed stream.
    stream_bytes: u64,
}

/// What [`prepare`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// Install root to hand to the stages.
    pub root: PathBuf,
    /// Packages already up to date.
    pub reused: usize,
    /// Packages decompressed and written now.
    pub written: usize,
    /// Total bytes of the cached streams.
    pub bytes: u64,
    /// Seconds spent.
    pub seconds: f64,
}

/// Outcome of [`prepare`].
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The cache is ready.
    Ready(Prepared),
    /// The packages need more than the bound; the cache was removed.
    TooLarge {
        /// Bytes the cache would need.
        needed: u64,
        /// The bound.
        limit: u64,
    },
}

/// Bring the cache under `out` up to date for `inputs` (the conversions'
/// input files; texture file caches are ignored). `cooked_name` / `maps_name`
/// are the install's cooked and maps folder names.
pub fn prepare(
    out: &Path,
    inputs: &[InputFile],
    cooked_name: &str,
    maps_name: &str,
    limit: u64,
    tool_version: &str,
) -> Result<Outcome> {
    let start = Instant::now();
    let root = out.join(DIR);
    let cooked = root.join("ASAMU").join(cooked_name);
    let packages: Vec<&InputFile> = inputs
        .iter()
        .filter(|f| {
            f.cooked_rel
                .rsplit_once('.')
                .is_some_and(|(_, e)| !e.eq_ignore_ascii_case("tfc"))
        })
        .collect();

    let old = read_index(&root.join(INDEX));
    let old_by_path: BTreeMap<&str, &IndexEntry> = old
        .as_ref()
        .filter(|i| i.format == FORMAT && i.version == VERSION && i.tool_version == tool_version)
        .map(|i| i.entries.iter().map(|e| (e.path.as_str(), e)).collect())
        .unwrap_or_default();

    // Pass 1: stream sizes (from the index when the source is unchanged,
    // else from the package summary) and the bound check, before writing.
    let mut plan = Vec::with_capacity(packages.len());
    let mut needed = 0u64;
    for f in &packages {
        let reuse = old_by_path
            .get(f.cooked_rel.as_str())
            .filter(|e| e.source_sha256 == f.sha256)
            .filter(|e| {
                file_size(&cooked_path(&cooked, maps_name, &f.cooked_rel)) == Some(e.stream_bytes)
            })
            .map(|e| e.stream_bytes);
        let stream_bytes = match reuse {
            Some(n) => n,
            None => {
                let (summary, file_len) = Summary::read_from_path(&f.path)
                    .with_context(|| format!("reading the summary of {}", f.path.display()))?;
                summary.uncompressed_stream_len().unwrap_or(file_len)
            }
        };
        needed = needed.saturating_add(stream_bytes);
        plan.push((*f, stream_bytes, reuse.is_some()));
    }
    if needed > limit {
        remove_cache(&root)?;
        return Ok(Outcome::TooLarge { needed, limit });
    }

    // Pass 2: write what is missing or stale.
    std::fs::create_dir_all(root.join("Engine"))
        .with_context(|| format!("creating {}", root.display()))?;
    std::fs::create_dir_all(cooked.join(maps_name))
        .with_context(|| format!("creating {}", cooked.display()))?;
    let mut entries = Vec::with_capacity(plan.len());
    let (mut reused, mut written) = (0usize, 0usize);
    // The bound also holds for the bytes actually written: a summary's
    // uncompressed length is only a claim of the (untrusted) file.
    let mut actual = 0u64;
    for (f, stream_bytes, reuse) in plan {
        let target = cooked_path(&cooked, maps_name, &f.cooked_rel);
        let stream_bytes = if reuse {
            reused += 1;
            stream_bytes
        } else {
            let pkg = Package::open(&f.path)
                .with_context(|| format!("decompressing {}", f.path.display()))?;
            let stream = pkg.into_stream();
            let len = u64::try_from(stream.len()).unwrap_or(u64::MAX);
            if actual.saturating_add(len) > limit {
                remove_cache(&root)?;
                return Ok(Outcome::TooLarge {
                    needed: actual.saturating_add(len).max(needed),
                    limit,
                });
            }
            remove_regular_file(&target)?;
            let checked = safety::check_output_path(&target, &f.path, false)?;
            safety::write_output(&checked, &stream, false)?;
            written += 1;
            len
        };
        actual = actual.saturating_add(stream_bytes);
        entries.push(IndexEntry {
            path: f.cooked_rel.clone(),
            source_sha256: f.sha256.clone(),
            stream_bytes,
        });
    }
    // Drop cached packages that are no longer inputs.
    let keep: std::collections::BTreeSet<String> = entries
        .iter()
        .map(|e| e.path.to_ascii_lowercase())
        .collect();
    for (dir, prefix) in [
        (cooked.clone(), String::new()),
        (cooked.join(maps_name), "Maps/".to_owned()),
    ] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_file = e.file_type().is_ok_and(|t| t.is_file());
            if is_file
                && super::inventory::is_input_name(&name)
                && !keep.contains(&format!("{prefix}{name}").to_ascii_lowercase())
            {
                remove_regular_file(&e.path())?;
            }
        }
    }
    let bytes = actual;
    let index = Index {
        format: FORMAT.to_owned(),
        version: VERSION,
        tool_version: tool_version.to_owned(),
        entries,
    };
    let json = serde_json::to_string_pretty(&index)?;
    let index_path = root.join(INDEX);
    let checked = safety::check_output_path(&index_path, &root, true)?;
    safety::write_output(&checked, json.as_bytes(), true)?;
    Ok(Outcome::Ready(Prepared {
        root,
        reused,
        written,
        bytes,
        seconds: start.elapsed().as_secs_f64(),
    }))
}

/// Remove the cache folder (it holds only data this module wrote).
pub fn remove_cache(root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(root) {
        Ok(m) if m.file_type().is_symlink() => {
            bail!(
                "{} is a symbolic link; refusing to touch it",
                root.display()
            )
        }
        Ok(m) if m.is_dir() => {
            std::fs::remove_dir_all(root).with_context(|| format!("removing {}", root.display()))
        }
        Ok(_) => bail!("{} is not a directory", root.display()),
        Err(_) => Ok(()),
    }
}

/// Total bytes of an existing cache folder (0 when absent).
pub fn existing_bytes(out: &Path) -> u64 {
    super::dir_totals(&out.join(DIR)).1
}

fn cooked_path(cooked: &Path, maps_name: &str, cooked_rel: &str) -> PathBuf {
    match cooked_rel.split_once('/') {
        Some((_, name)) => cooked.join(maps_name).join(name),
        None => cooked.join(cooked_rel),
    }
}

fn file_size(p: &Path) -> Option<u64> {
    std::fs::symlink_metadata(p)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

fn remove_regular_file(p: &Path) -> Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_file() || m.file_type().is_symlink() => {
            std::fs::remove_file(p).with_context(|| format!("removing {}", p.display()))
        }
        Ok(_) => bail!("{} is not a regular file", p.display()),
        Err(_) => Ok(()),
    }
}

fn read_index(p: &Path) -> Option<Index> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}
