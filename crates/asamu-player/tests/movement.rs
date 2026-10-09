//! Locomotion tests for the PLACEHOLDER movement model. These check internal
//! consistency (caps, integration, collision), not parity with the original.

mod common;

use asamu_player::movement::place_on_floor;
use asamu_player::world::CONTACT_SKIN;
use asamu_player::{BoxWorld, InputFrame, PlayerParams, PlayerState, step};
use common::{DT, flat_world, forward, run, standing, standing_z};
use glam::Vec3;

#[test]
fn walking_reaches_but_never_exceeds_max_ground_speed() {
    let params = PlayerParams::default();
    let max = params.movement.max_ground_speed.value;
    let world = flat_world();
    for input in [
        forward(),
        InputFrame {
            move_forward: 1.0,
            move_right: 1.0,
            ..InputFrame::default()
        },
        InputFrame {
            move_forward: -0.7,
            move_right: 0.7,
            ..InputFrame::default()
        },
    ] {
        let mut s = standing(&params, &world, 0.0, 0.0, 0.3);
        let z0 = s.position.z;
        let mut peak = 0.0_f32;
        let mut reached_at = None;
        for tick in 1..=180 {
            let ev = step(&mut s, &input, &params, &world, DT);
            assert!(s.grounded, "stays grounded on flat floor");
            assert!(!ev.left_ground && ev.landed.is_none() && !ev.jumped);
            assert_eq!(s.position.z, z0, "no vertical drift");
            assert_eq!(s.velocity.z, 0.0);
            let speed = s.horizontal_speed();
            peak = peak.max(speed);
            if reached_at.is_none() && speed >= max * 0.999 {
                reached_at = Some(tick);
            }
        }
        let expected = max
            * glam::Vec2::new(input.move_forward, input.move_right)
                .length()
                .min(1.0);
        assert!(peak <= max * (1.0 + 1e-6), "peak {peak} exceeds max {max}");
        assert!(
            (s.horizontal_speed() - expected).abs() <= expected * 1e-5,
            "{}",
            s.horizontal_speed()
        );
        if expected >= max * 0.999 {
            assert!(
                reached_at.is_some_and(|t| t < 60),
                "reached max within 1 s: {reached_at:?}"
            );
        }
    }
}

#[test]
fn braking_stops_without_reversing() {
    let params = PlayerParams::default();
    let world = flat_world();
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    run(&mut s, &[forward(); 60], &params, &world);
    let mut previous = s.horizontal_speed();
    for _ in 0..120 {
        step(&mut s, &InputFrame::default(), &params, &world, DT);
        let speed = s.horizontal_speed();
        assert!(speed <= previous);
        assert!(s.velocity.x >= 0.0, "never reverses");
        previous = speed;
    }
    assert_eq!(s.velocity, Vec3::ZERO);
}

#[test]
fn jump_leaves_ground_and_lands() {
    let params = PlayerParams::default();
    let world = flat_world();
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    let z0 = s.position.z;
    let jump = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let ev = step(&mut s, &jump, &params, &world, DT);
    assert!(ev.jumped);
    assert!(!s.grounded);
    assert!(s.position.z > z0);

    let mut apex = s.position.z;
    let mut landed = None;
    for tick in 2..600 {
        // Jump pressed again in the air must not double-jump.
        let ev = step(&mut s, &jump, &params, &world, DT);
        assert!(!ev.jumped || landed.is_some(), "no air jump at tick {tick}");
        apex = apex.max(s.position.z);
        if let Some(impact) = ev.landed {
            assert!(impact < 0.0, "lands moving down: {impact}");
            landed = Some(tick);
            break;
        }
    }
    let landed = landed.expect("landed");
    assert!(s.grounded);
    assert!((s.position.z - z0).abs() < 1e-3, "back on the floor");
    assert_eq!(s.velocity.z, 0.0);

    // Apex ≈ v² / 2|g| (semi-implicit Euler undershoots by ≤ v·dt/2).
    let v = params.movement.jump_velocity.value;
    let g = -params.movement.gravity_z.value;
    let analytic = v * v / (2.0 * g);
    let height = apex - z0;
    assert!(
        height <= analytic + 1e-3 && height >= analytic - v * DT,
        "apex {height} vs {analytic}"
    );
    // Flight time ≈ 2v/|g|.
    let flight = landed as f32 * DT;
    assert!((flight - 2.0 * v / g).abs() < 3.0 * DT, "flight {flight}");
}

