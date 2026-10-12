//! The turning rocks of AG-ParadiseCave over long runs (a second, independent
//! set of checks next to `parity_moving_anchor.rs`). Skips without
//! `ASAMU_CONVERTED_DIR` (a conversion with simple collision, Kismet and
//! Matinee).
//!
//! What moves a rock is in `asamu_game::kismet_host` ("Turning actors",
//! "Attached actors") and GRAPPLE.md G-AT-8. These tests assert rules only:
//!
//! - an anchor on a rock of every wheel stays on the rock's rigid motion for
//!   several hundred ticks (also after the release: the helper keeps riding);
//! - a rock's collision is where the rock is reported for drawing, and no
//!   longer where it was;
//! - a pawn standing on a rock that turns about a vertical axis is carried
//!   with it, neither flung nor sunk;
//! - a grapple that pulls the pawn onto a slow rock releases by proximity;
//! - a turning actor is moved by its rate alone and what is attached to it by
//!   its base alone, in every story level, also after 10 000 ticks;
//! - the merry-go-round's two arms, one on its Matinee track and one turned
//!   by its rate, each follow their own rule, and how far that takes them
//!   apart at this game's tick;
//! - a Matinee moves none of the level's three static mesh actors that its
//!   groups name;
//! - a snapshot loaded into a fresh level leaves the rocks where a fresh run
//!   has them, and a respawn does not disturb them.

use std::collections::BTreeSet;
use std::path::PathBuf;

use asamu_game::asamu_world::scene::{PlacedActor, PlacedPhysics};
use asamu_game::{Game, LevelScript, load_level_with_kismet};
use asamu_player::InputFrame;
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::world::{ActorClass, CollisionWorld};
use glam::{DVec3, Vec3};

const MAP: &str = "AG-ParadiseCave";

/// Half sizes of the pawn's box.
const BOX: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// How far from the rock's bounds the pawn is put, uu.
const STAND_OFF: f32 = 1200.0;

type Pose = (Vec3, [i32; 3]);

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let version_2 = std::fs::read_to_string(d.join("meshes").join("manifest.json"))
        .ok()
        .is_some_and(|m| m.contains("simple_collision"));
    (d.join("levels").is_dir() && d.join("kismet").is_dir() && version_2).then_some(d)
}

fn load_map(map: &str) -> Option<(Game, LevelScript)> {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion with Kismet) not set");
        return None;
    };
    let Ok((mut game, script)) = load_level_with_kismet(&dir, map) else {
        eprintln!("SKIP: {map} is not converted");
        return None;
    };
    let script = script.unwrap_or_else(|| panic!("{map} has Kismet"));
    game.start();
    Some((game, script))
}

fn load() -> Option<(Game, LevelScript)> {
    load_map(MAP)
}

/// The rows of a rotator's matrix (the world images of the local X, Y and Z
/// axes) with each angle floored to 4 units first, as the original's sine
/// table does; the sines are exact.
fn rows_of_the_table(rotation: [i32; 3]) -> [DVec3; 3] {
    rows(rotation.map(|u| ((u as u32 >> 2) & 0x3FFF) as f64 * 4.0))
}

/// The same with the angles as they are.
fn rows_exact(rotation: [i32; 3]) -> [DVec3; 3] {
    rows(rotation.map(f64::from))
}

fn rows(units: [f64; 3]) -> [DVec3; 3] {
    let sin_cos = |u: f64| (u * std::f64::consts::TAU / 65_536.0).sin_cos();
    let (sp, cp) = sin_cos(units[0]);
    let (sy, cy) = sin_cos(units[1]);
    let (sr, cr) = sin_cos(units[2]);
    [
        DVec3::new(cp * cy, cp * sy, sp),
        DVec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
        DVec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
    ]
}

/// Where a point that rides a base rigidly is after the base went from
/// `from` to `to`.
fn rigid(p: DVec3, from: Pose, to: Pose, rows: fn([i32; 3]) -> [DVec3; 3]) -> DVec3 {
    let offset = p - from.0.as_dvec3();
    let old = rows(from.1);
    let new = rows(to.1);
    let local = DVec3::new(offset.dot(old[0]), offset.dot(old[1]), offset.dot(old[2]));
    to.0.as_dvec3() + new[0] * local.x + new[1] * local.y + new[2] * local.z
}

/// A direction taken through a base's turn from `from` to `to`, as the
/// engine's matrices turn it (the sine table's angles).
fn turned(d: DVec3, from: [i32; 3], to: [i32; 3]) -> DVec3 {
    rigid(d, (Vec3::ZERO, from), (Vec3::ZERO, to), rows_of_the_table)
}

/// A tick's turn of a rate (units/s) at the game's 60 Hz tick.
fn step(rate: i32) -> i32 {
    (rate as f32 * (1.0_f32 / 60.0)) as i32
}

/// The bounds (centre, half diagonal) of the actor's collision that blocks
/// the grapple's trace, when it has any.
fn bounds(game: &Game, actor: u32) -> Option<(Vec3, f32)> {
    let sc = game.world().scene.as_ref()?;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for inst in &sc.dynamic {
        if inst.info.actor == Some(actor) && inst.info.blocks_traces && inst.enabled {
            min = min.min(inst.bounds.min);
            max = max.max(inst.bounds.max);
        }
    }
    (min.is_finite() && max.is_finite()).then(|| ((min + max) * 0.5, (max - min).length() * 0.5))
}

