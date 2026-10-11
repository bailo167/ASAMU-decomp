//! The Sandbox recording container.
//!
//! A recording made in a session is **not a parity trace** and must never be
//! taken for one. On disk it is its own container:
//!
//! ```text
//! line 1   the sandbox header ({"format":"asamu-sandbox-recording", "not_parity":true, ...})
//! line 2   the trace's meta line, written by the ordinary trace writer
//! line 3+  the trace's sample lines
//! ```
//!
//! The parity trace reader parses the first line as trace metadata, which
//! rejects unknown fields, so it refuses this file by construction; no
//! parity tool is edited for that. The embedded trace is tagged as well (its
//! level is prefixed with `sandbox:`, a note says it is not a parity run and
//! a modified parameter set is never labelled original), and a recording is
//! tagged even when the session was pristine. Files are named
//! `asamu-sandbox-<level>-tick<N>.sbxrec.jsonl` and live in the Sandbox's
//! own recordings directory.
//!
//! What the tagging promises and what it does not: the header and the notes
//! say how the session stood **when the recording was finished**, plus every
//! command the session logged. `param_set` is `classic` only when the
//! recording ran on the Classic set from start to end; the embedded trace's
//! `parameters:` note is rewritten otherwise. Rules, cheats and teleports do
//! not change that field: they are in the action list, and the recording is
//! tagged as not a parity run regardless.

use std::io::{BufRead, Read, Write};

use asamu_player::Trace;
use asamu_player::trace::TraceError;
use serde::{Deserialize, Serialize};

use crate::overlay::{Overlay, ParamSetLabel};
use crate::rules::Rules;
use crate::session::{LoggedAction, Session};

/// The `format` marker of the header line.
pub const RECORDING_FORMAT: &str = "asamu-sandbox-recording";
/// The container version this build reads and writes.
pub const RECORDING_VERSION: u32 = 1;
/// File-name suffix of a recording.
pub const RECORDING_SUFFIX: &str = ".sbxrec.jsonl";
/// What the embedded trace's level name is prefixed with.
pub const TRACE_LEVEL_PREFIX: &str = "sandbox:";
/// The note every embedded trace carries.
pub const TRACE_NOTE_NOT_PARITY: &str = "sandbox: not a parity run";
/// Start of the trace note that says which parameter set ran (written by the
/// game's recorder; rewritten here for a modified set).
const TRACE_NOTE_PARAMETERS: &str = "parameters:";
/// Longest header line [`SandboxRecording::read_jsonl`] accepts, bytes. A
/// bound against hostile files; a header of this build stays far below it
/// because [`SandboxRecording::finish`] bounds the action list.
pub const MAX_HEADER_BYTES: usize = 8 << 20;
/// Budget for the serialized action list of a header, bytes. Actions beyond
/// it are left out and the header says so (`actions_truncated`).
pub const MAX_HEADER_ACTION_BYTES: usize = 2 << 20;

/// The first line of a recording: what the session was when it was made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingHeader {
    /// [`RECORDING_FORMAT`].
    pub format: String,
    /// [`RECORDING_VERSION`].
    pub version: u32,
    /// Always `true`: this is not evidence of parity with the original.
    pub not_parity: bool,
    /// Name of the level.
    pub level: String,
    /// `classic` or `modified`.
    pub param_set: String,
    /// Name of the profile in use.
    pub profile: String,
    /// The parameter overrides in force when the recording ended.
    pub overrides: Overlay,
    /// The sticky rules in force when the recording ended.
    pub rules: Rules,
    /// Time control (freeze, steps, speed) was used during the session.
    pub time_control_used: bool,
    /// The session's commands with the tick each ran before.
    pub actions: Vec<LoggedAction>,
    /// The action log overflowed; `actions` is incomplete.
    pub actions_truncated: bool,
}

/// A finished Sandbox recording: header plus the embedded trace.
#[derive(Clone, Debug, PartialEq)]
pub struct SandboxRecording {
    /// The header line.
    pub header: RecordingHeader,
    /// The embedded trace (tagged as a Sandbox run).
    pub trace: Trace,
}

