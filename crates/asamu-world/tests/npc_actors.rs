//! Synthetic tests of the story actors and the never-placed NPC pawns
//! (`asamu_world::npc`, `docs/reverse-engineering/NPCS.md`). Positions are
//! ours; sizes and times are the original's class defaults.

use asamu_world::npc::{
    BackpackAnim, BackpackState, COLLECTIBLE_TRIGGER_HALF_HEIGHT, COLLECTIBLE_TRIGGER_RADIUS,
    COLLECTIBLE_TRIGGER_Z, CollectibleDef, FoliageDef, GLOW_FLOWER_DURATION, GLOW_FLOWER_FADE_TIME,
    GlowFlowerDef, MaddieDef, MaddieState, MaddieStateName, NavPoint, NpcCylinder, NpcEvent,
    NpcOutput, NpcRuntime, NpcScene, StoryItemDef, StoryItemStateName, VillagerDef, VillagerState,
    VillagerStateName, tick_villager,
};
use asamu_world::objects::ObjectEvent;
use glam::Vec3;

const DT: f32 = 1.0 / 60.0;
const R: f32 = 21.0;
const H: f32 = 44.0;

fn collectible(id: u32, at: Vec3) -> CollectibleDef {
    CollectibleDef {
        id,
        level: "AG-Test".into(),
        name: format!("ASAMUCollectible_{id}"),
        location: at,
        trigger: NpcCylinder {
            center: at + Vec3::Z * COLLECTIBLE_TRIGGER_Z,
            radius: COLLECTIBLE_TRIGGER_RADIUS,
            half_height: COLLECTIBLE_TRIGGER_HALF_HEIGHT,
        },
    }
}

fn foliage(id: u32, at: Vec3) -> FoliageDef {
    FoliageDef {
        id,
        trigger: NpcCylinder {
            center: at + Vec3::Z * 40.0,
            radius: 50.0,
            half_height: 40.0,
        },
        sound: Some("MiscSounds.Foliage_Rustle_Cue".into()),
    }
}

fn touches(rt: &mut NpcRuntime, scene: &NpcScene, a: Vec3, b: Vec3) -> Vec<NpcEvent> {
    let mut out = NpcOutput::default();
    rt.update_touches(scene, a, b, R, H, &mut out);
    out.events
}

#[test]
fn collectibles_are_picked_up_once_in_path_order() {
    let scene = NpcScene {
        collectibles: vec![
            collectible(2, Vec3::new(600.0, 0.0, 0.0)),
            collectible(1, Vec3::new(300.0, 0.0, 0.0)),
        ],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, false);
    let z = Vec3::Z * H;
    let ev = touches(&mut rt, &scene, z, Vec3::new(900.0, 0.0, 0.0) + z);
    assert_eq!(
        ev,
        vec![
            NpcEvent::CollectibleCollected { id: 1 },
            NpcEvent::CollectibleCollected { id: 2 }
        ]
    );
    assert_eq!(rt.collected_count(), 2);
    // Walking back through them collects nothing more.
    let ev = touches(&mut rt, &scene, Vec3::new(900.0, 0.0, 0.0) + z, z);
    assert!(ev.is_empty());
}

#[test]
fn collectible_trigger_size_and_height_matter() {
    let scene = NpcScene {
        collectibles: vec![collectible(1, Vec3::ZERO)],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, false);
    // 50 + 21 = 71 uu: passing 72 uu to the side misses.
    let side = Vec3::new(0.0, 72.0, H);
    assert!(
        touches(
            &mut rt,
            &scene,
            side - Vec3::X * 200.0,
            side + Vec3::X * 200.0
        )
        .is_empty()
    );
    // Far above (trigger top 80, player half-height 44 → centre above 124).
    let above = Vec3::new(0.0, 0.0, 125.0);
    assert!(
        touches(
            &mut rt,
            &scene,
            above - Vec3::X * 200.0,
            above + Vec3::X * 200.0
        )
        .is_empty()
    );
    let through = Vec3::new(0.0, 70.0, H);
    assert_eq!(
        touches(
            &mut rt,
            &scene,
            through - Vec3::X * 200.0,
            through + Vec3::X * 200.0
        )
        .len(),
        1
    );
}

#[test]
fn time_trial_hides_collectibles_and_snapshots_restore_them() {
    let scene = NpcScene {
        collectibles: vec![collectible(1, Vec3::ZERO)],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, true);
    let p = Vec3::Z * H;
    assert!(touches(&mut rt, &scene, p - Vec3::X * 300.0, p + Vec3::X * 300.0).is_empty());
    let mut rt = NpcRuntime::new(&scene, 1, false);
    assert!(rt.set_collected(&scene, 1, true));
    assert!(!rt.set_collected(&scene, 99, true));
    assert!(touches(&mut rt, &scene, p - Vec3::X * 300.0, p + Vec3::X * 300.0).is_empty());
    assert_eq!(rt.collected_count(), 1);
}

