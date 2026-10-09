//! World-object state machines against `docs/reverse-engineering/GRAPPLE.md`
//! §12 (G-WO-1/2/6), G-AC-0 and `ABILITIES.md` §13 (attractor). Synthetic
//! levels only.

use asamu_world::objects::{
    ATTRACTOR_STEP, CRYSTAL_FADE_STEP_SLEEP, CrystalStateName,
    INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES,
};
use asamu_world::{
    Attractor, Interactable, Level, LevelAbilities, ObjectEvent, RechargeCrystal, WorldEvent,
    WorldObjects, graybox_test_level,
};
use glam::Vec3;

const DT: f32 = 1.0 / 60.0;

fn crystal(
    id: u32,
    parent_crystal: bool,
    linked_parent: Option<u32>,
    should_recharge: bool,
) -> RechargeCrystal {
    RechargeCrystal {
        id,
        center: Vec3::new(id as f32 * 200.0, 0.0, 500.0),
        half_extent: 30.0,
        recharge_delay: 10.0,
        should_recharge,
        parent_crystal,
        linked_parent,
    }
}

fn level_with(crystals: Vec<RechargeCrystal>) -> Level {
    let mut level = graybox_test_level();
    level.crystals = crystals;
    level.flowers.clear();
    level.movers.clear();
    level.interactables.clear();
    level.attractors.clear();
    level
}

fn tick(o: &mut WorldObjects, level: &Level, n: usize, events: &mut Vec<WorldEvent>) {
    let mut v = Vec3::ZERO;
    for _ in 0..n {
        o.tick(level, DT, 0.0, Vec3::ZERO, &mut v, events);
    }
}

#[test]
fn g_wo_1_crystal_cycle_charged_grappled_uncharged_recharged() {
    let level = level_with(vec![crystal(1, false, None, true)]);
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    assert!(o.crystal_charged(&level, 1));
    // Grappled while charged → `GrappledState` (the budget refill is the
    // gun's); not charged any more for the next grapple.
    o.apply(&level, ObjectEvent::Grappled(1), &mut ev);
    assert_eq!(o.crystals[0].state, CrystalStateName::Grappled);
    assert!(!o.crystal_charged(&level, 1));
    // `UnGrappled` (the 0.05 s instant release) → `UnCharged`.
    o.apply(&level, ObjectEvent::UnGrappled(1), &mut ev);
    assert_eq!(ev, vec![WorldEvent::CrystalUncharged { id: 1 }]);
    // 21 fade steps of one frame each (Sleep(0.005) < half a 60 Hz tick),
    // then RechargeDelay (10 s), then `Charged` again.
    const { assert!(CRYSTAL_FADE_STEP_SLEEP < DT * 0.5) };
    let mut recharged_at = None;
    for t in 1..=700 {
        let mut e = Vec::new();
        tick(&mut o, &level, 1, &mut e);
        if e.contains(&WorldEvent::CrystalRecharged { id: 1 }) {
            recharged_at = Some(t);
            break;
        }
    }
    // Begin in tick 1 (step 0), steps 1..20 in ticks 2..21, the loop ends
    // and Sleep(10) starts in tick 22, which wakes 600 ticks later.
    let mut remaining = 10.0_f32;
    let mut sleep_ticks = 0;
    loop {
        sleep_ticks += 1;
        remaining -= DT;
        if f64::from(remaining) < 0.5 * f64::from(DT) {
            break;
        }
    }
    assert_eq!(recharged_at, Some(22 + sleep_ticks));
    assert!(o.crystal_charged(&level, 1));
}

#[test]
fn g_wo_1_uncharged_crystals_ignore_grapples_and_bshouldrecharge_false_stays_dark() {
    let level = level_with(vec![crystal(1, false, None, false)]);
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(1), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(1), &mut ev);
    tick(&mut o, &level, 2000, &mut ev);
    assert_eq!(o.crystals[0].state, CrystalStateName::UnCharged);
    // A grapple of an uncharged crystal does not enter `GrappledState`.
    o.apply(&level, ObjectEvent::Grappled(1), &mut ev);
    assert_eq!(o.crystals[0].state, CrystalStateName::UnCharged);
    assert!(!o.crystal_charged(&level, 1));
}

