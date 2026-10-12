//! Recordings of the original game against what the converter assumes about
//! them (`asamu_trace::move_input`, facts M2–M6), and their conversion and
//! replay from a standing start.
//!
//! Set `ASAMU_TRACE_RAW_DIR` to a directory with `*.raw.jsonl` recordings
//! (they stay local: `research/local/traces/...`); skips when unset. With
//! `ASAMU_CONVERTED_DIR` (an `asamu-import` output) a segment of each map
//! that has a converted level is also replayed. Run with `--nocapture` to
//! see the counts the documentation quotes.
//!
//! The last test reads what recorder 0.2.0 adds (optional members, and the
//! markers and dropped-frame log of each recording's `.stats.json`): for a
//! version-1 recording it checks that none of that changes its conversion,
//! for a later one that the conversion uses it as documented, and prints
//! the counts.
//!
//! The last four tests are identities of the recordings themselves, with no
//! simulation (`docs/PARITY_FINDINGS.md` V17 and V18, and two questions of
//! its sections 6 and 7 that the keyboard recording of 2026-10-12 settles):
//! the float form of a walking pawn's velocity; that the grapple is attached
//! exactly while the pawn's physics mode is flying; that the Workshop's
//! console speed survives a held sprint key; and the two limits of the view
//! pitch.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use asamu_player::trace::TraceGrappleState;
use asamu_trace::compare::compare_traces;
use asamu_trace::convert::{ConvertOptions, PHYS_FLYING, convert, split_runs};
use asamu_trace::move_input::{MoveFrame, horizontal_magnitude, move_direction};
use asamu_trace::raw::{RawFile, RawPlayer};
use asamu_trace::replay::{ReplayLevel, ReplayOptions, replay};
use asamu_trace::segments::{EventKind, clean_runs, events};

fn recordings() -> Option<Vec<(String, RawFile)>> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_TRACE_RAW_DIR")?);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".raw.jsonl"))
        })
        .collect();
    paths.sort();
    let files: Vec<(String, RawFile)> = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                RawFile::from_str_lines(&text).unwrap(),
            )
        })
        .collect();
    (!files.is_empty()).then_some(files)
}

fn attached(p: &RawPlayer) -> bool {
    p.physics == PHYS_FLYING || p.gun.as_ref().is_some_and(|g| g.grappling)
}

fn magnitude(p: &RawPlayer) -> f64 {
    horizontal_magnitude(p.acceleration.x, p.acceleration.y)
}

/// `true` if the grapple that `now` flies on was attached by the weapon's
/// refire check, as far as two consecutive records of recorder 0.2.0 can
/// show it: the record before does not fly yet, its weapon is already in
/// its firing state, and the refire timer's count is smaller in `now` (the
/// looping timer has reached its period in that tick and called its
/// function). A record without the weapon's state and timers (recorder
/// 0.1.0) shows none of this and is never one.
fn attached_by_the_refire_check(before: &RawPlayer, now: &RawPlayer) -> bool {
    let count = |p: &RawPlayer| {
        p.gun
            .as_ref()
            .and_then(|g| g.refire_timer())
            .map(|t| t.count)
    };
    let firing = before
        .gun
        .as_ref()
        .and_then(|g| g.state.clone())
        .flatten()
        .is_some_and(|s| s.eq_ignore_ascii_case("WeaponFiring"));
    before.physics != PHYS_FLYING
        && firing
        && matches!((count(before), count(now)), (Some(b), Some(n)) if n < b)
}

fn angle_deg(rotation: [i32; 3], p: &RawPlayer, frame: MoveFrame) -> Option<f64> {
    move_direction(rotation, p.acceleration.x, p.acceleration.y, frame)
        .map(|d| f64::from(d.right).atan2(f64::from(d.forward)).to_degrees())
}

