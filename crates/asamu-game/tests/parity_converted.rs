//! The game layer on the user's own converted levels against the first
//! recordings of the original (`docs/PARITY_FINDINGS.md`; P3 to P6 and
//! P10). Skips without `ASAMU_CONVERTED_DIR` (a conversion with simple
//! collision: mesh manifest version 2, plus `kismet` and `matinee`).
//!
//! Expected values are the native rules (derived in the comments) and
//! numbers measured in the recordings (quoted with their record); nothing
//! here is a value our own code produced. The recordings themselves are not
//! needed to run this.

use std::path::PathBuf;

use asamu_game::save::Snapshot;
use asamu_game::{ConsoleCommand, Game, load_level_with_kismet};
use asamu_player::ue3_movement::PawnPhysicsState;
use asamu_player::world::{PrimitiveKind, TraceExtent};
use asamu_player::{CollisionWorld, InputFrame};
use glam::{Vec2, Vec3};

/// Half sizes of the pawn's box.
const BOX: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// The effective gravity of a falling pawn, uu/s²: `DefaultGravityZ` 520,
/// doubled by the falling integration (NATIVE_PHYSICS.md 4.6; the recorded
/// free-fall median is 1039.88, V18).
const GRAVITY: f32 = 1040.0;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let version_2 = std::fs::read_to_string(d.join("meshes").join("manifest.json"))
        .ok()
        .is_some_and(|m| m.contains("simple_collision"));
    if !(d.join("levels").is_dir() && d.join("kismet").is_dir() && version_2) {
        eprintln!(
            "SKIP: {} has no levels, Kismet and simple collision",
            d.display()
        );
        return None;
    }
    Some(d)
}

macro_rules! converted_dir_or_skip {
    () => {
        match converted_dir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion) not set");
                return;
            }
        }
    };
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

/// Puts the pawn at `position`, walking and based on its floor, facing
/// `yaw`, with the next floor check forced.
fn stand_at(g: &mut Game, position: Vec3, yaw: f32) {
    let p = g.player_mut();
    p.position = position;
    p.velocity = Vec3::ZERO;
    p.yaw = yaw;
    p.pitch = 0.0;
    p.grounded = true;
    p.pawn = PawnPhysicsState {
        floor: Vec3::Z,
        based: true,
        force_floor_check: true,
        ..PawnPhysicsState::default()
    };
}

#[test]
fn the_workshop_floor_holds_the_pawn_at_119_15() {
    let dir = converted_dir_or_skip!();
    let mut g = Game::load_level(&dir, "AG-Workshop").expect("AG-Workshop loads");
    g.start();
    // The Workshop's floor is BSP at z = 72. At (131, −339) a rug (a mesh
    // without a simple body, two triangles at 72.2) lies on it; the
    // original's pawn does not collide with it (P1). The walking floor
    // check's 28 uu trace reports BSP 1.0 uu nearer than it is and keeps the
    // reported distance at 2.15: the centre stands at 72 + 44 + 2.15 + 1.0 =
    // 119.15, the height of 1,817 recorded Workshop frames (V18).
    // From 2.5 uu above the floor (reported 1.5: raised by 0.65):
    stand_at(&mut g, Vec3::new(131.0, -339.0, 72.0 + 44.0 + 2.5), 0.0);
    g.tick(&idle()).unwrap();
    let z = g.player().position.z;
    assert!((z - 119.15).abs() < 1e-4, "after the floor check: {z}");
    assert!(g.player().grounded);
    // The box meets the BSP there and nothing of the rug; the bullet trace
    // meets the rug's triangles.
    let top = Vec3::new(131.0, -339.0, 200.0);
    let hit = g
        .world()
        .sweep_box(top, top - Vec3::Z * 200.0, BOX)
        .expect("the floor");
    assert_eq!(hit.kind, PrimitiveKind::Bsp);
    assert!(
        (hit.hit.position.z - (72.0 + 44.0)).abs() < 1e-3,
        "{}",
        hit.hit.position
    );
    let line = g
        .world()
        .trace_line(top, Vec3::NEG_Z, 400.0)
        .expect("the rug");
    assert_eq!(line.kind, PrimitiveKind::TriangleMesh);
    assert!(
        line.hit.position.z > 72.0 && line.hit.position.z < 72.5,
        "{}",
        line.hit.position
    );
    // Walking does not change the height (on the rug or off it).
    for _ in 0..12 {
        g.tick(&forward()).unwrap();
        let z = g.player().position.z;
        assert!((z - 119.15).abs() < 1e-4, "walking: {z}");
    }
}

#[test]
fn the_workshop_door_stops_the_pawn_at_x_303_08() {
    let dir = converted_dir_or_skip!();
    let mut g = Game::load_level(&dir, "AG-Workshop").expect("AG-Workshop loads");
    g.start();
    // WS1 ticks 1562 to 1569 (V11): walking towards −X at y ≈ −351 the
    // original is stopped by the door (`workshop_door_01`, a mesh with a
    // simple body) with its centre at x = 303.0848, then 303.0847 to
    // 303.0840. The contact plane for the centre is x = 302.985 (the body's
    // face plus the move's box, 21 × 1.001 wide), and a blocked pawn rests
    // 0.1 uu from it (the pull-back of a trace under 1 uu): 303.085.
    stand_at(
        &mut g,
        Vec3::new(330.0, -351.0, 119.15),
        core::f32::consts::PI,
    );
    for _ in 0..120 {
        g.tick(&forward()).unwrap();
    }
    let p = g.player().position;
    assert!((p.x - 303.08).abs() <= 0.02, "stopped at {p}");
    assert!((p.y + 351.0).abs() < 1e-3, "pushed straight in: {p}");
    assert!((p.z - 119.15).abs() < 1e-4, "{p}");
    assert!(g.player().horizontal_speed() < 1.0);
}

