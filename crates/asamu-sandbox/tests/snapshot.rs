//! Save-state slots and the keyframe rewind, on hand-made levels (ours; not
//! original content). Save states are a Sandbox tool; they have nothing to
//! do with the original's save system.
//!
//! Exactness is checked against copies of the same run kept by the test:
//! no number of the simulation is written down here.
#![allow(clippy::unwrap_used)]

use asamu_game::asamu_kismet::{Graph, LevelScripts, MatineeSet, RUNTIME_FORMAT, RUNTIME_VERSION};
use asamu_game::smoke::{DEFAULT_SEED, InputScript};
use asamu_game::{Game, LevelScript};
use asamu_player::{InputFrame, PlayerParams};
use asamu_sandbox::arena::{MOVEMENT_LAB, build_arena};
use asamu_sandbox::command::{Command, CommandError, SlotOp};
use asamu_sandbox::inspect::Inspection;
use asamu_sandbox::keys::TuneValue;
use asamu_sandbox::session::{Outcome, REWIND_INTERVAL_TICKS, Session, SimCx};
use asamu_sandbox::snapshot::{RewindRing, SLOT_COUNT, SimSnapshot, SnapshotError};

fn started_graybox() -> Game {
    let mut game = Game::graybox().unwrap();
    game.start();
    game
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

fn exec(session: &mut Session, game: &mut Game, cmd: Command) -> Result<Outcome, CommandError> {
    let mut script = None;
    session.execute(
        cmd,
        &mut SimCx {
            game,
            script: &mut script,
        },
    )
}

fn slot(op: SlotOp) -> Command {
    Command::Slot { op }
}

/// One tick the way a host runs it: rules, the game's own tick, observation.
fn host_tick(session: &mut Session, game: &mut Game, input: &InputFrame) {
    let mut script = None;
    session.before_tick(&mut SimCx {
        game,
        script: &mut script,
    });
    let report = game.tick(input).unwrap();
    session.after_tick(game, None, &report);
}

/// The observable simulation state of two games is equal.
fn assert_same(a: &Game, b: &Game, what: &str) {
    assert_eq!(a.player(), b.player(), "{what}: player");
    assert_eq!(a.objects(), b.objects(), "{what}: objects");
    assert_eq!(a.clock().tick(), b.clock().tick(), "{what}: tick");
    assert_eq!(
        a.active_checkpoint(),
        b.active_checkpoint(),
        "{what}: checkpoint"
    );
    assert_eq!(a.respawn_count(), b.respawn_count(), "{what}: respawns");
    assert_eq!(a.params(), b.params(), "{what}: parameters");
    assert_eq!(a.last_events(), b.last_events(), "{what}: last events");
}

// ---------------------------------------------------------------------------
// Slots.
// ---------------------------------------------------------------------------

#[test]
fn a_slot_brings_back_the_exact_state_and_the_run_resumes_identically() {
    let mut inputs = InputScript::new(DEFAULT_SEED);
    let mut game = started_graybox();
    let mut session = Session::classic();
    for _ in 0..150 {
        host_tick(&mut session, &mut game, &inputs.next_frame());
    }
    let at_save = game.clone();
    let inputs_at_save = inputs.clone();
    let outcome = exec(&mut session, &mut game, slot(SlotOp::Save { slot: 1 })).unwrap();
    assert!(!outcome.discontinuity, "saving moves nothing");
    assert_same(&game, &at_save, "after saving");
    assert_eq!(session.slots().selected(), 1);
    assert_eq!(session.slots().get(1).map(SimSnapshot::tick), Some(150));

    // Play on, keeping what happened.
    let mut later = Vec::new();
    for _ in 0..200 {
        host_tick(&mut session, &mut game, &inputs.next_frame());
        later.push(*game.player());
    }
    assert_ne!(game.player(), at_save.player());

    // Load: the saved state, exactly.
    let outcome = exec(&mut session, &mut game, slot(SlotOp::Load { slot: 1 })).unwrap();
    assert!(outcome.discontinuity);
    assert!(outcome.recording.is_none());
    assert_same(&game, &at_save, "after loading");

    // The same inputs give the same run again: the slot held everything the
    // simulation needs.
    let mut inputs = inputs_at_save;
    for expected in &later {
        host_tick(&mut session, &mut game, &inputs.next_frame());
        assert_eq!(game.player(), expected);
    }
    // And the slot is still there, untouched by all of that.
    exec(&mut session, &mut game, slot(SlotOp::Load { slot: 1 })).unwrap();
    assert_same(&game, &at_save, "after loading twice");
}

#[test]
fn a_slot_of_another_level_is_refused() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    for _ in 0..20 {
        host_tick(&mut session, &mut game, &forward());
    }
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 0 })).unwrap();
    let saved_level = game.level().name.clone();

    // The session moves to another level (as after a level change).
    let mut other = session
        .new_game(build_arena(MOVEMENT_LAB).unwrap())
        .unwrap();
    other.start();
    for _ in 0..5 {
        host_tick(&mut session, &mut other, &forward());
    }
    let before = other.clone();
    let logged = session.log().len();
    let result = exec(&mut session, &mut other, slot(SlotOp::Load { slot: 0 }));
    assert_eq!(
        result.err(),
        Some(CommandError::Snapshot(SnapshotError::DifferentLevel {
            saved: saved_level.clone(),
            current: other.level().name.clone(),
        }))
    );
    assert_same(&other, &before, "a refused load");
    assert_eq!(other.level().name, before.level().name);
    assert_eq!(session.log().len(), logged);

    // The view says so before anyone tries.
    let slots = Inspection::new(&other, None, &session).slots();
    assert_eq!(slots[0].tick, Some(20));
    assert!(!slots[0].loadable);
    let slots = Inspection::new(&game, None, &session).slots();
    assert!(slots[0].loadable);
    assert!(!slots[0].simulation_only);

    // The snapshot itself refuses too.
    let snapshot = SimSnapshot::capture(&game, None, "graybox");
    assert_eq!(snapshot.level_name(), saved_level);
    let mut script = None;
    let refused = snapshot.restore(&mut SimCx {
        game: &mut other,
        script: &mut script,
    });
    assert!(matches!(refused, Err(SnapshotError::DifferentLevel { .. })));
    assert_same(&other, &before, "a refused restore");
}

