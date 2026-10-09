//! Synthetic tests of the worm state machine (`asamu_world::npc`,
//! `docs/reverse-engineering/NPCS.md` §3). Geometry and positions are ours;
//! timings are the original's class defaults and animation lengths.

use asamu_world::npc::{
    NpcEffect, NpcEvent, NpcHull, NpcOutput, NpcRuntime, NpcScene, WORM_ANIM_COUNT, WormAnim,
    WormDef, WormEventKind, WormLight, WormParams, WormStateName, WormVolumeDef, WormVolumeRole,
};
use glam::Vec3;

const DT: f32 = 1.0 / 60.0;
const WORM: u32 = 7;

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
            id: 8,
            name: "ASAMUWormScreamVolume_0".into(),
            role: WormVolumeRole::Scream,
            hulls: vec![NpcHull::from_box(
                Vec3::new(-3000.0, -3000.0, -1000.0),
                Vec3::new(3000.0, 3000.0, 1000.0),
            )],
        }],
        ..NpcScene::default()
    }
}

/// Inside the scream volume.
const INSIDE: Vec3 = Vec3::new(1000.0, 0.0, 0.0);
/// Outside it.
const OUTSIDE: Vec3 = Vec3::new(5000.0, 0.0, 0.0);

struct Run {
    scene: NpcScene,
    rt: NpcRuntime,
    tick: u64,
    events: Vec<(u64, NpcEvent)>,
    effects: Vec<(u64, NpcEffect)>,
}

impl Run {
    fn new(seed: u64) -> Self {
        let scene = scene();
        let rt = NpcRuntime::new(&scene, seed, false);
        Self {
            scene,
            rt,
            tick: 0,
            events: Vec::new(),
            effects: Vec::new(),
        }
    }

    fn step(&mut self, player: Vec3) -> NpcOutput {
        let mut out = NpcOutput::default();
        self.rt.tick_actors(&self.scene, player, DT, &mut out);
        self.tick += 1;
        for e in &out.events {
            self.events.push((self.tick, e.clone()));
        }
        for e in &out.effects {
            self.effects.push((self.tick, *e));
        }
        out
    }

    fn start(&mut self) {
        let mut out = NpcOutput::default();
        assert!(self.rt.start_worm(&self.scene, WORM, &mut out));
        for e in out.events {
            self.events.push((self.tick, e));
        }
    }

    fn state(&self) -> WormStateName {
        self.rt.worms[0].state
    }

    /// Steps until `cond` holds (at most `max_secs`); returns the ticks taken.
    fn until(&mut self, player: Vec3, max_secs: f32, cond: impl Fn(&Self) -> bool) -> u64 {
        let start = self.tick;
        let max = (max_secs / DT) as u64;
        while !cond(self) {
            assert!(
                self.tick - start < max,
                "condition not reached in {max_secs} s"
            );
            self.step(player);
        }
        self.tick - start
    }

    fn worm_events(&self) -> Vec<(u64, WormEventKind)> {
        self.events
            .iter()
            .filter_map(|(t, e)| match e {
                NpcEvent::Worm { kind, .. } => Some((*t, *kind)),
                _ => None,
            })
            .collect()
    }

    fn secs(ticks: u64) -> f32 {
        ticks as f32 * DT
    }
}

#[test]
fn starts_disabled_and_stays_put() {
    let mut r = Run::new(1);
    assert_eq!(r.state(), WormStateName::Disabled);
    for _ in 0..600 {
        r.step(INSIDE);
    }
    assert_eq!(r.state(), WormStateName::Disabled);
    assert!(r.worm_events().is_empty());
    assert!(r.effects.is_empty());
    // `Disabled` plays the sleep idle (looping).
    assert_eq!(r.rt.worms[0].anim.active(), WormAnim::SleepIdle);
    assert!(r.rt.worms[0].anim.node(WormAnim::SleepIdle).looping);
}