#[test]
fn the_workshop_with_its_kismet_walks_at_132() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-Workshop").expect("loads");
    let mut script = script.expect("AG-Workshop has Kismet");
    g.start();
    // The level-loaded event runs the console command `setspeed 0.3`: the
    // pawn class's default `GroundSpeed` 440 × 0.3 = 132, which is the
    // `GroundSpeed` of all 3,001 recorded Workshop frames (V15).
    script.tick(&mut g, &idle()).unwrap();
    assert!(g.in_story_mode());
    assert_eq!(g.player().script.ground_speed, 132.0);
    assert_eq!(
        script.console_commands(),
        &[(
            "setspeed 0.3".to_owned(),
            ConsoleCommand::SetSpeed {
                factor: 0.3,
                ground_speed: 132.0
            }
        )]
    );
    // The pawn falls from the `PlayerStart`, lands, and walks.
    for _ in 0..99 {
        script.tick(&mut g, &idle()).unwrap();
    }
    assert!(g.player().grounded);
    let mut top = 0.0_f32;
    for _ in 0..60 {
        script.tick(&mut g, &forward()).unwrap();
        top = top.max(g.player().horizontal_speed());
    }
    assert!((top - 132.0).abs() < 0.01, "top speed {top}");
    assert_eq!(g.player().script.ground_speed, 132.0);
    // The sprint key does not overwrite it: the level is in story mode,
    // which ignores sprint presses (A-ST-1). Recorded (KM1, 2026-10-12): on
    // 1,950 Workshop frames with the sprint key and forward held,
    // `GroundSpeed` is 132.0 and the pawn's sprint flag clear on every one
    // (132.0 on all 14,017 Workshop records). This was P6's open point.
    let sprint = InputFrame {
        sprint_held: true,
        ..forward()
    };
    for _ in 0..60 {
        script.tick(&mut g, &sprint).unwrap();
        assert_eq!(g.player().script.ground_speed, 132.0);
        assert!(!g.player().script.sprint.active);
        assert!(g.player().horizontal_speed() < 132.01);
    }
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}

#[test]
fn a_disabled_blocking_volume_of_beautiful_city_lets_the_pawn_through() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-BeautifulCity").expect("loads");
    let script = script.expect("AG-BeautifulCity has Kismet");
    let id = script
        .actor_id_by_path("AG-BeautifulCity.TheWorld.PersistentLevel.DynamicBlockingVolume_6")
        .expect("the volume is in the level script's actor table");
    // PLAY2 ticks 3145 to 3148 of segment 4 (V13): the original flies at
    // 1,152 uu/s through (4353, −595, 37937), which lies inside
    // `DynamicBlockingVolume_6`; the volume stores `bEnabled = false`.
    let p = Vec3::new(4353.0, -595.0, 37_937.0);
    assert_eq!(g.actor_collision(id), Some(false));
    assert!(!g.world().overlaps_box(p, BOX));
    // Sweeps of 80 uu through the point: the box stays within 84 uu of it,
    // and no other collision is within 120 uu (V13).
    let sweeps = [Vec3::X, Vec3::Y, Vec3::Z].map(|axis| (p - axis * 40.0, p + axis * 40.0));
    for (a, b) in sweeps {
        assert!(g.world().sweep_box(a, b, BOX).is_none(), "{a} to {b}");
        assert!(g.world().sweep_box(b, a, BOX).is_none(), "{b} to {a}");
    }
    // Switched on (what a "Turn On" of the level script does), it blocks,
    // as a brush.
    assert!(g.set_actor_collision(id, true));
    assert!(g.world().overlaps_box(p, BOX));
    assert_eq!(g.actor_collision(id), Some(true));
    assert!(g.set_actor_collision(id, false));
    assert!(!g.world().overlaps_box(p, BOX));
    // The level's switchable volumes and how they start (V14: 2 of 7 off).
    let states: Vec<(usize, Option<bool>)> = (0..7)
        .map(|n| {
            let path =
                format!("AG-BeautifulCity.TheWorld.PersistentLevel.DynamicBlockingVolume_{n}");
            (
                n,
                script
                    .actor_id_by_path(&path)
                    .and_then(|i| g.actor_collision(i)),
            )
        })
        .collect();
    eprintln!("AG-BeautifulCity blocking volumes at load: {states:?}");
    assert_eq!(
        states.iter().filter(|s| s.1 == Some(false)).count(),
        2,
        "{states:?}"
    );
    assert_eq!(
        states.iter().filter(|s| s.1 == Some(true)).count(),
        5,
        "{states:?}"
    );
}

