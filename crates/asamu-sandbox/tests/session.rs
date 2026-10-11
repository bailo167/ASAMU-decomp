//! The session's commands, on the hand-made graybox level (ours; not
//! original content).
//!
//! The tests are differential wherever a command stands for an existing
//! game call: the same call is made on a second game and the two must stay
//! equal, so a legitimate change to the simulation never breaks them. No
//! number of the simulation is written down here.
#![allow(clippy::unwrap_used)]

use asamu_game::asamu_kismet::{Graph, LevelScripts, MatineeSet, RUNTIME_FORMAT, RUNTIME_VERSION};
use asamu_game::{Game, LevelScript};
use asamu_player::grapple_gun::UNLIMITED_GRAPPLES;
use asamu_player::{InputFrame, PlayerParams, grapple_gun, rocket_boots};
use asamu_sandbox::command::{Command, CommandError, SlotOp, TeleportTarget, TimeOp, Toggle};
use asamu_sandbox::keys::TuneValue;
use asamu_sandbox::overlay::{OverlayError, ParamSetLabel};
use asamu_sandbox::profile::Profile;
use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
use asamu_sandbox::session::{ACTION_LOG_CAP, Outcome, Session, SimCx};
use glam::Vec3;

fn started_graybox() -> Game {
    let mut game = Game::graybox().unwrap();
    game.start();
    game
}

/// `Session::execute` on a game without a level script.
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

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
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
    assert_eq!(a.state(), b.state(), "{what}: run state");
}

/// Runs both games on the same inputs and checks they stay equal.
fn assert_same_after_ticks(a: &mut Game, b: &mut Game, ticks: usize, what: &str) {
    for i in 0..ticks {
        let input = InputFrame {
            jump_pressed: i == 5,
            jump_held: (5..20).contains(&i),
            ..forward()
        };
        assert_eq!(a.tick(&input), b.tick(&input), "{what}: report of tick {i}");
    }
    assert_same(a, b, what);
}

/// Points the view at `target` with one tick of look input.
fn look_at(game: &mut Game, target: Vec3) {
    let d = target - game.eye_position();
    let p = *game.player();
    let input = InputFrame {
        look_yaw_delta: d.y.atan2(d.x) - p.yaw,
        look_pitch_delta: d.z.atan2(d.truncate().length()) - p.pitch,
        ..InputFrame::default()
    };
    game.tick(&input).unwrap();
}

/// Attaches the grapple to the graybox's first hook from the start.
fn attach(game: &mut Game) {
    let hook = game.level().grapple_points[0].position;
    look_at(game, hook);
    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let report = game.tick(&fire).unwrap();
    assert!(report.events.gun.attached.is_some(), "the hook is in reach");
    assert!(game.player().is_grapple_attached());
}

// ---------------------------------------------------------------------------
// Ability commands are the game's own calls.
// ---------------------------------------------------------------------------

#[test]
fn ability_commands_equal_the_direct_game_calls() {
    type Direct = fn(&mut Game);
    let cases: Vec<(Command, Direct)> = vec![
        (Command::SetMaxGrapples { n: 0 }, |g| g.set_max_grapples(0)),
        (Command::SetMaxGrapples { n: 2 }, |g| g.set_max_grapples(2)),
        (Command::SetMaxGrapples { n: -1 }, |g| {
            g.set_max_grapples(-1)
        }),
        (Command::RocketBoots { on: Toggle::Off }, |g| {
            g.enable_rocket_boots(false)
        }),
        (Command::RocketBoots { on: Toggle::On }, |g| {
            g.enable_rocket_boots(true)
        }),
        (
            // The graybox starts with the boots on: a toggle turns them off.
            Command::RocketBoots { on: Toggle::Toggle },
            |g| {
                let on = g.player().script.boots.enabled;
                g.enable_rocket_boots(!on);
            },
        ),
        (
            Command::StoryMode { on: Toggle::On },
            Game::enter_story_mode,
        ),
        (Command::StoryMode { on: Toggle::Toggle }, |g| {
            g.toggle_story_mode();
        }),
        (Command::RefillGrapples, |g| {
            grapple_gun::reset_grapple_amount(g.player_mut());
        }),
        (Command::ResetBoots, |g| {
            rocket_boots::reset_boots(&mut g.player_mut().script.boots);
        }),
        (Command::ActivateAttractors, |g| {
            let pads: Vec<u32> = g.level().attractors.iter().map(|a| a.id).collect();
            for id in pads {
                assert!(g.activate_attractor(id));
            }
        }),
        (Command::Respawn, Game::respawn),
        (Command::Kill, Game::kill_player),
    ];
    for (cmd, direct) in cases {
        let what = format!("{cmd:?}");
        // A few ticks in, with a grapple spent, so refills and respawns have
        // something to do.
        let mut ours = started_graybox();
        for _ in 0..30 {
            ours.tick(&forward()).unwrap();
        }
        ours.player_mut().script.gun.times_grappled = 2;
        let mut theirs = ours.clone();

        let mut session = Session::classic();
        let outcome = exec(&mut session, &mut ours, cmd.clone()).unwrap();
        direct(&mut theirs);
        assert!(!outcome.message.is_empty(), "{what}");
        assert!(!outcome.params_changed, "{what}");
        assert_same(&ours, &theirs, &what);
        assert_same_after_ticks(&mut ours, &mut theirs, 90, &what);

        assert_eq!(session.log().len(), 1, "{what}");
        assert_eq!(session.log().actions()[0].cmd, cmd, "{what}");
        assert_eq!(session.log().actions()[0].tick, 30, "{what}");
        assert!(!session.is_pristine(), "{what}");
        // No ability command touches the parameters.
        assert_eq!(*ours.params(), PlayerParams::asamu_original(), "{what}");
        assert_eq!(session.label(), ParamSetLabel::Classic, "{what}");
    }
}

#[test]
fn story_mode_off_leaves_story_mode_as_the_game_call_does() {
    let mut ours = started_graybox();
    ours.enter_story_mode();
    let mut theirs = ours.clone();
    let mut session = Session::classic();
    exec(
        &mut session,
        &mut ours,
        Command::StoryMode { on: Toggle::Off },
    )
    .unwrap();
    theirs.exit_story_mode();
    assert!(!ours.in_story_mode());
    assert_same(&ours, &theirs, "story mode off");
    // Asking for the state it already has changes nothing.
    exec(
        &mut session,
        &mut ours,
        Command::StoryMode { on: Toggle::Off },
    )
    .unwrap();
    assert_same(&ours, &theirs, "story mode off twice");
}

#[test]
fn cycle_grapples_walks_zero_to_unlimited_and_round() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    // The cycle the app's debug key has: 0, 1, 2, 3, unlimited, 0, ...
    let mut seen = vec![game.player().script.gun.max_grapples];
    for _ in 0..5 {
        exec(&mut session, &mut game, Command::CycleGrapples).unwrap();
        seen.push(game.player().script.gun.max_grapples);
    }
    let start = seen[0];
    assert_eq!(seen[5], start, "five presses come round: {seen:?}");
    let mut sorted = seen[..5].to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, [0, 1, 2, 3, UNLIMITED_GRAPPLES]);
    // Each step is what the level's own action would store.
    for pair in seen.windows(2) {
        let mut reference = started_graybox();
        let argument = match pair[0] {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => -1,
            _ => 0,
        };
        reference.set_max_grapples(argument);
        assert_eq!(pair[1], reference.player().script.gun.max_grapples);
    }
}