#[test]
fn restore_reapplies_the_current_overlay() {
    // Save, change one number, load, retry: the load keeps the new number.
    let mut game = started_graybox();
    let mut session = Session::classic();
    for _ in 0..40 {
        host_tick(&mut session, &mut game, &forward());
    }
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 0 })).unwrap();
    let saved = game.clone();
    assert_eq!(*saved.params(), PlayerParams::asamu_original());

    let key = "movement.custom_gravity_scaling";
    let scaling = f64::from(game.params().movement.custom_gravity_scaling.value);
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: key.to_owned(),
            value: TuneValue::Float(scaling * 0.5),
        },
    )
    .unwrap();
    for _ in 0..40 {
        host_tick(&mut session, &mut game, &forward());
    }

    let outcome = exec(&mut session, &mut game, slot(SlotOp::Load { slot: 0 })).unwrap();
    assert!(outcome.discontinuity);
    // The simulation is the saved one...
    assert_eq!(game.clock().tick(), saved.clock().tick());
    assert_eq!(game.player().position, saved.player().position);
    assert_eq!(game.player().velocity, saved.player().velocity);
    assert_eq!(game.objects(), saved.objects());
    // ...under the session's current parameters, not the saved ones.
    assert_eq!(game.params(), session.params());
    assert_ne!(*game.params(), PlayerParams::asamu_original());
    let mut script = None;
    assert_eq!(
        session.reconcile(&mut SimCx {
            game: &mut game,
            script: &mut script
        }),
        Ok(false),
        "nothing left to reconcile after a load"
    );

    // The slot itself still holds the set it was saved under: back on
    // Classic, a load is the saved state bit for bit.
    exec(&mut session, &mut game, Command::ResetAllParams).unwrap();
    exec(&mut session, &mut game, slot(SlotOp::Load { slot: 0 })).unwrap();
    assert_same(&game, &saved, "loaded under the Classic set again");
}

#[test]
fn rules_hold_across_a_load() {
    use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 0 })).unwrap();
    assert!(game.player().script.boots.enabled);
    let rules = Rules {
        grapples: GrappleRule::Fixed(1),
        rocket_boots: Switch::Off,
        auto_refill: false,
    };
    exec(&mut session, &mut game, Command::SetRules { rules }).unwrap();
    exec(&mut session, &mut game, slot(SlotOp::Load { slot: 0 })).unwrap();
    // The slot was saved with the level's abilities; the rules are applied
    // to what it brings back, at once.
    assert_eq!(game.player().script.gun.max_grapples, 1);
    assert!(!game.player().script.boots.enabled);
}