#[test]
fn the_beautiful_city_cutscene_switches_all_its_blocking_volumes() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-BeautifulCity").expect("loads");
    let mut script = script.expect("AG-BeautifulCity has Kismet");
    g.start();
    let ids: Vec<u32> = (0..7)
        .map(|n| {
            script
                .actor_id_by_path(&format!(
                    "AG-BeautifulCity.TheWorld.PersistentLevel.DynamicBlockingVolume_{n}"
                ))
                .expect("the volume is in the level script's actor table")
        })
        .collect();
    let state =
        |g: &Game| -> Vec<Option<bool>> { ids.iter().map(|i| g.actor_collision(*i)).collect() };
    let on = Some(true);
    let off = Some(false);
    // As placed (`bEnabled`, V14): `_3` and `_6` start switched off.
    assert_eq!(state(&g), [on, on, on, off, on, on, off]);
    // The level script (read from the user's converted graph): the touch
    // event of `Trigger_14` starts a cutscene, turns story mode on and
    // pulses "Turn On" of one toggle action whose `Target` link holds five
    // volumes, in this order: `_6`, `_0`, `_5`, `_4`, `_1`. The cutscene's
    // "Finished" output turns story mode off and pulses that action's
    // "Turn Off". The action acts on every linked volume
    // (`SequenceAction.Targets` is an array of objects).
    //
    // The recording (PLAY2, segment 4): story mode goes off at tick 2470;
    // at tick 2597 the pawn walks at (9827.794, 788.718, 38198.684), which
    // is inside `DynamicBlockingVolume_5` (the third linked volume, which
    // starts colliding), and at tick 3145 it flies through
    // (4353, −595, 37937), inside `DynamicBlockingVolume_6` (V13).
    let in_5 = Vec3::new(9_827.794, 788.718, 38_198.684);
    let in_6 = Vec3::new(4_353.0, -595.0, 37_937.0);
    assert!(
        g.world().overlaps_box(in_5, BOX),
        "inside the volume as placed"
    );
    assert!(g.set_actor_collision(ids[5], false));
    assert!(!g.world().overlaps_box(in_5, BOX), "nothing else is there");
    assert!(g.set_actor_collision(ids[5], true));
    assert!(
        !g.world().overlaps_box(in_6, BOX),
        "`_6` starts switched off"
    );
    // Let the level start (the pawn lands, story mode of the level start).
    for _ in 0..120 {
        script.tick(&mut g, &idle()).unwrap();
    }
    assert_eq!(state(&g), [on, on, on, off, on, on, off]);
    // Into the trigger. A touch takes effect in the next frame
    // (KISMET_RUNTIME.md 1).
    let (trigger, _) = script
        .actor_transform_by_path("AG-BeautifulCity.TheWorld.PersistentLevel.Trigger_14")
        .expect("the trigger is in the level script's actor table");
    {
        let p = g.player_mut();
        p.position = trigger;
        p.velocity = Vec3::ZERO;
    }
    script.tick(&mut g, &idle()).unwrap();
    script.tick(&mut g, &idle()).unwrap();
    assert!(g.in_story_mode(), "the cutscene's story mode");
    assert_eq!(state(&g), [on, on, on, off, on, on, on], "all five on");
    assert!(g.world().overlaps_box(in_6, BOX));
    // To the cutscene's end: the five stay on while it runs and go off
    // with the story mode, in the same frame.
    let mut ended = None;
    for tick in 3..=9_000 {
        script.tick(&mut g, &idle()).unwrap();
        if !g.in_story_mode() {
            ended = Some(tick);
            break;
        }
        assert_eq!(
            state(&g),
            [on, on, on, off, on, on, on],
            "tick {tick} of the cutscene"
        );
    }
    let ended = ended.expect("the cutscene ends");
    eprintln!(
        "AG-BeautifulCity: the Trigger_14 cutscene ends {ended} ticks after the touch; volumes \
         then: {:?}",
        state(&g)
    );
    assert_eq!(
        state(&g),
        [off, off, on, off, off, off, off],
        "the five linked volumes are off, `_2` and `_3` as they were"
    );
    // Where the recorded pawn walked and flew afterwards is free.
    assert!(!g.world().overlaps_box(in_5, BOX));
    assert!(!g.world().overlaps_box(in_6, BOX));
    assert_eq!(g.respawn_count(), 0, "the pawn did not die on the way");
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}

