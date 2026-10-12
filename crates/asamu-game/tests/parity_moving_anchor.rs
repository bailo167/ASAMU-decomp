//! The grapple's anchor on the rock wheels of AG-ParadiseCave: the anchor
//! rides the rock it was fired at (GRAPPLE.md G-AT-7, G-AT-8, G-WO-6). Skips
//! without `ASAMU_CONVERTED_DIR` (a conversion with simple collision, Kismet
//! and Matinee).
//!
//! The level has three kinds of moving rock (level data, LEVEL_FORMAT.md
//! "Actor fields"; the motion rules are in `asamu_game::kismet_host`,
//! "Turning actors", and KISMET_RUNTIME.md):
//!
//! - a rock that a Matinee movement track turns about its own location (the
//!   rock's mesh sits at the end of an arm, its `PrePivot`): `InterpActor_64`,
//!   `PHYS_Interpolating`, the track's roll goes from 0 to 359.9 degrees in
//!   7.2 s;
//! - a rock the engine turns by itself: `InterpActor_51`, `PHYS_Rotating`,
//!   `RotationRate` yaw −9102 units/s, bound to no Matinee; and
//!   `InterpActor_25`, `PHYS_Rotating`, roll −4551 units/s, which a Matinee
//!   group names but does not move;
//! - a rock attached to such an actor: `InterpActor_122`, hard-attached to
//!   the turning `InterpActor_16` (yaw −2184 units/s).
//!
//! What is asserted is the rule, never a number taken from a run: the
//! anchor's place is the rigid motion of the rock's base applied to the
//! place the grapple hit, computed here in `f64` from the base's transforms;
//! the base's own turn is the level's rate times the time that passed, in
//! whole units (the designed rate, which the game plays with: `kismet_host`,
//! "Turning actors"), or, with the original's rule selected, the rate times
//! the tick with the fraction dropped, every tick.

use std::path::PathBuf;

use asamu_game::asamu_world::scene::PlacedPhysics;
use asamu_game::kismet_host::TurnRule;
use asamu_game::{Game, LevelScript, load_level_with_kismet};
use asamu_player::InputFrame;
use asamu_player::world::{ActorClass, CollisionWorld};
use glam::{DVec3, Vec3};

const MAP: &str = "AG-ParadiseCave";

/// Half sizes of the pawn's box.
const BOX: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// Ticks a grapple is held for. At 60 Hz that is a third of a second: the
/// fastest wheel turns 0.29 rad in it, and a pawn that starts
/// [`STAND_OFF`] from the rock is still further than the release distance
/// (200 uu) from it, since it cannot fly faster than 2000 uu/s (667 uu in
/// that time).
const HOLD_TICKS: usize = 20;

/// How far from the rock's bounds the pawn is put, uu.
const STAND_OFF: f32 = 1200.0;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let version_2 = std::fs::read_to_string(d.join("meshes").join("manifest.json"))
        .ok()
        .is_some_and(|m| m.contains("simple_collision"));
    (d.join("levels").is_dir() && d.join("kismet").is_dir() && version_2).then_some(d)
}

fn load() -> Option<(Game, LevelScript)> {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion with Kismet) not set");
        return None;
    };
    let Ok((mut game, script)) = load_level_with_kismet(&dir, MAP) else {
        eprintln!("SKIP: {MAP} is not converted");
        return None;
    };
    let script = script.unwrap_or_else(|| panic!("{MAP} has Kismet"));
    game.start();
    Some((game, script))
}

/// The rows of the rotation matrix of a rotator (pitch, yaw, roll): the
/// world images of the local X, Y and Z axes. Each angle is floored to 4
/// units first, as the original's sine table does (`(angle >> 2) & 0x3FFF`,
/// 16 384 entries per turn); the sines are exact here.
fn rows(rotation: [i32; 3]) -> [DVec3; 3] {
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
}

