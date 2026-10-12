//! Every story level started the way the game starts it: the level's own
//! Kismet, and a save applied before the first tick (`docs/PARITY_FINDINGS.md`
//! P10; ABILITIES.md A-CP-5). Skips without `ASAMU_CONVERTED_DIR` (a
//! conversion with simple collision, Kismet and Matinee).
//!
//! The start rule (the pawn spawns walking without a base, the save's load
//! resets it to the level's first checkpoint, the first tick is the floor
//! check) is tested level by level against the recordings in
//! `parity_converted.rs`. This file asks what that rule must not break, on
//! all seven story levels, three of which are in no recording:
//!
//! - the place the reset puts the pawn at is free of collision;
//! - the pawn stands or lands there and stays alive for its first ten
//!   seconds (the original is played from each of these starts);
//! - the level's script runs its level-loaded events in the first tick
//!   without an error, and what the recordings show of their effect at the
//!   first recorded frame holds (story mode, `GroundSpeed`);
//! - a start that falls first lands once, and that landing is a landing;
//! - a save written at that start, applied to a newly loaded level, puts the
//!   pawn at the same place (a save names a checkpoint, never a position).
//!
//! Expected values are the recordings' (quoted with their record); what no
//! recording shows is printed, not asserted.

use std::path::PathBuf;

use asamu_game::load_level_with_kismet;
use asamu_game::save::{ChapterId, Snapshot};
use asamu_player::InputFrame;
use glam::Vec3;

/// Half sizes of the pawn's box.
const BOX: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// Ten seconds of 60 Hz ticks.
const TICKS: u32 = 600;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    let version_2 = std::fs::read_to_string(d.join("meshes").join("manifest.json"))
        .ok()
        .is_some_and(|m| m.contains("simple_collision"));
    (d.join("levels").is_dir() && d.join("kismet").is_dir() && version_2).then_some(d)
}

/// What the first recorded frame of a level start shows (`None`: the level
/// is in no recording).
struct Recorded {
    /// The level's script has turned story mode on by then.
    story_mode: bool,
    /// `GroundSpeed`, uu/s.
    ground_speed: f32,
    /// The pawn stands (walking physics, a base) in the first recorded
    /// frame, at this place; `None`: it is falling from the start then.
    stands_at: Option<[f64; 3]>,
    /// `AirControl` once the pawn stands.
    air_control: f32,
}

/// The recorded level starts.
///
/// - AG-Workshop: KM1 record 0 (frame 1179, world time 0.413 s): walking on
///   the world at (409.6443, −347.4434, 119.15), `GroundSpeed` 132,
///   `AirControl` 0.30. WS1's first record is at the same place.
/// - AG-ParadiseCave: KM1 record 14017 (frame 15199, 0.535 s): walking on a
///   static mesh at (210583.4375, −97921.15625, 110826.6328), `GroundSpeed`
///   440, `AirControl` 0.30.
/// - AG-BeautifulCity: DC1 record 7795 and KM1 record 51010: falling,
///   `GroundSpeed` 264 (the story speed); walking from DC1 record 7802 with
///   `AirControl` still 0.30 (a landing in story mode does not switch it).
/// - AG-Darkcave: PLAY2 record 25512: falling, `GroundSpeed` 440; walking
///   from record 25525 with `AirControl` 0.35.
fn recorded(chapter: ChapterId) -> Option<Recorded> {
    match chapter.map_name().to_ascii_lowercase().as_str() {
        "ag-workshop" => Some(Recorded {
            story_mode: true,
            ground_speed: 132.0,
            stands_at: Some([
                409.644_287_109_375,
                -347.443_389_892_578_1,
                119.150_001_525_878_9,
            ]),
            air_control: 0.30,
        }),
        "ag-paradisecave" => Some(Recorded {
            story_mode: false,
            ground_speed: 440.0,
            stands_at: Some([210_583.437_5, -97_921.156_25, 110_826.632_812_5]),
            air_control: 0.30,
        }),
        "ag-beautifulcity" => Some(Recorded {
            story_mode: true,
            ground_speed: 264.0,
            stands_at: None,
            air_control: 0.30,
        }),
        "ag-darkcave" => Some(Recorded {
            story_mode: false,
            ground_speed: 440.0,
            stands_at: None,
            air_control: 0.35,
        }),
        _ => None,
    }
}