#[test]
fn the_beautiful_city_first_cutscene_switches_its_two_blocking_volumes() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-BeautifulCity").expect("loads");
    let mut script = script.expect("AG-BeautifulCity has Kismet");
    g.apply_snapshot(&Snapshot::fresh());
    g.start();
    let ids: Vec<u32> = (0..7)
        .map(|n| {
            script
                .actor_id_by_path(&format!(
                    "AG-BeautifulCity.TheWorld.PersistentLevel.DynamicBlockingVolume_{n}"
                ))
                .expect("the volume is in the level script's actor table")
        })
        .collect();
    let state =
        |g: &Game| -> Vec<Option<bool>> { ids.iter().map(|i| g.actor_collision(*i)).collect() };
    let on = Some(true);
    let off = Some(false);
    // The level script (read from the user's converted graph): the touch
    // event of `Trigger_12` pulses "Turn On" of a toggle action on
    // `DynamicBlockingVolume_3` and starts a cutscene; the cutscene's "End"
    // output pulses "Turn Off" of that action and of one on
    // `DynamicBlockingVolume_2`, and turns the level's story mode off.
    //
    // The recording (DC1; the level starts at record 7795 in story mode):
    // - records 9873 to 9899: the pawn walks through `_3`, at record 9886
    //   with its whole box inside it at (−57667.246, 43885.992, 53270.047).
    //   `_3` stores `bEnabled = false` and the cutscene has not begun;
    // - record 13213: `GroundSpeed` goes from 264 to 440 (story mode off:
    //   the cutscene's end);
    // - records 13289 to 13510: the pawn walks through `_2`, at record
    //   13320 with its whole box inside it at (−57812.703, 43337.707,
    //   53156.914). `_2` stores `bEnabled = true`: it was switched off by
    //   then.
    let in_3 = Vec3::new(-57_667.246, 43_885.992, 53_270.047);
    let in_2 = Vec3::new(-57_812.703, 43_337.707, 53_156.914);
    assert_eq!(state(&g), [on, on, on, off, on, on, off]);
    // The two ids are those two volumes: each recorded place is blocked
    // exactly while its volume is switched on, by nothing else.
    assert!(g.world().overlaps_box(in_2, BOX), "`_2` as placed");
    assert!(g.set_actor_collision(ids[2], false));
    assert!(!g.world().overlaps_box(in_2, BOX), "nothing else is there");
    assert!(g.set_actor_collision(ids[2], true));
    assert!(
        !g.world().overlaps_box(in_3, BOX),
        "`_3` starts switched off"
    );
    assert!(g.set_actor_collision(ids[3], true));
    assert!(g.world().overlaps_box(in_3, BOX), "`_3` switched on");
    assert!(g.set_actor_collision(ids[3], false));
    // The level start: nothing switches (the pawn lands, in story mode).
    for _ in 0..120 {
        script.tick(&mut g, &idle()).unwrap();
    }
    assert!(g.in_story_mode());
    assert_eq!(state(&g), [on, on, on, off, on, on, off]);
    // Into the trigger; a touch takes effect in the next frame.
    let (trigger, _) = script
        .actor_transform_by_path("AG-BeautifulCity.TheWorld.PersistentLevel.Trigger_12")
        .expect("the trigger is in the level script's actor table");
    {
        let p = g.player_mut();
        p.position = trigger;
        p.velocity = Vec3::ZERO;
    }
    script.tick(&mut g, &idle()).unwrap();
    script.tick(&mut g, &idle()).unwrap();
    assert_eq!(
        state(&g),
        [on, on, on, on, on, on, off],
        "`_3` goes on with the touch"
    );
    assert!(g.world().overlaps_box(in_3, BOX));
    // To the cutscene's end: `_2` and `_3` stay on while it runs and go off
    // in the frame the story mode goes off.
    let mut ended = None;
    for tick in 3..=20_000 {
        script.tick(&mut g, &idle()).unwrap();
        if !g.in_story_mode() {
            ended = Some(tick);
            break;
        }
        assert_eq!(
            state(&g),
            [on, on, on, on, on, on, off],
            "tick {tick} of the cutscene"
        );
    }
    let ended = ended.expect("the cutscene ends");
    eprintln!(
        "AG-BeautifulCity: the Trigger_12 cutscene ends {ended} ticks after the touch; volumes \
         then: {:?}",
        state(&g)
    );
    assert_eq!(
        state(&g),
        [on, on, off, off, on, on, off],
        "`_2` and `_3` are off, the others as they were"
    );
    // Where the recorded pawn walked afterwards is free.
    assert!(!g.world().overlaps_box(in_2, BOX));
    assert!(!g.world().overlaps_box(in_3, BOX));
    assert_eq!(g.respawn_count(), 0, "the pawn did not die on the way");
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}

/// A recorded respawn: where the checkpoint's spawn point is, and the
/// pawn's first record after the teleport.
struct Respawn {
    map: &'static str,
    record: &'static str,
    /// The checkpoint's spawn point (x, y), to find it.
    spawn_xy: Vec2,
    /// Height and vertical velocity of the first record after the teleport
    /// (one frame of free fall from rest).
    first_z: f64,
    first_vz: f64,
    /// Height of the first walking record after it.
    rest_z: f64,
    /// The spawn point's box is inside the floor: the placement search
    /// lifts the pawn.
    lifted: bool,
}

const RESPAWNS: [Respawn; 5] = [
    Respawn {
        map: "AG-ParadiseCave",
        record: "DC1 997",
        spawn_xy: Vec2::new(88_822.66, -169_414.1),
        first_z: 96_932.351_562_5,
        first_vz: -17.530,
        rest_z: 96_933.234_375,
        lifted: false,
    },
    Respawn {
        map: "AG-ParadiseCave",
        record: "DC1 5746",
        spawn_xy: Vec2::new(54_023.44, -167_009.33),
        first_z: 110_853.570_312_5,
        first_vz: -17.184,
        rest_z: 110_853.0,
        lifted: true,
    },
    Respawn {
        map: "AG-BeautifulCity",
        record: "PLAY2 9084",
        spawn_xy: Vec2::new(-29_446.91, 18_781.23),
        first_z: 47_449.687_5,
        first_vz: -17.172,
        rest_z: 47_449.105_468_75,
        lifted: true,
    },
    Respawn {
        map: "AG-BeautifulCity",
        record: "PLAY2 20797",
        spawn_xy: Vec2::new(-6_491.87, -34_322.9),
        first_z: 37_224.738_281_25,
        first_vz: -18.360,
        rest_z: 37_224.175_781_25,
        lifted: true,
    },
    Respawn {
        map: "AG-BeautifulCity",
        record: "PLAY2 25183",
        spawn_xy: Vec2::new(-51_345.08, -44_628.66),
        first_z: 37_430.343_75,
        first_vz: -17.202,
        rest_z: f64::NAN,
        lifted: false,
    },
];

