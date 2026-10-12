//! The whole-recording parity measurement: every event of the recordings of
//! the original, by family, replayed one step at a time and free-running,
//! against the tolerances of `docs/PARITY_FINDINGS.md` section 5.
//!
//! Needs recordings in `ASAMU_TRACE_RAW_DIR` (every `*.raw.jsonl` there, each
//! with its `.stats.json` when it has one; they stay local) and an
//! `asamu-import` output with simple collision (mesh manifest version 2) in
//! `ASAMU_CONVERTED_DIR`; skips otherwise. Run with `--nocapture`: the test
//! prints, for each group of recordings, one table row per family (`ROW`,
//! and the same as `TABLE {json}`), one `CASE {json}` line per compared
//! stretch and one `TICK {json}` line per one-step tick outside its
//! tolerance, for further analysis.
//!
//! The recordings are measured in groups, never pooled: those whose move
//! input the converter had to derive from the pawn's acceleration (a
//! gamepad: the three of 2026-10-10, about 61 frames a second) together,
//! and each recording with the move keys in its records (exact input) by
//! itself (the first is KM1 of 2026-10-12, 78 frames a second). Each
//! group's line says which it is and its median frame rate.
//!
//! **This is a measurement, not a pass mark.** Nothing here asserts that our
//! simulation agrees with the original: the test asserts only that the
//! measurement itself is sound (every replay got the recording's inputs and
//! frame lengths, started where it was told to, and the families were
//! found). The numbers are printed as they are.
//!
//! # What is compared
//!
//! The original's samples (position, velocity, walking flag, attached flag,
//! anchor, view angles; the standard conversion of the recording, move
//! input derived from the pawn's acceleration) against our simulation's on
//! the converted level, fed the recording's inputs with each frame's own
//! length. Two ways, both through `asamu_trace::replay`:
//!
//! - **one step**: one replay of each whole segment that restarts every
//!   tick from the recording's previous sample, so sample `k` is one tick
//!   of our rules from the original's state `k − 1`;
//! - **free-running**: one replay per stretch, started from the recording's
//!   state at the stretch's first tick and then left alone. The start is
//!   forced where that tick is not a standing-still tick (script state no
//!   record shows starts fresh there; each family's start is chosen so
//!   that this state is at rest, see [`cases`]).
//!
//! Stretches that contain a teleport or a level-script state change are
//! left out (a replay of inputs cannot follow those), as are grapples on a
//! target that moves and stretches in which the pawn walks on an
//! `InterpActor` or is pushed (the per-sample replay does not run Matinee,
//! so a mover stays where the level file has it; "pushed" is a walking tick
//! in which the pawn gains 0.5 uu/s, or 2 velocity quanta where that is
//! more, over what its input gives), and stretches in the air after a
//! flight on a target that moves, up to the landing (the replay could not
//! make that attach itself, so what the attach does to the script state, one
//! level off the move-input lock, is missing: with exact key input the
//! pawn of a one-step replay then does not steer where the original
//! does). In a recording with exact move keys, a walking tick with a move
//! key held and no recorded acceleration is one in which the original
//! ignored the input (a move-input lock that, on the ground, only level
//! script sets, and that no record shows): those are left out too. The
//! notes count what was left out.
//!
//! # Tolerances (PARITY_FINDINGS.md section 5; measured noise only)
//!
//! `q` is the spacing of `f32` at the larger horizontal coordinate (for
//! vertical numbers: at `z`), and a velocity quantum is `q / dt` of the
//! tick. A stretch **agrees** when every tick of it is inside all of:
//!
//! | Quantity | One step | Free-running |
//! |---|---|---|
//! | walking flag, attached flag | equal | equal |
//! | view yaw, pitch | 0 rotator units | 0 rotator units |
//! | horizontal position, walking | 2 q | 4 q |
//! | horizontal velocity, walking | 2 quanta | 2 quanta |
//! | vertical position, walking | width of the native hover band, 0.5 uu | the same |
//! | position, falling | 2 q | the larger of 4 q and `4 × 0.45 × quantum × dt × N^1.5`, plus the hover band's width if the pawn took off inside the replay |
//! | horizontal velocity, falling | 2 quanta of twice the size | `4 × 0.78 × quantum × √N` |
//! | vertical velocity, falling | 2 quanta of twice the size | the same |
//! | position, attached | 0.01 uu | as falling |
//! | velocity, attached | 0.5 uu/s | `4 × 0.78 × quantum × √N` |
//! | anchor, press in the air | 0.15 uu | 0.15 uu |
//!
//! Horizontal numbers are judged per coordinate (each coordinate is rounded
//! by itself; `q` is the spacing at the larger one), and a velocity limit
//! has two spacings of the velocity value itself added (a difference of
//! exactly two quanta is computed from two rounded velocities).
//!
//! **Two readings, both counted.** Section 5 gives the falling quantum
//! "of twice the size" for the vertical velocity only (the engine sets a
//! falling pawn's velocity to twice its average over the tick minus the old
//! one, NATIVE_PHYSICS.md 4.3, so one spacing of position is two quanta of
//! velocity), and does not say whether a horizontal limit is for the
//! length of the difference or for each coordinate. The table above takes
//! the doubled quantum for the horizontal velocity of a falling tick too,
//! and each coordinate by itself. The measurement that supports it: with
//! exact input at 78 frames a second, the one-step falling ticks whose only
//! difference is the horizontal velocity differ by 2.44, 3.45 or 4.87 uu/s
//! in AG-ParadiseCave, which are one spacing of position in one coordinate,
//! in both, and two spacings (the spacing there is 1/64 uu, the frame
//! 0.01282 s). Every count is also printed for the **stricter reading**
//! (the length of the horizontal difference; the falling quantum not
//! doubled for the horizontal velocity), so nothing depends on this choice.
//!
//! `N` is the number of ticks since the pawn last walked, or since the
//! replay started. A fall that follows a grapple flight in the same
//! free-running replay has the flight's vertical velocity bound (`4 × 0.78 ×
//! quantum × √N`), not the free-fall one: its vertical velocity was steered
//! by the pull. Section 5 has no vertical walking tolerance ("none
//! yet"). The one used here is the width of the band inside which the
//! original's floor check leaves a walking pawn alone
//! (`MAX_FLOOR_DIST − MIN_FLOOR_DIST`, NATIVE_PHYSICS.md 3.3): two walks
//! over the same floor may differ by that much, and the original's own
//! standing heights over mesh triangles span 0.39 to 0.45 uu of it
//! (4.717 to 5.162, findings V7; 4.779 to 5.167 at 20 places in the second
//! wave's re-measurement). The free-running airborne rows are the findings'
//! TENTATIVE random-walk bound. An anchor of a press on the ground has no
//! tolerance yet (the walk bob is not in the start state): those are
//! listed, not judged. The FOV is not compared (the recordings of
//! 2026-10-10 hold the cached view FOV).
//!
//! The vertical noise is measured, not assumed: the test prints, per group,
//! how far our height is from the original's after one walking tick from
//! the original's own state (`NOISE` lines: a histogram in `f32` spacings
//! and in uu), and the same for the horizontal position.
//!
//! A second, coarse mark is printed next to it: position within 1 uu,
//! velocity within 10 uu/s, flags equal, anchor within 1 uu. That is the
//! analysis threshold of the findings study, **not** a parity tolerance; it
//! separates "follows the original through the event" from "bit-level
//! noise" when reading the table.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use asamu_core::glam::Vec3;
use asamu_player::Trace;
use asamu_player::trace::{TraceGrappleState, TraceSample};
use asamu_player::ue3_movement::{MAX_FLOOR_DIST, MIN_FLOOR_DIST};
use asamu_trace::convert::{ConvertOptions, PHYS_WALKING, convert};
use asamu_trace::raw::{RawFile, RawPlayer};
use asamu_trace::replay::{ReplayLevel, ReplayOptions, StartPolicy, replay};
use asamu_trace::segments::{EventKind, clean_runs, is_clean_start, recorded_events};
use asamu_trace::sidecar::{RecorderLog, sidecar_path};
use asamu_trace::state::StateTimeline;

