//! What the recorder writes next to a raw recording: `<recording>.stats.json`.
//!
//! The Windows recorder writes one JSON object when a recording ends
//! (`Recording.status` in `tools/trace-recorder/asamu_win.py`): its sampling
//! counters and timings, and, from recorder 0.2.0 on, two lists that belong
//! to the recording as much as its records do and are in no record:
//!
//! - **markers** (`markers`): what the `mark NAME` command set while the
//!   recording ran, each `{name, id, frame, records, state, utc}`. `frame` is
//!   the `GFrameCounter` of the last record written when the marker was
//!   taken (`null` before the first record), `records` the number of records
//!   written by then. A marker names a moment of the session ("zoom hold 1"),
//!   so that a scenario's boundaries can be found in the converted traces.
//! - **the dropped-frame log** (`stats.drops`): every frame that got no
//!   record, or a record without its input, each `{frame, last, count,
//!   reason, detail}` for the frames `frame..=last`. `reason` is `missed`,
//!   `skipped`, `unconfirmed`, `torn` or `late` (no record), or `no-input`
//!   (a record whose keys are not its frame's; the recorder writes it as the
//!   last of its run). Consecutive frames of one reason share an entry, and
//!   a stretch without a player shares one whose frames were not all seen
//!   (`count` is then smaller than the span). The log holds at most 2,000
//!   entries; `stats.drops_omitted` counts the frames beyond that.
//!
//! [`RecorderLog`] reads these two lists, typed and strictly (an entry with
//! a member this reader does not know is refused), together with the three
//! numbers that tie the file to its recording (`records`, `first_frame`,
//! `last_frame`). Everything else in the object (timings, counters, process
//! ids) is the recorder's own diagnostics: it changes from version to
//! version and is not read.
//!
//! A sidecar of recorder 0.1.0 has neither list: converting with it is
//! converting without it. [`crate::convert`] says what it does with the
//! lists.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::raw::RawFile;

/// File-name suffix of a raw recording.
pub const RAW_SUFFIX: &str = ".raw.jsonl";
/// File-name suffix of its sidecar.
pub const STATS_SUFFIX: &str = ".stats.json";
/// Largest sidecar read, bytes (a full log of 2,000 entries and 500 markers
/// is well below 1 MiB).
pub const MAX_STATS_BYTES: u64 = 8 << 20;
/// `reason` of a log entry whose frame has a record without its input.
pub const REASON_NO_INPUT: &str = "no-input";
/// Longest marker name kept, in characters (the recorder's own limit,
/// `MAX_MARKER_NAME` in `asamu_win.py`).
pub const MAX_MARKER_NAME: usize = 64;

/// A member the recorder always writes and that may be `null`: unlike a
/// plain `Option`, its absence is an error.
fn nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// A named marker of the recorder's `mark` command.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    /// The name given to `mark`.
    pub name: String,
    /// The request's id (the recorder's own bookkeeping).
    pub id: String,
    /// `GFrameCounter` of the last record written when the marker was
    /// taken; `None` (`null` in the file) before the first record.
    #[serde(deserialize_with = "nullable")]
    pub frame: Option<u64>,
    /// Records written by then.
    pub records: u64,
    /// The recorder's state then (`recording`, `waiting-for-player`, ...).
    pub state: String,
    /// Wall-clock time (UTC) of the marker.
    pub utc: String,
}

impl Marker {
    /// The name as the converters write it: every character below U+0020
    /// and U+007F replaced by a space, at most [`MAX_MARKER_NAME`]
    /// characters (the recorder stores printable names of that length; a
    /// file edited by hand must not put a line break into a notes line).
    #[must_use]
    pub fn label(&self) -> String {
        self.name
            .chars()
            .take(MAX_MARKER_NAME)
            .map(|c| {
                if c < '\u{20}' || c == '\u{7f}' {
                    ' '
                } else {
                    c
                }
            })
            .collect()
    }
}

