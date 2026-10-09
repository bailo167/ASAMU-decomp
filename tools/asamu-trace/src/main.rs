//! asamu-trace — behavioural parity tooling (see `docs/TRACE_CAPTURE.md`).
//!
//! ```text
//! asamu-trace convert RAW.raw.jsonl [--out FILE | --out-dir DIR] [--segment N] [--level NAME] [--list]
//! asamu-trace replay TRACE --out OURS [--converted DIR [--map NAME] [--kismet]] [--tick-rate HZ]
//!                    [--from-tick T] [--ticks N] [--no-init] [--max-grapples N] [--placeholder]
//!                    [--compare [--json SUMMARY] [--tol-*]]
//! asamu-trace compare A B [--tol-position UU] [--tol-velocity UU/S] [--tol-angle RAD] [--tol-fov DEG]
//!                    [--tol-anchor UU] [--json SUMMARY] [--fail-on-divergence]
//! asamu-trace report SUMMARY.json... [--title TEXT] [--out FILE]
//! asamu-trace validate FILE...
//! asamu-trace check-recorder [--binary PATH] [--repo DIR] [--verbose]
//! ```

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use asamu_player::Trace;
use asamu_player::trace::CompareTolerances;
use asamu_trace::compare::{CompareSummary, Verdict, compare_traces, render_text};
use asamu_trace::convert::{ConvertOptions, convert, output_names};
use asamu_trace::raw::{RawFile, looks_raw};
use asamu_trace::replay::{ReplayLevel, ReplayOptions, replay};
use asamu_trace::{layout, read_trace, report, write_trace};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "asamu-trace",
    version,
    about = "Convert original-game recordings, replay them through our simulation, compare and report"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// A tolerance: finite and not negative. (An infinite tolerance would be
/// written to the summary JSON as `null`, which `report` cannot read back;
/// use a large finite value to ignore a field.)
fn tolerance(s: &str) -> Result<f64, String> {
    let v: f64 = s.trim().parse().map_err(|e| format!("{e}"))?;
    if v.is_finite() && v >= 0.0 {
        Ok(v)
    } else {
        Err(format!(
            "{v} is not a finite, non-negative number (use e.g. 1e30 to ignore a field)"
        ))
    }
}

#[derive(Args, Clone, Copy)]
struct TolArgs {
    /// Position tolerance, UU.
    #[arg(long, default_value_t = 0.0, value_parser = tolerance)]
    tol_position: f64,
    /// Velocity tolerance, UU/s.
    #[arg(long, default_value_t = 0.0, value_parser = tolerance)]
    tol_velocity: f64,
    /// Yaw/pitch tolerance, radians.
    #[arg(long, default_value_t = 0.0, value_parser = tolerance)]
    tol_angle: f64,
    /// FOV tolerance, degrees.
    #[arg(long, default_value_t = 0.0, value_parser = tolerance)]
    tol_fov: f64,
    /// Grapple anchor tolerance, UU.
    #[arg(long, default_value_t = 0.0, value_parser = tolerance)]
    tol_anchor: f64,
}