/// One segment of a recording with its raw records (record `k` of the run
/// is sample `k`).
struct Seg {
    rec: String,
    /// Index of the recording's group ([`Group`]).
    group: usize,
    index: usize,
    trace: Trace,
    players: Vec<RawPlayer>,
    /// `WorldInfo.DeltaSeconds` of each record.
    ds: Vec<f32>,
    dir: PathBuf,
    map: String,
    /// Sample indices of teleports and level-script state changes.
    ends: Vec<usize>,
    /// Per sample: a walking tick in which the pawn gained speed over what
    /// its input gives ([`Seg::walk_tick`]): something moved it.
    pushed: Vec<bool>,
    /// Per sample, recordings with exact move keys only: a walking tick
    /// whose input has a move key and whose record has no acceleration. The
    /// original ignored the move input: a move-input lock (ABILITIES.md
    /// A-IL-1) that on the ground only level script sets (cinematic mode),
    /// and that no record shows.
    ignored: Vec<bool>,
    /// Per sample: attached to a target that moves (the anchor of the
    /// grapple does not stay within 0.01 uu of its first one).
    moving: Vec<bool>,
    /// Per sample: in the air after such a grapple, up to and with the
    /// landing. A replay cannot make that attach itself (it has no mover to
    /// hit), so what the attach does to the script state (it takes one
    /// level off the move-input lock, GRAPPLE.md G-AT-5) is missing from
    /// then on.
    after_moving: Vec<bool>,
    events: Vec<(usize, EventKind)>,
}

/// One walking tick of a recording against an unobstructed one
/// ([`Seg::walk_tick`]), uu/s.
struct WalkTick {
    /// Recorded speed minus the unobstructed tick's.
    gained: f64,
    /// Length of the difference of the two velocities.
    differs: f64,
    /// From which difference the two count as different.
    limit: f64,
}

impl Seg {
    fn s(&self, k: usize) -> &TraceSample {
        &self.trace.samples[k]
    }
    fn len(&self) -> usize {
        self.trace.samples.len()
    }
    fn walking(&self, k: usize) -> bool {
        self.s(k).grounded
    }
    fn attached(&self, k: usize) -> bool {
        self.s(k).grapple_state == TraceGrappleState::Attached
    }
    fn falling(&self, k: usize) -> bool {
        !self.walking(k) && !self.attached(k)
    }
    /// The pawn walks on a mover (an `InterpActor`) at sample `k`.
    fn on_mover(&self, k: usize) -> bool {
        self.walking(k)
            && self.players[k]
                .base
                .as_deref()
                .is_some_and(|b| b.starts_with("InterpActor"))
    }
    /// No teleport or level-script change in the ticks `(a, b]`.
    fn valid(&self, a: usize, b: usize) -> bool {
        !self.ends.iter().any(|e| *e > a && *e <= b)
    }
    /// Somewhere in the samples `a..=b` the pawn walks on a mover or is
    /// pushed: a level object the per-sample replay does not run acts on it.
    fn mover(&self, a: usize, b: usize) -> bool {
        (a..=b.min(self.len() - 1))
            .any(|k| self.on_mover(k) || self.pushed[k] || self.after_moving[k] || self.ignored[k])
    }
    /// The horizontal velocity an unobstructed walking tick `k` gives
    /// against the recorded one, and the difference from which the two
    /// count as different, when the pawn walks before and after it. That
    /// threshold
    /// is the ground analysis's 0.5 uu/s, or 2 velocity quanta where that is
    /// more (far from the origin the recorded velocity, a displacement of
    /// rounded positions over the frame length, is that coarse: 1.2 uu/s a
    /// quantum in AG-ParadiseCave at 78 frames a second).
    fn walk_tick(&self, k: usize) -> Option<WalkTick> {
        if k == 0 || !(self.walking(k - 1) && self.walking(k)) || self.ends.contains(&k) {
            return None;
        }
        let (p, q) = (&self.players[k - 1], &self.players[k]);
        let a = (f64::from(q.acceleration.x), f64::from(q.acceleration.y));
        let len = a.0.hypot(a.1);
        let dir = if len > 0.0 {
            (a.0 / len, a.1 / len)
        } else {
            (0.0, 0.0)
        };
        let v = free_walk(
            (f64::from(p.velocity.x), f64::from(p.velocity.y)),
            dir,
            f64::from(self.ds[k]),
            f64::from(q.ground_speed),
        );
        let at = self.s(k).position;
        let quantum = ulp32(at.x.abs().max(at.y.abs())) / f64::from(self.ds[k]);
        let recorded = (f64::from(q.velocity.x), f64::from(q.velocity.y));
        Some(WalkTick {
            gained: recorded.0.hypot(recorded.1) - v.0.hypot(v.1),
            differs: (v.0 - recorded.0).hypot(v.1 - recorded.1),
            limit: (2.0 * quantum).max(0.5),
        })
    }
    fn name(&self) -> String {
        format!("{} seg{}", self.rec, self.index)
    }
}

fn converted_map(level: &str) -> Option<(PathBuf, String)> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let map = std::fs::read_dir(dir.join("levels"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".scene.json").map(str::to_owned))
        .find(|n| n.eq_ignore_ascii_case(level))?;
    Some((dir, map))
}

/// Recordings measured together.
struct Group {
    /// The recordings' labels.
    recordings: Vec<String>,
    /// The move input is the recorded move keys (else: derived from the
    /// pawn's acceleration).
    keys: bool,
    records: usize,
    samples: usize,
    /// Every frame length of the group's records, seconds.
    ds: Vec<f32>,
}

impl Group {
    fn name(&self) -> String {
        let mut ds = self.ds.clone();
        ds.sort_by(f32::total_cmp);
        let median = ds.get(ds.len() / 2).copied().unwrap_or(f32::NAN);
        format!(
            "{} ({}, median frame length {:.5} s = {:.1} frames a second; {} records, {} samples \
             in runs)",
            self.recordings.join("+"),
            if self.keys {
                "exact input: the recorded move keys"
            } else {
                "move input derived from the pawn's acceleration"
            },
            median,
            1.0 / f64::from(median),
            self.records,
            self.samples
        )
    }
}

/// The label of a recording: the scenario part of its file name
/// (`<time>-<label>[-…].raw.jsonl`).
fn label(file: &str) -> String {
    let stem = file.strip_suffix(".raw.jsonl").unwrap_or(file);
    let mut parts = stem.split('-');
    match (parts.next(), parts.next()) {
        (Some(_), Some(l)) if !l.is_empty() => l.to_owned(),
        _ => stem.to_owned(),
    }
}

