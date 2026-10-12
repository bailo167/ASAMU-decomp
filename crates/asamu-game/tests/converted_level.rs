//! Converted-level gameplay on synthetic scenes (no game data): spawning on
//! triangle collision, the death sequence, checkpoints, KillZ, touches,
//! dynamic kill zones, level streaming, falling rocks, crystals and the
//! grapple on triangle geometry, level-start abilities and determinism.

use asamu_game::{DEATH_FADE_DOWN_TIME, Game, TickReport};
use asamu_player::{CollisionWorld, InputFrame, PlayerParams};
use asamu_world::WorldEvent;
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, json};
use asamu_world::gameplay::DeathCause;
use asamu_world::scene::{self, LoadOptions, MemorySource};
use glam::Vec3;

/// Actor id of `slot` in the persistent level.
fn world_event_id(level: u8, slot: usize) -> u32 {
    scene::actor_id(level, slot).expect("slot fits")
}

const DT: f32 = 1.0 / 60.0;

fn game(src: &MemorySource, map: &str) -> Game {
    let loaded = scene::load_map(src, map, &LoadOptions::default()).expect("fixture loads");
    // Only the original's own warning (rotate to a missing spawn actor), the
    // missing mesh manifest of mesh-less fixtures, and the loader's notice
    // that a mesh manifest of version 1 (which `MeshFixtures::write` writes:
    // a conversion from before the simple collision) makes the pawn collide
    // with every mesh's triangles. The loader has said that since it reads
    // the meshes' simple collision; the worlds of these tests are unchanged.
    assert!(
        loaded
            .warnings
            .iter()
            .all(|w| w.contains("without a spawn point actor")
                || w.contains("no static-mesh collision")
                || w.contains("has no simple-collision data")),
        "{:?}",
        loaded.warnings
    );
    let mut g =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), 60.0).expect("game starts");
    g.start();
    g
}