#[test]
fn foliage_rustles_on_every_new_touch() {
    let mut scene = NpcScene::default();
    for i in 0..1000u32 {
        scene
            .foliage
            .push(foliage(i, Vec3::new(i as f32 * 2000.0, 0.0, 0.0)));
    }
    let mut rt = NpcRuntime::new(&scene, 1, false);
    let z = Vec3::Z * H;
    let a = Vec3::new(-200.0, 0.0, 0.0) + z;
    let b = Vec3::new(10.0, 0.0, 0.0) + z;
    assert_eq!(
        touches(&mut rt, &scene, a, b),
        vec![NpcEvent::FoliageTouched { id: 0 }]
    );
    // Staying inside: no new touch.
    assert!(touches(&mut rt, &scene, b, b + Vec3::X * 5.0).is_empty());
    // Leave and come back: rustles again.
    let far = Vec3::new(-300.0, 0.0, 0.0) + z;
    assert!(touches(&mut rt, &scene, b + Vec3::X * 5.0, far).is_empty());
    assert_eq!(touches(&mut rt, &scene, far, b).len(), 1);
    // A long move across many bushes reports them in order.
    let ev = touches(
        &mut rt,
        &scene,
        Vec3::new(1000.0, 0.0, 0.0) + z,
        Vec3::new(7000.0, 0.0, 0.0) + z,
    );
    assert_eq!(
        ev,
        vec![
            NpcEvent::FoliageTouched { id: 1 },
            NpcEvent::FoliageTouched { id: 2 },
            NpcEvent::FoliageTouched { id: 3 }
        ]
    );
    // A teleport-sized move still works (and leaves the old bush).
    let ev = touches(
        &mut rt,
        &scene,
        Vec3::new(7000.0, 0.0, 0.0) + z,
        Vec3::new(1_000_000.0, 0.0, 0.0) + z,
    );
    assert_eq!(
        ev.len(),
        497,
        "every bush from 4 to 500 (at the end point) is crossed"
    );
    rt.clear_touches();
    assert!(rt.foliage_touching.iter().all(|t| !t));
}

fn item(id: u32) -> StoryItemDef {
    StoryItemDef {
        id,
        level: "AG-Test".into(),
        name: format!("ASAMUInteractable_Actor_{id}"),
        location: Vec3::ZERO,
        max_interact_times: 1,
        optional: false,
        parent: false,
        linked_parent: None,
        linked_children: Vec::new(),
        has_glow: true,
    }
}

fn interact(rt: &mut NpcRuntime, scene: &NpcScene, id: u32) -> Vec<NpcEvent> {
    let mut out = NpcOutput::default();
    rt.apply_object_event(scene, ObjectEvent::InteractWith(id), &mut out);
    out.events
}

#[test]
fn interactables_count_uses_and_fade_their_symbol() {
    let mut once = item(1);
    once.max_interact_times = 1;
    let mut forever = item(2);
    forever.max_interact_times = 0;
    let scene = NpcScene {
        story_items: vec![once, forever],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, false);
    assert_eq!(
        interact(&mut rt, &scene, 1),
        vec![NpcEvent::ActorInteractedWith { originator: 1 }]
    );
    assert!(
        interact(&mut rt, &scene, 1).is_empty(),
        "MaxInteractTimes 1"
    );
    for _ in 0..3 {
        assert_eq!(interact(&mut rt, &scene, 2).len(), 1, "unlimited");
    }
    assert_eq!(rt.story_items[0].state, StoryItemStateName::FadingDown);
    // 101 fade steps of 0.01 s: one tick each at 60 Hz, plus the start.
    let mut out = NpcOutput::default();
    let mut ticks = 0;
    while rt.story_items[0].state == StoryItemStateName::FadingDown {
        rt.tick_actors(&scene, Vec3::new(9999.0, 0.0, 0.0), DT, &mut out);
        ticks += 1;
        assert!(ticks < 200);
    }
    assert_eq!(ticks, 102);
    assert_eq!(rt.story_items[0].state, StoryItemStateName::Disabled);
    assert_eq!(rt.story_items[0].glow, 0.0);
    assert_eq!(
        rt.story_items[1].state,
        StoryItemStateName::Idle,
        "uses left"
    );
}