impl SandboxRecording {
    /// Wraps a trace the game recorded during `session` and tags it: the
    /// trace's level gets the [`TRACE_LEVEL_PREFIX`], its notes get
    /// [`TRACE_NOTE_NOT_PARITY`], and a `parameters: original` note is
    /// replaced unless the recording ran on the Classic set throughout. A
    /// recording of a pristine session is tagged too.
    #[must_use]
    pub fn finish(mut trace: Trace, session: &Session) -> Self {
        let label = session.label();
        let classic = label == ParamSetLabel::Classic && !session.recording_ran_modified();
        let level = trace.meta.level.clone().unwrap_or_default();
        if !level.starts_with(TRACE_LEVEL_PREFIX) {
            trace.meta.level = Some(format!("{TRACE_LEVEL_PREFIX}{level}"));
        }
        if !classic {
            let note = match label {
                ParamSetLabel::Modified { overrides } => format!(
                    "{TRACE_NOTE_PARAMETERS} MODIFIED by a sandbox session \
                     ({overrides} overrides when the recording ended; not the original's values)"
                ),
                ParamSetLabel::Placeholder => {
                    format!("{TRACE_NOTE_PARAMETERS} placeholder (sandbox session)")
                }
                ParamSetLabel::Classic => format!(
                    "{TRACE_NOTE_PARAMETERS} MODIFIED by a sandbox session during the recording \
                     (back to the Classic set when it ended; not the original's values throughout)"
                ),
            };
            let mut replaced = false;
            for existing in &mut trace.meta.notes {
                if existing.starts_with(TRACE_NOTE_PARAMETERS) {
                    existing.clone_from(&note);
                    replaced = true;
                }
            }
            if !replaced {
                trace.meta.notes.push(note);
            }
        }
        if !trace.meta.notes.iter().any(|n| n == TRACE_NOTE_NOT_PARITY) {
            trace.meta.notes.push(TRACE_NOTE_NOT_PARITY.to_owned());
        }

        let (actions, dropped) = bounded_actions(session.log().actions());
        let header = RecordingHeader {
            format: RECORDING_FORMAT.to_owned(),
            version: RECORDING_VERSION,
            not_parity: true,
            level: level
                .strip_prefix(TRACE_LEVEL_PREFIX)
                .unwrap_or(&level)
                .to_owned(),
            param_set: if classic { "classic" } else { "modified" }.to_owned(),
            profile: session.profile().name.clone(),
            overrides: session.overlay().clone(),
            rules: *session.rules(),
            time_control_used: session.time().was_used(),
            actions,
            actions_truncated: session.log().truncated() || dropped,
        };
        Self { header, trace }
    }

    /// Writes the container: the header line, then the trace (written by the
    /// ordinary trace writer). Nothing is written when the header or the
    /// trace is invalid.
    ///
    /// # Errors
    /// A header that is not a Sandbox recording header, an invalid trace,
    /// serialization or I/O failure.
    pub fn write_jsonl<W: Write>(&self, mut w: W) -> Result<(), RecordingError> {
        check_header(&self.header)?;
        check_tagged(&self.trace)?;
        self.trace.validate().map_err(trace_error)?;
        let line = serde_json::to_string(&self.header)
            .map_err(|e| RecordingError::Io(format!("serialization failed: {e}")))?;
        if line.len() > MAX_HEADER_BYTES {
            return Err(RecordingError::NotARecording(format!(
                "the header is {} bytes (at most {MAX_HEADER_BYTES})",
                line.len()
            )));
        }
        let io = |e: std::io::Error| RecordingError::Io(e.to_string());
        w.write_all(line.as_bytes()).map_err(io)?;
        w.write_all(b"\n").map_err(io)?;
        self.trace.write_jsonl(&mut w).map_err(trace_error)?;
        w.flush().map_err(io)
    }

