//! The parameter catalogue and the Sandbox's effect table.
//!
//! The catalogue must be exactly the Classic provenance report (the Sandbox
//! adds no parameter and renames none). The effect table is ours, so it is
//! not trusted: every Inert key is shown to leave a scripted run bit
//! identical, and every Live key is shown to change the very next tick.
//! (Latched and spawn-only keys are covered in `tests/relatch.rs`.)

use std::collections::BTreeSet;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_game::Game;
use asamu_game::smoke::{DEFAULT_SEED, InputScript};
use asamu_player::{InputFrame, PlayerParams};
use asamu_sandbox::keys::{Catalog, EFFECTS, Effect, INERT_GROUP, TuneValue, ValueKind, effect_of};
use asamu_sandbox::overlay::Overlay;
use asamu_sandbox::relatch::retune;
use asamu_world::graybox_test_level;
use glam::Vec3;
use serde_json::Value;

#[test]
fn catalog_matches_the_provenance_report() {
    let classic = PlayerParams::asamu_original();
    let report = classic.provenance_report();
    let root = serde_json::to_value(&classic).unwrap();
    let catalog = Catalog::classic();
    assert_eq!(catalog, *Catalog::shared());

    // One entry per report row, in report order, nothing added or renamed.
    assert_eq!(catalog.len(), report.len());
    assert!(!catalog.is_empty());
    let mut seen = BTreeSet::new();
    for (info, row) in catalog.iter().zip(&report) {
        assert_eq!(info.key, row.name);
        assert!(seen.insert(info.key.clone()), "{} twice", info.key);
        assert_eq!(info.unit, row.unit, "{}", info.key);
        assert_eq!(info.description, row.description, "{}", info.key);
        assert_eq!(info.classic_provenance, row.provenance, "{}", info.key);
        assert_eq!(catalog.get(&info.key), Some(info));
        assert_eq!(info.effect, effect_of(&info.key), "{}", info.key);

        // A key is `group.field`, and that path is the serialized leaf.
        let (group, field) = info.key.split_once('.').unwrap();
        assert!(!field.contains('.'), "{}", info.key);
        let leaf = &root[group][field];
        let value = &leaf["value"];
        assert!(leaf.get("provenance").is_some(), "{}", info.key);

        // Kind and Classic value are the leaf's own, and the Classic value
        // reads as the report prints it.
        match (&info.kind, &info.classic, value) {
            (ValueKind::Float, TuneValue::Float(v), Value::Number(n)) => {
                assert!(n.is_f64(), "{}", info.key);
                let stored = n.as_f64().unwrap() as f32;
                assert_eq!((*v as f32).to_bits(), stored.to_bits(), "{}", info.key);
                assert_eq!(stored.to_string(), row.value, "{}", info.key);
                assert_eq!(
                    v.to_string(),
                    row.value,
                    "{}: shown as the report shows it",
                    info.key
                );
            }
            (ValueKind::Int, TuneValue::Int(v), Value::Number(n)) => {
                assert_eq!(n.as_i64(), Some(*v), "{}", info.key);
                assert_eq!(v.to_string(), row.value, "{}", info.key);
            }
            (ValueKind::Bool, TuneValue::Bool(v), Value::Bool(b)) => {
                assert_eq!(v, b, "{}", info.key);
                assert_eq!(v.to_string(), row.value, "{}", info.key);
            }
            (ValueKind::Choice(_), TuneValue::Text(v), Value::String(s)) => {
                assert_eq!(v, s, "{}", info.key);
                assert_eq!(*v, row.value, "{}", info.key);
            }
            other => panic!("{}: kind, value and leaf disagree: {other:?}", info.key),
        }
        assert!(info.step.is_finite() && info.step > 0.0, "{}", info.key);
        if let TuneValue::Float(v) = info.classic
            && v != 0.0
        {
            // A nudge is a small, visible fraction of the value (ours).
            let fraction = info.step / v.abs();
            assert!((0.02..=0.1).contains(&fraction), "{}: {fraction}", info.key);
        }
    }

    // The groups are the report's, in its order, and every key belongs to
    // one of them.
    let mut groups: Vec<&str> = Vec::new();
    for row in &report {
        let group = row.name.split_once('.').unwrap().0;
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    assert_eq!(catalog.groups(), groups);
    assert!(groups.len() >= 3);
    assert_eq!(catalog.get("movement"), None);
    assert_eq!(catalog.get("no.such_key"), None);
}

#[test]
fn no_stale_effect_entries() {
    let catalog = Catalog::classic();
    let mut seen = BTreeSet::new();
    for (key, effect) in EFFECTS {
        assert!(
            catalog.get(key).is_some(),
            "the effect table lists {key}, which is not a parameter (any more)"
        );
        assert!(seen.insert(*key), "{key} is listed twice");
        assert_ne!(*effect, Effect::Unclassified, "{key}");
        assert_eq!(effect_of(key), *effect, "{key}");
        assert!(
            !key.starts_with(&format!("{INERT_GROUP}.")),
            "{key}: the whole {INERT_GROUP} group is inert without being listed"
        );
        if let Effect::Inert(reason) = effect {
            assert!(!reason.is_empty(), "{key}");
        }
    }
    // The inert group exists and all of it is inert, with a reason.
    let inert_group: Vec<_> = catalog
        .iter()
        .filter(|info| info.key.starts_with(&format!("{INERT_GROUP}.")))
        .collect();
    assert!(!inert_group.is_empty());
    for info in inert_group {
        assert!(
            matches!(info.effect, Effect::Inert(reason) if !reason.is_empty()),
            "{}",
            info.key
        );
    }
    // Anything else is unclassified, including keys that do not exist: a
    // parameter added to Classic later never breaks the Sandbox.
    assert_eq!(
        effect_of("movement.some_future_parameter"),
        Effect::Unclassified
    );
    assert_eq!(effect_of("future_group.value"), Effect::Unclassified);
    assert_eq!(effect_of(""), Effect::Unclassified);
    assert_eq!(effect_of(INERT_GROUP), Effect::Unclassified);
    for info in catalog.iter() {
        let listed = EFFECTS.iter().any(|(key, _)| *key == info.key);
        let in_group = info.key.starts_with(&format!("{INERT_GROUP}."));
        assert_eq!(
            info.effect == Effect::Unclassified,
            !(listed || in_group),
            "{}",
            info.key
        );
    }
    // Every class of the table is in use.
    for class in ["live", "latched", "spawn_only", "inert"] {
        assert!(
            catalog.iter().any(|info| {
                serde_json::to_value(info.effect)
                    .ok()
                    .is_some_and(|v| v == class || v.get(class).is_some())
            }),
            "no key is {class}"
        );
    }
}

#[test]
fn choice_values_deserialise() {
    let mut listed = 0;
    for info in Catalog::classic().iter() {
        let ValueKind::Choice(names) = info.kind else {
            continue;
        };
        let TuneValue::Text(classic) = &info.classic else {
            panic!("{}: a choice holds a name", info.key);
        };
        // The Classic name itself is always accepted (and is no override).
        let mut overlay = Overlay::default();
        overlay
            .set(&info.key, TuneValue::Text(classic.clone()))
            .unwrap_or_else(|e| panic!("{} = {classic}: {e}", info.key));
        assert!(overlay.is_empty(), "{}", info.key);
        if names.is_empty() {
            // An enum the Sandbox has no list for (a parameter added to
            // Classic later): shown, not steppable, and not an error.
            assert_eq!(
                overlay.nudge(&info.key, 1, 1.0),
                Ok(info.classic.clone()),
                "{}",
                info.key
            );
            continue;
        }
        listed += 1;
        // The list holds the Classic name and has no duplicates.
        assert!(names.contains(&classic.as_str()), "{}", info.key);
        assert_eq!(
            names.iter().collect::<BTreeSet<_>>().len(),
            names.len(),
            "{}",
            info.key
        );
        // Every listed name is one the parameter set really reads.
        for name in names {
            let mut overlay = Overlay::default();
            overlay
                .set(&info.key, TuneValue::Text((*name).to_owned()))
                .unwrap_or_else(|e| panic!("{} = {name}: {e}", info.key));
            let params = overlay
                .apply()
                .unwrap_or_else(|e| panic!("{} = {name}: {e}", info.key));
            let entry = params
                .provenance_report()
                .into_iter()
                .find(|e| e.name == info.key)
                .unwrap();
            assert_eq!(entry.value, *name, "{}", info.key);
            assert_eq!(
                overlay.is_empty(),
                *name == classic.as_str(),
                "{}",
                info.key
            );
        }
        // And nothing else is accepted.
        let mut overlay = Overlay::default();
        assert!(
            overlay
                .set(&info.key, TuneValue::Text("no_such_choice".to_owned()))
                .is_err()
        );
    }
    assert!(listed >= 1, "the catalogue has choice keys with names");
}

#[test]
fn suggestions_find_near_misses_only() {
    let catalog = Catalog::classic();
    for info in catalog.iter() {
        // The key itself, a key with one letter dropped, the bare field.
        assert_eq!(catalog.suggest(&info.key), Some(info.key.as_str()));
        let mut typo = info.key.clone();
        typo.remove(typo.len() / 2);
        let suggestion = catalog.suggest(&typo).unwrap_or_else(|| panic!("{typo}"));
        assert!(catalog.get(suggestion).is_some());
        assert_eq!(
            catalog.suggest(&info.key.to_uppercase()),
            Some(info.key.as_str())
        );
    }
    assert_eq!(catalog.suggest(""), None);
    assert_eq!(catalog.suggest("   "), None);
    assert_eq!(catalog.suggest("q"), None);
    assert_eq!(catalog.suggest("entirely different words"), None);
    assert_eq!(catalog.suggest(&"movement.".repeat(50)), None);
}

// ---------------------------------------------------------------------------
// The effect classes, tested against the simulation.
// ---------------------------------------------------------------------------

/// One valid, non-Classic value for `key` after `steps` nudges (the first
/// count that gives one).
fn nudged(key: &str, steps: &[i32]) -> Option<Overlay> {
    steps.iter().find_map(|steps| {
        let mut overlay = Overlay::default();
        (overlay.nudge(key, *steps, 1.0).is_ok() && !overlay.is_empty()).then_some(overlay)
    })
}

/// What a scripted run did, to show that it exercised the systems an
/// inert key could have mattered to.
#[derive(Default)]
struct Activity {
    jumps: usize,
    landings: usize,
    attaches: usize,
    releases: usize,
    boosts: usize,
}

/// Everything observable about a scripted run on the graybox with `params`:
/// the recorded trace, the player and tick report of every tick (exact), and
/// what happened.
///
/// The inputs are the smoke suite's pseudo-random script with a fixed cycle
/// of deliberate actions laid over it: a jump and a rocket boost, then
/// aiming at the nearest grapple target and holding the button (random
/// looking alone almost never attaches, and a run that never flies on the
/// grapple could not tell whether a flying-speed value is inert). The
/// aiming reads the game, so the inputs are a function of its state: an
/// inert key gives the same inputs, any other key a different run.
fn scripted_run(params: PlayerParams, ticks: usize) -> (String, Vec<String>, Activity) {
    let mut game =
        Game::new(graybox_test_level(), params, DEFAULT_TICK_RATE_HZ).expect("the set is valid");
    game.start();
    game.start_recording();
    let mut inputs = InputScript::new(DEFAULT_SEED);
    let mut lines = Vec::with_capacity(ticks);
    let mut activity = Activity::default();
    for tick in 0..ticks {
        let mut input = inputs.next_frame();
        let player = *game.player();
        match tick % 240 {
            30 => {
                input.jump_pressed = true;
                input.jump_held = true;
            }
            31..=44 => input.jump_held = true,
            45 => input.jump_pressed = !player.grounded,
            120 => {
                // The fire trace reads the view as the previous tick left
                // it: look now, press on the next tick.
                input.grapple_held = false;
                let eye = game.eye_position();
                let level = game.level();
                let nearest = level
                    .grapple_points
                    .iter()
                    .map(|point| point.position)
                    .chain(level.crystals.iter().map(|crystal| crystal.center))
                    .min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye)));
                if let Some(target) = nearest {
                    let to = target - eye;
                    input.look_yaw_delta = to.y.atan2(to.x) - player.yaw;
                    input.look_pitch_delta = to.z.atan2(to.truncate().length()) - player.pitch;
                }
            }
            121..=180 => {
                input.look_yaw_delta = 0.0;
                input.look_pitch_delta = 0.0;
                input.grapple_held = true;
            }
            _ => {}
        }
        let report = game.tick(&input).expect("playing");
        activity.jumps += usize::from(report.events.jumped);
        activity.landings += usize::from(report.events.landing.is_some());
        activity.attaches += usize::from(report.events.gun.attached.is_some());
        activity.releases += usize::from(report.events.gun.released.is_some());
        activity.boosts += usize::from(report.events.boots.is_some());
        lines.push(format!(
            "{} {}",
            serde_json::to_string(&report).expect("a report serializes"),
            serde_json::to_string(game.player()).expect("a player serializes")
        ));
    }
    let trace = game
        .stop_recording()
        .expect("recording")
        .to_jsonl_string()
        .expect("a valid trace");
    (trace, lines, activity)
}

