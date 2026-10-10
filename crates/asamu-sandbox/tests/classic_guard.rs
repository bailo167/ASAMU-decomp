//! Guards that Classic is unchanged by the Sandbox.
//!
//! Classic (the Classic parameter set, the Classic constructors and the
//! unchanged tick) is the faithful target of the project. The Sandbox sits on
//! top of it, and these tests are what holds it to "on top of": a game that
//! never met a session, a game built by a pristine session, a game that was
//! handed the set it already runs, and a game a pristine session watched
//! every tick must be **the same game, bit for bit**.
//!
//! Every test here is differential. Nothing asserts a number: the control is
//! always the Classic path itself, run in this same process, so a legitimate
//! change to Classic (parity work revising a value or the physics) moves both
//! sides together and never breaks a guard, while any leak from the Sandbox
//! into Classic shows as a difference.
//!
//! What "the same" means below ([`Run`]): for every tick the tick's whole
//! report and the whole player state (script-layer state included), then the
//! recorded parity trace as bytes, the level objects, and finally the text of
//! the entire game value. Floats are compared through their exact shortest
//! form, so `-0.0` is not `0.0` here.
//!
//! None of this is evidence about the original game. It is evidence that the
//! Sandbox does not change what the recreation's Classic mode does.

use std::path::PathBuf;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_game::asamu_kismet::{Graph, LevelScripts, MatineeSet, RUNTIME_FORMAT, RUNTIME_VERSION};
use asamu_game::smoke::{self, DEFAULT_SEED, InputScript};
use asamu_game::{Game, LevelScript, TickReport, load_level_with_kismet};
use asamu_player::{InputFrame, PlayerParams, Trace};
use asamu_sandbox::command::{Command, Toggle};
use asamu_sandbox::keys::{Catalog, TuneValue};
use asamu_sandbox::overlay::{
    OVERRIDE_NOTE_PREFIX, Overlay, ParamSetLabel, overridden_keys, param_set_label,
};
use asamu_sandbox::profile::Profile;
use asamu_sandbox::recording::{
    RECORDING_FORMAT, RecordingHeader, SandboxRecording, TRACE_LEVEL_PREFIX, TRACE_NOTE_NOT_PARITY,
};
use asamu_sandbox::relatch::{RelatchReport, relatch, retune};
use asamu_sandbox::rules::Rules;
use asamu_sandbox::session::{Session, SimCx};
use asamu_sandbox::snapshot::SimSnapshot;
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, json};
use asamu_world::graybox_test_level;
use asamu_world::scene::{self, LoadOptions, MemorySource};
use glam::Vec3;

/// Ticks of the standard run (50 s at the default rate): long enough for the
/// input script to walk, sprint, jump, grapple, fall and respawn many times.
const TICKS: usize = 3000;

// ---------------------------------------------------------------------------
// The harness.
// ---------------------------------------------------------------------------

/// Everything observable about one run of a game.
struct Run {
    /// The reports, typed (to check that the run was eventful).
    reports: Vec<TickReport>,
    /// One line per tick: the tick's report and the scripted outputs, exact.
    report_lines: Vec<String>,
    /// One line per tick: the whole player state after the tick, exact.
    player_lines: Vec<String>,
    /// The parity trace the game recorded during the run, as written.
    trace: String,
    /// The level objects after the run.
    objects: String,
    /// The whole game value after the run, as text (see the module docs).
    game_text: String,
}

/// What a run does around the unchanged tick.
trait Hooks {
    /// Before the tick with this index (0-based).
    fn before(&mut self, _index: usize, _game: &mut Game, _script: &mut Option<LevelScript>) {}
    /// After that tick.
    fn after(&mut self, _game: &Game, _script: Option<&LevelScript>, _report: &TickReport) {}
}

/// The control: nothing but the tick.
struct Untouched;
impl Hooks for Untouched {}

/// A pristine session doing everything a host does around each tick.
struct Watched(Session);
impl Hooks for Watched {
    fn before(&mut self, index: usize, game: &mut Game, script: &mut Option<LevelScript>) {
        let mut cx = SimCx { game, script };
        assert_eq!(
            self.0.reconcile(&mut cx),
            Ok(false),
            "a pristine session never retunes (tick {index})"
        );
        self.0.before_tick(&mut cx);
    }

    fn after(&mut self, game: &Game, script: Option<&LevelScript>, report: &TickReport) {
        self.0.after_tick(game, script, report);
    }
}

fn exact<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("simulation state serializes")
}

/// The inputs of a run: the smoke suite's pseudo-random input script, with a
/// few deliberate actions laid over it in a fixed cycle, so that every run
/// really jumps, fires the rocket boots, and attaches, holds and releases
/// the grapple (random looking alone almost never finds a grapple target,
/// and a guard run that never grapples cannot notice a change to the
/// grapple budget).
///
/// The deliberate part reads the game it drives, to aim. The inputs are
/// therefore a pure function of the game's state: two games that are the
/// same get the same inputs, and two that differ drift apart even faster.
/// The driver never writes the game.
#[derive(Clone)]
struct Driver {
    script: InputScript,
    tick: usize,
}

impl Driver {
    /// Ticks of one cycle of deliberate actions (ours).
    const CYCLE: usize = 240;
    /// Jump, then press jump again in the air (the rocket-boost key).
    const JUMP: usize = 30;
    const BOOST: usize = 45;
    /// Look at the nearest grapple target with the button up, then hold it.
    const AIM: usize = 120;
    const HOLD_UNTIL: usize = 180;

    fn new(seed: u64) -> Self {
        Self {
            script: InputScript::new(seed),
            tick: 0,
        }
    }

    /// The standard driver.
    fn standard() -> Self {
        Self::new(DEFAULT_SEED)
    }

