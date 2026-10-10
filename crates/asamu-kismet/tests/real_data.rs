//! Gated tests on the user's converted data (`ASAMU_CONVERTED_DIR` with
//! `kismet/` and `matinee/` from `asamu-import kismet` / `matinee`). They
//! skip when the variable is unset; CI has no game data.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use asamu_kismet::matinee::{TrackData, rotation_translation_matrix};
use asamu_kismet::{NullHost, Runtime, coverage, load_level_scripts};

fn converted() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    d.join("kismet").is_dir().then_some(d)
}

fn maps(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join("kismet"))
        .map(|r| {
            r.filter_map(Result::ok)
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".kismet.json"))
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// This crate's Matinee port reproduces the importer's `asamu_ue3::matinee`
/// samples of every bound move track bit for bit.
#[test]
fn matinee_probes_match_the_reference_evaluator() {
    let Some(dir) = converted() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let mut checked = 0usize;
    for map in maps(&dir) {
        let s = load_level_scripts(&dir, &map, &[]).unwrap();
        for p in &s.graph.probes {
            let (action, data) = s.matinee.data_of(p.action).expect("probe action has data");
            assert_eq!(action.node, p.action);
            let group = data
                .groups
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(&p.group))
                .unwrap();
            let actor = s
                .graph
                .actor(s.graph.actor_by_path(&p.actor).unwrap())
                .unwrap();
            let base = actor.base.and_then(|b| s.graph.actor(b));
            let moves: Vec<_> = group
                .tracks
                .iter()
                .filter_map(|t| match &t.data {
                    TrackData::Move(m) if !t.disabled && m.is_active() => Some(m),
                    _ => None,
                })
                .collect();
            // A probe belongs to one of the group's active move tracks: the
            // one whose samples all match.
            let matched = moves.iter().any(|m| {
                let inst = match base {
                    Some(b) => m.instance_with_base(
                        actor.location,
                        actor.rotation,
                        &rotation_translation_matrix(b.rotation, b.location),
                        p.position,
                    ),
                    None => m.instance(actor.location, actor.rotation, p.position),
                };
                p.samples.iter().all(|smp| {
                    let Some((loc, rot)) = m.sample(smp.t, &inst) else {
                        return false;
                    };
                    let rot = rot.unwrap_or(actor.rotation);
                    loc.map(f32::to_bits) == smp.location.map(f32::to_bits) && rot == smp.rotation
                })
            });
            assert!(
                matched,
                "{map}: probe {} / {} / {}",
                p.action, p.group, p.actor
            );
            checked += 1;
        }
    }
    eprintln!("{checked} Matinee probes matched bit for bit");
}

/// Every converted graph loads, every class it uses is implemented, and a
/// headless run (no game, every host call a no-op) of 600 updates raises
/// no interpreter error.
#[test]
fn every_converted_graph_loads_and_runs_headless() {
    let Some(dir) = converted() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let mut total = std::collections::BTreeMap::new();
    for map in maps(&dir) {
        let s = load_level_scripts(&dir, &map, &[]).unwrap();
        assert!(s.graph.warnings.is_empty(), "{map}: {:?}", s.graph.warnings);
        let c = coverage(&s.graph);
        assert!(c.missing.is_empty(), "{map}: unimplemented {:?}", c.missing);
        for (k, v) in c.present {
            *total.entry(k).or_insert(0usize) += v;
        }
        let mut rt = Runtime::new(Arc::new(s.graph), Arc::new(s.matinee));
        let mut host = NullHost;
        for _ in 0..600 {
            rt.tick(1.0 / 60.0, &mut host);
        }
        let errors: Vec<&String> = rt
            .errors()
            .iter()
            .filter(|e| !e.contains("checkpoint not found") && !e.contains("attractor"))
            .collect();
        assert!(errors.is_empty(), "{map}: {errors:?}");
    }
    eprintln!("classes present across maps: {total:?}");
}

/// Every sound and narrator action's cue has the durations the runtime
/// needs: `SeqAct_PlaySound` runs for its first wave's duration (voice
/// waves live in the map's `_LOC_INT` companion, which the importer must
/// search), the narrator for the cue's `Duration`. A missing duration makes
/// the action finish at once (e.g. Workshop's 32 s prologue line).
#[test]
fn every_sound_action_has_its_durations() {
    let Some(dir) = converted() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let mut checked = 0usize;
    for map in maps(&dir) {
        let s = load_level_scripts(&dir, &map, &[]).unwrap();
        for n in &s.graph.nodes {
            let (prop, first_wave) = match n.class {
                asamu_kismet::OpClass::PlaySound => ("PlaySound", true),
                asamu_kismet::OpClass::NarratorLine => ("Cue", false),
                _ => continue,
            };
            let Some(cue) = n.param(prop).and_then(asamu_kismet::KValue::as_obj) else {
                continue;
            };
            let d = s.graph.sound(cue).unwrap_or_default();
            assert!(d.duration.is_some(), "{map}: {} has no cue duration", n.id);
            if first_wave {
                assert!(
                    d.first_wave_duration.is_some(),
                    "{map}: {} has no first-wave duration",
                    n.id
                );
            }
            checked += 1;
        }
    }
    eprintln!("{checked} sound and narrator actions have their durations");
}