#[test]
fn start_runs_idle_and_waking_up_in_the_same_tick() {
    let mut r = Run::new(1);
    r.start();
    assert_eq!(r.state(), WormStateName::Idle);
    r.step(OUTSIDE);
    // GotoState from state code continues at once (ProcessState).
    assert_eq!(r.state(), WormStateName::WakingUp);
    assert_eq!(r.worm_events(), vec![(1, WormEventKind::WakingUp)]);
    // `StateAnims.PlayAnim` restarted every node, non-looping.
    let w = &r.rt.worms[0];
    for a in WormAnim::ALL {
        assert!(w.anim.node(a).playing, "{a:?} playing");
        assert!(!w.anim.node(a).looping, "{a:?} not looping");
    }
    assert_eq!(WormAnim::ALL.len(), WORM_ANIM_COUNT);
}

#[test]
fn wakes_up_in_four_seconds_with_a_light_ramp() {
    let mut r = Run::new(1);
    r.start();
    r.step(OUTSIDE);
    let mut last_light = -1.0;
    let ticks = r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    let _ = &mut last_light;
    let t = Run::secs(ticks + 1);
    // 40 or 41 steps of 0.1 s (f32 accumulation), each 6 ticks at 60 Hz.
    assert!((4.0..=4.2).contains(&t), "awake after {t} s");
    let ev = r.worm_events();
    assert_eq!(
        ev.iter().map(|e| e.1).collect::<Vec<_>>(),
        vec![WormEventKind::WakingUp, WormEventKind::Awaken]
    );
    assert_eq!(r.rt.worms[0].light_strength, 1.0);
    let full = r.rt.worms[0].full_awake_time;
    assert!((6.0..=8.0).contains(&full), "awake time {full}");
    assert!(r.rt.worms[0].awake);
}