#[test]
fn pinned_abilities_refuse_one_off_commands() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let rules = Rules {
        grapples: GrappleRule::Fixed(1),
        rocket_boots: Switch::Off,
        auto_refill: false,
    };
    let outcome = exec(&mut session, &mut game, Command::SetRules { rules }).unwrap();
    assert!(!outcome.message.is_empty());
    assert_eq!(*session.rules(), rules);
    // The rules apply at once, not only before the next tick.
    assert_eq!(game.player().script.gun.max_grapples, 1);
    assert!(!game.player().script.boots.enabled);

    let before = *game.player();
    let logged = session.log().len();
    for cmd in [
        Command::SetMaxGrapples { n: 3 },
        Command::CycleGrapples,
        Command::RocketBoots { on: Toggle::On },
    ] {
        let result = exec(&mut session, &mut game, cmd.clone());
        assert!(matches!(result, Err(CommandError::Refused(_))), "{cmd:?}");
    }
    assert_eq!(*game.player(), before);
    assert_eq!(
        session.log().len(),
        logged,
        "refused commands are not logged"
    );

    // Back to the level's rules: the one-off commands work again.
    exec(
        &mut session,
        &mut game,
        Command::SetRules {
            rules: Rules::default(),
        },
    )
    .unwrap();
    exec(&mut session, &mut game, Command::SetMaxGrapples { n: 3 }).unwrap();
    assert_eq!(game.player().script.gun.max_grapples, 3);
}

#[test]
fn ability_commands_need_the_script_layer() {
    let mut game = Game::graybox_placeholder().unwrap();
    game.start();
    let before = *game.player();
    let mut session = Session::classic();
    for cmd in [
        Command::SetMaxGrapples { n: 3 },
        Command::CycleGrapples,
        Command::RocketBoots { on: Toggle::On },
        Command::StoryMode { on: Toggle::On },
        Command::RefillGrapples,
        Command::ResetBoots,
    ] {
        let result = exec(&mut session, &mut game, cmd.clone());
        assert!(matches!(result, Err(CommandError::Refused(_))), "{cmd:?}");
    }
    // Tuning a placeholder game is refused by the game itself.
    let tune = Command::SetParam {
        key: "movement.jump_velocity".to_owned(),
        value: TuneValue::Float(10.0),
    };
    assert!(matches!(
        exec(&mut session, &mut game, tune),
        Err(CommandError::Params(_))
    ));
    assert_eq!(*game.player(), before);
    assert_eq!(*game.params(), PlayerParams::placeholder());
    assert!(session.overlay().is_empty());
    assert!(session.log().is_empty());
}

// ---------------------------------------------------------------------------
// Teleports.
// ---------------------------------------------------------------------------

#[test]
fn teleport_sets_the_documented_fields() {
    let mut game = started_graybox();
    for _ in 0..20 {
        game.tick(&forward()).unwrap();
    }
    let before = *game.player();
    assert!(before.grounded && before.velocity != Vec3::ZERO);
    let mut session = Session::classic();
    let target = [100.0, -50.0, 400.0];
    let outcome = exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Position {
                position: target,
                yaw: Some(0.75),
                pitch: Some(-0.25),
            },
        },
    )
    .unwrap();
    assert!(outcome.discontinuity);
    assert!(!outcome.params_changed);

    let after = *game.player();
    // The recipe: position, view, zero velocity, airborne, a forced floor
    // check, and the cached point of view following the view.
    assert_eq!(after.position, Vec3::from_array(target));
    assert_eq!(after.yaw, 0.75);
    assert_eq!(after.pitch, -0.25);
    assert_eq!(after.velocity, Vec3::ZERO);
    assert!(!after.grounded);
    assert!(after.pawn.force_floor_check);
    assert_eq!(after.script.pov_yaw, 0.75);
    assert_eq!(after.script.pov_pitch, -0.25);
    // Nothing else moved: undo those fields and the state is the old one.
    let mut undone = after;
    undone.position = before.position;
    undone.yaw = before.yaw;
    undone.pitch = before.pitch;
    undone.velocity = before.velocity;
    undone.grounded = before.grounded;
    undone.pawn.force_floor_check = before.pawn.force_floor_check;
    undone.script.pov_yaw = before.script.pov_yaw;
    undone.script.pov_pitch = before.script.pov_pitch;
    assert_eq!(undone, before);

    // Without a view the player keeps looking where it looked.
    exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Position {
                position: [0.0, 0.0, 300.0],
                yaw: None,
                pitch: None,
            },
        },
    )
    .unwrap();
    assert_eq!(game.player().yaw, 0.75);
    assert_eq!(game.player().pitch, -0.25);

    // The player falls from there and lands; the state stays finite.
    for _ in 0..240 {
        let report = game.tick(&InputFrame::default()).unwrap();
        assert!(!report.events.non_finite_rejected);
    }
    assert!(game.player().is_finite());
    assert!(game.player().grounded);
}

#[test]
fn a_teleport_view_is_wrapped_and_clamped_like_a_look() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Position {
                position: [0.0, 0.0, 200.0],
                yaw: Some(7.0),
                pitch: Some(40.0),
            },
        },
    )
    .unwrap();
    let p = *game.player();
    assert_eq!(p.yaw, asamu_core::rotator::wrap_radians(7.0));
    assert!((-std::f32::consts::PI..std::f32::consts::PI).contains(&p.yaw));
    let max_pitch = game.params().camera.max_pitch_degrees.value.to_radians();
    assert_eq!(p.pitch, max_pitch);
    // A tick does not move the view further: it was already within bounds.
    game.tick(&InputFrame::default()).unwrap();
    assert!((game.player().yaw - p.yaw).abs() < 1.0e-5);
    assert_eq!(game.player().pitch, p.pitch);
}

#[test]
fn teleport_and_fly_are_refused_while_the_grapple_is_attached() {
    let mut game = started_graybox();
    attach(&mut game);
    let before = game.clone();
    let mut session = Session::classic();
    for cmd in [
        Command::Teleport {
            to: TeleportTarget::Start,
        },
        Command::Teleport {
            to: TeleportTarget::Position {
                position: [0.0, 0.0, 500.0],
                yaw: None,
                pitch: None,
            },
        },
        Command::Teleport {
            to: TeleportTarget::Checkpoint { id: 1 },
        },
        Command::Teleport {
            to: TeleportTarget::NextTarget,
        },
        Command::Fly { on: Toggle::On },
    ] {
        match exec(&mut session, &mut game, cmd.clone()) {
            Err(CommandError::Refused(why)) => assert!(why.contains("attached"), "{why}"),
            other => panic!("{cmd:?} was not refused: {other:?}"),
        }
    }
    assert_same(&game, &before, "refused while attached");
    assert!(game.player().is_grapple_attached());
    assert!(session.log().is_empty());
    assert!(!session.flying());
    assert!(session.time().is_default());
    assert!(session.is_pristine());
}

#[test]
fn bad_teleports_are_refused() {
    let mut game = started_graybox();
    let before = *game.player();
    let mut session = Session::classic();
    for to in [
        TeleportTarget::Position {
            position: [f32::NAN, 0.0, 0.0],
            yaw: None,
            pitch: None,
        },
        TeleportTarget::Position {
            position: [0.0, f32::INFINITY, 0.0],
            yaw: None,
            pitch: None,
        },
        TeleportTarget::Position {
            position: [0.0, 0.0, 0.0],
            yaw: Some(f32::NAN),
            pitch: None,
        },
        TeleportTarget::Position {
            position: [0.0, 1.0e30, 0.0],
            yaw: None,
            pitch: None,
        },
        TeleportTarget::Checkpoint { id: 9999 },
        TeleportTarget::Mark {
            name: "nowhere".to_owned(),
        },
    ] {
        let result = exec(
            &mut session,
            &mut game,
            Command::Teleport { to: to.clone() },
        );
        assert!(matches!(result, Err(CommandError::Refused(_))), "{to:?}");
    }
    assert_eq!(*game.player(), before);
    assert!(session.log().is_empty());
}