#[test]
fn optional_story_items_register_like_the_original() {
    // A parent with one child (linked through the child), and a stand-alone
    // optional item.
    let mut parent = item(10);
    parent.parent = true;
    parent.optional = true;
    let mut child = item(11);
    child.linked_parent = Some(10);
    let mut solo = item(12);
    solo.optional = true;
    let scene = NpcScene {
        story_items: vec![parent, child, solo],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, false);
    // The child forwards to its parent: parent registered, event with the
    // parent as originator, the parent's children disabled.
    assert_eq!(
        interact(&mut rt, &scene, 11),
        vec![
            NpcEvent::ActorInteractedWith { originator: 11 },
            NpcEvent::StoryItemRegistered { item: Some(10) },
            NpcEvent::ActorInteractedWith { originator: 10 },
        ]
    );
    assert_eq!(rt.story_items[1].times, 1);
    // The stand-alone item registers its null parent (SAVE.md Q5).
    assert_eq!(
        interact(&mut rt, &scene, 12),
        vec![
            NpcEvent::ActorInteractedWith { originator: 12 },
            NpcEvent::StoryItemRegistered { item: None },
        ]
    );
    // Interacting with the parent itself: registers itself, disables the
    // child (uses exhausted).
    let mut rt = NpcRuntime::new(&scene, 1, false);
    assert_eq!(
        interact(&mut rt, &scene, 10),
        vec![
            NpcEvent::ActorInteractedWith { originator: 10 },
            NpcEvent::StoryItemRegistered { item: Some(10) },
        ]
    );
    assert_eq!(rt.story_items[1].times, 1, "child exhausted");
    assert_eq!(rt.story_items[1].state, StoryItemStateName::FadingDown);
    assert!(interact(&mut rt, &scene, 11).is_empty());
}

fn flower_scene() -> NpcScene {
    NpcScene {
        flowers: vec![GlowFlowerDef {
            id: 5,
            name: "ASAMUGlowFlower_0".into(),
            location: Vec3::ZERO,
            glow_duration: GLOW_FLOWER_DURATION,
            fade_time: GLOW_FLOWER_FADE_TIME,
            lights: Vec::new(),
        }],
        ..NpcScene::default()
    }
}

fn grapple(rt: &mut NpcRuntime, scene: &NpcScene) -> Vec<NpcEvent> {
    let mut out = NpcOutput::default();
    rt.apply_object_event(scene, ObjectEvent::Grappled(5), &mut out);
    out.events
}

fn tick(rt: &mut NpcRuntime, scene: &NpcScene, n: usize) -> Vec<NpcEvent> {
    let mut out = NpcOutput::default();
    for _ in 0..n {
        rt.tick_actors(scene, Vec3::new(5000.0, 0.0, 0.0), DT, &mut out);
    }
    out.events
}

#[test]
fn glow_flowers_fade_in_hold_and_fade_out() {
    let scene = flower_scene();
    let mut rt = NpcRuntime::new(&scene, 1, false);
    assert_eq!(
        grapple(&mut rt, &scene),
        vec![NpcEvent::GlowFlowerGrappled { id: 5 }]
    );
    assert!(rt.flowers[0].glowing);
    let ev = tick(&mut rt, &scene, 1);
    assert_eq!(ev, vec![NpcEvent::GlowFlowerGlow { id: 5 }]);
    // Fade in over ~1 s (60 steps of 0.016667 s).
    tick(&mut rt, &scene, 62);
    assert_eq!(rt.flowers[0].alpha, 1.0);
    // Holds for glowDuration (10 s), then fades out within ~1 s.
    tick(&mut rt, &scene, 9 * 60);
    assert_eq!(rt.flowers[0].alpha, 1.0, "still holding at 10 s");
    tick(&mut rt, &scene, 2 * 60 + 10);
    assert!(!rt.flowers[0].glowing, "back to NotGlowing");
    assert_eq!(rt.flowers[0].alpha, 0.0);
}

#[test]
fn regrappling_a_glowing_flower_extends_and_restarts_the_glow() {
    let scene = flower_scene();
    let mut rt = NpcRuntime::new(&scene, 1, false);
    grapple(&mut rt, &scene);
    tick(&mut rt, &scene, 5 * 60);
    // Regrapple during the hold: the remaining time resets to 10 s.
    grapple(&mut rt, &scene);
    assert_eq!(rt.flowers[0].glow_time_remaining, GLOW_FLOWER_DURATION);
    tick(&mut rt, &scene, 9 * 60);
    assert!(rt.flowers[0].glowing && rt.flowers[0].alpha == 1.0);
    // At the end of the hold the remembered grapple restarts the glow (the
    // sound plays again) instead of fading out at once.
    let ev = tick(&mut rt, &scene, 2 * 60);
    assert!(ev.contains(&NpcEvent::GlowFlowerGlow { id: 5 }), "{ev:?}");
    let ev = tick(&mut rt, &scene, 3 * 60);
    assert!(!rt.flowers[0].glowing, "{ev:?}");
}