fn wrap_deg(d: f64) -> f64 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// Distance of a direction from the nearest of forward, right, back, left.
fn off_axis(angle: f64) -> f64 {
    [0.0, 90.0, 180.0, -90.0]
        .into_iter()
        .map(|a| wrap_deg(angle - a).abs())
        .fold(f64::INFINITY, f64::min)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

#[test]
fn recordings_agree_with_the_acceleration_mapping() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    // M2: magnitudes while walking or falling. M3: none while flying.
    let mut magnitudes: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut flying, mut flying_zero) = (0, 0);
    // M3: flying records with an acceleration: the first record of a flight
    // whose attach the refire check made, and any other (there is none).
    let mut flying_after_refire = 0;
    let mut flying_unexplained: Vec<String> = Vec::new();
    // M3: zero records after a release, by whether the button was still held.
    let mut after_release: BTreeMap<(&str, usize), usize> = BTreeMap::new();
    // M4/M5: samples within 0.03° of an axis in the previous record's
    // truncated axes; how many are on it (0.0002°) there, with exact angles,
    // and in the same record's axes.
    let (mut near, mut on_axis, mut on_axis_exact, mut on_axis_same) = (0, 0, 0, 0);
    // M6: first moves after a flight that start from a kept pitch.
    let (mut pitched, mut apart, mut full_continues, mut yaw_continues) = (0, 0, 0, 0);
    let (mut d_full, mut d_yaw) = (Vec::new(), Vec::new());
    let mut largest_pitch = 0_i32;
    // M6, the lean alone: windows of six consecutive derived samples (no
    // pitch) in which the lean changes the direction by 0.05° or more, so
    // that a stick held still shows as still in one kind of axes only.
    let (mut lean_windows, mut still_full, mut still_yaw) = (0, 0, 0);
    let (mut derived, mut leaning, mut largest_lean_effect) = (0, 0, 0.0_f64);
    for (name, file) in &files {
        let pad = file
            .records
            .iter()
            .filter_map(|r| r.player.as_ref())
            .any(|p| p.keys.iter().any(|k| k.starts_with("XboxTypeS_")));
        println!(
            "{name}: {} records, gamepad buttons: {pad}",
            file.records.len()
        );
        for run in split_runs(&file.records) {
            let players: Vec<&RawPlayer> = run.iter().map(|r| r.player.as_ref().unwrap()).collect();
            for (k, p) in players.iter().enumerate() {
                let m = magnitude(p);
                if p.physics == PHYS_FLYING {
                    flying += 1;
                    flying_zero += usize::from(m == 0.0);
                    if m != 0.0 {
                        let before = k.checked_sub(1).map(|i| players[i]);
                        if before.is_some_and(|b| attached_by_the_refire_check(b, p)) {
                            flying_after_refire += 1;
                        } else {
                            flying_unexplained.push(format!("{name} frame {}", run[k].frame));
                        }
                    }
                } else {
                    let class = if m == 0.0 {
                        "0"
                    } else if (m - 2048.0).abs() < 0.01 {
                        "2048"
                    } else if (m - 1.0).abs() < 0.001 {
                        "1"
                    } else {
                        "other"
                    };
                    *magnitudes.entry(class).or_default() += 1;
                    let pitch = asamu_core::rotator::normalize_rotator_axis(p.pawn_rotation[0]);
                    largest_pitch = largest_pitch.max(pitch.abs());
                }
            }
            // (direction in the full axes, in yaw-only axes) of the current
            // stretch of consecutive derived samples without pitch.
            let mut stretch: Vec<(f64, f64)> = Vec::new();
            let mut count_windows = |stretch: &mut Vec<(f64, f64)>| {
                let mut i = 0;
                while i + 6 <= stretch.len() {
                    let w = &stretch[i..i + 6];
                    let full: Vec<f64> = w.iter().map(|x| wrap_deg(x.0 - w[0].0)).collect();
                    let yaw: Vec<f64> = w.iter().map(|x| wrap_deg(x.1 - w[0].1)).collect();
                    let range = |v: &[f64]| {
                        v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                            - v.iter().copied().fold(f64::INFINITY, f64::min)
                    };
                    let effect: Vec<f64> = yaw.iter().zip(&full).map(|(a, b)| a - b).collect();
                    if range(&effect) >= 0.05 {
                        lean_windows += 1;
                        still_full += usize::from(range(&full) < 0.02);
                        still_yaw += usize::from(range(&yaw) < 0.02);
                        i += 6;
                    } else {
                        i += 1;
                    }
                }
                stretch.clear();
            };
            for k in 1..players.len() {
                let (before, now) = (players[k - 1], players[k]);
                let level_before =
                    asamu_core::rotator::normalize_rotator_axis(before.pawn_rotation[0]) & !3 == 0;
                if attached(now) || magnitude(now) == 0.0 || !level_before {
                    count_windows(&mut stretch);
                } else {
                    let full = angle_deg(before.pawn_rotation, now, MoveFrame::Original).unwrap();
                    let yaw_only = [0, before.pawn_rotation[1], 0];
                    let flat = angle_deg(yaw_only, now, MoveFrame::Original).unwrap();
                    stretch.push((full, flat));
                    derived += 1;
                    let effect = wrap_deg(full - flat).abs();
                    leaning += usize::from(effect > 1e-3);
                    largest_lean_effect = largest_lean_effect.max(effect);
                }
                // A release: the record after the last attached one.
                if before.gun.as_ref().is_some_and(|g| g.grappling)
                    && !now.gun.as_ref().is_some_and(|g| g.grappling)
                {
                    let held = before.keys.iter().any(|key| key.contains("RightTrigger"));
                    let mut zero = 0;
                    for p in players.iter().skip(k).take(10) {
                        if attached(p) || magnitude(p) > 0.0 {
                            break;
                        }
                        zero += 1;
                    }
                    let kind = if held { "button held" } else { "button up" };
                    *after_release.entry((kind, zero)).or_default() += 1;
                }
                if attached(now) || magnitude(now) == 0.0 {
                    continue;
                }
                let truncated = angle_deg(before.pawn_rotation, now, MoveFrame::Original).unwrap();
                if off_axis(truncated) < 0.03 {
                    near += 1;
                    on_axis += usize::from(off_axis(truncated) < 2e-4);
                    // The exact-angle axes are yaw-only: compare where the
                    // pawn has no pitch or roll to speak of.
                    let flat = [0, before.pawn_rotation[1], 0];
                    let exact = angle_deg(flat, now, MoveFrame::Yaw).unwrap();
                    on_axis_exact += usize::from(off_axis(exact) < 2e-4);
                    let same = angle_deg(now.pawn_rotation, now, MoveFrame::Original).unwrap();
                    on_axis_same += usize::from(off_axis(same) < 2e-4);
                }
                // M6: the previous record still has the flight's pitch (8°
                // or more), this one has none, and the next frame moves too.
                let pitch = asamu_core::rotator::normalize_rotator_axis(before.pawn_rotation[0]);
                let level = asamu_core::rotator::normalize_rotator_axis(now.pawn_rotation[0]) == 0;
                if pitch.abs() >= 1456
                    && level
                    && let Some(next) = players.get(k + 1)
                    && !attached(next)
                    && let Some(following) = angle_deg(now.pawn_rotation, next, MoveFrame::Original)
                {
                    pitched += 1;
                    let yaw_only = [0, before.pawn_rotation[1], 0];
                    let flat = angle_deg(yaw_only, now, MoveFrame::Original).unwrap();
                    if wrap_deg(truncated - flat).abs() > 0.25 {
                        apart += 1;
                        let (a, b) = (
                            wrap_deg(truncated - following).abs(),
                            wrap_deg(flat - following).abs(),
                        );
                        full_continues += usize::from(a < 0.2);
                        yaw_continues += usize::from(b < 0.2);
                        d_full.push(a);
                        d_yaw.push(b);
                    }
                }
            }
            count_windows(&mut stretch);
        }
    }
    println!(
        "M6 lean alone: {derived} derived samples without pitch, {leaning} with a lean that \
         changes the direction by more than 0.001 deg (up to {largest_lean_effect:.2} deg); {lean_windows} windows of \
         6 samples in which that change varies by 0.05 deg or more: direction still (0.02 deg) in \
         the full axes in {still_full}, in yaw-only axes in {still_yaw}"
    );
    println!("M2 horizontal |Acceleration| while walking or falling: {magnitudes:?}");
    println!(
        "M3 flying records: {flying}, with zero acceleration: {flying_zero}, with the acceleration \
         of the tick in which the refire check attached: {flying_after_refire}, others: \
         {flying_unexplained:?}"
    );
    println!(
        "M3 zero-acceleration records after a release (button, count) -> releases: {after_release:?}"
    );
    println!(
        "M4/M5 samples within 0.03 deg of an axis (previous record's truncated axes): {near}; on \
         the axis to 0.0002 deg: {on_axis}; with exact angles (yaw only): {on_axis_exact}; in the \
         same record's axes: {on_axis_same}"
    );
    println!(
        "M6 first moves after a flight with a kept pitch >= 8 deg: {pitched}; full and yaw-only \
         axes more than 0.25 deg apart: {apart}; direction continues into the next frame (0.2 \
         deg) with the full axes: {full_continues} (median {:.6} deg), yaw-only: {yaw_continues} \
         (median {:.3} deg); largest pitch outside a flight: {:.1} deg",
        median(&mut d_full),
        median(&mut d_yaw),
        f64::from(largest_pitch) * 360.0 / 65536.0
    );
    assert_eq!(magnitudes.get("other"), None, "M2: {magnitudes:?}");
    // M3: no steering while attached. A flying record has no acceleration,
    // with one kind of exception that only a recording of recorder 0.2.0
    // can show (the weapon's state and timers): the first record of a
    // flight whose attach was made by the weapon's refire check instead of
    // the button press keeps the acceleration of the move of that tick; the
    // next record has none. In the recording of 2026-10-12 (52,266 records,
    // keyboard and mouse) 66 flights begin after a record of the frame
    // before: 62 by the press (weapon `Active` before; no acceleration in
    // the first flying record of any, 27 of them with a move held in the
    // record before) and 4 by the refire check (weapon `WeaponFiring`
    // before, the refire count wraps in that tick), of which 1 had a move
    // held and keeps its acceleration (frame 38296) and 3 had none. The
    // three recordings of 2026-10-10 (recorder 0.1.0, 9,601 flying records)
    // have no flying record with an acceleration at all, and for them this
    // is the assertion it was: `flying == flying_zero`.
    assert_eq!(
        flying_unexplained,
        Vec::<String>::new(),
        "M3: steering while attached"
    );
    assert_eq!(flying, flying_zero + flying_after_refire, "M3");
    assert!(
        !after_release.keys().any(|(_, zero)| *zero == 0),
        "M3: a move in the frame of a release: {after_release:?}"
    );
    if near >= 100 {
        assert!(on_axis * 100 >= near * 99, "M4/M5: {on_axis} of {near}");
        assert!(
            on_axis_exact * 2 < near,
            "M5: exact angles fit {on_axis_exact} of {near}"
        );
        assert!(
            on_axis_same < on_axis,
            "M4: the same record's axes fit as well"
        );
    }
    if apart >= 10 {
        assert!(full_continues > 2 * yaw_continues, "M6");
    }
    if lean_windows >= 50 {
        assert!(
            still_full > still_yaw,
            "M6 (lean): {still_full} against {still_yaw}"
        );
    }
}