fn directions() -> Vec<Vec3> {
    let mut out = Vec::new();
    for x in [-1.0_f32, 0.0, 1.0] {
        for y in [-1.0_f32, 0.0, 1.0] {
            for z in [-1.0_f32, 0.0, 1.0] {
                let d = Vec3::new(x, y, z);
                if d != Vec3::ZERO {
                    out.push(d.normalize());
                }
            }
        }
    }
    out
}

/// A free place [`STAND_OFF`] from the actor's bounds with a clear line to
/// it: the eye, the direction from the actor to the eye, and where the line
/// from the eye meets the actor. The `skip` first such places are passed
/// over.
fn view_of(game: &Game, target: u32, skip: usize) -> Option<(Vec3, Vec3, Vec3)> {
    let (centre, reach) = bounds(game, target)?;
    let eye_offset = game.player().view_location(game.params()) - game.player().position;
    let range = reach + STAND_OFF;
    let mut found = 0;
    for d in directions() {
        let eye = centre + d * range;
        if game.world().overlaps_box(eye - eye_offset, BOX) {
            continue;
        }
        let Some(hit) = game.world().trace_line(eye, -d, range) else {
            continue;
        };
        if hit.hit.surface.actor != Some(target) {
            continue;
        }
        if found == skip {
            return Some((eye, d, hit.hit.position));
        }
        found += 1;
    }
    None
}

/// Puts the pawn at rest in the air with its eye at `eye`, looking at `at`.
fn look_from(game: &mut Game, eye: Vec3, at: Vec3) {
    let eye_offset = game.player().view_location(game.params()) - game.player().position;
    let aim = (at - eye).normalize();
    let p = game.player_mut();
    p.position = eye - eye_offset;
    p.velocity = Vec3::ZERO;
    p.grounded = false;
    p.pawn.based = false;
    p.yaw = aim.y.atan2(aim.x);
    p.pitch = aim.z.clamp(-1.0, 1.0).asin();
}

/// The actors of the level's wheels: every actor with a `RotationRate` (the
/// engine turns it, or a Matinee track does) and everything attached to one,
/// as `(actor, the actor at the root of its attachment)`.
fn wheel_actors(game: &Game) -> Vec<(PlacedActor, PlacedActor)> {
    let map = game.scene_map().expect("a converted level");
    let mut out = Vec::new();
    for a in &map.placed {
        let mut root = a;
        for _ in 0..8 {
            match root.base.and_then(|b| map.placed_actor(b)) {
                Some(b) => root = b,
                None => break,
            }
        }
        if root.rotation_rate != [0; 3] && a.kind == "interp_actor" {
            out.push((a.clone(), root.clone()));
        }
    }
    out
}

fn idle() -> InputFrame {
    InputFrame::default()
}

fn fire() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

// ---------------------------------------------------------------------------
// The anchor.
// ---------------------------------------------------------------------------

/// Ticks the helper is watched for after its attach.
const RIDE_TICKS: usize = 400;

/// Ticks the button is held for (the pawn, pulled at 2000 uu/s at most,
/// stays further than the release distance from a rock 1200 uu away).
const HOLD_TICKS: usize = 6;