#[test]
fn a_still_player_is_not_noticed_and_movement_alerts() {
    let mut r = Run::new(2);
    r.start();
    r.until(INSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    for _ in 0..120 {
        r.step(INSIDE);
    }
    assert_eq!(r.state(), WormStateName::Awake, "standing still is safe");
    // Moving 20 uu (below positionSensitivity 25) is safe as well.
    let small = INSIDE + Vec3::new(20.0, 0.0, 0.0);
    for _ in 0..30 {
        r.step(small);
    }
    assert_eq!(r.state(), WormStateName::Awake);
    // 30 uu from the last reference alerts at the next 0.1 s check.
    let moved = small + Vec3::new(30.0, 0.0, 0.0);
    let ticks = r.until(moved, 0.2, |r| r.state() == WormStateName::Alerted);
    assert!(ticks <= 7, "alerted after {ticks} ticks");
    assert!(r.rt.worms[0].player_discovered);
    assert_eq!(r.rt.worms[0].anim.state_anims, 2);
    assert!(
        r.worm_events()
            .iter()
            .any(|e| e.1 == WormEventKind::Alerted)
    );
}

#[test]
fn a_paused_worm_ignores_the_player_while_awake() {
    let mut r = Run::new(3);
    r.start();
    r.until(INSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    assert!(r.rt.pause_worm(&r.scene.clone(), WORM, true));
    for i in 0..120 {
        r.step(INSIDE + Vec3::new(i as f32 * 10.0, 0.0, 0.0));
    }
    assert_eq!(r.state(), WormStateName::Awake);
    // Outside the volume nothing happens either.
    assert!(r.rt.pause_worm(&r.scene.clone(), WORM, false));
    for i in 0..60 {
        r.step(OUTSIDE + Vec3::new(i as f32 * 10.0, 0.0, 0.0));
    }
    assert_eq!(r.state(), WormStateName::Awake);
}

/// Gets a worm into `Alerted` with the player inside the volume.
fn alerted() -> Run {
    let mut r = Run::new(4);
    r.start();
    r.until(INSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    r.step(INSIDE);
    let moved = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    r.until(moved, 0.3, |r| r.state() == WormStateName::Alerted);
    r
}

#[test]
fn the_alert_times_out_back_to_awake() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    let ticks = r.until(p, 5.0, |r| {
        r.worm_events()
            .iter()
            .any(|e| e.1 == WormEventKind::FinishedAlerted)
    });
    let t = Run::secs(ticks);
    assert!((3.95..=4.1).contains(&t), "finished alert after {t} s");
    // The timer runs after the state code: `Awake` begins next tick.
    assert_eq!(r.state(), WormStateName::Awake);
    let before = r.worm_events().len();
    r.step(p);
    let ev = r.worm_events();
    assert_eq!(ev.len(), before + 1);
    assert_eq!(ev.last().map(|e| e.1), Some(WormEventKind::Awaken));
}

#[test]
fn moving_after_the_alert_starts_screaming_and_pushing() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    // The reference is taken after alertedSleepTime (1 s); moving before
    // that is forgotten.
    for _ in 0..70 {
        r.step(p);
    }
    assert_eq!(r.state(), WormStateName::Alerted);
    let q = p + Vec3::new(0.0, 30.0, 0.0);
    r.until(q, 0.2, |r| r.state() == WormStateName::Screaming);
    let t0 = r.tick;
    assert!(
        r.events
            .iter()
            .any(|(_, e)| matches!(e, NpcEvent::CameraShake { start: true, .. }))
    );
    // Pushes every 0.1 s: X/Y of unit(player − worm)·500, Z −50.
    for _ in 0..60 {
        r.step(q);
    }
    let pushes: Vec<_> = r
        .effects
        .iter()
        .filter(|(t, _)| *t >= t0)
        .map(|(_, e)| *e)
        .collect();
    assert!(
        (10..=11).contains(&pushes.len()),
        "{} pushes in 1 s",
        pushes.len()
    );
    let NpcEffect::PushPlayer { delta_v, worm } = pushes[0] else {
        panic!("push expected");
    };
    assert_eq!(worm, WORM);
    let dir = (q - Vec3::ZERO).normalize() * 500.0;
    assert!(
        (delta_v - Vec3::new(dir.x, dir.y, -50.0)).length() < 1e-3,
        "{delta_v}"
    );
    assert!(r.rt.worms[0].screaming);
}

#[test]
fn leaving_the_volume_stops_the_scream_and_sleeps_at_once() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    r.until(p + Vec3::Y * 30.0, 0.2, |r| {
        r.state() == WormStateName::Screaming
    });
    let n = r.worm_events().len();
    r.until(OUTSIDE, 0.2, |r| r.state() != WormStateName::Screaming);
    assert_eq!(r.state(), WormStateName::Sleeping, "same tick (state code)");
    let tail: Vec<_> = r.worm_events()[n..].iter().map(|e| e.1).collect();
    assert_eq!(
        tail,
        vec![
            WormEventKind::StoppedScreaming,
            WormEventKind::FallingAsleep
        ]
    );
    assert!(!r.rt.worms[0].screaming);
    assert!(
        !r.rt.worms[0].player_discovered,
        "Sleeping begin cleared it"
    );
    assert!(r.rt.worms[0].cancel_scream_timer.is_none());
    assert!(
        r.events
            .iter()
            .any(|(_, e)| matches!(e, NpcEvent::CameraShake { start: false, .. }))
    );
}

#[test]
fn screaming_for_scream_time_max_kills_the_player() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    let q = p + Vec3::Y * 30.0;
    r.until(q, 0.2, |r| r.state() == WormStateName::Screaming);
    let t0 = r.tick;
    let ticks = r.until(q, 9.0, |r| {
        r.effects
            .iter()
            .any(|(_, e)| matches!(e, NpcEffect::KillPlayer { .. }))
    });
    let t = Run::secs(ticks);
    assert!((7.95..=8.1).contains(&t), "killed after {t} s");
    let since: Vec<_> = r
        .worm_events()
        .into_iter()
        .filter(|(tick, _)| *tick > t0)
        .map(|e| e.1)
        .collect();
    assert_eq!(
        since,
        vec![
            WormEventKind::StoppedScreaming,
            WormEventKind::StoppedScreaming,
            WormEventKind::FallingAsleep
        ]
    );
    // From a timer: the state changed, its code runs next tick.
    assert_eq!(r.state(), WormStateName::Sleeping);
    let kills = r
        .effects
        .iter()
        .filter(|(_, e)| matches!(e, NpcEffect::KillPlayer { .. }))
        .count();
    assert_eq!(kills, 1);
}

