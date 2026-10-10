//! Retuning a running game: the pawn's latched run-time values follow the
//! new set, and only they do.
//!
//! The reference for "follow" is not a formula written here. It is a second
//! game that was **built with the new set** and brought into the same
//! situation by the same inputs: after a retune, the latched values of the
//! retuned game must equal that game's, bit for bit. The rules themselves
//! are ours (Sandbox tooling); the test values are ours too.

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_game::smoke::{DEFAULT_SEED, InputScript};
use asamu_game::{Game, SetParamsError};
use asamu_player::params::ParamError;
use asamu_player::{InputFrame, PawnStateName, PlayerParams, PlayerState};
use asamu_sandbox::keys::{Catalog, EFFECTS, Effect, TuneValue};
use asamu_sandbox::overlay::Overlay;
use asamu_sandbox::relatch::{RelatchReport, relatch, retune};
use asamu_world::graybox_test_level;
use glam::Vec3;

/// A started graybox game running `params`.
fn game_with(params: &PlayerParams) -> Game {
    let mut game = Game::new(graybox_test_level(), params.clone(), DEFAULT_TICK_RATE_HZ)
        .expect("the set is valid");
    game.start();
    game
}

fn classic_game() -> Game {
    game_with(&PlayerParams::asamu_original())
}