/// How high above its floor point a pawn of a fresh game rests once it has
/// settled (the simulation's own answer; no number is assumed here).
fn resting_height() -> f32 {
    let mut game = started_graybox();
    for _ in 0..10 {
        game.tick(&InputFrame::default()).unwrap();
    }
    assert!(game.player().grounded);
    game.player().position.z - game.level().player_start.feet.z
}

#[test]
fn start_and_checkpoint_teleports_stand_the_player_on_the_spawn() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let level = game.level().clone();
    let spawned = Game::graybox().unwrap().player().position.z - level.player_start.feet.z;
    let resting = resting_height();
    for (to, feet) in [
        (TeleportTarget::Start, level.player_start.feet),
        (
            TeleportTarget::Checkpoint {
                id: level.checkpoints[0].id,
            },
            level.checkpoints[0].spawn.feet,
        ),
        (
            TeleportTarget::Checkpoint {
                id: level.checkpoints[1].id,
            },
            level.checkpoints[1].spawn.feet,
        ),
    ] {
        exec(
            &mut session,
            &mut game,
            Command::Teleport { to: to.clone() },
        )
        .unwrap();
        let p = *game.player();
        assert_eq!(p.position.truncate(), feet.truncate(), "{to:?}");
        // Just above the floor point, where a spawn places the pawn...
        let height = p.position.z - feet.z;
        assert!(
            (height - spawned).abs() < 0.01,
            "{to:?}: {height} vs {spawned}"
        );
        // ...and it settles there as a spawned pawn does.
        for _ in 0..10 {
            game.tick(&InputFrame::default()).unwrap();
        }
        let settled = *game.player();
        assert!(settled.grounded, "{to:?}");
        assert_eq!(settled.position.truncate(), feet.truncate(), "{to:?}");
        let height = settled.position.z - feet.z;
        assert!(
            (height - resting).abs() < 0.01,
            "{to:?}: {height} vs {resting}"
        );
    }
}

#[test]
fn bookmarks_and_the_target_list() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let checkpoints = game.level().checkpoints.len();
    assert_eq!(session.teleport_targets(&game).len(), 1 + checkpoints);

    // A bookmark remembers the place and the view.
    for _ in 0..40 {
        game.tick(&forward()).unwrap();
    }
    look_at(&mut game, Vec3::new(500.0, 300.0, 200.0));
    let marked = *game.player();
    let outcome = exec(
        &mut session,
        &mut game,
        Command::SetMark {
            name: "a".to_owned(),
        },
    )
    .unwrap();
    assert!(!outcome.discontinuity);
    assert_eq!(*game.player(), marked, "setting a bookmark moves nothing");
    assert_eq!(session.marks_on(&game.level().name), ["a"]);
    assert!(session.marks_on("another level").is_empty());

    exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Start,
        },
    )
    .unwrap();
    assert_ne!(game.player().position, marked.position);
    exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Mark {
                name: "a".to_owned(),
            },
        },
    )
    .unwrap();
    assert_eq!(game.player().position, marked.position);
    assert_eq!(game.player().yaw, marked.yaw);
    assert_eq!(game.player().pitch, marked.pitch);

    // The list: start, checkpoints, bookmarks. `NextTarget` walks it in
    // order and comes round.
    let targets = session.teleport_targets(&game);
    assert_eq!(targets.len(), 2 + checkpoints);
    assert_eq!(targets[0].target, TeleportTarget::Start);
    assert_eq!(
        targets.last().map(|t| t.target.clone()),
        Some(TeleportTarget::Mark {
            name: "a".to_owned()
        })
    );
    let mut visited = Vec::new();
    for _ in 0..=targets.len() {
        exec(
            &mut session,
            &mut game,
            Command::Teleport {
                to: TeleportTarget::NextTarget,
            },
        )
        .unwrap();
        visited.push(game.player().position);
    }
    for (target, position) in targets.iter().zip(&visited) {
        let mut reference = game.clone();
        let mut other = Session::classic();
        // The same bookmark in the reference session.
        reference.player_mut().position = marked.position;
        reference.player_mut().yaw = marked.yaw;
        reference.player_mut().pitch = marked.pitch;
        exec(
            &mut other,
            &mut reference,
            Command::SetMark {
                name: "a".to_owned(),
            },
        )
        .unwrap();
        exec(
            &mut other,
            &mut reference,
            Command::Teleport {
                to: target.target.clone(),
            },
        )
        .unwrap();
        assert_eq!(*position, reference.player().position, "{}", target.label);
    }
    assert_eq!(visited[targets.len()], visited[0], "the list comes round");

    // Bookmark names are bounded.
    for name in [String::new(), "x".repeat(41), "two\nlines".to_owned()] {
        let result = exec(&mut session, &mut game, Command::SetMark { name });
        assert!(matches!(result, Err(CommandError::Refused(_))));
    }
}

#[test]
fn teleport_to_the_crosshair_places_the_player_against_the_surface() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    // Looking at the start platform's floor ahead.
    look_at(&mut game, Vec3::new(200.0, -100.0, 0.0));
    let aim = game.gun_aim().unwrap();
    let hit = aim.impact.hit.expect("the floor is in the crosshair");
    assert!(hit.normal.z > 0.9);
    exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::AimPoint,
        },
    )
    .unwrap();
    let p = *game.player();
    let half_height = game.params().movement.capsule_half_height.value;
    assert!((p.position.truncate() - aim.impact.location.truncate()).length() < 0.01);
    let above = p.position.z - aim.impact.location.z;
    assert!(above > half_height && above < half_height + 1.0, "{above}");
    // Standing there after a few ticks, as a spawned pawn stands: not
    // pushed out of anything.
    for _ in 0..10 {
        game.tick(&InputFrame::default()).unwrap();
    }
    let settled = *game.player();
    assert!(settled.grounded);
    assert!((settled.position.truncate() - p.position.truncate()).length() < 0.01);
    let height = settled.position.z - aim.impact.location.z;
    assert!((height - resting_height()).abs() < 0.01, "{height}");

    // Looking at the sky: nothing to stand against.
    let sky = game.eye_position() + Vec3::new(0.0, -1.0, 5.0);
    look_at(&mut game, sky);
    assert!(game.gun_aim().unwrap().impact.hit.is_none());
    let before = *game.player();
    let result = exec(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::AimPoint,
        },
    );
    assert!(matches!(result, Err(CommandError::Refused(_))));
    assert_eq!(*game.player(), before);
}

// ---------------------------------------------------------------------------
// Fly placement and time.
// ---------------------------------------------------------------------------