/// Where the point `p` that rides a base rigidly is after the base went
/// from the pose `from` to the pose `to` (location, rotation): its offset
/// from the base's location keeps its coordinates in the base's frame.
fn rigid(p: DVec3, from: (Vec3, [i32; 3]), to: (Vec3, [i32; 3])) -> DVec3 {
    let offset = p - from.0.as_dvec3();
    let old = rows(from.1);
    let new = rows(to.1);
    let local = DVec3::new(offset.dot(old[0]), offset.dot(old[1]), offset.dot(old[2]));
    to.0.as_dvec3() + new[0] * local.x + new[1] * local.y + new[2] * local.z
}

/// Puts the pawn in free space [`STAND_OFF`] from the bounds of the actor
/// `target`'s collision, looking at it along a line on which nothing else
/// is in the way, at rest and falling. Returns the place its view meets the
/// actor.
fn stand_before(game: &mut Game, target: u32) -> Vec3 {
    let (centre, reach) = {
        let sc = game.world().scene.as_ref().expect("a converted level");
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for inst in &sc.dynamic {
            if inst.info.actor == Some(target) && inst.info.blocks_traces && inst.enabled {
                min = min.min(inst.bounds.min);
                max = max.max(inst.bounds.max);
            }
        }
        assert!(
            min.is_finite() && max.is_finite(),
            "actor {target} has collision that blocks the grapple's trace"
        );
        ((min + max) * 0.5, (max - min).length() * 0.5)
    };
    let eye_offset = game.player().view_location(game.params()) - game.player().position;
    let range = reach + STAND_OFF;
    let mut directions = Vec::new();
    for x in [-1.0_f32, 0.0, 1.0] {
        for y in [-1.0_f32, 0.0, 1.0] {
            for z in [-1.0_f32, 0.0, 1.0] {
                let d = Vec3::new(x, y, z);
                if d != Vec3::ZERO {
                    directions.push(d.normalize());
                }
            }
        }
    }
    for d in directions {
        let eye = centre + d * range;
        let position = eye - eye_offset;
        if game.world().overlaps_box(position, BOX) {
            continue;
        }
        let Some(hit) = game.world().trace_line(eye, -d, range) else {
            continue;
        };
        if hit.hit.surface.actor != Some(target) {
            continue;
        }
        let at = hit.hit.position;
        let aim = (at - eye).normalize();
        let p = game.player_mut();
        p.position = position;
        p.velocity = Vec3::ZERO;
        p.grounded = false;
        p.pawn.based = false;
        p.yaw = aim.y.atan2(aim.x);
        p.pitch = aim.z.clamp(-1.0, 1.0).asin();
        return at;
    }
    panic!("no free line of sight to actor {target} from {range} uu");
}