/// The segments of every recording and their groups, or the reason there
/// are none.
fn load() -> Result<(Vec<Seg>, Vec<Group>), String> {
    let dir = std::env::var_os("ASAMU_TRACE_RAW_DIR")
        .map(PathBuf::from)
        .ok_or("ASAMU_TRACE_RAW_DIR is not set")?;
    if std::env::var_os("ASAMU_CONVERTED_DIR").is_none() {
        return Err("ASAMU_CONVERTED_DIR is not set".to_owned());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".raw.jsonl"))
        })
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err("no *.raw.jsonl in ASAMU_TRACE_RAW_DIR".to_owned());
    }
    let mut out = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    for path in paths {
        let rec = label(&path.file_name().unwrap().to_string_lossy());
        let raw = RawFile::from_str_lines(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let log = sidecar_path(&path)
            .filter(|p| p.is_file())
            .map(|p| RecorderLog::read(&p).unwrap());
        let converted = convert(
            &raw,
            &ConvertOptions {
                log,
                ..ConvertOptions::default()
            },
        )
        .unwrap();
        let keys = converted.iter().any(|s| {
            s.trace.meta.notes.iter().any(|n| {
                n.starts_with("move input: the mapped move keys (auto: a move key is held")
            })
        });
        // The recordings without move keys share the first such group; one
        // with keys is a group of its own.
        let group = match groups.iter().position(|g| !g.keys).filter(|_| !keys) {
            Some(g) => g,
            None => {
                groups.push(Group {
                    recordings: Vec::new(),
                    keys,
                    records: 0,
                    samples: 0,
                    ds: Vec::new(),
                });
                groups.len() - 1
            }
        };
        groups[group].recordings.push(rec.clone());
        groups[group].records += raw.records.len();
        for (index, seg) in converted.into_iter().enumerate() {
            let first = raw
                .records
                .iter()
                .position(|r| r.frame == seg.first_frame)
                .unwrap();
            let n = seg.trace.samples.len();
            let records = &raw.records[first..first + n];
            let players: Vec<RawPlayer> =
                records.iter().map(|r| r.player.clone().unwrap()).collect();
            let ds: Vec<f32> = records
                .iter()
                .map(|r| r.world.as_ref().unwrap().delta_seconds)
                .collect();
            // Record k is sample k.
            for (p, s) in players.iter().zip(&seg.trace.samples) {
                assert_eq!(p.location, s.position, "{rec} seg{index} tick {}", s.tick);
                assert_eq!(p.physics == PHYS_WALKING, s.grounded);
            }
            groups[group].samples += n;
            groups[group].ds.extend_from_slice(&ds);
            let level = seg.trace.meta.level.clone().unwrap();
            let (dir, map) =
                converted_map(&level).ok_or(format!("no converted level for {level}"))?;
            let timeline = StateTimeline::from_notes(&seg.trace.meta.notes).unwrap();
            let all = recorded_events(&seg.trace.samples, timeline.as_ref()).unwrap();
            let mut seg = Seg {
                rec: rec.clone(),
                group,
                index,
                players,
                ds,
                dir,
                map,
                pushed: Vec::new(),
                ignored: vec![false; n],
                moving: vec![false; n],
                after_moving: vec![false; n],
                ends: all
                    .iter()
                    .filter(|e| e.kind.ends_validity())
                    .map(|e| e.index)
                    .collect::<std::collections::BTreeSet<usize>>()
                    .into_iter()
                    .collect(),
                events: all.iter().map(|e| (e.index, e.kind)).collect(),
                trace: seg.trace,
            };
            seg.pushed = (0..n)
                .map(|k| seg.walk_tick(k).is_some_and(|w| w.gained >= w.limit))
                .collect();
            if keys {
                for k in 1..n {
                    let i = &seg.s(k).input;
                    let a = seg.players[k].acceleration;
                    seg.ignored[k] = seg.walking(k - 1)
                        && seg.walking(k)
                        && (i.move_forward != 0.0 || i.move_right != 0.0)
                        && a.x == 0.0
                        && a.y == 0.0;
                }
            }
            let mut k = 1;
            while k < n {
                if seg.attached(k) && !seg.attached(k - 1) {
                    let last = (k..n).take_while(|j| seg.attached(*j)).last().unwrap_or(k);
                    let anchor0 = seg.s(k).grapple_anchor.unwrap();
                    let still = (k..=last).all(|j| {
                        seg.s(j)
                            .grapple_anchor
                            .is_some_and(|a| (a - anchor0).length() <= 0.01)
                    });
                    seg.moving[k..=last].fill(!still);
                    if !still {
                        // To the landing; a later attach on a target that
                        // stands still is our own again.
                        let mut j = last + 1;
                        while j < n && !seg.attached(j) {
                            seg.after_moving[j] = true;
                            if seg.walking(j) {
                                break;
                            }
                            j += 1;
                        }
                    }
                    k = last;
                }
                k += 1;
            }
            out.push(seg);
        }
    }
    Ok((out, groups))
}

fn options(seg: &Seg, start: usize, ticks: Option<usize>, one_step: bool) -> ReplayOptions {
    ReplayOptions {
        level: ReplayLevel::Converted {
            dir: seg.dir.clone(),
            map: seg.map.clone(),
            kismet: false,
        },
        start_tick: Some(seg.s(start).tick),
        max_ticks: ticks.map(|t| t as u64),
        // The Workshop's 132 uu/s is a console speed and shows no story
        // mode; the pawn is in story mode for the whole level (findings V10:
        // 132.0 on every Workshop record of every recording).
        story_mode: seg.map.eq_ignore_ascii_case("AG-Workshop").then_some(true),
        start: StartPolicy::Force,
        one_step,
        ..ReplayOptions::default()
    }
}

// ---------------------------------------------------------------------------
// Tolerances (the module docs' table).
// ---------------------------------------------------------------------------

/// Width of the native hover band, uu.
fn band() -> f64 {
    f64::from(MAX_FLOOR_DIST) - f64::from(MIN_FLOOR_DIST)
}

/// Spacing of `f32` at `|x|`.
fn ulp32(x: f32) -> f64 {
    let a = x.abs();
    f64::from(f32::from_bits(a.to_bits() + 1)) - f64::from(a)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Quantity {
    Missing,
    Walking,
    Attached,
    View,
    PositionH,
    PositionV,
    VelocityH,
    VelocityV,
    Anchor,
}

impl Quantity {
    fn name(self) -> &'static str {
        match self {
            Self::Missing => "no sample of ours",
            Self::Walking => "walking flag",
            Self::Attached => "attached flag",
            Self::View => "view angle",
            Self::PositionH => "horizontal position",
            Self::PositionV => "vertical position",
            Self::VelocityH => "horizontal velocity",
            Self::VelocityV => "vertical velocity",
            Self::Anchor => "anchor",
        }
    }
}

/// The tolerances of one tick: horizontal and vertical position, horizontal
/// and vertical velocity.
struct Tolerance {
    ph: f64,
    pv: f64,
    vh: f64,
    vv: f64,
    /// The horizontal velocity tolerance of the stricter reading (see the
    /// module docs): a falling tick's quantum not doubled.
    vh_strict: f64,
}

/// The tolerance at sample `k` of a replay that started at sample `start`
/// (`None` for a tick in which no time passes).
fn tolerance(seg: &Seg, k: usize, start: usize, one_step: bool) -> Option<Tolerance> {
    let o = seg.s(k);
    let dt = o.time - seg.s(k - 1).time;
    if dt <= 0.0 {
        return None;
    }
    let q_h = ulp32(o.position.x.abs().max(o.position.y.abs()));
    let q_z = ulp32(o.position.z);
    let (qv_h, qv_z) = (q_h / dt, q_z / dt);
    Some(if one_step {
        if seg.attached(k) {
            Tolerance {
                ph: 0.01,
                pv: 0.01,
                vh: 0.5,
                vv: 0.5,
                vh_strict: 0.5,
            }
        } else if seg.walking(k) {
            Tolerance {
                ph: 2.0 * q_h,
                pv: band(),
                vh: 2.0 * qv_h,
                vv: 2.0 * qv_z,
                vh_strict: 2.0 * qv_h,
            }
        } else {
            Tolerance {
                ph: 2.0 * q_h,
                pv: 2.0 * q_z,
                vh: 2.0 * (2.0 * qv_h),
                vv: 2.0 * (2.0 * qv_z),
                vh_strict: 2.0 * qv_h,
            }
        }
    } else if seg.walking(k) {
        Tolerance {
            ph: 4.0 * q_h,
            pv: band(),
            vh: 2.0 * qv_h,
            vv: 2.0 * qv_z,
            vh_strict: 2.0 * qv_h,
        }
    } else {
        // Ticks since the pawn last walked, or since the replay started.
        let last_walk = (start..k).rev().find(|j| seg.walking(*j));
        let since = last_walk.unwrap_or(start);
        let n = (k - since) as f64;
        let carried = if last_walk.is_some() { band() } else { 0.0 };
        // The pull of a flight steered the vertical velocity too.
        let flown = (since..=k).any(|j| seg.attached(j));
        let drift = |q: f64, qv: f64| (4.0 * q).max(4.0 * 0.45 * qv * dt * n.powf(1.5));
        let spread = |qv: f64| 4.0 * 0.78 * qv * n.sqrt();
        Tolerance {
            ph: drift(q_h, qv_h),
            pv: drift(q_z, qv_z) + carried,
            vh: spread(qv_h),
            vv: if flown {
                spread(qv_z)
            } else {
                2.0 * (2.0 * qv_z)
            },
            vh_strict: spread(qv_h),
        }
    })
}

/// The largest value of an error and its tick.
#[derive(Clone, Copy, Default)]
struct Peak {
    value: f64,
    tick: u64,
}

impl Peak {
    fn add(&mut self, value: f64, tick: u64) {
        if value > self.value {
            *self = Self { value, tick };
        }
    }
    fn json(self) -> serde_json::Value {
        serde_json::json!([self.value, self.tick])
    }
}

/// One stretch in one mode.
#[derive(Clone, Default)]
struct Outcome {
    ticks: usize,
    /// Every tick inside the section 5 tolerances.
    agrees: bool,
    /// Every tick inside them in the stricter reading as well (see the
    /// module docs).
    strict: bool,
    /// Every tick inside the study's analysis threshold.
    follows: bool,
    /// The first tick outside the section 5 tolerances: quantity, error,
    /// tolerance.
    first: Option<(u64, Quantity, f64, f64)>,
    /// Ticks before the first one outside the section 5 tolerances, and
    /// before the first one outside the study's threshold.
    ticks_agreeing: usize,
    ticks_following: usize,
    position: Peak,
    position_h: Peak,
    position_v: Peak,
    velocity: Peak,
    velocity_h: Peak,
    velocity_v: Peak,
    anchor: Peak,
    /// Ticks on which a flag differs.
    flags: usize,
    first_flag: Option<u64>,
    /// One step: ticks of the stretch that started inside our collision.
    overlaps: usize,
}