#[test]
fn fly_placement_freezes_moves_and_leaves_one_logged_teleport() {
    let mut game = started_graybox();
    for _ in 0..10 {
        game.tick(&forward()).unwrap();
    }
    let mut session = Session::classic();
    let mut script = None;
    let mut cx = SimCx {
        game: &mut game,
        script: &mut script,
    };
    // Outside fly placement the move does nothing.
    let start = cx.game.player().position;
    session.fly_move(&mut cx, Vec3::new(10.0, 0.0, 0.0));
    assert_eq!(cx.game.player().position, start);

    session
        .execute(Command::Fly { on: Toggle::On }, &mut cx)
        .unwrap();
    assert!(session.flying());
    assert!(session.time().frozen(), "fly placement freezes time");
    assert_eq!(cx.game.player().velocity, Vec3::ZERO);

    session.fly_move(&mut cx, Vec3::new(120.0, -40.0, 300.0));
    session.fly_move(&mut cx, Vec3::new(f32::NAN, 0.0, 0.0));
    session.fly_move(&mut cx, Vec3::new(0.0, 0.0, 20.0));
    let placed = (start + Vec3::new(120.0, -40.0, 300.0)) + Vec3::new(0.0, 0.0, 20.0);
    assert_eq!(cx.game.player().position, placed);
    // Moves are not logged one by one.
    assert_eq!(session.log().len(), 1);

    let outcome = session
        .execute(Command::Fly { on: Toggle::Off }, &mut cx)
        .unwrap();
    assert!(outcome.discontinuity);
    assert!(!session.flying());
    assert!(
        !session.time().frozen(),
        "time was running before: it resumes"
    );
    let p = *cx.game.player();
    assert_eq!(p.position, placed);
    assert_eq!(p.velocity, Vec3::ZERO);
    assert!(!p.grounded && p.pawn.force_floor_check);
    // One teleport says where the player ended up, then the command.
    let log = session.log().actions();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log[1].cmd,
        Command::Teleport {
            to: TeleportTarget::Position {
                position: placed.to_array(),
                yaw: Some(p.yaw),
                pitch: Some(p.pitch),
            }
        }
    );
    assert_eq!(log[2].cmd, Command::Fly { on: Toggle::Off });
}

#[test]
fn fly_placement_keeps_a_freeze_that_was_there_before() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(
        &mut session,
        &mut game,
        Command::Time {
            op: TimeOp::Freeze(Toggle::On),
        },
    )
    .unwrap();
    exec(&mut session, &mut game, Command::Fly { on: Toggle::Toggle }).unwrap();
    assert!(session.flying());
    exec(&mut session, &mut game, Command::Fly { on: Toggle::Toggle }).unwrap();
    assert!(!session.flying());
    assert!(session.time().frozen());
}

#[test]
fn time_never_runs_in_fly_placement() {
    // Unfreezing, stepping and resetting leave fly placement first.
    for op in [
        TimeOp::Freeze(Toggle::Off),
        TimeOp::Freeze(Toggle::Toggle),
        TimeOp::Step { n: 2 },
        TimeOp::Reset,
    ] {
        let mut game = started_graybox();
        let mut session = Session::classic();
        exec(&mut session, &mut game, Command::Fly { on: Toggle::On }).unwrap();
        let outcome = exec(&mut session, &mut game, Command::Time { op }).unwrap();
        assert!(!session.flying(), "{op:?}");
        assert!(outcome.discontinuity, "{op:?}");
        assert!(game.player().pawn.force_floor_check, "{op:?}");
    }
    // Changing the speed does not.
    for op in [
        TimeOp::Slower,
        TimeOp::Faster,
        TimeOp::Scale { value: 2.0 },
        TimeOp::Freeze(Toggle::On),
        TimeOp::Step { n: 0 },
    ] {
        let mut game = started_graybox();
        let mut session = Session::classic();
        exec(&mut session, &mut game, Command::Fly { on: Toggle::On }).unwrap();
        exec(&mut session, &mut game, Command::Time { op }).unwrap();
        assert!(session.flying(), "{op:?}");
        assert!(session.time().frozen(), "{op:?}");
    }
    // A host that unfroze behind the session's back: the tick hook lands
    // the player before the tick.
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(&mut session, &mut game, Command::Fly { on: Toggle::On }).unwrap();
    session.time_mut().set_frozen(false);
    let held = *game.player();
    let logged = session.log().len();
    let mut script = None;
    session.before_tick(&mut SimCx {
        game: &mut game,
        script: &mut script,
    });
    assert!(!session.flying());
    // The hook itself moves nothing: the pawn was already held as a teleport
    // leaves it, so it simply falls from where it was placed.
    assert_eq!(*game.player(), held);
    assert!(!held.grounded && held.pawn.force_floor_check);
    assert_eq!(
        session.log().len(),
        logged + 1,
        "where it was left is logged"
    );
}

#[test]
fn time_commands_drive_the_model() {
    let mut game = started_graybox();
    let before = game.clone();
    let mut session = Session::classic();
    let time = |session: &mut Session, game: &mut Game, op: TimeOp| {
        exec(session, game, Command::Time { op })
    };
    time(&mut session, &mut game, TimeOp::Freeze(Toggle::On)).unwrap();
    assert!(session.time().frozen());
    time(&mut session, &mut game, TimeOp::Step { n: 3 }).unwrap();
    assert_eq!(session.time().pending_steps(), 3);
    time(&mut session, &mut game, TimeOp::Freeze(Toggle::Toggle)).unwrap();
    assert!(!session.time().frozen());
    time(&mut session, &mut game, TimeOp::Slower).unwrap();
    assert!(session.time().speed() < 1.0);
    time(&mut session, &mut game, TimeOp::Faster).unwrap();
    time(&mut session, &mut game, TimeOp::Faster).unwrap();
    assert!(session.time().speed() > 1.0);
    time(&mut session, &mut game, TimeOp::Scale { value: 0.5 }).unwrap();
    assert_eq!(session.time().speed(), 0.5);
    let logged = session.log().len();
    for bad in [0.0, -2.0, 1.0e9, f32::NAN] {
        let result = time(&mut session, &mut game, TimeOp::Scale { value: bad });
        assert!(matches!(result, Err(CommandError::Refused(_))), "{bad}");
    }
    assert_eq!(session.time().speed(), 0.5);
    assert_eq!(session.log().len(), logged);
    time(&mut session, &mut game, TimeOp::Reset).unwrap();
    assert!(session.time().is_default());
    assert!(session.time().was_used());
    assert!(!session.is_pristine());
    // Time control is the session's alone: the game was never written.
    assert_same(&game, &before, "time commands");
}

// ---------------------------------------------------------------------------
// Parameters.
// ---------------------------------------------------------------------------

/// A float key of the Classic set and its value.
fn classic_float(session: &Session, key: &str) -> f64 {
    match session.catalog().get(key).map(|info| info.classic.clone()) {
        Some(TuneValue::Float(v)) => v,
        other => panic!("{key} is not a float key: {other:?}"),
    }
}

