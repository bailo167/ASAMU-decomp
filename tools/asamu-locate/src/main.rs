//! `asamu-locate` — print where the original *A Story About My Uncle* install lives.
//!
//! Output contains local absolute paths: it is for the user's terminal, not for committing.

use std::path::PathBuf;
use std::process::ExitCode;

use asamu_locate::{Discovery, Install, LocateOptions, locate_with, steam_root_candidates};
use clap::Parser;

/// Locate a legitimate Steam install of A Story About My Uncle (App ID 278360).
#[derive(Debug, Parser)]
#[command(name = "asamu-locate", version)]
struct Cli {
    /// Print machine-readable JSON instead of a summary.
    #[arg(long)]
    json: bool,
    /// Use this install root instead of Steam discovery (same as ASAMU_ORIGINAL_DIR).
    #[arg(long, value_name = "PATH")]
    original_dir: Option<PathBuf>,
    /// Use this Steam root instead of the OS defaults (same as ASAMU_STEAM_ROOT).
    #[arg(long, value_name = "PATH")]
    steam_root: Option<PathBuf>,
    /// Only list the Steam roots that would be searched, then exit.
    #[arg(long)]
    candidates: bool,
}

fn opt_path(p: Option<&PathBuf>) -> String {
    p.map_or_else(|| "(none)".to_string(), |p| p.display().to_string())
}

fn print_summary(install: &Install) {
    println!("A Story About My Uncle (Steam App ID {})", install.app_id);
    println!("  root           {}", install.root.display());
    println!("  layout         {}", install.layout.label());
    match &install.discovery {
        Discovery::OriginalDir { given } => {
            println!(
                "  found via      ASAMU_ORIGINAL_DIR / --original-dir ({})",
                given.display()
            );
        }
        Discovery::Steam {
            steam_root,
            library,
            manifest_path,
        } => {
            println!("  found via      Steam root {}", steam_root.display());
            println!("  library        {}", library.display());
            println!("  app manifest   {}", opt_path(manifest_path.as_ref()));
        }
    }
    println!(
        "  build id       {}",
        install
            .build_id
            .map_or_else(|| "(unknown)".to_string(), |b| b.to_string())
    );
    if install.depots.is_empty() {
        println!("  depots         (unknown)");
    }
    for depot in &install.depots {
        println!(
            "  depot          {} manifest {}",
            depot.depot_id,
            depot
                .manifest_id
                .map_or_else(|| "(unknown)".to_string(), |m| m.to_string())
        );
    }
    println!("  app bundle     {}", opt_path(install.app_bundle.as_ref()));
    println!("  executable     {}", opt_path(install.executable.as_ref()));
    println!("  content dir    {}", install.content_dir.display());
    println!("  game dir       {}", install.game_dir.display());
    println!("  engine dir     {}", install.engine_dir.display());
    println!("  cooked dir     {}", install.cooked_dir.display());
    println!("  maps dir       {}", install.maps_dir.display());
    println!("  config dir     {}", install.config_dir.display());
    println!("  localization   {}", install.localization_dir.display());
    for warning in &install.warnings {
        println!("  warning        {warning}");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut opts = LocateOptions::from_env();
    if cli.original_dir.is_some() {
        opts.original_dir = cli.original_dir;
    }
    if cli.steam_root.is_some() {
        opts.steam_root = cli.steam_root;
    }

    if cli.candidates {
        for root in steam_root_candidates(&opts) {
            let marker = if root.join("steamapps").is_dir() {
                "exists "
            } else {
                "missing"
            };
            println!("{marker} {}", root.display());
        }
        return ExitCode::SUCCESS;
    }

    match locate_with(&opts) {
        Ok(install) => {
            if cli.json {
                match serde_json::to_string_pretty(&install) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("asamu-locate: cannot serialize result: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print_summary(&install);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("asamu-locate: {e}");
            ExitCode::FAILURE
        }
    }
}