    /// The things worth aiming at: the level's grapple points and recharge
    /// crystals (converted levels list their crystals too).
    fn nearest_target(game: &Game) -> Option<Vec3> {
        let eye = game.eye_position();
        let level = game.level();
        level
            .grapple_points
            .iter()
            .map(|point| point.position)
            .chain(level.crystals.iter().map(|crystal| crystal.center))
            .min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye)))
    }

    /// The input of the next tick of `game`.
    fn input_for(&mut self, game: &Game) -> InputFrame {
        let mut input = self.script.next_frame();
        let phase = self.tick % Self::CYCLE;
        self.tick += 1;
        let player = game.player();
        if phase == Self::JUMP {
            input.jump_pressed = true;
            input.jump_held = true;
        } else if phase > Self::JUMP && phase < Self::BOOST {
            input.jump_held = true;
        } else if phase == Self::BOOST {
            input.jump_pressed = !player.grounded;
        } else if phase == Self::AIM {
            // The fire trace reads the view as the previous tick left it,
            // so: look now, press on the next tick.
            input.grapple_held = false;
            if let Some(target) = Self::nearest_target(game) {
                let to = target - game.eye_position();
                input.look_yaw_delta = to.y.atan2(to.x) - player.yaw;
                input.look_pitch_delta = to.z.atan2(to.truncate().length()) - player.pitch;
            }
        } else if phase > Self::AIM && phase <= Self::HOLD_UNTIL {
            input.look_yaw_delta = 0.0;
            input.look_pitch_delta = 0.0;
            input.grapple_held = true;
        } else if game.crosshair() {
            // Outside the cycle's own actions: fire whenever the crosshair
            // says a grapple would hold.
            input.grapple_held = true;
        }
        input
    }

    /// Drives `game` for `ticks` ticks without observing anything.
    fn advance(&mut self, game: &mut Game, ticks: usize) {
        for _ in 0..ticks {
            let input = self.input_for(game);
            game.tick(&input).expect("the game is playing");
        }
    }
}

/// Runs `ticks` ticks of the standard driver on a started game, the way
/// every host does (through the level script when there is one), recording
/// a parity trace, and returns everything observable.
fn run(
    game: &mut Game,
    script: &mut Option<LevelScript>,
    ticks: usize,
    hooks: &mut dyn Hooks,
) -> Run {
    run_from(game, script, Driver::standard(), ticks, hooks)
}

/// [`run`] with the inputs continued from `driver`.
fn run_from(
    game: &mut Game,
    script: &mut Option<LevelScript>,
    driver: Driver,
    ticks: usize,
    hooks: &mut dyn Hooks,
) -> Run {
    let mut out = run_without_game_text(game, script, driver, ticks, hooks);
    out.game_text = format!("{game:?}");
    out
}

/// [`run_from`] leaving [`Run::game_text`] empty: for games whose value
/// holds a whole converted map of the user's, which is far too large to
/// print (and is immutable, shared data anyway).
fn run_without_game_text(
    game: &mut Game,
    script: &mut Option<LevelScript>,
    mut driver: Driver,
    ticks: usize,
    hooks: &mut dyn Hooks,
) -> Run {
    game.start_recording();
    let mut out = Run {
        reports: Vec::with_capacity(ticks),
        report_lines: Vec::with_capacity(ticks),
        player_lines: Vec::with_capacity(ticks),
        trace: String::new(),
        objects: String::new(),
        game_text: String::new(),
    };
    for index in 0..ticks {
        // The input is decided from the game as the previous tick left it,
        // before the hooks get to touch anything.
        let input = driver.input_for(game);
        hooks.before(index, game, script);
        let (report, scripted) = match script.as_mut() {
            Some(level_script) => {
                let tick = level_script
                    .tick(game, &input)
                    .expect("the game is playing");
                let scripted = format!("{:?} {:?}", tick.outputs, tick.npc_events);
                (tick.report, scripted)
            }
            None => (
                game.tick(&input).expect("the game is playing"),
                String::new(),
            ),
        };
        hooks.after(game, script.as_ref(), &report);
        out.report_lines
            .push(format!("{} {scripted}", exact(&report)));
        out.player_lines.push(exact(game.player()));
        out.reports.push(report);
    }
    out.trace = game
        .stop_recording()
        .expect("the run was recording")
        .to_jsonl_string()
        .expect("a recorded trace is valid");
    out.objects = exact(game.objects());
    out
}

/// The first line on which two line lists differ, for a readable failure.
fn first_difference(control: &[String], other: &[String]) -> Option<String> {
    if control.len() != other.len() {
        return Some(format!(
            "{} lines instead of {}",
            other.len(),
            control.len()
        ));
    }
    control
        .iter()
        .zip(other)
        .position(|(a, b)| a != b)
        .map(|i| {
            format!(
                "first difference at line {}:\n  control: {}\n  other:   {}",
                i + 1,
                clip(&control[i]),
                clip(&other[i])
            )
        })
}