#[test]
fn parameter_commands_tune_the_running_game() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let key = "movement.custom_gravity_scaling";
    let classic = classic_float(&session, key);

    let outcome = exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: key.to_owned(),
            value: TuneValue::Float(classic * 0.5),
        },
    )
    .unwrap();
    assert!(outcome.params_changed);
    assert!(outcome.message.contains(key), "{}", outcome.message);
    assert_eq!(session.overlay().len(), 1);
    assert_eq!(session.label(), ParamSetLabel::Modified { overrides: 1 });
    assert!(session.profile_modified());
    // The game runs the session's set, and that set is the overlay applied.
    assert_eq!(game.params(), session.params());
    assert_eq!(*session.params(), session.overlay().apply().unwrap());
    assert_eq!(
        f64::from(game.params().movement.custom_gravity_scaling.value),
        classic * 0.5
    );
    assert_ne!(*game.params(), PlayerParams::asamu_original());
    // The banner never calls a modified set the original's.
    let banner = session.banner(&game);
    assert!(banner.contains("MODIFIED"), "{banner}");
    assert!(banner.contains("NOT the original's values"), "{banner}");
    assert!(banner.contains(key), "{banner}");

    // A nudge moves it; the reverse nudge moves it back.
    let nudged = Command::NudgeParam {
        key: key.to_owned(),
        steps: 2,
        scale: 1.0,
    };
    exec(&mut session, &mut game, nudged).unwrap();
    let up = game.params().movement.custom_gravity_scaling.value;
    assert!(f64::from(up) > classic * 0.5);
    assert_eq!(game.params(), session.params());
    exec(
        &mut session,
        &mut game,
        Command::NudgeParam {
            key: key.to_owned(),
            steps: -2,
            scale: 1.0,
        },
    )
    .unwrap();
    assert!(
        (f64::from(game.params().movement.custom_gravity_scaling.value) - classic * 0.5).abs()
            < 1e-6
    );

    // A second key, then one reset, then all.
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
    )
    .unwrap();
    assert_eq!(session.overlay().len(), 2);
    let outcome = exec(
        &mut session,
        &mut game,
        Command::ResetParam {
            key: key.to_owned(),
        },
    )
    .unwrap();
    assert!(outcome.params_changed);
    assert_eq!(session.overlay().len(), 1);
    assert_eq!(
        f64::from(game.params().movement.custom_gravity_scaling.value),
        classic
    );
    // Resetting a key that is not overridden is a no-op, not an error.
    let outcome = exec(
        &mut session,
        &mut game,
        Command::ResetParam {
            key: key.to_owned(),
        },
    )
    .unwrap();
    assert!(!outcome.params_changed);

    let outcome = exec(&mut session, &mut game, Command::ResetAllParams).unwrap();
    assert!(outcome.params_changed);
    assert!(session.overlay().is_empty());
    assert_eq!(session.label(), ParamSetLabel::Classic);
    assert_eq!(*game.params(), PlayerParams::asamu_original());
    assert_eq!(*session.params(), PlayerParams::asamu_original());
    // Classic again, but not pristine: commands ran.
    assert!(!session.is_pristine());
}

#[test]
fn a_tuned_parameter_changes_the_simulation() {
    // Half gravity: the same scripted jump goes higher than under Classic.
    let jump = |game: &mut Game| {
        let mut top = f32::MIN;
        for i in 0..400 {
            let input = InputFrame {
                jump_pressed: i == 0,
                jump_held: true,
                ..InputFrame::default()
            };
            game.tick(&input).unwrap();
            top = top.max(game.player().position.z);
        }
        top
    };
    let mut classic_game = started_graybox();
    let classic_top = jump(&mut classic_game);

    let mut game = started_graybox();
    let mut session = Session::classic();
    let key = "movement.custom_gravity_scaling";
    let classic = classic_float(&session, key);
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: key.to_owned(),
            value: TuneValue::Float(classic * 0.5),
        },
    )
    .unwrap();
    let floaty_top = jump(&mut game);
    assert!(
        floaty_top > classic_top + 1.0,
        "{floaty_top} vs {classic_top}"
    );
}

#[test]
fn a_refused_parameter_changes_nothing_and_is_not_logged() {
    let mut game = started_graybox();
    let before = game.clone();
    let mut session = Session::classic();
    let cases = [
        // No such key (with a suggestion).
        Command::SetParam {
            key: "movement.jump_velocty".to_owned(),
            value: TuneValue::Float(1.0),
        },
        Command::NudgeParam {
            key: "no.such_key".to_owned(),
            steps: 1,
            scale: 1.0,
        },
        Command::ResetParam {
            key: "no.such_key".to_owned(),
        },
        // Wrong type.
        Command::SetParam {
            key: "movement.jump_velocity".to_owned(),
            value: TuneValue::Bool(true),
        },
        // Not a number the parameter can hold.
        Command::SetParam {
            key: "movement.jump_velocity".to_owned(),
            value: TuneValue::Float(f64::NAN),
        },
        // Fails the Classic validation.
        Command::SetParam {
            key: "movement.capsule_radius".to_owned(),
            value: TuneValue::Float(-1.0),
        },
    ];
    for cmd in cases {
        let result = exec(&mut session, &mut game, cmd.clone());
        assert!(
            matches!(result, Err(CommandError::Overlay(_))),
            "{cmd:?}: {result:?}"
        );
    }
    // The typo gets a suggestion.
    let typo = exec(
        &mut session,
        &mut game,
        Command::ResetParam {
            key: "movement.jump_velocty".to_owned(),
        },
    );
    match typo {
        Err(CommandError::Overlay(OverlayError::UnknownKey { key, suggestion })) => {
            assert_eq!(key, "movement.jump_velocty");
            assert_eq!(suggestion.as_deref(), Some("movement.jump_velocity"));
        }
        other => panic!("{other:?}"),
    }
    assert_same(&game, &before, "refused parameters");
    assert!(session.overlay().is_empty());
    assert_eq!(*session.params(), PlayerParams::asamu_original());
    assert!(session.log().is_empty());
    assert!(session.is_pristine());
}

#[test]
fn load_profile_replaces_overrides_rules_and_speed() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let builtin = Profile::builtin();
    let find = |name: &str| {
        builtin
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("built-in profile {name}"))
            .clone()
    };

    // An own override first: a profile replaces it.
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
    )
    .unwrap();
    let moon = find("moon");
    let outcome = exec(
        &mut session,
        &mut game,
        Command::LoadProfile {
            profile: Box::new(moon.clone()),
        },
    )
    .unwrap();
    assert!(outcome.params_changed);
    assert_eq!(session.profile().name, "moon");
    assert_eq!(*session.overlay(), moon.overrides);
    assert_eq!(*session.params(), moon.overrides.apply().unwrap());
    assert_eq!(game.params(), session.params());
    assert!(!session.profile_modified(), "freshly loaded");

    // Rules come with the profile and apply at once.
    let grapple = find("infinite-grapple");
    exec(
        &mut session,
        &mut game,
        Command::LoadProfile {
            profile: Box::new(grapple.clone()),
        },
    )
    .unwrap();
    assert_eq!(*session.rules(), grapple.rules);
    assert!(session.overlay().is_empty());
    assert_eq!(*game.params(), PlayerParams::asamu_original());
    assert_eq!(game.player().script.gun.max_grapples, UNLIMITED_GRAPPLES);

    // So does the speed; a profile without one means normal speed.
    let slow = find("bullet-time");
    exec(
        &mut session,
        &mut game,
        Command::LoadProfile {
            profile: Box::new(slow.clone()),
        },
    )
    .unwrap();
    assert_eq!(Some(session.time().speed()), slow.time_scale);
    assert!(session.rules().is_default());
    exec(
        &mut session,
        &mut game,
        Command::LoadProfile {
            profile: Box::new(Profile::classic()),
        },
    )
    .unwrap();
    assert_eq!(session.time().speed(), 1.0);
    assert_eq!(session.label(), ParamSetLabel::Classic);

    // A profile on another base is refused whole.
    let foreign = Profile {
        base: "placeholder".to_owned(),
        rules: Rules {
            auto_refill: true,
            ..Rules::default()
        },
        ..Profile::classic()
    };
    let result = exec(
        &mut session,
        &mut game,
        Command::LoadProfile {
            profile: Box::new(foreign),
        },
    );
    assert!(matches!(result, Err(CommandError::Refused(_))));
    assert!(session.rules().is_default());
    assert_eq!(session.profile().name, "classic");
}

