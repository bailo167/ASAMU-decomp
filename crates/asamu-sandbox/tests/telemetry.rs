//! Telemetry and prediction against scripted runs on hand-made levels
//! (ours; not original content).
//!
//! The expected values are taken from the tick reports of the same run by
//! the test itself, never written down: the tests hold the read-outs to the
//! simulation as it is, whatever its numbers are. Nothing here is a
//! measurement of the original.
#![allow(clippy::unwrap_used)]

use asamu_game::{Game, TickReport};
use asamu_player::InputFrame;
use asamu_sandbox::arena::{MOVEMENT_LAB, build_arena};
use asamu_sandbox::command::Command;
use asamu_sandbox::predict::predict;
use asamu_sandbox::session::{Session, SimCx};
use asamu_sandbox::telemetry::{ATTEMPTS_KEPT, Telemetry};
use glam::Vec3;

fn started_graybox() -> Game {
    let mut game = Game::graybox().unwrap();
    game.start();
    game
}

/// The movement lab: a long flat runway ahead of the start, nothing moving.
fn started_movement_lab() -> Game {
    let level = build_arena(MOVEMENT_LAB).unwrap();
    let mut game = Session::classic().new_game(level).unwrap();
    game.start();
    game
}

/// One tick, observed.
fn tick(game: &mut Game, telemetry: &mut Telemetry, input: &InputFrame) -> TickReport {
    let report = game.tick(input).unwrap();
    telemetry.observe(game, &report);
    report
}

/// Look deltas that point the eye at `target`.
fn aim_at(game: &Game, target: Vec3) -> InputFrame {
    let d = target - game.eye_position();
    let p = game.player();
    InputFrame {
        look_yaw_delta: d.y.atan2(d.x) - p.yaw,
        look_pitch_delta: d.z.atan2(d.truncate().length()) - p.pitch,
        ..InputFrame::default()
    }
}

/// What the test itself sees of one jump in the reports.
struct SeenJump {
    before: Vec3,
    takeoff_speed: f32,
    top: f32,
    ticks: u32,
    landing: Vec3,
    landing_velocity_z: f32,
}

/// Runs a full jump (jump held) with `move_forward` held, after `run_up`
/// ticks, and returns what the reports showed.
fn scripted_jump(
    game: &mut Game,
    telemetry: &mut Telemetry,
    move_forward: f32,
    run_up: usize,
) -> SeenJump {
    let walk = InputFrame {
        move_forward,
        ..InputFrame::default()
    };
    for _ in 0..run_up {
        tick(game, telemetry, &walk);
    }
    assert!(game.player().grounded);
    assert!(telemetry.last_jump().is_none());
    let before = game.player().position;
    let mut seen: Option<SeenJump> = None;
    for i in 0..2000 {
        let input = InputFrame {
            jump_pressed: i == 0,
            jump_held: true,
            ..walk
        };
        let report = tick(game, telemetry, &input);
        let p = *game.player();
        match &mut seen {
            None => {
                assert!(report.events.jumped, "the jump starts on the first tick");
                assert!(telemetry.measuring_jump());
                seen = Some(SeenJump {
                    before,
                    takeoff_speed: p.speed(),
                    top: p.position.z,
                    ticks: 1,
                    landing: p.position,
                    landing_velocity_z: 0.0,
                });
            }
            Some(jump) => {
                jump.ticks += 1;
                jump.top = jump.top.max(p.position.z);
                if let Some(impact) = report.events.landed {
                    jump.landing = p.position;
                    jump.landing_velocity_z = impact;
                    break;
                }
                assert!(telemetry.last_jump().is_none(), "not landed yet");
            }
        }
    }
    let seen = seen.unwrap();
    assert!(game.player().grounded, "the jump landed");
    assert!(!telemetry.measuring_jump());
    seen
}

#[test]
fn jump_stats_on_a_scripted_standing_jump() {
    let mut game = started_movement_lab();
    let mut telemetry = Telemetry::default();
    let seen = scripted_jump(&mut game, &mut telemetry, 0.0, 10);
    let dt = game.clock().dt();
    let stats = telemetry.last_jump().expect("a completed jump");

    assert_eq!(stats.takeoff_speed, seen.takeoff_speed);
    assert_eq!(stats.apex_height, seen.top - seen.before.z);
    assert_eq!(stats.airtime, seen.ticks as f32 * dt);
    assert_eq!(stats.landing_velocity_z, seen.landing_velocity_z);
    // A standing jump: up and down on the spot.
    assert!(stats.apex_height > 0.0);
    assert!(stats.airtime > 0.0);
    assert!(stats.landing_velocity_z < 0.0);
    assert!(stats.distance < 0.01, "{}", stats.distance);
    assert!(stats.takeoff_speed > 0.0);
}