fn horizontal(v: Vec3) -> f64 {
    f64::from(v.x).hypot(f64::from(v.y))
}

/// Compares the ticks `lo..=hi` of `seg` with `ours` (samples in tick
/// order; a missing tick counts against the stretch). `start` is the sample
/// the replay started from; `anchor`: judge the anchor at 0.15 uu.
fn evaluate(
    seg: &Seg,
    ours: &[TraceSample],
    (lo, hi): (usize, usize),
    start: usize,
    one_step: bool,
    anchor: bool,
    overlaps: &[u64],
) -> Outcome {
    let mut out = Outcome {
        agrees: true,
        strict: true,
        follows: true,
        ..Outcome::default()
    };
    let fail = |out: &mut Outcome, tick: u64, q: Quantity, error: f64, tol: f64| {
        out.agrees = false;
        out.strict = false;
        if out.first.is_none() {
            out.first = Some((tick, q, error, tol));
        }
    };
    for k in lo..=hi {
        let o = seg.s(k);
        out.ticks += 1;
        out.overlaps += usize::from(overlaps.binary_search(&o.tick).is_ok());
        let Ok(i) = ours.binary_search_by_key(&o.tick, |s| s.tick) else {
            out.follows = false;
            fail(&mut out, o.tick, Quantity::Missing, 1.0, 0.0);
            continue;
        };
        let b = &ours[i];
        let Some(tol) = tolerance(seg, k, start, one_step) else {
            out.ticks_agreeing += usize::from(out.agrees);
            out.ticks_following += usize::from(out.follows);
            continue;
        };
        if b.grounded != o.grounded || b.grapple_state != o.grapple_state {
            out.flags += 1;
            out.first_flag.get_or_insert(o.tick);
            out.follows = false;
            let q = if b.grounded == o.grounded {
                Quantity::Attached
            } else {
                Quantity::Walking
            };
            fail(&mut out, o.tick, q, 1.0, 0.0);
        }
        let turn = |a: f32, b: f32| {
            let d = (f64::from(a) - f64::from(b)).rem_euclid(std::f64::consts::TAU);
            d.min(std::f64::consts::TAU - d) * 32768.0 / std::f64::consts::PI
        };
        let view = turn(b.yaw, o.yaw).max(turn(b.pitch, o.pitch));
        if view >= 0.5 {
            fail(&mut out, o.tick, Quantity::View, view, 0.0);
        }
        let dp = b.position - o.position;
        let dv = b.velocity - o.velocity;
        let (ph, pv) = (horizontal(dp), f64::from(dp.z).abs());
        let (vh, vv) = (horizontal(dv), f64::from(dv.z).abs());
        let (p3, v3) = (ph.hypot(pv), vh.hypot(vv));
        out.position.add(p3, o.tick);
        out.position_h.add(ph, o.tick);
        out.position_v.add(pv, o.tick);
        out.velocity.add(v3, o.tick);
        out.velocity_h.add(vh, o.tick);
        out.velocity_v.add(vv, o.tick);
        if p3 > 1.0 || v3 > 10.0 {
            out.follows = false;
        }
        // Per coordinate, and with the spacing of the two velocity values
        // themselves allowed for (a difference of exactly two quanta is
        // computed from two rounded `f32` velocities).
        let (ph_c, vh_c) = (
            f64::from(dp.x.abs().max(dp.y.abs())),
            f64::from(dv.x.abs().max(dv.y.abs())),
        );
        let slack = |v: f32, w: f32| 2.0 * ulp32(v.abs().max(w.abs()));
        let slack_h = slack(o.velocity.x, o.velocity.y);
        let slack_v = slack(o.velocity.z, o.velocity.z);
        if ph > tol.ph || vh > tol.vh_strict + slack_h {
            out.strict = false;
        }
        for (q, error, limit) in [
            (Quantity::PositionH, ph_c, tol.ph),
            (Quantity::PositionV, pv, tol.pv),
            (Quantity::VelocityH, vh_c, tol.vh + slack_h),
            (Quantity::VelocityV, vv, tol.vv + slack_v),
        ] {
            if error > limit {
                fail(&mut out, o.tick, q, error, limit);
            }
        }
        if let (Some(a), Some(b)) = (o.grapple_anchor, b.grapple_anchor) {
            let e = f64::from((b - a).length());
            out.anchor.add(e, o.tick);
            if anchor {
                if e > 0.15 {
                    fail(&mut out, o.tick, Quantity::Anchor, e, 0.15);
                }
                if e > 1.0 {
                    out.follows = false;
                }
            }
        }
        out.ticks_agreeing += usize::from(out.agrees);
        out.ticks_following += usize::from(out.follows);
    }
    out
}

// ---------------------------------------------------------------------------
// The families.
// ---------------------------------------------------------------------------

/// One stretch of one segment to compare.
#[derive(Clone)]
struct Case {
    family: &'static str,
    seg: usize,
    /// The sample of the event the stretch is about.
    event: usize,
    /// The free-running replay: start sample and last sample. `None`: not
    /// possible (the stretch starts attached).
    run: Option<(usize, usize)>,
    /// The ticks judged, inclusive.
    judged: (usize, usize),
    /// Judge the anchor (a press in the air on a target that stands still).
    anchor: bool,
}

/// Horizontal velocity after one walking tick without contact
/// (NATIVE_PHYSICS.md 2.1 and 2.3, in `f64`), for finding the ticks in
/// which the original's pawn was held up. `dir` is the unit direction of
/// the acceleration or zero.
fn free_walk(v: (f64, f64), dir: (f64, f64), dt: f64, max_speed: f64) -> (f64, f64) {
    const FRICTION: f64 = 8.0;
    const ACCEL_RATE: f64 = 2048.0;
    let mut v = v;
    if dir != (0.0, 0.0) {
        let s = v.0.hypot(v.1);
        v = (
            v.0 - (v.0 - dir.0 * s) * FRICTION * dt,
            v.1 - (v.1 - dir.1 * s) * FRICTION * dt,
        );
    } else {
        let v0 = v;
        let (mut avg, mut t) = ((0.0, 0.0), dt);
        while t > 0.0 {
            let h = t.min(0.03);
            t -= h;
            v = (
                v.0 * (1.0 - 2.0 * h * FRICTION),
                v.1 * (1.0 - 2.0 * h * FRICTION),
            );
            if v.0 * v0.0 + v.1 * v0.1 > 0.0 {
                avg = (avg.0 + v.0 * (h / dt), avg.1 + v.1 * (h / dt));
            }
        }
        v = avg;
        if v.0 * v0.0 + v.1 * v0.1 < 0.0 || v.0 * v.0 + v.1 * v.1 < 100.0 {
            v = (0.0, 0.0);
        }
    }
    v = (v.0 + dir.0 * ACCEL_RATE * dt, v.1 + dir.1 * ACCEL_RATE * dt);
    let s = v.0.hypot(v.1);
    if s > max_speed {
        v = (v.0 / s * max_speed, v.1 / s * max_speed);
    }
    v
}

/// Maximal runs of indices in `1..n` for which `hit` holds, runs closer
/// than `gap` joined.
fn runs(n: usize, gap: usize, hit: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for k in (1..n).filter(|k| hit(*k)) {
        match out.last_mut() {
            Some(last) if k - last.1 <= gap => last.1 = k,
            _ => out.push((k, k)),
        }
    }
    out
}

