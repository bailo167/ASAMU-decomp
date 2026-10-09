//! `asamu_game::npc::NpcSystem`: effects on the player, event routing,
//! Kismet entry points (synthetic scenes written here), and a real-data run
//! on the converted AG-Darkcave (skips without `ASAMU_CONVERTED_DIR`).

use asamu_game::Game;
use asamu_game::npc::defs::{
    CollectibleDef, GlowFlowerDef, NpcCylinder, NpcHull, StoryItemDef, WormLight, WormParams,
    WormVolumeDef, WormVolumeRole,
};
use asamu_game::npc::{
    NpcEvent, NpcOptions, NpcScene, NpcSystem, WormDef, WormEventKind, WormStateName,
    apply_worm_push,
};
use asamu_player::grapple_gun::Attachment;
use asamu_player::{PlayerParams, PlayerState, SimEvent, StepEvents, pawn};
use asamu_world::scene::MemorySource;
use glam::Vec3;

const DT: f32 = 1.0 / 60.0;
const WORM: u32 = 3;

fn scene() -> NpcScene {
    NpcScene {
        worms: vec![WormDef {
            id: WORM,
            name: "ASAMUNPC_WormPawn_0".into(),
            location: Vec3::ZERO,
            rotation: [0; 3],
            look_targets: Vec::new(),
            light: WormLight::default(),
            params: WormParams::ORIGINAL,
            meshes: Vec::new(),
        }],
        worm_volumes: vec![WormVolumeDef {
            id: 4,
            name: "ASAMUWormScreamVolume_0".into(),
            role: WormVolumeRole::Scream,
            hulls: vec![NpcHull::from_box(Vec3::splat(-3000.0), Vec3::splat(3000.0))],
        }],
        collectibles: vec![CollectibleDef {
            id: 5,
            level: "AG-Test".into(),
            name: "ASAMUCollectible_0".into(),
            location: Vec3::new(0.0, 2000.0, 0.0),
            trigger: NpcCylinder {
                center: Vec3::new(0.0, 2000.0, 40.0),
                radius: 50.0,
                half_height: 40.0,
            },
        }],
        flowers: vec![GlowFlowerDef {
            id: 6,
            name: "ASAMUGlowFlower_0".into(),
            location: Vec3::ZERO,
            glow_duration: 10.0,
            fade_time: 1.0,
            lights: Vec::new(),
        }],
        story_items: vec![StoryItemDef {
            id: 7,
            level: "AG-Test".into(),
            name: "ASAMUInteractable_Actor_0".into(),
            location: Vec3::ZERO,
            max_interact_times: 1,
            optional: false,
            parent: false,
            linked_parent: None,
            linked_children: Vec::new(),
            has_glow: true,
        }],
        ..NpcScene::default()
    }
}

fn player() -> (PlayerState, PlayerParams) {
    let params = PlayerParams::asamu_original();
    let mut p = PlayerState::new(Vec3::new(1000.0, 0.0, 0.0), 0.0);
    pawn::start(&mut p, &params);
    p.grounded = true;
    (p, params)
}

#[test]
fn the_push_releases_the_grapple_sets_falling_and_adds_velocity() {
    let (mut p, _) = player();
    p.script.gun.attached = Some(Attachment::default());
    p.pawn.flying = true;
    p.grounded = false;
    p.velocity = Vec3::new(10.0, 0.0, 0.0);
    let released = apply_worm_push(&mut p, Vec3::new(500.0, 0.0, -50.0));
    assert!(!p.is_grapple_attached());
    assert!(!p.pawn.flying && !p.grounded && !p.pawn.based);
    assert_eq!(p.velocity, Vec3::new(510.0, 0.0, -50.0));
    assert!(
        released
            .kismet
            .iter()
            .any(|e| e == SimEvent::PlayerReleasedGrapple)
    );
    // Walking, not attached: no release events, but falling.
    let (mut p, _) = player();
    let released = apply_worm_push(&mut p, Vec3::new(0.0, 500.0, -50.0));
    assert!(released.kismet.is_empty());
    assert!(!p.grounded);
    // A non-finite push is ignored.
    let before = p.velocity;
    apply_worm_push(&mut p, Vec3::NAN);
    assert_eq!(p.velocity, before);
}

