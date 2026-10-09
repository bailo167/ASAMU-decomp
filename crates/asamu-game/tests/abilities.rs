//! The original grapple gun, rocket boots and world objects through the
//! `asamu-game` API on the hand-made graybox level (GRAPPLE.md, ABILITIES.md;
//! the graybox is test geometry, not original content).

use asamu_game::{Game, TickReport};
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::{BootsStateName, InputFrame, SimEvent};
use asamu_world::WorldEvent;
use glam::Vec3;

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

/// Look deltas (yaw, pitch) that point the eye at `target`.
fn look_at(g: &Game, target: Vec3) -> (f32, f32) {
    let d = target - g.eye_position();
    let p = g.player();
    (
        d.y.atan2(d.x) - p.yaw,
        d.z.atan2(d.truncate().length()) - p.pitch,
    )
}

fn aim_input(g: &Game, target: Vec3) -> InputFrame {
    let (yaw, pitch) = look_at(g, target);
    InputFrame {
        look_yaw_delta: yaw,
        look_pitch_delta: pitch,
        ..InputFrame::default()
    }
}

fn hold_fire() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

#[test]
fn level_start_applies_the_graybox_test_abilities() {
    // LevelAbilities::graybox_test: 3 grapples and the rocket boots on (a
    // test configuration; the original's gun starts at 0 and the boots off).
    let g = Game::graybox().unwrap();
    let gun = &g.player().script.gun;
    assert_eq!(gun.max_grapples, 3);
    assert!(g.player().script.boots.enabled);
    // Kismet-style actions.
    let mut g = Game::graybox().unwrap();
    g.set_max_grapples(1);
    assert_eq!(g.player().script.gun.max_grapples, 1);
    g.set_max_grapples(-1);
    assert_eq!(g.player().script.gun.max_grapples, 32_767);
    g.enable_rocket_boots(false);
    assert!(!g.player().script.boots.enabled);
    g.enable_grapple(false);
    assert!(!g.player().script.gun.can_grapple);
    g.hide_grapple_gun(true, false, false);
    assert!(g.player().script.gun.hand_hidden);
}

/// Walks to the edge of the start platform, jumps, grapples hook 0 and
/// releases the button `release_after` ticks after the attach (or never).
fn cross_with_release(release_after: Option<u32>) -> (Game, Vec<TickReport>) {
    let mut g = Game::graybox().expect("graybox game");
    g.start();
    let hook = g.level().grapple_points[0].position;
    let mut reports = Vec::new();
    let mut jumped_at = None;
    let mut attached_at = None;
    for _ in 0..900 {
        let p = *g.player();
        let tick = g.clock().tick();
        let mut input = forward();
        match (jumped_at, attached_at) {
            (None, _) => {
                if p.grounded && p.position.x >= 520.0 {
                    input.jump_pressed = true;
                    input.jump_held = true;
                    jumped_at = Some(tick);
                }
            }
            (Some(j), None) => {
                input = InputFrame::default();
                if tick == j + 8 {
                    input = aim_input(&g, hook);
                } else if tick > j + 8 {
                    input = hold_fire();
                }
            }
            (Some(_), Some(a)) => {
                let holding = release_after.is_none_or(|r| tick < a + u64::from(r));
                input = InputFrame {
                    grapple_held: holding,
                    ..InputFrame::default()
                };
            }
        }
        let r = g.tick(&input).expect("playing");
        if attached_at.is_none() && r.events.gun.attached.is_some() {
            attached_at = Some(tick);
        }
        reports.push(r);
        if r.respawned || g.active_checkpoint() == Some(1) && g.player().grounded {
            break;
        }
    }
    (g, reports)
}

