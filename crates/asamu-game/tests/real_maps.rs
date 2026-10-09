//! Every converted map: the player spawns at its `PlayerStart`, comes to
//! rest standing on solid geometry, and walking steps fast.
//!
//! Gated: runs only when `ASAMU_CONVERTED_DIR` points at the output of
//! `asamu-import --out <dir> levels` and `meshes --collision` (user-local
//! data derived from the user's own install). Converts nothing; CI skips.

use std::path::PathBuf;
use std::time::Instant;

use asamu_game::Game;
use asamu_player::world::{CollisionShape, CollisionWorld};
use asamu_player::{InputFrame, PlayerParams};
use asamu_world::scene;
use glam::Vec3;

fn converted_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    dir.join("levels").is_dir().then_some(dir)
}

#[test]
fn every_map_spawns_the_player_grounded_on_solid_geometry() {
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let mut names: Vec<String> = std::fs::read_dir(dir.join("levels"))
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".scene.json").map(str::to_owned))
        .collect();
    names.sort();
    let mut played = 0;
    for name in names {
        let map = scene::load_map_from_dir(&dir, &name).unwrap();
        if map.actors.player_start().is_none() {
            eprintln!("{name}: no PlayerStart (a streamed sub-level)");
            continue;
        }
        let mut g = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        g.start();
        // Settle: the pawn drops from the start's height and lands.
        let mut settled = None;
        for i in 0..240 {
            let r = g.tick(&InputFrame::default()).unwrap();
            assert!(r.died.is_none(), "{name}: died while settling at tick {i}");
            if g.player().grounded && g.player().velocity.length() < 1.0 && i > 10 {
                settled = Some(i);
                break;
            }
        }
        let p = *g.player();
        assert!(settled.is_some(), "{name}: not at rest after 4 s: {p:?}");
        // Standing on solid geometry: the floor is within the walking hover
        // band below the cylinder (NATIVE_PHYSICS.md 3.x: 1.9 – 2.4 uu).
        let shape = CollisionShape {
            radius: 21.0,
            half_height: 44.0,
        };
        let floor = g
            .world()
            .sweep_capsule(p.position, p.position - Vec3::Z * 10.0, shape)
            .unwrap_or_else(|| panic!("{name}: nothing below the player at {}", p.position));
        assert!(floor.distance < 3.0, "{name}: floor {floor:?}");
        assert!(
            !g.world().overlaps(p.position, shape),
            "{name}: spawned inside geometry"
        );
        // Walk forward for 10 s with sprint; time the ticks.
        let mut times = Vec::new();
        for i in 0..600 {
            let input = InputFrame {
                move_forward: 1.0,
                sprint_held: true,
                jump_pressed: i % 120 == 60,
                jump_held: i % 120 >= 60 && i % 120 < 70,
                ..InputFrame::default()
            };
            let t = Instant::now();
            g.tick(&input).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1e6);
            assert!(g.player().is_finite(), "{name}");
        }
        times.sort_by(f64::total_cmp);
        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let max = times[times.len() - 1];
        eprintln!(
            "{name:<18} settled after {:>3} ticks at {:?}; walk: avg {avg:>6.1} us, max {max:>7.1} us per tick, {} respawns",
            settled.unwrap_or(0),
            p.position,
            g.respawn_count()
        );
        if !cfg!(debug_assertions) {
            assert!(avg < 500.0, "{name}: {avg} us per tick");
        }
        played += 1;
    }
    assert!(played >= 10, "{played}");
}

#[test]
fn real_maps_replay_deterministically() {
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    for name in ["AG-Workshop", "AG-IceCave"] {
        let Ok(map) = scene::load_map_from_dir(&dir, name) else {
            continue;
        };
        let run = |map: scene::LoadedMap| {
            let mut g = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0)
                .expect("game starts");
            g.set_falling_rocks_active(true);
            g.start();
            g.start_recording();
            for i in 0..480 {
                let input = InputFrame {
                    move_forward: 1.0,
                    sprint_held: i > 120,
                    jump_pressed: i % 100 == 50,
                    jump_held: i % 100 >= 50 && i % 100 < 60,
                    look_yaw_delta: if i % 80 == 0 { 0.4 } else { 0.0 },
                    ..InputFrame::default()
                };
                g.tick(&input).expect("playing");
            }
            g.stop_recording()
                .expect("recording")
                .to_jsonl_string()
                .expect("trace")
        };
        let a = run(map.clone());
        let b = run(map);
        assert_eq!(a, b, "{name}");
        eprintln!("{name}: {} trace bytes, identical", a.len());
    }
}