/// The anchor of an attached grapple is the gun's `vGrappleLocation`: the
/// gun's own `vDistance` is the pawn's distance to it. The anchor helper's
/// location is something else on many records (`asamu_trace::convert`,
/// "Units and fields").
#[test]
fn the_guns_distance_is_measured_to_its_hit_location() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    let (mut attached_records, mut to_location, mut to_helper) = (0, 0, 0);
    let (mut worst, mut worst_at_attach) = (0.0_f32, 0);
    let mut helper_off = Vec::new();
    for (_, file) in &files {
        let mut before = false;
        for p in file.records.iter().filter_map(|r| r.player.as_ref()) {
            let Some(g) = p.gun.as_ref().filter(|g| g.grappling) else {
                before = false;
                continue;
            };
            attached_records += 1;
            let off = ((g.grapple_location - p.location).length() - g.distance).abs();
            to_location += usize::from(off < 0.01);
            if off >= 0.01 {
                worst = worst.max(off);
                worst_at_attach += usize::from(!before);
            }
            if let Some(helper) = g.anchor {
                let off = ((helper - p.location).length() - g.distance).abs();
                to_helper += usize::from(off < 1.0);
                helper_off.push(f64::from(off));
            }
            before = true;
        }
    }
    println!(
        "attached records: {attached_records}; vDistance = |vGrappleLocation - Location| within \
         0.01 uu on {to_location} (the others: up to {worst:.2} uu off, {worst_at_attach} of them \
         the first record of a grapple); = |helper - Location| within 1 uu on {to_helper} (median \
         difference {:.1} uu)",
        median(&mut helper_off)
    );
    if attached_records >= 100 {
        assert!(to_location * 100 >= attached_records * 99);
    }
}