fn idle() -> InputFrame {
    InputFrame::default()
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

/// A square BSP floor `[-half, half]²` at height `z`.
fn floor(s: &mut SceneFixture, half: f32, z: f32) {
    s.set_bsp(
        vec![
            [-half, -half, z],
            [half, -half, z],
            [half, half, z],
            [-half, half, z],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
}

fn run_until(
    g: &mut Game,
    input: InputFrame,
    max: usize,
    mut done: impl FnMut(&TickReport, &Game) -> bool,
) -> Option<TickReport> {
    for _ in 0..max {
        let r = g.tick(&input).expect("playing");
        if done(&r, g) {
            return Some(r);
        }
    }
    None
}

/// Ticks of the death fade at 60 Hz: the one-shot timer adds `dt` per tick
/// from the tick after the death and fires when the sum exceeds 0.3.
fn fade_ticks() -> u64 {
    let mut count = 0.0f32;
    let mut k = 0;
    loop {
        k += 1;
        count += DT;
        if count > DEATH_FADE_DOWN_TIME {
            return k;
        }
    }
}

#[test]
fn spawns_at_the_player_start_lands_and_walks_on_triangles() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Flat", -10_000.0);
    floor(&mut s, 5000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.write(&mut src);
    let mut g = game(&src, "AG-Flat");
    // A-CP-5: the pawn's centre at the start, in walking physics and
    // without a base, as the original's possession leaves a new pawn
    // (PARITY_FINDINGS P10; six recorded level starts).
    let start = Vec3::new(0.0, 0.0, 80.0);
    assert_eq!(g.player().position, start);
    assert!(g.player().grounded && !g.player().pawn.based && !g.player().pawn.flying);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    // The first tick is the walking floor check (NATIVE_PHYSICS.md 3.3): its
    // 28 uu trace does not reach a floor 36 uu below the box, so the pawn
    // starts to fall in that tick, where it is and with zero velocity (a
    // pawn that did not move carries no time into the fall). That is the
    // recorded start of AG-BeautifulCity and AG-Darkcave: free fall from the
    // start's height, beginning when the first tick ends.
    let r = g.tick(&idle()).unwrap();
    assert!(!g.player().grounded && r.events.landing.is_none());
    assert_eq!(g.player().position, start);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    // It lands in the 16th falling tick (derived below): tick 17.
    let mut landed = None;
    for tick in 2..=60 {
        let r = g.tick(&idle()).unwrap();
        if g.player().grounded {
            assert!(r.events.landing.is_some(), "the landing is a landing");
            landed = Some(tick);
            break;
        }
    }
    assert_eq!(landed, Some(17));
    for _ in 0..30 {
        g.tick(&idle()).unwrap();
    }
    // Where the drop leaves the pawn (NATIVE_PHYSICS.md 3.3, 4.4, 10.2). Its
    // bottom starts 36 uu above the floor and falls from rest at 60 Hz:
    // falling step k moves it (2k − 1)·520/3600 uu, n steps n²·0.14444 uu,
    // so it meets the floor in step 16 (15 steps: 32.5 uu), a move of
    // 31 × 0.14444 = 4.478 uu. The original reports that hit a tenth of the
    // move short (the pull-back of a trace between 1 and 10 uu), so the pawn
    // lands 0.4478 uu above the floor. The forced walking floor check then
    // reports the floor 1.0 uu nearer, clamped to a distance of 0, and moves
    // the pawn up by the whole 2.15: 2.5978 uu, where it stays while it
    // stands still. (Before the port applied the pull-back: 2.15.)
    let hover = g.player().position.z - 44.0;
    let landing_move = 31.0 * 520.0 / 3600.0;
    assert!(
        (hover - (2.15 + 0.1 * landing_move)).abs() < 1e-3,
        "hover {hover}"
    );
    for _ in 0..60 {
        g.tick(&forward()).unwrap();
    }
    assert!((g.player().horizontal_speed() - 440.0).abs() < 0.05);
    assert!(g.player().grounded);
    // The world reports the BSP as world geometry.
    let hit = g
        .world()
        .raycast(g.player().position, Vec3::NEG_Z, 500.0)
        .unwrap();
    assert_eq!(
        hit.surface.class,
        asamu_player::world::ActorClass::WorldGeometry
    );
    assert!(g.level().name.contains("converted"));
}

#[test]
fn a_start_within_reach_of_its_floor_stands_without_a_landing() {
    // The pawn starts walking and without a base (A-CP-5), so its first tick
    // is the walking floor check (NATIVE_PHYSICS.md 3.3): a 28 uu trace of
    // the box straight down, whose hit on BSP is reported 1.0 uu nearer than
    // it is (a tenth of the trace, at most 1.0 uu, 10.2). A floor it finds
    // is a new base, and the pawn is put at the reported distance 2.15: its
    // bottom a true 3.15 uu above the floor, from above or from below, in
    // that one tick and without a landing. Recorded: the AG-Workshop pawn
    // stands at 72 + 44 + 3.15 = 119.15 and the AG-ParadiseCave pawn at its
    // standing height, 19.7 uu below its start point, from their first
    // recorded frames, with `AirControl` still 0.30.
    let stands = |start_z: f32| {
        let mut src = MemorySource::new();
        let mut s = SceneFixture::new("AG-Reach", -10_000.0);
        floor(&mut s, 5000.0, 0.0);
        s.player_start(Vec3::new(0.0, 0.0, start_z), 0);
        s.write(&mut src);
        let mut g = game(&src, "AG-Reach");
        assert_eq!(g.player().position.z, start_z);
        assert!(g.player().grounded && !g.player().pawn.based);
        let r = g.tick(&idle()).unwrap();
        assert!(r.events.landing.is_none(), "from {start_z}: no landing");
        assert!(g.player().grounded && g.player().pawn.based);
        assert_eq!(g.player().velocity, Vec3::ZERO);
        assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
        let z = g.player().position.z;
        // It stays there while it stands.
        for _ in 0..30 {
            g.tick(&idle()).unwrap();
        }
        assert_eq!(g.player().position.z, z, "from {start_z}");
        z
    };
    // 20 uu above the floor (reported 19): moved down to 44 + 3.15.
    let z = stands(44.0 + 20.0);
    assert!((z - 47.15).abs() < 1e-4, "{z}");
    // 27.5 uu above (reported 26.5), still inside the 28 uu trace.
    let z = stands(44.0 + 27.5);
    assert!((z - 47.15).abs() < 1e-4, "{z}");
    // 2.32 uu above (reported 1.32, as where the placement search puts the
    // AG-Workshop pawn): moved up to the same height.
    let z = stands(44.0 + 2.32);
    assert!((z - 47.15).abs() < 1e-4, "{z}");
}

/// A platform at the respawn point, nothing under the start but a kill zone.
/// The spawn point is 40 uu above the platform, so the pawn's box (half
/// height 44) is 4 uu inside the floor there, as the spawn points of the
/// shipped checkpoints that the original lifts are (3 to 6 uu,
/// PARITY_FINDINGS P10).
fn death_fixture() -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Death", -50_000.0);
    s.set_bsp(
        vec![
            [500.0, -500.0, 0.0],
            [1500.0, -500.0, 0.0],
            [1500.0, 500.0, 0.0],
            [500.0, 500.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(-2000.0, 0.0, 500.0), 0);
    s.kill_zone(
        Vec3::new(-5000.0, -5000.0, -3000.0),
        Vec3::new(5000.0, 5000.0, -1000.0),
    );
    let marker = s.marker(Place {
        location: Vec3::new(1000.0, 0.0, 40.0),
        rotation: [0, 16_384, 0],
        scale: Vec3::ONE,
    });
    let path = s.path(marker);
    s.checkpoint(
        Place::at(Vec3::new(1000.0, 0.0, 40.0)),
        200.0,
        60.0,
        0,
        json!({"spawnPointActor": path, "spawnPointOffset": {"X": 0.0, "Y": 0.0, "Z": 0.0}}),
    );
    s.write(&mut src);
    src
}

#[test]
fn kill_zone_runs_the_death_sequence_and_respawns_at_the_checkpoint() {
    let src = death_fixture();
    let mut g = game(&src, "AG-Death");
    let died =
        run_until(&mut g, idle(), 600, |r, _| r.died.is_some()).expect("fell into the kill zone");
    assert_eq!(died.died, Some(DeathCause::KillZone));
    assert!(died.world.iter().any(|e| matches!(
        e,
        WorldEvent::PlayerDied {
            cause: DeathCause::KillZone
        }
    )));
    assert!(g.is_dying());
    let death_tick = died.tick;
    // The pawn keeps simulating (falling) during the fade.
    let z_at_death = g.player().position.z;
    let respawn = run_until(&mut g, idle(), 60, |r, _| r.respawned).unwrap();
    assert_eq!(
        respawn.tick - death_tick,
        fade_ticks(),
        "0.3 s one-shot timer"
    );
    assert!(
        respawn
            .world
            .iter()
            .any(|e| e == WorldEvent::PlayerRespawned)
    );
    assert!(!g.is_dying());
    assert_eq!(g.respawn_count(), 1);
    // Teleported to the marker, with the marker's yaw. The box does not fit
    // there (its bottom is 4 uu inside the platform), so the engine's
    // placement search stands it over the floor (`UWorld::FindSpot`, read in
    // the user's executable; `find_spot` in `converted.rs`): a 1 uu slice at
    // the centre (bottom at z = 39) is traced down 88 uu, meets the BSP
    // after 39 uu, reported 1.0 uu short (a tenth of the trace, at most 1.0
    // on BSP): 38 uu. The centre goes to 40 − (2 × 38/88 − 1) × 44 + 1 = 47:
    // the bottom 3.0 uu above the floor. The tick's physics then ran from
    // there with zero velocity: one falling step of 520/3600 uu.
    let p = *g.player();
    assert!(p.position.z > z_at_death);
    assert!(
        (p.position.truncate() - glam::Vec2::new(1000.0, 0.0)).length() < 1e-3,
        "{}",
        p.position
    );
    assert!(
        (p.yaw - core::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "yaw {}",
        p.yaw
    );
    let step = 520.0 / 3600.0;
    assert!(
        (p.position.z - (47.0 - step)).abs() < 1e-3,
        "{}",
        p.position
    );
    // A-DT-2: the physics mode is kept — it died falling, so it falls the
    // 3.0 uu onto the platform and lands: after 4 steps it is 16 × 0.1444 =
    // 2.311 uu down, the fifth (9 × 0.1444 = 1.3 uu) meets the floor and is
    // reported a tenth of itself short, and the walking floor check of the
    // landing tick moves the pawn up by 2.15 (the floor is reported 1.0 uu
    // nearer than 0.13, that is at 0). It rests 0.13 + 2.15 uu above the
    // floor, 0.72 below where it was put: what three recorded respawns show
    // (0.71 to 0.73, PARITY_FINDINGS P10).
    let landed = run_until(&mut g, idle(), 60, |_, g| g.player().grounded).unwrap();
    assert_eq!(landed.tick - respawn.tick, 4, "lands in its fifth step");
    assert!(
        (g.player().position.z - (44.0 + 2.15 + 0.1 * 9.0 * step)).abs() < 1e-3,
        "{}",
        g.player().position
    );
    // Leaving the kill zone reported an UnTouch at the teleport.
    assert!(
        respawn
            .world
            .iter()
            .any(|e| matches!(e, WorldEvent::UnTouch { .. }))
    );
}

#[test]
fn second_death_within_the_fade_restarts_the_timer() {
    let src = death_fixture();
    let mut g = game(&src, "AG-Death");
    run_until(&mut g, idle(), 600, |r, _| r.died.is_some()).unwrap();
    for _ in 0..10 {
        assert!(!g.tick(&idle()).unwrap().respawned);
    }
    g.kill_player();
    let r = g.tick(&idle()).unwrap();
    assert!(r.world.iter().any(|e| matches!(
        e,
        WorldEvent::PlayerDied {
            cause: DeathCause::Scripted
        }
    )));
    let mut n = 1;
    while !g.tick(&idle()).unwrap().respawned {
        n += 1;
        assert!(n < 100);
    }
    // Restarted at the kill_player call: the full fade counted again.
    assert_eq!(n + 1, fade_ticks());
}

#[test]
fn checkpoints_activate_on_touch_and_the_latest_one_is_the_respawn() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Checkpoints", -50_000.0);
    floor(&mut s, 10_000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 50.0), 0);
    let a = s.checkpoint(
        Place::at(Vec3::new(0.0, 0.0, 40.0)),
        200.0,
        60.0,
        0,
        json!({}),
    );
    let b = s.checkpoint(
        Place::at(Vec3::new(1500.0, 0.0, 40.0)),
        200.0,
        60.0,
        1,
        json!({}),
    );
    // Index 2 can only be triggered from Kismet; index 3 starts disabled.
    let c = s.checkpoint(
        Place::at(Vec3::new(3000.0, 0.0, 40.0)),
        200.0,
        60.0,
        2,
        json!({"bTriggeredFromKismet": true}),
    );
    let d = s.checkpoint(
        Place::at(Vec3::new(4500.0, 0.0, 40.0)),
        200.0,
        60.0,
        3,
        json!({"bEnabled": false}),
    );
    s.write(&mut src);
    let id = |slot: usize| world_event_id(0, slot);
    let mut g = game(&src, "AG-Checkpoints");
    // Fresh start: nothing registered; the respawn is the first checkpoint.
    assert_eq!(g.active_checkpoint(), None);
    assert_eq!(
        g.scene_respawn_point().unwrap().0,
        Vec3::new(0.0, 0.0, 40.0)
    );
    let mut events = Vec::new();
    for _ in 0..(60 * 13) {
        let r = g.tick(&forward()).unwrap();
        events.extend(r.world.iter());
    }
    assert!(g.player().position.x > 5000.0);
    let activated: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            WorldEvent::CheckpointActivated { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(activated, vec![id(a), id(b)], "{events:?}");
    // Index 0 equals the lookup's index: activated, not saved; index 1 saved.
    let saved: Vec<i32> = events
        .iter()
        .filter_map(|e| match e {
            WorldEvent::CheckpointSaved { index } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(saved, vec![1]);
    assert_eq!(g.active_checkpoint(), Some(id(b)));
    // Kismet triggers the third; enabling the fourth needs a new touch.
    assert!(g.trigger_checkpoint(id(c)));
    assert!(g.set_checkpoint_enabled(id(d), true));
    let r = g.tick(&idle()).unwrap();
    assert!(r.world.contains(&WorldEvent::CheckpointSaved { index: 2 }));
    assert_eq!(r.checkpoint_activated, Some(id(c)));
    assert_eq!(g.active_checkpoint(), Some(id(c)));
    // An immediate respawn (no fade) goes to the latest checkpoint.
    g.respawn();
    assert!((g.player().position.truncate() - glam::Vec2::new(3000.0, 0.0)).length() < 1e-3);
    assert_eq!(g.player().velocity, Vec3::ZERO);
}

#[test]
fn kill_z_kills_once_per_descent() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-KillZ", -500.0);
    s.set_bsp(
        vec![
            [500.0, -500.0, 0.0],
            [1500.0, -500.0, 0.0],
            [1500.0, 500.0, 0.0],
            [500.0, 500.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(-2000.0, 0.0, 500.0), 0);
    s.checkpoint(
        Place::at(Vec3::new(1000.0, 0.0, 40.0)),
        200.0,
        60.0,
        0,
        json!({}),
    );
    s.write(&mut src);
    let mut g = game(&src, "AG-KillZ");
    let r = run_until(&mut g, idle(), 600, |r, _| r.died.is_some()).unwrap();
    assert_eq!(r.died, Some(DeathCause::KillZ));
    // Still below KillZ during the fade: no new death.
    let respawn = run_until(&mut g, idle(), 60, |r, _| {
        assert!(r.died.is_none() || r.respawned);
        r.respawned
    })
    .unwrap();
    assert_eq!(respawn.died, None);
    run_until(&mut g, idle(), 120, |_, g| g.player().grounded).unwrap();
    assert!(!g.is_dying());
}

#[test]
fn triggers_and_volumes_report_touches_and_dynamic_kill_zones_toggle() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Touch", -50_000.0);
    floor(&mut s, 10_000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 50.0), 0);
    let trig = s.trigger(Vec3::new(1000.0, 0.0, 40.0), 100.0, 60.0);
    let vol = s.trigger_volume(
        Vec3::new(2000.0, -300.0, 0.0),
        Vec3::new(2200.0, 300.0, 300.0),
    );
    let dkz = s.dynamic_kill_zone(
        Vec3::new(4000.0, -300.0, 0.0),
        Vec3::new(4200.0, 300.0, 300.0),
    );
    s.checkpoint(
        Place::at(Vec3::new(0.0, 0.0, 40.0)),
        100.0,
        60.0,
        0,
        json!({}),
    );
    s.write(&mut src);
    let id = |slot: usize| world_event_id(0, slot);
    let mut g = game(&src, "AG-Touch");
    assert!(g.set_volume_enabled(id(dkz), false));
    let mut events = Vec::new();
    for _ in 0..(60 * 11) {
        let r = g.tick(&forward()).unwrap();
        assert!(r.died.is_none(), "the dynamic kill zone is disabled");
        events.extend(r.world.iter());
    }
    let touches: Vec<WorldEvent> = events
        .iter()
        .copied()
        .filter(|e| matches!(e, WorldEvent::Touch { .. } | WorldEvent::UnTouch { .. }))
        .collect();
    assert_eq!(
        touches,
        vec![
            WorldEvent::Touch { id: id(trig) },
            WorldEvent::UnTouch { id: id(trig) },
            WorldEvent::Touch { id: id(vol) },
            WorldEvent::UnTouch { id: id(vol) },
        ]
    );
    // Back inside the dynamic kill zone, enabling it kills.
    g.player_mut().position = Vec3::new(4100.0, 0.0, 46.15);
    g.player_mut().velocity = Vec3::ZERO;
    let r = g.tick(&idle()).unwrap();
    assert!(r.died.is_none());
    assert!(g.set_volume_enabled(id(dkz), true));
    let r = g.tick(&idle()).unwrap();
    assert_eq!(r.died, Some(DeathCause::DynamicKillZone));
}

#[test]
fn streamed_sub_level_brings_its_collision() {
    let mut src = MemorySource::new();
    let mut main = SceneFixture::new("AG-Main", -50_000.0);
    main.player_start(Vec3::new(0.0, 0.0, 500.0), 0);
    main.stream("thecore", "LevelStreamingKismet");
    main.write(&mut src);
    let mut core = SceneFixture::new("TheCore", -50_000.0);
    floor(&mut core, 5000.0, 0.0);
    core.write(&mut src);
    let mut g = game(&src, "AG-Main");
    assert!(!g.is_level_streamed("TheCore"));
    for _ in 0..30 {
        g.tick(&idle()).unwrap();
    }
    assert!(!g.player().grounded, "no floor yet");
    // Kismet streams TheCore in while the player is still above the floor.
    g.player_mut().position = Vec3::new(0.0, 0.0, 500.0);
    g.player_mut().velocity = Vec3::ZERO;
    assert!(g.set_level_streamed("thecore", true));
    assert!(g.is_level_streamed("TheCore"));
    run_until(&mut g, idle(), 120, |_, g| g.player().grounded)
        .expect("lands on the streamed floor");
    // The fall from 500 ends with a move longer than 10 uu (about 970 uu/s),
    // so the landing is reported the capped 1.0 uu short of the floor
    // (NATIVE_PHYSICS.md 10.2); the walking floor check of the same tick
    // reports that as 0 and moves the pawn up 2.15: its centre is at
    // 44 + 1.0 + 2.15 = 47.15 (46.15 before the port applied the pull-back).
    assert!((g.player().position.z - 47.15).abs() < 0.5);
    assert!(!g.set_level_streamed("nope", true));
}

/// A crate floor, a recharge crystal in front of the player and a falling
/// rock to the side.
fn objects_fixture() -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-IceCave", -50_000.0);
    floor(&mut s, 10_000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 50.0), 0);
    s.checkpoint(
        Place::at(Vec3::new(0.0, 0.0, 40.0)),
        100.0,
        60.0,
        0,
        json!({}),
    );
    s.mesh_actor(
        "asamu.ASAMURechargeCrystal",
        "recharge_crystal",
        "Pkg.Crystal",
        Place::at(Vec3::new(1500.0, 0.0, 600.0)),
        "COLLIDE_BlockAll",
        None,
        json!({"RechargeDelay": 2.0}),
    );
    s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Pkg.Crystal",
        Place::at(Vec3::new(0.0, 1500.0, 3000.0)),
        "COLLIDE_CustomDefault",
        None,
        json!({"fallDistance": 2000.0, "fallingLowerRate": 600.0, "fallingHigherRate": 600.0}),
    );
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Pkg.Crystal", "AG-IceCave", v, t);
    meshes.write(&mut src, true);
    src
}

fn look_at(g: &Game, target: Vec3) -> InputFrame {
    let d = target - g.eye_position();
    let p = g.player();
    InputFrame {
        look_yaw_delta: d.y.atan2(d.x) - p.yaw,
        look_pitch_delta: d.z.atan2(d.truncate().length()) - p.pitch,
        ..InputFrame::default()
    }
}

#[test]
fn grapple_on_a_converted_crystal_refills_and_uncharges_it() {
    let src = objects_fixture();
    let mut g = game(&src, "AG-IceCave");
    // IceCave's level-start abilities: 3 grapples, boots on.
    assert_eq!(g.player().script.gun.max_grapples, 3);
    assert!(g.player().script.boots.enabled);
    for _ in 0..40 {
        g.tick(&idle()).unwrap();
    }
    let crystal = Vec3::new(1500.0, 0.0, 600.0);
    g.tick(&look_at(&g, crystal)).unwrap();
    g.tick(&idle()).unwrap();
    // The crosshair/fire trace sees the charged crystal.
    let aim = g
        .world()
        .raycast(g.eye_position(), crystal - g.eye_position(), 5000.0)
        .unwrap();
    assert!(matches!(
        aim.surface.class,
        asamu_player::world::ActorClass::RechargeCrystal { charged: true }
    ));
    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let r = g.tick(&fire).unwrap();
    assert_eq!(
        r.events.gun.fire,
        Some(asamu_player::FireOutcome::Attached),
        "{r:?}"
    );
    // Release-instant target: 0.05 s later it releases and uncharges.
    let mut uncharged = false;
    for _ in 0..20 {
        let r = g.tick(&fire).unwrap();
        uncharged |= r
            .world
            .iter()
            .any(|e| matches!(e, WorldEvent::CrystalUncharged { .. }));
    }
    assert!(uncharged);
    let aim = g
        .world()
        .raycast(g.eye_position(), crystal - g.eye_position(), 5000.0)
        .unwrap();
    assert!(matches!(
        aim.surface.class,
        asamu_player::world::ActorClass::RechargeCrystal { charged: false }
    ));
}

#[test]
fn falling_rocks_move_their_collision_when_activated() {
    let src = objects_fixture();
    let mut g = game(&src, "AG-IceCave");
    let probe = |g: &Game| {
        g.world()
            .raycast(Vec3::new(0.0, 1500.0, 5000.0), Vec3::NEG_Z, 10_000.0)
            .map(|h| h.position.z)
    };
    let top = probe(&g).unwrap();
    assert!((top - 3050.0).abs() < 1e-3);
    for _ in 0..30 {
        g.tick(&idle()).unwrap();
    }
    assert_eq!(probe(&g), Some(top), "inactive until Kismet activates it");
    g.set_falling_rocks_active(true);
    for _ in 0..60 {
        g.tick(&idle()).unwrap();
    }
    let lower = probe(&g).unwrap();
    assert!(lower < top - 50.0, "{lower}");
    // The collision world reports the rock's moving location (anchor follow).
    let rock_id = world_event_id(0, 4);
    let loc = g.world().actor_location(rock_id).unwrap();
    assert!((loc.z - (lower - 50.0)).abs() < 1e-3, "{loc} vs {lower}");
}

/// [`objects_fixture`] with a falling rock that spins: `bShouldRotate`, a
/// yaw rate of 9000 units/s reached at once (`RotationAccelRate` 100 with
/// the 0.017 s update: the whole difference in one step).
fn spinning_rock_fixture() -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-IceCave", -50_000.0);
    floor(&mut s, 10_000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 50.0), 0);
    s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Pkg.Rock",
        Place::at(Vec3::new(0.0, 1500.0, 3000.0)),
        "COLLIDE_CustomDefault",
        None,
        json!({
            "fallDistance": 20000.0, "fallingLowerRate": 300.0, "fallingHigherRate": 300.0,
            "bShouldRotate": true, "RotationAccelRate": 100.0,
            "rotationLowerRate": {"X": 0.0, "Y": 9000.0, "Z": 0.0},
            "rotationHigherRate": {"X": 0.0, "Y": 9000.0, "Z": 0.0},
        }),
    );
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Pkg.Rock", "AG-IceCave", v, t);
    meshes.write(&mut src, true);
    src
}