/// Fires the grapple at the actor `target` and holds it for
/// [`HOLD_TICKS`]: the anchor must ride the actor `base` rigidly (`base` is
/// `target` itself, or the actor `target` is hard-attached to).
///
/// Returns the poses of `base` at the attach and after the last tick, and
/// the anchor at those two times.
#[allow(clippy::type_complexity)]
fn grapple_and_ride(
    game: &mut Game,
    script: &mut LevelScript,
    target: u32,
    base: u32,
    tolerance: f64,
) -> ((Vec3, [i32; 3]), (Vec3, [i32; 3]), DVec3, DVec3) {
    // The level's script decides how many grapples there are; this test is
    // not about the budget.
    game.set_max_grapples(3);
    game.enable_grapple(true);
    stand_before(game, target);
    // What the fire trace will meet: the rock, as an `InterpActor` with its
    // id (without either the anchor would not follow, G-AT-7).
    let aim = game.gun_aim().expect("the pawn has the gun");
    let hit = aim.impact.hit.expect("the view meets the rock");
    assert_eq!(hit.surface.actor, Some(target), "{:?}", hit.surface);
    assert_eq!(hit.surface.class, ActorClass::InterpActor);
    assert!(aim.acceptable, "in range and grapple-able: {aim:?}");

    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let t = script.tick(game, &fire).expect("the game is playing");
    let attached = t.report.events.gun.attached.expect("the grapple attaches");
    assert_eq!(attached.surface.actor, Some(target));
    let follow = game.player().script.gun.follow.expect("the anchor follows");
    assert_eq!(follow.actor, target);
    // The base did not move between the attach (an input event) and the end
    // of the tick: the level's movers move before the input events.
    let base0 = script.mover_transform(base).expect("the base is a mover");
    let anchor0 = game.player().script.gun.grapple_location.as_dvec3();
    let mut last = (base0, anchor0);
    let mut furthest = 0.0_f64;
    for tick in 1..=HOLD_TICKS {
        script.tick(game, &fire).expect("the game is playing");
        let gun = game.player().script.gun;
        assert!(
            gun.is_attached(),
            "released in tick {tick} at {} uu",
            gun.distance
        );
        let pose = script.mover_transform(base).expect("the base is a mover");
        let anchor = gun.grapple_location.as_dvec3();
        // G-AT-8: the anchor is the helper's location, and the helper rides
        // its base rigidly.
        let expected = rigid(anchor0, base0, pose);
        furthest = furthest.max((anchor - expected).length());
        assert!(
            (anchor - expected).length() <= tolerance,
            "tick {tick}: the anchor is at {anchor}, the rock took its place to {expected} \
             ({} uu apart; it started at {anchor0})",
            (anchor - expected).length()
        );
        // The pull's distance is measured to the anchor where it is now
        // (G-PH-2): the gun ticks after the pawn, so it is the distance
        // between where the tick leaves the two.
        let d = (game.player().position - gun.grapple_location).length();
        assert!(
            (gun.distance - d).abs() <= 1e-3 * d.max(1.0),
            "tick {tick}: measured {} uu, the anchor is {d} uu away",
            gun.distance
        );
        last = (pose, anchor);
    }
    // Not asserted beyond the tolerance above: how close ours stays.
    eprintln!(
        "actor {target} (base {base}): the anchor went {:.1} uu in {HOLD_TICKS} ticks, at most \
         {furthest:.4} uu from its base's rigid motion; the pawn ended {:.0} uu from it",
        (last.1 - anchor0).length(),
        game.player().script.gun.distance
    );
    (base0, last.0, anchor0, last.1)
}

/// Rounding room for an anchor that is carried once per tick in `f32`: at
/// this level's coordinates (up to 262 144 uu) an `f32` step is 1/64 uu,
/// and a carry rounds a handful of times (the offset, three products, two
/// sums: at most 6 half-steps), so [`HOLD_TICKS`] carries stay within
/// 20 × 6 / 128 = 0.94 uu. The exact sines here against the table's
/// single-precision entries add less than 0.01 uu on a 5000 uu arm.
const CARRY_TOLERANCE: f64 = 1.0;