/// Every recording converts; a gamepad run gets a direction for exactly the
/// records that have an acceleration outside a grapple; and, with converted
/// levels, a replay from a standing start gets the original's inputs and
/// start state. How far our pawn then moves is printed, not asserted (that
/// is the simulation's parity, measured elsewhere), except that it moves
/// somewhere in at least one replay whose original moves.
#[test]
fn recordings_convert_and_replay_from_a_standing_start() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    let converted_dir = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from);
    let (mut replayed, mut moving, mut moved_too) = (0, 0, 0);
    for (name, file) in &files {
        let segments = convert(file, &ConvertOptions::default()).unwrap();
        let runs = split_runs(&file.records);
        assert_eq!(segments.len(), runs.len(), "{name}");
        for (segment, run) in segments.iter().zip(&runs) {
            let trace = &segment.trace;
            trace.validate().unwrap();
            let from_acceleration = trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("move input: derived from the pawn's Acceleration"));
            if from_acceleration {
                for (k, s) in trace.samples.iter().enumerate().skip(1) {
                    let p = run[k].player.as_ref().unwrap();
                    let moves = s.input.move_forward != 0.0 || s.input.move_right != 0.0;
                    assert_eq!(moves, !attached(p) && magnitude(p) > 0.0, "{name} tick {k}");
                    if moves {
                        let len =
                            f64::from(s.input.move_forward).hypot(f64::from(s.input.move_right));
                        assert!((len - 1.0).abs() < 1e-6, "{name} tick {k}: {len}");
                    }
                }
            }
            assert!(
                asamu_trace::state::StateTimeline::from_notes(&trace.meta.notes)
                    .unwrap()
                    .is_some()
            );
            // A replay: from the standing stretch before the first move that
            // follows one, 150 ticks.
            let (Some(dir), Some(level)) = (&converted_dir, &trace.meta.level) else {
                continue;
            };
            let scene = std::fs::read_dir(dir.join("levels"))
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter_map(|n| n.strip_suffix(".scene.json").map(str::to_owned))
                .find(|n| n.eq_ignore_ascii_case(level));
            let Some(map) = scene else { continue };
            let all = events(&trace.samples);
            let stretches = clean_runs(&trace.samples);
            let start = stretches.iter().find_map(|(a, b)| {
                (b - a >= 2
                    && all
                        .iter()
                        .any(|e| e.kind == EventKind::MoveBegins && e.index == b + 1))
                .then_some(*b - 1)
            });
            let Some(start) = start else { continue };
            let r = replay(
                trace,
                &ReplayOptions {
                    level: ReplayLevel::Converted {
                        dir: dir.clone(),
                        map: map.clone(),
                        kismet: false,
                    },
                    start_tick: Some(trace.samples[start].tick),
                    max_ticks: Some(150),
                    ..ReplayOptions::default()
                },
            )
            .unwrap();
            let s = compare_traces(
                "original",
                trace,
                "replay",
                &r.trace,
                &asamu_player::trace::CompareTolerances::default(),
            );
            assert_eq!(s.diff.input_mismatches, 0, "{name} from {start}");
            assert!(
                r.trace
                    .meta
                    .notes
                    .iter()
                    .any(|n| n.starts_with("start state applied")),
                "{name}: {:#?}",
                r.trace.meta.notes
            );
            let first = &r.trace.samples[0];
            let moved = r
                .trace
                .samples
                .iter()
                .map(|x| (x.position - first.position).length())
                .fold(0.0, f32::max);
            let original_moved = trace.samples[start..]
                .iter()
                .take(r.trace.samples.len())
                .map(|x| (x.position - trace.samples[start].position).length())
                .fold(0.0, f32::max);
            println!(
                "{name} on {map} from tick {start}, {} ticks: the original moves {original_moved:.1} uu, \
                 our replay {moved:.1} uu; largest position difference {:.3} uu, velocity {:.3} uu/s; \
                 attached samples {}",
                r.trace.samples.len() - 1,
                s.diff.position.max,
                s.diff.velocity.max,
                r.trace
                    .samples
                    .iter()
                    .filter(|x| x.grapple_state == TraceGrappleState::Attached)
                    .count()
            );
            if original_moved > 10.0 {
                moving += 1;
                moved_too += usize::from(moved > 10.0);
            }
            replayed += 1;
        }
    }
    println!(
        "{replayed} segment(s) replayed; the original moves in {moving}, our replay in {moved_too} \
         of those"
    );
    assert!(moving == 0 || moved_too > 0, "no replay moves");
}

