//! Converted-level gameplay on synthetic scenes (no game data): spawning on
//! triangle collision, the death sequence, checkpoints, KillZ, touches,
//! dynamic kill zones, level streaming, falling rocks, crystals and the
//! grapple on triangle geometry, level-start abilities and determinism.

use asamu_game::{DEATH_FADE_DOWN_TIME, Game, TickReport};
use asamu_player::world::CONTACT_SKIN;
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
    // Only the original's own warning (rotate to a missing spawn actor) and
    // the missing mesh manifest of mesh-less fixtures.
    assert!(
        loaded
            .warnings
            .iter()
            .all(|w| w.contains("without a spawn point actor")
                || w.contains("no static-mesh collision")),
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
    // A-CP-5: the pawn's centre at the start, walking (it drops 36 uu).
    assert_eq!(g.player().position, Vec3::new(0.0, 0.0, 80.0));
    assert!(g.player().grounded);
    run_until(&mut g, idle(), 60, |_, g| {
        g.player().velocity == Vec3::ZERO && g.player().grounded
    })
    .unwrap();
    for _ in 0..30 {
        g.tick(&idle()).unwrap();
    }
    // Native walking hovers 2.15 uu above the floor (NATIVE_PHYSICS.md 3.x).
    let hover = g.player().position.z - 44.0;
    assert!((hover - 2.15).abs() < 1e-3, "hover {hover}");
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

/// A platform at the respawn point, nothing under the start but a kill zone.
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
        location: Vec3::new(1000.0, 0.0, 0.0),
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
    // Teleported onto the platform at the marker (FindSpot lifted the centre
    // out of the floor), with the marker's yaw; the tick's physics then ran
    // from there with zero velocity.
    let p = *g.player();
    assert!(p.position.z > z_at_death);
    assert!(
        (p.position.truncate() - glam::Vec2::new(1000.0, 0.0)).length() < 1.0,
        "{}",
        p.position
    );
    assert!(
        (p.yaw - core::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "yaw {}",
        p.yaw
    );
    assert!(p.position.z > 40.0 && p.position.z < 60.0, "{}", p.position);
    // A-DT-2: the physics mode is kept — it died falling, so it falls the
    // short way onto the platform and lands.
    run_until(&mut g, idle(), 60, |_, g| g.player().grounded).unwrap();
    assert!(
        (g.player().position.z - (44.0 + 2.15)).abs() < 0.5,
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
    assert!((g.player().position.z - 46.15).abs() < 0.5);
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
    let z = g.player().position.z;
    assert!((z - (44.0 + CONTACT_SKIN)).abs() < 1e-3, "{z}");
    assert!(!g.world().overlaps(
        g.player().position,
        asamu_player::CollisionShape {
            radius: 21.0,
            half_height: 44.0
        }
    ));
}