#[test]
fn empty_and_missing_slots() {
    let mut game = started_graybox();
    let before = game.clone();
    let mut session = Session::classic();
    for index in 0..SLOT_COUNT {
        assert_eq!(
            exec(&mut session, &mut game, slot(SlotOp::Load { slot: index })).err(),
            Some(CommandError::Snapshot(SnapshotError::EmptySlot(index + 1)))
        );
    }
    for op in [
        SlotOp::Select { slot: SLOT_COUNT },
        SlotOp::Save { slot: SLOT_COUNT },
        SlotOp::Load { slot: SLOT_COUNT },
        SlotOp::Clear { slot: SLOT_COUNT },
        SlotOp::Load { slot: usize::MAX },
    ] {
        let result = exec(&mut session, &mut game, slot(op));
        assert!(
            matches!(
                result,
                Err(CommandError::Snapshot(SnapshotError::NoSuchSlot(_)))
            ),
            "{op:?}"
        );
    }
    assert_same(&game, &before, "refused slot commands");
    assert!(session.log().is_empty());
    assert_eq!(session.slots().selected(), 0);

    // Select, save, clear.
    exec(&mut session, &mut game, slot(SlotOp::Select { slot: 3 })).unwrap();
    assert_eq!(session.slots().selected(), 3);
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 2 })).unwrap();
    assert_eq!(session.slots().selected(), 2, "saving selects");
    assert_eq!(session.slots().iter().filter(Option::is_some).count(), 1);
    exec(&mut session, &mut game, slot(SlotOp::Clear { slot: 2 })).unwrap();
    assert!(session.slots().get(2).is_none());
    assert!(exec(&mut session, &mut game, slot(SlotOp::Load { slot: 2 })).is_err());
    // Clearing an empty slot is not an error.
    exec(&mut session, &mut game, slot(SlotOp::Clear { slot: 2 })).unwrap();
}

#[test]
fn a_recording_is_not_carried_by_a_slot() {
    // The snapshot never holds the live game's trace.
    let mut game = started_graybox();
    game.start_recording();
    for _ in 0..10 {
        game.tick(&forward()).unwrap();
    }
    let snapshot = SimSnapshot::capture(&game, None, "while recording");
    assert!(game.is_recording(), "capturing does not stop the live one");
    let mut target = started_graybox();
    let mut script = None;
    snapshot
        .restore(&mut SimCx {
            game: &mut target,
            script: &mut script,
        })
        .unwrap();
    assert!(!target.is_recording());
    assert_eq!(target.clock().tick(), 10);
    let live = game.stop_recording().unwrap();
    assert_eq!(live.samples.len(), 11, "the live trace went on untouched");

    // Through the session: a load while recording finishes the recording
    // first and hands it to the host; the loaded game is not recording.
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 0 })).unwrap();
    exec(
        &mut session,
        &mut game,
        Command::Record {
            on: asamu_sandbox::command::Toggle::On,
        },
    )
    .unwrap();
    for _ in 0..25 {
        host_tick(&mut session, &mut game, &forward());
    }
    exec(&mut session, &mut game, slot(SlotOp::Save { slot: 1 })).unwrap();
    assert!(game.is_recording(), "saving does not stop the recording");
    let outcome = exec(&mut session, &mut game, slot(SlotOp::Load { slot: 0 })).unwrap();
    let recording = outcome.recording.expect("the running recording, finished");
    assert_eq!(recording.trace.samples.len(), 26);
    assert_eq!(recording.trace.samples.last().map(|s| s.tick), Some(25));
    assert!(!game.is_recording());
    assert_eq!(game.clock().tick(), 0);
    // The slot saved while recording does not resume a recording either.
    let outcome = exec(&mut session, &mut game, slot(SlotOp::Load { slot: 1 })).unwrap();
    assert!(outcome.recording.is_none());
    assert!(!game.is_recording());
    assert_eq!(game.clock().tick(), 25);
}

// ---------------------------------------------------------------------------
// A scripted level.
// ---------------------------------------------------------------------------