#[test]
fn an_anchor_on_a_rock_of_every_wheel_rides_it_for_hundreds_of_ticks() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    for _ in 0..30 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    game.enable_grapple(true);
    game.unlimited_grapples(true);
    let wheels = wheel_actors(&game);
    assert!(wheels.len() >= 20, "{} wheel actors", wheels.len());
    let mut ridden: BTreeSet<String> = BTreeSet::new();
    let mut roots: BTreeSet<String> = BTreeSet::new();
    let mut worst = (0.0_f64, String::new());
    for (actor, root) in &wheels {
        let target = actor.id;
        // Up to three places to fire from: the rock goes on turning between
        // the aim and the fire trace, which can take it out of the line.
        let mut attached = None;
        for attempt in 0..3 {
            let Some((eye, _, at)) = view_of(&game, target, attempt) else {
                break;
            };
            look_from(&mut game, eye, at);
            let pin = game.player().position;
            // The level's script sets the budget; this test is not about it.
            game.set_max_grapples(3);
            game.enable_grapple(true);
            game.unlimited_grapples(true);
            let t = script.tick(&mut game, &fire()).expect("playing");
            match t.report.events.gun.attached {
                Some(a) if a.surface.actor == Some(target) => {
                    assert_eq!(a.surface.class, ActorClass::InterpActor);
                    attached = Some(pin);
                    break;
                }
                _ => {
                    // Let go and wait out the refire check.
                    for _ in 0..12 {
                        script.tick(&mut game, &idle()).expect("playing");
                    }
                }
            }
        }
        let Some(pin) = attached else {
            eprintln!("{}: no attach (no collision in sight)", actor.name);
            continue;
        };
        let follow = game.player().script.gun.follow.expect("the anchor follows");
        assert_eq!(follow.actor, target, "{}", actor.name);
        let pose0 = script.mover_transform(target).expect("a mover");
        let root0 = script.mover_transform(root.id).expect("a mover");
        let anchor0 = follow.helper.as_dvec3();
        assert_eq!(follow.helper, game.player().script.gun.grapple_location);
        let mut furthest = 0.0_f64;
        let mut furthest_root = 0.0_f64;
        let mut travelled = 0.0_f64;
        let mut last = anchor0;
        for tick in 1..=RIDE_TICKS {
            let input = if tick <= HOLD_TICKS { fire() } else { idle() };
            if tick > HOLD_TICKS {
                // Keep the pawn out of the way, at rest in the air.
                let p = game.player_mut();
                p.position = pin;
                p.velocity = Vec3::ZERO;
                p.grounded = false;
                p.pawn.based = false;
            }
            script.tick(&mut game, &input).expect("playing");
            let gun = game.player().script.gun;
            let f = gun.follow.expect("the helper keeps its base");
            assert_eq!(f.actor, target, "{} tick {tick}", actor.name);
            if tick <= HOLD_TICKS {
                assert!(gun.is_attached(), "{} tick {tick}", actor.name);
                assert_eq!(gun.grapple_location, f.helper);
            }
            let pose = script.mover_transform(target).expect("a mover");
            let helper = f.helper.as_dvec3();
            let expected = rigid(anchor0, pose0, pose, rows_of_the_table);
            furthest = furthest.max((helper - expected).length());
            let by_root = rigid(
                anchor0,
                root0,
                script.mover_transform(root.id).expect("a mover"),
                rows_exact,
            );
            furthest_root = furthest_root.max((helper - by_root).length());
            travelled += (helper - last).length();
            last = helper;
        }
        eprintln!(
            "{} (on {}): the helper went {travelled:.0} uu in {RIDE_TICKS} ticks, at most \
             {furthest:.3} uu from the rock's rigid motion, {furthest_root:.3} uu from its root's",
            actor.name, root.name
        );
        // Carried once a tick in single precision at coordinates of up to
        // 262 144 uu (a step of 1/64 uu): the roundings of 400 carries do not
        // add up to more than this.
        assert!(
            furthest <= 2.0,
            "{}: the helper left its rock's rigid motion by {furthest} uu",
            actor.name
        );
        // Against the root's exact motion: the table floors each angle to 4
        // units, which is 0.0004 rad on an arm of up to 5200 uu (2.0 uu) for
        // each of up to three members that turn, and an attached rock's own
        // rotation is rebuilt to whole units.
        assert!(
            furthest_root <= 2.0 + 6.5,
            "{}: the helper left its wheel by {furthest_root} uu",
            actor.name
        );
        assert!(travelled > 100.0, "{}: the rock moves", actor.name);
        if furthest > worst.0 {
            worst = (furthest, actor.name.clone());
        }
        ridden.insert(actor.name.clone());
        roots.insert(root.name.clone());
        // Let go of the button's refire check before the next rock.
        for _ in 0..12 {
            script.tick(&mut game, &idle()).expect("playing");
        }
    }
    eprintln!(
        "{} rocks ridden on {} wheels; worst {:.3} uu on {}",
        ridden.len(),
        roots.len(),
        worst.0,
        worst.1
    );
    // The three kinds: turned by a Matinee track, turned by the engine (with
    // and without a Matinee group that names it), attached to a turning one.
    for name in [
        "InterpActor_64",
        "InterpActor_51",
        "InterpActor_25",
        "InterpActor_122",
    ] {
        assert!(ridden.contains(name), "{name} was not ridden: {ridden:?}");
    }
    // Every wheel the engine turns that has collision to hit.
    for name in ["InterpActor_16", "InterpActor_25", "InterpActor_51"] {
        assert!(roots.contains(name), "no rock of {name}: {roots:?}");
    }
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}

// ---------------------------------------------------------------------------
// Collision and picture.
// ---------------------------------------------------------------------------