#[test]
fn the_anchor_rides_a_rock_a_matinee_turns() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    let idle = InputFrame::default();
    let path = format!("{MAP}.TheWorld.PersistentLevel.InterpActor_64");
    let hub = script
        .actor_id_by_path(&path)
        .expect("the level's script knows the rock");
    let (placed_at, placed_rotation) = script.actor_placement(hub).expect("placed");
    // The level-loaded events start the wheels' Matinees: run until the
    // rock has turned.
    for _ in 0..30 {
        script.tick(&mut game, &idle).expect("the game is playing");
    }
    let (at, rotation) = script.mover_transform(hub).expect("the rock is a mover");
    assert_eq!(at, placed_at, "the rock turns about its own location");
    assert_ne!(rotation, placed_rotation, "the wheel turns");

    let (from, to, anchor0, anchor1) =
        grapple_and_ride(&mut game, &mut script, hub, hub, CARRY_TOLERANCE);
    // The rock's own turn, from the level's Matinee data: the track's roll
    // goes linearly from 0 to 359.9 degrees in 7.2 s, relative to the rock's
    // placement, so a tick of 1/60 s adds 359.9 / 360 × 65 536 / 7.2 / 60 =
    // 151.66 units of roll, and HOLD_TICKS ticks 3033.2. The rotation is
    // rebuilt from a matrix every tick: each member within a unit, twice.
    let per_tick = 359.9 / 360.0 * 65_536.0 / 7.2 / 60.0;
    let turned = f64::from(to.1[2].wrapping_sub(from.1[2]));
    assert!(
        (turned - per_tick * HOLD_TICKS as f64).abs() <= 2.0,
        "the rock's roll went from {} to {}: {turned} units in {HOLD_TICKS} ticks",
        from.1[2],
        to.1[2]
    );
    assert_eq!(from.0, to.0, "the rock's location does not move");
    // So the anchor moved: by the chord of that turn on its circle about the
    // rock's roll axis (the local X axis). 3033 units are 0.2908 rad.
    let axis = rows(from.1)[0];
    let arm = anchor0 - from.0.as_dvec3();
    let radius = (arm - axis * arm.dot(axis)).length();
    let angle = per_tick * HOLD_TICKS as f64 * std::f64::consts::TAU / 65_536.0;
    let chord = 2.0 * radius * (angle / 2.0).sin();
    let moved = (anchor1 - anchor0).length();
    // Two units of roll on the arm, and the carries' rounding.
    let room = radius * 2.0 * std::f64::consts::TAU / 65_536.0 + CARRY_TOLERANCE;
    assert!(
        (moved - chord).abs() <= room,
        "the anchor moved {moved} uu on an arm of {radius} uu; the wheel's turn is {chord} uu"
    );
    assert!(
        chord > 100.0,
        "the test is about a rock that moves: {chord}"
    );
}

// --- The rocks the engine turns by itself.

/// A tick's turn of an actor with the rate `rate` (units/s) by the
/// original's rule ([`TurnRule::Native`]): the rate times the tick's time in
/// single precision, the fraction dropped towards zero. With the 60 Hz tick:
/// 9102 / 60 = 151.7, 151 units; 4551 / 60 = 75.85, 75; 2184 / 60 = 36.4, 36.
fn step(rate: i32) -> i32 {
    (rate as f32 * (1.0_f32 / 60.0)) as i32
}

/// What an actor with the rate `rate` (units/s) has turned by after `ticks`
/// ticks of the 60 Hz game at its designed rate
/// ([`TurnRule::DesignedRate`], what the game plays with): the truncation,
/// towards zero, of the rate times the time that passed. The tick is the
/// `f32` nearest 1/60 s, 8 947 849 / 2^29 (28 parts in 2^29 longer than
/// 1/60 s); in whole numbers, so that nothing rounds here.
fn designed_turn(rate: i32, ticks: i32) -> i32 {
    let turn = i128::from(rate) * i128::from(ticks) * 8_947_849 / 536_870_912;
    i32::try_from(turn).expect("a turn of these tests fits a rotator's member")
}

#[test]
fn the_steps_of_the_level_s_rates() {
    assert_eq!(step(-9102), -151);
    assert_eq!(step(-4551), -75);
    assert_eq!(step(-2184), -36);
}

#[test]
fn the_designed_turns_of_the_level_s_rates() {
    // The tick [`designed_turn`] counts with is the game's: the bits of the
    // `f32` nearest 1/60 s are the exponent 121 (2^-6, so the 24 bits count
    // in steps of 2^-29) and the 24 bits 8 947 849.
    let bits = (1.0_f32 / 60.0).to_bits();
    assert_eq!(bits >> 23, 121);
    assert_eq!((bits & 0x7F_FFFF) | 0x80_0000, 8_947_849);
    // 90 ticks, a second and a half: 9102 × 1.5 = 13 653, 4551 × 1.5 =
    // 6826.5 and 2184 × 1.5 = 3276 units (the `f32` tick adds less than a
    // thousandth of a unit), where 90 truncated steps are 13 590, 6750 and
    // 3240.
    assert_eq!(designed_turn(-9102, 90), -13_653);
    assert_eq!(designed_turn(-4551, 90), -6826);
    assert_eq!(designed_turn(-2184, 90), -3276);
    assert_eq!(
        [90 * step(-9102), 90 * step(-4551), 90 * step(-2184)],
        [-13_590, -6750, -3240]
    );
    // The ticks of the anchor tests: 31 × 151.7 = 4702.7, 51 × 151.7 =
    // 7736.7; 30 × 36.4 = 1092, 31 × 36.4 = 1128.4, 51 × 36.4 = 1856.4.
    assert_eq!(designed_turn(-9102, 31), -4702);
    assert_eq!(designed_turn(-9102, 51), -7736);
    assert_eq!(designed_turn(-2184, 30), -1092);
    assert_eq!(designed_turn(-2184, 31), -1128);
    assert_eq!(designed_turn(-2184, 51), -1856);
}

