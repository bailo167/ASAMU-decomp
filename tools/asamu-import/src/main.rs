//! asamu-import — convert data from the user's own legitimate install of
//! *A Story About My Uncle* into user-local, runtime-friendly formats.
//!
//! Converted data is derived from copyrighted game files: it is written to a
//! user-local directory, never into this repository and never into the game
//! install, and must not be redistributed.
//!
//! Each conversion lives in its own module (owned by one workstream):
//! - [`textures`]: Texture2D → DDS (+ optional PNG previews)
//! - [`meshes`]: StaticMesh → glTF 2.0
//! - [`levels`]: map actors/volumes/collision → scene description

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod levels;
mod meshes;
#[path = "../../asamu-inspect/src/safety.rs"]
#[allow(dead_code)]
mod safety;
mod textures;

#[derive(Parser, Debug)]
#[command(about = "Convert a legitimate ASAMU install into user-local runtime data")]
struct Cli {
    /// Root of the original install (defaults to Steam discovery / ASAMU_ORIGINAL_DIR).
    #[arg(long, global = true)]
    original: Option<PathBuf>,
    /// Output directory for converted data (defaults to the user data directory).
    #[arg(long, global = true)]
    out: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Convert textures.
    Textures(textures::Args),
    /// Convert static meshes.
    Meshes(meshes::Args),
    /// Extract level scene descriptions.
    Levels(levels::Args),
}

/// Default user-local output directory (no extra crates): per-OS data dir + `asamu-decomp/converted`.
pub fn default_output_dir() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    Some(base.join("asamu-decomp").join("converted"))
}

/// Shared context passed to every conversion.
pub struct Ctx {
    pub original: Option<PathBuf>,
    pub out: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let out = match cli.out {
        Some(o) => o,
        None => default_output_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot determine a user data directory; pass --out"))?,
    };
    let ctx = Ctx {
        original: cli.original,
        out,
    };
    match cli.cmd {
        Cmd::Textures(a) => textures::run(&ctx, a),
        Cmd::Meshes(a) => meshes::run(&ctx, a),
        Cmd::Levels(a) => levels::run(&ctx, a),
    }
}