#[test]
fn a_kill_zone_death_stops_the_scream() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    r.until(p + Vec3::Y * 30.0, 0.2, |r| {
        r.state() == WormStateName::Screaming
    });
    let mut out = NpcOutput::default();
    let scene = r.scene.clone();
    r.rt.notify_player_killed(&scene, &mut out);
    assert_eq!(r.state(), WormStateName::Sleeping);
    let kinds: Vec<_> = out
        .events
        .iter()
        .filter_map(|e| match e {
            NpcEvent::Worm { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            WormEventKind::StoppedScreaming,
            WormEventKind::FallingAsleep
        ]
    );
    // Outside `Screaming` a death changes nothing.
    let mut out = NpcOutput::default();
    r.rt.notify_player_killed(&scene, &mut out);
    assert!(out.events.is_empty());
}

#[test]
fn sleep_lasts_the_fade_plus_a_random_time_then_wakes() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    r.until(p + Vec3::Y * 30.0, 0.2, |r| {
        r.state() == WormStateName::Screaming
    });
    r.until(OUTSIDE, 0.2, |r| r.state() == WormStateName::Sleeping);
    let ticks = r.until(OUTSIDE, 20.0, |r| r.state() == WormStateName::WakingUp);
    let t = Run::secs(ticks);
    // fallAsleepTime 2 + RandRange(12, 14), frame-quantised.
    assert!((14.0..=16.3).contains(&t), "slept {t} s");
}

#[test]
fn shut_down_queues_sleep_and_prevents_waking() {
    let mut r = Run::new(5);
    r.start();
    r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    let scene = r.scene.clone();
    assert!(r.rt.shut_down_worm(&scene, WORM));
    assert!(r.rt.worms[0].sleep_is_queued);
    // The next "looked to the middle" animation end puts it to sleep.
    let ticks = r.until(OUTSIDE, 8.0, |r| r.state() == WormStateName::Sleeping);
    assert!(Run::secs(ticks) < 6.0, "slept after {} s", Run::secs(ticks));
    // Shut down: it never wakes again.
    for _ in 0..(40.0 / DT) as usize {
        r.step(OUTSIDE);
    }
    assert_eq!(r.state(), WormStateName::Sleeping);
    // Starting it again clears the shutdown.
    r.start();
    r.step(OUTSIDE);
    assert_eq!(r.state(), WormStateName::WakingUp);
    assert!(!r.rt.worms[0].shut_down);
}

/// Longest possible wait between the end of the awake checks and the sleep:
/// the longest side look, then the longest look back to the middle
/// (`bSleepIsQueued` forces the middle look, whose end sleeps), plus one check
/// step and tick rounding.
fn max_sleep_delay() -> f32 {
    WormAnim::LongLookRight.length() + WormAnim::RightToMiddle.length() + 0.1 + 3.0 * DT
}

#[test]
fn the_look_cycle_starts_with_a_short_look_and_sleeps_soon_after_the_awake_time() {
    for seed in 0..12u64 {
        let mut r = Run::new(seed);
        r.start();
        r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
        let awake_at = r.tick;
        // The middle list's first node: ShortLookleft (1.458 s).
        assert_eq!(r.rt.worms[0].anim.active(), WormAnim::ShortLookLeft);
        let ticks = r.until(OUTSIDE, 2.0, |r| r.rt.worms[0].anim.look_around != 1);
        let t = Run::secs(ticks);
        assert!(
            (WormAnim::ShortLookLeft.length() - DT..=WormAnim::ShortLookLeft.length() + 2.0 * DT)
                .contains(&t),
            "first look ended after {t} s"
        );
        assert_eq!(r.rt.worms[0].anim.look_around, 0, "looked left");
        // The next look (the left list's choice) was replayed from 0.
        let next = r.rt.worms[0].anim.active();
        assert!(
            matches!(next, WormAnim::LeftToMiddle | WormAnim::LongLookRight),
            "{next:?}"
        );
        assert!(r.rt.worms[0].anim.node(next).playing);
        let full = r.rt.worms[0].full_awake_time;
        // Undisturbed, it sleeps at the first middle look that ends after
        // the awake time ran out.
        r.until(OUTSIDE, 20.0, |r| r.state() == WormStateName::Sleeping);
        let slept = Run::secs(r.tick - awake_at);
        assert!(slept >= full, "seed {seed}: slept at {slept} < {full}");
        assert!(
            slept <= full + max_sleep_delay(),
            "seed {seed}: slept at {slept}, awake time {full}"
        );
    }
}