/// The stretches to compare. How each family is found, and where its
/// free-running replay starts:
///
/// - **walk start**: a move input begins while the pawn stands still on
///   the ground. From the tick before, 30 ticks or to the end of the move
///   or of the walk.
/// - **sprint**: the sprint button goes down while walking and
///   `GroundSpeed` rises. From 2 ticks before, 40 ticks or to the end of
///   the walk.
/// - **braking**: the move input ends while walking faster than 10 uu/s.
///   From 2 ticks before to 2 ticks after the pawn stands (at most 40).
/// - **wall contact**: walking ticks whose recorded horizontal velocity is
///   0.5 uu/s or more (or 2 velocity quanta, [`Seg::walk_tick`]) from what
///   an unobstructed tick gives ([`free_walk`]), without a gain of speed,
///   ticks closer than 8 joined. From 5 ticks before to 5 after (at most 120).
/// - **steps and slopes**: 8 or more walking ticks that each change the
///   height by more than 0.05 uu (gaps up to 3 ticks joined). From 3 ticks
///   before to 3 after (at most 150).
/// - **jump**, **power jump**, **leap**: the pawn leaves the ground with a
///   vertical speed above 900 uu/s, of exactly 1600, of exactly 750 (the
///   air analysis's classes). From 3 ticks before (2 ticks before the
///   power-jump button went down), through the flight to the tick before
///   it lands or attaches (at most 120 air ticks).
/// - **landing**: falling, then walking. From up to 5 falling ticks before
///   (at least one) to 5 walking ticks after. **landing out of a flight** (the grapple
///   lets go and the pawn lands in one frame) can be compared one step at a
///   time only.
/// - **fall**: 10 or more ticks falling with a vertical speed of zero or
///   less, to the tick before the pawn lands or attaches (at most 240).
///   Starts in the air, where no latent script state acts on a fall.
/// - **grapple attach**, **pull**, **proximity release**, **button
///   release**, **anchor**: every attach on a target that stands still. One
///   free-running replay per grapple, from 2 ticks before the press (or
///   from the tick after the previous grapple let go) to 5 ticks after the
///   release; judged over the ticks to the attach, the attached ticks, and
///   the release tick with the 5 after it. The anchor is judged on the
///   attach tick, for presses in the air.
/// - **attach and release in one frame**: the used-grapple counter rises
///   without an attached sample. From 3 ticks before to 3 after.
/// - **walk from a standing start**: from the last tick of every stretch in
///   which the pawn stands still with nothing held, to the end of the walk
///   that follows (at most 900 ticks; the ground analysis's sweep, which
///   started one tick earlier. The last tick is used because a replay's
///   start runs our floor check at once, while the original's pawn keeps
///   the height its landing left until its first step: started on the last
///   standing tick, the two happen in the same tick whenever a step
///   follows). **free run from a standing start**: from the same tick,
///   whatever follows (walks, jumps, falls, grapples), 1,500 ticks or to
///   the tick before the first one no replay of inputs can follow. These
///   two are the long free-running comparisons; a start of this kind is the
///   only one at which every piece of script state is known to be at rest.
fn cases(segs: &[Seg], notes: &mut Vec<String>) -> Vec<Case> {
    let mut out = Vec::new();
    for (si, seg) in segs.iter().enumerate() {
        let n = seg.len();
        let hspeed = |k: usize| horizontal(seg.s(k).velocity);
        let moves = |k: usize| {
            let i = &seg.s(k).input;
            i.move_forward != 0.0 || i.move_right != 0.0
        };
        // The first sample at or after `from` (and before `k`) from which
        // every sample up to `k - 1` satisfies `ok`.
        let clip_back = |from: usize, k: usize, ok: &dyn Fn(usize) -> bool| {
            let mut w = k;
            while w > from && ok(w - 1) {
                w -= 1;
            }
            w
        };
        // The last sample at or before `to` up to which every sample from
        // `k` satisfies `ok`.
        let clip_on = |k: usize, to: usize, ok: &dyn Fn(usize) -> bool| {
            let mut w = k;
            while w < to.min(n - 1) && ok(w + 1) {
                w += 1;
            }
            w
        };
        let walking = |k: usize| seg.walking(k);
        // A stretch replayed from `w0` and judged over the ticks after it.
        let on_a_mover = std::cell::Cell::new(0_usize);
        let stretch = |family, event, w0: usize, w1: usize| {
            if w1 >= w0 + 3 && seg.valid(w0, w1) && seg.mover(w0, w1) {
                on_a_mover.set(on_a_mover.get() + 1);
                return None;
            }
            (w1 >= w0 + 3 && seg.valid(w0, w1)).then_some(Case {
                family,
                seg: si,
                event,
                run: Some((w0, w1)),
                judged: (w0 + 1, w1),
                anchor: false,
            })
        };

        for &(k, kind) in &seg.events {
            if k < 3 || k + 1 >= n {
                continue;
            }
            match kind {
                EventKind::MoveBegins
                    if seg.walking(k - 1) && seg.walking(k) && hspeed(k - 1) == 0.0 =>
                {
                    let w1 = clip_on(k, k + 30, &|j| seg.walking(j) && moves(j));
                    out.extend(stretch("walk start", k, k - 1, w1));
                }
                EventKind::SprintDown
                    if seg.walking(k - 1)
                        && seg.walking(k)
                        && seg.players[k].ground_speed > seg.players[k - 1].ground_speed =>
                {
                    let w0 = clip_back(k - 2, k, &walking);
                    out.extend(stretch("sprint", k, w0, clip_on(k, k + 40, &walking)));
                }
                EventKind::MoveEnds
                    if seg.walking(k - 1) && seg.walking(k) && hspeed(k - 1) > 10.0 =>
                {
                    let stop = (k..n.min(k + 38)).find(|j| hspeed(*j) == 0.0);
                    let w0 = clip_back(k - 2, k, &walking);
                    let w1 = clip_on(k, stop.map_or(k + 40, |j| j + 2), &|j| {
                        seg.walking(j) && !moves(j)
                    });
                    out.extend(stretch("braking", k, w0, w1));
                }
                EventKind::LeftGround if seg.falling(k) => {
                    let vz = seg.s(k).velocity.z;
                    let family = if (vz - 1600.0).abs() < 1e-3 {
                        "power jump"
                    } else if (vz - 750.0).abs() < 1e-3 {
                        "leap"
                    } else if vz > 900.0 {
                        "jump"
                    } else {
                        continue;
                    };
                    let mut w0 = k - 3;
                    if family != "jump" {
                        let mut j = k - 1;
                        while j > 1 && j + 5 > k && !seg.s(j).input.power_jump_held {
                            j -= 1;
                        }
                        while j > 1 && seg.s(j).input.power_jump_held {
                            j -= 1;
                        }
                        w0 = j.saturating_sub(2).max(1);
                    }
                    let w0 = clip_back(w0, k, &walking);
                    let w1 = clip_on(k, k + 120, &|j| seg.falling(j));
                    out.extend(stretch(family, k, w0, w1));
                }
                EventKind::Landed => {
                    let w1 = clip_on(k, k + 5, &walking);
                    if seg.attached(k - 1) {
                        if seg.valid(k - 1, w1) && !seg.mover(k - 1, w1) {
                            out.push(Case {
                                family: "landing out of a flight",
                                seg: si,
                                event: k,
                                run: None,
                                judged: (k, w1),
                                anchor: false,
                            });
                        }
                    } else {
                        let w0 = clip_back(k.saturating_sub(5), k, &|j| seg.falling(j));
                        if k > w0 {
                            out.extend(stretch("landing", k, w0, w1));
                        }
                    }
                }
                EventKind::GrappleAttachedWithinFrame => {
                    out.extend(stretch(
                        "attach and release in one frame",
                        k,
                        k - 3,
                        (k + 3).min(n - 1),
                    ));
                }
                _ => {}
            }
        }

        // Wall contacts: walking ticks held up.
        let held_up = |k: usize| {
            !seg.pushed[k]
                && !seg.ignored[k]
                && seg.walk_tick(k).is_some_and(|w| w.differs >= w.limit)
        };
        let contacts = runs(n, 8, held_up);
        notes.push(format!(
            "{}: {} walking tick(s) held up, in {} contact(s); {} walking tick(s) pushed (speed \
             gained that no input gives: a level object the replay does not run); {} walking \
             tick(s) with a move key held and no acceleration (the original ignored the input)",
            seg.name(),
            (1..n).filter(|k| held_up(*k)).count(),
            contacts.len(),
            seg.pushed.iter().filter(|p| **p).count(),
            seg.ignored.iter().filter(|p| **p).count()
        ));
        for (a, b) in contacts {
            let w0 = clip_back(a.saturating_sub(5).max(1), a, &walking);
            let w1 = clip_on(b, (b + 5).min(w0 + 120), &walking);
            out.extend(stretch("wall contact", a, w0, w1));
        }

        // Steps and slopes.
        let climbs = |k: usize| {
            seg.walking(k - 1)
                && seg.walking(k)
                && (seg.s(k).position.z - seg.s(k - 1).position.z).abs() > 0.05
        };
        for (a, b) in runs(n, 3, climbs) {
            if (a..=b).filter(|k| climbs(*k)).count() < 8 {
                continue;
            }
            let w0 = clip_back(a.saturating_sub(3).max(1), a, &walking);
            let w1 = clip_on(b, (b + 3).min(w0 + 150), &walking);
            out.extend(stretch("steps and slopes", a, w0, w1));
        }

        // Falls.
        let drops = |k: usize| seg.falling(k) && seg.s(k).velocity.z <= 0.0;
        for (a, b) in runs(n, 1, drops) {
            let b = b.min(a + 240);
            if b >= a + 10 {
                out.extend(stretch("fall", a, a, b));
            }
        }

        // From every standing start: the walk that follows, and whatever
        // follows.
        let moving_targets: Vec<usize> = (2..n)
            .filter(|k| seg.moving[*k] && !seg.attached(*k - 1))
            .collect();
        for (_, start) in clean_runs(&seg.trace.samples) {
            // The last sample before a tick no replay of inputs follows: a
            // teleport, a level-script change, a mover under the pawn, a
            // grapple on a target that moves (6 ticks before its attach:
            // the press and the refire check come first).
            let mut limit = n - 1;
            if let Some(e) = seg.ends.iter().find(|e| **e > start) {
                limit = limit.min(*e - 1);
            }
            if let Some(k) =
                (start..n).find(|k| seg.on_mover(*k) || seg.pushed[*k] || seg.ignored[*k])
            {
                limit = limit.min(k.saturating_sub(1));
            }
            if let Some(k) = moving_targets.iter().find(|k| **k > start) {
                limit = limit.min(k.saturating_sub(6));
            }
            if limit < start + 3 {
                continue;
            }
            let walk_end = clip_on(start, (start + 900).min(limit), &walking);
            if (start..=walk_end).any(|k| hspeed(k) > 0.0) {
                out.extend(stretch(
                    "walk from a standing start",
                    start,
                    start,
                    walk_end,
                ));
            }
            let free_end = (start + 1500).min(limit);
            if free_end >= start + 30 {
                out.extend(stretch(
                    "free run from a standing start",
                    start,
                    start,
                    free_end,
                ));
            }
        }

        // Grapples.
        for k in 2..n {
            if !(seg.attached(k) && !seg.attached(k - 1)) {
                continue;
            }
            let release = (k..n).find(|j| !seg.attached(*j));
            let last = release.unwrap_or(n) - 1;
            let anchor0 = seg.s(k).grapple_anchor.unwrap();
            let still = (k..=last).all(|j| {
                seg.s(j)
                    .grapple_anchor
                    .is_some_and(|a| (a - anchor0).length() <= 0.01)
            });
            if !still {
                notes.push(format!(
                    "{} tick {}: a grapple on a moving target, {} attached tick(s): not compared",
                    seg.name(),
                    seg.s(k).tick,
                    last + 1 - k
                ));
                continue;
            }
            // The press: the button went down at most 5 ticks before.
            let press = (k.saturating_sub(5).max(1)..=k)
                .rev()
                .find(|j| seg.s(*j).input.grapple_held && !seg.s(*j - 1).input.grapple_held)
                .unwrap_or(k);
            let w0 = clip_back(press.saturating_sub(2).max(1), k, &|j| !seg.attached(j));
            let w1 = release.map_or(last, |r| clip_on(r, r + 5, &|j| !seg.attached(j)));
            if !seg.valid(w0, w1) || k < w0 + 1 {
                notes.push(format!(
                    "{} tick {}: a grapple across a teleport or a level-script change: not compared",
                    seg.name(),
                    seg.s(k).tick
                ));
                continue;
            }
            // (A grapple in the air after a flight on a target that moves
            // is compared: the attach is ours again, and nothing steers a
            // flight.)
            if (w0..=w1).any(|j| seg.on_mover(j) || seg.pushed[j] || seg.ignored[j]) {
                on_a_mover.set(on_a_mover.get() + 1);
                continue;
            }
            let in_the_air = !seg.walking(k - 1);
            // A part of the grapple's one free-running replay.
            let part = |family, event, lo: usize, hi: usize, anchor| {
                (hi >= lo).then_some(Case {
                    family,
                    seg: si,
                    event,
                    run: Some((w0, w1)),
                    judged: (lo, hi),
                    anchor,
                })
            };
            out.extend(part("grapple attach", k, w0 + 1, k, false));
            out.extend(part("pull", k, k + 1, last, false));
            out.extend(if in_the_air {
                part("anchor, press in the air", k, k, k, true)
            } else {
                part("anchor, press on the ground (not judged)", k, k, k, false)
            });
            if let Some(r) = release {
                let family = if seg.s(r).input.grapple_held {
                    "proximity release"
                } else {
                    "button release"
                };
                out.extend(part(family, r, r, w1, false));
            }
        }
        if on_a_mover.get() > 0 {
            notes.push(format!(
                "{}: {} stretch(es) in which the pawn walks on an InterpActor, is pushed, has its \
                 move keys ignored, or is in the air after a flight on a target that moves: not \
                 compared",
                seg.name(),
                on_a_mover.get()
            ));
        }
    }
    out
}