/// The recorder's sidecar of a recording in `ASAMU_TRACE_RAW_DIR`, if it has
/// one.
fn sidecar(name: &str) -> Option<asamu_trace::sidecar::RecorderLog> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_TRACE_RAW_DIR")?);
    let path = asamu_trace::sidecar::sidecar_path(&dir.join(name))?;
    path.is_file()
        .then(|| asamu_trace::sidecar::RecorderLog::read(&path).unwrap())
}

/// What recorder 0.2.0 adds, on real recordings. A version-1 recording has
/// none of it, and its conversion with its (0.1.0) sidecar is its
/// conversion without. A recording with optional members converts with the
/// camera's FOV rule; its markers are all placed; and the counts say how
/// many of its frame gaps the dropped-frame log explains.
#[test]
fn optional_members_markers_and_dropped_frames_of_real_recordings() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    for (name, file) in &files {
        let counts = file.optional_counts();
        let plain = convert(file, &ConvertOptions::default()).unwrap();
        let log = sidecar(name);
        println!(
            "{name}: recorder {:?}, {} records, optional members {counts:?}, sidecar: {}",
            file.header.recorder,
            file.records.len(),
            match &log {
                None => "none".to_owned(),
                Some(l) => format!(
                    "{} marker(s), dropped-frame log {}",
                    l.markers.len(),
                    l.drops
                        .as_ref()
                        .map_or("not kept".to_owned(), |d| format!("{} entries", d.len()))
                ),
            }
        );
        let converted = match &log {
            None => plain.clone(),
            Some(l) => {
                l.check_belongs_to(file).unwrap();
                let opts = ConvertOptions {
                    log: Some(l.clone()),
                    ..ConvertOptions::default()
                };
                convert(file, &opts).unwrap()
            }
        };
        if log
            .as_ref()
            .is_none_or(asamu_trace::sidecar::RecorderLog::is_empty)
        {
            assert_eq!(
                converted, plain,
                "{name}: a sidecar without lists changes nothing"
            );
        }
        let samples: usize = converted.iter().map(|s| s.trace.samples.len()).sum();
        if counts.is_empty() {
            // Version 1: the FOV column is the cached view FOV wherever the
            // camera's FOV was read, and the state line has the version-1
            // fields only.
            assert!(file.header.optional_fields.is_empty(), "{name}");
            for seg in &converted {
                let notes = &seg.trace.meta.notes;
                assert!(!notes.iter().any(|n| n.starts_with("fov: camera FOV lock")));
                let state = notes.iter().find(|n| n.starts_with("state: ")).unwrap();
                for key in [
                    "floor",
                    "weapon_state",
                    "refire",
                    "gun_timers",
                    "cylinder",
                    "base_level",
                    "base_eye_height",
                ] {
                    assert!(
                        !state.contains(key),
                        "{name}: {key} in a version-1 state note"
                    );
                }
            }
        } else {
            // The camera's rule on every sample, and how often it zoomed.
            let (mut read, mut locked) = (0, 0);
            let mut locked_fovs: BTreeMap<String, usize> = BTreeMap::new();
            for seg in &converted {
                let first = file
                    .records
                    .iter()
                    .position(|r| r.frame == seg.first_frame)
                    .unwrap();
                for (k, s) in seg.trace.samples.iter().enumerate() {
                    let p = file.records[first + k].player.as_ref().unwrap();
                    let usable = |f: f32| f > 0.0 && f < 180.0;
                    match (p.fov_locked, p.fov_lock) {
                        (Some(true), Some(lock)) if usable(lock) => {
                            assert_eq!(s.fov, lock, "{name}: frame {}", seg.first_frame + k as u64);
                            read += 1;
                            locked += 1;
                            *locked_fovs.entry(format!("{lock}")).or_insert(0) += 1;
                        }
                        (Some(false), _) => {
                            read += 1;
                            if let Some(f) = p.fov_camera.filter(|f| usable(*f)) {
                                assert_eq!(s.fov, f, "{name}");
                            }
                        }
                        _ => {}
                    }
                }
            }
            println!(
                "    camera FOV lock read on {read} of {samples} samples, set on {locked}; locked FOVs: {locked_fovs:?}"
            );
        }
        if let Some(l) = &log {
            // Every marker is in exactly one run's notes.
            let placed: usize = converted
                .iter()
                .map(|s| {
                    asamu_trace::segments::markers_of(&s.trace.meta.notes)
                        .unwrap()
                        .len()
                })
                .sum();
            if !converted.is_empty() {
                assert_eq!(placed, l.markers.len(), "{name}: markers placed");
            }
            for seg in &converted {
                for m in asamu_trace::segments::markers_of(&seg.trace.meta.notes).unwrap() {
                    println!(
                        "    marker {:?}: frames {}..={}, tick {} ({:?})",
                        m.name, seg.first_frame, seg.last_frame, m.tick, m.place
                    );
                }
            }
            if let Some(drops) = &l.drops {
                // The recorder logs every frame it has no record for.
                let (mut gaps, mut explained) = (0, 0);
                for w in file.records.windows(2) {
                    let (a, b) = (w[0].frame, w[1].frame);
                    if b > a + 1 {
                        gaps += 1;
                        explained +=
                            usize::from((a + 1..b).all(|f| {
                                drops.iter().any(|d| !d.is_no_input() && d.overlaps(f, f))
                            }));
                    }
                }
                let no_input = l.no_input_frames();
                let in_the_middle = file
                    .records
                    .windows(2)
                    .filter(|w| no_input.contains(&w[0].frame) && w[1].frame == w[0].frame + 1)
                    .count();
                println!(
                    "    {gaps} frame gap(s) between records, {explained} with a log entry for every frame; \
                     {} record(s) without input, {in_the_middle} of them followed by the next frame's record; \
                     {} frame(s) not logged",
                    no_input.len(),
                    l.drops_omitted
                );
                // (Counted, not asserted: that the log accounts for every
                // gap is the recorder's own test; the converter writes
                // "no entry" for one it does not explain and ends a run
                // after a record without input wherever it is.)
            }
        }
    }
}