fn clip(line: &str) -> String {
    const MAX: usize = 600;
    if line.len() <= MAX {
        return line.to_owned();
    }
    let mut end = MAX;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... ({} bytes)", &line[..end], line.len())
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Fails unless `other` is the same run as `control`, bit for bit.
fn assert_identical(control: &Run, other: &Run, what: &str) {
    if let Some(diff) = first_difference(&control.report_lines, &other.report_lines) {
        panic!("{what}: the tick reports differ; {diff}");
    }
    if let Some(diff) = first_difference(&control.player_lines, &other.player_lines) {
        panic!("{what}: the player state differs; {diff}");
    }
    if let Some(diff) = first_difference(&lines(&control.trace), &lines(&other.trace)) {
        panic!("{what}: the recorded trace differs; {diff}");
    }
    assert!(
        control.trace == other.trace,
        "{what}: the recorded trace differs in its line endings"
    );
    assert!(
        control.objects == other.objects,
        "{what}: the level objects differ\n  control: {}\n  other:   {}",
        clip(&control.objects),
        clip(&other.objects)
    );
    assert!(
        control.game_text == other.game_text,
        "{what}: the game values differ somewhere outside the player, the objects and the trace"
    );
}

/// Fails unless the two runs differ: the harness can tell a changed game
/// from an unchanged one (used to show the guards are not vacuous).
fn assert_different(control: &Run, other: &Run, what: &str) {
    assert!(
        control.player_lines != other.player_lines && control.trace != other.trace,
        "{what}: expected the run to differ from the control, and it did not"
    );
}

/// The standard run must exercise the systems, or equality proves little:
/// walking, jumping and landing, the grapple attaching and releasing (which
/// spends the grapple budget) and the rocket boots.
fn assert_eventful(run: &Run, what: &str) {
    let count = |f: fn(&TickReport) -> bool| run.reports.iter().filter(|r| f(r)).count();
    assert!(count(|r| r.events.jumped) >= 1, "{what}: no jumps");
    assert!(
        count(|r| r.events.landing.is_some()) >= 1,
        "{what}: no landings"
    );
    assert!(
        count(|r| r.events.gun.attached.is_some()) >= 1,
        "{what}: the grapple never attached"
    );
    assert!(
        count(|r| r.events.gun.released.is_some()) >= 1,
        "{what}: the grapple never released"
    );
    assert!(
        count(|r| r.events.boots.is_some()) >= 1,
        "{what}: the rocket boots never fired"
    );
    let first = run.player_lines.first();
    assert!(
        run.player_lines.iter().any(|line| Some(line) != first),
        "{what}: the player never changed"
    );
    // Shown with `--nocapture`.
    eprintln!(
        "{what}: {} ticks, {} jumps, {} landings, {} grapple attaches, {} releases, \
         {} boots events, {} respawns, {} deaths",
        run.reports.len(),
        count(|r| r.events.jumped),
        count(|r| r.events.landing.is_some()),
        count(|r| r.events.gun.attached.is_some()),
        count(|r| r.events.gun.released.is_some()),
        count(|r| r.events.boots.is_some()),
        count(|r| r.respawned),
        count(|r| r.died.is_some()),
    );
}

/// A started Classic graybox game, built by the Classic constructor.
fn classic_graybox() -> Game {
    let mut game = Game::graybox().expect("the graybox level is valid");
    game.start();
    game
}

/// The standard control run on the Classic graybox.
fn graybox_control() -> Run {
    let control = run(&mut classic_graybox(), &mut None, TICKS, &mut Untouched);
    assert_eventful(&control, "graybox control");
    control
}

/// One valid, non-Classic value for `key`: a nudge up, else down, else the
/// next choice. `None` when the key has no other valid value.
fn another_value(key: &str) -> Option<TuneValue> {
    for steps in [1, -1, 2, -2] {
        let mut probe = Overlay::default();
        if probe.nudge(key, steps, 1.0).is_ok() && !probe.is_empty() {
            return probe.get(key).cloned();
        }
    }
    None
}

/// An overlay changing several values the Classic pipeline reads.
fn tuned_overlay() -> Overlay {
    let mut overlay = Overlay::default();
    for key in [
        "movement.custom_gravity_scaling",
        "movement.ground_acceleration",
        "movement.jump_velocity",
        "pawn.move_speed",
    ] {
        let value = another_value(key).expect("the key can be tuned");
        overlay.set(key, value).expect("the tuned set is valid");
    }
    overlay
}

// ---------------------------------------------------------------------------
// The parameter set.
// ---------------------------------------------------------------------------

#[test]
fn empty_overlay_is_the_classic_set() {
    let classic = PlayerParams::asamu_original();
    let applied = Overlay::default().apply().unwrap();
    // Values and provenance: `PlayerParams` equality compares both.
    assert_eq!(applied, classic);
    assert_eq!(
        applied.provenance_markdown_table(),
        classic.provenance_markdown_table()
    );
    assert_eq!(applied.provenance_report(), classic.provenance_report());
    assert_eq!(applied.placeholder_names(), classic.placeholder_names());
    assert_eq!(
        serde_json::to_string(&applied).unwrap(),
        serde_json::to_string(&classic).unwrap()
    );
    assert_eq!(param_set_label(&applied), ParamSetLabel::Classic);
    assert!(overridden_keys(&applied).is_empty());

    // The same through every way an empty overlay comes into being.
    let from_json: Overlay = serde_json::from_str("{}").unwrap();
    assert_eq!(from_json.apply().unwrap(), classic);
    assert_eq!(
        Overlay::default().normalized().unwrap().apply().unwrap(),
        classic
    );
    assert_eq!(Profile::classic().overrides.apply().unwrap(), classic);
    assert_eq!(*Session::classic().params(), classic);
    assert_eq!(*Session::new(Profile::classic()).unwrap().params(), classic);
    assert_eq!(Session::classic().label(), ParamSetLabel::Classic);

    // And the Classic constructor still hands out the Classic set.
    assert_eq!(*Game::graybox().unwrap().params(), classic);
    assert_eq!(
        param_set_label(Game::graybox().unwrap().params()),
        ParamSetLabel::Classic
    );
}

#[test]
fn set_then_clear_restores_the_classic_set() {
    let classic = PlayerParams::asamu_original();
    let catalog = Catalog::classic();
    let mut tuned_keys = 0;
    for info in catalog.iter() {
        let Some(value) = another_value(&info.key) else {
            // A choice with a single name: nothing else to set it to.
            continue;
        };
        tuned_keys += 1;
        let mut overlay = Overlay::default();
        overlay.set(&info.key, value).unwrap();
        let tuned = overlay.apply().unwrap();
        assert_ne!(tuned, classic, "{}", info.key);
        assert_eq!(overridden_keys(&tuned), vec![info.key.clone()]);

        // Cleared: the Classic set again, provenance included.
        assert!(overlay.clear(&info.key));
        assert!(overlay.is_empty());
        assert_eq!(overlay.apply().unwrap(), classic, "{}", info.key);

        // Set back to the Classic value: the entry is gone, not kept as an
        // override that happens to hold the Classic number.
        let mut overlay = Overlay::default();
        overlay
            .set(&info.key, another_value(&info.key).unwrap())
            .unwrap();
        overlay.set(&info.key, info.classic.clone()).unwrap();
        assert!(overlay.is_empty(), "{}", info.key);
        assert_eq!(overlay.apply().unwrap(), classic, "{}", info.key);

        // Nudged away and back.
        let mut overlay = Overlay::default();
        if overlay.nudge(&info.key, 1, 1.0).is_ok() && !overlay.is_empty() {
            overlay.nudge(&info.key, -1, 1.0).unwrap();
            assert!(overlay.is_empty(), "{}: up then down", info.key);
            assert_eq!(overlay.apply().unwrap(), classic, "{}", info.key);
        }
    }
    assert!(
        tuned_keys + 4 >= catalog.len(),
        "nearly every key was exercised ({tuned_keys} of {})",
        catalog.len()
    );

    // Many at once, then all cleared.
    let mut overlay = tuned_overlay();
    assert!(overlay.len() >= 4);
    assert_ne!(overlay.apply().unwrap(), classic);
    overlay.clear_all();
    assert_eq!(overlay.apply().unwrap(), classic);
    assert_eq!(
        overlay.apply().unwrap().provenance_markdown_table(),
        classic.provenance_markdown_table()
    );
}

#[test]
fn a_tuned_set_is_never_labelled_classic() {
    let classic = PlayerParams::asamu_original();
    let classic_placeholders = classic.placeholder_names();
    for info in Catalog::classic().iter() {
        let Some(value) = another_value(&info.key) else {
            continue;
        };
        let mut overlay = Overlay::default();
        overlay.set(&info.key, value).unwrap();
        let tuned = overlay.apply().unwrap();

        // By label, by key list ...
        assert_eq!(
            param_set_label(&tuned),
            ParamSetLabel::Modified { overrides: 1 },
            "{}",
            info.key
        );
        assert_eq!(overridden_keys(&tuned), vec![info.key.clone()]);

        // ... and in the set itself: the changed value carries placeholder
        // provenance whose note starts with the override prefix, so every
        // existing report (the placeholder count of the app's banner, the
        // provenance table) shows it as not original without knowing about
        // the Sandbox.
        let entry = tuned
            .provenance_report()
            .into_iter()
            .find(|e| e.name == info.key)
            .unwrap();
        match &entry.provenance {
            asamu_core::Provenance::Placeholder { note } => {
                assert!(
                    note.starts_with(OVERRIDE_NOTE_PREFIX),
                    "{}: {note}",
                    info.key
                );
            }
            other => panic!("{}: an override with provenance {other}", info.key),
        }
        let placeholders = tuned.placeholder_names();
        assert!(placeholders.contains(&info.key), "{}", info.key);
        assert!(placeholders.len() >= classic_placeholders.len());
        assert!(
            tuned
                .provenance_markdown_table()
                .contains(OVERRIDE_NOTE_PREFIX),
            "{}",
            info.key
        );

        // A session on that overlay says so too.
        let session = Session::new(Profile {
            overrides: overlay,
            ..Profile::classic()
        })
        .unwrap();
        assert_eq!(session.label(), ParamSetLabel::Modified { overrides: 1 });
        assert!(!session.is_pristine());
        assert!(!session.profile().is_pristine());
    }
    // The Classic set itself has no value marked as an override.
    assert!(
        !classic
            .provenance_markdown_table()
            .contains(OVERRIDE_NOTE_PREFIX)
    );
}

// ---------------------------------------------------------------------------
// The running game.
// ---------------------------------------------------------------------------

#[test]
fn pristine_session_game_is_game_graybox() {
    let control = graybox_control();

    let session = Session::classic();
    assert!(session.is_pristine());
    let mut ours = session
        .new_game(graybox_test_level())
        .expect("the graybox level is valid");
    // The same game before the first tick ...
    assert!(
        format!("{ours:?}") == format!("{:?}", Game::graybox().unwrap()),
        "the session's game differs from Game::graybox() at construction"
    );
    assert_eq!(ours.clock().tick_rate_hz(), DEFAULT_TICK_RATE_HZ);
    ours.start();
    // ... and after 3,000 of them.
    let observed = run(&mut ours, &mut None, TICKS, &mut Untouched);
    assert_identical(&control, &observed, "a pristine session's game");
    assert_eq!(*ours.params(), PlayerParams::asamu_original());

    // The same through a session made from the classic profile.
    let from_profile = Session::new(Profile::classic()).unwrap();
    let mut game = from_profile.new_game(graybox_test_level()).unwrap();
    game.start();
    let observed = run(&mut game, &mut None, TICKS, &mut Untouched);
    assert_identical(&control, &observed, "the classic profile's game");
}

#[test]
fn noop_set_params_mid_run_changes_no_bit() {
    /// Hands the game the set it already runs.
    struct Noop {
        every: usize,
    }
    impl Hooks for Noop {
        fn before(&mut self, index: usize, game: &mut Game, _: &mut Option<LevelScript>) {
            if index > 0 && index.is_multiple_of(self.every) {
                assert_eq!(game.set_params(PlayerParams::asamu_original()), Ok(()));
                let same = game.params().clone();
                assert_eq!(game.set_params(same), Ok(()));
            }
        }
    }
    let control = graybox_control();
    // At tick 500 (and every 500 after it).
    let once = run(
        &mut classic_graybox(),
        &mut None,
        TICKS,
        &mut Noop { every: 500 },
    );
    assert_identical(&control, &once, "set_params(same set) every 500 ticks");
    // And before every single tick.
    let always = run(
        &mut classic_graybox(),
        &mut None,
        TICKS,
        &mut Noop { every: 1 },
    );
    assert_identical(&control, &always, "set_params(same set) every tick");
}

#[test]
fn pristine_parameter_calls_are_inert() {
    /// Every parameter-side entry point with nothing to do.
    struct Idle;
    impl Hooks for Idle {
        fn before(&mut self, index: usize, game: &mut Game, _: &mut Option<LevelScript>) {
            let classic = PlayerParams::asamu_original();
            assert_eq!(
                retune(game, classic.clone()),
                Ok(RelatchReport::default()),
                "tick {index}"
            );
            assert_eq!(
                retune(game, Overlay::default().apply().unwrap()),
                Ok(RelatchReport::default())
            );
            assert_eq!(
                relatch(game.player_mut(), &classic, &classic),
                RelatchReport::default()
            );
            assert_eq!(Rules::default().enforce(game), 0);
        }
    }
    let control = graybox_control();
    let observed = run(&mut classic_graybox(), &mut None, TICKS, &mut Idle);
    assert_identical(
        &control,
        &observed,
        "retune / relatch / enforce with nothing to do",
    );
}

#[test]
fn pristine_hooks_are_inert() {
    let control = graybox_control();
    let mut watched = Watched(Session::classic());
    let observed = run(&mut classic_graybox(), &mut None, TICKS, &mut watched);
    assert_identical(
        &control,
        &observed,
        "a pristine session watching every tick",
    );
    // Watching is not acting: the session is still pristine and Classic.
    assert!(watched.0.is_pristine());
    assert_eq!(watched.0.label(), ParamSetLabel::Classic);
    assert!(watched.0.log().is_empty());
    assert!(watched.0.time().is_default());
}

/// The harness is able to see a difference: the same run with a tuned set,
/// or with a rule in force, is **not** the control. (Without this, every
/// equality above could be the harness being blind.)
#[test]
fn the_harness_tells_a_tuned_game_from_classic() {
    let control = graybox_control();

    // A game built with a tuned set.
    let tuned = tuned_overlay().apply().unwrap();
    let mut game = Game::new(graybox_test_level(), tuned.clone(), DEFAULT_TICK_RATE_HZ).unwrap();
    game.start();
    let built = run(&mut game, &mut None, TICKS, &mut Untouched);
    assert_different(&control, &built, "a game built with a tuned set");

    // A Classic game retuned at tick 500: identical up to there, not after.
    struct RetuneAt(usize, PlayerParams);
    impl Hooks for RetuneAt {
        fn before(&mut self, index: usize, game: &mut Game, _: &mut Option<LevelScript>) {
            if index == self.0 {
                let report = retune(game, self.1.clone()).expect("the tuned set is valid");
                assert!(!report.relatched.is_empty(), "latched values followed");
            }
        }
    }
    let retuned = run(
        &mut classic_graybox(),
        &mut None,
        TICKS,
        &mut RetuneAt(500, tuned),
    );
    assert!(
        control.player_lines[..500] == retuned.player_lines[..500],
        "nothing differs before the retune"
    );
    assert_different(&control, &retuned, "a game retuned at tick 500");

    // A rule in force.
    struct Enforce(Rules);
    impl Hooks for Enforce {
        fn before(&mut self, _: usize, game: &mut Game, _: &mut Option<LevelScript>) {
            self.0.enforce(game);
        }
    }
    let rules = Rules {
        grapples: asamu_sandbox::rules::GrappleRule::Fixed(0),
        rocket_boots: asamu_sandbox::rules::Switch::Off,
        auto_refill: false,
    };
    let ruled = run(
        &mut classic_graybox(),
        &mut None,
        TICKS,
        &mut Enforce(rules),
    );
    assert_different(&control, &ruled, "a game with rules in force");
}

/// Building, running and retuning a tuned game leaves nothing behind: the
/// Classic set, the catalogue and a Classic game made afterwards are what
/// they were before.
#[test]
fn a_tuned_session_leaves_nothing_behind_for_classic() {
    let classic_before = PlayerParams::asamu_original();
    let catalog_before = Catalog::classic();
    let control = graybox_control();

    // A tuned session: its own game, run with the session's hooks, with a
    // second retune half way and its rules in force.
    let profile = Profile {
        name: "guard".to_owned(),
        overrides: tuned_overlay(),
        rules: Rules {
            grapples: asamu_sandbox::rules::GrappleRule::Unlimited,
            rocket_boots: asamu_sandbox::rules::Switch::On,
            auto_refill: true,
        },
        ..Profile::classic()
    };
    let session = Session::new(profile).unwrap();
    assert!(!session.is_pristine());
    let mut game = session.new_game(graybox_test_level()).unwrap();
    game.start();
    struct Tuned(Session);
    impl Hooks for Tuned {
        fn before(&mut self, index: usize, game: &mut Game, script: &mut Option<LevelScript>) {
            let mut cx = SimCx { game, script };
            self.0
                .reconcile(&mut cx)
                .expect("the session's set applies");
            self.0.before_tick(&mut cx);
            if index == 300 {
                // The game falls back to Classic behind the session's back;
                // the next reconcile brings the tuned set in again.
                retune(game, PlayerParams::asamu_original()).expect("valid");
            }
        }
        fn after(&mut self, game: &Game, script: Option<&LevelScript>, report: &TickReport) {
            self.0.after_tick(game, script, report);
        }
    }
    let tuned = run(&mut game, &mut None, 900, &mut Tuned(session));
    assert!(tuned.player_lines[..] != control.player_lines[..900]);

    // Classic afterwards: untouched.
    assert_eq!(PlayerParams::asamu_original(), classic_before);
    assert_eq!(Catalog::classic(), catalog_before);
    assert_eq!(Overlay::default().apply().unwrap(), classic_before);
    let again = graybox_control();
    assert_identical(&control, &again, "Classic after a tuned session");
}

// ---------------------------------------------------------------------------
// Save states.
// ---------------------------------------------------------------------------

#[test]
fn snapshot_resume_is_bit_identical_and_isolated() {
    const BEFORE: usize = 700;
    const AFTER: usize = 900;
    const DETOUR: usize = 400;

    /// A Classic graybox game `BEFORE` ticks in (not recording), and the
    /// driver as it stands then.
    fn advanced() -> (Game, Driver) {
        let mut game = classic_graybox();
        let mut driver = Driver::standard();
        driver.advance(&mut game, BEFORE);
        (game, driver)
    }

    // The uninterrupted run from tick BEFORE on.
    let (mut uninterrupted, driver) = advanced();
    let control = run_from(
        &mut uninterrupted,
        &mut None,
        driver.clone(),
        AFTER,
        &mut Untouched,
    );
    assert_eventful(&control, "snapshot control");

    // Capture at BEFORE (while a recording runs: the slot must not carry it).
    let (mut game, _) = advanced();
    game.start_recording();
    let slot = SimSnapshot::capture(&game, None, "guard");
    drop(game.stop_recording());
    assert_eq!(slot.tick(), BEFORE as u64);
    let slot_text = format!("{slot:?}");

    // The live game runs on, somewhere else entirely.
    let mut script = None;
    let detour = run_from(
        &mut game,
        &mut script,
        Driver::new(DEFAULT_SEED ^ 0xA5A5),
        DETOUR,
        &mut Untouched,
    );
    assert!(
        detour.player_lines.last() != control.player_lines.get(DETOUR - 1),
        "the detour went somewhere else"
    );
    assert_eq!(
        format!("{slot:?}"),
        slot_text,
        "the slot changed while the live game ran"
    );

    // Restore and continue: the uninterrupted run, bit for bit.
    for attempt in 1..=2 {
        slot.restore(&mut SimCx {
            game: &mut game,
            script: &mut script,
        })
        .expect("a slot of the same level restores");
        assert!(!game.is_recording(), "a slot never carries a recording");
        assert_eq!(game.clock().tick(), BEFORE as u64);
        let resumed = run_from(
            &mut game,
            &mut script,
            driver.clone(),
            AFTER,
            &mut Untouched,
        );
        assert_identical(
            &control,
            &resumed,
            &format!("resuming from the slot (attempt {attempt})"),
        );
        // Neither the restore nor the resumed run touched the slot, so it
        // can be loaded again.
        assert_eq!(format!("{slot:?}"), slot_text, "attempt {attempt}");
    }
}

// ---------------------------------------------------------------------------
// Converted levels.
// ---------------------------------------------------------------------------

/// A synthetic converted level (triangle collision; written here, no game
/// data): a floor, a raised deck, blocks to grapple and climb, a recharge
/// crystal, a falling rock, checkpoints, a trigger and a pit with a kill
/// zone. The name and every size are ours.
fn fixture_source() -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("SandboxGuardMap", -6000.0);
    // Two BSP quads: the floor, and a deck 300 above part of it.
    s.set_bsp(
        vec![
            [-4000.0, -4000.0, 0.0],
            [4000.0, -4000.0, 0.0],
            [4000.0, 4000.0, 0.0],
            [-4000.0, 4000.0, 0.0],
            [1200.0, -900.0, 300.0],
            [2600.0, -900.0, 300.0],
            [2600.0, 900.0, 300.0],
            [1200.0, 900.0, 300.0],
        ],
        vec![[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]],
    );
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.checkpoint(
        Place::at(Vec3::new(0.0, 0.0, 40.0)),
        120.0,
        60.0,
        0,
        json!({}),
    );
    s.checkpoint(
        Place::at(Vec3::new(900.0, 300.0, 40.0)),
        150.0,
        60.0,
        1,
        json!({}),
    );
    s.trigger_volume(
        Vec3::new(300.0, -300.0, 0.0),
        Vec3::new(700.0, 300.0, 200.0),
    );
    s.kill_zone(
        Vec3::new(-4000.0, 2500.0, -200.0),
        Vec3::new(4000.0, 4000.0, 60.0),
    );
    s.blocking_volume(
        Vec3::new(-900.0, -200.0, 0.0),
        Vec3::new(-800.0, 200.0, 180.0),
    );
    for (i, at) in [
        Vec3::new(600.0, 0.0, 50.0),
        Vec3::new(800.0, -500.0, 420.0),
        Vec3::new(-500.0, 600.0, 520.0),
        Vec3::new(1500.0, 200.0, 700.0),
    ]
    .into_iter()
    .enumerate()
    {
        let scale = Vec3::splat(1.0 + i as f32 * 0.5);
        s.static_mesh(
            "Guard.Block",
            Place {
                location: at,
                rotation: [0, 4096 * i as i32, 0],
                scale,
            },
        );
    }
    s.mesh_actor(
        "asamu.ASAMURechargeCrystal",
        "recharge_crystal",
        "Guard.Block",
        Place::at(Vec3::new(1000.0, 0.0, 600.0)),
        "COLLIDE_BlockAll",
        None,
        json!({"RechargeDelay": 2.0}),
    );
    s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Guard.Block",
        Place::at(Vec3::new(0.0, -1200.0, 900.0)),
        "COLLIDE_CustomDefault",
        None,
        json!({"fallDistance": 600.0, "fallingLowerRate": 300.0, "fallingHigherRate": 300.0}),
    );
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (vertices, triangles) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Guard.Block", "SandboxGuardMap", vertices, triangles);
    meshes.write(&mut src, true);
    src
}

