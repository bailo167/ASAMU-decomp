//! The console command `SetSpeed F` of the level scripts
//! (docs/PARITY_FINDINGS.md P6; ABILITIES.md A-WK-3, A-WK-4, A-ST-1…3).
//!
//! Expected values: the command writes the pawn class's default
//! `GroundSpeed` (440, the `UTPawn` default) times `F`, and every other
//! writer of `GroundSpeed` keeps writing its own value (sprint `MoveSpeed ×
//! 2` = 880, sprint removed and story exit `MoveSpeed` = 440, story entry
//! `MoveSpeed × 0.6` = 264). In `f32`: 440 × 0.3 = 132.0 and 440 × 0.15 =
//! 66.0 exactly (the products 132.0000052 and 66.0000026 are nearer to those
//! than to their `f32` neighbours 132.0000153 and 66.0000076); the
//! original's `GroundSpeed` reads 132.0 on 3,001 of 3,001 recorded Workshop
//! frames.

mod common;

use asamu_player::pawn::{self, ConsoleCommand};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, step_with,
};
use common::{DT, flat_world};

fn original() -> PlayerParams {
    PlayerParams::asamu_original()
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, DT);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

/// A started pawn standing at the origin of `world`.
fn standing(params: &PlayerParams, world: &BoxWorld) -> PlayerState {
    let mut s = common::standing(params, world, 0.0, 0.0, 0.0);
    pawn::start(&mut s, params);
    tick(&mut s, &InputFrame::default(), params, world);
    assert!(s.grounded);
    s
}

fn forward(sprint: bool) -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        sprint_held: sprint,
        ..InputFrame::default()
    }
}

/// Horizontal speed after walking forward for two seconds.
fn plateau(s: &mut PlayerState, params: &PlayerParams, world: &BoxWorld, sprint: bool) -> f32 {
    for _ in 0..120 {
        tick(s, &forward(sprint), params, world);
    }
    s.horizontal_speed()
}

#[test]
fn factor_0_3_gives_132_and_the_pawn_walks_at_it() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    assert_eq!(s.script.ground_speed, 440.0);
    let mut expected = s;
    assert!(pawn::set_speed(&mut s, &params, 0.3));
    assert_eq!(s.script.ground_speed, 132.0);
    // Nothing else is written: no sprint flag, no pawn state.
    expected.script.ground_speed = 132.0;
    assert_eq!(s, expected);
    let speed = plateau(&mut s, &params, &world, false);
    assert!((speed - 132.0).abs() < 0.05, "{speed}");
    // The Epilogue's other factor.
    assert!(pawn::set_speed(&mut s, &params, 0.15));
    assert_eq!(s.script.ground_speed, 66.0);
    // The factor scales the class default, not the current value.
    assert!(pawn::set_speed(&mut s, &params, 1.0));
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn the_command_line_is_matched_without_regard_to_case() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    // The two spellings in the maps (AG-Workshop, AG-Epilogue).
    assert_eq!(
        pawn::console_command(&mut s, &params, "setspeed 0.3"),
        ConsoleCommand::SetSpeed {
            factor: 0.3,
            ground_speed: 132.0
        }
    );
    assert_eq!(s.script.ground_speed, 132.0);
    assert_eq!(
        pawn::console_command(&mut s, &params, "SetSpeed 0.15"),
        ConsoleCommand::SetSpeed {
            factor: 0.15,
            ground_speed: 66.0
        }
    );
    assert_eq!(s.script.ground_speed, 66.0);
    assert_eq!(
        pawn::console_command(&mut s, &params, "  SETSPEED   1  "),
        ConsoleCommand::SetSpeed {
            factor: 1.0,
            ground_speed: 440.0
        }
    );
    // Not this layer's, or not well formed: nothing changes.
    let before = s;
    for line in [
        "changesize 1.4",
        "setspeed",
        "setspeed fast",
        "setspeed 0.3 0.4",
        "setspeed nan",
        "setspeed inf",
        "setspeeds 0.3",
        "",
    ] {
        assert_eq!(
            pawn::console_command(&mut s, &params, line),
            ConsoleCommand::NotHandled,
            "{line:?}"
        );
        assert_eq!(s, before, "{line:?}");
    }
}