#[test]
fn every_builtin_profile_loads_into_a_running_game() {
    for profile in Profile::builtin() {
        let mut game = started_graybox();
        for _ in 0..20 {
            game.tick(&forward()).unwrap();
        }
        let mut session = Session::classic();
        exec(
            &mut session,
            &mut game,
            Command::LoadProfile {
                profile: Box::new(profile.clone()),
            },
        )
        .unwrap();
        assert_eq!(game.params(), session.params(), "{}", profile.name);
        // The same as starting a session from the profile.
        let fresh = Session::new(profile.clone()).unwrap();
        assert_eq!(fresh.params(), session.params(), "{}", profile.name);
        assert_eq!(fresh.rules(), session.rules(), "{}", profile.name);
        assert_eq!(
            fresh.time().speed(),
            session.time().speed(),
            "{}",
            profile.name
        );
        // And it plays: a scripted run stays finite.
        let mut script = None;
        for i in 0..300 {
            let input = InputFrame {
                jump_pressed: i % 60 == 10,
                jump_held: i % 60 > 10,
                sprint_held: i > 100,
                ..forward()
            };
            session.before_tick(&mut SimCx {
                game: &mut game,
                script: &mut script,
            });
            let report = game.tick(&input).unwrap();
            assert!(!report.events.non_finite_rejected, "{}", profile.name);
            session.after_tick(&game, None, &report);
        }
        assert!(game.player().is_finite(), "{}", profile.name);
    }
}

#[test]
fn to_profile_round_trips() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let key = "movement.custom_gravity_scaling";
    let classic = classic_float(&session, key);
    let rules = Rules {
        grapples: GrappleRule::Unlimited,
        rocket_boots: Switch::Off,
        auto_refill: true,
    };
    for cmd in [
        Command::SetParam {
            key: key.to_owned(),
            value: TuneValue::Float(classic * 0.5),
        },
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
        Command::SetRules { rules },
        Command::Time {
            op: TimeOp::Scale { value: 0.5 },
        },
    ] {
        exec(&mut session, &mut game, cmd).unwrap();
    }

    let profile = session.to_profile("my-lab");
    assert_eq!(profile.name, "my-lab");
    assert_eq!(profile.overrides, *session.overlay());
    assert_eq!(profile.rules, rules);
    assert_eq!(profile.time_scale, Some(0.5));
    assert!(!profile.is_pristine());

    // Through its file form and back.
    let json = profile.to_json_pretty().unwrap();
    let read = Profile::from_json_slice(json.as_bytes()).unwrap();
    assert_eq!(read, profile);

    // A session from it runs the same set, rules and speed.
    let again = Session::new(read).unwrap();
    assert_eq!(again.params(), session.params());
    assert_eq!(again.rules(), session.rules());
    assert_eq!(again.time().speed(), session.time().speed());
    assert_eq!(again.to_profile("my-lab").overrides, profile.overrides);

    // A pristine session saves as a pristine profile.
    let pristine = Session::classic().to_profile("plain");
    assert!(pristine.is_pristine());
    assert_eq!(pristine.time_scale, None);
}

// ---------------------------------------------------------------------------
// Rules on a scripted level.
// ---------------------------------------------------------------------------

/// A synthetic level script (written here, by us): at level start it sets
/// the grapple capacity to 3 and enables the rocket boots, as a level's own
/// Kismet would.
fn level_start_scripts() -> LevelScripts {
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
            r#"{"Grapples": 3}"#
        ),
        action(
            3,
            "asamu.SeqAct_ToggleRocketBoots",
            None,
            r#"{"Enable": true}"#
        ),
    );
    let doc = format!(
        r#"{{"format": "{RUNTIME_FORMAT}", "version": {RUNTIME_VERSION}, "package": "T",
            "nodes": {nodes}, "actors": []}}"#
    );
    LevelScripts {
        graph: Graph::from_json_slice(doc.as_bytes()).unwrap(),
        matinee: MatineeSet::default(),
        missing_sublevels: Vec::new(),
    }
}

#[test]
fn rules_survive_a_kismet_level_start() {
    // Control: without rules the level's script decides.
    let mut control = started_graybox();
    let mut control_script = LevelScript::new(&mut control, level_start_scripts());
    assert_eq!(control.player().script.gun.max_grapples, 0, "a fresh start");
    assert!(!control.player().script.boots.enabled);
    control_script
        .tick(&mut control, &InputFrame::default())
        .unwrap();
    assert_eq!(control.player().script.gun.max_grapples, 3);
    assert!(control.player().script.boots.enabled);
    assert!(control_script.runtime().errors().is_empty());

    // With rules: the session pins the abilities across the level start.
    let mut game = started_graybox();
    let mut script = Some(LevelScript::new(&mut game, level_start_scripts()));
    let rules = Rules {
        grapples: GrappleRule::Unlimited,
        rocket_boots: Switch::Off,
        auto_refill: false,
    };
    let mut session = Session::new(Profile {
        rules,
        ..Profile::classic()
    })
    .unwrap();
    for tick in 0..10 {
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        assert_eq!(session.reconcile(&mut cx), Ok(false));
        session.before_tick(&mut cx);
        // Every tick starts under the rules, whatever the script did in the
        // tick before (the level start is inside the first one).
        assert_eq!(
            game.player().script.gun.max_grapples,
            UNLIMITED_GRAPPLES,
            "tick {tick}"
        );
        assert!(!game.player().script.boots.enabled, "tick {tick}");
        let scripted = script
            .as_mut()
            .unwrap()
            .tick(&mut game, &InputFrame::default())
            .unwrap();
        session.after_tick(&game, script.as_ref(), &scripted.report);
    }
    // The script did fire: after the first tick it had written its values,
    // and the rules corrected them before the second.
    let mut probe = started_graybox();
    let mut probe_script = Some(LevelScript::new(&mut probe, level_start_scripts()));
    let mut probe_session = Session::new(Profile {
        rules,
        ..Profile::classic()
    })
    .unwrap();
    probe_session.before_tick(&mut SimCx {
        game: &mut probe,
        script: &mut probe_script,
    });
    probe_script
        .as_mut()
        .unwrap()
        .tick(&mut probe, &InputFrame::default())
        .unwrap();
    assert_eq!(probe.player().script.gun.max_grapples, 3);
    assert!(probe.player().script.boots.enabled);

    // Default rules on the same level change nothing: equal to the control.
    let mut plain = started_graybox();
    let mut plain_script = Some(LevelScript::new(&mut plain, level_start_scripts()));
    let mut pristine = Session::classic();
    let mut cx = SimCx {
        game: &mut plain,
        script: &mut plain_script,
    };
    pristine.before_tick(&mut cx);
    plain_script
        .as_mut()
        .unwrap()
        .tick(&mut plain, &InputFrame::default())
        .unwrap();
    assert_eq!(plain.player(), control.player());
}

// ---------------------------------------------------------------------------
// The log.
// ---------------------------------------------------------------------------

#[test]
fn the_action_log_is_bounded() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let extra = 50;
    for i in 0..(ACTION_LOG_CAP + extra) {
        let cmd = Command::Slot {
            op: SlotOp::Select { slot: i % 4 },
        };
        exec(&mut session, &mut game, cmd).unwrap();
        assert_eq!(session.log().truncated(), i >= ACTION_LOG_CAP);
    }
    assert_eq!(session.log().len(), ACTION_LOG_CAP);
    assert!(session.log().truncated());
    // The first ones are kept, the overflow is dropped.
    assert_eq!(
        session.log().actions()[ACTION_LOG_CAP - 1].cmd,
        Command::Slot {
            op: SlotOp::Select {
                slot: (ACTION_LOG_CAP - 1) % 4
            }
        }
    );
    // Commands still run when the log is full.
    assert_eq!(session.slots().selected(), (ACTION_LOG_CAP + extra - 1) % 4);
    assert!(!session.is_pristine());
}