/// A started Classic game on the synthetic converted level, built by the
/// Classic constructor for loaded maps. The abilities are switched on the
/// way a level's Kismet does, identically for every game.
fn fixture_game() -> Game {
    let loaded = scene::load_map(
        &fixture_source(),
        "SandboxGuardMap",
        &LoadOptions::default(),
    )
    .expect("the fixture loads");
    let mut game =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)
            .expect("the fixture has a player start");
    game.set_max_grapples(3);
    game.enable_rocket_boots(true);
    game.start();
    game
}

#[test]
fn converted_fixture_is_unchanged_by_a_pristine_session() {
    let control = run(&mut fixture_game(), &mut None, TICKS, &mut Untouched);
    assert_eventful(&control, "converted fixture control");
    assert!(
        fixture_game().scene_map().is_some(),
        "the fixture runs the converted-level tick"
    );

    let mut watched = Watched(Session::classic());
    let observed = run(&mut fixture_game(), &mut None, TICKS, &mut watched);
    assert_identical(
        &control,
        &observed,
        "a pristine session on a converted level",
    );
    assert!(watched.0.is_pristine());

    // And the harness is not blind on this tick path either.
    let mut tuned = fixture_game();
    retune(&mut tuned, tuned_overlay().apply().unwrap()).unwrap();
    let tuned = run(&mut tuned, &mut None, TICKS, &mut Untouched);
    assert_different(&control, &tuned, "a tuned game on the converted level");
}

