//! A time-trial run end to end on the user's converted data, through the
//! map's own Kismet (TIME_TRIAL.md TT-2/3/4; verification pass).
//!
//! Ignored by default (loads the five time-trial maps); run with
//! `ASAMU_CONVERTED_DIR=<dir> cargo test --release -p asamu-game --test
//! timetrial_real_data -- --ignored`. Skips without `ASAMU_CONVERTED_DIR`
//! (`asamu-import levels`, `meshes --collision`, `kismet`, `matinee`).
//!
//! Per map, in the time-trial game type:
//!
//! - the end's touch event is disabled in the map and turned on by the
//!   level-start Kismet (`SeqEvent_LevelLoaded` → `SeqCond_IsTimeTrial` →
//!   `SeqAct_Toggle`); in story play it stays off and the start gate starts
//!   nothing;
//! - standing at the spawn starts nothing, and the respawn point of a run
//!   without a registered checkpoint lies outside the start gate: a death
//!   there clears the stopwatch and does **not** start it again;
//! - crossing the gate starts the run, crossing it again after such a death
//!   starts the stopwatch from zero, the end trigger ends it with the time
//!   since that second crossing, and Kismet then opens the front end.
//!
//! The Kismet outputs drive [`TimeTrialRun`] exactly as the app does.

use std::path::{Path, PathBuf};

use asamu_core::glam::Vec3;
use asamu_game::asamu_kismet::{Graph, OpClass, Output};
use asamu_game::save::ChapterId;
use asamu_game::timetrial::{EndOutcome, Stopwatch, TimeTrialRun};
use asamu_game::{Game, LevelOptions, LevelScript, load_level_with_kismet_options};
use asamu_player::InputFrame;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    (d.join("kismet").is_dir() && d.join("levels").is_dir()).then_some(d)
}

fn converted(dir: &Path, map: &str) -> bool {
    dir.join("levels")
        .join(format!("{map}.scene.json"))
        .is_file()
}

/// The touch event whose links reach an op of `class` directly or through
/// `SeqCond_IsTimeTrial` only.
fn touch_leading_to(g: &Graph, class: OpClass) -> Option<usize> {
    let targets = |id: usize| -> Vec<usize> {
        g.node(id).map_or_else(Vec::new, |n| {
            n.outputs
                .iter()
                .flat_map(|o| o.links.iter().map(|(t, _)| *t))
                .collect()
        })
    };
    g.nodes
        .iter()
        .filter(|n| n.class == OpClass::Touch)
        .find(|ev| {
            let mut stack = targets(ev.id);
            let mut steps = 0;
            while let Some(id) = stack.pop() {
                steps += 1;
                if steps > 64 {
                    break;
                }
                let Some(n) = g.node(id) else { continue };
                if n.class == class {
                    return true;
                }
                if n.class == OpClass::IsTimeTrial {
                    stack.extend(targets(id));
                }
            }
            false
        })
        .map(|n| n.id)
}

/// World actor id of a touch event's originator.
fn originator_id(script: &LevelScript, event: usize) -> u32 {
    let g = script.runtime().graph();
    let path = g
        .node(event)
        .and_then(|n| n.event.as_ref())
        .and_then(|e| e.originator.clone())
        .expect("the touch event has an originator");
    let actor = g.actor_by_path(&path).expect("the originator is an actor");
    script.world_id(actor).expect("the originator is loaded")
}

/// What a stretch of frames produced.
#[derive(Debug, Default, PartialEq)]
struct Seen {
    /// Frames (0-based within the stretch) of `SeqAct_StartTimeTrial`.
    starts: Vec<u64>,
    /// Frames of `SeqAct_EndTimeTrial`.
    ends: Vec<u64>,
    /// Frame of the player reset of a death sequence.
    respawn: Option<u64>,
    /// Frame at which Kismet opened the front end.
    front_end: Option<u64>,
}

/// Runs `count` idle frames, feeding `run` the way the app does (Kismet
/// start / end, the death rule at the player reset, the HUD update).
fn frames(game: &mut Game, script: &mut LevelScript, run: &mut TimeTrialRun, count: u64) -> Seen {
    let mut seen = Seen::default();
    for i in 0..count {
        let Some(t) = script.tick(game, &InputFrame::default()) else {
            break;
        };
        for o in &t.outputs {
            match o {
                Output::TimeTrial { start: true } => {
                    seen.starts.push(i);
                    run.start(t.report.tick);
                }
                Output::TimeTrial { start: false } => {
                    seen.ends.push(i);
                    run.end(t.report.tick);
                }
                Output::LevelTransition { map, .. }
                    if map.eq_ignore_ascii_case("ASAMUFrontEndMap") =>
                {
                    seen.front_end.get_or_insert(i);
                }
                _ => {}
            }
        }
        if t.report.respawned {
            seen.respawn = Some(i);
            run.on_player_reset(game.latest_checkpoint_index());
        }
        run.update_target(t.report.tick);
    }
    seen
}