#[test]
fn in_story_mode_the_speed_stays_until_the_story_ends() {
    // The Workshop: the level's loaded event enters story mode (264) and
    // then runs `setspeed 0.3`. In story mode the sprint keys do nothing
    // (A-ST-2), so a sprint press leaves 132; leaving story mode writes 440.
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    pawn::enter_story_mode(&mut s, &params);
    assert_eq!(s.script.ground_speed, 264.0);
    assert_eq!(
        pawn::console_command(&mut s, &params, "setspeed 0.3"),
        ConsoleCommand::SetSpeed {
            factor: 0.3,
            ground_speed: 132.0
        }
    );
    assert!(s.script.is_story());
    let speed = plateau(&mut s, &params, &world, false);
    assert!((speed - 132.0).abs() < 0.05, "{speed}");
    // Sprint held for two seconds, then released: still 132.
    let speed = plateau(&mut s, &params, &world, true);
    assert_eq!(s.script.ground_speed, 132.0);
    assert!(!s.script.sprint.active && !s.script.sprint.armed);
    assert!((speed - 132.0).abs() < 0.05, "{speed}");
    tick(&mut s, &forward(false), &params, &world);
    assert_eq!(s.script.ground_speed, 132.0);
    pawn::exit_story_mode(&mut s, &params);
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn outside_story_mode_a_sprint_press_overwrites_it() {
    // Sprint applied writes MoveSpeed × 2, sprint removed writes MoveSpeed:
    // neither looks at the value the command left (A-WK-3, A-WK-4).
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    assert!(pawn::set_speed(&mut s, &params, 0.3));
    tick(&mut s, &forward(false), &params, &world);
    assert_eq!(s.script.ground_speed, 132.0);
    tick(&mut s, &forward(true), &params, &world);
    assert_eq!(s.script.ground_speed, 880.0);
    assert!(s.script.sprint.active);
    tick(&mut s, &forward(false), &params, &world);
    assert_eq!(s.script.ground_speed, 440.0);
    assert!(!s.script.sprint.active);
}

#[test]
fn a_story_transition_after_the_command_overwrites_it() {
    let params = original();
    let world = flat_world();
    let mut s = standing(&params, &world);
    assert!(pawn::set_speed(&mut s, &params, 0.3));
    pawn::enter_story_mode(&mut s, &params);
    assert_eq!(s.script.ground_speed, 264.0);
    assert!(pawn::set_speed(&mut s, &params, 0.3));
    pawn::exit_story_mode(&mut s, &params);
    assert_eq!(s.script.ground_speed, 440.0);
}

#[test]
fn the_command_starts_a_pawn_that_has_not_ticked_yet() {
    // A level-loaded event runs before the first tick. The first tick then
    // keeps the speed (the pawn start does not run twice).
    let params = original();
    let world = flat_world();
    let mut s = common::standing(&params, &world, 0.0, 0.0, 0.0);
    assert!(!s.script.started);
    assert!(pawn::set_speed(&mut s, &params, 0.3));
    assert!(s.script.started);
    assert_eq!(s.script.ground_speed, 132.0);
    tick(&mut s, &InputFrame::default(), &params, &world);
    assert_eq!(s.script.ground_speed, 132.0);
}

#[test]
fn without_the_script_layer_or_with_a_bad_factor_nothing_happens() {
    // The graybox parameter set has no run-time GroundSpeed.
    let placeholder = PlayerParams::placeholder();
    let world = flat_world();
    let mut s = common::standing(&placeholder, &world, 0.0, 0.0, 0.0);
    let before = s;
    assert!(!pawn::set_speed(&mut s, &placeholder, 0.3));
    assert_eq!(
        pawn::console_command(&mut s, &placeholder, "setspeed 0.3"),
        ConsoleCommand::NotHandled
    );
    assert_eq!(s, before);
    // A factor that is not finite would make the state non-finite, and so
    // would a finite one whose product with 440 overflows an f32 (the
    // largest f32 is 3.4e38, so from 7.8e35 on).
    let params = original();
    let mut s = standing(&params, &world);
    let before = s;
    for factor in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        1.0e36,
        -1.0e36,
    ] {
        assert!(!pawn::set_speed(&mut s, &params, factor), "{factor}");
        assert_eq!(s, before, "{factor}");
    }
    for line in ["setspeed 1e36", "setspeed -1e38", "setspeed 1e39"] {
        assert_eq!(
            pawn::console_command(&mut s, &params, line),
            ConsoleCommand::NotHandled,
            "{line:?}"
        );
        assert_eq!(s, before, "{line:?}");
    }
    assert!(s.is_finite());
    // Just below the overflow the command still works: 440 × 7.0e35 =
    // 3.08e38 is finite.
    assert!(pawn::set_speed(&mut s, &params, 7.0e35));
    assert!(s.is_finite());
    assert_eq!(s.script.ground_speed, 440.0 * 7.0e35_f32);
}
