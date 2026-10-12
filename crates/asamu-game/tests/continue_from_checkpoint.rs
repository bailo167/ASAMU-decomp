//! Continue in AG-ParadiseCave: a level started from a checkpoint save does
//! not play its opening again (play-test defect 14 of 2026-10-12). Skips
//! without `ASAMU_CONVERTED_DIR` (a conversion with Kismet and Matinee).
//!
//! The rule is in `asamu_kismet::save_state` and
//! `asamu_game::kismet_host` ("A load from a checkpoint"); the ungated tests
//! of it are there, on hand-made level scripts. Here the shipped level's own
//! script decides: its opening (a camera flight, cinematic mode, the
//! narrator's line at the landing) hangs off the "save loaded" event behind a
//! bool variable, and a save written at a checkpoint holds that variable.
//!
//! With `ASAMU_CONTINUE_SAVE_OUT=<folder>` the first test also writes the
//! save it made (our own documents: `snapshot.json`, `general.json`,
//! `progression.json`) into `<folder>/saves`, so that the app can be started
//! with `ASAMU_SAVE_DIR=<folder>` and `ASAMU_MENU_ACTION=continue` to look at
//! the same Continue on screen. A developer's aid; the folder is user-local
//! data and never part of the repository.

use std::path::PathBuf;

use asamu_game::asamu_kismet::{Output, ToggleMode, VarState};
use asamu_game::kismet_host::ScriptRestore;
use asamu_game::save::{
    ChapterId, SaveSession, SaveStore, Snapshot, checkpoint_saved, decode_document, encode_document,
};
use asamu_game::{Game, LevelScript, load_level_with_kismet};
use asamu_player::InputFrame;

const MAP: &str = "AG-ParadiseCave";

/// Ticks of two seconds.
const TWO_SECONDS: usize = 120;

/// Ticks by which a fresh start's opening is over (it takes 8.6 s to the
/// narrator's line in a recorded session of ours; 15 s here).
const OPENING: usize = 900;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    (d.join("levels").is_dir() && d.join("kismet").is_dir() && d.join("matinee").is_dir())
        .then_some(d)
}

fn load() -> Option<(Game, LevelScript)> {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a conversion with Kismet and Matinee) not set");
        return None;
    };
    let Ok((game, script)) = load_level_with_kismet(&dir, MAP) else {
        eprintln!("SKIP: {MAP} is not converted");
        return None;
    };
    Some((game, script.unwrap_or_else(|| panic!("{MAP} has Kismet"))))
}

/// The level start as the app runs it (`apps/asamu`, `ui/flow.rs`): the
/// snapshot at the spawn, the script's load, the start.
fn start(game: &mut Game, script: &mut LevelScript, snapshot: &Snapshot) -> (i32, ScriptRestore) {
    let index = game.apply_snapshot(snapshot).checkpoint_index;
    let restore = script.start_from_save(Some(snapshot), Some(index));
    game.start();
    (index, restore)
}

/// What the presentation was asked for in a stretch of ticks.
#[derive(Debug, Default)]
struct Seen {
    /// Ticks (from 1) with a director cut to a group's camera.
    cuts: Vec<usize>,
    /// Ticks with cinematic mode switched on or toggled.
    cinematic: Vec<usize>,
    /// Ticks with a view target other than the player.
    targets: Vec<usize>,
    /// Ticks with a Matinee fade above zero.
    fades: Vec<usize>,
    /// Narrator lines started.
    narrator: Vec<String>,
    /// The pawn's largest horizontal distance from where it stood before
    /// the first tick.
    drift: f32,
    /// Paths of the opening sequence's Matinees that were started.
    intro: Vec<String>,
}

impl Seen {
    /// Nothing took the view from the player.
    fn view_is_the_player_s(&self) -> bool {
        self.cuts.is_empty()
            && self.cinematic.is_empty()
            && self.targets.is_empty()
            && self.fades.is_empty()
    }
}

fn run(game: &mut Game, script: &mut LevelScript, ticks: usize, seen: &mut Seen, from: usize) {
    let origin = game.player().position;
    for i in 0..ticks {
        let tick = from + i + 1;
        let t = script
            .tick(game, &InputFrame::default())
            .expect("the game is playing");
        for o in &t.outputs {
            match o {
                Output::MatineeCut { group: Some(_), .. } => seen.cuts.push(tick),
                Output::CinematicMode { mode, .. } if *mode != ToggleMode::Off => {
                    seen.cinematic.push(tick);
                }
                Output::CameraTarget { target: Some(_) } => seen.targets.push(tick),
                Output::MatineeFade { amount, .. } if *amount > 0.0 => seen.fades.push(tick),
                Output::NarratorLine { id, .. } => seen.narrator.push(id.clone()),
                _ => {}
            }
        }
        let moved = (game.player().position - origin).truncate().length();
        seen.drift = seen.drift.max(moved);
    }
    seen.intro = script
        .runtime()
        .graph()
        .nodes
        .iter()
        .filter(|n| {
            n.class == asamu_game::asamu_kismet::OpClass::Interp
                && n.path.contains("Intro_Sequence")
                && script.runtime().activate_count(n.id) > 0
        })
        .map(|n| n.path.clone())
        .collect();
}