fn teleport(game: &mut Game, to: Vec3) {
    let p = game.player_mut();
    p.position = to;
    p.velocity = Vec3::ZERO;
    p.pawn.force_floor_check = true;
}

/// One run on `chapter`'s map; returns the frames of its events and the
/// result (compared between two runs: determinism).
fn run_map(dir: &Path, chapter: ChapterId) -> (Vec<u64>, EndOutcome) {
    let map = chapter.map_name();
    let options = LevelOptions {
        npcs: true,
        time_trial: true,
    };
    let (mut game, script) = load_level_with_kismet_options(dir, map, options).expect("map loads");
    let mut script = script.expect("the map has Kismet");
    game.start();
    let hz = game.clock().tick_rate_hz();
    let graph = script.runtime().graph().clone();
    let start_ev = touch_leading_to(&graph, OpClass::StartTimeTrial).expect("start event");
    let end_ev = touch_leading_to(&graph, OpClass::EndTimeTrial).expect("end event");
    for (ev, max) in [(start_ev, 0), (end_ev, 1)] {
        let def = graph
            .node(ev)
            .and_then(|n| n.event.as_ref())
            .expect("event");
        assert_eq!(def.max_trigger_count, max, "{map}: MaxTriggerCount");
    }
    assert!(
        script.runtime().is_enabled(start_ev),
        "{map}: start enabled"
    );
    assert!(
        !script.runtime().is_enabled(end_ev),
        "{map}: the end event is disabled in the map"
    );
    let gate_id = originator_id(&script, start_ev);
    let end_id = originator_id(&script, end_ev);
    let scene = game.scene_map().expect("converted map");
    let hull = scene
        .actors
        .volumes
        .iter()
        .find(|v| v.id == gate_id)
        .and_then(|v| v.hulls.first())
        .expect("the start is a trigger volume with a hull")
        .clone();
    let end_at = scene
        .actors
        .triggers
        .iter()
        .find(|t| t.id == end_id)
        .expect("the end is a trigger")
        .location;
    let gate_at = (hull.min + hull.max) * 0.5;
    // Signed distance to the gate's hull (positive: outside).
    let outside = |p: Vec3| {
        hull.planes
            .iter()
            .map(|(n, w)| n.dot(p.as_dvec3()) - w)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let reach = f64::from(
        game.params().movement.capsule_radius.value
            + game.params().movement.capsule_half_height.value,
    );
    let (respawn_at, _) = game.scene_respawn_point().expect("a respawn point");
    let gate_distance = outside(respawn_at);
    assert!(
        gate_distance > reach && outside(game.player().position) > reach,
        "{map}: spawn and respawn lie outside the start gate ({:.0} / {gate_distance:.0} UU)",
        outside(game.player().position),
    );

    let mut run = TimeTrialRun::new(Some(chapter), hz);
    let mut log = Vec::new();

    // Level start: Kismet turns the end event on; nothing starts.
    let idle = frames(&mut game, &mut script, &mut run, 300);
    assert_eq!(idle, Seen::default(), "{map}: idle frames");
    assert!(
        script.runtime().is_enabled(end_ev),
        "{map}: the level-start Kismet enables the end in time trial"
    );
    assert_eq!(game.latest_checkpoint_index(), None, "{map}");
    assert!(!run.active && run.stopwatch == Stopwatch::Cleared);

    // The gate starts the run.
    teleport(&mut game, gate_at);
    let s = frames(&mut game, &mut script, &mut run, 120);
    assert_eq!(s.starts.len(), 1, "{map}: one start per crossing ({s:?})");
    assert!(run.active && run.running(), "{map}");
    log.extend(&s.starts);

    // A death before any checkpoint is registered: the stopwatch is
    // cleared, the run stays active, and the respawn starts nothing.
    game.kill_player();
    let d = frames(&mut game, &mut script, &mut run, 240);
    assert!(d.respawn.is_some(), "{map}: the death sequence resets");
    assert!(
        d.starts.is_empty(),
        "{map}: the respawn does not start ({d:?})"
    );
    assert_eq!(game.latest_checkpoint_index(), None, "{map}");
    assert_eq!(run.stopwatch, Stopwatch::Cleared, "{map}");
    assert!(run.active && run.restart_allowed(), "{map}");
    log.extend(d.respawn);

    // The gate again: counting from this crossing.
    teleport(&mut game, gate_at);
    let s2 = frames(&mut game, &mut script, &mut run, 120);
    assert_eq!(s2.starts.len(), 1, "{map}: the start re-fires ({s2:?})");
    let Stopwatch::Running { start_tick } = run.stopwatch else {
        panic!("{map}: running after the second crossing");
    };
    log.extend(&s2.starts);

    // The end trigger ends the run; Kismet opens the front end.
    teleport(&mut game, end_at);
    let e = frames(&mut game, &mut script, &mut run, 420);
    assert_eq!(e.ends.len(), 1, "{map}: one end ({e:?})");
    assert!(e.starts.is_empty(), "{map}");
    assert!(!run.active && !run.restart_allowed(), "{map}");
    let Stopwatch::Paused { ticks } = run.stopwatch else {
        panic!("{map}: paused by the end");
    };
    let outcome = EndOutcome::Finished(ticks as f64 / hz);
    assert_eq!(
        run.finished.map(EndOutcome::Finished),
        Some(outcome),
        "{map}"
    );
    // The frames between the second crossing's start and the end's touch:
    // the rest of the 120 gate frames plus the end's first frames.
    let since_start = 120 - s2.starts[0] + e.ends[0];
    assert_eq!(ticks, since_start, "{map}: counted from tick {start_tick}");
    let open = e.front_end.expect("the front end opens after the end");
    let seconds = (open - e.ends[0]) as f64 / hz;
    assert!(
        (3.9..=5.2).contains(&seconds),
        "{map}: front end {seconds:.2} s after the end"
    );
    log.extend(&e.ends);
    log.push(open);
    eprintln!(
        "{map}: gate {gate_distance:.0} UU from the respawn point; start, respawn, start, end, front end at \
         frames {log:?}; front end {seconds:.2} s after the end; {outcome:?}"
    );
    (log, outcome)
}

#[test]
#[ignore = "loads the five time-trial maps; run with --ignored"]
fn a_time_trial_runs_from_the_gate_to_the_end_on_every_map() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted levels and Kismet not set");
        return;
    };
    let mut ran = 0;
    for chapter in ChapterId::WITH_COLLECTIBLES {
        let map = chapter.map_name();
        if !converted(&dir, map) {
            eprintln!("SKIP {map}: not converted");
            continue;
        }
        let first = run_map(&dir, chapter);
        // Deterministic: a second run gives the same frames and time.
        assert_eq!(run_map(&dir, chapter), first, "{map}: deterministic");
        ran += 1;
    }
    eprintln!("{ran} time-trial maps ran");
}