/// The id, placement and rate of the actor named `name`, which the level
/// places in `PHYS_Rotating`.
fn turning(game: &Game, name: &str, rate: [i32; 3]) -> (u32, Vec3, [i32; 3]) {
    let map = game.scene_map().expect("a converted level");
    let a = map
        .placed_actor_named(name)
        .unwrap_or_else(|| panic!("{name} is placed"));
    assert_eq!(a.physics, PlacedPhysics::Rotating, "{name}");
    assert_eq!(a.rotation_rate, rate, "{name}: the level's RotationRate");
    assert_eq!(a.base, None, "{name}");
    (a.id, a.location, a.rotation)
}

/// How many actors each story level places in `PHYS_Rotating` with a
/// `RotationRate` (a census of the level data: every one is an
/// `InterpActor` without a base).
#[test]
fn the_turning_actors_of_the_story_levels() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion with Kismet) not set");
        return;
    };
    for (map, turning, without_a_rate) in [
        ("AG-Workshop", 0, 0),
        ("AG-ParadiseCave", 7, 0),
        ("AG-BeautifulCity", 2, 0),
        ("AG-Darkcave", 0, 0),
        ("AG-IceCave", 0, 0),
        ("AG-StarHaven", 5, 8),
        ("AG-Epilogue", 0, 0),
    ] {
        let Ok((game, script)) = load_level_with_kismet(&dir, map) else {
            eprintln!("SKIP: {map} is not converted");
            continue;
        };
        let script = script.unwrap_or_else(|| panic!("{map} has Kismet"));
        let placed = &game.scene_map().expect("a converted level").placed;
        let rotating = placed
            .iter()
            .filter(|a| a.physics == PlacedPhysics::Rotating);
        let with_a_rate = rotating
            .clone()
            .filter(|a| a.rotation_rate != [0; 3])
            .count();
        assert_eq!(with_a_rate, turning, "{map}");
        assert_eq!(rotating.count() - with_a_rate, without_a_rate, "{map}");
        // Each of them turns, and nothing else does.
        let mut ids: Vec<u32> = placed
            .iter()
            .filter(|a| a.physics == PlacedPhysics::Rotating && a.rotation_rate != [0; 3])
            .map(|a| a.id)
            .collect();
        ids.sort_unstable();
        let mut known: Vec<u32> = script.turning_actors().iter().map(|t| t.0).collect();
        known.sort_unstable();
        assert_eq!(known, ids, "{map}");
    }
}