#[test]
fn the_collision_of_a_turning_rock_is_where_the_rock_is_drawn() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    // One tick, so that every turning actor is reported as moved.
    script.tick(&mut game, &idle()).expect("playing");
    let wheels = wheel_actors(&game);
    // A point on each rock's surface now, with the line it was found on.
    let mut marks = Vec::new();
    for (actor, _) in &wheels {
        let Some((_, d, at)) = view_of(&game, actor.id, 0) else {
            continue;
        };
        let pose = script.mover_transform(actor.id).expect("a mover");
        let (centre, reach) = bounds(&game, actor.id).expect("it has collision");
        marks.push((actor.clone(), pose, d, at, centre, reach));
    }
    assert!(marks.len() >= 10, "{} rocks with collision", marks.len());
    const TICKS: usize = 150;
    for _ in 0..TICKS {
        script.tick(&mut game, &idle()).expect("playing");
    }
    let moved = script.moved_actors();
    let (mut vacated, mut checked, mut obstructed, mut boxed) = (0, 0, 0, 0);
    let mut widest = 0.0_f64;
    for (actor, pose0, d0, at0, centre0, reach) in &marks {
        let id = actor.id;
        let pose = script.mover_transform(id).expect("a mover");
        // What draws the level is told this pose, once, and the placement to
        // measure it from.
        let reported: Vec<_> = moved.iter().filter(|m| m.0 == id).collect();
        assert_eq!(reported.len(), 1, "{}: {reported:?}", actor.name);
        assert_eq!((reported[0].1, reported[0].2), pose, "{}", actor.name);
        assert!(script.actor_placement(id).is_some(), "{}", actor.name);
        // ... and what an anchor rides.
        let sc = game.world().scene.as_ref().expect("a converted level");
        assert_eq!(sc.actor_location(id), Some(pose.0), "{}", actor.name);
        assert_eq!(sc.actor_rotation(id), Some(pose.1), "{}", actor.name);

        // The marked point and its line, taken with the rock as what draws
        // it takes its picture (`rotation_rows` of the two poses: the sine
        // table's angles).
        let at = rigid(at0.as_dvec3(), *pose0, pose, rows_of_the_table);
        let d = turned(d0.as_dvec3(), pose0.1, pose.1);
        // A line trace along the turned line meets the rock at the turned
        // point (unless something else stands in the last 30 uu before it
        // now).
        let hit = game
            .world()
            .trace_line((at + d * 30.0).as_vec3(), (-d).as_vec3(), 60.0)
            .unwrap_or_else(|| panic!("{}: nothing where the rock is drawn", actor.name));
        if hit.hit.surface.actor == Some(id) {
            // Measured across the face that was hit: along a line that
            // grazes the face, a rounding of 1/64 uu in the rock's place
            // shows many times over.
            let across = (hit.hit.position.as_dvec3() - at)
                .dot(hit.hit.normal.as_dvec3())
                .abs();
            let along = (hit.hit.position.as_dvec3() - at).length();
            widest = widest.max(across);
            assert!(
                across <= 0.25 && along <= 5.0,
                "{}: its surface is {across} uu (along the line {along} uu) from where its \
                 pose puts it",
                actor.name
            );
            checked += 1;
        } else {
            obstructed += 1;
        }
        // The pawn's box, swept along the same line, stops at the rock.
        let blocks_pawn = sc
            .dynamic
            .iter()
            .any(|i| i.info.actor == Some(id) && i.info.blocks_pawn && i.enabled);
        let start = (at + d * 120.0).as_vec3();
        if blocks_pawn && !game.world().overlaps_box(start, BOX) {
            let swept = game
                .world()
                .sweep_box(start, (at - d * 120.0).as_vec3(), BOX)
                .unwrap_or_else(|| panic!("{}: the pawn's box passes through it", actor.name));
            if swept.hit.surface.actor == Some(id) {
                let stop = swept.hit.position.as_dvec3();
                assert!(
                    (stop - at).dot(d) > -1.0,
                    "{}: the box's centre went {} uu past the rock's surface",
                    actor.name,
                    -(stop - at).dot(d)
                );
                boxed += 1;
            }
        }

        // Where the rock was: when it has gone clear of its former bounds,
        // neither the line nor the box meets it there any more.
        let centre = rigid(centre0.as_dvec3(), *pose0, pose, rows_of_the_table);
        if (centre - centre0.as_dvec3()).length() > f64::from(2.0 * reach + 200.0) {
            vacated += 1;
            let was = game
                .world()
                .trace_line(*at0 + *d0 * 80.0, -*d0, 80.0 + reach);
            assert!(
                was.is_none_or(|h| h.hit.surface.actor != Some(id)),
                "{}: a line still meets it where it was",
                actor.name
            );
            let was = game
                .world()
                .sweep_box(*at0 + *d0 * 60.0, *at0 - *d0 * 60.0, BOX);
            assert!(
                was.is_none_or(|h| h.hit.surface.actor != Some(id)),
                "{}: the pawn's box still meets it where it was",
                actor.name
            );
        }
    }
    eprintln!(
        "{} rocks marked: {checked} met by a line at their new place, at most {widest:.3} uu \
         off ({obstructed} behind something else), {boxed} by the pawn's box, {vacated} gone \
         from the old one",
        marks.len()
    );
    assert!(checked >= 10 && obstructed <= 3, "{checked} {obstructed}");
    assert!(boxed >= 5, "{boxed} rocks stopped the pawn's box");
    assert!(vacated >= 5, "{vacated} rocks left their place");
}

// ---------------------------------------------------------------------------
// The pawn on a rock.
// ---------------------------------------------------------------------------

/// A place on top of the actor where the pawn's box rests on a level face:
/// the box's centre there.
fn standing_place(game: &Game, actor: u32) -> Option<Vec3> {
    let sc = game.world().scene.as_ref()?;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for inst in &sc.dynamic {
        if inst.info.actor == Some(actor) && inst.info.blocks_pawn && inst.enabled {
            min = min.min(inst.bounds.min);
            max = max.max(inst.bounds.max);
        }
    }
    if !(min.is_finite() && max.is_finite()) {
        return None;
    }
    let mut best: Option<(f32, Vec3)> = None;
    for i in 1..12 {
        for j in 1..12 {
            let x = min.x + (max.x - min.x) * i as f32 / 12.0;
            let y = min.y + (max.y - min.y) * j as f32 / 12.0;
            let top = Vec3::new(x, y, max.z + 60.0);
            if game.world().overlaps_box(top, BOX) {
                continue;
            }
            let Some(hit) = game
                .world()
                .sweep_box(top, Vec3::new(x, y, min.z - 60.0), BOX)
            else {
                continue;
            };
            if hit.hit.surface.actor != Some(actor) || hit.hit.normal.z < 0.98 {
                continue;
            }
            // The place nearest the middle of the bounds: away from edges.
            let middle = (Vec3::new(x, y, 0.0)
                - Vec3::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5, 0.0))
            .length();
            if best.is_none_or(|b| middle < b.0) {
                best = Some((middle, hit.hit.position + Vec3::Z * 0.25));
            }
        }
    }
    best.map(|b| b.1)
}