/// A level script written here (ours): at level start it sets the grapple
/// budget and switches the rocket boots on, which is what a level's own
/// start events do inside the first scripted tick.
fn level_start_script() -> LevelScripts {
    let action = |id: usize, class: &str, next: Option<usize>, params: &str| {
        let links = next.map_or(String::new(), |n| format!(r#"{{"op": {n}, "input": 0}}"#));
        format!(
            r#"{{"id": {id}, "class": "{class}", "kind": "action", "parent": 0,
                "inputs": [{{"desc": "In"}}], "outputs": [{{"desc": "Out", "links": [{links}]}}],
                "params": {params}, "auto_activate_outputs": true}}"#
        )
    };
    let nodes = format!(
        r#"[
            {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3]}},
            {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
              "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
              "event": {{"max_trigger_count": 1}}}},
            {}, {}
        ]"#,
        action(
            2,
            "asamu.SeqAct_SetMaxGrapples",
            Some(3),
            r#"{"Grapples": 2}"#
        ),
        action(
            3,
            "asamu.SeqAct_ToggleRocketBoots",
            None,
            r#"{"Enable": true}"#
        ),
    );
    let document = format!(
        r#"{{"format": "{RUNTIME_FORMAT}", "version": {RUNTIME_VERSION}, "package": "Guard",
            "nodes": {nodes}, "actors": []}}"#
    );
    LevelScripts {
        graph: Graph::from_json_slice(document.as_bytes()).expect("the test graph is valid"),
        matinee: MatineeSet::default(),
        missing_sublevels: Vec::new(),
    }
}