    /// Reads a container written by [`SandboxRecording::write_jsonl`].
    ///
    /// # Errors
    /// Not a Sandbox recording, an unsupported version, or a malformed
    /// header or trace; never panics on malformed input.
    pub fn read_jsonl<R: BufRead>(mut r: R) -> Result<Self, RecordingError> {
        let header = loop {
            let Some(line) = read_header_line(&mut r)? else {
                return Err(RecordingError::NotARecording(
                    "the file is empty".to_owned(),
                ));
            };
            if !line.trim().is_empty() {
                break parse_header(&line)?;
            }
        };
        let trace = Trace::read_jsonl(r).map_err(trace_error)?;
        check_tagged(&trace)?;
        Ok(Self { header, trace })
    }

    /// The file name for this recording:
    /// `asamu-sandbox-<level-slug>-tick<N>.sbxrec.jsonl`, `N` being the tick
    /// of the last sample.
    #[must_use]
    pub fn file_name(&self) -> String {
        let tick = self.trace.samples.last().map_or(0, |s| s.tick);
        format!(
            "asamu-sandbox-{}-tick{tick}{RECORDING_SUFFIX}",
            slug(&self.header.level)
        )
    }
}

/// The actions that fit the header's byte budget, and whether any were left
/// out.
fn bounded_actions(actions: &[LoggedAction]) -> (Vec<LoggedAction>, bool) {
    let mut kept = Vec::with_capacity(actions.len());
    let mut bytes = 0_usize;
    for action in actions {
        // An action that does not serialize could not be written either.
        let Ok(size) = serde_json::to_vec(action).map(|v| v.len()) else {
            return (kept, true);
        };
        bytes = bytes.saturating_add(size).saturating_add(1);
        if bytes > MAX_HEADER_ACTION_BYTES {
            return (kept, true);
        }
        kept.push(action.clone());
    }
    (kept, false)
}

/// A lower-case file-name part made from a level name: letters and digits,
/// everything else collapsed to single dashes, at most 40 characters.
fn slug(level: &str) -> String {
    let mut out = String::new();
    for c in level.chars() {
        if out.len() >= 40 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "level".to_owned()
    } else {
        out.to_owned()
    }
}

fn trace_error(e: TraceError) -> RecordingError {
    match e {
        TraceError::Io(io) => RecordingError::Io(io.to_string()),
        other => RecordingError::Trace(other.to_string()),
    }
}

fn check_header(header: &RecordingHeader) -> Result<(), RecordingError> {
    if header.format != RECORDING_FORMAT {
        return Err(RecordingError::NotARecording(format!(
            "format {:?}, expected {RECORDING_FORMAT:?}",
            header.format
        )));
    }
    if header.version != RECORDING_VERSION {
        return Err(RecordingError::UnsupportedVersion(header.version));
    }
    if !header.not_parity {
        return Err(RecordingError::NotARecording(
            "not_parity must be true: a sandbox recording is never a parity run".to_owned(),
        ));
    }
    Ok(())
}

/// The embedded trace must carry the Sandbox tags, so that it cannot pass
/// for a parity trace even when cut out of its container.
fn check_tagged(trace: &Trace) -> Result<(), RecordingError> {
    let level_tagged = trace
        .meta
        .level
        .as_deref()
        .is_some_and(|l| l.starts_with(TRACE_LEVEL_PREFIX));
    let noted = trace.meta.notes.iter().any(|n| n == TRACE_NOTE_NOT_PARITY);
    if level_tagged && noted {
        Ok(())
    } else {
        Err(RecordingError::Trace(format!(
            "the embedded trace is not tagged as a sandbox run \
             (level prefix {TRACE_LEVEL_PREFIX:?} and note {TRACE_NOTE_NOT_PARITY:?})"
        )))
    }
}