#[test]
fn jump_stats_on_a_scripted_running_jump() {
    // Along the runway, at walking speed.
    let mut game = started_movement_lab();
    let mut telemetry = Telemetry::default();
    let seen = scripted_jump(&mut game, &mut telemetry, 1.0, 90);
    let running = telemetry.last_jump().expect("a completed jump");
    let covered = (seen.landing - seen.before).truncate().length();
    assert_eq!(running.distance, covered);
    assert!(running.distance > 1.0);
    assert_eq!(running.apex_height, seen.top - seen.before.z);
    // The take-off speed is the whole velocity, not only the vertical part.
    assert_eq!(running.takeoff_speed, seen.takeoff_speed);
    assert!(running.takeoff_speed > game.player().horizontal_speed());

    // The read-out is the latest jump: stop, jump on the spot.
    for _ in 0..120 {
        tick(&mut game, &mut telemetry, &InputFrame::default());
    }
    assert_eq!(
        telemetry.last_jump(),
        Some(running),
        "kept until the next jump"
    );
    for i in 0..2000 {
        let input = InputFrame {
            jump_pressed: i == 0,
            jump_held: true,
            ..InputFrame::default()
        };
        let report = tick(&mut game, &mut telemetry, &input);
        if i > 0 && report.events.landed.is_some() {
            break;
        }
    }
    let standing = telemetry.last_jump().expect("a second completed jump");
    assert_ne!(standing, running);
    assert!(standing.distance < running.distance);
}

#[test]
fn walking_off_a_ledge_is_measured_as_time_in_the_air() {
    // Backwards off the start platform: no jump, but a take-off. The fall
    // ends in a respawn, which is not a landing.
    let mut game = started_graybox();
    let mut telemetry = Telemetry::default();
    let back = InputFrame {
        move_forward: -1.0,
        ..InputFrame::default()
    };
    let mut left_ground = false;
    for _ in 0..1200 {
        let report = tick(&mut game, &mut telemetry, &back);
        if report.events.left_ground {
            left_ground = true;
            assert!(telemetry.measuring_jump());
        }
        if report.respawned {
            break;
        }
    }
    assert!(left_ground);
    assert_eq!(game.respawn_count(), 1);
    assert!(
        telemetry.last_jump().is_none(),
        "a respawn is not a landing"
    );
    assert!(!telemetry.measuring_jump());
    // The respawn ended the attempt: its trail is archived, a new one began.
    assert_eq!(telemetry.attempts().len(), 1);
    assert!(telemetry.attempts()[0].len() >= 2);
    assert_eq!(telemetry.trail().count(), 1);
    assert_eq!(telemetry.trail().next(), Some(game.player().position));
}

#[test]
fn swing_stats_on_a_scripted_grapple() {
    let mut game = started_graybox();
    let mut telemetry = Telemetry::default();
    let hook = game.level().grapple_points[0].position;
    for _ in 0..5 {
        tick(&mut game, &mut telemetry, &InputFrame::default());
    }
    let aim = aim_at(&game, hook);
    tick(&mut game, &mut telemetry, &aim);

    let hold = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let before = game.player().position;
    let attach = tick(&mut game, &mut telemetry, &hold);
    let attached = attach.events.gun.attached.expect("the hook is in reach");
    assert!(telemetry.measuring_swing());
    assert!(telemetry.last_swing().is_none());

    let mut ticks = 1_u32;
    let mut peak = game.player().speed();
    for _ in 0..30 {
        let report = tick(&mut game, &mut telemetry, &hold);
        assert!(report.events.gun.released.is_none(), "still held and far");
        ticks += 1;
        peak = peak.max(game.player().speed());
    }
    // Letting go of the button releases on that tick.
    let release = tick(&mut game, &mut telemetry, &InputFrame::default());
    let released = release.events.gun.released.expect("released by the button");
    ticks += 1;
    peak = peak.max(game.player().speed());

    let stats = telemetry.last_swing().expect("a completed swing");
    assert!(!telemetry.measuring_swing());
    let dt = game.clock().dt();
    assert_eq!(stats.attach_distance, (attached.anchor - before).length());
    assert_eq!(stats.duration, ticks as f32 * dt);
    assert_eq!(stats.peak_speed, peak);
    assert_eq!(stats.release_speed, released.velocity.length());
    assert!(stats.attach_distance > 0.0);
    assert!(stats.peak_speed > 0.0);
    // The swing lifted the pawn off the ground without a jump; that flight
    // is not reported as a jump while the swing was on.
    assert!(telemetry.last_jump().is_none());
}