#[test]
fn graybox_gap_is_crossed_with_the_original_grapple() {
    // walk → jump → grapple target → pull → release → keep momentum → land
    // (the first gameplay slice, ROADMAP.md), with the original rules.
    let mut crossed = None;
    for release in (0..60).map(Some).chain([None]) {
        let (g, reports) = cross_with_release(release);
        if g.active_checkpoint() == Some(1) && !reports.iter().any(|r| r.respawned) {
            crossed = Some((release, g, reports));
            break;
        }
    }
    let (release, g, reports) = crossed.expect("some release timing lands on platform 2");
    let attach = reports.iter().find_map(|r| r.events.gun.attached).unwrap();
    let hook = g.level().grapple_points[0].position;
    assert!(
        (attach.anchor - hook).length() < 60.0 * 1.8,
        "anchor on the hook cube"
    );
    let released = reports.iter().find_map(|r| r.events.gun.released).unwrap();
    match release {
        Some(_) => assert_eq!(released.reason, ReleaseReason::Button),
        None => assert_eq!(released.reason, ReleaseReason::Proximity),
    }
    assert!(g.player().grounded);
    assert!(
        reports
            .iter()
            .any(|r| r.events.kismet.contains(&SimEvent::PlayerReleasedGrapple))
    );
    // Deterministic: the same inputs give the same run.
    let (g2, reports2) = cross_with_release(release);
    assert_eq!(reports, reports2);
    assert_eq!(g.player(), g2.player());
}

#[test]
fn graybox_crystal_refills_uncharges_and_recharges() {
    let mut g = Game::graybox().unwrap();
    g.start();
    let crystal = g.level().crystals[0].clone();
    // Spend two grapples' worth of budget first.
    g.player_mut().script.gun.times_grappled = 2;
    let aim = aim_input(&g, crystal.center);
    g.tick(&aim).unwrap();
    let r = g.tick(&hold_fire()).unwrap();
    let attach = r.events.gun.attached.expect("grapples the crystal");
    assert!(attach.instant_release);
    assert!(
        r.events
            .kismet
            .contains(&SimEvent::ActorGrappled { actor: crystal.id })
    );
    assert_eq!(
        g.player().script.gun.times_grappled,
        0,
        "charged crystal: full budget"
    );
    // The 0.05 s timer releases; the crystal loses its charge.
    let mut uncharged = false;
    for _ in 0..5 {
        let r = g.tick(&hold_fire()).unwrap();
        if r.world
            .contains(&WorldEvent::CrystalUncharged { id: crystal.id })
        {
            assert_eq!(
                r.events.gun.released.unwrap().reason,
                ReleaseReason::InstantTimer
            );
            uncharged = true;
            break;
        }
    }
    assert!(uncharged);
    assert!(!g.objects().crystal_charged(g.level(), crystal.id));
    // Recharges after RechargeDelay (10 s) plus the 21-frame fade.
    let mut recharged = false;
    for _ in 0..(10 * 60 + 60) {
        let r = g.tick(&InputFrame::default()).unwrap();
        if r.world
            .contains(&WorldEvent::CrystalRecharged { id: crystal.id })
        {
            recharged = true;
            break;
        }
    }
    assert!(recharged);
}

/// Grapples the graybox crystal once (it loses its charge), then idles;
/// returns the game and the report tick at which the crystal recharged.
fn uncharge_crystal_and_wait() -> (Game, u64) {
    let mut g = Game::graybox().expect("graybox game");
    g.start();
    let crystal = g.level().crystals[0].clone();
    g.tick(&aim_input(&g, crystal.center)).expect("playing");
    g.tick(&hold_fire()).expect("playing");
    for _ in 0..(11 * 60) {
        let r = g.tick(&InputFrame::default()).expect("playing");
        if r.world
            .contains(&WorldEvent::CrystalRecharged { id: crystal.id })
        {
            return (g, r.tick);
        }
    }
    panic!("the crystal never recharged");
}