/// Where the point `p` that rides a base rigidly is after the base went
/// from the pose `from` to the pose `to` (location, rotation as pitch, yaw,
/// roll): its offset from the base's location keeps its coordinates in the
/// base's frame (GRAPPLE.md G-AT-8). The rows of the rotation matrix with
/// the sine table's flooring of each angle to 4 units; exact sines.
fn rigid(p: Vec3, from: (Vec3, [i32; 3]), to: (Vec3, [i32; 3])) -> Vec3 {
    use glam::DVec3;
    let rows = |rotation: [i32; 3]| {
        let sin_cos = |units: i32| {
            let index = (units as u32 >> 2) & 0x3FFF;
            (f64::from(index) * std::f64::consts::TAU / 16_384.0).sin_cos()
        };
        let (sp, cp) = sin_cos(rotation[0]);
        let (sy, cy) = sin_cos(rotation[1]);
        let (sr, cr) = sin_cos(rotation[2]);
        [
            DVec3::new(cp * cy, cp * sy, sp),
            DVec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
            DVec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
        ]
    };
    let offset = (p - from.0).as_dvec3();
    let (old, new) = (rows(from.1), rows(to.1));
    let local = DVec3::new(offset.dot(old[0]), offset.dot(old[1]), offset.dot(old[2]));
    (to.0.as_dvec3() + new[0] * local.x + new[1] * local.y + new[2] * local.z).as_vec3()
}