#[test]
fn maddie_leaves_talking_beyond_200_uu() {
    let def = MaddieDef {
        id: 1,
        name: "ASAMUNPC_MaddiePawn_0".into(),
        location: Vec3::ZERO,
        rotation: [0; 3],
        meshes: Vec::new(),
    };
    let mut m = MaddieState::default();
    m.talk();
    m.tick(&def, Vec3::new(199.0, 0.0, 0.0));
    assert_eq!(m.state, MaddieStateName::TalkingWithPlayer);
    assert_eq!(m.look_target, Vec3::new(199.0, 0.0, 0.0));
    m.tick(&def, Vec3::new(200.0, 0.0, 0.0));
    assert_eq!(m.state, MaddieStateName::Idle);
}

#[test]
fn backpack_maddie_needs_to_be_enabled_to_wave() {
    let mut b = BackpackState::default();
    assert!(!b.play(BackpackAnim::Wave));
    b.set_enabled(true);
    assert!(b.attached);
    assert!(b.play(BackpackAnim::Wave));
    assert_eq!(b.anim, Some(BackpackAnim::Wave));
    assert_eq!(b.anim_serial, 1);
    b.set_enabled(false);
    assert!(!b.attached && b.anim.is_none());
    assert_eq!(
        b.anim_serial, 1,
        "serial survives so a respawned arm restarts"
    );
}

fn villager(path: Vec<NavPoint>) -> VillagerDef {
    VillagerDef {
        id: 3,
        name: "ASAMUNPC_VillagerPawn_0".into(),
        location: Vec3::ZERO,
        rotation: [0; 3],
        use_scripted_path: !path.is_empty(),
        scripted_path: path,
        meshes: Vec::new(),
    }
}

fn nav(id: u32, x: f32, y: f32) -> NavPoint {
    NavPoint {
        id,
        location: Vec3::new(x, y, 0.0),
        radius: 0.0,
    }
}

#[test]
fn villagers_walk_their_scripted_path_back_and_forth() {
    let def = villager(vec![
        nav(1, 0.0, 0.0),
        nav(2, 400.0, 0.0),
        nav(3, 400.0, 400.0),
    ]);
    let mut v = VillagerState::new(&def, 42);
    let far = Vec3::new(9000.0, 0.0, 0.0);
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    let mut returned = false;
    for i in 0..(30.0 / DT) as usize {
        tick_villager(&mut v, &def, &[], far, DT);
        max_x = max_x.max(v.location.x);
        max_y = max_y.max(v.location.y);
        if i > 300 && max_y > 350.0 && v.location.truncate().length() < 20.0 {
            returned = true;
        }
    }
    assert!(
        max_x > 380.0 && max_y > 380.0,
        "walked the path ({max_x}, {max_y})"
    );
    assert!(returned, "came back to the first point");
    assert_eq!(v.state, VillagerStateName::WalkingScriptedPath);
}

#[test]
fn roaming_without_navigation_points_disables_the_villager() {
    let def = villager(Vec::new());
    let mut v = VillagerState::new(&def, 1);
    for _ in 0..(5.0 / DT) as usize {
        tick_villager(&mut v, &def, &[], Vec3::ZERO, DT);
    }
    assert_eq!(v.state, VillagerStateName::Disabled);
    // With a navigation point it walks there, then idles again.
    let mut v = VillagerState::new(&def, 1);
    let points = [nav(9, 300.0, 0.0)];
    let mut reached = false;
    for _ in 0..(10.0 / DT) as usize {
        tick_villager(&mut v, &def, &points, Vec3::new(9000.0, 0.0, 0.0), DT);
        if v.location.x > 280.0 {
            reached = true;
        }
    }
    assert!(reached, "roamed to the navigation point");
}

#[test]
fn talking_pushes_and_pops_the_villager_state() {
    let def = villager(vec![nav(1, 0.0, 0.0), nav(2, 1000.0, 0.0)]);
    let mut v = VillagerState::new(&def, 7);
    for _ in 0..120 {
        tick_villager(&mut v, &def, &[], Vec3::ZERO, DT);
    }
    let before = (v.state, v.pc);
    let other = v.location + Vec3::new(0.0, 500.0, 0.0);
    v.start_talking(other);
    assert_eq!(v.state, VillagerStateName::TalkWithPawn);
    tick_villager(&mut v, &def, &[], Vec3::ZERO, DT);
    assert_eq!(v.yaw, 16_384, "faces the other pawn");
    v.stop_talking();
    assert_eq!((v.state, v.pc), before);
}