#[test]
fn a_screaming_worm_pushes_and_finally_kills() {
    let mut npcs = NpcSystem::new(scene(), NpcOptions::default());
    let (mut p, _) = player();
    assert!(npcs.start_worm(WORM));
    // Kismet action events are queued for the next report.
    let r = npcs.tick_actors(&mut p, DT);
    assert!(r.events.iter().any(|e| matches!(
        e,
        NpcEvent::WormState {
            state: WormStateName::Idle,
            ..
        }
    )));
    assert!(r.events.iter().any(|e| matches!(
        e,
        NpcEvent::Worm {
            kind: WormEventKind::WakingUp,
            ..
        }
    )));
    // Awake after ~4 s; then keep walking in circles inside the volume.
    let mut pushes = 0;
    let mut killed_at = None;
    for i in 0..(20.0 / DT) as usize {
        let a = i as f32 * 0.05;
        p.position = Vec3::new(1000.0 + 200.0 * a.cos(), 200.0 * a.sin(), 0.0);
        p.velocity = Vec3::ZERO;
        let r = npcs.tick_actors(&mut p, DT);
        if r.pushed {
            pushes += 1;
            assert!(
                p.velocity.x > 0.0 && p.velocity.z == -50.0,
                "{:?}",
                p.velocity
            );
        }
        if r.kill_player {
            killed_at = Some(i);
            break;
        }
    }
    assert!(pushes >= 70, "{pushes} pushes");
    assert!(killed_at.is_some(), "the scream timed out");
    assert_eq!(npcs.runtime().worms[0].state, WormStateName::Sleeping);
}

#[test]
fn handler_calls_and_interactions_are_routed() {
    let mut npcs = NpcSystem::new(scene(), NpcOptions::default());
    let mut events = StepEvents::default();
    events.kismet.push(SimEvent::ActorGrappled { actor: 6 });
    events.kismet.push(SimEvent::InteractWith { actor: 7 });
    events.kismet.push(SimEvent::PlayerLanded);
    let out = npcs.apply_sim_events(&events);
    assert_eq!(
        out,
        vec![
            NpcEvent::GlowFlowerGrappled { id: 6 },
            NpcEvent::ActorInteractedWith { originator: 7 }
        ]
    );
    assert!(npcs.runtime().flowers[0].glowing);
    // Second interaction: no uses left.
    assert!(
        npcs.apply_sim_events(&events)
            .iter()
            .all(|e| !matches!(e, NpcEvent::ActorInteractedWith { .. }))
    );
}

#[test]
fn collectible_touch_uses_the_player_cylinder() {
    let mut npcs = NpcSystem::new(scene(), NpcOptions::default());
    let (_, params) = player();
    let h = params.movement.capsule_half_height.value;
    let ev = npcs.update_touches(
        Vec3::new(0.0, 1800.0, h),
        Vec3::new(0.0, 2200.0, h),
        &params,
    );
    assert_eq!(ev, vec![NpcEvent::CollectibleCollected { id: 5 }]);
    assert_eq!(npcs.runtime().collected_count(), 1);
    // Time trial: hidden.
    let mut tt = NpcSystem::new(
        scene(),
        NpcOptions {
            time_trial: true,
            ..NpcOptions::default()
        },
    );
    assert!(
        tt.update_touches(
            Vec3::new(0.0, 1800.0, h),
            Vec3::new(0.0, 2200.0, h),
            &params
        )
        .is_empty()
    );
    assert!(tt.set_collected(5, true));
}