const FAMILIES: [&str; 20] = [
    "walk start",
    "sprint",
    "braking",
    "wall contact",
    "steps and slopes",
    "jump",
    "power jump",
    "leap",
    "landing",
    "landing out of a flight",
    "fall",
    "grapple attach",
    "pull",
    "proximity release",
    "button release",
    "anchor, press in the air",
    "anchor, press on the ground (not judged)",
    "attach and release in one frame",
    "walk from a standing start",
    "free run from a standing start",
];

fn outcome_json(o: &Outcome) -> serde_json::Value {
    serde_json::json!({
        "ticks": o.ticks,
        "agrees": o.agrees,
        "strict": o.strict,
        "follows": o.follows,
        "ticks_agreeing": o.ticks_agreeing,
        "ticks_following": o.ticks_following,
        "first": o.first.map(|(t, q, e, tol)| serde_json::json!([t, q.name(), e, tol])),
        "position": o.position.json(),
        "position_h": o.position_h.json(),
        "position_v": o.position_v.json(),
        "velocity": o.velocity.json(),
        "velocity_h": o.velocity_h.json(),
        "velocity_v": o.velocity_v.json(),
        "anchor": o.anchor.json(),
        "flags": o.flags,
        "first_flag": o.first_flag,
        "overlaps": o.overlaps,
    })
}

/// One family of one group, in one mode.
#[derive(Default)]
struct Mode {
    compared: usize,
    agree: usize,
    strict: usize,
    follow: usize,
    flags_equal: usize,
    ticks: usize,
    ticks_agreeing: usize,
    ticks_following: usize,
    worst_position: (f64, String),
    worst_velocity: (f64, String),
    worst_anchor: (f64, String),
    first: BTreeMap<&'static str, usize>,
}

impl Mode {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "compared": self.compared,
            "agree": self.agree,
            "strict": self.strict,
            "follow": self.follow,
            "flags_equal": self.flags_equal,
            "ticks": self.ticks,
            "ticks_agreeing": self.ticks_agreeing,
            "ticks_following": self.ticks_following,
            "worst_position": [self.worst_position.0, self.worst_position.1],
            "worst_velocity": [self.worst_velocity.0, self.worst_velocity.1],
            "worst_anchor": [self.worst_anchor.0, self.worst_anchor.1],
            "first": self.first,
        })
    }
}

/// One family of one group.
#[derive(Default)]
struct Row {
    cases: usize,
    ticks: usize,
    clean_starts: usize,
    with_overlap: usize,
    modes: [Mode; 2],
}

/// Counts of an error over ticks, by size: in `f32` spacings (0, at most 1,
/// 2, 4, 16) and beyond that in uu (at most 0.01, 0.05, 0.1, the hover
/// band's width, 1, 2.2, more).
#[derive(Default)]
struct Histogram {
    bins: [usize; 12],
    total: usize,
    worst: (f64, String),
}

const BINS: [&str; 12] = [
    "0",
    "<= 1 spacing",
    "<= 2 spacings",
    "<= 4 spacings",
    "<= 16 spacings",
    "<= 0.01 uu",
    "<= 0.05 uu",
    "<= 0.1 uu",
    "<= 0.5 uu",
    "<= 1 uu",
    "<= 2.2 uu",
    "more",
];

impl Histogram {
    fn add(&mut self, error: f64, spacing: f64, at: impl Fn() -> String) {
        let bin = if error == 0.0 {
            0
        } else if let Some(i) = [1.0, 2.0, 4.0, 16.0]
            .iter()
            .position(|n| error <= n * spacing)
        {
            i + 1
        } else {
            [0.01, 0.05, 0.1, band(), 1.0, 2.2]
                .iter()
                .position(|l| error <= *l)
                .map_or(11, |i| i + 5)
        };
        self.bins[bin] += 1;
        self.total += 1;
        if error > self.worst.0 {
            self.worst = (error, at());
        }
    }
    fn line(&self) -> String {
        let parts: Vec<String> = BINS
            .iter()
            .zip(self.bins)
            .filter(|(_, n)| *n > 0)
            .map(|(name, n)| format!("{name}: {n}"))
            .collect();
        format!(
            "{} tick(s); {}; largest {:.6} uu ({})",
            self.total,
            parts.join(", "),
            self.worst.0,
            self.worst.1
        )
    }
}

