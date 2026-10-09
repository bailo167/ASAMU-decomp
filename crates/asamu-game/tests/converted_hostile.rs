//! Converted levels under hostile or unusual data (synthetic only): mutated
//! scene values must never panic or produce non-finite player state, and a
//! respawn point inside a kill zone kills again (A-DT-1).

use asamu_game::{DEATH_FADE_DOWN_TIME, Game};
use asamu_player::{InputFrame, PlayerParams};
use asamu_world::WorldEvent;
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, Value, box_mesh, json};
use asamu_world::gameplay::DeathCause;
use asamu_world::scene::{self, DataSource, LoadOptions, MemorySource};
use glam::Vec3;

const MAP: &str = "AG-Hostile";

/// Floor, start, checkpoint with a spawn marker, kill zone, trigger, a
/// crystal, a falling rock and a blocking crate.
fn fixture() -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -50_000.0);
    s.set_bsp(
        vec![
            [-4000.0, -4000.0, 0.0],
            [4000.0, -4000.0, 0.0],
            [4000.0, 4000.0, 0.0],
            [-4000.0, 4000.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    let marker = s.marker(Place {
        location: Vec3::new(1000.0, 0.0, 0.0),
        rotation: [0, 16_384, 0],
        scale: Vec3::ONE,
    });
    let path = s.path(marker);
    s.checkpoint(
        Place::at(Vec3::new(300.0, 0.0, 40.0)),
        200.0,
        60.0,
        0,
        json!({"spawnPointActor": path, "spawnPointOffset": {"X": 10.0, "Y": 0.0, "Z": 50.0}}),
    );
    s.kill_zone(
        Vec3::new(-500.0, 600.0, -100.0),
        Vec3::new(500.0, 900.0, 200.0),
    );
    s.trigger(Vec3::new(-600.0, 0.0, 40.0), 100.0, 50.0);
    s.mesh_actor(
        "asamu.ASAMURechargeCrystal",
        "recharge_crystal",
        "Pkg.Meshes.Box",
        Place::at(Vec3::new(0.0, -1500.0, 500.0)),
        "COLLIDE_BlockAll",
        None,
        json!({"RechargeDelay": 4.0}),
    );
    s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Pkg.Meshes.Box",
        Place::at(Vec3::new(1500.0, 1500.0, 800.0)),
        "COLLIDE_CustomDefault",
        None,
        json!({"fallDistance": 500.0, "fallingLowerRate": 400.0, "fallingHigherRate": 400.0}),
    );
    s.static_mesh("Pkg.Meshes.Box", Place::at(Vec3::new(400.0, 0.0, 0.0)));
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Pkg.Meshes.Box", MAP, v, t);
    meshes.write(&mut src, true);
    src
}

/// JSON pointer paths of every number in `v`.
fn number_paths(v: &Value, at: String, out: &mut Vec<String>) {
    match v {
        Value::Number(_) => out.push(at),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                number_paths(x, format!("{at}/{i}"), out);
            }
        }
        Value::Object(o) => {
            for (k, x) in o {
                number_paths(
                    x,
                    format!("{at}/{}", k.replace('~', "~0").replace('/', "~1")),
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Loads, starts and plays a few ticks; must never panic, and the player
/// stays finite whenever the game starts.
fn exercise(src: &MemorySource) -> bool {
    let Ok(map) = scene::load_map(src, MAP, &LoadOptions::default()) else {
        return false;
    };
    let Ok(mut g) = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0) else {
        return false;
    };
    g.set_falling_rocks_active(true);
    g.start();
    for i in 0..12 {
        let input = InputFrame {
            move_forward: 1.0,
            grapple_held: i % 4 < 2,
            jump_pressed: i == 3,
            ..InputFrame::default()
        };
        g.tick(&input).expect("playing");
        assert!(g.player().is_finite(), "{:?}", g.player());
    }
    g.kill_player();
    for _ in 0..25 {
        g.tick(&InputFrame::default()).expect("playing");
        assert!(g.player().is_finite(), "{:?}", g.player());
    }
    true
}

#[test]
fn extreme_scene_values_never_panic_or_go_non_finite() {
    let base = fixture();
    let path = format!("levels/{MAP}.scene.json");
    let text = String::from_utf8(base.read(&path, u64::MAX).expect("scene")).expect("utf-8");
    let scene_json: Value = text.parse().expect("json");
    let mut paths = Vec::new();
    number_paths(&scene_json["actors"], "/actors".to_owned(), &mut paths);
    assert!(paths.len() > 300, "{}", paths.len());
    // 1e39 and -1e39 overflow to ±inf as f32; the integers probe slots,
    // indices and rotations.
    let extremes = [
        json!(1.0e39),
        json!(-1.0e39),
        json!(3.0e38),
        json!(0),
        json!(-1),
        json!(65_536),
        json!(4_294_967_296u64),
        json!(-2_147_483_648i64),
    ];
    let (mut runs, mut started) = (0, 0);
    for (k, p) in paths.iter().enumerate() {
        let value = &extremes[k % extremes.len()];
        let mut mutated = scene_json.clone();
        if let Some(slot) = mutated.pointer_mut(p) {
            *slot = value.clone();
        }
        let mut src = base.clone();
        src.insert(path.clone(), mutated.to_string().into_bytes());
        runs += 1;
        started += usize::from(exercise(&src));
    }
    eprintln!("{runs} mutated scenes, {started} started a game");
    assert!(runs > 300 && started > runs / 2, "{started}/{runs}");
    // Duplicate slots (actor ids) everywhere.
    let mut dup = scene_json.clone();
    if let Some(actors) = dup["actors"].as_array_mut() {
        for a in actors.iter_mut() {
            a["slot"] = json!(1);
        }
    }
    let mut src = base.clone();
    src.insert(path, dup.to_string().into_bytes());
    let _ = exercise(&src);
}

fn fade_ticks() -> u64 {
    let (mut c, mut k) = (0.0f32, 0);
    loop {
        k += 1;
        c += 1.0 / 60.0;
        if c > DEATH_FADE_DOWN_TIME {
            return k;
        }
    }
}

#[test]
fn a_respawn_point_inside_a_kill_zone_kills_again() {
    // The checkpoint's spawn point lies inside a second kill zone.
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -50_000.0);
    s.set_bsp(
        vec![
            [-4000.0, -4000.0, 0.0],
            [4000.0, -4000.0, 0.0],
            [4000.0, 4000.0, 0.0],
            [-4000.0, 4000.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.checkpoint(
        Place::at(Vec3::new(2000.0, 0.0, 40.0)),
        100.0,
        60.0,
        0,
        json!({"bRotatePlayerToSpawnPointRot": false}),
    );
    let first = s.kill_zone(
        Vec3::new(-300.0, 500.0, -50.0),
        Vec3::new(300.0, 900.0, 300.0),
    );
    let second = s.kill_zone(
        Vec3::new(1800.0, -200.0, -50.0),
        Vec3::new(2200.0, 200.0, 300.0),
    );
    s.write(&mut src);
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).expect("map");
    let mut g = Game::from_loaded_map(map, PlayerParams::asamu_original(), 60.0).expect("game");
    g.start();
    for _ in 0..30 {
        g.tick(&InputFrame::default());
    }
    g.player_mut().position = Vec3::new(0.0, 700.0, 100.0);
    let r = g.tick(&InputFrame::default()).expect("tick");
    assert_eq!(r.died, Some(DeathCause::KillZone));
    assert!(r.world.contains(&WorldEvent::Touch {
        id: scene::actor_id(0, first).expect("id")
    }));
    let mut k = 0;
    let respawn = loop {
        k += 1;
        let r = g.tick(&InputFrame::default()).expect("tick");
        if r.respawned {
            break r;
        }
        assert!(k < 100);
    };
    assert_eq!(k, fade_ticks());
    // Teleported into the second kill zone: touched there, dying again.
    assert!((g.player().position.truncate() - glam::Vec2::new(2000.0, 0.0)).length() < 1.0);
    assert!(respawn.world.contains(&WorldEvent::Touch {
        id: scene::actor_id(0, second).expect("id")
    }));
    assert_eq!(respawn.died, Some(DeathCause::KillZone));
    assert!(g.is_dying());
    // The next reset lands on the same spot, which is still touched: no new
    // touch, no new death (touches begin once).
    let mut k = 0;
    let again = loop {
        k += 1;
        let r = g.tick(&InputFrame::default()).expect("tick");
        if r.respawned {
            break r;
        }
        assert!(k < 100);
    };
    assert_eq!(again.died, None);
    assert!(!g.is_dying());
    assert_eq!(g.respawn_count(), 2);
}
