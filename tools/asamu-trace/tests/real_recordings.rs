//! Recordings of the original game against what the converter assumes about
//! them (`asamu_trace::move_input`, facts M2–M6), and their conversion and
//! replay from a standing start.
//!
//! Set `ASAMU_TRACE_RAW_DIR` to a directory with `*.raw.jsonl` recordings
//! (they stay local: `research/local/traces/...`); skips when unset. With
//! `ASAMU_CONVERTED_DIR` (an `asamu-import` output) a segment of each map
//! that has a converted level is also replayed. Run with `--nocapture` to
//! see the counts the documentation quotes.

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
    p.physics == PHYS_FLYING || p.gun.is_some_and(|g| g.grappling)
}

fn magnitude(p: &RawPlayer) -> f64 {
    horizontal_magnitude(p.acceleration.x, p.acceleration.y)
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
            for p in &players {
                let m = magnitude(p);
                if p.physics == PHYS_FLYING {
                    flying += 1;
                    flying_zero += usize::from(m == 0.0);
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
                if before.gun.is_some_and(|g| g.grappling) && !now.gun.is_some_and(|g| g.grappling)
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
    println!("M3 flying records: {flying}, with zero acceleration: {flying_zero}");
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
    assert_eq!(flying, flying_zero, "M3: steering while attached");
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
            let Some(g) = p.gun.filter(|g| g.grappling) else {
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