/// G-WO-3: the anchor follows a falling rock. While it is grappled the rock
/// brakes without turning, so the anchor goes down with it; after the
/// release it falls and spins again, and the helper, still based on it,
/// rides the spin too.
#[test]
fn the_anchor_on_a_falling_rock_rides_its_fall_and_its_spin() {
    let src = spinning_rock_fixture();
    let mut g = game(&src, "AG-IceCave");
    let rock_id = g.scene_map().unwrap().actors.rocks[0].id;
    let rock = |g: &Game| {
        let s = g.scene_runtime().unwrap().rocks[0];
        (s.location, s.rotation)
    };
    g.set_falling_rocks_active(true);
    for _ in 0..30 {
        g.tick(&idle()).unwrap();
    }
    let (at, rotation) = rock(&g);
    assert!(at.z < 2990.0, "the rock falls: {at}");
    assert_ne!(rotation, [0; 3], "the rock spins: {rotation:?}");
    // What an anchor rides: the collision world has the rock's pose.
    assert_eq!(g.world().actor_location(rock_id), Some(at));
    assert_eq!(g.world().actor_rotation(rock_id), Some(rotation));
    // Look a little below the rock's middle (it falls on while the view
    // turns) and fire.
    let target = at - Vec3::Z * 10.0;
    g.tick(&look_at(&g, target)).unwrap();
    let pose0 = rock(&g);
    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let r = g.tick(&fire).unwrap();
    let attached = r.events.gun.attached.expect("the grapple attaches");
    assert_eq!(attached.surface.actor, Some(rock_id));
    let anchor0 = attached.anchor;
    // Grappled: the rock brakes (`decelRate` 1000) and does not turn.
    let mut lowest = pose0.0.z;
    for tick in 0..25 {
        if tick > 0 {
            g.tick(&fire).unwrap();
        }
        let pose = rock(&g);
        assert_eq!(
            pose.1, pose0.1,
            "tick {tick}: a grappled rock does not turn"
        );
        assert!(pose.0.z <= lowest);
        lowest = pose.0.z;
        let anchor = g.player().script.gun.anchor().expect("attached");
        // The same rotation: the rock's translation (added tick by tick in
        // `f32`: steps of 2.4e-4 uu at these heights).
        let expected = anchor0 + (pose.0 - pose0.0);
        assert!(
            (anchor - expected).length() < 0.01,
            "tick {tick}: {anchor} vs {expected}"
        );
    }
    assert!(lowest < pose0.0.z - 20.0, "the rock went on down: {lowest}");
    // Released: the rock falls and spins again; the helper stays based on
    // it and rides both (cosmetic then, G-AT-8).
    g.tick(&idle()).unwrap();
    assert!(g.player().script.gun.anchor().is_none());
    let mut turned = false;
    for tick in 0..40 {
        g.tick(&idle()).unwrap();
        let pose = rock(&g);
        turned |= pose.1 != pose0.1;
        let helper = g.player().script.gun.follow.expect("still based").helper;
        let expected = rigid(anchor0, pose0, pose);
        // Carried once per tick in `f32` at coordinates of a few thousand
        // uu (steps of 2.4e-4 uu): well within 0.05 uu after 65 carries.
        assert!(
            (helper - expected).length() < 0.05,
            "tick {tick}: the helper is at {helper}, the rock took its place to {expected}"
        );
    }
    assert!(turned, "the released rock spins again");
}

