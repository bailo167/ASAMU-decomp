//! asamu-trace — behavioural parity tooling (see `docs/TRACE_CAPTURE.md`).
//!
//! ```text
//! asamu-trace convert RAW.raw.jsonl [--out FILE | --out-dir DIR] [--segment N] [--level NAME] [--list]
//!                    [--move-input auto|keys|acceleration] [--move-frame original|yaw]
//! asamu-trace starts TRACE [--events]
//! asamu-trace replay TRACE --out OURS [--converted DIR [--map NAME] [--kismet]]
//!                    [--tick-rate HZ | --variable-dt]
//!                    [--from-tick T] [--ticks N] [--start refuse|snap|force] [--story-mode on|off]
//!                    [--one-step] [--level-events stop|inject|ignore]
//!                    [--no-init] [--max-grapples N] [--placeholder]
//!                    [--compare [--json SUMMARY] [--fov-verdict auto|count|exclude] [--tol-*]]
//! asamu-trace compare A B [--tol-position UU] [--tol-velocity UU/S] [--tol-angle RAD] [--tol-fov DEG]
//!                    [--tol-anchor UU] [--fov-verdict auto|count|exclude] [--json SUMMARY]
//!                    [--fail-on-divergence]
//! asamu-trace report SUMMARY.json... [--title TEXT] [--out FILE]
//! asamu-trace validate FILE...
//! asamu-trace check-recorder [--binary PATH] [--repo DIR] [--verbose]
//! ```
//!
//! `replay` steps a fixed-rate trace with fixed ticks and a trace without a
//! fixed rate (`tick_rate: null`, the original without benchmark mode) with
//! each sample's own frame length; `--tick-rate` forces fixed ticks,
//! `--variable-dt` forces the frame lengths (see `asamu_trace::replay`).
//! A replay of an original recording ends before the first teleport or
//! level-script state change (`--level-events`); `--one-step` restarts every
//! tick from the recording's previous sample, so that a comparison measures
//! one tick of our rules instead of accumulated drift. `compare` leaves the
//! FOV out of the verdict of a recording whose FOV column is the camera's
//! cached view FOV (`--fov-verdict`).

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use asamu_player::Trace;
use asamu_player::trace::CompareTolerances;
use asamu_trace::compare::{
    CompareOptions, CompareSummary, FovPolicy, HARNESS_NOTE_PREFIXES, Verdict, compare_traces_with,
    render_text,
};
use asamu_trace::convert::{ConvertOptions, MoveInput, convert, output_names, raw_timing};
use asamu_trace::move_input::MoveFrame;
use asamu_trace::raw::{RawFile, looks_raw};
use asamu_trace::replay::{EventPolicy, ReplayLevel, ReplayOptions, StartPolicy, Stepping, replay};
use asamu_trace::timestep::StepStats;
use asamu_trace::{layout, read_trace, report, segments, write_trace};
use clap::{Args, Parser, Subcommand, ValueEnum};

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

/// `--fov-verdict`.
#[derive(Clone, Copy, ValueEnum)]
enum FovArg {
    /// Leave the FOV out when a trace's notes say its FOV column is the
    /// camera's cached view FOV (which does not show the zoom), else count it.
    Auto,
    /// Count the FOV.
    Count,
    /// Leave the FOV out.
    Exclude,
}

/// `--level-events`.
#[derive(Clone, Copy, ValueEnum)]
enum EventsArg {
    /// End the replay before the tick (one-step: leave the tick out).
    Stop,
    /// Take the recorded change over and go on, with a note.
    Inject,
    /// Simulate the tick like any other, with a warning.
    Ignore,
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
    /// Whether the FOV counts for the verdict (its numbers are always
    /// reported; no tolerance changes).
    #[arg(long, value_enum, default_value = "auto")]
    fov_verdict: FovArg,
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

    fn options(self) -> CompareOptions {
        CompareOptions {
            fov: match self.fov_verdict {
                FovArg::Auto => FovPolicy::Auto,
                FovArg::Count => FovPolicy::Count,
                FovArg::Exclude => FovPolicy::Exclude,
            },
        }
    }
}

/// `--move-input`.
#[derive(Clone, Copy, ValueEnum)]
enum MoveInputArg {
    /// Per run: the keys if a move key is held in it, else the acceleration.
    Auto,
    /// The held keys through the game's bindings.
    Keys,
    /// The pawn's recorded acceleration (gamepad recordings).
    Acceleration,
}

/// `--move-frame`.
#[derive(Clone, Copy, ValueEnum)]
enum MoveFrameArg {
    /// The original's axes (the stick's direction).
    Original,
    /// The exact yaw only (diagnostic: the acceleration's direction).
    Yaw,
}

/// `--start`.
#[derive(Clone, Copy, ValueEnum)]
enum StartArg {
    /// Fail and name the standing-still ticks around the start tick.
    Refuse,
    /// Start at the next standing-still tick.
    Snap,
    /// Start at the tick all the same.
    Force,
}