/// Reads one line of at most [`MAX_HEADER_BYTES`]; `None` at the end of the
/// input.
fn read_header_line<R: BufRead>(r: &mut R) -> Result<Option<String>, RecordingError> {
    let mut buf = Vec::new();
    // +2 so a maximal line plus "\r\n" still fits.
    let limit = u64::try_from(MAX_HEADER_BYTES.saturating_add(2)).unwrap_or(u64::MAX);
    let read = r
        .by_ref()
        .take(limit)
        .read_until(b'\n', &mut buf)
        .map_err(|e| RecordingError::Io(e.to_string()))?;
    if read == 0 {
        return Ok(None);
    }
    let mut bytes = buf.as_slice();
    if let Some(rest) = bytes.strip_suffix(b"\n") {
        bytes = rest;
    }
    if let Some(rest) = bytes.strip_suffix(b"\r") {
        bytes = rest;
    }
    if bytes.len() > MAX_HEADER_BYTES {
        return Err(RecordingError::NotARecording(format!(
            "the first line is longer than {MAX_HEADER_BYTES} bytes"
        )));
    }
    match std::str::from_utf8(bytes) {
        Ok(line) => Ok(Some(line.to_owned())),
        Err(_) => Err(RecordingError::NotARecording(
            "the first line is not valid UTF-8".to_owned(),
        )),
    }
}

/// Parses the header line: the format marker and the version are checked
/// before the rest, so a parity trace or a later version gets a clear error.
fn parse_header(line: &str) -> Result<RecordingHeader, RecordingError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| RecordingError::NotARecording(format!("the first line is not JSON: {e}")))?;
    let format = value.get("format").and_then(serde_json::Value::as_str);
    if format != Some(RECORDING_FORMAT) {
        return Err(RecordingError::NotARecording(format!(
            "format {}, expected {RECORDING_FORMAT:?}",
            format.map_or_else(|| "missing".to_owned(), |f| format!("{f:?}"))
        )));
    }
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(RECORDING_VERSION) => {}
        Some(v) => {
            return Err(RecordingError::UnsupportedVersion(
                u32::try_from(v).unwrap_or(u32::MAX),
            ));
        }
        None => {
            return Err(RecordingError::NotARecording(
                "the header has no version".to_owned(),
            ));
        }
    }
    let header: RecordingHeader = serde_json::from_value(value)
        .map_err(|e| RecordingError::NotARecording(format!("malformed header: {e}")))?;
    check_header(&header)?;
    Ok(header)
}

/// Why a recording could not be read or written.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RecordingError {
    /// The first line is not a Sandbox recording header.
    #[error("not a sandbox recording: {0}")]
    NotARecording(String),
    /// A container version this build does not read.
    #[error("unsupported recording version {0} (supported: {RECORDING_VERSION})")]
    UnsupportedVersion(u32),
    /// The embedded trace is invalid.
    #[error("embedded trace: {0}")]
    Trace(String),
    /// Reading or writing failed.
    #[error("i/o error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;

    #[test]
    fn the_header_line_has_the_documented_fields() {
        let session = Session::classic();
        let header = RecordingHeader {
            format: RECORDING_FORMAT.to_owned(),
            version: RECORDING_VERSION,
            not_parity: true,
            level: "sandbox arena: movement-lab".to_owned(),
            param_set: "classic".to_owned(),
            profile: session.profile().name.clone(),
            overrides: session.overlay().clone(),
            rules: *session.rules(),
            time_control_used: false,
            actions: vec![LoggedAction {
                tick: 12,
                cmd: Command::Respawn,
            }],
            actions_truncated: false,
        };
        let json = serde_json::to_string(&header).unwrap();
        assert_eq!(
            json,
            r#"{"format":"asamu-sandbox-recording","version":1,"not_parity":true,"level":"sandbox arena: movement-lab","param_set":"classic","profile":"classic","overrides":{},"rules":{"grapples":"level","rocket_boots":"level","auto_refill":false},"time_control_used":false,"actions":[{"tick":12,"cmd":{"cmd":"respawn"}}],"actions_truncated":false}"#
        );
        assert_eq!(
            serde_json::from_str::<RecordingHeader>(&json).unwrap(),
            header
        );
        // The parity trace reader takes line 1 for trace metadata, which
        // rejects unknown fields: a recording is never read as a trace.
        assert!(Trace::from_jsonl_str(&format!("{json}\n")).is_err());
    }
}