/// One entry of the dropped-frame log: the frames `frame..=last`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DroppedFrames {
    /// First frame of the entry (the number its record has or would have
    /// had).
    pub frame: u64,
    /// Last frame of the entry.
    pub last: u64,
    /// Frames counted (smaller than the span for a stretch without a
    /// player, whose frames were not all seen).
    pub count: u64,
    /// `missed`, `skipped`, `unconfirmed`, `torn`, `late` or `no-input`.
    pub reason: String,
    /// What happened, in the recorder's words (`null` when it gave none).
    #[serde(deserialize_with = "nullable")]
    pub detail: Option<String>,
}

impl DroppedFrames {
    /// `true` for a frame that has a record, written without its input.
    #[must_use]
    pub fn is_no_input(&self) -> bool {
        self.reason == REASON_NO_INPUT
    }

    /// `true` if the entry covers a frame of `lo..=hi`.
    #[must_use]
    pub fn overlaps(&self, lo: u64, hi: u64) -> bool {
        self.frame <= hi && self.last >= lo
    }

    /// The entry as a notes line has it: `F reason: detail` or
    /// `F..=L reason: detail` (the Python converter writes the same).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut text = if self.frame == self.last {
            format!("{} {}", self.frame, self.reason)
        } else {
            format!("{}..={} {}", self.frame, self.last, self.reason)
        };
        if let Some(d) = self.detail.as_deref().filter(|d| !d.is_empty()) {
            text.push_str(": ");
            text.push_str(d);
        }
        text
    }
}

/// The markers and the dropped-frame log of one recording.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecorderLog {
    /// Recorder name and version, as the sidecar has it.
    pub recorder: Option<String>,
    /// Records the recorder wrote.
    pub records: Option<u64>,
    /// `GFrameCounter` of its first record.
    pub first_frame: Option<u64>,
    /// `GFrameCounter` of its last record.
    pub last_frame: Option<u64>,
    /// The markers, in the order they were set.
    pub markers: Vec<Marker>,
    /// Markers not kept because the list was full.
    pub markers_omitted: u64,
    /// The dropped-frame log; `None` when the recorder kept none (0.1.0).
    pub drops: Option<Vec<DroppedFrames>>,
    /// Frames not logged because the log was full.
    pub drops_omitted: u64,
}

#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    recorder: Option<String>,
    #[serde(default)]
    records: Option<u64>,
    #[serde(default)]
    first_frame: Option<u64>,
    #[serde(default)]
    last_frame: Option<u64>,
    #[serde(default)]
    markers: Option<Vec<Marker>>,
    #[serde(default)]
    markers_omitted: Option<u64>,
    #[serde(default)]
    stats: Option<WireStats>,
}

#[derive(Deserialize)]
struct WireStats {
    #[serde(default)]
    drops: Option<Vec<DroppedFrames>>,
    #[serde(default)]
    drops_omitted: Option<u64>,
}

impl RecorderLog {
    /// Parses a sidecar's text.
    ///
    /// # Errors
    /// Not a JSON object, a marker or a log entry that is not what the
    /// recorder writes, or an entry whose last frame is before its first.
    pub fn from_json_str(text: &str) -> Result<Self> {
        // (A struct whose members all have defaults would also be read from
        // a JSON array; the recorder writes an object.)
        let value: serde_json::Value =
            serde_json::from_str(text).context("invalid recorder stats")?;
        if !value.is_object()
            || value
                .get("stats")
                .is_some_and(|s| !s.is_object() && !s.is_null())
        {
            bail!("invalid recorder stats: not a JSON object with an object as its stats");
        }
        let w: Wire = serde_json::from_value(value).context("invalid recorder stats")?;
        let (drops, drops_omitted) = match w.stats {
            Some(s) => (s.drops, s.drops_omitted.unwrap_or(0)),
            None => (None, 0),
        };
        if let Some(bad) = drops.iter().flatten().find(|d| d.last < d.frame) {
            bail!(
                "invalid recorder stats: a dropped-frame entry ends before it starts (frames {}..={})",
                bad.frame,
                bad.last
            );
        }
        Ok(Self {
            recorder: w.recorder,
            records: w.records,
            first_frame: w.first_frame,
            last_frame: w.last_frame,
            markers: w.markers.unwrap_or_default(),
            markers_omitted: w.markers_omitted.unwrap_or(0),
            drops,
            drops_omitted,
        })
    }