#[test]
fn every_story_level_starts_alive_from_a_new_save_with_its_script() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a version 2 conversion with Kismet) not set");
        return;
    };
    let idle = InputFrame::default();
    let mut checked = 0;
    for chapter in ChapterId::ALL {
        let map = chapter.map_name();
        let Ok((mut g, script)) = load_level_with_kismet(&dir, map) else {
            eprintln!("SKIP: {map} is not converted");
            continue;
        };
        let mut script = script.unwrap_or_else(|| panic!("{map} has Kismet"));
        let player_start = g.player().position;
        // The save's load, before the first tick: the reset to the level's
        // first checkpoint (A-CP-5, A-CP-4).
        let applied = g.apply_snapshot(&Snapshot::fresh());
        assert!(applied.reset_to_checkpoint, "{map}");
        assert_eq!(applied.checkpoint_index, -1, "{map}");
        assert_eq!(g.respawn_count(), 0, "{map}: a load is not a respawn");
        let placed = g.player().position;
        assert!(
            !g.world().overlaps_box(placed, BOX),
            "{map}: the reset put the pawn inside collision at {placed}"
        );
        // Walking, without a base, at rest: the first tick is the floor
        // check.
        assert!(g.player().grounded && !g.player().pawn.based, "{map}");
        assert_eq!(g.player().velocity, Vec3::ZERO, "{map}");
        g.start();

        // The first tick: the level-loaded events of the script and the
        // floor check.
        let first = script.tick(&mut g, &idle).expect("the game is playing");
        assert!(first.report.died.is_none(), "{map}: dies in its first tick");
        assert!(
            first.report.events.landing.is_none(),
            "{map}: a first tick is a floor check, never a landing"
        );
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{map}: {:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
        let stands_at_once = g.player().grounded;
        if stands_at_once {
            assert!(g.player().pawn.based, "{map}: standing has a base");
        } else {
            // No floor within the check's reach: the fall starts where the
            // pawn is, at rest.
            assert_eq!(g.player().position, placed, "{map}");
            assert_eq!(g.player().velocity, Vec3::ZERO, "{map}");
        }
        let after_first = g.player().position;
        let rec = recorded(chapter);
        if let Some(rec) = &rec {
            // The recordings' first frames (0.41 to 0.56 s of world time)
            // already show what the level-loaded events set.
            assert_eq!(g.in_story_mode(), rec.story_mode, "{map}: story mode");
            assert_eq!(
                g.player().script.ground_speed,
                rec.ground_speed,
                "{map}: GroundSpeed"
            );
            assert_eq!(
                stands_at_once,
                rec.stands_at.is_some(),
                "{map}: stands in the first tick"
            );
            if let Some(at) = rec.stands_at {
                // Two steps of the recording's resolution at these
                // coordinates (the spacing of `f32` values).
                let step = |v: f64| f64::from(f32::EPSILON) * v.abs().log2().floor().exp2();
                let p = after_first;
                for (ours, theirs) in [(p.x, at[0]), (p.y, at[1]), (p.z, at[2])] {
                    assert!(
                        (f64::from(ours) - theirs).abs() <= 2.0 * step(theirs),
                        "{map}: stands at {p}, recorded {at:?}"
                    );
                }
            }
        }

        // Ten seconds without input: alive, no script error, one landing at
        // most, and at the end at rest on a floor.
        let mut landings = 0;
        let mut landed_in = None;
        let mut died = None;
        for tick in 2..=TICKS {
            let t = script.tick(&mut g, &idle).expect("the game is playing");
            if t.report.events.landing.is_some() {
                landings += 1;
                landed_in.get_or_insert(tick);
            }
            if t.report.died.is_some() && died.is_none() {
                died = Some((tick, t.report.died, g.player().position));
            }
        }
        assert!(died.is_none(), "{map}: dies after its start: {died:?}");
        assert_eq!(g.respawn_count(), 0, "{map}");
        assert!(
            script.runtime().errors().is_empty() && script.host_errors().is_empty(),
            "{map}: {:?} {:?}",
            script.runtime().errors(),
            script.host_errors()
        );
        assert!(g.player().grounded, "{map}: still in the air after 10 s");
        assert!(
            !g.world().overlaps_box(g.player().position, BOX),
            "{map}: rests inside collision at {}",
            g.player().position
        );
        if stands_at_once {
            assert_eq!(landings, 0, "{map}: a standing start has no landing");
        } else {
            assert_eq!(landings, 1, "{map}: the start's fall lands once");
        }
        if let Some(rec) = &rec {
            assert!(
                (g.player().script.air_control - rec.air_control).abs() < 1e-6,
                "{map}: AirControl {} against the recorded {}",
                g.player().script.air_control,
                rec.air_control
            );
        }
        let rest = g.player().position;
        eprintln!(
            "{map}: PlayerStart {player_start}, first checkpoint places the pawn at {placed} \
             ({:.2} uu away), after the first tick {after_first} ({}), after 10 s {rest}; \
             landing in tick {landed_in:?}; story mode {}, GroundSpeed {}, AirControl {}; {}",
            (placed - player_start).length(),
            if stands_at_once {
                "standing"
            } else {
                "falling"
            },
            g.in_story_mode(),
            g.player().script.ground_speed,
            g.player().script.air_control,
            if rec.is_some() {
                "recorded"
            } else {
                "in no recording"
            },
        );

        // A save written now names the chapter's checkpoint, not a place:
        // applied to a newly loaded level it puts the pawn where the new
        // game's reset did.
        let saved = g.capture_snapshot(Some(&Snapshot::fresh()));
        let (mut again, _) = load_level_with_kismet(&dir, map).expect("loads again");
        let reapplied = again.apply_snapshot(&saved);
        assert!(reapplied.reset_to_checkpoint, "{map}");
        assert_eq!(
            again.player().position,
            placed,
            "{map}: the save's reset (checkpoint {})",
            reapplied.checkpoint_index
        );
        assert!(
            again.player().grounded && !again.player().pawn.based,
            "{map}"
        );
        checked += 1;
    }
    eprintln!("{checked} story levels started");
}