#[test]
fn the_engine_turns_the_rocks_in_the_rotating_physics() {
    // At the designed rate, which the loaders give the game, and by the
    // original's rule, which a script turns by when it is told to.
    for rule in [TurnRule::DesignedRate, TurnRule::Native] {
        let Some((mut game, mut script)) = load() else {
            return;
        };
        assert_eq!(script.turn_rule(), TurnRule::DesignedRate);
        script.set_turn_rule(rule);
        let idle = InputFrame::default();
        // (name, RotationRate as pitch, yaw, roll): the level's values.
        let rocks = [
            // Bound to no Matinee.
            ("InterpActor_51", [0, -9102, 0]),
            // Named by a Matinee group (a roll of −359 degrees in 14.4 s),
            // which does not move an actor that is not in the interpolating
            // physics: the recorded rock turns 58 units a frame of 12.82 ms,
            // the truncated 4551 × 0.01282 = 58.3 (KM1 from frame 50 368:
            // within 3.5 units over 105 968; the track would be 318 units
            // ahead by then).
            ("InterpActor_25", [0, 0, -4551]),
            ("InterpActor_16", [0, -2184, 0]),
        ];
        let placed: Vec<(u32, Vec3, [i32; 3])> = rocks
            .iter()
            .map(|(name, rate)| turning(&game, name, *rate))
            .collect();
        let known = script.turning_actors();
        for ((name, rate), (id, _, _)) in rocks.iter().zip(&placed) {
            assert!(known.contains(&(*id, *rate)), "{name} turns by itself");
        }
        const TICKS: i32 = 90;
        for _ in 0..TICKS {
            script.tick(&mut game, &idle).expect("the game is playing");
        }
        // Nothing but its rate moves the actor. At the designed rate each
        // member has turned by the truncation of its rate times the time
        // that passed (13 653, 6826 and 3276 units); by the original's rule
        // every tick added the truncated step (90 × 151, 75 and 36).
        let turned = |rate: i32| match rule {
            TurnRule::DesignedRate => designed_turn(rate, TICKS),
            TurnRule::Native => TICKS * step(rate),
        };
        for ((name, rate), (id, at, rotation)) in rocks.iter().zip(&placed) {
            let (now_at, now) = script.mover_transform(*id).expect("a mover");
            let expected = [
                rotation[0] + turned(rate[0]),
                rotation[1] + turned(rate[1]),
                rotation[2] + turned(rate[2]),
            ];
            assert_eq!(now, expected, "{name} after {TICKS} ticks ({rule:?})");
            assert_eq!(now_at, *at, "{name} turns about its own location");
            // What draws the level sees it.
            assert!(
                script.moved_actors().contains(&(*id, now_at, now)),
                "{name} is reported as moved"
            );
            assert_eq!(
                script.actor_placement(*id),
                Some((*at, *rotation)),
                "{name}"
            );
        }
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
    }
}

#[test]
fn the_anchor_rides_a_rock_the_engine_turns() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    let idle = InputFrame::default();
    let (hub, at, rotation) = turning(&game, "InterpActor_51", [0, -9102, 0]);
    for _ in 0..30 {
        script.tick(&mut game, &idle).expect("the game is playing");
    }
    let (from, to, anchor0, anchor1) =
        grapple_and_ride(&mut game, &mut script, hub, hub, CARRY_TOLERANCE);
    // 30 ticks and the attach tick, then HOLD_TICKS more, at 151.7 units of
    // yaw a tick: 31 × 151.7 = 4702.7 and 51 × 151.7 = 7736.7, so the rock
    // is 4702 units from its placement at the attach and 7736 at the end.
    let held = HOLD_TICKS as i32;
    let (attach, end) = (designed_turn(-9102, 31), designed_turn(-9102, 31 + held));
    assert_eq!((attach, end), (-4702, -7736));
    assert_eq!(from, (at, [rotation[0], rotation[1] + attach, rotation[2]]));
    assert_eq!(to, (at, [rotation[0], rotation[1] + end, rotation[2]]));
    // The anchor went along its circle about the vertical through the
    // rock's location (a yaw turns about Z): the chord of 7736 − 4702 =
    // 3034 units, 0.2909 rad, on its horizontal distance from that axis.
    // The table floors each end to 4 units.
    let arm = anchor0 - at.as_dvec3();
    let radius = arm.truncate().length();
    let angle = f64::from(attach - end) * std::f64::consts::TAU / 65_536.0;
    let chord = 2.0 * radius * (angle / 2.0).sin();
    let moved = (anchor1 - anchor0).length();
    let room = radius * 8.0 * std::f64::consts::TAU / 65_536.0 + CARRY_TOLERANCE;
    assert!(
        (moved - chord).abs() <= room,
        "the anchor moved {moved} uu on an arm of {radius} uu; the rock's turn is {chord} uu"
    );
    assert!((anchor1.z - anchor0.z).abs() <= CARRY_TOLERANCE);
    assert!(
        chord > 100.0,
        "the test is about a rock that moves: {chord}"
    );
}