impl TolArgs {
    fn tolerances(self) -> CompareTolerances {
        CompareTolerances {
            position: self.tol_position,
            velocity: self.tol_velocity,
            angle: self.tol_angle,
            fov: self.tol_fov,
            anchor: self.tol_anchor,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Raw recording (asamu-trace-raw v1) -> canonical trace(s) (asamu-trace v1, source original).
    Convert {
        /// The raw recording.
        raw: PathBuf,
        /// Output file (one segment, or with --segment).
        #[arg(long, conflicts_with = "out_dir")]
        out: Option<PathBuf>,
        /// Output directory (default: next to the raw file).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Only this segment (0-based).
        #[arg(long)]
        segment: Option<usize>,
        /// Level name to write instead of the recorded map.
        #[arg(long)]
        level: Option<String>,
        /// Only list the segments.
        #[arg(long)]
        list: bool,
    },
    /// Replay a trace's inputs through our simulation and record our trace.
    Replay {
        /// The trace to replay (usually an original recording).
        trace: PathBuf,
        /// Where to write our trace.
        #[arg(long)]
        out: PathBuf,
        /// Converted data directory (asamu-import output); default: the graybox level.
        #[arg(long)]
        converted: Option<PathBuf>,
        /// Map in the converted data (default: the trace's level).
        #[arg(long, requires = "converted")]
        map: Option<String>,
        /// Run the map's Kismet (60 Hz traces only).
        #[arg(long, requires = "converted")]
        kismet: bool,
        /// Tick rate, Hz (required for variable-rate traces).
        #[arg(long)]
        tick_rate: Option<f64>,
        /// Ignore the trace's init: note (grapple capacity, boots).
        #[arg(long)]
        no_init: bool,
        /// Grapple capacity override.
        #[arg(long, allow_hyphen_values = true)]
        max_grapples: Option<i32>,
        /// Placeholder parameters and model (debugging).
        #[arg(long)]
        placeholder: bool,
        /// Start at this tick of the input trace (segment replay).
        #[arg(long)]
        from_tick: Option<u64>,
        /// Replay at most this many ticks.
        #[arg(long)]
        ticks: Option<u64>,
        /// Compare our trace with the input trace afterwards.
        #[arg(long)]
        compare: bool,
        /// With --compare: write the summary JSON here.
        #[arg(long, requires = "compare")]
        json: Option<PathBuf>,
        #[command(flatten)]
        tol: TolArgs,
    },
    /// Compare two traces aligned by tick (a = reference, b = under test).
    Compare {
        /// Reference trace.
        a: PathBuf,
        /// Trace under test.
        b: PathBuf,
        /// Write the summary JSON here.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Exit with status 1 unless the verdict is exact or within tolerance.
        #[arg(long)]
        fail_on_divergence: bool,
        #[command(flatten)]
        tol: TolArgs,
    },
    /// Markdown table (for docs/PARITY.md) from comparison summaries.
    Report {
        /// Summary JSON files written by `compare --json`.
        #[arg(required = true)]
        summaries: Vec<PathBuf>,
        /// Section title.
        #[arg(long, default_value = "Measured parity (trace replays)")]
        title: String,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Validate canonical traces or raw recordings and print a summary.
    Validate {
        /// Files to check.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Check the recorder's symbols and offsets (layout data, Python sources, executable).
    CheckRecorder {
        /// The original executable (default: ASAMU_ORIGINAL_DIR or the Steam library).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Repository root (default: the current directory or this checkout).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Print every check, not only failures.
        #[arg(long)]
        verbose: bool,
    },
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn read_raw(path: &Path) -> Result<RawFile> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    RawFile::read(BufReader::new(f)).with_context(|| format!("reading {}", path.display()))
}

fn write_summary(s: &CompareSummary, path: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(s)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
}

fn cmd_convert(
    raw: &Path,
    out: Option<&Path>,
    out_dir: Option<&Path>,
    segment: Option<usize>,
    level: Option<String>,
    list: bool,
) -> Result<()> {
    let file = read_raw(raw)?;
    let segs = convert(&file, &ConvertOptions { level })?;
    if list {
        for (i, s) in segs.iter().enumerate() {
            println!(
                "segment {i}: frames {}..={} ({} samples, level {:?}, tick rate {:?})",
                s.first_frame,
                s.last_frame,
                s.trace.samples.len(),
                s.trace.meta.level,
                s.trace.meta.tick_rate
            );
        }
        return Ok(());
    }
    if segs.is_empty() {
        bail!("no convertible segment (need at least two consecutive frames with a player)");
    }
    let chosen: Vec<(usize, &Trace)> = match segment {
        Some(i) => vec![(
            i,
            &segs
                .get(i)
                .with_context(|| format!("segment {i} does not exist ({} segments)", segs.len()))?
                .trace,
        )],
        None => segs.iter().map(|s| &s.trace).enumerate().collect(),
    };
    if let Some(out) = out {
        if chosen.len() != 1 {
            bail!("{} segments: use --segment N or --out-dir", chosen.len());
        }
        write_trace(chosen[0].1, out)?;
        println!("{}", out.display());
        return Ok(());
    }
    let dir = out_dir
        .map(Path::to_path_buf)
        .or_else(|| raw.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let names = output_names(&file_name(raw), segs.len());
    for (i, t) in chosen {
        let p = dir.join(&names[i]);
        write_trace(t, &p)?;
        println!("{}", p.display());
    }
    Ok(())
}

fn cmd_validate(files: &[PathBuf]) -> Result<bool> {
    let mut all_ok = true;
    for p in files {
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if looks_raw(first) {
            match RawFile::from_str_lines(&text).and_then(|f| {
                let segs = convert(&f, &ConvertOptions::default())?;
                Ok((f, segs))
            }) {
                Ok((f, segs)) => println!(
                    "{}: raw v{} ({}), {} records, {} usable, {} convertible segment(s), {} bindings",
                    p.display(),
                    f.header.version,
                    f.header.recorder,
                    f.records.len(),
                    f.records.iter().filter(|r| r.usable()).count(),
                    segs.len(),
                    f.header.bindings.len()
                ),
                Err(e) => {
                    all_ok = false;
                    println!("{}: INVALID raw recording: {e:#}", p.display());
                }
            }
        } else {
            match Trace::from_jsonl_str(&text) {
                Ok(t) => {
                    let gaps = t
                        .samples
                        .windows(2)
                        .filter(|w| w[1].tick != w[0].tick + 1)
                        .count();
                    println!(
                        "{}: trace v{} source {:?}, level {:?}, tick rate {:?}, {} samples, ticks {:?}..={:?}, {gaps} gap(s)",
                        p.display(),
                        t.meta.schema_version,
                        t.meta.source,
                        t.meta.level,
                        t.meta.tick_rate,
                        t.samples.len(),
                        t.samples.first().map(|s| s.tick),
                        t.samples.last().map(|s| s.tick)
                    );
                }
                Err(e) => {
                    all_ok = false;
                    println!("{}: INVALID trace: {e}", p.display());
                }
            }
        }
    }
    Ok(all_ok)
}

fn repo_root(arg: Option<PathBuf>) -> PathBuf {
    if let Some(r) = arg {
        return r;
    }
    if Path::new(layout::LAYOUT_PATH).is_file() {
        return PathBuf::from(".");
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Convert {
            raw,
            out,
            out_dir,
            segment,
            level,
            list,
        } => {
            cmd_convert(
                &raw,
                out.as_deref(),
                out_dir.as_deref(),
                segment,
                level,
                list,
            )?;
        }
        Cmd::Replay {
            trace,
            out,
            converted,
            map,
            kismet,
            tick_rate,
            no_init,
            max_grapples,
            placeholder,
            from_tick,
            ticks,
            compare,
            json,
            tol,
        } => {
            let original = read_trace(&trace)?;
            let level = match converted {
                None => ReplayLevel::Graybox,
                Some(dir) => {
                    let map = map
                        .or_else(|| original.meta.level.clone())
                        .context("the trace names no level; pass --map")?;
                    ReplayLevel::Converted { dir, map, kismet }
                }
            };
            let opts = ReplayOptions {
                level,
                tick_rate,
                use_init: !no_init,
                max_grapples,
                placeholder,
                start_tick: from_tick,
                max_ticks: ticks,
            };
            let r = replay(&original, &opts)?;
            write_trace(&r.trace, &out)?;
            println!(
                "{}: {} samples{}{}",
                out.display(),
                r.trace.samples.len(),
                r.stopped_at_gap
                    .map(|t| format!(", stopped at the gap after tick {t}"))
                    .unwrap_or_default(),
                if r.respawns > 0 {
                    format!(", {} respawn(s)", r.respawns)
                } else {
                    String::new()
                }
            );
            if compare {
                let s = compare_traces(
                    &file_name(&trace),
                    &original,
                    &file_name(&out),
                    &r.trace,
                    &tol.tolerances(),
                );
                print!("{}", render_text(&s));
                if let Some(j) = json {
                    write_summary(&s, &j)?;
                }
            }
        }
        Cmd::Compare {
            a,
            b,
            json,
            fail_on_divergence,
            tol,
        } => {
            let ta = read_trace(&a)?;
            let tb = read_trace(&b)?;
            let s = compare_traces(&file_name(&a), &ta, &file_name(&b), &tb, &tol.tolerances());
            print!("{}", render_text(&s));
            if let Some(j) = json {
                write_summary(&s, &j)?;
            }
            if fail_on_divergence && matches!(s.verdict, Verdict::Diverged | Verdict::NoOverlap) {
                return Ok(ExitCode::FAILURE);
            }
        }
        Cmd::Report {
            summaries,
            title,
            out,
        } => {
            let mut all = Vec::new();
            for p in &summaries {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                let s: CompareSummary = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", p.display()))?;
                if s.format != asamu_trace::compare::SUMMARY_FORMAT {
                    bail!("{}: not a comparison summary", p.display());
                }
                if s.version != asamu_trace::compare::SUMMARY_VERSION {
                    bail!(
                        "{}: unsupported summary version {} (supported: {})",
                        p.display(),
                        s.version,
                        asamu_trace::compare::SUMMARY_VERSION
                    );
                }
                let name = file_name(p);
                let name = name.strip_suffix(".json").unwrap_or(&name).to_owned();
                all.push((name, s));
            }
            let md = report::render_markdown(&title, &all);
            match out {
                Some(o) => {
                    std::fs::write(&o, md).with_context(|| format!("writing {}", o.display()))?;
                    println!("{}", o.display());
                }
                None => print!("{md}"),
            }
        }
        Cmd::Validate { files } => {
            if !cmd_validate(&files)? {
                return Ok(ExitCode::FAILURE);
            }
        }
        Cmd::CheckRecorder {
            binary,
            repo,
            verbose,
        } => {
            let root = repo_root(repo);
            let r = layout::run_all(&root, binary.as_deref())?;
            for c in &r.checks {
                if verbose || !c.ok {
                    println!(
                        "[{}] {:<13} {}",
                        if c.ok { "ok" } else { "FAIL" },
                        c.group,
                        c.what
                    );
                }
            }
            let failures = r.failures().len();
            println!(
                "{} checks, {failures} failed; {} Python lookups; executable: {}",
                r.checks.len(),
                r.python_lookups,
                r.binary.as_ref().map_or_else(
                    || "not found (binary checks skipped)".to_owned(),
                    |p| p.display().to_string()
                )
            );
            if failures > 0 {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("asamu-trace: {e:#}");
            ExitCode::from(2)
        }
    }
}