    /// Reads a sidecar file (at most [`MAX_STATS_BYTES`]).
    ///
    /// # Errors
    /// I/O errors, a file that is too large, and [`Self::from_json_str`]'s.
    pub fn read(path: &Path) -> Result<Self> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut text = String::new();
        f.take(MAX_STATS_BYTES + 1)
            .read_to_string(&mut text)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.len() as u64 > MAX_STATS_BYTES {
            bail!("{}: larger than {MAX_STATS_BYTES} bytes", path.display());
        }
        Self::from_json_str(&text).with_context(|| format!("reading {}", path.display()))
    }

    /// `true` when the sidecar has neither a marker nor a dropped-frame
    /// log: converting with it is converting without it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty() && self.drops.is_none()
    }

    /// Checks that the sidecar is this recording's: the number of records
    /// and the first and last frame number it states (where it states them)
    /// are the raw file's. Markers and log entries are frame numbers, which
    /// mean nothing in another recording.
    ///
    /// # Errors
    /// A number that differs, with both values.
    pub fn check_belongs_to(&self, raw: &RawFile) -> Result<()> {
        let have = raw.records.len() as u64;
        if let Some(n) = self.records
            && n != have
        {
            bail!(
                "the recorder stats are not this recording's: they count {n} records, the raw file has {have}"
            );
        }
        for (what, stated, found) in [
            (
                "first",
                self.first_frame,
                raw.records.first().map(|r| r.frame),
            ),
            ("last", self.last_frame, raw.records.last().map(|r| r.frame)),
        ] {
            if let (Some(s), Some(f)) = (stated, found)
                && s != f
            {
                bail!(
                    "the recorder stats are not this recording's: their {what} frame is {s}, the raw file's is {f}"
                );
            }
        }
        Ok(())
    }

    /// The frames whose record was written without its input.
    #[must_use]
    pub fn no_input_frames(&self) -> std::collections::BTreeSet<u64> {
        let mut frames = std::collections::BTreeSet::new();
        for d in self.drops.iter().flatten().filter(|d| d.is_no_input()) {
            // The recorder writes one frame per such entry; a hostile span
            // is not expanded beyond a few frames.
            for f in d.frame..=d.last.min(d.frame.saturating_add(MAX_NO_INPUT_SPAN)) {
                frames.insert(f);
            }
        }
        frames
    }
}

/// Most frames one `no-input` entry is taken to cover beyond its first (the
/// recorder never joins two: the frame after such a record has no record,
/// so the next one cannot be consecutive).
const MAX_NO_INPUT_SPAN: u64 = 64;

