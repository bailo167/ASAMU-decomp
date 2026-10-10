//! Deterministic runtime for the subset of UE3 Kismet (sequences, events,
//! actions, conditions, variables) and Matinee playback used by ASAMU
//! levels.
//!
//! - [`graph`]: the importer's per-map runtime graph
//!   (`<converted>/kismet/<map>.kismet.json`), validated.
//! - [`matinee`]: the importer's Matinee export and the Matinee evaluators
//!   (a port of `asamu_ue3::matinee`, cross-checked by probes).
//! - [`runtime`]: the interpreter ([`Runtime`]): UE3's op scheduling,
//!   events, variables, latent actions; [`classes`](crate::runtime) holds the
//!   per-class behaviour.
//! - [`host`]: the game side: [`Host`] for game-affecting effects, [`Output`]
//!   for presentation events.
//! - [`narrator`]: the narrator manager that `SeqAct_NarratorLine` drives.
//! - [`anim`]: Matinee animation-control tracks (`SetAnimPosition` calls).
//! - [`camera_anim`]: camera animations (`CameraAnim` assets, the camera's
//!   animation pool and blending, the gameplay script's calls).
//!
//! Behaviour, evidence and confidence for every class:
//! `docs/reverse-engineering/KISMET_RUNTIME.md`. Everything is deterministic:
//! ordered containers only, no wall clock, seeded random variables.

pub mod anim;
pub mod camera_anim;
mod classes;
pub mod graph;
pub mod host;
mod interp;
pub mod matinee;
pub mod narrator;
pub mod ops;
pub mod runtime;
pub mod value;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use graph::{ActorInfo, ActorRef, Graph, GraphError, RUNTIME_FORMAT, RUNTIME_VERSION};
pub use host::{Host, NullHost, Output, PropertyValue, ToggleMode};
pub use matinee::MatineeSet;
pub use ops::{IMPLEMENTED, OpClass};
pub use runtime::{Obj, Runtime, RuntimeStats};
pub use value::KValue;

/// Largest runtime graph or Matinee file read (bytes).
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Errors loading a level's scripts from a converted-data directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// No runtime graph for the map.
    #[error("no converted Kismet graph for {0} (run `asamu-import kismet`)")]
    Missing(String),
    /// A file could not be read.
    #[error("{path}: {message}")]
    Io {
        /// File.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// The graph is invalid.
    #[error("{path}: {source}")]
    Graph {
        /// File.
        path: String,
        /// Problem.
        source: GraphError,
    },
    /// The Matinee file is invalid.
    #[error("{path}: {message}")]
    Matinee {
        /// File.
        path: String,
        /// Problem.
        message: String,
    },
}

/// Case-insensitive lookup of `<dir>/<sub>/<name><suffix>`.
fn find_file(dir: &Path, sub: &str, name: &str, suffix: &str) -> Option<PathBuf> {
    let folder = dir.join(sub);
    let want = format!("{name}{suffix}").to_ascii_lowercase();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.into_iter().find(|p| {
        p.file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.to_ascii_lowercase() == want)
    })
}

fn read_limited(path: &Path) -> Result<Vec<u8>, LoadError> {
    let io = |e: std::io::Error| LoadError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    };
    let meta = std::fs::metadata(path).map_err(io)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(LoadError::Io {
            path: path.display().to_string(),
            message: format!("{} bytes exceeds the limit", meta.len()),
        });
    }
    std::fs::read(path).map_err(io)
}

/// Level scripts loaded from a converted-data directory.
#[derive(Debug, Clone)]
pub struct LevelScripts {
    /// The merged graph (persistent level first, then the sub-levels found).
    pub graph: Graph,
    /// Matinee data (node ids in merged-graph numbering).
    pub matinee: MatineeSet,
    /// Sub-levels whose graph was not found (no Kismet of their own, or not
    /// converted).
    pub missing_sublevels: Vec<String>,
}

/// Loads `<dir>/kismet/<map>.kismet.json` (required), the sub-levels'
/// graphs (optional) and the matching `<dir>/matinee/*.matinee.json`
/// (optional), merged into one graph.
///
/// # Errors
/// The map's graph is missing or invalid, or a Matinee file is invalid.
pub fn load_level_scripts(
    dir: &Path,
    map: &str,
    sublevels: &[String],
) -> Result<LevelScripts, LoadError> {
    let main_path = find_file(dir, "kismet", map, ".kismet.json")
        .ok_or_else(|| LoadError::Missing(map.to_owned()))?;
    let graph_of = |path: &Path| -> Result<Graph, LoadError> {
        Graph::from_json_slice(&read_limited(path)?).map_err(|source| LoadError::Graph {
            path: path.display().to_string(),
            source,
        })
    };
    let main = graph_of(&main_path)?;
    let mut matinee = MatineeSet::default();
    let mut offsets: Vec<(String, usize)> = vec![(map.to_owned(), 0)];
    let mut subs = Vec::new();
    let mut missing = Vec::new();
    let mut next = main.nodes.len();
    for s in sublevels {
        match find_file(dir, "kismet", s, ".kismet.json") {
            Some(p) => {
                let g = graph_of(&p)?;
                offsets.push((s.clone(), next));
                next += g.nodes.len();
                subs.push(g);
            }
            None => missing.push(s.clone()),
        }
    }
    for (name, off) in &offsets {
        if let Some(p) = find_file(dir, "matinee", name, ".matinee.json") {
            matinee
                .add_json(&read_limited(&p)?, *off)
                .map_err(|e| LoadError::Matinee {
                    path: p.display().to_string(),
                    message: e.to_string(),
                })?;
        }
    }
    Ok(LevelScripts {
        graph: main.merge(subs),
        matinee,
        missing_sublevels: missing,
    })
}

/// Coverage of a graph's classes by the interpreter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Op and variable classes present (short name → nodes).
    pub present: BTreeMap<String, usize>,
    /// Of those, classes with interpreter behaviour.
    pub implemented: BTreeMap<String, usize>,
    /// Classes without interpreter behaviour (run with generic semantics).
    pub missing: BTreeMap<String, usize>,
}

/// Which of the graph's classes the interpreter implements.
#[must_use]
pub fn coverage(graph: &Graph) -> Coverage {
    let mut c = Coverage::default();
    for n in &graph.nodes {
        // Frames, prefab archetypes and detached objects are not executed.
        if n.kind == graph::NodeKind::Other {
            continue;
        }
        let name = n.class_short().to_owned();
        *c.present.entry(name.clone()).or_insert(0) += 1;
        if n.class == OpClass::Unknown {
            *c.missing.entry(name).or_insert(0) += 1;
        } else {
            *c.implemented.entry(name).or_insert(0) += 1;
        }
    }
    c
}
