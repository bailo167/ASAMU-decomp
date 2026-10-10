//! Behavioural parity tooling for ASAMU traces.
//!
//! The pipeline (see `docs/TRACE_CAPTURE.md`):
//!
//! ```text
//! original game ──(tools/trace-recorder, LLDB)──► raw recording (asamu-trace-raw v1)
//!               ──convert──► canonical trace (asamu-trace v1, source = original)
//!               ──replay───► our simulation driven by the same inputs ──► runtime trace
//!               ──compare──► per-field errors, first divergence, summary JSON
//!               ──report───► markdown for docs/PARITY.md
//! ```
//!
//! - [`raw`]: the recorder's lossless per-frame format (engine units: rotator
//!   units, raw key names, physics mode numbers).
//! - [`bindings`]: key names → logical actions through the game's own key
//!   bindings (recorded from the running game).
//! - [`convert`]: raw → canonical trace (unit normalisation, frame alignment,
//!   segmentation at gaps). The Python recorder's converter mirrors it; a test
//!   checks the two agree.
//! - [`replay`]: drives [`asamu_game::Game`] (graybox or a converted level,
//!   optionally with Kismet) with a trace's initial state and inputs: with
//!   fixed ticks for a fixed-rate trace, with each sample's own frame length
//!   for a variable-rate one.
//! - [`timestep`]: the frame lengths of a trace (`time[k] − time[k−1]`) and
//!   what the simulation does with them.
//! - [`stepper`]: a tick with a caller-supplied `dt`, rebuilt from the
//!   public simulation API (`Game::tick` takes its step from a fixed clock).
//! - [`compare`]: [`asamu_player::trace::compare_with`] plus per-field first
//!   exceedances, a check that both traces stepped with the same frame
//!   lengths, and a verdict.
//! - [`report`]: markdown tables from comparison summaries.
//! - [`macho`] and [`layout`]: static self-checks of the recorder's symbols
//!   and offsets against the repository's layout data and, when present, the
//!   original executable.

pub mod bindings;
pub mod compare;
pub mod convert;
pub mod layout;
pub mod macho;
pub mod raw;
pub mod replay;
pub mod report;
pub mod stepper;
pub mod timestep;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use asamu_player::Trace;

/// Reads and validates a canonical trace file.
///
/// # Errors
/// I/O or trace validation errors (with the path).
pub fn read_trace(path: &Path) -> Result<Trace> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Trace::read_jsonl(BufReader::new(f)).with_context(|| format!("reading {}", path.display()))
}

/// Writes a canonical trace file (validated first).
///
/// # Errors
/// Validation or I/O errors.
pub fn write_trace(trace: &Trace, path: &Path) -> Result<()> {
    let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    trace
        .write_jsonl(std::io::BufWriter::new(f))
        .with_context(|| format!("writing {}", path.display()))
}