#[test]
fn g_wo_1_family_child_uncharges_its_parent_and_siblings() {
    let level = level_with(vec![
        crystal(10, true, None, true),
        crystal(11, false, Some(10), true),
        crystal(12, false, Some(10), true),
        crystal(13, false, None, true),
    ]);
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(11), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(11), &mut ev);
    for id in [10, 11, 12] {
        assert!(!o.crystal_charged(&level, id), "{id}");
        assert!(ev.contains(&WorldEvent::CrystalUncharged { id }));
    }
    assert!(o.crystal_charged(&level, 13), "unrelated crystal untouched");
    // A parent grappled directly uncharges its children too.
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(10), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(10), &mut ev);
    for id in [10, 11, 12] {
        assert!(!o.crystal_charged(&level, id), "{id}");
    }
}

/// Ticks until crystal `id` reports `CrystalRecharged`; returns the tick
/// count (1-based) or `None` within `limit` ticks.
fn ticks_until_recharged(
    o: &mut WorldObjects,
    level: &Level,
    id: u32,
    limit: usize,
) -> Option<usize> {
    for t in 1..=limit {
        let mut e = Vec::new();
        tick(o, level, 1, &mut e);
        if e.contains(&WorldEvent::CrystalRecharged { id }) {
            return Some(t);
        }
    }
    None
}

#[test]
fn g_wo_1_uncharging_an_already_uncharged_crystal_restarts_its_fade_and_recharge() {
    // UE3 `GotoState` to the current state skips the state change but jumps
    // to `Begin` again (latent sleep dropped), so a family member grappled
    // later restarts a sibling's or parent's 21-step fade and its whole
    // `RechargeDelay`. Child 2 recharges after 5 s, the parent after 10 s;
    // grappling child 2 again at 6 s pushes the parent's recharge to
    // 6 s + fade + 10 s, and no second `CrystalUncharged` is reported for it.
    let mut parent = crystal(1, true, None, true);
    parent.recharge_delay = 10.0;
    let mut child = crystal(2, false, Some(1), true);
    child.recharge_delay = 5.0;
    let level = level_with(vec![parent, child]);
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(1), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(1), &mut ev);
    assert!(!o.crystal_charged(&level, 1) && !o.crystal_charged(&level, 2));
    // Reference: an undisturbed parent recharges after the fade + 10 s.
    let mut reference = o.clone();
    let parent_alone = ticks_until_recharged(&mut reference, &level, 1, 800).unwrap();
    // The child recharges first (fade + 5 s).
    let child_back = ticks_until_recharged(&mut o, &level, 2, 800).unwrap();
    assert!(child_back < parent_alone);
    tick(&mut o, &level, 30, &mut ev);
    let elapsed = child_back + 30;
    let mut ev2 = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(2), &mut ev2);
    o.apply(&level, ObjectEvent::UnGrappled(2), &mut ev2);
    assert_eq!(
        ev2,
        vec![WorldEvent::CrystalUncharged { id: 2 }],
        "the parent was already uncharged: restarted, not reported again"
    );
    let parent_back = ticks_until_recharged(&mut o, &level, 1, 1200).unwrap();
    assert_eq!(
        elapsed + parent_back,
        elapsed + parent_alone,
        "the parent's whole fade + recharge restarted at the second grapple"
    );
}

#[test]
fn g_wo_1_a_child_linked_to_a_non_parent_crystal_still_notifies_it() {
    // `AddChildCrystal` and `NotifyGrappled` look only at the child's own
    // `bParentCrystal` flag and its link, never at the linked crystal's
    // flag: the linked crystal and every child linked to it lose their
    // charge; a crystal without a link (and not a parent) only uncharges
    // itself.
    let level = level_with(vec![
        crystal(20, false, None, true),
        crystal(21, false, Some(20), true),
        crystal(22, false, Some(20), true),
    ]);
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    o.apply(&level, ObjectEvent::Grappled(22), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(22), &mut ev);
    for id in [20, 21, 22] {
        assert!(!o.crystal_charged(&level, id), "{id}");
    }
    // The head itself (no parent flag, no link) grappled: only itself.
    let mut o = WorldObjects::new(&level);
    o.apply(&level, ObjectEvent::Grappled(20), &mut ev);
    o.apply(&level, ObjectEvent::UnGrappled(20), &mut ev);
    assert!(!o.crystal_charged(&level, 20));
    assert!(o.crystal_charged(&level, 21) && o.crystal_charged(&level, 22));
}