#[test]
fn inert_keys_leave_a_scripted_run_bit_identical() {
    const TICKS: usize = 3000;
    let control = scripted_run(PlayerParams::asamu_original(), TICKS);
    // The run walks, jumps, lands, flies on the grapple and boosts.
    let did = &control.2;
    assert!(
        did.jumps >= 1
            && did.landings >= 1
            && did.attaches >= 1
            && did.releases >= 1
            && did.boosts >= 1,
        "the control run is not eventful enough: {} jumps, {} landings, {} attaches, \
         {} releases, {} boosts",
        did.jumps,
        did.landings,
        did.attaches,
        did.releases,
        did.boosts
    );
    let mut inert = 0;
    let mut skipped = Vec::new();
    for info in Catalog::classic().iter() {
        let Effect::Inert(_) = info.effect else {
            continue;
        };
        // Several different values, in both directions where there are any.
        let overlays: Vec<Overlay> = [&[1, 2][..], &[-1, -2], &[4], &[-4]]
            .iter()
            .filter_map(|steps| nudged(&info.key, steps))
            .collect();
        if overlays.is_empty() {
            // A choice with a single name cannot be changed at all.
            skipped.push(info.key.clone());
            continue;
        }
        inert += 1;
        for overlay in overlays {
            let params = overlay.apply().unwrap();
            assert_ne!(params, PlayerParams::asamu_original());
            let run = scripted_run(params, TICKS);
            let first = control.1.iter().zip(&run.1).position(|(a, b)| a != b);
            assert!(
                first.is_none() && control.0 == run.0,
                "{} is listed as inert, but {:?} changes the run (first at tick {:?})",
                info.key,
                overlay.get(&info.key),
                first.map(|i| i + 1)
            );
        }
    }
    assert!(inert >= 8, "only {inert} inert keys were exercised");
    assert!(skipped.len() <= 1, "{skipped:?}");

    // The run is able to show a difference: a key that is not inert does
    // change it. (Without this the loop above could pass on a blind run.)
    for key in ["movement.custom_gravity_scaling", "pawn.move_speed"] {
        let overlay = nudged(key, &[1]).unwrap();
        let run = scripted_run(overlay.apply().unwrap(), TICKS);
        assert!(
            control.0 != run.0 && control.1 != run.1,
            "{key} changed nothing"
        );
    }
}