#[test]
fn every_event_of_the_recordings_one_step_and_free_running() {
    let (segs, groups) = match load() {
        Ok(s) => s,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let mut notes = Vec::new();
    let all = cases(&segs, &mut notes);

    // One step: one replay of each whole segment.
    let one_step: Vec<(Vec<TraceSample>, Vec<u64>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = segs
            .iter()
            .map(|seg| {
                scope.spawn(move || {
                    let r = replay(&seg.trace, &options(seg, 0, None, true)).unwrap();
                    assert!(r.one_step, "{}", seg.name());
                    // Every tick but those with a teleport or a
                    // level-script change, with the recording's inputs.
                    assert_eq!(
                        r.trace.samples.len() + seg.ends.len(),
                        seg.len(),
                        "{}",
                        seg.name()
                    );
                    for b in r.trace.samples.iter().skip(1) {
                        let i = seg
                            .trace
                            .samples
                            .binary_search_by_key(&b.tick, |s| s.tick)
                            .unwrap();
                        assert_eq!(b.input, seg.s(i).input, "{} tick {}", seg.name(), b.tick);
                        assert_eq!(b.time, seg.s(i).time, "{} tick {}", seg.name(), b.tick);
                    }
                    (r.trace.samples, r.resync_overlaps)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Every tick, one step at a time, by what the pawn does in it.
    type Phases = BTreeMap<&'static str, (usize, usize, usize, BTreeMap<&'static str, usize>)>;
    let mut phases: Vec<Phases> = groups.iter().map(|_| Phases::new()).collect();
    // The noise of one walking tick from the original's own state (not on a
    // mover, not started inside our collision): vertical and horizontal.
    let mut noise: Vec<[Histogram; 2]> = groups.iter().map(|_| Default::default()).collect();
    for (seg, (samples, overlaps)) in segs.iter().zip(&one_step) {
        for k in 1..seg.len() {
            let Ok(i) = samples.binary_search_by_key(&seg.s(k).tick, |s| s.tick) else {
                continue;
            };
            let o = evaluate(seg, samples, (k, k), k - 1, true, false, overlaps);
            let mover = seg.on_mover(k - 1) || seg.on_mover(k);
            let pushed = seg.pushed[k];
            let phase = match (
                seg.attached(k - 1),
                seg.walking(k - 1),
                seg.attached(k),
                seg.walking(k),
            ) {
                (false, true, false, true) if mover => "walking on a mover (not run by the replay)",
                (false, true, false, true) if pushed => {
                    "walking, pushed by a level object (not run by the replay)"
                }
                (false, true, false, true) if seg.ignored[k] => {
                    "walking, move keys ignored by the original (a lock no record shows)"
                }
                (false, true, false, true) => "walking",
                (false, false, false, _) if seg.after_moving[k] => {
                    "in the air after a flight on a target that moves (not run by the replay)"
                }
                (false, false, false, false) => "falling",
                (true, _, true, _) if seg.moving[k] => {
                    "attached to a target that moves (not run by the replay)"
                }
                (true, _, true, _) => "attached",
                (false, true, false, false) => "leaving the ground",
                (false, false, false, true) => "landing",
                (false, _, true, _) => "attaching",
                (true, _, false, false) => "letting go",
                (true, _, false, true) => "letting go and landing",
            };
            let (a, b) = (seg.s(k - 1), seg.s(k));
            let u = &samples[i];
            if phase == "walking" && o.overlaps == 0 && u.grounded {
                let at = || format!("{} tick {}", seg.name(), b.tick);
                noise[seg.group][0].add(o.position_v.value, ulp32(b.position.z), at);
                noise[seg.group][1].add(
                    o.position_h.value,
                    ulp32(b.position.x.abs().max(b.position.y.abs())),
                    at,
                );
            }
            let row = phases[seg.group].entry(phase).or_default();
            row.0 += 1;
            row.1 += usize::from(o.agrees);
            row.2 += usize::from(o.strict);
            if let Some((_, q, error, tol)) = o.first {
                *row.3.entry(q.name()).or_default() += 1;
                println!(
                    "TICK {}",
                    serde_json::json!({
                        "rec": seg.rec,
                        "group": seg.group,
                        "seg": seg.index,
                        "map": seg.map,
                        "tick": b.tick,
                        "phase": phase,
                        "first": [q.name(), error, tol],
                        "ph": o.position_h.value,
                        "pv": o.position_v.value,
                        "vh": o.velocity_h.value,
                        "vv": o.velocity_v.value,
                        "pos": [a.position.x, a.position.y, a.position.z],
                        "dz": f64::from(u.position.z) - f64::from(b.position.z),
                        "overlap": o.overlaps > 0,
                        "base": seg.players[k].base,
                        "base_before": seg.players[k - 1].base,
                        "orig_dz": f64::from(b.position.z) - f64::from(a.position.z),
                        "orig_speed": [horizontal(a.velocity), horizontal(b.velocity)],
                        "our_speed": horizontal(u.velocity),
                        "our_walking": u.grounded,
                        "our_attached": u.grapple_state == TraceGrappleState::Attached,
                        "moves": b.input.move_forward != 0.0 || b.input.move_right != 0.0,
                    })
                );
            }
        }
    }

    // Free-running: one replay per distinct stretch.
    let mut wanted: Vec<(usize, usize, usize)> = all
        .iter()
        .filter_map(|c| c.run.map(|(a, b)| (c.seg, a, b)))
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let next = AtomicUsize::new(0);
    let done: Mutex<BTreeMap<(usize, usize, usize), Vec<TraceSample>>> =
        Mutex::new(BTreeMap::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(si, a, b)) = wanted.get(i) else {
                        break;
                    };
                    let seg = &segs[si];
                    let r = replay(&seg.trace, &options(seg, a, Some(b - a), false)).unwrap();
                    // The whole stretch, from the sample asked for, with
                    // the recording's inputs and times.
                    assert!(!r.one_step && r.stopped_at_event.is_none());
                    assert_eq!(r.start_tick, seg.s(a).tick);
                    assert_eq!(r.trace.samples.len(), b - a + 1, "{} {a}..{b}", seg.name());
                    for (j, s) in r.trace.samples.iter().enumerate().skip(1) {
                        assert_eq!(s.tick, seg.s(a + j).tick);
                        assert_eq!(s.input, seg.s(a + j).input);
                        assert_eq!(s.time, seg.s(a + j).time);
                    }
                    done.lock().unwrap().insert((si, a, b), r.trace.samples);
                }
            });
        }
    });
    let free = done.into_inner().unwrap();

    // The comparison.
    let mut rows: Vec<BTreeMap<&str, Row>> = groups.iter().map(|_| BTreeMap::new()).collect();
    for c in &all {
        let seg = &segs[c.seg];
        let (samples, overlaps) = &one_step[c.seg];
        let start = c.run.map_or(c.judged.0 - 1, |r| r.0);
        let a = evaluate(
            seg,
            samples,
            c.judged,
            c.judged.0 - 1,
            true,
            c.anchor,
            overlaps,
        );
        let b = c.run.map(|(w0, w1)| {
            evaluate(
                seg,
                &free[&(c.seg, w0, w1)],
                c.judged,
                w0,
                false,
                c.anchor,
                &[],
            )
        });
        let row = rows[seg.group].entry(c.family).or_default();
        row.cases += 1;
        row.ticks += a.ticks;
        row.clean_starts += usize::from(is_clean_start(&seg.trace.samples, start));
        row.with_overlap += usize::from(a.overlaps > 0);
        for (mode, o) in row.modes.iter_mut().zip([Some(&a), b.as_ref()]) {
            let Some(o) = o else { continue };
            mode.compared += 1;
            mode.agree += usize::from(o.agrees);
            mode.strict += usize::from(o.strict);
            mode.follow += usize::from(o.follows);
            mode.flags_equal += usize::from(o.flags == 0);
            mode.ticks += o.ticks;
            mode.ticks_agreeing += o.ticks_agreeing;
            mode.ticks_following += o.ticks_following;
            let at = |p: Peak| format!("{} tick {}", seg.name(), p.tick);
            if o.position.value > mode.worst_position.0 {
                mode.worst_position = (o.position.value, at(o.position));
            }
            if o.velocity.value > mode.worst_velocity.0 {
                mode.worst_velocity = (o.velocity.value, at(o.velocity));
            }
            if o.anchor.value > mode.worst_anchor.0 {
                mode.worst_anchor = (o.anchor.value, at(o.anchor));
            }
            if let Some((_, q, _, _)) = o.first {
                *mode.first.entry(q.name()).or_default() += 1;
            }
        }
        println!(
            "CASE {}",
            serde_json::json!({
                "family": c.family,
                "rec": seg.rec,
                "group": seg.group,
                "seg": seg.index,
                "map": seg.map,
                "event": seg.s(c.event).tick,
                "base": seg.players[c.event].base,
                "run": c.run.map(|(a, b)| [seg.s(a).tick, seg.s(b).tick]),
                "judged": [seg.s(c.judged.0).tick, seg.s(c.judged.1).tick],
                "clean_start": is_clean_start(&seg.trace.samples, start),
                "one_step": outcome_json(&a),
                "free": b.as_ref().map(outcome_json),
            })
        );
    }
    for n in &notes {
        println!("NOTE {n}");
    }
    println!(
        "{} segments, {} samples, {} stretches, {} free-running replays; one-step ticks that start \
         inside our collision: {}",
        segs.len(),
        segs.iter().map(Seg::len).sum::<usize>(),
        all.len(),
        free.len(),
        one_step.iter().map(|o| o.1.len()).sum::<usize>()
    );
    for (seg, (_, overlaps)) in segs.iter().zip(&one_step) {
        if !overlaps.is_empty() {
            println!(
                "NOTE {}: {} one-step tick(s) start inside our collision (first {}, last {})",
                seg.name(),
                overlaps.len(),
                overlaps[0],
                overlaps[overlaps.len() - 1]
            );
        }
    }
    for (g, group) in groups.iter().enumerate() {
        println!("GROUP {g}: {}", group.name());
        for (phase, (ticks, agree, strict, first)) in &phases[g] {
            println!(
                "PHASE [{g}] {phase}: {ticks} one-step tick(s), {agree} inside the section 5 \
                 tolerances ({strict} in the stricter reading); first quantity outside on the \
                 others: {first:?}"
            );
        }
        println!(
            "NOISE [{g}] height after one walking tick from the original's state: {}",
            noise[g][0].line()
        );
        println!(
            "NOISE [{g}] horizontal position after one walking tick from the original's state: {}",
            noise[g][1].line()
        );
        for family in FAMILIES {
            let Some(row) = rows[g].get(family) else {
                println!("ROW [{g}] {family}: none found");
                continue;
            };
            println!(
                "ROW [{g}] {family}: {} stretch(es), {} tick(s); free-running start is a \
                 standing-still tick in {}; a one-step tick starts inside our collision in {}",
                row.cases, row.ticks, row.clean_starts, row.with_overlap
            );
            for (name, m) in ["one step", "free-running"].into_iter().zip(&row.modes) {
                if m.compared == 0 {
                    println!("    {name}: not compared");
                    continue;
                }
                println!(
                    "    {name}: {} compared; inside the section 5 tolerances {} (in the stricter \
                     reading {}); inside the study's threshold (1 uu, 10 uu/s) {}; flags equal on \
                     every tick {}; ticks before the first one outside the tolerances {} of {}, \
                     outside the study's threshold {}",
                    m.compared,
                    m.agree,
                    m.strict,
                    m.follow,
                    m.flags_equal,
                    m.ticks_agreeing,
                    m.ticks,
                    m.ticks_following
                );
                println!(
                    "        largest position difference {:.6} uu ({}); velocity {:.6} uu/s ({}); \
                     anchor {:.6} uu ({})",
                    m.worst_position.0,
                    m.worst_position.1,
                    m.worst_velocity.0,
                    m.worst_velocity.1,
                    m.worst_anchor.0,
                    if m.worst_anchor.1.is_empty() {
                        "-"
                    } else {
                        &m.worst_anchor.1
                    }
                );
                println!(
                    "        first quantity outside its tolerance: {:?}",
                    m.first
                );
            }
            println!(
                "TABLE {}",
                serde_json::json!({
                    "group": g,
                    "family": family,
                    "stretches": row.cases,
                    "ticks": row.ticks,
                    "clean_starts": row.clean_starts,
                    "with_overlap": row.with_overlap,
                    "one_step": row.modes[0].json(),
                    "free": (row.modes[1].compared > 0).then(|| row.modes[1].json()),
                })
            );
        }

        // The measurement found the events it was written for: with the
        // three recordings of 2026-10-10, the counts the analysts found
        // independently (PARITY_FINDINGS.md V18 and section 3: 12 power
        // jumps and 16 leaps by their first vertical speed, 4 jumps, 42
        // proximity releases, 52 presses in the air on a target that stands
        // still).
        if !group.keys && group.records == 45_237 {
            let count = |f: &str| rows[g].get(f).map_or(0, |r| r.cases);
            assert_eq!(
                [
                    count("power jump"),
                    count("leap"),
                    count("jump"),
                    count("proximity release"),
                    count("anchor, press in the air"),
                ],
                [12, 16, 4, 42, 52]
            );
        }
    }
}

