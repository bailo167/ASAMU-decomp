//! Replays on a level converted from the user's own install.
//!
//! Set `ASAMU_CONVERTED_DIR` to an `asamu-import` output directory (levels,
//! collision meshes and, for the Kismet case, `kismet`/`matinee` exports);
//! `ASAMU_TRACE_MAP` picks the map (default `AG-Workshop`). Skips when unset.
//! A runtime recording made on the converted level must replay exactly.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use asamu_game::{Game, load_level_with_kismet};
use asamu_player::InputFrame;
use asamu_player::trace::compare;
use asamu_trace::replay::{ReplayLevel, ReplayOptions, replay};

fn converted() -> Option<(PathBuf, String)> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let map = std::env::var("ASAMU_TRACE_MAP").unwrap_or_else(|_| "AG-Workshop".to_owned());
    dir.join("levels")
        .join(format!("{map}.scene.json"))
        .is_file()
        .then_some((dir, map))
}

fn inputs(n: usize) -> Vec<InputFrame> {
    (0..n)
        .map(|i| InputFrame {
            move_forward: if i < 100 { 1.0 } else { 0.0 },
            move_right: if (40..60).contains(&i) { 1.0 } else { 0.0 },
            look_yaw_delta: if i == 10 { 0.25 } else { 0.0 },
            jump_pressed: i == 70,
            jump_held: (70..80).contains(&i),
            sprint_held: (10..50).contains(&i),
            ..InputFrame::default()
        })
        .collect()
}

#[test]
fn converted_level_replays_exactly() {
    let Some((dir, map)) = converted() else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR is not set to a converted directory with the map");
        return;
    };
    let mut g = Game::load_level(&dir, &map).unwrap();
    g.start();
    g.start_recording();
    for i in inputs(150) {
        g.tick(&i).unwrap();
    }
    let ours = g.stop_recording().unwrap();
    let opts = ReplayOptions {
        level: ReplayLevel::Converted {
            dir: dir.clone(),
            map: map.clone(),
            kismet: false,
        },
        ..ReplayOptions::default()
    };
    let r = replay(&ours, &opts).unwrap();
    let d = compare(&ours, &r.trace);
    assert!(d.is_exact(), "{d:?}");
    assert!(
        ours.samples
            .iter()
            .any(|s| s.position != ours.samples[0].position)
    );
}

#[test]
fn converted_level_with_kismet_replays_exactly() {
    let Some((dir, map)) = converted() else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR is not set to a converted directory with the map");
        return;
    };
    // A load error is a failure (the scene exists); only a missing Kismet
    // export skips.
    let (mut g, script) = load_level_with_kismet(&dir, &map).unwrap();
    let Some(mut script) = script else {
        eprintln!("skipped: no Kismet export for {map}");
        return;
    };
    g.start();
    g.start_recording();
    for i in inputs(150) {
        script.tick(&mut g, &i).unwrap();
    }
    let ours = g.stop_recording().unwrap();
    let opts = ReplayOptions {
        level: ReplayLevel::Converted {
            dir,
            map,
            kismet: true,
        },
        ..ReplayOptions::default()
    };
    let r = replay(&ours, &opts).unwrap();
    let d = compare(&ours, &r.trace);
    assert!(d.is_exact(), "{d:?}");
    assert!(
        r.trace
            .meta
            .notes
            .iter()
            .any(|n| n.contains("with Kismet at")),
        "{:#?}",
        r.trace.meta.notes
    );
    // A segment replay says that Kismet restarted from level start.
    let seg = replay(
        &ours,
        &ReplayOptions {
            start_tick: Some(50),
            max_ticks: Some(30),
            ..opts
        },
    )
    .unwrap();
    assert_eq!(seg.trace.samples.len(), 31);
    assert!(
        seg.trace
            .meta
            .notes
            .iter()
            .any(|n| n.starts_with("note: Kismet starts from level start")),
        "{:#?}",
        seg.trace.meta.notes
    );
}