#[test]
fn actions_are_stamped_with_the_tick_they_ran_before() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(&mut session, &mut game, Command::RefillGrapples).unwrap();
    for _ in 0..7 {
        game.tick(&forward()).unwrap();
    }
    exec(&mut session, &mut game, Command::CycleGrapples).unwrap();
    let ticks: Vec<u64> = session.log().actions().iter().map(|a| a.tick).collect();
    assert_eq!(ticks, [0, 7]);
    // The log is what a recording header stores: it survives JSON.
    let json = serde_json::to_string(session.log().actions()).unwrap();
    let back: Vec<asamu_sandbox::session::LoggedAction> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, session.log().actions());
}

#[test]
fn every_kind_of_command_runs_or_is_refused_cleanly() {
    // One of each variant on one session: none panics, the state stays
    // finite, and the game still ticks afterwards.
    let commands = vec![
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
        Command::NudgeParam {
            key: "movement.air_control".to_owned(),
            steps: 1,
            scale: 1.0,
        },
        Command::ResetParam {
            key: "movement.air_control".to_owned(),
        },
        Command::ResetAllParams,
        Command::LoadProfile {
            profile: Box::new(Profile::classic()),
        },
        Command::SetRules {
            rules: Rules::default(),
        },
        Command::SetMaxGrapples { n: 2 },
        Command::CycleGrapples,
        Command::RocketBoots { on: Toggle::Toggle },
        Command::StoryMode { on: Toggle::On },
        Command::StoryMode { on: Toggle::Off },
        Command::RefillGrapples,
        Command::ResetBoots,
        Command::ActivateAttractors,
        Command::SetMark {
            name: "here".to_owned(),
        },
        Command::Slot {
            op: SlotOp::Save { slot: 0 },
        },
        Command::Record { on: Toggle::On },
        Command::Teleport {
            to: TeleportTarget::Checkpoint { id: 1 },
        },
        Command::Teleport {
            to: TeleportTarget::NextTarget,
        },
        Command::Teleport {
            to: TeleportTarget::Mark {
                name: "here".to_owned(),
            },
        },
        Command::Fly { on: Toggle::On },
        Command::Fly { on: Toggle::Off },
        Command::Time {
            op: TimeOp::Step { n: 1 },
        },
        Command::Time { op: TimeOp::Reset },
        Command::Respawn,
        Command::Kill,
        Command::Slot {
            op: SlotOp::Load { slot: 0 },
        },
        Command::Slot {
            op: SlotOp::Clear { slot: 0 },
        },
        Command::Slot {
            op: SlotOp::Select { slot: 3 },
        },
        Command::Rewind,
        Command::Record { on: Toggle::Off },
    ];
    let mut game = started_graybox();
    let mut session = Session::classic();
    let mut script = None;
    let mut executed = 0;
    for cmd in commands {
        for _ in 0..40 {
            let mut cx = SimCx {
                game: &mut game,
                script: &mut script,
            };
            session.before_tick(&mut cx);
            let report = game.tick(&forward()).unwrap();
            session.after_tick(&game, None, &report);
        }
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        match session.execute(cmd.clone(), &mut cx) {
            Ok(outcome) => {
                executed += 1;
                assert!(!outcome.message.is_empty(), "{cmd:?}");
            }
            Err(CommandError::Refused(why)) => assert!(!why.is_empty(), "{cmd:?}"),
            Err(other) => panic!("{cmd:?}: {other}"),
        }
        assert!(game.player().is_finite(), "{cmd:?}");
        assert_eq!(game.params(), session.params(), "{cmd:?}");
    }
    // Every executed command is in the log, refused ones are not; the one
    // extra entry is the teleport that leaving fly placement logs.
    assert!(executed > 20, "{executed}");
    assert_eq!(executed, session.log().len() - 1);
    assert_eq!(*game.params(), PlayerParams::asamu_original());
}

// ---------------------------------------------------------------------------
// A converted level (synthetic: written here, no game data).
// ---------------------------------------------------------------------------