#[test]
fn respawns_appear_where_the_recorded_ones_did() {
    let dir = converted_dir_or_skip!();
    for r in &RESPAWNS {
        let mut g = Game::load_level(&dir, r.map).expect("the map loads");
        g.start();
        let (id, spawn) = {
            let map = g.scene_map().expect("a converted map");
            let c = map
                .actors
                .checkpoints
                .iter()
                .find(|c| (c.spawn_location.truncate() - r.spawn_xy).length() < 0.5)
                .unwrap_or_else(|| panic!("{}: no checkpoint spawns there", r.record));
            (c.id, c.spawn_location)
        };
        assert!(g.trigger_checkpoint(id), "{}", r.record);
        g.tick(&idle()).unwrap();
        assert_eq!(
            g.scene_respawn_point().map(|p| p.0),
            Some(spawn),
            "{}: the respawn uses this checkpoint",
            r.record
        );
        // The start's pawn is falling, as the five recorded pawns were when
        // they were teleported (physics Falling before and after).
        assert!(!g.player().grounded, "{}", r.record);
        g.respawn();
        let placed = g.player().position;
        assert_eq!(placed.truncate(), spawn.truncate(), "{}", r.record);
        assert_eq!(g.player().velocity, Vec3::ZERO);
        // The recorded teleport height: the first record, one frame of
        // free fall from rest below it (v²/2g).
        let fall = r.first_vz * r.first_vz / (2.0 * f64::from(GRAVITY));
        let recorded = r.first_z + fall;
        // The recording's resolution there (the spacing of `f32` heights),
        // four steps of it.
        let ulp = f64::from(f32::EPSILON) * recorded.abs().log2().floor().exp2();
        let tolerance = 4.0 * ulp;
        let lift = placed.z - spawn.z;
        eprintln!(
            "{} ({}): spawn z {}, placed {} (lift {lift:.4}), recorded {recorded:.4} \
             (difference {:+.4}, tolerance {tolerance:.4})",
            r.record,
            r.map,
            spawn.z,
            placed.z,
            f64::from(placed.z) - recorded
        );
        if r.lifted {
            assert!(lift > 3.0, "{}: lifted out of the floor", r.record);
        } else {
            assert_eq!(
                placed.z, spawn.z,
                "{}: a free spot is used as it is",
                r.record
            );
        }
        assert!(
            (f64::from(placed.z) - recorded).abs() <= tolerance,
            "{}: placed {} against the recorded {recorded}",
            r.record,
            placed.z
        );
        if !r.lifted {
            continue;
        }
        // The search stands the box 3.0 uu above a simple-collision floor
        // (2 uu above a floor trace reported 1.0 uu short). At 60 Hz the
        // fall from rest moves 520/3600 × (2k − 1) uu in step k: 2.311 uu
        // after four steps, and the fifth (1.3 uu) meets the floor, is
        // reported a tenth of itself short, and the landing tick's floor
        // check adds 2.15. Rest: 3.0 − 0.13 − 2.15 = 0.72 below the place.
        let step = 520.0_f32 / 3600.0;
        let mut landed = None;
        for tick in 1..=10 {
            g.tick(&idle()).unwrap();
            if g.player().grounded {
                landed = Some(tick);
                break;
            }
        }
        let rest = g.player().position.z;
        eprintln!(
            "{}: landed in tick {landed:?} at {rest}, {:.4} below the place \
             (recorded rest {}, {:.4} below)",
            r.record,
            placed.z - rest,
            r.rest_z,
            recorded - r.rest_z
        );
        assert_eq!(landed, Some(5), "{}", r.record);
        let expected = placed.z - 3.0 + 0.1 * 9.0 * step + 2.15;
        assert!(
            f64::from((rest - expected).abs()) <= tolerance,
            "{}: rest {rest} against {expected}",
            r.record
        );
        // The original's own frames were 16.4 to 17.9 ms long, so its last
        // falling move was 1.325 uu, not 1.3: its rest differs from a 60 Hz
        // one by 0.1 × 0.025 uu at most.
        assert!(
            (f64::from(rest) - r.rest_z).abs() <= tolerance + 0.01,
            "{}: rest {rest} against the recorded {}",
            r.record,
            r.rest_z
        );
    }
}

/// The placement search at every checkpoint of the story maps, not only at
/// the five recorded ones: a respawn there finds a place, the pawn comes to
/// rest on a floor and does not die again. (A teleport that finds no place
/// leaves the dead pawn where it died; the original is played through every
/// one of these checkpoints.) How many spawn points are free, lifted or
/// moved sideways is printed, not asserted.
#[test]
fn a_respawn_finds_a_place_at_every_checkpoint() {
    let dir = converted_dir_or_skip!();
    let maps = [
        "AG-Workshop",
        "AG-ParadiseCave",
        "AG-BeautifulCity",
        "AG-Darkcave",
        "AG-StarHaven",
        "AG-IceCave",
        "AG-Epilogue",
    ];
    for map in maps {
        let Ok(mut g) = Game::load_level(&dir, map) else {
            eprintln!("SKIP: {map} is not converted");
            continue;
        };
        g.start();
        // In index order: a triggered checkpoint becomes the respawn point
        // when its index is above the present one's (A-CP-2).
        let mut checkpoints: Vec<(i32, u32, Vec3, String)> = g
            .scene_map()
            .expect("a converted map")
            .actors
            .checkpoints
            .iter()
            .map(|c| (c.index, c.id, c.spawn_location, c.name.clone()))
            .collect();
        checkpoints.sort_by_key(|c| c.0);
        let (mut free, mut lifted, mut sideways, mut skipped) = (0, 0, 0, 0);
        for (index, id, spawn, name) in &checkpoints {
            assert!(g.trigger_checkpoint(*id), "{map} {name}");
            g.tick(&idle()).unwrap();
            if g.scene_respawn_point().map(|p| p.0) != Some(*spawn) {
                // An index that is not above the present one's.
                skipped += 1;
                continue;
            }
            let before = g.player().position;
            let respawns = g.respawn_count();
            g.respawn();
            let placed = g.player().position;
            // A teleport that finds no place leaves the pawn where it was.
            // (The first checkpoint of AG-BeautifulCity and of AG-Darkcave
            // is the `PlayerStart`, where the pawn still is after its first
            // tick: in place, at the free spawn point.)
            assert!(
                placed != before || placed == *spawn,
                "{map} {name} (index {index}): no place at {spawn}"
            );
            assert!(
                !g.world().overlaps_box(placed, BOX),
                "{map} {name}: placed inside collision at {placed}"
            );
            if placed == *spawn {
                free += 1;
            } else if placed.truncate() == spawn.truncate() {
                lifted += 1;
            } else {
                sideways += 1;
            }
            // Ten seconds are enough to fall onto the floor below.
            let mut rests = false;
            for _ in 0..600 {
                let r = g.tick(&idle()).unwrap();
                assert!(
                    r.died.is_none(),
                    "{map} {name} (index {index}): dies again after a respawn at {placed}"
                );
                if g.player().grounded && g.player().velocity == Vec3::ZERO {
                    rests = true;
                    break;
                }
            }
            assert!(rests, "{map} {name}: no rest after a respawn at {placed}");
            assert_eq!(g.respawn_count(), respawns + 1, "{map} {name}");
        }
        eprintln!(
            "{map}: {} checkpoints: spawn point free at {free}, pawn lifted at {lifted}, moved \
             sideways at {sideways}; {skipped} not reached by index order",
            checkpoints.len()
        );
        assert!(free + lifted + sideways > 0, "{map}");
    }
}