#[test]
fn huge_and_broken_foliage_triggers_are_bounded_and_handled() {
    // Hostile radii: each of these would cover millions of grid cells. They
    // go to the always-tested list instead (bounded memory), and still
    // rustle; non-finite triggers never do.
    let mut scene = NpcScene::default();
    for i in 0..2000u32 {
        let mut f = foliage(i, Vec3::new(i as f32 * 10.0, 0.0, 0.0));
        f.trigger.radius = 3.0e6;
        scene.foliage.push(f);
    }
    let mut broken = foliage(5000, Vec3::ZERO);
    broken.trigger.radius = f32::NAN;
    scene.foliage.push(broken);
    let mut nowhere = foliage(5001, Vec3::splat(f32::INFINITY));
    nowhere.trigger.radius = 50.0;
    scene.foliage.push(nowhere);
    let mut rt = NpcRuntime::new(&scene, 1, false);
    let z = Vec3::Z * (H + 40.0);
    let ev = touches(&mut rt, &scene, Vec3::new(0.0, 0.0, 9000.0), z);
    assert_eq!(ev.len(), 2000, "every huge bush is entered once");
    assert!(
        !ev.iter()
            .any(|e| matches!(e, NpcEvent::FoliageTouched { id: 5000 | 5001 }))
    );
    assert!(
        touches(&mut rt, &scene, z, z + Vec3::X).is_empty(),
        "staying inside"
    );
}

#[test]
fn a_roaming_villager_already_at_its_target_goes_idle_at_once() {
    // `Roam` tests "reached" before moving: no poll sleep, no walk.
    let def = villager(Vec::new());
    let points = [nav(9, 5.0, 0.0)];
    for seed in 0..40u64 {
        let mut v = VillagerState::new(&def, seed);
        for _ in 0..(4.0 / DT) as usize {
            tick_villager(&mut v, &def, &points, Vec3::new(9000.0, 0.0, 0.0), DT);
            assert!(!v.moving, "seed {seed}: never walks");
            assert_eq!(v.location, Vec3::ZERO);
            // Never in the roaming poll sleep (only Idle's 1-3 s pauses).
            if v.state == VillagerStateName::Roaming {
                assert!(v.sleep.is_none(), "seed {seed}: {:?}", v.sleep);
            }
        }
    }
}

#[test]
fn an_empty_scripted_path_waits_in_place() {
    let mut def = villager(Vec::new());
    def.use_scripted_path = true;
    let mut v = VillagerState::new(&def, 3);
    for _ in 0..(30.0 / DT) as usize {
        tick_villager(&mut v, &def, &[], Vec3::ZERO, DT);
        assert_eq!(v.location, Vec3::ZERO);
    }
    assert_eq!(v.state, VillagerStateName::WalkingScriptedPath);
    assert!(
        v.sleep.is_some_and(|s| (0.0..=5.0).contains(&s)),
        "{:?}",
        v.sleep
    );
}

#[test]
fn a_parent_fades_when_used_even_without_a_glow_mesh() {
    // `NotifyInteracted` sends a parent to `FadingDown` unconditionally; a
    // plain item without a glow mesh does not fade.
    let mut parent = item(20);
    parent.parent = true;
    parent.has_glow = false;
    let mut plain = item(21);
    plain.has_glow = false;
    let scene = NpcScene {
        story_items: vec![parent, plain],
        ..NpcScene::default()
    };
    let mut rt = NpcRuntime::new(&scene, 1, false);
    assert_eq!(interact(&mut rt, &scene, 20).len(), 1);
    assert_eq!(rt.story_items[0].state, StoryItemStateName::FadingDown);
    assert_eq!(interact(&mut rt, &scene, 21).len(), 1);
    assert_eq!(rt.story_items[1].state, StoryItemStateName::Idle);
    let mut out = NpcOutput::default();
    for _ in 0..200 {
        rt.tick_actors(&scene, Vec3::new(9999.0, 0.0, 0.0), DT, &mut out);
    }
    assert_eq!(rt.story_items[0].state, StoryItemStateName::Disabled);
    assert_eq!(
        rt.story_items[1].state,
        StoryItemStateName::Idle,
        "no symbol, no fade"
    );
}
