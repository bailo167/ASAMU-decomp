//! The time-control model: freeze, single step and speed as a state
//! machine. Plain data; the host's clocks follow it, never the reverse.

use asamu_sandbox::command::CommandError;
use asamu_sandbox::time::{MAX_PENDING_STEPS, MAX_SPEED, MIN_SPEED, SPEED_STEPS, TimeControl};

#[test]
fn freezing_and_unfreezing() {
    let mut t = TimeControl::default();
    assert!(!t.frozen());
    t.set_frozen(true);
    assert!(t.frozen());
    assert!(!t.is_default());
    assert!(t.was_used());
    // Frozen without a requested step: no tick runs.
    assert!(!t.take_step());
    t.set_frozen(false);
    assert!(!t.frozen());
    assert!(t.is_default());
    // Having been used is history.
    assert!(t.was_used());
}

#[test]
fn single_steps_run_one_tick_each_and_only_while_frozen() {
    let mut t = TimeControl::default();
    t.set_frozen(true);
    t.request_steps(3);
    assert_eq!(t.pending_steps(), 3);
    assert!(t.take_step());
    assert!(t.take_step());
    assert_eq!(t.pending_steps(), 1);
    assert!(t.take_step());
    assert!(!t.take_step());
    assert!(t.frozen(), "stepping does not unfreeze");

    // Requests add up; unfreezing drops what is left.
    t.request_steps(2);
    t.request_steps(2);
    assert_eq!(t.pending_steps(), 4);
    t.set_frozen(false);
    assert_eq!(t.pending_steps(), 0);
    assert!(!t.take_step());
}

#[test]
fn a_step_request_while_running_freezes_first() {
    let mut t = TimeControl::default();
    t.request_steps(1);
    assert!(t.frozen());
    assert!(t.was_used());
    assert!(t.take_step());
    assert!(!t.take_step());
    assert!(t.frozen());
}

#[test]
fn pending_steps_are_bounded() {
    let mut t = TimeControl::default();
    t.request_steps(u32::MAX);
    assert_eq!(t.pending_steps(), MAX_PENDING_STEPS);
    t.request_steps(u32::MAX);
    assert_eq!(t.pending_steps(), MAX_PENDING_STEPS);
}

#[test]
fn slower_and_faster_walk_the_speed_steps_and_stop_at_the_ends() {
    let mut t = TimeControl::default();
    assert_eq!(t.speed(), 1.0);
    let below: Vec<f32> = SPEED_STEPS.iter().copied().filter(|s| *s < 1.0).collect();
    for expected in below.iter().rev() {
        t.slower();
        assert_eq!(t.speed(), *expected);
    }
    t.slower();
    assert_eq!(t.speed(), MIN_SPEED, "stays at the slowest");
    for expected in SPEED_STEPS.iter().skip(1) {
        t.faster();
        assert_eq!(t.speed(), *expected);
    }
    t.faster();
    assert_eq!(t.speed(), MAX_SPEED, "stays at the fastest");
    assert!(t.was_used());
    assert!(!t.frozen(), "speed changes do not freeze");
}

#[test]
fn stepping_from_an_arbitrary_speed_lands_on_the_neighbouring_step() {
    let mut t = TimeControl::default();
    t.set_speed(0.7).unwrap();
    t.faster();
    assert_eq!(t.speed(), 1.0);
    t.set_speed(0.7).unwrap();
    t.slower();
    assert_eq!(t.speed(), 0.5);
}

#[test]
fn set_speed_accepts_the_step_range_only() {
    let mut t = TimeControl::default();
    for good in [MIN_SPEED, 0.33, 1.0, 3.0, MAX_SPEED] {
        assert_eq!(t.set_speed(good), Ok(()));
        assert_eq!(t.speed(), good);
    }
    for bad in [
        0.0,
        -1.0,
        MIN_SPEED * 0.5,
        MAX_SPEED * 2.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ] {
        let before = t.clone();
        assert!(
            matches!(t.set_speed(bad), Err(CommandError::Refused(_))),
            "{bad}"
        );
        assert_eq!(t, before, "a refused speed changes nothing ({bad})");
    }
}

#[test]
fn reset_returns_to_the_default_state() {
    let mut t = TimeControl::default();
    t.set_frozen(true);
    t.request_steps(5);
    t.set_speed(4.0).unwrap();
    assert!(!t.is_default());
    t.reset();
    assert!(t.is_default());
    assert!(!t.frozen());
    assert_eq!(t.speed(), 1.0);
    assert_eq!(t.pending_steps(), 0);
    assert!(!t.take_step());
    // The record of having been used survives a reset.
    assert!(t.was_used());
    assert_ne!(t, TimeControl::default());
}

#[test]
fn writes_that_change_nothing_are_not_use() {
    let mut t = TimeControl::default();
    t.set_frozen(false);
    t.request_steps(0);
    t.set_speed(1.0).unwrap();
    t.reset();
    assert!(!t.was_used());
    assert_eq!(t, TimeControl::default());
}