/// `true` if the pawn of `p` stands on a mover (an `InterpActor`).
fn on_a_mover(p: &RawPlayer) -> bool {
    p.base
        .as_deref()
        .is_some_and(|b| b.starts_with("InterpActor"))
}

/// Findings V17 (NATIVE_PHYSICS.md 2.4): after a walking tick the pawn's
/// velocity is its displacement times the `f32` reciprocal of the frame
/// length, component by component, with no vertical part; not the
/// displacement divided by the frame length. Counted over every pair of
/// consecutive walking records whose pawn does not stand on a mover.
///
/// - The three recordings of 2026-10-10 (about 61 frames a second): 27,443
///   pairs, all of them in the reciprocal form and 23,839 in the division
///   form too.
/// - The recording of 2026-10-12 (78 frames a second): 25,630 pairs, all in
///   the reciprocal form, 20,610 in the division form.
///
/// On a mover the velocity leaves the mover's own movement out while the
/// displacement has it, so the identity does not hold there: of the 552
/// pairs on an `InterpActor` in the recording of 2026-10-12, 55 are in the
/// form (the mover stood still). Those pairs are counted apart and not
/// judged.
#[test]
fn walking_velocity_is_the_displacement_times_the_reciprocal_of_the_frame_length() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    for (name, file) in &files {
        let (mut pairs, mut reciprocal, mut division) = (0, 0, 0);
        let (mut on_movers, mut on_movers_in_form) = (0, 0);
        let mut first_miss = None;
        for run in split_runs(&file.records) {
            for w in run.windows(2) {
                let (a, b) = (w[0].player.as_ref().unwrap(), w[1].player.as_ref().unwrap());
                if a.physics != 1 || b.physics != 1 {
                    continue;
                }
                let dt = w[1].world.as_ref().unwrap().delta_seconds;
                let (dx, dy) = (b.location.x - a.location.x, b.location.y - a.location.y);
                let r = 1.0_f32 / dt;
                let is = |x: f32, y: f32| {
                    b.velocity.x.to_bits() == x.to_bits()
                        && b.velocity.y.to_bits() == y.to_bits()
                        && b.velocity.z == 0.0
                };
                if on_a_mover(a) || on_a_mover(b) {
                    on_movers += 1;
                    on_movers_in_form += usize::from(is(dx * r, dy * r));
                    continue;
                }
                pairs += 1;
                if is(dx * r, dy * r) {
                    reciprocal += 1;
                } else if first_miss.is_none() {
                    first_miss = Some(w[1].frame);
                }
                division += usize::from(is(dx / dt, dy / dt));
            }
        }
        println!(
            "{name}: {pairs} pairs of consecutive walking records off movers: velocity = \
             displacement x (1/dt) on {reciprocal}, = displacement / dt on {division}; on a mover \
             {on_movers} pairs, {on_movers_in_form} of them in the form"
        );
        assert_eq!(
            reciprocal, pairs,
            "{name}: first miss at frame {first_miss:?}"
        );
        // The counts the documentation quotes, by recording.
        let expected = match (file.records.len(), name.as_str()) {
            (3_001, n) if n.contains("WS1") => Some((3_000, 2_586, 0, 0)),
            (14_311, n) if n.contains("DC1") => Some((8_187, 7_085, 0, 0)),
            (27_925, n) if n.contains("PLAY2") => Some((16_256, 14_168, 0, 0)),
            (52_266, n) if n.contains("KM1") => Some((25_630, 20_610, 552, 55)),
            _ => None,
        };
        if let Some(expected) = expected {
            assert_eq!(
                (pairs, division, on_movers, on_movers_in_form),
                expected,
                "{name}"
            );
        }
    }
}