/// A started Classic game with a level script, ticked the way the app ticks
/// scripted levels.
fn scripted_game() -> (Game, Option<LevelScript>) {
    let mut game = fixture_game();
    let script = LevelScript::new(&mut game, level_start_script());
    (game, Some(script))
}

#[test]
fn scripted_level_is_unchanged_by_a_pristine_session() {
    let (mut game, mut script) = scripted_game();
    let control = run(&mut game, &mut script, TICKS, &mut Untouched);
    assert_eventful(&control, "scripted control");
    // The script really ran: its level-start actions set the abilities.
    assert_eq!(game.player().script.gun.max_grapples, 2);
    assert!(game.player().script.boots.enabled);
    let errors = script
        .as_ref()
        .map(|s| s.runtime().errors().to_vec())
        .unwrap_or_default();
    assert!(errors.is_empty(), "{errors:?}");

    let (mut game, mut script) = scripted_game();
    let mut watched = Watched(Session::classic());
    let observed = run(&mut game, &mut script, TICKS, &mut watched);
    assert_identical(
        &control,
        &observed,
        "a pristine session on a scripted level",
    );
    assert!(watched.0.is_pristine());
}

fn converted_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    dir.join("levels").is_dir().then_some(dir)
}

/// The same differential on the user's own converted maps, with their real
/// Kismet and NPCs. Runs only with `ASAMU_CONVERTED_DIR` (user-local data
/// converted from the user's own install; never in the repository, never in
/// CI). `ASAMU_SANDBOX_GUARD_MAPS=a,b` limits the maps.
#[test]
fn real_maps_are_unchanged_by_a_pristine_session() {
    const REAL_TICKS: usize = 2000;
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let only: Option<Vec<String>> = std::env::var("ASAMU_SANDBOX_GUARD_MAPS")
        .ok()
        .map(|list| list.split(',').map(|m| m.trim().to_owned()).collect());
    let mut compared = 0;
    for map in smoke::converted_maps(&dir) {
        if only.as_ref().is_some_and(|only| !only.contains(&map)) {
            continue;
        }
        let load = || load_level_with_kismet(&dir, &map);
        let (mut game, mut script) = match load() {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("{map}: not playable on its own ({e}); skipped");
                continue;
            }
        };
        game.start();
        let inputs = Driver::standard;
        // The per-tick reports, scripted outputs, player states, the trace
        // and the level objects are compared; the whole-game text is not
        // (it would print the user's entire converted map).
        let control =
            run_without_game_text(&mut game, &mut script, inputs(), REAL_TICKS, &mut Untouched);

        let (mut game, mut script) = load().expect("the map loaded a moment ago");
        game.start();
        let mut watched = Watched(Session::classic());
        let ours =
            run_without_game_text(&mut game, &mut script, inputs(), REAL_TICKS, &mut watched);
        assert_identical(&control, &ours, &format!("a pristine session on {map}"));
        assert!(watched.0.is_pristine(), "{map}");
        let count = |f: fn(&TickReport) -> bool| control.reports.iter().filter(|r| f(r)).count();
        eprintln!(
            "{map}: {REAL_TICKS} scripted ticks identical (kismet: {}; {} jumps, {} landings, \
             {} grapple attaches, {} boots events, {} deaths)",
            script.is_some(),
            count(|r| r.events.jumped),
            count(|r| r.events.landing.is_some()),
            count(|r| r.events.gun.attached.is_some()),
            count(|r| r.events.boots.is_some()),
            count(|r| r.died.is_some()),
        );
        compared += 1;
    }
    assert!(compared > 0, "no converted map could be played");
}