#[test]
fn an_awake_worm_never_stalls_its_look_chain() {
    // A worm that stays awake for a long time keeps looking around: every
    // selection replays the chosen look node (`bPlayActiveChild`), also one
    // that already played this awake period.
    let mut scene = scene();
    scene.worms[0].params.awake_time_min = 1000.0;
    scene.worms[0].params.awake_time_max = 1000.0;
    for seed in 0..20u64 {
        let mut rt = NpcRuntime::new(&scene, seed, false);
        let mut out = NpcOutput::default();
        assert!(rt.start_worm(&scene, WORM, &mut out));
        let mut switches = 0;
        let mut last = rt.worms[0].anim.active();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..(120.0 / DT) as usize {
            rt.tick_actors(&scene, OUTSIDE, DT, &mut out);
            let w = &rt.worms[0];
            let a = w.anim.active();
            if a != last {
                switches += 1;
                last = a;
            }
            if w.state == WormStateName::Awake {
                seen.insert(a);
                assert!(w.anim.node(a).playing, "seed {seed}: {a:?} stalled");
            }
        }
        assert_eq!(rt.worms[0].state, WormStateName::Awake, "seed {seed}");
        // ~116 s of looks of at most 2.5 s each.
        assert!(switches >= 46, "seed {seed}: {switches} switches");
        assert!(seen.len() >= 5, "seed {seed}: {seen:?}");
    }
}

#[test]
fn with_every_look_node_stopped_nothing_ends_and_the_lists_stay() {
    // Engine rule: only a node that plays reaches its end; a stopped node
    // raises no `OnAnimEnd`, so nothing switches.
    let mut r = Run::new(6);
    r.start();
    r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
    for n in &mut r.rt.worms[0].anim.nodes {
        n.playing = false;
    }
    let before = r.rt.worms[0].anim.clone();
    for _ in 0..(20.0 / DT) as usize {
        r.step(OUTSIDE);
    }
    let w = &r.rt.worms[0];
    assert_eq!(
        w.anim.look_around, before.look_around,
        "no anim end, no switch"
    );
    assert_eq!(w.state, WormStateName::Awake);
    assert!(
        w.sleep_is_queued,
        "the check loop ended after the awake time"
    );
}

#[test]
fn a_shut_down_awake_worm_sleeps_at_the_next_middle_look() {
    for seed in 0..20u64 {
        let mut r = Run::new(seed);
        r.start();
        r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
        // Shut down half a second into the first look.
        for _ in 0..30 {
            r.step(OUTSIDE);
        }
        let scene = r.scene.clone();
        assert!(r.rt.shut_down_worm(&scene, WORM));
        let ticks = r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Sleeping);
        // The rest of ShortLookleft, then LeftToMiddle (the queue forces it).
        let expect = WormAnim::ShortLookLeft.length() - 30.0 * DT + WormAnim::LeftToMiddle.length();
        let t = Run::secs(ticks);
        assert!(
            (t - expect).abs() <= 3.0 * DT,
            "seed {seed}: slept after {t} s, expected {expect}"
        );
    }
}

#[test]
fn screaming_replays_the_scream_animation() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    let q = p + Vec3::new(0.0, 30.0, 0.0);
    r.until(q, 0.2, |r| r.state() == WormStateName::Screaming);
    let w = &r.rt.worms[0];
    assert_eq!(w.anim.active(), WormAnim::Scream);
    let n = w.anim.node(WormAnim::Scream);
    assert!(n.playing && !n.looping, "{n:?}");
    assert!(n.position <= DT, "replayed from 0: {n:?}");
}

#[test]
fn the_aim_follows_the_player_only_when_discovered() {
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    let before = r.rt.worms[0].current_aim;
    for _ in 0..60 {
        r.step(p);
    }
    let after = r.rt.worms[0].current_aim;
    assert!(
        (after - p).length() < (before - p).length(),
        "turns towards the player"
    );
}