#[test]
fn a_pawn_standing_on_a_rock_that_turns_is_carried_with_it() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    for _ in 0..10 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    // The rocks whose turn is a yaw: their tops stay level. (A wheel that
    // rolls tips its rocks over, and a pawn slides off.)
    let wheels: Vec<(PlacedActor, PlacedActor)> = wheel_actors(&game)
        .into_iter()
        .filter(|(_, root)| {
            root.physics == PlacedPhysics::Rotating
                && root.rotation_rate[0] == 0
                && root.rotation_rate[2] == 0
        })
        .collect();
    let mut carried = Vec::new();
    for (actor, root) in &wheels {
        let Some(place) = standing_place(&game, actor.id) else {
            eprintln!("{}: no level place to stand on", actor.name);
            continue;
        };
        {
            // Standing, a quarter of a unit above the face: the fastest rock
            // moves 30 uu a tick, away from under a pawn that still has to
            // fall onto it.
            let p = game.player_mut();
            p.position = place;
            p.velocity = Vec3::ZERO;
            p.grounded = true;
        }
        // Settle.
        for _ in 0..20 {
            script.tick(&mut game, &idle()).expect("playing");
        }
        if !game.player().grounded {
            eprintln!(
                "{}: the pawn did not come to rest on it (placed at {place}, now at {}, velocity {})",
                actor.name,
                game.player().position,
                game.player().velocity
            );
            continue;
        }
        let p0 = game.player().position.as_dvec3();
        let yaw0 = f64::from(game.player().yaw);
        let pose0 = script.mover_transform(actor.id).expect("a mover");
        let root0 = script.mover_transform(root.id).expect("a mover");
        const TICKS: usize = 300;
        let mut furthest = 0.0_f64;
        let mut fastest = 0.0_f32;
        let mut ungrounded = 0;
        for tick in 1..=TICKS {
            script.tick(&mut game, &idle()).expect("playing");
            let pose = script.mover_transform(actor.id).expect("a mover");
            let expected = rigid(p0, pose0, pose, rows_of_the_table);
            let p = game.player();
            let off = (p.position.as_dvec3() - expected).length();
            furthest = furthest.max(off);
            // Its own velocity stays what a pawn at rest has: the carry is
            // a move of its base, not a speed of its own.
            fastest = fastest.max(p.velocity.length());
            if !p.grounded {
                ungrounded += 1;
            }
            assert!(
                off <= 4.0,
                "{} tick {tick}: the pawn is {off} uu from where the rock took its place \
                 (at {}, expected {expected}; grounded {}, velocity {})",
                actor.name,
                p.position,
                p.grounded,
                p.velocity
            );
            // Not sunk into the rock, not lifted off it.
            assert!(
                (f64::from(p.position.z) - expected.z).abs() <= 2.0,
                "{} tick {tick}: height {} against {}",
                actor.name,
                p.position.z,
                expected.z
            );
        }
        let root1 = script.mover_transform(root.id).expect("a mover");
        let turn =
            f64::from(root1.1[1].wrapping_sub(root0.1[1])) * std::f64::consts::TAU / 65_536.0;
        let yaw = f64::from(game.player().yaw);
        let yaw_off = ((yaw - yaw0 - turn + std::f64::consts::PI)
            .rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI)
            .abs();
        let went = (game.player().position.as_dvec3() - p0).length();
        eprintln!(
            "{} (on {}): carried {went:.0} uu in {TICKS} ticks, at most {furthest:.3} uu from the \
             rock's motion, own speed at most {fastest:.2} uu/s, {ungrounded} ticks off the \
             ground, view turned with the rock to within {yaw_off:.5} rad",
            actor.name, root.name
        );
        assert_eq!(ungrounded, 0, "{}: the pawn left the rock", actor.name);
        assert!(fastest <= 1.0, "{}: flung at {fastest} uu/s", actor.name);
        assert!(
            yaw_off <= 1e-3,
            "{}: the view turned {yaw_off} rad off",
            actor.name
        );
        assert!(
            !game.world().overlaps_box(
                game.player().position + Vec3::Z * 2.0,
                BOX - Vec3::splat(1.0)
            ),
            "{}: the pawn is inside the rock",
            actor.name
        );
        carried.push(actor.name.clone());
    }
    eprintln!("carried on: {carried:?}");
    // The merry-go-round platform, on which the recorded pawn stood (KM1
    // from frame 23 656: at rest it turned with the platform, 28 units of
    // yaw a frame), and at least one more.
    assert!(
        carried.iter().any(|n| n == "InterpActor_16"),
        "the merry-go-round platform: {carried:?}"
    );
    assert!(carried.len() >= 2, "{carried:?}");
}