// ---------------------------------------------------------------------------
// Recordings.
// ---------------------------------------------------------------------------

/// A short trace recorded by `game` under the standard input script.
fn recorded_trace(game: &mut Game, ticks: usize) -> Trace {
    game.start_recording();
    let mut inputs = InputScript::new(DEFAULT_SEED);
    for _ in 0..ticks {
        game.tick(&inputs.next_frame()).expect("playing");
    }
    game.stop_recording().expect("recording")
}

/// The parity reader refuses the Sandbox header line by construction: it is
/// not trace metadata. This part needs nothing but the header type.
#[test]
fn parity_reader_rejects_the_sandbox_header_line() {
    let trace = recorded_trace(&mut classic_graybox(), 120);
    let trace_text = trace.to_jsonl_string().unwrap();
    // The plain trace is a parity trace ...
    assert_eq!(Trace::from_jsonl_str(&trace_text).unwrap(), trace);

    // ... and the same trace behind a Sandbox header line is not.
    let session = Session::classic();
    let header = RecordingHeader {
        format: RECORDING_FORMAT.to_owned(),
        version: asamu_sandbox::recording::RECORDING_VERSION,
        not_parity: true,
        level: "graybox".to_owned(),
        param_set: "classic".to_owned(),
        profile: session.profile().name.clone(),
        overrides: session.overlay().clone(),
        rules: *session.rules(),
        time_control_used: false,
        actions: Vec::new(),
        actions_truncated: false,
    };
    let container = format!("{}\n{trace_text}", serde_json::to_string(&header).unwrap());
    assert!(
        Trace::from_jsonl_str(&container).is_err(),
        "the parity reader accepted a file that starts with a Sandbox header"
    );
    // Blank lines in front do not get it past the reader either.
    assert!(Trace::from_jsonl_str(&format!("\n\n{container}")).is_err());
}

#[test]
fn parity_reader_rejects_sandbox_recordings() {
    // A pristine session's recording is a Sandbox recording too.
    let session = Session::classic();
    let mut game = session.new_game(graybox_test_level()).unwrap();
    game.start();
    let trace = recorded_trace(&mut game, 240);
    let recording = SandboxRecording::finish(trace, &session);
    assert!(recording.header.not_parity);
    assert_eq!(recording.header.format, RECORDING_FORMAT);

    let mut bytes = Vec::new();
    recording.write_jsonl(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        Trace::from_jsonl_str(&text).is_err(),
        "the parity reader accepted a Sandbox recording"
    );
    assert!(Trace::read_jsonl(text.as_bytes()).is_err());

    // The embedded trace alone still reads as a trace: the container is one
    // header line in front of an ordinary trace, nothing else.
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.contains(RECORDING_FORMAT));
    let embedded = Trace::from_jsonl_str(rest).unwrap();
    assert_eq!(embedded.meta, recording.trace.meta);
    assert!(asamu_player::compare(&embedded, &recording.trace).is_exact());
    assert_eq!(embedded.samples.len(), 241);
    // And even alone it says what it is.
    assert!(
        embedded
            .meta
            .level
            .as_deref()
            .is_some_and(|level| level.starts_with("sandbox:")),
        "{:?}",
        embedded.meta.level
    );

    // The Sandbox's own reader takes the container back.
    assert_eq!(
        SandboxRecording::read_jsonl(text.as_bytes()).unwrap(),
        recording
    );
}