#[test]
fn kill_volumes_and_checkpoints_follow_the_spec() {
    // On every AG map: touching the highest-index checkpoint makes it the
    // latest (and saves), a lower one then changes nothing (A-CP-3); every
    // enabled kill volume starts the death sequence, and the reset follows
    // the 0.3 s timer (strict `count > rate`, G-TM-4) and teleports to the
    // latest checkpoint's spawn with its yaw (A-DT-2, A-CP-4).
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let fade = {
        let (mut c, mut k) = (0.0f32, 0u64);
        loop {
            k += 1;
            c += 1.0 / 60.0;
            if c > asamu_game::DEATH_FADE_DOWN_TIME {
                break k;
            }
        }
    };
    for name in [
        "AG-ParadiseCave",
        "AG-BeautifulCity",
        "AG-Darkcave",
        "AG-StarHaven",
        "AG-IceCave",
    ] {
        let Ok(map) = scene::load_map_from_dir(&dir, name) else {
            continue;
        };
        let mut g = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).unwrap();
        g.start();
        for _ in 0..30 {
            g.tick(&InputFrame::default()).unwrap();
        }
        let actors = g.scene_map().unwrap().actors.clone();
        let mut order: Vec<usize> = (0..actors.checkpoints.len())
            .filter(|&i| {
                actors.checkpoints[i].enabled && !actors.checkpoints[i].triggered_from_kismet
            })
            .collect();
        order.sort_by_key(|&i| std::cmp::Reverse(actors.checkpoints[i].index));
        let (hi, lo) = (order[0], order[1]);
        let hi_def = &actors.checkpoints[hi];
        g.player_mut().position = hi_def.location;
        let r = g.tick(&InputFrame::default()).unwrap();
        assert!(
            r.world.contains(&asamu_world::WorldEvent::CheckpointSaved {
                index: hi_def.index
            }),
            "{name}"
        );
        assert_eq!(g.active_checkpoint(), Some(hi_def.id), "{name}");
        g.player_mut().position = actors.checkpoints[lo].location;
        let r = g.tick(&InputFrame::default()).unwrap();
        assert!(
            r.world
                .iter()
                .any(|e| matches!(e, asamu_world::WorldEvent::CheckpointActivated { .. })),
            "{name}"
        );
        assert!(
            !r.world
                .iter()
                .any(|e| matches!(e, asamu_world::WorldEvent::CheckpointSaved { .. })),
            "{name}"
        );
        assert_eq!(g.active_checkpoint(), Some(hi_def.id), "{name}");
        let (spawn, spawn_rot) = g.scene_respawn_point().unwrap();
        let mut killed = 0;
        for v in actors
            .volumes
            .iter()
            .filter(|v| v.enabled && v.kind != asamu_world::gameplay::VolumeKind::TriggerVolume)
        {
            // A point inside: the middle of the first hull's bounds, pulled
            // inside every plane if needed.
            let hull = &v.hulls[0];
            let mut p = ((hull.min + hull.max) * 0.5).as_dvec3();
            for _ in 0..64 {
                let mut moved = false;
                for (n, w) in &hull.planes {
                    let out = n.dot(p) - w;
                    if out > -1.0 {
                        p -= *n * (out + 1.0);
                        moved = true;
                    }
                }
                if !moved {
                    break;
                }
            }
            g.player_mut().position = p.as_vec3();
            g.player_mut().velocity = Vec3::ZERO;
            let r = g.tick(&InputFrame::default()).unwrap();
            assert!(r.died.is_some(), "{name}: {} did not kill", v.name);
            let mut k = 0;
            loop {
                k += 1;
                let r = g.tick(&InputFrame::default()).unwrap();
                if r.respawned {
                    break;
                }
                assert!(k < 100, "{name}: no respawn");
            }
            assert_eq!(k, fade, "{name}: {}", v.name);
            let at = g.player().position;
            assert!(
                (at.truncate() - spawn.truncate()).length() < 1.0 && (at.z - spawn.z).abs() < 30.0,
                "{name}: respawned at {at}, spawn {spawn}"
            );
            // Same direction (the simulation keeps yaw in (-π, π]).
            let yaw = asamu_world::rotation::units_to_radians(spawn_rot[1]);
            let turn = (g.player().yaw - yaw).rem_euclid(core::f32::consts::TAU);
            assert!(
                turn < 1e-4 || core::f32::consts::TAU - turn < 1e-4,
                "{name}: yaw {} vs spawn {yaw}",
                g.player().yaw
            );
            // Let the player land before the next volume.
            for _ in 0..40 {
                g.tick(&InputFrame::default()).unwrap();
            }
            killed += 1;
        }
        eprintln!(
            "{name}: {killed} kill volumes kill; respawn after {fade} ticks at the latest checkpoint"
        );
        assert!(killed > 0, "{name}");
    }
}