/// Findings V18: the grapple is attached (the gun's `bGrappling`) exactly
/// on the records whose physics mode is flying, and a flying record has no
/// acceleration (GRAPPLE.md G-PH-1: no steering while attached), with one
/// exception that only a recording of recorder 0.2.0 can show: the first
/// record of a flight that the weapon's refire check attached keeps the
/// acceleration of that tick's move (`attached_by_the_refire_check`; fact
/// M3 above).
///
/// The three recordings of 2026-10-10: 9,601 flying records, none with
/// acceleration. The recording of 2026-10-12: 8,077, one with acceleration
/// (frame 38296, the first record of such a flight).
#[test]
fn the_grapple_is_attached_exactly_while_the_pawn_flies() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    for (name, file) in &files {
        let (mut flying, mut with_acceleration) = (0, 0);
        let mut before: Option<&asamu_trace::raw::RawRecord> = None;
        for r in &file.records {
            let Some(p) = r.player.as_ref() else {
                before = None;
                continue;
            };
            let grappling = p.gun.as_ref().is_some_and(|g| g.grappling);
            flying += usize::from(p.physics == PHYS_FLYING);
            assert_eq!(
                grappling,
                p.physics == PHYS_FLYING,
                "{name} frame {}: physics {}",
                r.frame,
                p.physics
            );
            if grappling && magnitude(p) != 0.0 {
                with_acceleration += 1;
                // Only the first record of a flight, and only one the
                // refire check attached.
                let q = before
                    .filter(|b| b.frame + 1 == r.frame)
                    .and_then(|b| b.player.as_ref());
                assert!(
                    q.is_some_and(|q| attached_by_the_refire_check(q, p)),
                    "{name} frame {}: a flying record with acceleration",
                    r.frame
                );
            }
            before = Some(r);
        }
        println!("{name}: {flying} flying records, {with_acceleration} with acceleration");
        let expected = match (file.records.len(), name.as_str()) {
            (3_001, n) if n.contains("WS1") => Some((0, 0)),
            (14_311, n) if n.contains("DC1") => Some((3_822, 0)),
            (27_925, n) if n.contains("PLAY2") => Some((5_779, 0)),
            (52_266, n) if n.contains("KM1") => Some((8_077, 1)),
            _ => None,
        };
        if let Some(expected) = expected {
            assert_eq!((flying, with_acceleration), expected, "{name}");
        }
    }
}