#[test]
#[ignore = "loads the five time-trial maps; run with --ignored"]
fn story_play_never_starts_or_ends_a_time_trial() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR with converted levels and Kismet not set");
        return;
    };
    for chapter in ChapterId::WITH_COLLECTIBLES {
        let map = chapter.map_name();
        if !converted(&dir, map) {
            eprintln!("SKIP {map}: not converted");
            continue;
        }
        let (mut game, script) =
            load_level_with_kismet_options(&dir, map, LevelOptions::default()).expect("map loads");
        let mut script = script.expect("the map has Kismet");
        game.start();
        let graph = script.runtime().graph().clone();
        let start_ev = touch_leading_to(&graph, OpClass::StartTimeTrial).expect("start event");
        let end_ev = touch_leading_to(&graph, OpClass::EndTimeTrial).expect("end event");
        let gate_id = originator_id(&script, start_ev);
        let gate_at = game
            .scene_map()
            .and_then(|m| m.actors.volumes.iter().find(|v| v.id == gate_id))
            .and_then(|v| v.hulls.first())
            .map(|h| (h.min + h.max) * 0.5)
            .expect("gate");
        let mut run = TimeTrialRun::new(Some(chapter), game.clock().tick_rate_hz());
        let idle = frames(&mut game, &mut script, &mut run, 120);
        assert!(idle.starts.is_empty() && idle.ends.is_empty(), "{map}");
        assert!(
            !script.runtime().is_enabled(end_ev),
            "{map}: the end event stays off in story play"
        );
        teleport(&mut game, gate_at);
        let s = frames(&mut game, &mut script, &mut run, 120);
        assert!(
            s.starts.is_empty(),
            "{map}: the gate starts nothing ({s:?})"
        );
        assert!(!run.active && run.stopwatch == Stopwatch::Cleared, "{map}");
    }
}