#[test]
fn g_ac_0_interactables_count_uses_against_max_interact_times() {
    let mut level = level_with(Vec::new());
    level.interactables = vec![
        Interactable {
            id: 1,
            min: Vec3::ZERO,
            max: Vec3::ONE,
            max_interact_times: INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES,
            label: "once".into(),
        },
        Interactable {
            id: 2,
            min: Vec3::ZERO,
            max: Vec3::ONE,
            max_interact_times: 0,
            label: "unlimited".into(),
        },
    ];
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    for _ in 0..3 {
        o.apply(&level, ObjectEvent::InteractWith(1), &mut ev);
        o.apply(&level, ObjectEvent::InteractWith(2), &mut ev);
    }
    let count = |id| {
        ev.iter()
            .filter(|e| **e == WorldEvent::ActorInteractedWith { id })
            .count()
    };
    assert_eq!(count(1), 1, "MaxInteractTimes 1 (class default)");
    assert_eq!(count(2), 3, "0 = unlimited");
}

#[test]
fn attractor_writes_every_0_05_s_with_a_ramp_and_never_stops() {
    // ABILITIES.md §13: once activated, every 0.05 s V += −(t/D)·unit(P −
    // pad)·((R/|P − pad|)·S·(1 − b) + S·b), t += 0.05 up to D.
    let mut level = level_with(Vec::new());
    let pad = Attractor::as_placed(5, Vec3::ZERO);
    level.attractors = vec![pad.clone()];
    let mut o = WorldObjects::new(&level);
    let mut ev = Vec::new();
    let pawn = Vec3::new(500.0, 0.0, 0.0);
    let mut v = Vec3::ZERO;
    o.tick(&level, DT, 0.0, pawn, &mut v, &mut ev);
    assert_eq!(v, Vec3::ZERO, "inactive until Kismet activates it");
    assert!(o.activate_attractor(&level, 5));
    assert!(!o.activate_attractor(&level, 99));
    let mut writes = Vec::new();
    for t in 0..400 {
        let before = v;
        o.tick(&level, DT, 0.0, pawn, &mut v, &mut ev);
        if v != before {
            writes.push(t);
        }
    }
    // First write at t = 0 adds nothing; then every 3 ticks (Sleep(0.05)
    // at 60 Hz) a write with ramp 0.05, 0.10, …
    assert_eq!(writes.first(), Some(&3));
    assert!(writes.windows(2).all(|w| w[1] - w[0] == 3), "{writes:?}");
    // Accumulated: Σ ramp_k · 390·…: check the first non-zero step exactly.
    let mut o2 = WorldObjects::new(&level);
    o2.activate_attractor(&level, 5);
    let mut v2 = Vec3::ZERO;
    for _ in 0..4 {
        o2.tick(&level, DT, 0.0, pawn, &mut v2, &mut ev);
    }
    let expected = pad.velocity_change(ATTRACTOR_STEP, pawn).unwrap();
    assert!((v2 - expected).length() < 1e-4, "{v2} vs {expected}");
    assert!(v2.x < 0.0, "toward the pad");
}

#[test]
fn placed_attractor_values_and_graybox_abilities() {
    let a = Attractor::as_placed(1, Vec3::ZERO);
    assert_eq!(
        (
            a.range,
            a.strength,
            a.velocity_base_amount,
            a.attract_duration
        ),
        (1000.0, 200.0, 0.05, 10.0)
    );
    let g = graybox_test_level();
    assert_eq!(g.abilities, LevelAbilities::graybox_test());
    assert!(g.validate().is_ok());
    assert!(!g.crystals.is_empty() && !g.flowers.is_empty() && !g.movers.is_empty());
    assert!(!g.interactables.is_empty() && !g.attractors.is_empty());
    // Duplicate object ids are rejected.
    let mut bad = g.clone();
    bad.flowers[0].id = bad.crystals[0].id;
    assert!(bad.validate().is_err());
}

#[test]
fn movers_move_on_their_path_and_report_their_location() {
    let level = graybox_test_level();
    let mut o = WorldObjects::new(&level);
    let id = level.movers[0].id;
    let rest = level.mover_location(&o, id).unwrap();
    let mut v = Vec3::ZERO;
    let mut ev = Vec::new();
    o.tick(&level, DT, 1.0, Vec3::ZERO, &mut v, &mut ev);
    let moved = level.mover_location(&o, id).unwrap();
    assert!((moved - rest - level.movers[0].path.offset_at(1.0)).length() < 1e-4);
    let prims = level.collision_primitives(Some(&o));
    let mover_prim = prims.iter().find(|p| p.actor == Some(id)).unwrap();
    assert!((mover_prim.min - (level.movers[0].min + o.mover_offsets[0])).length() < 1e-4);
}