#[test]
fn worm_runs_are_deterministic() {
    let run = || {
        let mut r = Run::new(99);
        r.start();
        for i in 0..(60.0 / DT) as usize {
            let wobble = Vec3::new((i / 90 % 3) as f32 * 40.0, 0.0, 0.0);
            let p = if (i / 600) % 2 == 0 { INSIDE } else { OUTSIDE };
            r.step(p + wobble);
        }
        (format!("{:?}", r.events), format!("{:?}", r.effects))
    };
    assert_eq!(run(), run());
}

#[test]
fn hostile_dt_and_positions_are_ignored() {
    let mut r = Run::new(7);
    r.start();
    let scene = r.scene.clone();
    let mut out = NpcOutput::default();
    for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        r.rt.tick_actors(&scene, INSIDE, dt, &mut out);
    }
    r.rt.tick_actors(&scene, Vec3::NAN, DT, &mut out);
    assert!(out.events.is_empty() && out.effects.is_empty());
    assert_eq!(r.state(), WormStateName::Idle);
    assert!(!r.rt.start_worm(&scene, 12345, &mut out), "unknown worm");
}

#[test]
fn look_cycle_outcomes_over_seeds() {
    // Statistic for NPCS.md: when an undisturbed worm falls asleep after
    // waking (our random stream; the distribution, not the per-seed outcome,
    // is the point). Every seed must sleep: the look chain never stalls.
    let mut sleep_after_awake_time = Vec::new();
    let mut awake_spans = Vec::new();
    for seed in 0..200u64 {
        let mut r = Run::new(seed);
        r.start();
        r.until(OUTSIDE, 6.0, |r| r.state() == WormStateName::Awake);
        let at = r.tick;
        let full = r.rt.worms[0].full_awake_time;
        r.until(OUTSIDE, 20.0, |r| r.state() == WormStateName::Sleeping);
        let span = Run::secs(r.tick - at);
        assert!(
            span >= full && span <= full + max_sleep_delay(),
            "seed {seed}: {span}"
        );
        awake_spans.push(span);
        sleep_after_awake_time.push(span - full);
    }
    let n = awake_spans.len() as f32;
    let mean = awake_spans.iter().sum::<f32>() / n;
    let extra = sleep_after_awake_time.iter().sum::<f32>() / n;
    let max = sleep_after_awake_time
        .iter()
        .copied()
        .fold(0.0f32, f32::max);
    println!(
        "look cycle over 200 seeds: all slept; awake for {mean:.2} s on average ({extra:.2} s after the awake time, at most {max:.2} s)"
    );
    assert_eq!(awake_spans.len(), 200);
}

#[test]
fn a_kismet_restart_mid_scream_keeps_the_scream_timer() {
    // Only `Sleeping` clears `CancelScreamTimer`: a `SeqAct_StartWorm` during
    // the scream restarts the cycle, and the old timer still fires
    // `screamTimeMax` after the scream began. Its `NotifyKilled` then finds
    // no screaming worm, so the new cycle goes on.
    let mut r = alerted();
    let p = INSIDE + Vec3::new(40.0, 0.0, 0.0);
    for _ in 0..70 {
        r.step(p);
    }
    let q = p + Vec3::new(0.0, 30.0, 0.0);
    r.until(q, 0.2, |r| r.state() == WormStateName::Screaming);
    let t0 = r.tick;
    r.start();
    r.step(OUTSIDE);
    assert_eq!(r.state(), WormStateName::WakingUp);
    assert!(r.rt.worms[0].cancel_scream_timer.is_some());
    let killed = |r: &Run| {
        r.effects
            .iter()
            .any(|(t, e)| *t > t0 && matches!(e, NpcEffect::KillPlayer { .. }))
    };
    r.until(OUTSIDE, 9.0, killed);
    let t = Run::secs(r.tick - t0);
    assert!(
        (7.95..=8.1).contains(&t),
        "killed {t} s after the scream began"
    );
    assert_eq!(
        r.state(),
        WormStateName::Awake,
        "the new cycle is untouched"
    );
}