/// A synthetic level script (written here, by us): at level start, once, it
/// sets the grapple capacity to 3.
fn level_start_scripts() -> LevelScripts {
    let doc = format!(
        r#"{{"format": "{RUNTIME_FORMAT}", "version": {RUNTIME_VERSION}, "package": "T",
            "nodes": [
              {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2]}},
              {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                "event": {{"max_trigger_count": 1}}}},
              {{"id": 2, "class": "asamu.SeqAct_SetMaxGrapples", "kind": "action", "parent": 0,
                "inputs": [{{"desc": "In"}}], "outputs": [{{"desc": "Out", "links": []}}],
                "params": {{"Grapples": 3}}, "auto_activate_outputs": true}}
            ],
            "actors": []}}"#
    );
    LevelScripts {
        graph: Graph::from_json_slice(doc.as_bytes()).unwrap(),
        matinee: MatineeSet::default(),
        missing_sublevels: Vec::new(),
    }
}

#[test]
fn a_slot_on_a_scripted_level_holds_the_script_too() {
    let mut game = started_graybox();
    let mut script = Some(LevelScript::new(&mut game, level_start_scripts()));
    let mut session = Session::classic();
    assert_eq!(game.player().script.gun.max_grapples, 0);

    // Save before the level start has fired.
    session
        .execute(
            slot(SlotOp::Save { slot: 0 }),
            &mut SimCx {
                game: &mut game,
                script: &mut script,
            },
        )
        .unwrap();
    let view = Inspection::new(&game, script.as_ref(), &session).slots();
    assert!(view[0].simulation_only, "a scripted slot says what it is");
    assert!(
        Inspection::new(&game, script.as_ref(), &session)
            .summary()
            .scripted
    );

    let tick = |game: &mut Game, script: &mut Option<LevelScript>| {
        script
            .as_mut()
            .unwrap()
            .tick(game, &InputFrame::default())
            .unwrap();
    };
    tick(&mut game, &mut script);
    assert_eq!(
        game.player().script.gun.max_grapples,
        3,
        "the level start fired"
    );
    // It fires once: undone by hand, it stays undone.
    game.set_max_grapples(1);
    tick(&mut game, &mut script);
    assert_eq!(game.player().script.gun.max_grapples, 1);

    // Load: game and script are back before the level start, so it fires
    // again. Had only the game been restored, the capacity would stay 0.
    session
        .execute(
            slot(SlotOp::Load { slot: 0 }),
            &mut SimCx {
                game: &mut game,
                script: &mut script,
            },
        )
        .unwrap();
    assert_eq!(game.clock().tick(), 0);
    assert_eq!(game.player().script.gun.max_grapples, 0);
    tick(&mut game, &mut script);
    assert_eq!(game.player().script.gun.max_grapples, 3);
}

// ---------------------------------------------------------------------------
// Rewind.
// ---------------------------------------------------------------------------

#[test]
fn the_ring_returns_its_keyframes_exactly() {
    let mut inputs = InputScript::new(DEFAULT_SEED);
    let mut game = started_graybox();
    let mut ring = RewindRing::new(20, 8);
    let mut history = Vec::new();
    for _ in 0..90 {
        game.tick(&inputs.next_frame()).unwrap();
        ring.observe(&game, None);
        history.push(game.clone());
    }
    // Keyframes at ticks 1, 21, 41, 61, 81.
    assert_eq!(ring.len(), 5);
    let mut script = None;
    for expected_tick in [81_u64, 61, 41, 21, 1] {
        let keyframe = ring.step_back().unwrap();
        assert_eq!(keyframe.tick(), expected_tick);
        let mut restored = started_graybox();
        keyframe
            .restore(&mut SimCx {
                game: &mut restored,
                script: &mut script,
            })
            .unwrap();
        let original = &history[usize::try_from(expected_tick).unwrap() - 1];
        assert_same(&restored, original, "a keyframe");
    }
    assert!(ring.is_empty());
}