fn ticks(game: &mut Game, input: &InputFrame, count: usize) {
    for _ in 0..count {
        game.tick(input).expect("playing");
    }
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

/// The run-time values the script layer copies from the parameter set.
#[derive(Debug, PartialEq, Eq)]
struct Latched {
    ground_speed: u32,
    air_control: u32,
    jump_z: u32,
    fov: u32,
    zoom_enabled: bool,
    air_speed: u32,
}

fn latched(player: &PlayerState) -> Latched {
    let s = &player.script;
    Latched {
        ground_speed: s.ground_speed.to_bits(),
        air_control: s.air_control.to_bits(),
        jump_z: s.jump_z.to_bits(),
        fov: s.fov.to_bits(),
        zoom_enabled: s.zoom_enabled,
        air_speed: s.air_speed.to_bits(),
    }
}

/// The Classic set with `key` moved by `steps` nudges.
fn tuned(changes: &[(&str, i32)]) -> PlayerParams {
    let mut overlay = Overlay::default();
    for (key, steps) in changes {
        overlay
            .nudge(key, *steps, 1.0)
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        assert!(overlay.get(key).is_some(), "{key} did not change");
    }
    overlay.apply().expect("the tuned set is valid")
}

/// Nudges that change a key of any kind (an odd count flips a flag).
const STEPS: i32 = 3;

/// Every Latched key changed at once.
fn all_latched() -> PlayerParams {
    let changes: Vec<(&str, i32)> = EFFECTS
        .iter()
        .filter(|(_, effect)| *effect == Effect::Latched)
        .map(|(key, _)| (*key, STEPS))
        .collect();
    assert!(changes.len() >= 8, "{changes:?}");
    tuned(&changes)
}

/// The situations a pawn's latched values depend on.
#[derive(Clone, Copy, Debug)]
enum Situation {
    /// Just started, standing.
    Fresh,
    /// Walking.
    Walking,
    /// Sprinting on the ground.
    Sprinting,
    /// In story mode.
    Story,
    /// After a jump and its landing (the landed air control is in force).
    Landed,
    /// Sprinting, then jumped: the sprint is removed and armed, in the air.
    SprintJump,
    /// Story mode entered and left again.
    StoryLeft,
}

const SITUATIONS: [Situation; 7] = [
    Situation::Fresh,
    Situation::Walking,
    Situation::Sprinting,
    Situation::Story,
    Situation::Landed,
    Situation::SprintJump,
    Situation::StoryLeft,
];

impl Situation {
    /// Drives a started game into the situation. Works the same on a
    /// Classic and on a tuned game (it waits on states, not on tick counts).
    fn enter(self, game: &mut Game) {
        ticks(game, &idle(), 20);
        let sprint = InputFrame {
            sprint_held: true,
            ..forward()
        };
        match self {
            Self::Fresh => {}
            Self::Walking => ticks(game, &forward(), 10),
            Self::Sprinting => {
                ticks(game, &sprint, 10);
                assert!(game.player().script.sprint.active, "{self:?}");
            }
            Self::Story => {
                game.enter_story_mode();
                ticks(game, &idle(), 5);
                assert!(game.in_story_mode(), "{self:?}");
            }
            Self::Landed => {
                let jump = InputFrame {
                    jump_pressed: true,
                    jump_held: true,
                    ..idle()
                };
                ticks(game, &jump, 1);
                assert!(!game.player().grounded, "{self:?}: jumped");
                for _ in 0..2000 {
                    if game.player().grounded {
                        break;
                    }
                    ticks(game, &idle(), 1);
                }
                assert!(game.player().grounded, "{self:?}: landed again");
            }
            Self::SprintJump => {
                ticks(game, &sprint, 5);
                let jump = InputFrame {
                    jump_pressed: true,
                    jump_held: true,
                    ..sprint
                };
                ticks(game, &jump, 1);
                assert!(!game.player().grounded, "{self:?}: jumped");
                assert!(game.player().script.sprint.armed, "{self:?}: sprint armed");
            }
            Self::StoryLeft => {
                game.enter_story_mode();
                ticks(game, &idle(), 3);
                game.exit_story_mode();
                ticks(game, &idle(), 3);
                assert!(!game.in_story_mode(), "{self:?}");
            }
        }
    }
}

#[test]
fn identity_when_old_equals_new() {
    let classic = PlayerParams::asamu_original();
    let tuned = all_latched();
    for params in [&classic, &tuned] {
        for situation in SITUATIONS {
            let mut game = game_with(params);
            situation.enter(&mut game);
            let before = format!("{game:?}");

            let mut player = *game.player();
            assert_eq!(
                relatch(&mut player, params, params),
                RelatchReport::default(),
                "{situation:?}"
            );
            assert_eq!(player, *game.player(), "{situation:?}");

            assert_eq!(
                retune(&mut game, params.clone()),
                Ok(RelatchReport::default()),
                "{situation:?}"
            );
            assert!(
                format!("{game:?}") == before,
                "{situation:?}: the game changed"
            );
        }
    }
}

#[test]
fn a_retuned_pawn_holds_what_a_pawn_built_with_the_new_set_holds() {
    let classic = PlayerParams::asamu_original();
    // Each Latched key on its own, then all of them together.
    let mut sets: Vec<(String, PlayerParams)> = EFFECTS
        .iter()
        .filter(|(_, effect)| *effect == Effect::Latched)
        .map(|(key, _)| ((*key).to_owned(), tuned(&[(key, STEPS)])))
        .collect();
    sets.push(("all latched keys".to_owned(), all_latched()));

    for (what, new) in &sets {
        for situation in SITUATIONS {
            // The reference: built with the new set.
            let mut reference = game_with(new);
            situation.enter(&mut reference);

            // Classic, brought to the same situation, then retuned.
            let mut game = classic_game();
            situation.enter(&mut game);
            let report = retune(&mut game, new.clone()).unwrap();
            assert_eq!(*game.params(), *new);
            assert_eq!(
                latched(game.player()),
                latched(reference.player()),
                "{what}, {situation:?}: {report:?}"
            );
            assert!(report.deferred.is_empty(), "{what}: {report:?}");
            for key in &report.relatched {
                assert_eq!(
                    Catalog::shared().get(key).map(|info| info.effect),
                    Some(Effect::Latched),
                    "{what}: reported {key}"
                );
            }

            // And back: retuned to Classic it holds what a Classic pawn in
            // that situation holds.
            let mut classic_reference = classic_game();
            situation.enter(&mut classic_reference);
            retune(&mut game, classic.clone()).unwrap();
            assert_eq!(
                latched(game.player()),
                latched(classic_reference.player()),
                "{what}, {situation:?}: back to Classic"
            );
        }
    }
}

#[test]
fn ground_speed_follows_in_walking_sprinting_and_story_mode() {
    let new = tuned(&[
        ("pawn.move_speed", 2),
        ("pawn.sprint_speed_multiplier", 2),
        ("pawn.story_speed_multiplier", 2),
    ]);
    let pawn = new.pawn.as_ref().unwrap();
    let cases = [
        (
            Situation::Walking,
            pawn.move_speed.value,
            vec!["pawn.move_speed"],
        ),
        (
            Situation::Sprinting,
            pawn.move_speed.value * pawn.sprint_speed_multiplier.value,
            vec!["pawn.move_speed", "pawn.sprint_speed_multiplier"],
        ),
        (
            Situation::Story,
            pawn.move_speed.value * pawn.story_speed_multiplier.value,
            vec!["pawn.move_speed", "pawn.story_speed_multiplier"],
        ),
    ];
    for (situation, expected, keys) in cases {
        let mut game = classic_game();
        situation.enter(&mut game);
        let before = game.player().script.ground_speed;
        let report = retune(&mut game, new.clone()).unwrap();
        let after = game.player().script.ground_speed;
        assert_eq!(after.to_bits(), expected.to_bits(), "{situation:?}");
        assert_ne!(after, before, "{situation:?}");
        assert_eq!(report.relatched, keys, "{situation:?}");
    }

    // A multiplier that is not in force is not reported and changes nothing.
    let sprint_only = tuned(&[("pawn.sprint_speed_multiplier", 2)]);
    let mut game = classic_game();
    Situation::Walking.enter(&mut game);
    let before = *game.player();
    assert_eq!(
        retune(&mut game, sprint_only.clone()),
        Ok(RelatchReport::default())
    );
    assert_eq!(*game.player(), before);
    // It is used as soon as the pawn sprints.
    let sprint = InputFrame {
        sprint_held: true,
        ..forward()
    };
    ticks(&mut game, &sprint, 1);
    let pawn = sprint_only.pawn.as_ref().unwrap();
    assert_eq!(
        game.player().script.ground_speed,
        pawn.move_speed.value * pawn.sprint_speed_multiplier.value
    );
}

#[test]
fn air_control_follows_before_and_after_the_first_landing() {
    let new = tuned(&[("movement.air_control", 2), ("pawn.landed_air_control", -2)]);
    let start = new.movement.air_control.value;
    let landed = new.pawn.as_ref().unwrap().landed_air_control.value;
    assert_ne!(start, landed);

    // Before the first landing: the start value.
    let mut game = classic_game();
    Situation::Fresh.enter(&mut game);
    let report = retune(&mut game, new.clone()).unwrap();
    assert_eq!(game.player().script.air_control, start);
    assert_eq!(report.relatched, ["movement.air_control"]);

    // After a landing: the landed value.
    let mut game = classic_game();
    Situation::Landed.enter(&mut game);
    let report = retune(&mut game, new.clone()).unwrap();
    assert_eq!(game.player().script.air_control, landed);
    assert_eq!(report.relatched, ["pawn.landed_air_control"]);

    // A pawn retuned before its first landing picks the new landed value
    // up by itself when it lands (the landing reads the set).
    let mut game = classic_game();
    Situation::Fresh.enter(&mut game);
    retune(&mut game, new.clone()).unwrap();
    let mut probe = game.clone();
    Situation::Landed.enter(&mut probe);
    assert_eq!(probe.player().script.air_control, landed);
}

#[test]
fn values_the_level_changed_are_left_alone() {
    let new = all_latched();
    let mut game = classic_game();
    Situation::Walking.enter(&mut game);
    // As if the level's script had set them (values of ours).
    {
        let script = &mut game.player_mut().script;
        script.ground_speed = 123.0;
        script.air_control = 0.011;
        script.jump_z = 77.0;
        script.fov = 61.0;
        script.air_speed = 999.0;
    }
    let classic_zoom = game.player().script.zoom_enabled;
    game.set_zoom_available(!classic_zoom);
    let before = *game.player();
    let report = retune(&mut game, new.clone()).unwrap();
    assert_eq!(report, RelatchReport::default());
    assert_eq!(
        *game.player(),
        before,
        "nothing the level set was overwritten"
    );
    assert_eq!(*game.params(), new, "while the set itself is in place");
}

#[test]
fn fov_is_not_touched_while_zooming() {
    let new = tuned(&[("camera.fov_degrees", -2)]);
    let new_fov = new.camera.fov_degrees.value;
    let zoom = InputFrame {
        power_jump_held: true,
        ..idle()
    };

    // Story mode, zoom key held, a few steps into the zoom.
    let mut game = classic_game();
    Situation::Story.enter(&mut game);
    ticks(&mut game, &zoom, 8);
    assert_eq!(game.player().script.code.state, PawnStateName::Zooming);
    let mid_zoom = game.player().script.fov;
    assert_ne!(mid_zoom, game.params().camera.fov_degrees.value, "mid zoom");

    let report = retune(&mut game, new.clone()).unwrap();
    assert_eq!(game.player().script.fov.to_bits(), mid_zoom.to_bits());
    assert!(
        !report.relatched.contains(&"camera.fov_degrees"),
        "{report:?}"
    );

    // The zoom itself reads the set: released, it ends on the new FOV.
    for _ in 0..600 {
        if game.player().script.code.state != PawnStateName::Zooming {
            break;
        }
        ticks(&mut game, &idle(), 1);
    }
    assert_eq!(game.player().script.code.state, PawnStateName::StoryState);
    assert_eq!(game.fov(), new_fov);

    // Not zooming, the FOV follows at once.
    let mut game = classic_game();
    Situation::Story.enter(&mut game);
    let report = retune(&mut game, new).unwrap();
    assert_eq!(game.fov(), new_fov);
    assert_eq!(report.relatched, ["camera.fov_degrees"]);
}

#[test]
fn spawn_only_keys_are_deferred() {
    let spawn_only: Vec<&str> = EFFECTS
        .iter()
        .filter(|(_, effect)| *effect == Effect::SpawnOnly)
        .map(|(key, _)| *key)
        .collect();
    assert_eq!(spawn_only.len(), 3, "{spawn_only:?}");
    let changes: Vec<(&str, i32)> = spawn_only.iter().map(|key| (*key, 1)).collect();
    let new = tuned(&changes);

    // On a running game: reported as deferred, and the game runs on exactly
    // as it would have (the values are read when the gun and boots spawn).
    let mut control = classic_game();
    let mut game = classic_game();
    let mut inputs = InputScript::new(DEFAULT_SEED);
    for _ in 0..300 {
        let input = inputs.next_frame();
        control.tick(&input).unwrap();
        game.tick(&input).unwrap();
    }
    let before = *game.player();
    let report = retune(&mut game, new.clone()).unwrap();
    assert!(report.relatched.is_empty(), "{report:?}");
    let mut deferred = report.deferred.clone();
    deferred.sort_unstable();
    let mut expected = spawn_only.clone();
    expected.sort_unstable();
    assert_eq!(deferred, expected);
    assert_eq!(*game.player(), before);
    for i in 0..2000 {
        let input = inputs.next_frame();
        assert_eq!(game.tick(&input), control.tick(&input), "tick {i}");
        assert_eq!(game.player(), control.player(), "tick {i}");
    }

    // In a game built with the set they are what the gun and boots spawn
    // with. (The graybox level's own ability table then sets the budget and
    // the boots, as a level does; the fire latch is not in that table.)
    let can_grapple =
        |params: &PlayerParams| params.gun.as_ref().map(|g| g.initial_can_grapple.value);
    assert_ne!(
        can_grapple(&new),
        can_grapple(&PlayerParams::asamu_original())
    );
    let built = Game::new(graybox_test_level(), new.clone(), DEFAULT_TICK_RATE_HZ).unwrap();
    assert_eq!(
        Some(built.player().script.gun.can_grapple),
        can_grapple(&new)
    );
    assert_ne!(
        built.player().script.gun.can_grapple,
        Game::graybox().unwrap().player().script.gun.can_grapple
    );
}

#[test]
fn a_changed_cylinder_keeps_a_standing_pawn_on_its_feet() {
    let taller = tuned(&[("movement.capsule_half_height", 4)]);
    let lift = taller.movement.capsule_half_height.value
        - PlayerParams::asamu_original()
            .movement
            .capsule_half_height
            .value;
    assert!(lift > 1.0);

    // Standing: the centre moves by the difference, the feet stay.
    let mut game = classic_game();
    Situation::Fresh.enter(&mut game);
    assert!(game.player().grounded);
    let before = *game.player();
    let feet_before = before.position.z
        - PlayerParams::asamu_original()
            .movement
            .capsule_half_height
            .value;
    let report = retune(&mut game, taller.clone()).unwrap();
    assert_eq!(report.relatched, ["movement.capsule_half_height"]);
    assert_eq!(game.player().position.z, before.position.z + lift);
    assert!(game.player().pawn.force_floor_check);
    assert_eq!(game.player().velocity, before.velocity);
    // It keeps standing where it stood: no fall, no landing.
    for i in 0..60 {
        let r = game.tick(&idle()).unwrap();
        assert!(game.player().grounded, "tick {i}");
        assert!(
            r.events.landing.is_none() && !r.respawned,
            "tick {i}: {r:?}"
        );
        let feet = game.player().position.z - taller.movement.capsule_half_height.value;
        assert!((feet - feet_before).abs() < 0.5, "tick {i}: feet at {feet}");
    }
    // And shrinking back puts the centre back.
    retune(&mut game, PlayerParams::asamu_original()).unwrap();
    assert!((game.player().position.z - before.position.z).abs() < 0.5);
    ticks(&mut game, &idle(), 30);
    assert!(game.player().grounded);

    // In the air only the floor check is forced.
    let mut game = classic_game();
    Situation::Fresh.enter(&mut game);
    let player = game.player_mut();
    player.position += Vec3::Z * 800.0;
    player.grounded = false;
    ticks(&mut game, &idle(), 2);
    let before = *game.player();
    let report = retune(&mut game, taller).unwrap();
    assert_eq!(report.relatched, ["movement.capsule_half_height"]);
    assert_eq!(game.player().position, before.position);

    // A wider cylinder is reported and forces the floor check too.
    let wider = tuned(&[("movement.capsule_radius", 2)]);
    let mut game = classic_game();
    Situation::Fresh.enter(&mut game);
    let before = *game.player();
    let report = retune(&mut game, wider).unwrap();
    assert_eq!(report.relatched, ["movement.capsule_radius"]);
    assert_eq!(game.player().position, before.position);
    assert!(game.player().pawn.force_floor_check);
}

#[test]
fn a_live_key_changes_the_next_tick() {
    let lighter = tuned(&[("movement.custom_gravity_scaling", -8)]);
    let airborne = || {
        let mut game = classic_game();
        let player = game.player_mut();
        player.position += Vec3::Z * 1500.0;
        player.grounded = false;
        ticks(&mut game, &idle(), 3);
        game
    };
    let mut control = airborne();
    let mut game = airborne();
    let report = retune(&mut game, lighter).unwrap();
    assert_eq!(
        report,
        RelatchReport::default(),
        "nothing latched to rewrite"
    );
    assert_eq!(game.player(), control.player());
    let before = control.player().velocity.z;
    control.tick(&idle()).unwrap();
    game.tick(&idle()).unwrap();
    let classic_gain = control.player().velocity.z - before;
    let tuned_gain = game.player().velocity.z - before;
    assert!(classic_gain < 0.0);
    assert!(
        tuned_gain > classic_gain && tuned_gain < 0.0,
        "lighter gravity on the very next tick: {tuned_gain} vs {classic_gain}"
    );
}

#[test]
fn a_refused_retune_changes_nothing() {
    let mut game = classic_game();
    Situation::Sprinting.enter(&mut game);
    let before = format!("{game:?}");

    let mut invalid = PlayerParams::asamu_original();
    invalid.movement.jump_velocity.value = -5.0;
    assert!(matches!(
        retune(&mut game, invalid),
        Err(SetParamsError::Invalid(ParamError::OutOfRange {
            name: "movement.jump_velocity",
            ..
        }))
    ));
    assert_eq!(
        retune(&mut game, PlayerParams::placeholder()),
        Err(SetParamsError::PipelineChange)
    );
    let mut no_boots = PlayerParams::asamu_original();
    no_boots.boots = None;
    assert_eq!(
        retune(&mut game, no_boots),
        Err(SetParamsError::PipelineChange)
    );
    assert!(
        format!("{game:?}") == before,
        "a refused retune changed the game"
    );

    // Every overlay value goes through the same door: no overlay can remove
    // a script-layer group, so a retune from an overlay never switches the
    // pipeline.
    let mut overlay = Overlay::default();
    overlay
        .set("pawn.move_speed", TuneValue::Float(100.0))
        .unwrap();
    let applied = overlay.apply().unwrap();
    assert!(applied.pawn.is_some() && applied.gun.is_some() && applied.boots.is_some());
    assert!(retune(&mut game, applied).is_ok());
}