#[test]
fn a_new_game_stands_in_the_workshop_where_the_recorded_pawn_stands() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-Workshop").expect("loads");
    let mut script = script.expect("AG-Workshop has Kismet");
    // A new game writes a save with an empty checkpoint table and opens
    // AG-Workshop; at the pawn's spawn that save loads and the player is
    // reset to the level's latest checkpoint, the first one when the table
    // names none (A-CP-5, A-CP-4). AG-Workshop's only checkpoint has no
    // spawn point actor: the spawn point is its own location, (409.6443,
    // −347.4434, 73.32102), 68 uu from the `PlayerStart`.
    let applied = g.apply_snapshot(&Snapshot::fresh());
    assert!(applied.reset_to_checkpoint);
    // The box does not fit there (the BSP floor is at z = 72), so the
    // placement search runs (NATIVE_PHYSICS.md 10.3): the 1 uu slice at the
    // point has its bottom 0.321 uu above the floor; its 88 uu trace down
    // reports that hit 1.0 uu short, so at time 0; the centre goes to
    // 73.32102 − (2 × 0 − 1) × 44 + 1 = 118.32102.
    let placed = g.player().position;
    assert!(
        (placed - Vec3::new(409.6443, -347.4434, 118.321_02)).length() < 1e-3,
        "{placed}"
    );
    // The pawn is walking and has no base (a teleport drops it).
    assert!(g.player().grounded && !g.player().pawn.based);
    g.start();
    // First tick: the walking floor check reports the floor at 2.321 − 1.0 =
    // 1.321 uu and sets the reported distance to 2.15: the centre at 72 + 44
    // + 2.15 + 1.0 = 119.15, with no landing. The level-loaded script sets
    // the speed (P6).
    //
    // Recorded (KM1, a new game from the menu, record 1179, the first one
    // of the level at world time 0.41343 s, two ticks after the load): the
    // pawn walks at (409.6443, −347.4434, 119.15) with zero velocity, based
    // on the world, `GroundSpeed` 132, `AirControl` 0.30. A pawn that had
    // been falling from the placement would still be in the air then (2.3
    // uu take 0.067 s) and would come to rest lower. WS1's first record
    // (6869) is at the same place.
    let r = script.tick(&mut g, &idle()).unwrap();
    assert!(r.report.events.landing.is_none());
    let p = g.player().position;
    assert!(
        (p - Vec3::new(409.6443, -347.4434, 119.15)).length() < 1e-4,
        "{p}"
    );
    assert!(g.player().grounded && g.player().pawn.based);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    assert_eq!(g.player().script.ground_speed, 132.0);
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    // It stays there (records 1179 to 1204 are all at that place).
    for _ in 0..60 {
        script.tick(&mut g, &idle()).unwrap();
    }
    assert_eq!(g.player().position, p);
}

#[test]
fn the_paradise_cave_start_stands_on_its_floor_without_a_landing() {
    let dir = converted_dir_or_skip!();
    let mut g = Game::load_level(&dir, "AG-ParadiseCave").expect("AG-ParadiseCave loads");
    // The level's first checkpoint has its own spawn point, 40 uu from the
    // `PlayerStart`; a save without an entry for the level resets the player
    // to it (A-CP-5). The box is free there: the pawn is put exactly at it.
    let applied = g.apply_snapshot(&Snapshot::fresh());
    assert!(applied.reset_to_checkpoint);
    let spawn = g.scene_respawn_point().expect("a checkpoint").0;
    assert_eq!(g.player().position, spawn);
    assert!(g.player().grounded && !g.player().pawn.based);
    g.start();
    // Recorded (KM1 record 15199, the first one of the level, at world time
    // 0.535 s): the pawn walks, with zero velocity, at (210583.4375,
    // −97921.1562, 110826.6328) on a floor whose normal is (−0.04453021,
    // −0.09869812, 0.99412054), with `AirControl` 0.30. That is 19.7 uu
    // below the spawn point (110846.35) only 0.135 s after the original's
    // first tick ended: a fall of 19.7 uu takes 0.195 s, and its landing
    // would have switched `AirControl` to 0.35 (the level does not start in
    // story mode). The pawn did not fall: its first walking floor check (a
    // 28 uu trace) found the floor and put the pawn at the reported distance
    // 2.15 above it (NATIVE_PHYSICS.md 3.3).
    let r = g.tick(&idle()).unwrap();
    assert!(r.events.landing.is_none());
    assert!(g.player().grounded && g.player().pawn.based);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    let p = g.player().position;
    let recorded = [210_583.437_5_f64, -97_921.156_2, 110_826.632_8];
    eprintln!(
        "AG-ParadiseCave start: spawn point {spawn}, after the first tick [{:.4}, {:.4}, {:.4}] \
         (recorded {recorded:?}), floor {}",
        p.x,
        p.y,
        p.z,
        g.player().pawn.floor
    );
    // Positions there are 1/64 uu apart in x and y and 1/128 uu in z; the
    // recorded y is printed to four places. Two steps of each.
    assert!((f64::from(p.x) - recorded[0]).abs() <= 1.0 / 32.0, "{p}");
    assert!((f64::from(p.y) - recorded[1]).abs() <= 1.0 / 32.0, "{p}");
    assert!((f64::from(p.z) - recorded[2]).abs() <= 1.0 / 64.0, "{p}");
    let floor = g.player().pawn.floor;
    assert!(
        (floor - Vec3::new(-0.044_530_21, -0.098_698_12, 0.994_120_54)).length() < 1e-6,
        "{floor}"
    );
    // It stays there while it stands (records 15199 to 15224 are all at
    // that height).
    for _ in 0..60 {
        g.tick(&idle()).unwrap();
    }
    assert_eq!(g.player().position, p);
}