#[test]
fn rewind_returns_the_keyframe_exactly_and_the_run_resumes_identically() {
    let interval = u64::from(REWIND_INTERVAL_TICKS);
    let mut inputs = InputScript::new(DEFAULT_SEED);
    let mut frames = Vec::new();
    let mut history = Vec::new();
    let mut game = started_graybox();
    let mut session = Session::classic();
    let ticks = 3 * interval + interval / 2 - 5;
    for _ in 0..ticks {
        let input = inputs.next_frame();
        host_tick(&mut session, &mut game, &input);
        frames.push(input);
        history.push(game.clone());
    }
    // Keyframes at ticks 1, 1 + interval, ...; the latest is too close to
    // "now" to be worth a rewind, so the one before it is the target.
    assert_eq!(session.rewind().len(), 4);
    let now = game.clock().tick();
    let target = 1 + 2 * interval;
    assert!(now - (1 + 3 * interval) <= interval / 2);

    let outcome = exec(&mut session, &mut game, Command::Rewind).unwrap();
    assert!(outcome.discontinuity);
    assert!(
        outcome.message.contains(&target.to_string()),
        "{}",
        outcome.message
    );
    assert_eq!(game.clock().tick(), target);
    let index = |tick: u64| usize::try_from(tick).unwrap() - 1;
    assert_same(&game, &history[index(target)], "the rewound state");

    // Replaying the inputs from there is the same run.
    for tick in (target + 1)..=now {
        host_tick(&mut session, &mut game, &frames[index(tick)]);
        assert_same(&game, &history[index(tick)], "the resumed run");
    }

    // Pressing again right after a rewind goes one keyframe further back.
    exec(&mut session, &mut game, Command::Rewind).unwrap();
    let first = game.clock().tick();
    exec(&mut session, &mut game, Command::Rewind).unwrap();
    let second = game.clock().tick();
    assert_eq!(first - second, interval);
    assert_same(&game, &history[index(second)], "two keyframes back");
    // It ends at the oldest keyframe and stays there.
    for _ in 0..10 {
        exec(&mut session, &mut game, Command::Rewind).unwrap();
    }
    assert_eq!(game.clock().tick(), 1);
    assert_same(&game, &history[0], "the oldest keyframe");
}

#[test]
fn rewind_keeps_the_current_tuning() {
    let interval = u64::from(REWIND_INTERVAL_TICKS);
    let mut game = started_graybox();
    let mut session = Session::classic();
    for _ in 0..(2 * interval + 5) {
        host_tick(&mut session, &mut game, &forward());
    }
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
    )
    .unwrap();
    exec(&mut session, &mut game, Command::Rewind).unwrap();
    assert_eq!(game.params(), session.params());
    assert_ne!(*game.params(), PlayerParams::asamu_original());
}

#[test]
fn rewind_needs_a_keyframe_and_pauses_while_recording() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    // Nothing observed yet.
    let before = game.clone();
    assert!(matches!(
        exec(&mut session, &mut game, Command::Rewind),
        Err(CommandError::Refused(_))
    ));
    assert_same(&game, &before, "a refused rewind");
    assert!(session.log().is_empty());

    for _ in 0..40 {
        host_tick(&mut session, &mut game, &forward());
    }
    let held = session.rewind().len();
    assert!(held >= 1);

    // While recording no keyframe is taken...
    exec(
        &mut session,
        &mut game,
        Command::Record {
            on: asamu_sandbox::command::Toggle::On,
        },
    )
    .unwrap();
    for _ in 0..200 {
        host_tick(&mut session, &mut game, &forward());
    }
    assert_eq!(session.rewind().len(), held);
    // ...and a rewind finishes the recording first.
    let outcome = exec(&mut session, &mut game, Command::Rewind).unwrap();
    let recording = outcome.recording.expect("the recording, finished");
    assert_eq!(recording.trace.samples.last().map(|s| s.tick), Some(240));
    assert!(!game.is_recording());
    assert!(game.clock().tick() < 40);
}

#[test]
fn the_summary_shows_the_rewind_reach() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let empty = Inspection::new(&game, None, &session).summary();
    assert!(empty.rewind_available);
    assert_eq!(empty.rewind_keyframes, 0);
    assert_eq!(empty.rewind_seconds, 0.0);
    let interval = u64::from(REWIND_INTERVAL_TICKS);
    for _ in 0..(4 * interval + 1) {
        host_tick(&mut session, &mut game, &forward());
    }
    let summary = Inspection::new(&game, None, &session).summary();
    assert_eq!(summary.rewind_keyframes, 5);
    let expected = (4 * interval) as f32 * game.clock().dt();
    assert!((summary.rewind_seconds - expected).abs() < 1.0e-4);
}