/// The pawn's abilities as the save keeps them.
fn abilities(game: &Game) -> (i32, bool, bool) {
    let s = &game.player().script;
    (s.gun.max_grapples, s.boots.enabled, s.gun.can_grapple)
}

fn collected(game: &Game) -> Vec<bool> {
    game.npcs()
        .map(|n| {
            n.runtime()
                .collectibles
                .iter()
                .map(|c| c.collected)
                .collect()
        })
        .unwrap_or_default()
}

/// Plays the level from a fresh start through its opening to its fifth
/// checkpoint (in index order), with the grapple picked up and one
/// collectible taken on the way, and returns the save written there, the
/// checkpoint's index and what the opening showed.
///
/// The way there is scripted, not walked: the two crystals' trigger volumes
/// are touched through the script's own touch events, a collectible is set
/// taken, and the checkpoints are activated in order as a
/// `SeqAct_TriggerCheckpoint` would.
fn play_to_the_fifth_checkpoint() -> Option<(Snapshot, i32, Seen)> {
    let (mut game, mut script) = load()?;
    let fresh = Snapshot::fresh();
    let (index, restore) = start(&mut game, &mut script, &fresh);
    assert_eq!(index, -1);
    assert_eq!(restore, ScriptRestore::Nothing);
    let mut opening = Seen::default();
    run(&mut game, &mut script, OPENING, &mut opening, 0);

    // The grapple: the level's two unlock volumes (limit 1 and the latch,
    // then limit 2), found as the touch events that lead to a
    // `SeqAct_SetMaxGrapples`.
    let graph = script.runtime().graph().clone();
    let mut unlocks: Vec<(i32, asamu_game::asamu_kismet::ActorRef)> = Vec::new();
    for n in &graph.nodes {
        if n.class != asamu_game::asamu_kismet::OpClass::Touch {
            continue;
        }
        let Some(actor) = n
            .event
            .as_ref()
            .and_then(|e| e.originator.as_deref())
            .and_then(|p| graph.actor_by_path(p))
        else {
            continue;
        };
        for (target, _) in n.outputs.iter().flat_map(|o| o.links.iter()) {
            if let Some(t) = graph.node(*target)
                && t.class == asamu_game::asamu_kismet::OpClass::SetMaxGrapples
                && let Some(limit) = t.param("Grapples").map(|v| v.as_int())
            {
                unlocks.push((limit, actor));
            }
        }
    }
    unlocks.sort_by_key(|u| u.0);
    assert_eq!(
        unlocks.iter().map(|u| u.0).collect::<Vec<_>>(),
        vec![1, 2],
        "the level's two grapple unlocks"
    );
    for (_, actor) in &unlocks {
        script.runtime_mut().touch(*actor, true);
        run(&mut game, &mut script, 3, &mut Seen::default(), 0);
    }
    assert_eq!(abilities(&game), (2, false, true), "grapple picked up");

    // A collectible.
    let first_collectible = game.npcs().expect("NPCs").scene().collectibles[0].id;
    assert!(
        game.npcs_mut()
            .expect("NPCs")
            .set_collected(first_collectible, true)
    );

    // The checkpoints, in index order, to the fifth.
    let mut checkpoints = game
        .scene_map()
        .expect("a converted level")
        .actors
        .checkpoints
        .clone();
    checkpoints.sort_by_key(|c| c.index);
    assert!(checkpoints.len() >= 5);
    let mut saved = None;
    let mut session = SaveSession::in_memory();
    session.new_game().unwrap();
    session.begin_level(MAP, Some("Sanctuary")).unwrap();
    for c in checkpoints.iter().skip(1).take(4) {
        assert!(game.trigger_checkpoint(c.id), "{}", c.name);
        let t = script
            .tick(&mut game, &InputFrame::default())
            .expect("playing");
        if let Some(index) = checkpoint_saved(&t.report) {
            // What the app does then (`ui/flow.rs`, `process_ticks`).
            let mut snapshot = game.capture_snapshot(session.snapshot.as_ref());
            script.save_into(&mut snapshot);
            session.on_checkpoint_saved(snapshot).unwrap();
            saved = Some(index);
        }
    }
    let fifth = checkpoints[4].index;
    assert_eq!(saved, Some(fifth), "the fifth checkpoint wrote the save");
    let snapshot = session.snapshot.clone().expect("a snapshot");
    assert_eq!(
        snapshot.checkpoints.get(&ChapterId::Sanctuary),
        Some(&fifth)
    );

    if let Some(out) = std::env::var_os("ASAMU_CONTINUE_SAVE_OUT") {
        let dir = PathBuf::from(out).join("saves");
        let mut on_disk = SaveSession::open(SaveStore::new(&dir));
        on_disk.new_game().unwrap();
        on_disk.begin_level(MAP, Some("Sanctuary")).unwrap();
        on_disk.on_checkpoint_saved(snapshot.clone()).unwrap();
        eprintln!("the save was written to {}", dir.display());
    }
    Some((snapshot, fifth, opening))
}