// ---------------------------------------------------------------------------
// The pull onto a rock.
// ---------------------------------------------------------------------------

#[test]
fn a_grapple_that_pulls_the_pawn_onto_a_rock_releases_by_proximity() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    for _ in 0..10 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    game.enable_grapple(true);
    game.unlimited_grapples(true);
    // The rocks of the merry-go-round (a rim speed of a few hundred uu/s):
    // the pull, 2000 uu/s at most, catches them. A rock of the fast wheels
    // runs from the pawn at up to 2200 uu/s on its circle, where only the
    // button ends the chase.
    let hub = game
        .scene_map()
        .and_then(|m| m.placed_actor_named("InterpActor_16"))
        .expect("the merry-go-round")
        .id;
    let rocks: Vec<PlacedActor> = wheel_actors(&game)
        .into_iter()
        .filter(|(_, root)| root.id == hub)
        .map(|(a, _)| a)
        .collect();
    let mut released = 0;
    for rock in &rocks {
        let Some((eye, _, at)) = view_of(&game, rock.id, 0) else {
            continue;
        };
        look_from(&mut game, eye, at);
        game.set_max_grapples(3);
        game.enable_grapple(true);
        game.unlimited_grapples(true);
        let t = script.tick(&mut game, &fire()).expect("playing");
        let Some(a) = t.report.events.gun.attached else {
            continue;
        };
        if a.surface.actor != Some(rock.id) {
            for _ in 0..12 {
                script.tick(&mut game, &idle()).expect("playing");
            }
            continue;
        }
        let anchor0 = game.player().script.gun.grapple_location;
        let mut previous = (game.player().position - anchor0).length();
        let mut out = None;
        for tick in 1..=240 {
            let t = script.tick(&mut game, &fire()).expect("playing");
            let gun = game.player().script.gun;
            let d = (game.player().position - gun.grapple_location).length();
            if let Some(r) = t.report.events.gun.released {
                out = Some((tick, r.reason, d, gun.grapple_location));
                break;
            }
            assert!(gun.is_attached(), "{} tick {tick}", rock.name);
            previous = d;
        }
        let (tick, reason, d, anchor) =
            out.unwrap_or_else(|| panic!("{}: no release in 240 ticks", rock.name));
        assert_eq!(reason, ReleaseReason::Proximity, "{}", rock.name);
        // In the first tick whose distance to the moved anchor is below the
        // release distance (200 uu, G-RL-2).
        assert!(
            d < 200.0 && previous >= 200.0,
            "{}: {previous} then {d}",
            rock.name
        );
        let rode = (anchor - anchor0).length();
        eprintln!(
            "{}: released by proximity in tick {tick} at {d:.1} uu; the anchor had gone {rode:.1} uu",
            rock.name
        );
        assert!(rode > 20.0, "{}: the anchor moved {rode}", rock.name);
        // The pawn is not inside anything when it lets go.
        assert!(
            !game
                .world()
                .overlaps_box(game.player().position, BOX - Vec3::splat(1.0)),
            "{}: released inside collision",
            rock.name
        );
        released += 1;
        for _ in 0..12 {
            script.tick(&mut game, &idle()).expect("playing");
        }
    }
    assert!(released >= 1, "no rock of the merry-go-round was reached");
}

// ---------------------------------------------------------------------------
// One mover per actor.
// ---------------------------------------------------------------------------

/// Checks, after `ticks` ticks from the level's start, that every turning
/// actor is at its placement turned by its rate's steps and nothing else,
/// and that everything attached to one is where its base's motion takes its
/// placement. Returns how many of each were checked.
fn check_turns_and_attachments(
    map: &str,
    game: &Game,
    script: &LevelScript,
    ticks: i32,
) -> (usize, usize) {
    let placed = &game.scene_map().expect("a converted level").placed;
    let turning = script.turning_actors();
    for (id, rate) in &turning {
        let (at, rotation) = script.actor_placement(*id).expect("placed");
        let expected = [
            rotation[0].wrapping_add(ticks.wrapping_mul(step(rate[0]))),
            rotation[1].wrapping_add(ticks.wrapping_mul(step(rate[1]))),
            rotation[2].wrapping_add(ticks.wrapping_mul(step(rate[2]))),
        ];
        assert_eq!(
            script.mover_transform(*id),
            Some((at, expected)),
            "{map}: actor {id} after {ticks} ticks at {rate:?}"
        );
    }
    let ids: BTreeSet<u32> = turning.iter().map(|t| t.0).collect();
    let mut attached = 0;
    for a in placed {
        let Some(base) = a.base.filter(|b| ids.contains(b)) else {
            continue;
        };
        attached += 1;
        let from = script.actor_placement(base).expect("placed");
        let to = script.mover_transform(base).expect("a mover");
        let (at, rotation) = script.actor_placement(a.id).expect("placed");
        let (now, now_rotation) = script
            .mover_transform(a.id)
            .unwrap_or_else(|| panic!("{map}: {} is not a mover", a.name));
        let expected = rigid(at.as_dvec3(), from, to, rows_of_the_table);
        let off = (now.as_dvec3() - expected).length();
        assert!(
            off <= 0.25,
            "{map}: {} is {off} uu from where its base took it after {ticks} ticks",
            a.name
        );
        // Its axes turned with the base's (all through the sine table, as
        // the engine builds them; a rotation rebuilt from a matrix is within
        // a unit a member).
        let want: Vec<DVec3> = rows_of_the_table(rotation)
            .iter()
            .map(|r| turned(*r, from.1, to.1))
            .collect();
        let have = rows_exact(now_rotation);
        for (w, h) in want.iter().zip(have) {
            assert!(
                (*w - h).length() <= 1e-3,
                "{map}: {} after {ticks} ticks: axis {h} against {w}",
                a.name
            );
        }
    }
    // No actor is reported twice.
    let moved = script.moved_actors();
    let unique: BTreeSet<u32> = moved.iter().map(|m| m.0).collect();
    assert_eq!(
        unique.len(),
        moved.len(),
        "{map}: an actor is reported twice"
    );
    (turning.len(), attached)
}

