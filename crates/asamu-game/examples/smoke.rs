//! End-to-end smoke run of converted maps (Kismet, Matinee movers, NPCs, the
//! player under a deterministic pseudo-random input script) and the story
//! chain check, headless. See `asamu_game::smoke` and `docs/INTEGRATION.md`.
//!
//! ```sh
//! asamu-import --out <dir> levels
//! asamu-import --out <dir> meshes --collision
//! asamu-import --out <dir> kismet
//! asamu-import --out <dir> matinee
//! cargo run --release -p asamu-game --example smoke -- --converted <dir> --chain
//! ```
//!
//! Options: `--converted DIR` (required), `--map NAME` (repeatable; default
//! every converted map), `--ticks N` (default 5000), `--seed N`, `--chain`
//! (also follow the story chain), `--chain-each` (one chain step per story
//! map, each from its own fresh load), `--movers` (list the movers that
//! moved), `--chain-ticks N` (frames allowed per map
//! after the trigger touch, default 7200). Exit status 1 when a map has
//! non-finite values or the chain does not reach the Epilogue.

use std::path::PathBuf;
use std::process::ExitCode;

use asamu_game::smoke::{self, DEFAULT_SEED};

struct Args {
    converted: PathBuf,
    maps: Vec<String>,
    ticks: u64,
    seed: u64,
    chain: bool,
    chain_each: bool,
    movers: bool,
    chain_ticks: u64,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        converted: PathBuf::new(),
        maps: Vec::new(),
        ticks: 5_000,
        seed: DEFAULT_SEED,
        chain: false,
        chain_each: false,
        movers: false,
        chain_ticks: 7_200,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        let number = |name: &str, v: String| v.parse::<u64>().map_err(|e| format!("{name}: {e}"));
        match a.as_str() {
            "--converted" => args.converted = PathBuf::from(value("--converted")?),
            "--map" => args.maps.push(value("--map")?),
            "--ticks" => args.ticks = number("--ticks", value("--ticks")?)?,
            "--seed" => args.seed = number("--seed", value("--seed")?)?,
            "--chain" => args.chain = true,
            "--chain-each" => args.chain_each = true,
            "--movers" => args.movers = true,
            "--chain-ticks" => args.chain_ticks = number("--chain-ticks", value("--chain-ticks")?)?,
            "-h" | "--help" => {
                return Err(
                    "usage: smoke --converted DIR [--map NAME]... [--ticks N] [--seed N] \
                            [--chain] [--chain-each] [--movers] [--chain-ticks N]"
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.converted.as_os_str().is_empty() {
        return Err("--converted DIR is required (the --out directory of asamu-import)".to_owned());
    }
    Ok(args)
}

fn print_step(s: &smoke::ChainStep) {
    println!(
        "  {} -> {:?} (expected {:?}) via {:?}{}{}{}{} after {} ticks; streamed {:?}; credits {}; \
         {} candidate triggers{}",
        s.map,
        s.reached,
        s.expected,
        s.triggers,
        if s.enabled_by_touch.is_empty() {
            String::new()
        } else {
            format!(", exit enabled by touching {:?}", s.enabled_by_touch)
        } + &if s.enabled_by_toggle {
            format!(", enabled by toggle {:?}", s.enabled)
        } else {
            String::new()
        },
        if s.direct_touch {
            ", direct touch"
        } else {
            ", teleport touches"
        },
        if s.progressed {
            format!(
                ", after the level's interactions (fired {:?}, injected {:?})",
                s.interactions_fired, s.interactions_injected
            )
        } else {
            String::new()
        },
        if s.ok() { " OK" } else { " MISMATCH" },
        s.ticks,
        s.streamed,
        s.credits,
        s.candidates.len(),
        if s.notes.is_empty() {
            String::new()
        } else {
            format!("; notes {:?}", s.notes)
        }
    );
}

fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let maps = if args.maps.is_empty() {
        smoke::converted_maps(&args.converted)
    } else {
        args.maps.clone()
    };
    let mut failed = false;
    for map in &maps {
        let start = std::time::Instant::now();
        match smoke::run_map(&args.converted, map, args.ticks, args.seed) {
            Ok(sum) => {
                println!("{} [{:.1} s]", sum.line(), start.elapsed().as_secs_f64());
                if args.movers {
                    for (id, a, b) in &sum.moved {
                        println!("  mover {id}: {a:?} -> {b:?}");
                    }
                }
                for p in &sum.problems {
                    println!("  PROBLEM {p}");
                }
                for e in sum.kismet_errors.iter().take(3) {
                    println!("  kismet error: {e}");
                }
                for e in sum.host_errors.iter().take(3) {
                    println!("  host error: {e}");
                }
                failed |= !sum.problems.is_empty();
            }
            // Sub-levels without a PlayerStart (TheCore, Freds_place) only
            // load inside their persistent map.
            Err(e) => println!("{map}: not loadable on its own ({e})"),
        }
    }
    if args.chain {
        println!("story chain:");
        let steps = smoke::follow_story_chain(&args.converted, args.chain_ticks);
        let mut reached_epilogue = false;
        for step in steps {
            match step {
                Ok(s) => {
                    print_step(&s);
                    reached_epilogue |= s.ok()
                        && s.expected
                            .as_deref()
                            .is_some_and(|e| e.eq_ignore_ascii_case("AG-Epilogue"));
                }
                Err(e) => println!("  load error: {e}"),
            }
        }
        failed |= !reached_epilogue;
    }
    if args.chain_each {
        println!("chain steps (each map on its own):");
        let chapters = asamu_game::save::ChapterId::ALL;
        for (c, next) in chapters.iter().zip(chapters.iter().skip(1)) {
            match smoke::chain_step(
                &args.converted,
                c.map_name(),
                Some(next.map_name()),
                args.chain_ticks,
            ) {
                Ok(s) => {
                    print_step(&s);
                    failed |= !s.ok();
                }
                Err(e) => println!("  {}: load error: {e}", c.map_name()),
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