#[test]
fn kismet_entry_points_report_unknown_actors() {
    let mut npcs = NpcSystem::new(scene(), NpcOptions::default());
    assert!(!npcs.start_worm(999));
    assert!(!npcs.shut_down_worm(999));
    assert!(!npcs.pause_worm(999, true));
    assert!(!npcs.worm_reset_sleep_timer(999));
    assert!(npcs.pause_worm(WORM, true));
    assert!(npcs.runtime().worms[0].paused);
    assert!(!npcs.play_maddie_backpack_anim(asamu_game::npc::BackpackAnim::Wave));
    npcs.maddie_backpack(true);
    assert!(npcs.play_maddie_backpack_anim(asamu_game::npc::BackpackAnim::Wave));
    assert!(!npcs.maddie_talk(1));
    assert!(!npcs.villager_start_talking(1, Vec3::ZERO));
    assert!(!npcs.villager_stop_talking(1));
    assert!(npcs.notify_player_killed().is_empty());
    npcs.on_player_respawned();
}

#[test]
fn spawn_from_a_converted_scene() {
    let json = r#"{"format":"asamu-scene","version":1,"package":"AG-Mini","actors":[
        {"slot":0,"name":"WorldInfo_0","class":"Engine.WorldInfo","kind":"world_info"},
        {"slot":1,"name":"ASAMUNPC_WormPawn_0","class":"asamu.ASAMUNPC_WormPawn","kind":"asamu_other",
         "location":[0,0,0]},
        {"slot":2,"name":"ASAMUGlowFlower_0","class":"asamu.ASAMUGlowFlower","kind":"asamu_other",
         "location":[1,2,3]}
    ]}"#;
    let mut src = MemorySource::new();
    src.insert("levels/AG-Mini.scene.json", json.as_bytes().to_vec());
    let levels = vec![asamu_world::scene::SubLevel {
        name: "AG-Mini".into(),
        streaming_class: None,
        initially_loaded: true,
        offset: Vec3::ZERO,
        actors: 3,
    }];
    let npcs = NpcSystem::from_source(&src, &levels, NpcOptions::default());
    assert!(!npcs.is_empty());
    assert_eq!(npcs.scene().worms.len(), 1);
    assert_eq!(npcs.scene().worms[0].id, 1);
    assert_eq!(npcs.scene().flowers[0].id, 2);
    assert_eq!(npcs.scene().actor_by_name("ASAMUNPC_WormPawn_0"), Some(1));
    // Same seed, same run.
    let run = |mut n: NpcSystem| {
        let (mut p, _) = player();
        n.start_worm(1);
        let mut log = Vec::new();
        for _ in 0..600 {
            log.extend(n.tick_actors(&mut p, DT).events);
        }
        format!("{log:?}")
    };
    assert_eq!(run(npcs.clone()), run(npcs));
}

/// Real data: worm on the converted AG-Darkcave driven next to the game.
#[test]
fn real_darkcave_worm_reacts_to_the_player() {
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let Ok(mut game) = Game::load_level(&dir, "AG-Darkcave") else {
        eprintln!("skipped: AG-Darkcave not converted");
        return;
    };
    let levels = game
        .scene_map()
        .map(|m| m.levels.clone())
        .unwrap_or_default();
    let mut npcs = NpcSystem::load_from_dir(&dir, &levels, NpcOptions::default());
    assert_eq!(npcs.scene().worms.len(), 1);
    let worm = npcs.scene().worms[0].clone();
    let volume = npcs.scene().worm_volumes[0].clone();
    assert!(
        volume.encompasses(worm.location),
        "the worm sits in its scream volume"
    );
    // Stand 1500 uu from the worm (inside the volume), wake it, then move.
    let spot = worm.location + Vec3::new(1500.0, 0.0, 0.0);
    assert!(volume.encompasses(spot));
    assert!(npcs.start_worm(worm.id));
    let mut alerted = false;
    for i in 0..(10.0 / DT) as usize {
        let p = game.player_mut();
        p.position = spot + Vec3::new((i / 30) as f32 * 30.0, 0.0, 0.0);
        let r = npcs.tick_actors(game.player_mut(), DT);
        alerted |= r.events.iter().any(|e| {
            matches!(
                e,
                NpcEvent::Worm {
                    kind: WormEventKind::Alerted,
                    ..
                }
            )
        });
    }
    assert!(alerted, "moving in the lair alerts the awake worm");
}
