//! asamu-symbols — sanitized analysis of the unstripped original Mac executable.
//!
//! Outputs statistics only. `--search` prints matching names for local
//! exploration and must not be committed.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use asamu_symbols::analysis::Analysis;
use asamu_symbols::classify::Category;
use asamu_symbols::{locate, macho, markdown, summary};

#[derive(Parser, Debug)]
#[command(
    name = "asamu-symbols",
    about = "Parse, demangle and categorize the symbols of the original Mac executable (statistics only)"
)]
struct Cli {
    /// Path to the Mach-O executable. Default: ASAMU_ORIGINAL_DIR, ASAMU_STEAM_ROOT,
    /// then the default macOS Steam library.
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Print the full statistics JSON to stdout (never the raw symbol list).
    #[arg(long)]
    json: bool,

    /// Print Markdown tables to stdout.
    #[arg(long)]
    markdown: bool,

    /// Write the sanitized summary JSON to this path (deterministic; < 200 KB).
    #[arg(long, value_name = "PATH")]
    write_summary: Option<PathBuf>,

    /// Regenerate the summary and fail if it differs from this file.
    #[arg(long, value_name = "PATH")]
    check_summary: Option<PathBuf>,

    /// LOCAL USE ONLY: print symbols whose demangled name contains WORD
    /// (case-insensitive). Do not commit this output.
    #[arg(long, value_name = "WORD")]
    search: Option<String>,

    /// Restrict --search to one category (e.g. ue3, udk, asamu, physx).
    #[arg(long, value_name = "CATEGORY", requires = "search")]
    category: Option<String>,

    /// Maximum lines printed by --search.
    #[arg(long, default_value_t = 200, requires = "search")]
    limit: usize,
}

fn binary_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = &cli.binary {
        return Ok(p.clone());
    }
    match locate::find_original_binary() {
        Some((path, source)) => {
            eprintln!(
                "asamu-symbols: using executable found via {}",
                source.label()
            );
            Ok(path)
        }
        None => bail!(
            "original executable not found; pass --binary or set ASAMU_ORIGINAL_DIR \
             (folder containing 'A Story About My Uncle.app')"
        ),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = binary_path(&cli)?;
    let data = std::fs::read(&path).with_context(|| "reading the executable")?;
    let image = macho::parse(&data).context("parsing the Mach-O image")?;
    drop(data);
    let analysis = Analysis::new(image);

    if let Some(word) = &cli.search {
        let filter = match &cli.category {
            Some(c) => {
                Some(Category::from_id(c).with_context(|| format!("unknown category '{c}'"))?)
            }
            None => None,
        };
        eprintln!("asamu-symbols: --search output is for local use only; do not commit it");
        let needle = word.to_ascii_lowercase();
        let mut printed = 0usize;
        for e in &analysis.entries {
            if printed >= cli.limit {
                eprintln!("asamu-symbols: limit {} reached", cli.limit);
                break;
            }
            if filter.is_some_and(|c| c != e.category) {
                continue;
            }
            let name = analysis.display(e);
            if !name.to_ascii_lowercase().contains(&needle) {
                continue;
            }
            let Some(sym) = analysis.symbol(e) else {
                continue;
            };
            println!(
                "{:016x} {} {:<11} {:<30} {}",
                sym.value,
                sym.nm_type,
                e.category.id(),
                analysis.origin_id(e),
                name
            );
            printed += 1;
        }
        return Ok(());
    }

    let s = summary::build(&analysis);
    let json = summary::to_json(&s)?;

    if let Some(out) = &cli.write_summary {
        if json.len() > summary::MAX_SUMMARY_BYTES {
            bail!(
                "summary is {} bytes, above the {} byte limit",
                json.len(),
                summary::MAX_SUMMARY_BYTES
            );
        }
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).context("creating summary directory")?;
        }
        std::fs::write(out, &json).context("writing summary")?;
        eprintln!(
            "asamu-symbols: wrote {} ({} bytes)",
            out.display(),
            json.len()
        );
    }
    if let Some(check) = &cli.check_summary {
        let existing = std::fs::read_to_string(check).context("reading summary to check")?;
        if existing != json {
            bail!(
                "{} is out of date; regenerate with --write-summary",
                check.display()
            );
        }
        eprintln!("asamu-symbols: {} is up to date", check.display());
    }
    if cli.json {
        print!("{json}");
    }
    if cli.markdown {
        print!("{}", markdown::render(&s));
    }
    if !cli.json && !cli.markdown && cli.write_summary.is_none() && cli.check_summary.is_none() {
        println!(
            "symbols: {} ({} defined, {} undefined); nlist entries {} incl. {} STABS",
            s.totals.symbols,
            s.totals.defined,
            s.totals.undefined,
            s.totals.nlist_entries,
            s.totals.stab_entries
        );
        for (cat, n) in &s.categories {
            println!("  {cat:<12} {n}");
        }
        println!("(use --json, --markdown or --write-summary for details)");
    }
    Ok(())
}