/// The situations in which a Live key is read on the very next tick.
#[derive(Clone, Copy, Debug)]
enum Scene {
    /// In the air, falling.
    Falling,
    /// In the air, falling close to the Classic terminal velocity.
    FallingFast,
    /// Standing still, about to walk.
    Standing,
    /// Walking at full speed, about to let go.
    Walking,
    /// Walking at full speed, still holding forward.
    WalkingOn,
    /// Looking straight up against the pitch limit.
    LookingUp,
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

impl Scene {
    /// A started Classic graybox game brought into the situation, and the
    /// input of the tick under test.
    fn stage(self) -> (Game, InputFrame) {
        let mut game = Game::graybox().expect("the graybox level is valid");
        game.start();
        let idle = InputFrame::default();
        let settle = |game: &mut Game, input: &InputFrame, ticks: usize| {
            for _ in 0..ticks {
                game.tick(input).expect("playing");
            }
        };
        settle(&mut game, &idle, 30);
        match self {
            Self::Falling | Self::FallingFast => {
                let terminal = game.params().movement.terminal_velocity.value;
                let player = game.player_mut();
                player.position += Vec3::Z * 4000.0;
                player.grounded = false;
                player.velocity = if matches!(self, Self::FallingFast) {
                    Vec3::NEG_Z * (terminal * 0.97)
                } else {
                    Vec3::ZERO
                };
                settle(&mut game, &idle, 2);
                assert!(!game.player().grounded, "{self:?}: still in the air");
                (game, idle)
            }
            Self::Standing => {
                assert!(game.player().grounded);
                (game, forward())
            }
            Self::Walking => {
                settle(&mut game, &forward(), 40);
                assert!(game.player().grounded && game.player().horizontal_speed() > 1.0);
                (game, idle)
            }
            Self::WalkingOn => {
                settle(&mut game, &forward(), 40);
                assert!(game.player().grounded && game.player().horizontal_speed() > 1.0);
                (game, forward())
            }
            Self::LookingUp => {
                let up = InputFrame {
                    look_pitch_delta: 3.0,
                    ..InputFrame::default()
                };
                settle(&mut game, &up, 2);
                assert!(game.player().pitch > 1.0);
                (game, idle)
            }
        }
    }
}

/// Where each Live key shows on the next tick, and which way to nudge it
/// (a limit must be nudged *into* the value it limits).
const LIVE_SCENES: &[(&str, Scene, i32)] = &[
    ("movement.world_gravity_z", Scene::Falling, 4),
    ("movement.custom_gravity_scaling", Scene::Falling, 4),
    ("movement.ground_acceleration", Scene::Standing, 4),
    ("movement.ground_friction", Scene::Walking, 4),
    ("movement.movement_speed_modifier", Scene::WalkingOn, -4),
    ("movement.terminal_velocity", Scene::FallingFast, -4),
    ("camera.max_pitch_degrees", Scene::LookingUp, -2),
];

#[test]
fn live_keys_change_the_next_tick() {
    // Every Live key has a scene, and every scene key is Live.
    let live: BTreeSet<&str> = EFFECTS
        .iter()
        .filter(|(_, effect)| *effect == Effect::Live)
        .map(|(key, _)| *key)
        .collect();
    let staged: BTreeSet<&str> = LIVE_SCENES.iter().map(|(key, _, _)| *key).collect();
    assert_eq!(live, staged, "the Live keys and the scenes that prove them");

    for (key, scene, steps) in LIVE_SCENES {
        let mut overlay = Overlay::default();
        overlay
            .nudge(key, *steps, 1.0)
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        let tuned = overlay.apply().unwrap();

        let (mut control, input) = scene.stage();
        let (mut game, _) = scene.stage();
        assert_eq!(game.player(), control.player(), "{key}: same situation");

        // Retuning a Live key rewrites nothing in the pawn ...
        let report = retune(&mut game, tuned).unwrap();
        assert!(report.relatched.is_empty(), "{key}: {report:?}");
        assert!(report.deferred.is_empty(), "{key}: {report:?}");
        assert_eq!(game.player(), control.player(), "{key}: pawn untouched");

        // ... and the very next tick runs on the new value.
        let before = *control.player();
        control.tick(&input).unwrap();
        game.tick(&input).unwrap();
        assert_ne!(*control.player(), before, "{key}: the scene moves");
        assert_ne!(
            game.player(),
            control.player(),
            "{key} is listed as live, but the next tick ({scene:?}) did not change"
        );
        assert!(game.player().is_finite(), "{key}");
    }
}

/// The counterpart: retuning the same game with the set it already runs
/// changes nothing in any of the scenes (so the differences above are the
/// keys', not the retune's).
#[test]
fn retuning_to_the_same_set_changes_no_scene() {
    for (key, scene, _) in LIVE_SCENES {
        let (mut control, input) = scene.stage();
        let (mut game, _) = scene.stage();
        let same = game.params().clone();
        retune(&mut game, same).unwrap();
        for _ in 0..30 {
            assert_eq!(game.tick(&input), control.tick(&input), "{key}");
            assert_eq!(game.player(), control.player(), "{key}");
        }
    }
}