#[test]
fn input_events_run_before_the_map_actors_so_a_crystal_recharging_this_tick_is_still_dark() {
    // GRAPPLE.md G-TM-2 / G-IN-5: key events are processed before the
    // map-placed actors tick. A press in the tick where the crystal's
    // recharge sleep ends still meets the `UnCharged` crystal: it costs a
    // grapple, refills nothing, and its `Grappled` handler is the empty one
    // (so the crystal keeps its fresh charge). One tick later the press
    // meets the charged crystal: full budget, and the crystal is drained.
    let (_, recharge_tick) = uncharge_crystal_and_wait();
    for (press_tick, charged) in [(recharge_tick, false), (recharge_tick + 1, true)] {
        let mut g = Game::graybox().unwrap();
        g.start();
        let crystal = g.level().crystals[0].clone();
        g.tick(&aim_input(&g, crystal.center)).unwrap();
        g.tick(&hold_fire()).unwrap();
        // Idle (button up) until two ticks before the press, re-aim, idle.
        while g.clock().tick() + 2 < press_tick {
            g.tick(&InputFrame::default()).unwrap();
        }
        g.tick(&aim_input(&g, crystal.center)).unwrap();
        g.player_mut().script.gun.times_grappled = 2;
        let r = g.tick(&hold_fire()).unwrap();
        assert_eq!(r.tick, press_tick);
        let a = r.events.gun.attached.expect("grapples the crystal");
        assert_eq!(
            a.surface.class,
            asamu_player::ActorClass::RechargeCrystal { charged },
            "press at tick {press_tick}"
        );
        assert_eq!(
            g.player().script.gun.times_grappled,
            if charged { 0 } else { 3 },
            "press at tick {press_tick}"
        );
        assert_eq!(
            r.world
                .contains(&WorldEvent::CrystalRecharged { id: crystal.id }),
            !charged,
            "the map actors recharged it after the input event"
        );
        // After the 0.05 s release: drained only when it was charged.
        for _ in 0..4 {
            g.tick(&hold_fire()).unwrap();
        }
        assert!(!g.player().is_grapple_attached());
        assert_eq!(
            g.objects().crystal_charged(g.level(), crystal.id),
            !charged,
            "press at tick {press_tick}"
        );
    }
}

#[test]
fn graybox_mover_carries_the_anchor() {
    let mut g = Game::graybox().unwrap();
    g.start();
    let mover = g.level().movers[0].clone();
    let rest = (mover.min + mover.max) * 0.5;
    g.tick(&aim_input(&g, rest)).unwrap();
    let id = mover.id;
    let before = g.level().mover_location(g.objects(), id).unwrap();
    let r = g.tick(&hold_fire()).unwrap();
    let a = r.events.gun.attached.expect("grapples the moving block");
    // The fire runs in the input event, before the map actors tick (G-TM-2):
    // the hit point lies on the block where the previous tick left it, and
    // the anchor helper rides the block's motion of the attach tick itself.
    let loc0 = g.level().mover_location(g.objects(), id).unwrap();
    assert!((loc0 - before).length() > 0.01, "the block moved this tick");
    let anchor0 = g.player().grapple_anchor().unwrap();
    assert!(
        (anchor0 - (a.anchor + (loc0 - before))).length() < 1e-2,
        "{anchor0}"
    );
    for _ in 0..10 {
        g.tick(&hold_fire()).unwrap();
    }
    if g.player().is_grapple_attached() {
        let loc1 = g.level().mover_location(g.objects(), id).unwrap();
        let anchor = g.player().grapple_anchor().unwrap();
        assert!(
            (anchor - (a.anchor + (loc1 - before))).length() < 1e-2,
            "{anchor}"
        );
        assert!((loc1 - loc0).length() > 1.0, "the block moved");
    } else {
        panic!("released too early");
    }
}

#[test]
fn respawn_releases_the_grapple_resets_the_boots_and_keeps_the_budget() {
    let mut g = Game::graybox().unwrap();
    g.start();
    // Boost in the air: walk off the back of the platform.
    let back = InputFrame {
        move_forward: -1.0,
        ..InputFrame::default()
    };
    while g.player().grounded {
        g.tick(&back).unwrap();
    }
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    g.tick(&space).unwrap();
    assert_eq!(g.player().script.boots.state, BootsStateName::Boosting);
    g.player_mut().script.gun.times_grappled = 1;
    g.respawn();
    assert_eq!(
        g.player().script.boots.state,
        BootsStateName::Ready,
        "ResetBoots (enabled)"
    );
    assert_eq!(
        g.player().script.gun.times_grappled,
        1,
        "not refilled by death"
    );
    assert_eq!(g.player().velocity, Vec3::ZERO);
}