/// Free-running stretches from a standing start that are inside the section
/// 5 tolerances on every tick, pinned: recording, segment, start tick,
/// ticks, and what the pawn does in them (counted from the recording by
/// the test, which prints it). Each is a stretch of the measurement above
/// ("free run from a standing start"), cut before the first tick outside a
/// tolerance where there is one; none was chosen or shortened by more than
/// that, and no tolerance is other than the module's table.
const PINNED: [(&str, usize, u64, usize); 12] = [
    ("WS1", 0, 2462, 538),
    ("DC1", 0, 5777, 301),
    ("DC1", 3, 1806, 1500),
    ("PLAY2", 1, 5980, 227),
    ("PLAY2", 5, 6928, 179),
    ("KM1", 0, 3311, 820),
    ("KM1", 0, 10527, 1234),
    ("KM1", 1, 667, 1500),
    ("KM1", 1, 1417, 790),
    ("KM1", 1, 5346, 616),
    ("KM1", 4, 932, 1500),
    ("KM1", 5, 10744, 712),
];

/// The pinned free runs stay inside the tolerances: the regression test of
/// the measurement. A recording that is not there is skipped with a line.
#[test]
fn the_free_runs_that_agree_keep_agreeing() {
    let (segs, _) = match load() {
        Ok(s) => s,
        Err(why) => {
            eprintln!("skipped: {why}");
            return;
        }
    };
    let (mut ticks, mut seen) = (0, 0);
    for (rec, index, tick, n) in PINNED {
        let Some(seg) = segs.iter().find(|s| s.rec == rec && s.index == index) else {
            eprintln!("skipped: no {rec} recording in ASAMU_TRACE_RAW_DIR");
            continue;
        };
        let a = seg
            .trace
            .samples
            .binary_search_by_key(&tick, |s| s.tick)
            .unwrap();
        let b = a + n;
        assert!(
            is_clean_start(&seg.trace.samples, a),
            "{} {tick}",
            seg.name()
        );
        assert!(seg.valid(a, b) && !seg.mover(a, b), "{} {tick}", seg.name());
        let r = replay(&seg.trace, &options(seg, a, Some(n), false)).unwrap();
        assert_eq!(r.trace.samples.len(), n + 1, "{} {tick}", seg.name());
        let o = evaluate(seg, &r.trace.samples, (a + 1, b), a, false, false, &[]);
        let mut what: BTreeMap<&str, usize> = BTreeMap::new();
        for (k, kind) in &seg.events {
            if *k > a && *k <= b {
                *what.entry(kind.name()).or_default() += 1;
            }
        }
        let walking = (a + 1..=b).filter(|k| seg.walking(*k)).count();
        let attached = (a + 1..=b).filter(|k| seg.attached(*k)).count();
        println!(
            "PINNED {} from tick {tick}, {n} ticks ({} walking, {} attached, {} falling): inside \
             the tolerances {} (stricter reading {}); first outside {:?}; largest position \
             difference {:.6} uu (tick {}), velocity {:.6} uu/s (tick {}); events {what:?}",
            seg.name(),
            walking,
            attached,
            n - walking - attached,
            o.agrees,
            o.strict,
            o.first,
            o.position.value,
            o.position.tick,
            o.velocity.value,
            o.velocity.tick
        );
        assert!(o.agrees, "{} from tick {tick}: {:?}", seg.name(), o.first);
        ticks += o.ticks;
        seen += 1;
    }
    println!("{seen} pinned free runs, {ticks} ticks, all inside the section 5 tolerances");
}