#[test]
fn a_turning_actor_is_moved_by_its_rate_alone_in_every_story_level() {
    if converted_dir().is_none() {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion with Kismet) not set");
        return;
    }
    for map in [
        "AG-Workshop",
        "AG-ParadiseCave",
        "AG-BeautifulCity",
        "AG-Darkcave",
        "AG-IceCave",
        "AG-StarHaven",
        "AG-Epilogue",
    ] {
        let Some((mut game, mut script)) = load_map(map) else {
            continue;
        };
        let mut done = 0;
        for until in [1, 300, 2000] {
            while done < until {
                script.tick(&mut game, &idle()).expect("playing");
                done += 1;
            }
            let (turning, attached) = check_turns_and_attachments(map, &game, &script, done);
            if until == 2000 {
                eprintln!("{map}: {turning} turning actors, {attached} attached to them");
            }
        }
        assert!(
            script.host_errors().is_empty(),
            "{map}: {:?}",
            script.host_errors()
        );
    }
}

#[test]
fn a_rock_and_what_is_attached_to_it_stay_together_for_ten_thousand_ticks() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    for _ in 0..10_000 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    let (turning, attached) = check_turns_and_attachments(MAP, &game, &script, 10_000);
    assert_eq!(turning, 7);
    assert!(attached >= 7, "{attached} attached actors");
}

/// The merry-go-round has two arms at one location, half a turn apart: the
/// same platform mesh twice, each with four attached rocks, both named by
/// one Matinee group whose track is one turn of yaw (less a tenth of a
/// degree) in 30 s. `InterpActor_136` is in the interpolating physics and
/// follows the track; `InterpActor_16` is in the rotating physics and turns
/// by its rate, 2184 units/s (a turn in 30 s), the fraction of each tick's
/// step dropped. So the two arms keep their half turn only at a frame time
/// whose step loses nothing: 28.0002 units in the recorded frames of
/// 12.8206 ms (KM1: the pawn at rest on `InterpActor_16` turned 28 units a
/// frame), 36 of 36.4 at this game's 60 Hz, where the arm that turns by its
/// rate falls 0.4 units a tick behind the other.
#[test]
fn the_two_arms_of_the_merry_go_round_turn_each_by_its_own_rule() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    let (by_rate, by_track) = {
        let map = game.scene_map().expect("a converted level");
        let a = map.placed_actor_named("InterpActor_16").expect("placed");
        let b = map.placed_actor_named("InterpActor_136").expect("placed");
        assert_eq!(a.location, b.location, "one axis");
        assert_eq!(a.rotation_rate, [0, -2184, 0]);
        assert_eq!(b.rotation_rate, [0, -2184, 0]);
        assert_eq!(a.physics, PlacedPhysics::Rotating);
        assert_eq!(b.physics, PlacedPhysics::Interpolating);
        // Half a turn apart as placed.
        assert_eq!((a.rotation[1] - b.rotation[1]).rem_euclid(65_536), 32_768);
        (a.id, b.id)
    };
    let yaw = |script: &LevelScript, id: u32| script.mover_transform(id).expect("a mover").1[1];
    // Two seconds in, then twenty more: the track (started with the level)
    // does not loop in them.
    for _ in 0..120 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    let (rate0, track0) = (yaw(&script, by_rate), yaw(&script, by_track));
    const TICKS: i32 = 1200;
    for _ in 0..TICKS {
        script.tick(&mut game, &idle()).expect("playing");
    }
    let (rate1, track1) = (yaw(&script, by_rate), yaw(&script, by_track));
    // By its rate: 2184 / 60 = 36.4, so 36 units a tick.
    assert_eq!(rate1 - rate0, -36 * TICKS);
    // By its track: 359.9 / 360 of a turn in 30 s, 36.399 units a tick (the
    // rotation is rebuilt from a matrix: whole turns left out, each end
    // within a unit).
    let per_tick = 359.9 / 360.0 * 65_536.0 / 30.0 / 60.0;
    let expected = -per_tick * f64::from(TICKS);
    let off = (f64::from(track1 - track0) - expected + 32_768.0).rem_euclid(65_536.0) - 32_768.0;
    assert!(
        off.abs() <= 3.0,
        "the arm on the track turned {} units (whole turns apart), the track is {expected}",
        track1 - track0
    );
    // So the arms are no longer half a turn apart: by 0.399 units a tick.
    let behind = f64::from(-36 * TICKS) - expected;
    eprintln!(
        "in {TICKS} ticks the arm that turns by its rate fell {behind:.0} units ({:.2} degrees) \
         behind the arm on the track: {:.3} degrees a second",
        behind * 360.0 / 65_536.0,
        behind * 360.0 / 65_536.0 / (f64::from(TICKS) / 60.0)
    );
    assert!((behind - 478.6).abs() < 1.0, "{behind}");
}