#[test]
fn story_mode_fire_interacts_with_the_graybox_interactable_once() {
    let mut g = Game::graybox().unwrap();
    g.start();
    let target = g.level().interactables[0].clone();
    let centre = (target.min + target.max) * 0.5;
    // Walk next to it (within 200 uu of the pawn centre), then story mode.
    g.player_mut().position = Vec3::new(centre.x + 60.0, centre.y, g.player().position.z);
    g.tick(&InputFrame::default()).unwrap();
    g.enter_story_mode();
    let mut interactions = 0;
    for _ in 0..3 {
        g.tick(&aim_input(&g, centre)).unwrap();
        let r = g.tick(&hold_fire()).unwrap();
        assert!(r.events.gun.attached.is_none());
        if r.world
            .contains(&WorldEvent::ActorInteractedWith { id: target.id })
        {
            interactions += 1;
        }
        for _ in 0..10 {
            g.tick(&InputFrame::default()).unwrap();
        }
    }
    assert_eq!(interactions, 1, "MaxInteractTimes 1");
}

#[test]
fn attractor_pulls_the_player_once_activated() {
    let mut g = Game::graybox().unwrap();
    g.start();
    for _ in 0..30 {
        g.tick(&InputFrame::default()).unwrap();
    }
    let p0 = g.player().position;
    let pad = g.level().attractors[0].clone();
    assert!(g.activate_attractor(pad.id));
    for _ in 0..120 {
        g.tick(&InputFrame::default()).unwrap();
    }
    let p1 = g.player().position;
    assert!(
        (p1 - pad.position).length() < (p0 - pad.position).length(),
        "{p0} → {p1}"
    );
}

#[test]
fn kill_z_while_attached_reports_the_death_release() {
    // T17: kill volume while attached: the release event (reason death),
    // then the respawn.
    let mut g = Game::graybox().expect("graybox game");
    g.start();
    let hook = g.level().grapple_points[0].position;
    g.tick(&aim_input(&g, hook)).expect("playing");
    let r = g.tick(&hold_fire()).expect("playing");
    assert!(r.events.gun.attached.is_some());
    let kill_z = g.level().kill_z;
    g.player_mut().position.z = kill_z - 100.0;
    let r = g.tick(&hold_fire()).expect("playing");
    assert!(r.respawned);
    assert_eq!(
        r.events.gun.released.map(|x| x.reason),
        Some(ReleaseReason::Death)
    );
    assert!(r.events.kismet.contains(&SimEvent::PlayerReleasedGrapple));
    assert!(!g.player().is_grapple_attached());
}

#[test]
fn a_death_during_a_boost_keeps_its_move_input_lock_until_the_next_landing() {
    // A-DT-2: the player reset sends enabled boots back to `Ready`
    // (`ResetBoots`) but never runs the boost end's lock release, and the
    // move-input lock is not part of the reset: after the respawn the move
    // axes stay locked until a landing releases the lock (ABILITIES.md §8).
    let mut g = Game::graybox().unwrap();
    g.start();
    let back = InputFrame {
        move_forward: -1.0,
        ..InputFrame::default()
    };
    while g.player().grounded {
        g.tick(&back).unwrap();
    }
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    g.tick(&space).unwrap();
    assert_eq!(g.player().script.boots.state, BootsStateName::Boosting);
    assert_eq!(g.player().script.move_input_lock, 1);
    g.respawn();
    assert_eq!(g.player().script.boots.state, BootsStateName::Ready);
    assert_eq!(g.player().script.move_input_lock, 1, "kept by the reset");
    // The pawn died falling, so it falls (and lands) again at the spawn.
    let mut landed = false;
    for _ in 0..60 {
        let r = g.tick(&forward()).unwrap();
        if r.events.landing.is_some() {
            landed = true;
            break;
        }
        assert_eq!(g.player().script.move_input_lock, 1);
        assert_eq!(
            g.player().velocity.truncate(),
            glam::Vec2::ZERO,
            "no air acceleration under the lock"
        );
    }
    assert!(landed);
    assert_eq!(
        g.player().script.move_input_lock,
        0,
        "the landing releases it"
    );
}