#[test]
fn a_jump_that_becomes_a_swing_is_not_reported_as_a_jump() {
    let mut game = started_graybox();
    let mut telemetry = Telemetry::default();
    let hook = game.level().grapple_points[0].position;
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let report = tick(&mut game, &mut telemetry, &jump);
    assert!(report.events.jumped);
    assert!(telemetry.measuring_jump());
    let hold_jump = InputFrame {
        jump_held: true,
        ..InputFrame::default()
    };
    for _ in 0..6 {
        tick(&mut game, &mut telemetry, &hold_jump);
    }
    let aim = InputFrame {
        jump_held: true,
        ..aim_at(&game, hook)
    };
    tick(&mut game, &mut telemetry, &aim);
    let fire = InputFrame {
        grapple_held: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let report = tick(&mut game, &mut telemetry, &fire);
    assert!(report.events.gun.attached.is_some());
    assert!(!telemetry.measuring_jump(), "the swing ended the jump");
    assert!(telemetry.measuring_swing());
    assert!(telemetry.last_jump().is_none());
}

#[test]
fn predict_equals_the_real_ticks_on_a_static_world() {
    // The movement lab has no moving object: the prediction's static world
    // is the real one.
    let level = build_arena(MOVEMENT_LAB).unwrap();
    assert!(level.movers.is_empty() && level.attractors.is_empty() && level.crystals.is_empty());
    let inputs = [
        InputFrame::default(),
        InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        },
        InputFrame {
            move_forward: 1.0,
            move_right: 0.05,
            sprint_held: true,
            ..InputFrame::default()
        },
        // Applied verbatim on every tick: a jump press on each one.
        InputFrame {
            move_forward: 1.0,
            jump_pressed: true,
            jump_held: true,
            ..InputFrame::default()
        },
        InputFrame {
            move_forward: 0.3,
            look_yaw_delta: 0.002,
            power_jump_held: true,
            ..InputFrame::default()
        },
    ];
    for input in inputs {
        let mut game = Session::classic().new_game(level.clone()).unwrap();
        game.start();
        for _ in 0..20 {
            game.tick(&InputFrame {
                move_forward: 1.0,
                ..InputFrame::default()
            })
            .unwrap();
        }
        let before = *game.player();
        let ticks = 240;
        let arc = predict(&game, &input, ticks);
        assert_eq!(arc.len(), ticks);
        assert_eq!(*game.player(), before, "predicting changes nothing");
        for (i, predicted) in arc.iter().enumerate() {
            let report = game.tick(&input).unwrap();
            assert!(!report.respawned, "the run stays on the level");
            assert_eq!(game.player().position, *predicted, "{input:?}, tick {i}");
        }
    }
}

#[test]
fn predict_follows_the_session_tuning() {
    // The prediction runs the game's own parameters: a tuned set predicts
    // the tuned arc.
    let level = build_arena(MOVEMENT_LAB).unwrap();
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let top = |arc: &[Vec3]| arc.iter().map(|p| p.z).fold(f32::MIN, f32::max);

    let mut classic = Session::classic().new_game(level.clone()).unwrap();
    classic.start();
    let classic_top = top(&predict(&classic, &jump, 300));

    let mut session = Session::classic();
    let mut game = session.new_game(level).unwrap();
    game.start();
    let key = "movement.custom_gravity_scaling";
    let scaling = f64::from(game.params().movement.custom_gravity_scaling.value);
    let mut script = None;
    session
        .execute(
            Command::SetParam {
                key: key.to_owned(),
                value: asamu_sandbox::keys::TuneValue::Float(scaling * 0.5),
            },
            &mut SimCx {
                game: &mut game,
                script: &mut script,
            },
        )
        .unwrap();
    let tuned = predict(&game, &jump, 300);
    assert!(top(&tuned) > classic_top + 1.0);
    for predicted in &tuned {
        game.tick(&jump).unwrap();
        assert_eq!(game.player().position, *predicted);
    }
}

#[test]
fn the_session_feeds_the_telemetry_and_discontinuities_start_attempts() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let mut script = None;
    let forward = InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    };
    let run = |session: &mut Session, game: &mut Game, ticks: usize| {
        for _ in 0..ticks {
            let report = game.tick(&forward).unwrap();
            session.after_tick(game, None, &report);
        }
    };
    run(&mut session, &mut game, 30);
    assert_eq!(session.telemetry().trail().count(), 30);
    assert_eq!(
        session.telemetry().trail().last(),
        Some(game.player().position)
    );
    assert!(session.telemetry().peak_speed() > 0.0);
    assert!(session.telemetry().attempts().is_empty());
    // Observing never made the session anything but pristine.
    assert!(session.is_pristine());

    // Each respawn ends an attempt; the last few are kept.
    for attempt in 1..=(ATTEMPTS_KEPT + 1) {
        session
            .execute(
                Command::Respawn,
                &mut SimCx {
                    game: &mut game,
                    script: &mut script,
                },
            )
            .unwrap();
        assert_eq!(session.telemetry().trail().count(), 0);
        assert_eq!(session.telemetry().peak_speed(), 0.0);
        assert_eq!(
            session.telemetry().attempts().len(),
            attempt.min(ATTEMPTS_KEPT)
        );
        run(&mut session, &mut game, 30);
    }
    // The archived trails are whole attempts.
    assert!(
        session
            .telemetry()
            .attempts()
            .iter()
            .all(|trail| trail.len() == 30)
    );
}