/// `--story-mode`.
#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    /// In story mode.
    On,
    /// Not in story mode.
    Off,
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
        /// Where the move axes come from.
        #[arg(long, value_enum, default_value = "auto")]
        move_input: MoveInputArg,
        /// The axes an acceleration is read in.
        #[arg(long, value_enum, default_value = "original")]
        move_frame: MoveFrameArg,
    },
    /// List where a replay of a trace may start (standing still, nothing held) and what
    /// happens in between.
    Starts {
        /// The trace.
        trace: PathBuf,
        /// List every event instead of a summary per stretch.
        #[arg(long)]
        events: bool,
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
        /// Run the map's Kismet (replays with fixed 60 Hz ticks only).
        #[arg(long, requires = "converted")]
        kismet: bool,
        /// Replay with fixed ticks at this rate, Hz, whatever the trace says
        /// (default: the trace's rate; a trace without a fixed rate is
        /// replayed with each sample's own frame length).
        #[arg(long)]
        tick_rate: Option<f64>,
        /// Step with each sample's own frame length (time[k] - time[k-1])
        /// even if the trace has a fixed tick rate.
        #[arg(long, conflicts_with = "tick_rate")]
        variable_dt: bool,
        /// Do not apply the recorded start state (the trace's state: or init: note,
        /// the start sample's button levels and FOV); with --one-step, do not
        /// write the recorded script state on later ticks either.
        #[arg(long)]
        no_init: bool,
        /// What to do when the start tick of an original recording is not a
        /// standing-still start.
        #[arg(long, value_enum, default_value = "refuse")]
        start: StartArg,
        /// Story mode at the start (when the recording does not show it).
        #[arg(long, value_enum)]
        story_mode: Option<OnOff>,
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
        /// Restart every tick from the input trace's previous sample
        /// (position, velocity, view, physics mode, attachment and the
        /// recorded script state): the errors are those of one tick of our
        /// rules, not accumulated drift.
        #[arg(long)]
        one_step: bool,
        /// What to do at a teleport or a level-script state change of an
        /// original recording (story mode, a console speed, the grapple
        /// capacity, the rocket boots), which inputs cannot reproduce.
        #[arg(long, value_enum, default_value = "stop")]
        level_events: EventsArg,
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
    opts: &ConvertOptions,
    list: bool,
) -> Result<()> {
    let file = read_raw(raw)?;
    let segs = convert(&file, opts)?;
    if list {
        for (i, s) in segs.iter().enumerate() {
            println!(
                "segment {i}: frames {}..={} ({} samples, level {:?}, tick rate {:?}, frame lengths {})",
                s.first_frame,
                s.last_frame,
                s.trace.samples.len(),
                s.trace.meta.level,
                s.trace.meta.tick_rate,
                StepStats::of_samples(&s.trace.samples).describe()
            );
            for n in s.trace.meta.notes.iter().filter(|n| {
                ["move input:", "check:", "input:", "event:", "fov:"]
                    .iter()
                    .any(|p| n.starts_with(p))
            }) {
                println!("    {n}");
            }
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
                Ok((f, segs)) => {
                    let t = raw_timing(&f);
                    let arg_check = if t.arg_checked > 0 {
                        format!(
                            "; DeltaSeconds = clamp(tick argument x TimeDilation, 0.0005, 0.4) on {} of {} frames ({} clamped)",
                            t.arg_equal, t.arg_checked, t.arg_clamped
                        )
                    } else {
                        String::new()
                    };
                    println!(
                        "{}: raw v{} ({}), {} records, {} usable, {} convertible segment(s), {} bindings, \
                         benchmarking {:?}, frame lengths {}{arg_check}",
                        p.display(),
                        f.header.version,
                        f.header.recorder,
                        f.records.len(),
                        f.records.iter().filter(|r| r.usable()).count(),
                        segs.len(),
                        f.header.bindings.len(),
                        f.header.benchmarking,
                        t.lengths.describe()
                    );
                }
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
                        .filter(|w| w[0].tick.checked_add(1) != Some(w[1].tick))
                        .count();
                    println!(
                        "{}: trace v{} source {:?}, level {:?}, tick rate {:?}, {} samples, ticks {:?}..={:?}, {gaps} gap(s), frame lengths {}",
                        p.display(),
                        t.meta.schema_version,
                        t.meta.source,
                        t.meta.level,
                        t.meta.tick_rate,
                        t.samples.len(),
                        t.samples.first().map(|s| s.tick),
                        t.samples.last().map(|s| s.tick),
                        StepStats::of_samples(&t.samples).describe()
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

/// The map to load for a trace's level name: the converted level whose
/// name equals it, ignoring ASCII case (the engine reports a map under the
/// name it was opened with, `ag-workshop` as well as `AG-Workshop`), else
/// the name as given.
fn converted_map(dir: &Path, level: &str) -> String {
    const SUFFIX: &str = ".scene.json";
    let Ok(entries) = std::fs::read_dir(dir.join("levels")) else {
        return level.to_owned();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(SUFFIX).map(str::to_owned))
        .collect();
    names.sort();
    if names.iter().any(|n| n == level) {
        return level.to_owned();
    }
    names
        .into_iter()
        .find(|n| n.eq_ignore_ascii_case(level))
        .unwrap_or_else(|| level.to_owned())
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
            move_input,
            move_frame,
        } => {
            let opts = ConvertOptions {
                level,
                move_input: match move_input {
                    MoveInputArg::Auto => MoveInput::Auto,
                    MoveInputArg::Keys => MoveInput::Keys,
                    MoveInputArg::Acceleration => MoveInput::Acceleration,
                },
                move_frame: match move_frame {
                    MoveFrameArg::Original => MoveFrame::Original,
                    MoveFrameArg::Yaw => MoveFrame::Yaw,
                },
            };
            cmd_convert(
                &raw,
                out.as_deref(),
                out_dir.as_deref(),
                segment,
                &opts,
                list,
            )?;
        }
        Cmd::Starts { trace, events } => {
            let t = read_trace(&trace)?;
            println!("{}: level {:?}", trace.display(), t.meta.level);
            print!("{}", segments::describe(&t, events)?);
        }
        Cmd::Replay {
            trace,
            out,
            converted,
            map,
            kismet,
            tick_rate,
            variable_dt,
            no_init,
            start,
            story_mode,
            max_grapples,
            placeholder,
            from_tick,
            ticks,
            one_step,
            level_events,
            compare,
            json,
            tol,
        } => {
            let original = read_trace(&trace)?;
            let level = match converted {
                None => ReplayLevel::Graybox,
                Some(dir) => {
                    let map = match map {
                        Some(m) => m,
                        None => converted_map(
                            &dir,
                            original
                                .meta
                                .level
                                .as_deref()
                                .context("the trace names no level; pass --map")?,
                        ),
                    };
                    ReplayLevel::Converted { dir, map, kismet }
                }
            };
            let opts = ReplayOptions {
                level,
                tick_rate,
                variable_dt,
                use_init: !no_init,
                story_mode: story_mode.map(|m| matches!(m, OnOff::On)),
                start: match start {
                    StartArg::Refuse => StartPolicy::Refuse,
                    StartArg::Snap => StartPolicy::Snap,
                    StartArg::Force => StartPolicy::Force,
                },
                max_grapples,
                placeholder,
                start_tick: from_tick,
                max_ticks: ticks,
                one_step,
                events: match level_events {
                    EventsArg::Stop => EventPolicy::Stop,
                    EventsArg::Inject => EventPolicy::Inject,
                    EventsArg::Ignore => EventPolicy::Ignore,
                },
            };
            let r = replay(&original, &opts)?;
            write_trace(&r.trace, &out)?;
            let stepping = match (r.stepping, &r.variable) {
                (Stepping::Fixed(rate), _) => format!("fixed {rate} Hz ticks"),
                (Stepping::PerSample, Some(v)) => {
                    let mut text = format!("per-sample frame lengths {}", v.lengths.describe());
                    if v.clamped > 0 {
                        text.push_str(&format!(", {} clamped", v.clamped));
                    }
                    if v.no_op > 0 {
                        text.push_str(&format!(", {} without time", v.no_op));
                    }
                    text
                }
                (Stepping::PerSample, None) => "per-sample frame lengths".to_owned(),
            };
            println!(
                "{}: {} samples, {stepping}{}{}{}{}",
                out.display(),
                r.trace.samples.len(),
                if r.one_step { ", one-step" } else { "" },
                r.stopped_at_gap
                    .map(|t| format!(", stopped at the gap after tick {t}"))
                    .unwrap_or_default(),
                r.stopped_at_event
                    .map(|t| format!(", stopped before the event at tick {t}"))
                    .unwrap_or_default(),
                if r.respawns > 0 {
                    format!(", {} respawn(s)", r.respawns)
                } else {
                    String::new()
                }
            );
            for n in r.trace.meta.notes.iter().filter(|n| {
                n.starts_with("start")
                    || n.starts_with("note:")
                    || HARNESS_NOTE_PREFIXES.iter().any(|p| n.starts_with(p))
            }) {
                println!("  {n}");
            }
            if compare {
                let mut s = compare_traces_with(
                    &file_name(&trace),
                    &original,
                    &file_name(&out),
                    &r.trace,
                    &tol.tolerances(),
                    &tol.options(),
                );
                // Printed above already.
                let harness_notes = std::mem::take(&mut s.harness_notes);
                print!("{}", render_text(&s));
                s.harness_notes = harness_notes;
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
            let s = compare_traces_with(
                &file_name(&a),
                &ta,
                &file_name(&b),
                &tb,
                &tol.tolerances(),
                &tol.options(),
            );
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