/// Coverage of the shipped Matinee tracks by the runtime (prints the table
/// MATINEE.md quotes): every track class is modelled, and the only tracks
/// with keys that act on something yet are not played are the
/// skeletal-control *scale* tracks.
#[test]
fn matinee_track_coverage() {
    use asamu_kismet::matinee::{TrackClassCoverage, TrackEffect};
    use std::collections::BTreeMap;
    let Some(dir) = converted() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let mut total: BTreeMap<(TrackEffect, String), TrackClassCoverage> = BTreeMap::new();
    for map in maps(&dir) {
        let s = load_level_scripts(&dir, &map, &[]).unwrap();
        for (k, c) in s.matinee.track_coverage() {
            let t = total.entry(k).or_default();
            t.tracks += c.tracks;
            t.with_keys += c.with_keys;
            t.effective += c.effective;
        }
    }
    if total.is_empty() {
        eprintln!("SKIP: no converted Matinee");
        return;
    }
    for ((effect, class), c) in &total {
        println!(
            "{effect:?} {class}: {} tracks, {} with keys, {} effective",
            c.tracks, c.with_keys, c.effective
        );
    }
    let unplayed: Vec<(&String, &TrackClassCoverage)> = total
        .iter()
        .filter(|((e, _), c)| *e == TrackEffect::NotPlayed && c.effective > 0)
        .map(|((_, class), c)| (class, c))
        .collect();
    assert!(
        unplayed
            .iter()
            .all(|(class, _)| class.as_str() == "InterpTrackSkelControlScale"),
        "{unplayed:?}"
    );
    let played: usize = total
        .iter()
        .filter(|((e, _), _)| *e != TrackEffect::NotPlayed)
        .map(|(_, c)| c.effective)
        .sum();
    assert!(played > 100, "{played}");
}

/// Every shipped `SeqAct_PlayCameraAnim` targets the player and names an
/// animation the converted data holds (the gameplay ones of `Startup.upk` or
/// the map's own): pulsing its Play input emits the play output with the
/// action's parameters, pulsing Stop the stop output. 5 actions in the maps
/// (AG-Darkcave 2, AG-StarHaven 3).
#[test]
fn shipped_camera_anim_actions_play_on_the_player() {
    use asamu_kismet::camera_anim::load_camera_anims;
    use asamu_kismet::{OpClass, Output};
    let Some(dir) = converted() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
        return;
    };
    if !dir.join("matinee").join("camera_anims.json").is_file() {
        eprintln!("SKIP: no converted camera animations (run `asamu-import matinee`)");
        return;
    }
    let mut total = 0usize;
    let mut per_map = Vec::new();
    for map in maps(&dir) {
        let s = load_level_scripts(&dir, &map, &[]).unwrap();
        let actions: Vec<usize> = s
            .graph
            .nodes
            .iter()
            .filter(|n| n.class == OpClass::PlayCameraAnim)
            .map(|n| n.id)
            .collect();
        if actions.is_empty() {
            continue;
        }
        let (set, problems) = load_camera_anims(&dir, std::slice::from_ref(&map));
        assert!(problems.is_empty(), "{map}: {problems:?}");
        let mut rt = Runtime::new(Arc::new(s.graph), Arc::new(s.matinee));
        let mut host = NullHost;
        rt.tick(1.0 / 60.0, &mut host);
        rt.take_outputs();
        for &op in &actions {
            for (input, want_play) in [(0usize, true), (1, false)] {
                rt.force_input(op, input);
                rt.tick(1.0 / 60.0, &mut host);
                let cams: Vec<Output> = rt
                    .take_outputs()
                    .into_iter()
                    .filter(|o| matches!(o, Output::CameraAnim { .. }))
                    .collect();
                let [
                    Output::CameraAnim {
                        play,
                        anim: Some(anim),
                        rate,
                        scale,
                        blend_in,
                        blend_out,
                        ..
                    },
                ] = cams.as_slice()
                else {
                    panic!("{map} node {op} input {input}: {cams:?}");
                };
                assert_eq!(*play, want_play, "{map} node {op}");
                assert!(
                    set.get(anim).is_some(),
                    "{map} node {op}: animation not converted"
                );
                assert!(
                    rate.is_finite() && scale.is_finite() && *blend_in >= 0.0 && *blend_out >= 0.0,
                    "{map} node {op}"
                );
            }
            total += 1;
        }
        per_map.push((map, actions.len()));
    }
    println!("SeqAct_PlayCameraAnim actions: {total} {per_map:?}");
    // The census (a partial conversion checks the maps it has).
    for (map, n) in &per_map {
        match map.as_str() {
            "AG-Darkcave" => assert_eq!(*n, 2, "{map}"),
            "AG-StarHaven" => assert_eq!(*n, 3, "{map}"),
            other => panic!("unexpected camera-animation action in {other}"),
        }
    }
}