/// Where the recorder writes the sidecar of the raw recording at `raw`:
/// `<stem>.stats.json` next to `<stem>.raw.jsonl`. `None` for a file that is
/// not named like a raw recording.
#[must_use]
pub fn sidecar_path(raw: &Path) -> Option<PathBuf> {
    let name = raw.file_name()?.to_str()?;
    let stem = name.strip_suffix(RAW_SUFFIX)?;
    Some(raw.with_file_name(format!("{stem}{STATS_SUFFIX}")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::raw::tests::{header, player, record};

    /// A sidecar as recorder 0.2.0 writes it (the members this reader does
    /// not use are left in, with made-up measurements).
    pub(crate) const SIDECAR: &str = r#"{
        "recorder": "asamu_win 0.2.0", "session": "20261012T101500Z", "state": "finished",
        "message": "STOP file found", "failed": false, "recorder_pid": 1, "game_pid": 2,
        "scenario": "KM1", "file": "20261012T101500Z-KM1.raw.jsonl", "frames_limit": null,
        "seconds_limit": null, "records": 3, "gaps": 1, "first_frame": 5, "last_frame": 9,
        "layout": "win-x86-steam-1822049", "benchmarking": false, "late_marker": "GDeltaTime",
        "bindings": 64, "bindings_confirmed": true, "elapsed_s": 2.5, "recorder_cpu_percent": 90.1,
        "stats": {
            "frames_seen": 5, "sampled": 3, "written": 3, "torn": 1, "unconfirmed": 1, "late": 0,
            "late_kept": 0, "missed": 0, "no_input": 1, "resyncs": 0, "input_retries": 0,
            "retries": 0, "errors": 0, "skipped": {"no-player": 4}, "live_reads": 0, "fps": 60.0,
            "capture_us": null, "window_ms": {"mean": 9.1, "p99": 12.0, "max": 13.0},
            "tick_ms": null, "period_ms": null, "reads_per_burst": null, "last_error": null,
            "drops": [
                {"frame": 1, "last": 4, "count": 2, "reason": "skipped", "detail": "no-player"},
                {"frame": 6, "last": 6, "count": 1, "reason": "no-input",
                 "detail": "record written without its input: the tick start of its frame was not seen"},
                {"frame": 7, "last": 7, "count": 1, "reason": "torn",
                 "detail": "the counter moved while the sample was read"},
                {"frame": 8, "last": 8, "count": 1, "reason": "unconfirmed", "detail": null}
            ],
            "drops_omitted": 0
        },
        "optional": null, "optional_in_header": [],
        "markers": [
            {"name": "zoom hold 1", "id": "a1", "frame": 5, "records": 1, "state": "recording",
             "utc": "2026-10-12T10:15:01Z"},
            {"name": "early", "id": "a0", "frame": null, "records": 0,
             "state": "waiting-for-player", "utc": "2026-10-12T10:15:00Z"}
        ],
        "markers_omitted": 0, "updated_utc": "2026-10-12T10:15:03Z"
    }"#;

    #[test]
    fn reads_markers_and_the_dropped_frame_log() {
        let log = RecorderLog::from_json_str(SIDECAR).unwrap();
        assert_eq!(log.recorder.as_deref(), Some("asamu_win 0.2.0"));
        assert_eq!(
            (log.records, log.first_frame, log.last_frame),
            (Some(3), Some(5), Some(9))
        );
        assert_eq!(log.markers.len(), 2);
        assert_eq!(
            (log.markers[0].name.as_str(), log.markers[0].frame),
            ("zoom hold 1", Some(5))
        );
        assert_eq!(log.markers[1].frame, None);
        let drops = log.drops.as_ref().unwrap();
        assert_eq!(drops.len(), 4);
        assert_eq!(drops[0].describe(), "1..=4 skipped: no-player");
        assert_eq!(
            drops[2].describe(),
            "7 torn: the counter moved while the sample was read"
        );
        assert_eq!(drops[3].describe(), "8 unconfirmed");
        assert!(drops[1].is_no_input() && !drops[2].is_no_input());
        assert!(drops[0].overlaps(4, 9) && !drops[0].overlaps(5, 9) && drops[0].overlaps(0, 1));
        assert_eq!(log.no_input_frames().into_iter().collect::<Vec<_>>(), [6]);
        assert!(!log.is_empty());

        // It is this recording's: three records, frames 5..=9.
        let raw = RawFile {
            header: header(),
            records: vec![
                record(5, player(0.0, 0, &[])),
                record(6, player(0.0, 0, &[])),
                record(9, player(0.0, 0, &[])),
            ],
        };
        log.check_belongs_to(&raw).unwrap();
        let mut other = raw.clone();
        other.records.pop();
        let e = format!("{:#}", log.check_belongs_to(&other).unwrap_err());
        assert!(e.contains("count 3 records") && e.contains("has 2"), "{e}");
        let mut shifted = raw.clone();
        shifted.records[0].frame = 4;
        let e = format!("{:#}", log.check_belongs_to(&shifted).unwrap_err());
        assert!(e.contains("first frame is 5") && e.contains("is 4"), "{e}");
        shifted.records[0].frame = 5;
        shifted.records[2].frame = 10;
        assert!(log.check_belongs_to(&shifted).is_err());
    }

    /// A sidecar of recorder 0.1.0 (no markers, no log) changes nothing; a
    /// broken one is an error, not a guess.
    #[test]
    fn old_and_hostile_sidecars() {
        let old = RecorderLog::from_json_str(
            r#"{"recorder":"asamu_win 0.1.0","records":3001,"gaps":0,"first_frame":6869,
                "last_frame":9869,"stats":{"torn":0,"missed":0,"skipped":{}}}"#,
        )
        .unwrap();
        assert!(old.is_empty());
        assert_eq!(old.drops, None);
        assert!(old.no_input_frames().is_empty());
        assert!(RecorderLog::from_json_str("{}").unwrap().is_empty());
        // An empty log is a log (the recorder kept one and dropped nothing).
        let kept =
            RecorderLog::from_json_str(r#"{"stats":{"drops":[],"drops_omitted":0}}"#).unwrap();
        assert!(!kept.is_empty());
        for bad in [
            "",
            "[]",
            "{\"markers\":{}}",
            // A marker without its frame member, with a member nobody
            // writes, with a wrong type.
            r#"{"markers":[{"name":"x","id":"","records":0,"state":"s","utc":"u"}]}"#,
            r#"{"markers":[{"name":"x","id":"","frame":1,"records":0,"state":"s","utc":"u","tick":3}]}"#,
            r#"{"markers":[{"name":7,"id":"","frame":1,"records":0,"state":"s","utc":"u"}]}"#,
            r#"{"markers":[{"name":"x","id":"","frame":-1,"records":0,"state":"s","utc":"u"}]}"#,
            // Log entries likewise, and one that ends before it starts.
            r#"{"stats":{"drops":[{"frame":1,"last":1,"count":1,"reason":"torn"}]}}"#,
            r#"{"stats":{"drops":[{"frame":1,"last":1,"count":1,"reason":"torn","detail":null,"x":1}]}}"#,
            r#"{"stats":{"drops":[{"frame":"1","last":1,"count":1,"reason":"torn","detail":null}]}}"#,
            r#"{"stats":{"drops":[{"frame":9,"last":8,"count":1,"reason":"torn","detail":null}]}}"#,
            r#"{"stats":{"drops":7}}"#,
            r#"{"stats":[]}"#,
            r#"{"records":"many"}"#,
        ] {
            assert!(RecorderLog::from_json_str(bad).is_err(), "{bad}");
        }
        // A hostile no-input span is not expanded without bound.
        let wide = RecorderLog::from_json_str(
            r#"{"stats":{"drops":[{"frame":10,"last":18446744073709551615,"count":1,"reason":"no-input","detail":null}]}}"#,
        )
        .unwrap();
        assert_eq!(wide.no_input_frames().len(), 65);
    }

    #[test]
    fn marker_labels_and_sidecar_paths() {
        let m = |name: &str| Marker {
            name: name.to_owned(),
            id: String::new(),
            frame: None,
            records: 0,
            state: String::new(),
            utc: String::new(),
        };
        assert_eq!(m("zoom hold 1").label(), "zoom hold 1");
        assert_eq!(m("a\tb\nc\u{7f}d").label(), "a b c d");
        assert_eq!(m(&"x".repeat(70)).label().chars().count(), MAX_MARKER_NAME);
        assert_eq!(m("Zürich \"45°\"").label(), "Zürich \"45°\"");
        assert_eq!(
            sidecar_path(Path::new("traces/20261012T101500Z-KM1.raw.jsonl")),
            Some(PathBuf::from("traces/20261012T101500Z-KM1.stats.json"))
        );
        assert_eq!(sidecar_path(Path::new("a.jsonl")), None);
        assert_eq!(sidecar_path(Path::new("")), None);
    }

    #[test]
    fn reads_a_file_with_a_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.stats.json");
        std::fs::write(&path, SIDECAR).unwrap();
        assert_eq!(RecorderLog::read(&path).unwrap().markers.len(), 2);
        assert!(RecorderLog::read(&dir.path().join("missing.stats.json")).is_err());
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_STATS_BYTES as usize));
        std::fs::write(&path, big).unwrap();
        let e = format!("{:#}", RecorderLog::read(&path).unwrap_err());
        assert!(e.contains("larger than"), "{e}");
    }
}