#[test]
fn the_darkcave_start_falls_from_the_player_start_and_lands() {
    let dir = converted_dir_or_skip!();
    let mut g = Game::load_level(&dir, "AG-Darkcave").expect("AG-Darkcave loads");
    g.start();
    // PLAY2 records 25512 to 25525: the first frames of the level are
    // falling frames from the `PlayerStart`'s height (4518.5845; the free
    // fall extrapolated back gives 4518.584), the pawn lands on mesh
    // triangles (`darkcavepart1`, box switch off) and that landing switches
    // `AirControl` from 0.30 to 0.35 (A-AC-2).
    //
    // The pawn starts walking, without a base (A-CP-5). The level's first
    // checkpoint has the `PlayerStart` as its spawn point, so the reset of
    // the save's load leaves it there.
    let start = g.player().position;
    assert!((start.z - 4518.5845).abs() < 1e-3, "{start}");
    assert!(g.player().grounded && !g.player().pawn.based);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    g.apply_snapshot(&Snapshot::fresh());
    assert_eq!(
        g.player().position,
        start,
        "the first checkpoint is the start"
    );
    assert!(g.player().grounded && !g.player().pawn.based);
    // The first tick's floor check finds no floor within its 28 uu (the
    // floor is 39 uu below the box): the pawn starts to fall where it is,
    // with zero velocity. Recorded: record 25512 is at world time 0.45063 s
    // with vz −52.623, that is 52.623/1040 = 0.0506 s of free fall, so the
    // fall began at 0.4000 s, when the original's first tick (a long frame,
    // clamped to 0.4 s) ended; the two recorded AG-BeautifulCity starts
    // give 0.4001 and 0.4000 s.
    let r = g.tick(&idle()).unwrap();
    assert!(!g.player().grounded && r.events.landing.is_none());
    assert_eq!(g.player().position, start);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    // The recorded landing: last falling record at vz −271.682 with a frame
    // of 16.62 ms, so the landing move was 271.682 × 0.01662 + 520 ×
    // 0.01662² = 4.659 uu; the first walking record is at 4482.1963. A
    // landing rests a tenth of its move above the contact and the landing
    // tick's floor check adds 2.15 (triangles are reported 2.8 uu nearer:
    // the floor at distance 0): the contact is at 4482.1963 − 0.4659 −
    // 2.15 = 4479.5804, 39.004 uu below the start.
    let contact = 4_482.196_3_f32 - 0.1 * 4.659 - 2.15;
    // At 60 Hz the fall moves 520/3600 × (2k − 1) uu in step k: 36.98 uu
    // after 16 steps, so step 17 (33 × 0.14444 = 4.767 uu) lands. The first
    // tick did not fall: tick 18.
    let step = 520.0_f32 / 3600.0;
    let mut landed = None;
    for tick in 2..=31 {
        let r = g.tick(&idle()).unwrap();
        if g.player().grounded {
            assert!(r.events.landing.is_some(), "the landing is a landing");
            landed = Some(tick);
            break;
        }
    }
    assert_eq!(landed, Some(18));
    let rest = g.player().position.z;
    let expected = contact + 0.1 * 33.0 * step + 2.15;
    eprintln!(
        "AG-Darkcave start: rest {rest} ({:.4} uu below the start), derived {expected}, \
         recorded 4482.19629 (36.388 uu below the start, with its own frame times)",
        start.z - rest
    );
    // The recorded heights are 0.0005 uu apart there and the landing move
    // is known to 0.01 uu (a tenth of that enters): 0.005 uu.
    assert!((rest - expected).abs() < 0.005, "{rest} against {expected}");
    assert_eq!(g.player().position.truncate(), start.truncate());
    assert!((g.player().script.air_control - 0.35).abs() < 1e-6);
}