#[test]
fn workshop_starts_in_story_mode() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Workshop", 1.0);
    floor(&mut s, 5000.0, 72.0);
    s.player_start(Vec3::new(0.0, 0.0, 152.0), 32_768);
    s.write(&mut src);
    let mut g = game(&src, "AG-Workshop");
    assert!(g.in_story_mode());
    assert_eq!(g.player().script.gun.max_grapples, 0);
    assert!((g.player().yaw.abs() - core::f32::consts::PI).abs() < 1e-3);
    for _ in 0..120 {
        g.tick(&forward()).unwrap();
    }
    // Story mode speed 264 (ABILITIES.md A1).
    assert!((g.player().horizontal_speed() - 264.0).abs() < 0.05);
}

#[test]
fn converted_games_are_deterministic() {
    let run = || {
        let src = objects_fixture();
        let mut g = game(&src, "AG-IceCave");
        g.set_falling_rocks_active(true);
        g.start_recording();
        for i in 0..400 {
            let mut input = forward();
            input.sprint_held = i > 100;
            input.jump_pressed = i % 90 == 0;
            input.look_yaw_delta = if i % 50 == 0 { 0.3 } else { 0.0 };
            g.tick(&input).unwrap();
        }
        let rocks = format!("{:?}", g.scene_runtime().unwrap().rocks);
        (
            g.stop_recording().unwrap().to_jsonl_string().unwrap(),
            rocks,
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn spawn_inside_geometry_is_lifted_out() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Lift", -50_000.0);
    floor(&mut s, 5000.0, 0.0);
    // The start's centre is 10 uu above the floor: overlapping.
    s.player_start(Vec3::new(0.0, 0.0, 10.0), 0);
    s.write(&mut src);
    let g = game(&src, "AG-Lift");
    // The spawn's placement search (`UWorld::FindSpot`; `find_spot` in
    // `converted.rs`): a 1 uu slice at the centre (bottom at z = 9) traced
    // down 88 uu meets the BSP after 9 uu, reported 1.0 uu short: 8. The
    // centre goes to 10 − (2 × 8/88 − 1) × 44 + 1 = 47, the bottom 3.0 uu
    // above the floor. (Our stand-in before the routine was read put the
    // pawn 0.05 uu above the floor.) The pawn is walking there, without a
    // base (A-CP-5).
    let mut g = g;
    let z = g.player().position.z;
    assert!((z - 47.0).abs() < 1e-3, "{z}");
    assert!(g.player().grounded && !g.player().pawn.based);
    assert!(
        !g.world()
            .overlaps_box(g.player().position, Vec3::new(21.0, 21.0, 44.0))
    );
    // Its first floor check reports the floor at 3.0 − 1.0 = 2.0 uu, takes
    // it as a new base and sets the reported distance to 2.15: 47.15.
    let r = g.tick(&idle()).unwrap();
    assert!(r.events.landing.is_none());
    let z = g.player().position.z;
    assert!((z - 47.15).abs() < 1e-4, "{z}");
    assert!(g.player().grounded && g.player().pawn.based);
}

#[test]
fn a_spawn_point_in_a_floor_sheet_gets_no_place() {
    // The placement search starts from a 1 uu slice at the wanted centre.
    // When that slice does not fit, four half-width slices around it are
    // tried at the same height; in the plane of a floor none fits, the
    // search finds no place and the teleport fails: the pawn stays where it
    // was (the original's reset then only logs). A respawn straight after
    // the start, with a checkpoint whose spawn point lies in the floor.
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Sheet", -50_000.0);
    floor(&mut s, 5000.0, 0.0);
    s.player_start(Vec3::new(0.0, 0.0, 400.0), 0);
    let marker = s.marker(Place::at(Vec3::new(1000.0, 0.0, 0.0)));
    let path = s.path(marker);
    s.checkpoint(
        Place::at(Vec3::new(3000.0, 0.0, 40.0)),
        200.0,
        60.0,
        0,
        json!({"spawnPointActor": path, "spawnPointOffset": {"X": 0.0, "Y": 0.0, "Z": 0.0}}),
    );
    s.write(&mut src);
    let mut g = game(&src, "AG-Sheet");
    g.tick(&idle()).unwrap();
    // The level's only checkpoint is where a respawn goes.
    assert_eq!(
        g.scene_respawn_point().map(|p| p.0),
        Some(Vec3::new(1000.0, 0.0, 0.0))
    );
    let before = g.player().position;
    g.respawn();
    assert_eq!(g.player().position, before, "the teleport failed");
    assert_eq!(
        g.player().velocity,
        Vec3::ZERO,
        "the reset's velocity write"
    );
}