/// A Matinee's movement track moves an actor only in the interpolating
/// physics, so not a static mesh actor either (`kismet_host`, "Turning
/// actors"). The level has three in groups that have a movement track and
/// nothing else; the recorded pawn stood at rest on one of them
/// (`StaticMeshActor_173`, KM1 frames 25 453 to 25 469 and 25 578 to 25 585)
/// and was not moved, where the group's track goes 50 uu down in 5 s and
/// back.
#[test]
fn a_matinee_moves_no_static_mesh_actor_of_the_level() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    let path = |name: &str| format!("{MAP}.TheWorld.PersistentLevel.{name}");
    let statics: Vec<(String, u32, Pose)> = [
        "StaticMeshActor_4",
        "StaticMeshActor_173",
        "StaticMeshActor_347",
    ]
    .iter()
    .map(|name| {
        let id = script
            .actor_id_by_path(&path(name))
            .unwrap_or_else(|| panic!("the level's script names {name}"));
        let placed = script.actor_placement(id).expect("placed");
        ((*name).to_owned(), id, placed)
    })
    .collect();
    // An actor of the group that names `StaticMeshActor_173`, in the
    // interpolating physics: the track moves it.
    let mover = script
        .actor_id_by_path(&path("InterpActor_8"))
        .expect("the level's script names it");
    let mover_placed = script.actor_placement(mover).expect("placed");
    let mut furthest = 0.0_f32;
    // 15 s: the tracks' way there and back, and half again.
    for tick in 1..=900 {
        script.tick(&mut game, &idle()).expect("playing");
        for (name, id, placed) in &statics {
            // A static mesh actor may have no collision to carry; where it
            // has a body, the body stays at the placement.
            if let Some(now) = script.mover_transform(*id) {
                assert_eq!(now, *placed, "{name} moved in tick {tick}");
            }
        }
        if let Some(now) = script.mover_transform(mover) {
            furthest = furthest.max((now.0 - mover_placed.0).length());
        }
    }
    let moved = script.moved_actors();
    for (name, id, _) in &statics {
        assert!(
            moved.iter().all(|m| m.0 != *id),
            "{name} is reported as moved"
        );
    }
    // The same group's track took the actor in the interpolating physics
    // its 50 uu.
    assert!(
        (furthest - 50.0).abs() < 0.5,
        "the group's interpolating actor went {furthest} uu"
    );
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}

// ---------------------------------------------------------------------------
// Saves and respawns.
// ---------------------------------------------------------------------------

#[test]
fn a_loaded_snapshot_and_a_respawn_leave_the_rocks_to_their_turn() {
    let Some((mut game, mut script)) = load() else {
        return;
    };
    for _ in 0..500 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    // A respawn in a running level: the rocks go on from where they are.
    let before: Vec<(u32, Pose)> = script
        .mover_ids()
        .into_iter()
        .filter_map(|id| Some((id, script.mover_transform(id)?)))
        .collect();
    game.respawn();
    let after: Vec<(u32, Pose)> = script
        .mover_ids()
        .into_iter()
        .filter_map(|id| Some((id, script.mover_transform(id)?)))
        .collect();
    assert_eq!(before, after, "a respawn moves no rock");
    for _ in 0..100 {
        script.tick(&mut game, &idle()).expect("playing");
    }
    check_turns_and_attachments(MAP, &game, &script, 600);

    // A snapshot taken now, loaded into a fresh level (what a load does: the
    // level starts again): the rocks are where a fresh run has them, and
    // stay in step with it.
    let snapshot = game.capture_snapshot(None);
    let (mut loaded, mut loaded_script) = load().expect("loaded once already");
    let applied = loaded.apply_snapshot(&snapshot);
    eprintln!("the loaded snapshot: {applied:?}");
    let (mut fresh, mut fresh_script) = load().expect("loaded once already");
    let poses = |s: &LevelScript| -> Vec<(u32, Pose)> {
        s.turning_actors()
            .into_iter()
            .filter_map(|(id, _)| Some((id, s.mover_transform(id)?)))
            .collect()
    };
    assert_eq!(poses(&loaded_script), poses(&fresh_script));
    for _ in 0..200 {
        loaded_script.tick(&mut loaded, &idle()).expect("playing");
        fresh_script.tick(&mut fresh, &idle()).expect("playing");
    }
    assert_eq!(poses(&loaded_script), poses(&fresh_script));
    check_turns_and_attachments(MAP, &loaded, &loaded_script, 200);
    // ... which is not where the run that saved had them.
    assert_ne!(poses(&loaded_script), poses(&script));
}