/// One run of a session the way a host runs it: the session's own game, the
/// session's `Record` command around the run, and every hook around every
/// tick. Returns the recording the session hands back and the game.
fn recorded_by_a_session(mut session: Session, ticks: usize) -> (SandboxRecording, Game) {
    let mut game = session
        .new_game(graybox_test_level())
        .expect("the graybox level is valid");
    game.start();
    let mut script = None;
    let started = session
        .execute(
            Command::Record { on: Toggle::On },
            &mut SimCx {
                game: &mut game,
                script: &mut script,
            },
        )
        .expect("recording starts");
    assert!(started.recording.is_none());
    let mut driver = Driver::standard();
    for _ in 0..ticks {
        let input = driver.input_for(&game);
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        session
            .reconcile(&mut cx)
            .expect("the session's set applies");
        session.before_tick(&mut cx);
        let report = game.tick(&input).expect("the game is playing");
        session.after_tick(&game, None, &report);
    }
    let stopped = session
        .execute(
            Command::Record { on: Toggle::Off },
            &mut SimCx {
                game: &mut game,
                script: &mut script,
            },
        )
        .expect("recording stops");
    let recording = stopped
        .recording
        .expect("the session hands the recording back");
    (recording, game)
}

/// Same inputs through a Classic game and through a Sandbox session with an
/// empty overlay and default rules, each recording its own way (Classic: the
/// game's recorder; the session: its `Record` command): the two traces hold
/// the same samples, byte for byte. Only the labels differ, and they differ
/// always: the session's file is a Sandbox recording even when every sample
/// in it is Classic's.
#[test]
fn a_pristine_sessions_own_recording_holds_the_classic_samples() {
    const RECORDED: usize = TICKS;

    // Classic: the Classic constructor and the game's own recorder (the
    // standard control run, which jumps, grapples, boosts and respawns).
    let control = graybox_control();
    let classic_trace = Trace::from_jsonl_str(&control.trace).unwrap();

    // The Sandbox: empty overlay, default rules.
    let session = Session::classic();
    assert!(session.overlay().is_empty() && session.rules().is_default());
    let (recording, game) = recorded_by_a_session(session, RECORDED);
    assert_eq!(recording.header.param_set, "classic");
    assert!(recording.header.overrides.is_empty());
    assert!(recording.header.rules.is_default());
    assert!(!recording.header.time_control_used);

    // Sample for sample the same bytes ...
    let sample_lines = |trace: &Trace| -> Vec<String> { trace.samples.iter().map(exact).collect() };
    if let Some(diff) = first_difference(
        &sample_lines(&classic_trace),
        &sample_lines(&recording.trace),
    ) {
        panic!("a pristine session's recording is not the Classic run; {diff}");
    }
    assert_eq!(recording.trace.samples.len(), RECORDED + 1);
    // ... and in the file as written: behind the header line and the trace's
    // own first line, the Classic trace's lines.
    let mut bytes = Vec::new();
    recording.write_jsonl(&mut bytes).unwrap();
    let written = String::from_utf8(bytes).unwrap();
    let ours: Vec<&str> = written.lines().skip(2).collect();
    let theirs: Vec<&str> = control.trace.lines().skip(1).collect();
    assert!(ours == theirs, "the written sample lines differ");
    // The games ended as the same value (the session's recorder is off, as
    // the control's is).
    assert!(
        format!("{game:?}") == control.game_text,
        "the session's game differs from the Classic game after the run"
    );

    // The labels are the whole difference: the level prefix and the note.
    let mut meta = recording.trace.meta.clone();
    let level = meta.level.take().expect("the level is named");
    meta.level = Some(
        level
            .strip_prefix(TRACE_LEVEL_PREFIX)
            .expect("the embedded trace's level carries the Sandbox prefix")
            .to_owned(),
    );
    let notes_before = meta.notes.len();
    meta.notes.retain(|note| note != TRACE_NOTE_NOT_PARITY);
    assert_eq!(meta.notes.len() + 1, notes_before, "{:?}", meta.notes);
    assert_eq!(meta, classic_trace.meta);
    // And they keep the parity reader out, Classic samples or not.
    assert!(Trace::from_jsonl_str(&written).is_err());

    // The comparison is not blind: one override, and the samples differ.
    let tuned = Session::new(Profile {
        name: "guard".to_owned(),
        overrides: tuned_overlay(),
        ..Profile::classic()
    })
    .unwrap();
    let (tuned_recording, _) = recorded_by_a_session(tuned, RECORDED);
    assert_eq!(tuned_recording.header.param_set, "modified");
    assert!(
        sample_lines(&tuned_recording.trace) != sample_lines(&classic_trace),
        "a tuned session recorded the Classic run"
    );
}

#[test]
fn a_tuned_recording_is_never_labelled_original() {
    let session = Session::new(Profile {
        name: "guard".to_owned(),
        overrides: tuned_overlay(),
        ..Profile::classic()
    })
    .unwrap();
    assert!(matches!(session.label(), ParamSetLabel::Modified { .. }));
    let mut game = session.new_game(graybox_test_level()).unwrap();
    game.start();
    // The game's own recorder labels its parameters by whether the script
    // layer is present, which a tuned set still has. Whatever it wrote, the
    // finished recording must not call a tuned set original.
    let raw = recorded_trace(&mut game, 120);
    let recording = SandboxRecording::finish(raw, &session);
    assert_eq!(recording.header.param_set, "modified");
    assert_eq!(recording.header.overrides, *session.overlay());
    let notes = &recording.trace.meta.notes;
    assert!(
        !notes
            .iter()
            .any(|note| note.contains("parameters: original")),
        "a tuned recording is labelled original: {notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("sandbox")),
        "{notes:?}"
    );
    assert!(
        recording
            .trace
            .meta
            .level
            .as_deref()
            .is_some_and(|level| level.starts_with("sandbox:")),
        "{:?}",
        recording.trace.meta.level
    );

    // Written and read back, the labels are still there.
    let mut bytes = Vec::new();
    recording.write_jsonl(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        !text.contains("parameters: original"),
        "in the written file"
    );
    assert!(Trace::from_jsonl_str(&text).is_err());
}