#[test]
fn continue_at_the_fifth_checkpoint_does_not_play_the_opening() {
    let Some((snapshot, fifth, opening)) = play_to_the_fifth_checkpoint() else {
        return;
    };
    // The fresh start did play it: the view was taken from the first tick,
    // and the narrator spoke at the landing.
    assert_eq!(opening.cinematic.first(), Some(&1), "{opening:?}");
    assert!(!opening.cuts.is_empty(), "the camera flight: {opening:?}");
    assert!(
        opening.narrator.iter().any(|l| l == "Land"),
        "{:?}",
        opening.narrator
    );
    eprintln!(
        "fresh start: cinematic mode at tick {:?}, {} director cuts from tick {:?}, narrator {:?}",
        opening.cinematic.first(),
        opening.cuts.len(),
        opening.cuts.first(),
        opening.narrator
    );

    // The save holds the level script, and goes through the file's text.
    assert!(snapshot.script_saved);
    let state = snapshot.script_state().expect("a script state");
    eprintln!(
        "the save at checkpoint {fifth}: {} events, {} variables, {} Matinees, {} actors, {} bytes",
        state.events.len(),
        state.variables.len(),
        state.matinees.len(),
        snapshot.actors.len(),
        encode_document(&snapshot).unwrap().len()
    );
    let guards: Vec<_> = state
        .variables
        .iter()
        .filter(|(p, v)| p.contains("Intro_Sequence") && **v == VarState::Bool(true))
        .collect();
    assert!(!guards.is_empty(), "the opening's flag is saved as set");
    let read: Snapshot = decode_document(&encode_document(&snapshot).unwrap()).unwrap();
    assert_eq!(read, snapshot);

    // Continue.
    let (mut game, mut script) = load().expect("loaded a moment ago");
    let (index, restore) = start(&mut game, &mut script, &read);
    assert_eq!(index, fifth);
    assert!(
        matches!(restore, ScriptRestore::Saved { .. }),
        "{restore:?}"
    );
    // Before the first tick: the pawn at the checkpoint, with the saved
    // abilities and collectibles.
    let spawn = {
        let map = game.scene_map().unwrap();
        map.actors
            .checkpoints
            .iter()
            .find(|c| c.index == fifth)
            .unwrap()
            .spawn_location
    };
    let off = (game.player().position - spawn).truncate().length();
    assert!(off < 1.0, "{off} uu from the checkpoint's spawn point");
    assert_eq!(abilities(&game), (2, false, true));
    let taken = collected(&game);
    assert_eq!(taken.iter().filter(|c| **c).count(), 1);
    assert!(taken[0]);

    let mut first = Seen::default();
    run(&mut game, &mut script, TWO_SECONDS, &mut first, 0);
    assert!(
        first.view_is_the_player_s(),
        "the first two seconds after Continue: {first:?}"
    );
    assert!(first.narrator.is_empty(), "{:?}", first.narrator);
    assert!(
        first.drift < 5.0,
        "the pawn stands where it was put: {first:?}"
    );
    // Through what was the opening's whole length: still nothing of it.
    let mut rest = Seen::default();
    run(
        &mut game,
        &mut script,
        OPENING - TWO_SECONDS,
        &mut rest,
        TWO_SECONDS,
    );
    assert!(rest.view_is_the_player_s(), "{rest:?}");
    assert!(
        !rest.narrator.iter().any(|l| l == "Land"),
        "{:?}",
        rest.narrator
    );
    // The opening's chain of three Matinees (the fall, the camera flight,
    // the landing) was never started; what the level-loaded event starts on
    // every load runs, as in the original.
    eprintln!(
        "Matinees of the opening sequence started: fresh {:?}, after Continue {:?}",
        opening.intro, rest.intro
    );
    assert!(
        rest.intro.iter().all(|p| opening.intro.contains(p)),
        "{:?}",
        rest.intro
    );
    assert!(
        opening.intro.len() >= rest.intro.len() + 3,
        "fresh {:?}, after Continue {:?}",
        opening.intro,
        rest.intro
    );
    // The abilities and collectibles are still the saved ones: the level's
    // own start did not reset them, and the unlock volumes do not fire twice.
    assert_eq!(abilities(&game), (2, false, true));
    assert_eq!(collected(&game), taken);
    assert!(
        script.runtime().errors().is_empty(),
        "{:?}",
        script.runtime().errors()
    );
    // A save of the continued game holds the same flags.
    let mut again = game.capture_snapshot(Some(&read));
    script.save_into(&mut again);
    let again = again.script_state().unwrap();
    for (path, _) in guards {
        assert_eq!(
            again.variables.get(path),
            Some(&VarState::Bool(true)),
            "{path}"
        );
    }
}