/// `true` for a record of AG-Workshop.
fn in_the_workshop(r: &asamu_trace::raw::RawRecord) -> bool {
    r.world
        .as_ref()
        .and_then(|w| w.map.as_deref())
        .is_some_and(|m| m.eq_ignore_ascii_case("AG-Workshop"))
}

/// Findings P6, its open point ("whether a sprint press in the Workshop
/// overwrites 132"; session item 2 of section 6): it does not. The
/// Workshop's level script sets the speed by the console command
/// `setspeed 0.3`, and 440 × 0.3 = 132 (`GroundSpeed` class default 440,
/// ABILITIES.md). In every Workshop record of every recording `GroundSpeed`
/// is exactly that and the pawn's sprint flag is clear, also on the frames
/// with the sprint key and a forward key held: the pawn is in story mode
/// there, which ignores the sprint (A-ST-1).
///
/// The recording of 2026-10-10 (WS1, gamepad): 3,001 Workshop records, no
/// sprint press. The recording of 2026-10-12 (keyboard): 14,017 Workshop
/// records, 1,950 of them with `LeftShift` and `W` held.
#[test]
fn the_workshops_console_speed_survives_a_held_sprint_key() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    for (name, file) in &files {
        let (mut records, mut sprint_held) = (0, 0);
        for r in file.records.iter().filter(|r| in_the_workshop(r)) {
            let Some(p) = r.player.as_ref() else { continue };
            records += 1;
            assert_eq!(p.ground_speed, 440.0 * 0.3, "{name} frame {}", r.frame);
            assert!(
                !p.pawn_flags.as_ref().is_some_and(|f| f.sprinting),
                "{name} frame {}: the sprint flag",
                r.frame
            );
            let held = |key: &str| p.keys.iter().any(|k| k == key);
            sprint_held += usize::from(held("LeftShift") && held("W"));
        }
        if records == 0 {
            continue;
        }
        println!(
            "{name}: {records} Workshop records, GroundSpeed 132 and no sprint flag on all; \
             LeftShift and W held on {sprint_held}"
        );
        if file.records.len() == 52_266 && name.contains("KM1") {
            assert_eq!((records, sprint_held), (14_017, 1_950));
        }
    }
}

/// Findings section 3 ("ViewPitchMax +18000: CONFIRMED, upper limit only")
/// and section 7 ("the lower pitch limit ... in no recording"): the view
/// pitch, as a signed 16-bit angle, never leaves −18001 to +18000 rotator
/// units. The lower limit is one unit beyond −18000 because the engine's
/// rule adds the minimum to 65535, not to 65536 (the controller's view
/// limit, read locally; `asamu_player::sim` has the same two numbers): the
/// stored value is 47535.
///
/// The recording of 2026-10-12 is the first to reach the lower limit: 118
/// records at exactly −18001 and 17 at exactly +18000 (session item 4 of
/// section 6: "look fully down and fully up").
#[test]
fn the_view_pitch_stays_between_its_two_limits() {
    let Some(files) = recordings() else {
        eprintln!("skipped: ASAMU_TRACE_RAW_DIR is not set to a directory with recordings");
        return;
    };
    for (name, file) in &files {
        let (mut lowest, mut highest) = (0, 0);
        for r in &file.records {
            let Some(p) = r.player.as_ref() else { continue };
            // The low 16 bits, as a signed angle.
            let pitch = (p.view_rotation[0] + 32_768).rem_euclid(65_536) - 32_768;
            assert!(
                (-18_001..=18_000).contains(&pitch),
                "{name} frame {}: view pitch {pitch}",
                r.frame
            );
            lowest += usize::from(pitch == -18_001);
            highest += usize::from(pitch == 18_000);
        }
        println!("{name}: view pitch at -18001 on {lowest} records, at +18000 on {highest}");
        if file.records.len() == 52_266 && name.contains("KM1") {
            assert_eq!((lowest, highest), (118, 17));
        }
    }
}