#[test]
fn gravity_integration_is_semi_implicit_euler() {
    let params = PlayerParams::default();
    let g = params.movement.gravity_z.value;
    let world = BoxWorld::new();
    let z0 = 10_000.0_f32;
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, z0), 0.0);
    let mut vz = 0.0_f32;
    let mut z = z0;
    let n = 120;
    for _ in 0..n {
        step(&mut s, &InputFrame::default(), &params, &world, DT);
        vz += g * DT;
        z += vz * DT;
        assert_eq!(s.velocity.z, vz, "bit-exact velocity");
        assert_eq!(s.position.z, z, "bit-exact position");
        assert!(!s.grounded);
    }
    let analytic_v = g * DT * n as f32;
    let analytic_z = z0 + g * DT * DT * (n * (n + 1)) as f32 / 2.0;
    assert!((s.velocity.z - analytic_v).abs() < 1e-2);
    assert!((s.position.z - analytic_z).abs() < 0.5);
    assert_eq!(s.velocity.x, 0.0);
    assert_eq!(s.velocity.y, 0.0);

    // Long fall reaches exactly the (placeholder) fall-speed cap.
    for _ in 0..2000 {
        step(&mut s, &InputFrame::default(), &params, &world, DT);
    }
    assert_eq!(s.velocity.z, -params.movement.max_fall_speed.value);
}

#[test]
fn walking_off_a_ledge_falls_and_lands_below() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(-500.0, false).with_box(
        Vec3::new(-500.0, -500.0, -100.0),
        Vec3::new(100.0, 500.0, 0.0),
        false,
    );
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    let mut left = None;
    let mut landed = None;
    for tick in 0..600 {
        let ev = step(&mut s, &forward(), &params, &world, DT);
        if ev.left_ground {
            left.get_or_insert(tick);
        }
        if ev.landed.is_some() {
            landed = Some(tick);
            break;
        }
    }
    let (left, landed) = (left.expect("left ground"), landed.expect("landed"));
    assert!(left < landed);
    assert!((s.position.z - standing_z(&params, -500.0)).abs() < 1e-3);
    assert!(s.position.x > 100.0);
}

#[test]
fn steps_up_low_ledges_and_is_blocked_by_walls() {
    let params = PlayerParams::default();
    let step_h = params.movement.step_height.value;
    let world = BoxWorld::new()
        .with_ground(0.0, false)
        .with_box(
            Vec3::new(200.0, -300.0, 0.0),
            Vec3::new(400.0, 300.0, step_h * 0.6),
            false,
        )
        .with_box(
            Vec3::new(400.0, -300.0, 0.0),
            Vec3::new(800.0, 300.0, step_h * 1.2),
            false,
        )
        .with_box(
            Vec3::new(800.0, -300.0, 0.0),
            Vec3::new(900.0, 300.0, 500.0),
            false,
        );
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    run(&mut s, &[forward(); 240], &params, &world);
    assert!(s.grounded);
    assert!(
        (s.position.z - standing_z(&params, step_h * 1.2)).abs() < 1e-3,
        "climbed both steps: {}",
        s.position
    );
    let radius = params.movement.capsule_radius.value;
    assert!(
        s.position.x <= 800.0 - radius,
        "stopped by the wall: {}",
        s.position
    );
    assert!(s.position.x >= 800.0 - radius - 2.0 * CONTACT_SKIN - 1e-3);
    assert_eq!(s.velocity.x, 0.0);
}

#[test]
fn ceiling_bump_kills_upward_velocity() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_ground(0.0, false).with_box(
        Vec3::new(-200.0, -200.0, 120.0),
        Vec3::new(200.0, 200.0, 200.0),
        false,
    );
    // Start under the ceiling (the `standing` helper would land on top of it).
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, standing_z(&params, 0.0) + 1.0), 0.0);
    assert!(place_on_floor(&mut s, &params.movement, &world, 10.0));
    let jump = InputFrame {
        jump_pressed: true,
        ..InputFrame::default()
    };
    step(&mut s, &jump, &params, &world, DT);
    let mut highest_top = f32::MIN;
    for _ in 0..120 {
        step(&mut s, &InputFrame::default(), &params, &world, DT);
        let top = s.position.z + params.movement.capsule_half_height.value;
        assert!(
            top <= 120.0,
            "never inside the ceiling: top {top} state {s:?}"
        );
        highest_top = highest_top.max(top);
        if s.grounded {
            break;
        }
    }
    assert!(s.grounded);
    assert!(
        highest_top >= 120.0 - 2.0 * CONTACT_SKIN,
        "reached the ceiling: {highest_top}"
    );
}