mod converted {
    use asamu_core::DEFAULT_TICK_RATE_HZ;
    use asamu_game::DEATH_FADE_DOWN_TIME;
    use asamu_sandbox::inspect::Inspection;
    use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, json};
    use asamu_world::rotation::units_to_radians;
    use asamu_world::scene::{self, LoadOptions, MemorySource};

    use super::*;

    const MAP: &str = "SandboxSessionMap";

    /// A floor, a player start, two checkpoints (the second with its own
    /// spawn marker and rotation) and a block. Every name and size is ours.
    fn fixture_game() -> Game {
        let mut src = MemorySource::new();
        let mut s = SceneFixture::new(MAP, -5000.0);
        s.set_bsp(
            vec![
                [-4000.0, -4000.0, 0.0],
                [4000.0, -4000.0, 0.0],
                [4000.0, 4000.0, 0.0],
                [-4000.0, 4000.0, 0.0],
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
        s.checkpoint(
            Place::at(Vec3::new(600.0, 0.0, 60.0)),
            150.0,
            60.0,
            0,
            json!({}),
        );
        let marker = s.marker(Place {
            location: Vec3::new(1500.0, 800.0, 60.0),
            rotation: [0, 16_384, 0],
            scale: Vec3::ONE,
        });
        let path = s.path(marker);
        s.checkpoint(
            Place::at(Vec3::new(1500.0, 0.0, 60.0)),
            150.0,
            60.0,
            1,
            json!({"spawnPointActor": path}),
        );
        s.static_mesh("Session.Block", Place::at(Vec3::new(-800.0, 0.0, 50.0)));
        s.write(&mut src);
        let mut meshes = MeshFixtures::new();
        let (vertices, triangles) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
        meshes.add("Session.Block", MAP, vertices, triangles);
        meshes.write(&mut src, true);

        let loaded = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
        let mut game =
            Game::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)
                .unwrap();
        game.set_max_grapples(3);
        game.start();
        // The start is above the floor: let the pawn land.
        for _ in 0..60 {
            game.tick(&InputFrame::default()).unwrap();
        }
        assert!(game.player().grounded);
        game
    }

    #[test]
    fn teleport_targets_are_the_maps_start_and_checkpoint_spawns() {
        let mut game = fixture_game();
        let mut session = Session::classic();
        let checkpoints = game.scene_map().unwrap().actors.checkpoints.clone();
        let start = game
            .scene_map()
            .unwrap()
            .actors
            .player_start()
            .unwrap()
            .clone();
        assert_eq!(checkpoints.len(), 2);

        let targets = session.teleport_targets(&game);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].target, TeleportTarget::Start);
        for (target, checkpoint) in targets[1..].iter().zip(&checkpoints) {
            assert_eq!(
                target.target,
                TeleportTarget::Checkpoint { id: checkpoint.id }
            );
            assert!(target.label.contains(&checkpoint.name), "{}", target.label);
        }

        // Each checkpoint: the pawn's centre at the respawn point, looking
        // the way the respawn would turn it.
        for checkpoint in &checkpoints {
            exec(
                &mut session,
                &mut game,
                Command::Teleport {
                    to: TeleportTarget::Checkpoint { id: checkpoint.id },
                },
            )
            .unwrap();
            let p = *game.player();
            assert_eq!(p.position, checkpoint.spawn_location, "{}", checkpoint.name);
            let yaw = units_to_radians(checkpoint.spawn_rotation[1]);
            assert!((p.yaw - yaw).abs() < 1.0e-6, "{}", checkpoint.name);
            assert_eq!(p.velocity, Vec3::ZERO);
            assert!(!p.grounded && p.pawn.force_floor_check);
            for _ in 0..30 {
                let report = game.tick(&InputFrame::default()).unwrap();
                assert!(report.died.is_none());
            }
            assert!(game.player().grounded, "{}: landed below", checkpoint.name);
            assert!(game.player().is_finite());
        }
        // The second checkpoint's spawn is its marker, not the checkpoint.
        assert_ne!(checkpoints[1].spawn_location, checkpoints[1].location);

        // The start: the map's player start.
        exec(
            &mut session,
            &mut game,
            Command::Teleport {
                to: TeleportTarget::Start,
            },
        )
        .unwrap();
        assert_eq!(game.player().position, start.location);

        // `NextTarget` walks the same list and comes round.
        let mut visited = Vec::new();
        for _ in 0..4 {
            exec(
                &mut session,
                &mut game,
                Command::Teleport {
                    to: TeleportTarget::NextTarget,
                },
            )
            .unwrap();
            visited.push(game.player().position);
        }
        assert_eq!(
            visited,
            [
                start.location,
                checkpoints[0].spawn_location,
                checkpoints[1].spawn_location,
                start.location
            ]
        );
        assert!(matches!(
            exec(
                &mut session,
                &mut game,
                Command::Teleport {
                    to: TeleportTarget::Checkpoint { id: 0xFFFF_FFF0 },
                },
            ),
            Err(CommandError::Refused(_))
        ));
    }

    #[test]
    fn kill_starts_the_death_sequence_and_respawn_resets_at_once() {
        let mut game = fixture_game();
        let mut session = Session::classic();
        for _ in 0..30 {
            game.tick(&forward()).unwrap();
        }
        // Kill: the game's own death sequence, as the direct call.
        let mut theirs = game.clone();
        let outcome = exec(&mut session, &mut game, Command::Kill).unwrap();
        theirs.kill_player();
        assert!(!outcome.discontinuity, "the reset comes after the fade");
        assert!(game.is_dying());
        assert_eq!(game.player(), theirs.player());
        // Moving the player by hand is refused while it runs.
        for cmd in [
            Command::Teleport {
                to: TeleportTarget::Start,
            },
            Command::Fly { on: Toggle::On },
        ] {
            assert!(matches!(
                exec(&mut session, &mut game, cmd),
                Err(CommandError::Refused(_))
            ));
        }
        let ticks = (f64::from(DEATH_FADE_DOWN_TIME) * game.clock().tick_rate_hz()) as usize + 3;
        let mut respawned = false;
        for _ in 0..ticks {
            let ours = game.tick(&InputFrame::default()).unwrap();
            let reference = theirs.tick(&InputFrame::default()).unwrap();
            assert_eq!(ours, reference);
            respawned |= ours.respawned;
        }
        assert!(respawned);
        assert!(!game.is_dying());
        assert_eq!(game.player(), theirs.player());

        // Respawn: the immediate reset, as the direct call.
        for _ in 0..30 {
            game.tick(&forward()).unwrap();
        }
        let mut theirs = game.clone();
        let outcome = exec(&mut session, &mut game, Command::Respawn).unwrap();
        theirs.respawn();
        assert!(outcome.discontinuity);
        assert!(!game.is_dying());
        assert_eq!(game.player(), theirs.player());
        assert_eq!(game.respawn_count(), theirs.respawn_count());
    }

    #[test]
    fn slots_work_and_rewind_is_refused_on_a_converted_level() {
        let mut game = fixture_game();
        let mut session = Session::classic();
        let mut script = None;
        for _ in 0..90 {
            session.before_tick(&mut SimCx {
                game: &mut game,
                script: &mut script,
            });
            let report = game.tick(&forward()).unwrap();
            session.after_tick(&game, None, &report);
        }
        // No keyframes are taken here, and the view says rewind is off.
        assert!(session.rewind().is_empty());
        let look = Inspection::new(&game, None, &session);
        let summary = look.summary();
        assert!(!summary.rewind_available);
        assert_eq!(summary.rewind_keyframes, 0);
        let world = look.world();
        assert!(!world.hand_made);
        assert_eq!(world.checkpoints, 2);
        assert!(world.details.iter().any(|(k, v)| k == "map" && v == MAP));
        let before = game.clone();
        match exec(&mut session, &mut game, Command::Rewind) {
            Err(CommandError::Refused(why)) => assert!(why.contains("hand-made"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(game.player(), before.player());

        // A slot holds the whole converted simulation.
        exec(
            &mut session,
            &mut game,
            Command::Slot {
                op: SlotOp::Save { slot: 0 },
            },
        )
        .unwrap();
        let saved = game.clone();
        let mut later = Vec::new();
        for _ in 0..60 {
            game.tick(&forward()).unwrap();
            later.push(*game.player());
        }
        let outcome = exec(
            &mut session,
            &mut game,
            Command::Slot {
                op: SlotOp::Load { slot: 0 },
            },
        )
        .unwrap();
        assert!(outcome.discontinuity);
        assert_eq!(game.player(), saved.player());
        assert_eq!(game.clock().tick(), saved.clock().tick());
        assert_eq!(game.active_checkpoint(), saved.active_checkpoint());
        for expected in &later {
            game.tick(&forward()).unwrap();
            assert_eq!(game.player(), expected);
        }
    }
}

// ---------------------------------------------------------------------------
// Messages say what will happen.
// ---------------------------------------------------------------------------

#[test]
fn a_step_request_reports_what_is_queued() {
    use asamu_sandbox::time::MAX_PENDING_STEPS;
    let mut game = started_graybox();
    let mut session = Session::classic();
    let step = |n| Command::Time {
        op: TimeOp::Step { n },
    };
    let first = exec(&mut session, &mut game, step(3)).unwrap();
    assert!(
        first.message.contains("3 ticks queued"),
        "{}",
        first.message
    );
    // Requests add up ...
    let second = exec(&mut session, &mut game, step(2)).unwrap();
    assert!(
        second.message.contains("5 ticks queued"),
        "{}",
        second.message
    );
    // ... and are bounded: the message names what will run, not the request.
    let huge = exec(&mut session, &mut game, step(u32::MAX)).unwrap();
    assert!(
        huge.message
            .contains(&format!("{MAX_PENDING_STEPS} ticks queued")),
        "{}",
        huge.message
    );
    assert!(!huge.message.contains(&u32::MAX.to_string()));
}

#[test]
fn a_negative_fixed_grapple_count_is_called_unlimited() {
    // The rule treats it as unlimited (the game's own spelling); so does
    // every line that describes the rules.
    let mut game = started_graybox();
    let mut session = Session::classic();
    let rules = Rules {
        grapples: GrappleRule::Fixed(-7),
        ..Rules::default()
    };
    let outcome = exec(&mut session, &mut game, Command::SetRules { rules }).unwrap();
    assert!(
        outcome.message.contains("unlimited grapples"),
        "{}",
        outcome.message
    );
    assert!(!outcome.message.contains("-7"), "{}", outcome.message);
    assert_eq!(game.player().script.gun.max_grapples, UNLIMITED_GRAPPLES);
    let banner = session.banner(&game);
    assert!(banner.contains("unlimited grapples"), "{banner}");
    // A count that is one stays a count.
    let rules = Rules {
        grapples: GrappleRule::Fixed(2),
        ..Rules::default()
    };
    let outcome = exec(&mut session, &mut game, Command::SetRules { rules }).unwrap();
    assert!(
        outcome.message.contains("2 grapples"),
        "{}",
        outcome.message
    );
}
