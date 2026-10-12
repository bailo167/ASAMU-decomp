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
    let start = g.player().position;
    assert!((start.z - 4518.5845).abs() < 1e-3, "{start}");
    assert!(!g.player().grounded);
    assert_eq!(g.player().velocity, Vec3::ZERO);
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    // The recorded landing: last falling record at vz −271.682 with a frame
    // of 16.62 ms, so the landing move was 271.682 × 0.01662 + 520 ×
    // 0.01662² = 4.659 uu; the first walking record is at 4482.1963. A
    // landing rests a tenth of its move above the contact and the landing
    // tick's floor check adds 2.15 (triangles are reported 2.8 uu nearer:
    // the floor at distance 0): the contact is at 4482.1963 − 0.4659 −
    // 2.15 = 4479.5804, 39.004 uu below the start.
    let contact = 4_482.196_3_f32 - 0.1 * 4.659 - 2.15;
    // At 60 Hz the fall moves 520/3600 × (2k − 1) uu in step k: 36.98 uu
    // after 16 steps, so step 17 (33 × 0.14444 = 4.767 uu) lands.
    let step = 520.0_f32 / 3600.0;
    let mut landed = None;
    for tick in 1..=30 {
        let r = g.tick(&idle()).unwrap();
        if g.player().grounded {
            assert!(r.events.landing.is_some(), "the landing is a landing");
            landed = Some(tick);
            break;
        }
    }
    assert_eq!(landed, Some(17));
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
    assert!(!g.player().grounded);
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
         landed in tick {landed:?} at {rest} (recorded rest 54996.598: difference {:+.4})",
        start.z - 100.0 * hit.reported_time(TraceExtent::Box, 100.0),
        f64::from(rest) - 54_996.597_656_25
    );
    // The recorded pawn was still falling 35.84 uu below the start and its
    // next falling move was 4.82 uu. At 60 Hz the fall is 32.5 uu after 15
    // steps and 36.98 after 16: the landing is in step 16 or 17.
    assert!(matches!(landed, Some(16 | 17)), "{landed:?}");
    assert!(g.in_story_mode());
    assert!((g.player().script.air_control - 0.30).abs() < 1e-6);
    // The recording bounds the contact there: below 54994.449 (still
    // falling) and, by the landing rule (rest = contact + a tenth of the
    // 4.82 uu landing move + 2.15), at 54993.97. The rest height is printed,
    // not asserted: ours is the simple body's contact plus 0.45 + 2.15.
    assert!(
        (54_993.96..=54_994.45).contains(&contact),
        "contact {contact}"
    );
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
