//! Replays on a level converted from the user's own install.
//!
//! Set `ASAMU_CONVERTED_DIR` to an `asamu-import` output directory (levels,
//! collision meshes and, for the Kismet case, `kismet`/`matinee` exports);
//! `ASAMU_TRACE_MAP` picks the map (default `AG-Workshop`). Skips when unset.
//! A runtime recording made on the converted level must replay exactly.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use asamu_game::{Game, load_level_with_kismet};
use asamu_player::trace::compare;
use asamu_player::{InputFrame, MovementModelKind, PlayerParams};
use asamu_trace::replay::{ReplayLevel, ReplayOptions, Stepping, replay};

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

/// Per-sample frame lengths on a converted level (the stepper's reduced
/// scope: player and level objects against the level's collision). With
/// frames of equal length it must be the fixed tick's run, as long as the
/// run does not die (deaths are scene logic the stepper does not have).
#[test]
fn converted_level_per_sample_replay_matches_the_fixed_tick() {
    let Some((dir, map)) = converted() else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR is not set to a converted directory with the map");
        return;
    };
    let mut g = Game::load_level(&dir, &map).unwrap();
    g.start();
    g.start_recording();
    let mut died = false;
    for i in inputs(150) {
        let r = g.tick(&i).unwrap();
        died |= r.died.is_some() || r.respawned;
    }
    let ours = g.stop_recording().unwrap();
    let level = ReplayLevel::Converted {
        dir: dir.clone(),
        map: map.clone(),
        kismet: false,
    };
    let per_sample = ReplayOptions {
        level: level.clone(),
        variable_dt: true,
        ..ReplayOptions::default()
    };
    let r = replay(&ours, &per_sample).unwrap();
    assert_eq!(r.stepping, Stepping::PerSample);
    assert_eq!(r.trace.samples.len(), ours.samples.len());
    assert!(
        r.trace.meta.notes.iter().any(|n| n.starts_with(
            "note: on a converted level this stepper simulates the player and the level objects"
        )),
        "{:#?}",
        r.trace.meta.notes
    );
    if died {
        eprintln!("the run dies on {map}: equality with the fixed tick not checked");
    } else {
        let d = compare(&ours, &r.trace);
        assert!(d.is_exact(), "{d:?}");
    }

    // The same inputs with uneven frame lengths: no fixed rate, replayed by
    // default with those lengths, deterministic, and another run than the
    // fixed tick's.
    let mut uneven = ours.clone();
    uneven.meta.tick_rate = None;
    let mut time = 0.0_f64;
    for (k, s) in uneven.samples.iter_mut().enumerate().skip(1) {
        time += f64::from(0.012 + 0.0015 * (k % 6) as f32);
        s.time = time;
    }
    let opts = ReplayOptions {
        level,
        ..ReplayOptions::default()
    };
    let a = replay(&uneven, &opts).unwrap();
    let b = replay(&uneven, &opts).unwrap();
    assert_eq!(a.stepping, Stepping::PerSample);
    assert_eq!(
        a.trace.to_jsonl_string().unwrap(),
        b.trace.to_jsonl_string().unwrap()
    );
    assert_eq!(a.variable.unwrap().lengths.steps, 150);
    assert!(a.trace.samples.iter().all(|s| s.position.is_finite()));
    assert!(!compare(&ours, &a.trace).is_exact());
    for (x, y) in a.trace.samples.iter().zip(&uneven.samples) {
        assert_eq!(x.time.to_bits(), y.time.to_bits());
        assert_eq!(x.input, y.input);
    }

    // Kismet needs the game's own (fixed) tick.
    let with_kismet = ReplayOptions {
        level: ReplayLevel::Converted {
            dir: dir.clone(),
            map: map.clone(),
            kismet: true,
        },
        ..ReplayOptions::default()
    };
    let e = replay(&uneven, &with_kismet).unwrap_err();
    assert!(e.to_string().contains("cannot run the map's Kismet"), "{e}");

    // Other fixed rates (a benchmark recording at 30 or 144 frames per
    // second): the fixed replay runs the level's game at that rate, the
    // per-sample replay steps with the frame lengths the sample times give.
    // Equal lengths, so the same run.
    for rate in [30.0_f64, 144.0] {
        let loaded = Game::load_level(&dir, &map).unwrap();
        let scene = loaded.scene_map().cloned().unwrap();
        let mut g = Game::from_loaded_map(scene, PlayerParams::asamu_original(), rate)
            .unwrap()
            .with_movement_model(MovementModelKind::Ue3Pawn);
        g.start();
        g.start_recording();
        let mut died = false;
        for i in inputs(150) {
            let r = g.tick(&i).unwrap();
            died |= r.died.is_some() || r.respawned;
        }
        let ours = g.stop_recording().unwrap();
        assert_eq!(ours.meta.tick_rate, Some(rate as f32));
        let level = ReplayLevel::Converted {
            dir: dir.clone(),
            map: map.clone(),
            kismet: false,
        };
        let fixed = replay(
            &ours,
            &ReplayOptions {
                level: level.clone(),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(fixed.stepping, Stepping::Fixed(rate));
        let d = compare(&ours, &fixed.trace);
        assert!(d.is_exact(), "{rate} Hz fixed: {d:?}");
        let per_sample = replay(
            &ours,
            &ReplayOptions {
                level,
                variable_dt: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(per_sample.stepping, Stepping::PerSample);
        assert!(per_sample.variable.unwrap().lengths.uniform());
        if died {
            eprintln!(
                "the run dies on {map} at {rate} Hz: equality with the fixed tick not checked"
            );
        } else {
            let d = compare(&fixed.trace, &per_sample.trace);
            assert!(d.is_exact(), "{rate} Hz per-sample: {d:?}");
        }
    }
}