#[test]
fn the_anchor_rides_a_rock_attached_to_a_turning_actor() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    let idle = InputFrame::default();
    let (hub, hub_at, _) = turning(&game, "InterpActor_16", [0, -2184, 0]);
    let (rock, rock_at, hard) = {
        let map = game.scene_map().expect("a converted level");
        let a = map
            .placed_actor_named("InterpActor_122")
            .expect("the rock is placed");
        assert_eq!(a.base, Some(hub), "the rock is attached to the hub");
        (a.id, a.location, a.hard_attach)
    };
    assert!(hard, "a hard attachment");
    let hub0 = script.mover_transform(hub).expect("a mover");
    for _ in 0..30 {
        script.tick(&mut game, &idle).expect("the game is playing");
    }
    // The attached rock is where the hub's rigid motion takes its placement
    // (a hard attachment keeps the transform relative to the base). Its
    // location is rebuilt in `f32` from that relative transform: within a
    // few steps of 1/64 uu.
    // 30 ticks of 36.4 units of yaw: 1092.
    let hub30 = script.mover_transform(hub).expect("a mover");
    assert_eq!(hub30.1[1], hub0.1[1] + designed_turn(-2184, 30));
    let (rock_now, _) = script.mover_transform(rock).expect("a mover");
    let expected = rigid(rock_at.as_dvec3(), hub0, hub30);
    assert!(
        (rock_now.as_dvec3() - expected).length() <= 0.1,
        "the attached rock is at {rock_now}, its base took its place to {expected}"
    );
    assert!(
        (rock_now - rock_at).length() > 50.0,
        "the attached rock moves"
    );
    // An anchor on the attached rock rides the hub's rigid motion too. The
    // rock's own rotation is rebuilt from a matrix every tick (each member
    // within half a unit, then floored to 4 units by the sine table): up to
    // 4.5 units per member, 0.00075 rad over the three, on the anchor's
    // offset from the rock's location (less than 300 uu for this rock) is
    // 0.22 uu a tick, which does not add up in the worst case over all
    // ticks but is allowed to here.
    let tolerance = CARRY_TOLERANCE + 0.22 * HOLD_TICKS as f64;
    let (from, to, anchor0, anchor1) =
        grapple_and_ride(&mut game, &mut script, rock, hub, tolerance);
    assert_eq!(from.0, hub_at);
    // The attach tick is the 31st and the last the 51st: 31 × 36.4 = 1128.4
    // and 51 × 36.4 = 1856.4, so the hub is at 1128 and then 1856 units from
    // its placement, 728 apart.
    let held = HOLD_TICKS as i32;
    let (attach, end) = (designed_turn(-2184, 31), designed_turn(-2184, 31 + held));
    assert_eq!((attach, end), (-1128, -1856));
    assert_eq!(from.1[1], hub0.1[1] + attach);
    assert_eq!(to.1[1], hub0.1[1] + end);
    assert!(
        (anchor0 - rock_now.as_dvec3()).length() < 600.0,
        "the anchor is on the rock"
    );
    // 728 units of yaw, 0.070 rad, on the rock's distance from the hub's
    // vertical axis.
    let radius = (anchor0 - hub_at.as_dvec3()).truncate().length();
    let chord = 2.0 * radius * (f64::from(attach - end) * std::f64::consts::PI / 65_536.0).sin();
    let moved = (anchor1 - anchor0).length();
    let room = radius * 8.0 * std::f64::consts::TAU / 65_536.0 + tolerance;
    assert!(
        (moved - chord).abs() <= room,
        "the anchor moved {moved} uu on an arm of {radius} uu; the hub's turn is {chord} uu"
    );
    assert!(chord > 30.0, "the test is about a rock that moves: {chord}");
}