#[test]
fn the_beautiful_city_start_falls_from_the_player_start() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-BeautifulCity").expect("loads");
    let mut script = script.expect("AG-BeautifulCity has Kismet");
    g.start();
    // DC1 records 7795 to 7802: falling from the `PlayerStart` (55030.293;
    // extrapolated 55030.278), still falling at 54994.449, walking at
    // 54996.598 on `Village_Entrance_Cave` (a mesh with a simple body). The
    // level's Kismet starts it in story mode, so the landing leaves
    // `AirControl` at 0.30 (A-AC-2), as recorded.
    let start = g.player().position;
    assert!((start.z - 55_030.293).abs() < 0.01, "{start}");
    // Walking, without a base (A-CP-5); the first checkpoint is the start.
    assert!(g.player().grounded && !g.player().pawn.based);
    g.apply_snapshot(&Snapshot::fresh());
    assert_eq!(
        g.player().position,
        start,
        "the first checkpoint is the start"
    );
    let hit = g
        .world()
        .sweep_box(start, start - Vec3::Z * 100.0, BOX)
        .expect("a floor under the start");
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    let contact = hit.hit.position.z;
    let mut landed = None;
    for tick in 1..=30 {
        script.tick(&mut g, &idle()).unwrap();
        if g.player().grounded {
            landed = Some(tick);
            break;
        }
    }
    let rest = g.player().position.z;
    eprintln!(
        "AG-BeautifulCity start: first contact at {contact} (reported {} on a 100 uu trace); \
         at 60 Hz it lands in tick {landed:?} at {rest}",
        start.z - 100.0 * hit.reported_time(TraceExtent::Box, 100.0),
    );
    // The recorded pawn was still falling 35.84 uu below the start and its
    // next falling move was 4.82 uu. At 60 Hz the fall is 32.5 uu after 15
    // steps and 36.98 after 16: the landing is in step 16 or 17, and the
    // first tick is the floor check that starts the fall: tick 17 or 18.
    assert!(matches!(landed, Some(17 | 18)), "{landed:?}");
    assert!(g.in_story_mode());
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    // What the recording says about the contact there. Record 7801 (the
    // last falling one) is at z = 54994.44921875 with vz = −273.066, and
    // record 7802 walks at 54996.59765625: 2.1484 uu **higher**, which is
    // the floor check's 2.15 at the heights' spacing there (1/256 uu). So
    // the landing move did not advance: the box was already nearer to the
    // floor than that move's pull-back (a tenth of 4.82 uu), its hit was
    // reported at time 0, and the floor check found the floor at a
    // reported distance of 0 and added 2.15. The contact is therefore
    // between 54994.449 − 0.482 = 54993.967 and 54994.449 (it is not fixed
    // more closely than that).
    assert!(
        (54_993.96..=54_994.45).contains(&contact),
        "contact {contact}"
    );
    // The same landing from the recorded state: one 60 Hz frame from record
    // 7801 moves 273.066/60 + 1040/3600 = 4.840 uu, reported 0.484 uu short;
    // the contact is nearer than that, so the pawn lands where it is and
    // rests 2.15 uu higher, at the recorded 54996.59765625 (to the spacing
    // of the heights, 0.0039 uu).
    {
        let p = g.player_mut();
        // 54994.45 is the `f32` 54994.44921875 of record 7801.
        p.position = Vec3::new(start.x, start.y, 54_994.45);
        p.velocity = Vec3::new(0.0, 0.0, -273.066);
        p.grounded = false;
        p.pawn = PawnPhysicsState::default();
    }
    script.tick(&mut g, &idle()).unwrap();
    let rest = g.player().position.z;
    assert!(g.player().grounded, "lands in that frame");
    assert!(
        (f64::from(rest) - 54_996.597_656_25).abs() <= 0.004,
        "rest {rest} against the recorded 54996.59765625"
    );
    // And it stays there while it stands (records 7802 to 7900 are all at
    // that height).
    for _ in 0..30 {
        script.tick(&mut g, &idle()).unwrap();
    }
    assert_eq!(g.player().position.z, rest);
}

#[test]
fn the_epilogue_runs_its_speed_and_size_commands() {
    let dir = converted_dir_or_skip!();
    let (mut g, script) = load_level_with_kismet(&dir, "AG-Epilogue").expect("loads");
    let mut script = script.expect("AG-Epilogue has Kismet");
    g.start();
    for _ in 0..120 {
        script.tick(&mut g, &idle()).unwrap();
    }
    // The level-loaded events of AG-Epilogue run `SetSpeed 0.15`,
    // `setspeed 0.3` and `changesize 1.4` (V15). Each is the stock command:
    // 440 × 0.15 = 66 or 440 × 0.3 = 132 uu/s, and a cylinder of 21 × 1.4 by
    // 44 × 1.4. Which speed command the original runs last is not
    // established (the level is in no recording); ours is printed.
    let commands = script.console_commands();
    eprintln!(
        "AG-Epilogue console commands: {commands:?}; GroundSpeed {}, position {}",
        g.player().script.ground_speed,
        g.player().position
    );
    let ran = |line: &str| {
        commands
            .iter()
            .find(|c| c.0.eq_ignore_ascii_case(line))
            .map(|c| c.1)
    };
    assert_eq!(
        ran("setspeed 0.15"),
        Some(ConsoleCommand::SetSpeed {
            factor: 0.15,
            ground_speed: 66.0
        })
    );
    assert_eq!(
        ran("setspeed 0.3"),
        Some(ConsoleCommand::SetSpeed {
            factor: 0.3,
            ground_speed: 132.0
        })
    );
    assert!(matches!(
        ran("changesize 1.4"),
        Some(ConsoleCommand::ChangeSize { placed: true, .. })
    ));
    let m = g.pawn_cylinder();
    assert!((m.radius - 29.4).abs() < 1e-4);
    assert!((m.half_height - 61.6).abs() < 1e-4);
    let speed = g.player().script.ground_speed;
    assert!(speed == 66.0 || speed == 132.0, "{speed}");
    // The grown pawn stands on the room's BSP floor (z = 72): its centre a
    // half height and the walking 3.15 uu above it once its floor check has
    // run (or the landing's rest: a tenth of the landing move plus 2.15).
    assert!(g.player().grounded);
    let gap = g.player().position.z - 72.0 - 61.6;
    assert!((2.15..=3.16).contains(&gap), "gap {gap}");
    assert!(
        script.runtime().errors().is_empty() && script.host_errors().is_empty(),
        "{:?} {:?}",
        script.runtime().errors(),
        script.host_errors()
    );
}