/// The same Continue from a save that holds no script state, as our builds
/// up to 2026-10-12 wrote it (the owner's own save is one): the level's own
/// start flags stand in, so the opening does not play; and the control: the
/// same load with nothing restored does play it, which is what those builds
/// did.
#[test]
fn a_save_without_script_state_does_not_play_the_opening_either() {
    let Some((snapshot, fifth, _)) = play_to_the_fifth_checkpoint() else {
        return;
    };
    let mut old = snapshot.clone();
    old.kismet.clear();
    old.script_saved = false;

    let (mut game, mut script) = load().expect("loaded a moment ago");
    let (index, restore) = start(&mut game, &mut script, &old);
    assert_eq!(index, fifth);
    assert!(
        matches!(restore, ScriptRestore::LevelStartVariables { .. }),
        "{restore:?}"
    );
    let mut seen = Seen::default();
    run(&mut game, &mut script, OPENING, &mut seen, 0);
    assert!(seen.view_is_the_player_s(), "{seen:?}");
    assert!(
        !seen.narrator.iter().any(|l| l == "Land"),
        "{:?}",
        seen.narrator
    );
    assert_eq!(abilities(&game), (2, false, true));

    // The control: the level started at the same checkpoint with nothing of
    // its script restored.
    let (mut game, mut script) = load().expect("loaded a moment ago");
    let index = game.apply_snapshot(&old).checkpoint_index;
    assert_eq!(
        script.start_from_save(None, Some(index)),
        ScriptRestore::Nothing
    );
    game.start();
    let mut seen = Seen::default();
    run(&mut game, &mut script, OPENING, &mut seen, 0);
    assert!(
        !seen.view_is_the_player_s(),
        "the control plays the opening"
    );
    assert!(
        seen.narrator.iter().any(|l| l == "Land"),
        "{:?}",
        seen.narrator
    );
}

/// A death at the fifth checkpoint puts the pawn back there and loads
/// nothing: the level script runs on (the original's death sequence resets
/// the falling rocks, fires the death event and moves the pawn).
#[test]
fn a_death_after_continue_respawns_without_the_opening() {
    let Some((snapshot, fifth, _)) = play_to_the_fifth_checkpoint() else {
        return;
    };
    let (mut game, mut script) = load().expect("loaded a moment ago");
    start(&mut game, &mut script, &snapshot);
    let mut seen = Seen::default();
    run(&mut game, &mut script, 30, &mut seen, 0);
    let before = script.runtime().capture_state();
    game.kill_player();
    let mut respawned = false;
    for _ in 0..TWO_SECONDS {
        let t = script.tick(&mut game, &InputFrame::default()).unwrap();
        respawned |= t.report.respawned;
        for o in &t.outputs {
            assert!(
                !matches!(o, Output::MatineeCut { group: Some(_), .. }),
                "a camera cut after a death"
            );
        }
    }
    assert!(respawned);
    assert_eq!(game.latest_checkpoint_index(), Some(fifth));
    // The script ran on: the level's own death event wrote its variables
    // (the no-death achievement's flag and a flag of its sounds), and
    // nothing of the opening changed.
    let after = script.runtime().capture_state();
    let changed: Vec<&String> = after
        .variables
        .iter()
        .filter(|(path, value)| before.variables.get(*path) != Some(*value))
        .map(|(path, _)| path)
        .collect();
    eprintln!("variables a death changed: {changed:?}");
    assert!(
        changed.iter().any(|p| p.contains(".Achievement.")),
        "the level's death event ran: {changed:?}"
    );
    assert!(
        !changed.iter().any(|p| p.contains("Intro_Sequence")),
        "{changed:?}"
    );
    assert_eq!(after.variables.len(), before.variables.len());
    assert_eq!(abilities(&game), (2, false, true));
}
