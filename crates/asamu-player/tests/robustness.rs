//! Robustness tests for the PLACEHOLDER simulation: hostile `dt`, non-finite
//! parameters and state, grounded flicker (box edges, seams, grapple pull
//! while standing), rope behaviour at extreme speed and while standing, and
//! grapple press/release edge cases. These check internal consistency, not
//! parity with the original.

mod common;

use asamu_player::grapple::eye_position;
use asamu_player::movement::collision_shape;
use asamu_player::sim::MAX_STEP_DT;
use asamu_player::{
    Aim, BoxWorld, GrappleEvent, GrappleState, InputFrame, PlayerParams, PlayerState, RopeMode,
    StepEvents, step,
};
use common::{DT, flat_world, forward, look_at, run, standing, standing_z};
use glam::Vec3;

fn hold() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

fn no_ground_transitions(events: &[StepEvents]) -> bool {
    events
        .iter()
        .all(|e| e.landed.is_none() && !e.left_ground && !e.jumped)
}

// ---------------------------------------------------------------- dt edges

#[test]
fn huge_dt_is_clamped_and_state_stays_finite() {
    let params = PlayerParams::default();
    let world = flat_world();
    for dt in [1.0, 1.0e6, 1.0e30, f32::MAX] {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 5000.0), 0.0);
        s.velocity = Vec3::new(300.0, 0.0, 0.0);
        let mut clamped_ref = s;
        let ev = step(&mut s, &forward(), &params, &world, dt);
        assert!(ev.dt_clamped, "dt {dt}");
        assert!(!ev.non_finite_rejected);
        assert!(s.is_finite(), "dt {dt}: {s:?}");
        // Identical to an explicit MAX_STEP_DT step.
        step(&mut clamped_ref, &forward(), &params, &world, MAX_STEP_DT);
        assert_eq!(s, clamped_ref, "dt {dt}");
    }
    // At or below the bound nothing is flagged.
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 5000.0), 0.0);
    assert!(!step(&mut s, &forward(), &params, &world, MAX_STEP_DT).dt_clamped);
    assert!(!step(&mut s, &forward(), &params, &world, DT).dt_clamped);
}

#[test]
fn tiny_and_subnormal_dt_are_harmless() {
    let params = PlayerParams::default();
    let world = flat_world();
    for dt in [1.0e-30_f32, f32::from_bits(1), f32::MIN_POSITIVE] {
        let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
        let before = s;
        for _ in 0..100 {
            let ev = step(&mut s, &forward(), &params, &world, dt);
            assert!(!ev.non_finite_rejected && !ev.dt_clamped);
            assert!(no_ground_transitions(&[ev]), "dt {dt}: {ev:?}");
        }
        assert!(s.is_finite());
        assert!(s.grounded);
        assert!(s.position.distance(before.position) < 1e-3, "dt {dt}");
    }
}

// ------------------------------------------------------- non-finite guards

#[test]
fn non_finite_parameters_never_poison_the_state() {
    let world = flat_world();
    let mut params = PlayerParams::default();
    params.movement.gravity_z.value = f32::NAN;
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    let before = s;
    let ev = step(&mut s, &InputFrame::default(), &params, &world, DT);
    assert!(ev.non_finite_rejected, "{ev:?}");
    assert_eq!(s, before, "state restored");

    // Absurd (finite) magnitudes whose motion overflows to infinity are
    // caught too.
    let params = PlayerParams::default();
    let mut s = PlayerState::new(Vec3::new(f32::MAX * 0.999, 0.0, 1000.0), 0.0);
    s.velocity.x = f32::MAX;
    let before = s;
    let ev = step(&mut s, &InputFrame::default(), &params, &world, DT);
    assert!(ev.non_finite_rejected, "{ev:?} {s:?}");
    assert_eq!(s, before);
}

#[test]
fn invalid_parameters_never_panic() {
    let world = flat_world();
    let tweaks: [fn(&mut PlayerParams); 8] = [
        |p| p.camera.max_pitch_degrees.value = f32::NAN,
        |p| p.camera.max_pitch_degrees.value = -45.0,
        |p| p.movement.capsule_radius.value = -20.0,
        |p| p.movement.ground_acceleration.value = -1.0,
        |p| p.movement.max_ground_speed.value = f32::INFINITY,
        |p| p.grapple.max_range.value = f32::NAN,
        |p| p.grapple.attached_max_speed.value = -1.0,
        |p| p.movement.walkable_floor_z.value = 2.0,
    ];
    for (i, tweak) in tweaks.iter().enumerate() {
        let mut params = PlayerParams::default();
        tweak(&mut params);
        assert!(params.validate().is_err(), "tweak {i} is invalid");
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 200.0), 0.0);
        let input = InputFrame {
            move_forward: 1.0,
            look_pitch_delta: 0.3,
            jump_pressed: true,
            grapple_held: true,
            ..InputFrame::default()
        };
        for _ in 0..30 {
            step(&mut s, &input, &params, &world, DT);
            assert!(s.is_finite(), "tweak {i}: {s:?}");
        }
    }
}

#[test]
fn non_finite_incoming_state_is_rejected_without_change() {
    let params = PlayerParams::default();
    let world = flat_world();
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
    s.velocity.x = f32::INFINITY;
    let ev = step(&mut s, &forward(), &params, &world, DT);
    assert!(ev.non_finite_rejected);
    assert_eq!(s.velocity.x, f32::INFINITY);
    assert_eq!(s.position, Vec3::new(0.0, 0.0, 100.0));
}

// ---------------------------------------------------- grounded flicker

#[test]
fn walking_across_seams_edges_and_small_steps_never_flickers() {
    let params = PlayerParams::default();
    let step_h = params.movement.step_height.value;
    let world = BoxWorld::new()
        .with_ground(-1000.0, false)
        // Three coplanar boxes meeting at seams x = 0 and x = 300.
        .with_box(
            Vec3::new(-600.0, -100.0, -50.0),
            Vec3::new(0.0, 100.0, 0.0),
            false,
        )
        .with_box(
            Vec3::new(0.0, -100.0, -50.0),
            Vec3::new(300.0, 100.0, 0.0),
            false,
        )
        .with_box(
            Vec3::new(300.0, -100.0, -50.0),
            Vec3::new(600.0, 100.0, 0.0),
            false,
        )
        // Stairs up and back down, each riser below the step height.
        .with_box(
            Vec3::new(600.0, -100.0, -50.0),
            Vec3::new(700.0, 100.0, step_h * 0.5),
            false,
        )
        .with_box(
            Vec3::new(700.0, -100.0, -50.0),
            Vec3::new(800.0, 100.0, step_h * 0.9),
            false,
        )
        .with_box(
            Vec3::new(800.0, -100.0, -50.0),
            Vec3::new(3000.0, 100.0, 0.0),
            false,
        );
    let mut s = standing(&params, &world, -500.0, 0.0, 0.0);
    // Walk over both seams and the stairs.
    let events = run(&mut s, &[forward(); 240], &params, &world);
    assert!(
        no_ground_transitions(&events),
        "{:?}",
        events
            .iter()
            .position(|e| e.landed.is_some() || e.left_ground)
    );
    assert!(s.grounded && s.position.x > 1000.0, "{s:?}");

    // Standing with the shape centre past a box edge (but the base still
    // overlapping the top face) is stable.
    let radius = params.movement.capsule_radius.value;
    let ledge = BoxWorld::new().with_ground(-1000.0, false).with_box(
        Vec3::new(-500.0, -500.0, -50.0),
        Vec3::new(0.0, 500.0, 0.0),
        false,
    );
    let mut s = standing(&params, &ledge, radius - 1.0, 0.0, 0.0);
    let z0 = s.position.z;
    let events = run(&mut s, &[InputFrame::default(); 120], &params, &ledge);
    assert!(no_ground_transitions(&events));
    assert!(s.grounded);
    assert_eq!(s.position.z, z0);
}

/// Grapple-able block 45° above and in front of a standing player.
fn anchor_world(target: Vec3) -> BoxWorld {
    flat_world().with_box(target - Vec3::splat(50.0), target + Vec3::splat(50.0), true)
}

fn attach_standing(params: &PlayerParams, world: &BoxWorld, target: Vec3) -> PlayerState {
    let mut s = standing(params, world, 0.0, 0.0, 0.0);
    let (yaw, pitch) = look_at(eye_position(&s, params), target);
    s.yaw = yaw;
    s.pitch = pitch;
    let ev = step(&mut s, &hold(), params, world, DT);
    assert!(
        matches!(ev.grapple, Some(GrappleEvent::Attached { .. })),
        "{ev:?}"
    );
    s
}

#[test]
fn pull_weaker_than_gravity_does_not_lift_a_standing_player() {
    let params = PlayerParams::default();
    let target = Vec3::new(700.0, 0.0, 700.0);
    let world = anchor_world(target);
    let mut s = attach_standing(&params, &world, target);
    assert!(
        s.grounded,
        "the attach tick itself keeps the player grounded"
    );
    let z = s.position.z;
    assert!((z - standing_z(&params, 0.0)).abs() < 1e-3);
    let events = run(&mut s, &[hold(); 240], &params, &world);
    assert!(no_ground_transitions(&events));
    assert!(s.grounded);
    assert_eq!(s.position.z, z, "no vertical drift");
    assert_eq!(s.velocity.z, 0.0);
    // The horizontal pull still drags the player towards the anchor.
    assert!(s.position.x > 1.0, "{s:?}");
}

#[test]
fn pull_stronger_than_gravity_lifts_off() {
    let mut params = PlayerParams::default();
    params.grapple.pull_acceleration.value = 3.0 * -params.movement.gravity_z.value;
    let target = Vec3::new(300.0, 0.0, 900.0);
    let world = anchor_world(target);
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    let (yaw, pitch) = look_at(eye_position(&s, &params), target);
    s.yaw = yaw;
    s.pitch = pitch;
    // The pull acts from the attach tick on.
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(matches!(ev.grapple, Some(GrappleEvent::Attached { .. })));
    assert!(!s.grounded && ev.left_ground && !ev.jumped, "{ev:?}");
    assert!(s.velocity.z > 0.0);
}

#[test]
fn taut_rope_tethers_a_standing_player_on_the_floor() {
    for mode in [RopeMode::Inelastic, RopeMode::ShortenToDistance] {
        let mut params = PlayerParams::default();
        params.grapple.rope_mode.value = mode;
        let target = Vec3::new(450.0, 0.0, 350.0);
        let world = anchor_world(target);
        let mut s = attach_standing(&params, &world, target);
        let z = s.position.z;
        assert!((z - standing_z(&params, 0.0)).abs() < 1e-3);
        let back = InputFrame {
            move_forward: -1.0,
            grapple_held: true,
            ..InputFrame::default()
        };
        let mut taut = 0;
        for tick in 0..300 {
            let ev = step(&mut s, &back, &params, &world, DT);
            assert!(no_ground_transitions(&[ev]), "{mode:?} tick {tick}: {ev:?}");
            assert!(!ev.rope_correction_blocked);
            assert!(s.grounded);
            assert_eq!(s.position.z, z, "{mode:?} tick {tick}: stays on the floor");
            assert_eq!(s.velocity.z, 0.0);
            let GrappleState::Attached {
                anchor,
                rope_length,
            } = s.grapple
            else {
                panic!("detached");
            };
            let dist = s.position.distance(anchor);
            assert!(dist <= rope_length + 1e-2, "{mode:?} tick {tick}");
            if dist >= rope_length - 0.5 {
                taut += 1;
            }
        }
        assert!(taut > 100, "{mode:?}: the rope actually went taut ({taut})");
    }
}

#[test]
fn rope_shorter_than_the_drop_lifts_the_player_and_hangs_without_flicker() {
    let params = PlayerParams::default();
    let world = flat_world();
    let floor_z = standing_z(&params, 0.0);
    // Anchor straight above; rope leaves the player hanging `gap` above the
    // standing height. Includes gaps far smaller than one tick of fall.
    for gap in [200.0_f32, 5.0, 0.2, 0.01] {
        let anchor = Vec3::new(0.0, 0.0, 800.0);
        let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
        s.grapple = GrappleState::Attached {
            anchor,
            rope_length: anchor.z - floor_z - gap,
        };
        s.grapple_was_held = true;
        let ev = step(&mut s, &hold(), &params, &world, DT);
        assert!(ev.left_ground && !s.grounded, "gap {gap}: lifted: {ev:?}");
        let events = run(&mut s, &[hold(); 240], &params, &world);
        assert!(
            no_ground_transitions(&events),
            "gap {gap}: {:?}",
            events.iter().find(|e| e.landed.is_some() || e.left_ground)
        );
        assert!(!s.grounded);
        let len = s.grapple.rope_length().unwrap_or(0.0);
        assert!(
            (s.position.distance(anchor) - len).abs() < 1e-2,
            "gap {gap}"
        );
        assert!(s.position.z > floor_z, "gap {gap}");
    }
}

// --------------------------------------------------- rope at high speed

#[test]
fn rope_constraint_and_cap_hold_at_extreme_speed() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_box(
        Vec3::new(-50.0, -50.0, 2000.0),
        Vec3::new(50.0, 50.0, 2100.0),
        true,
    );
    for speed in [5_000.0_f32, 1.0e5, 1.0e7] {
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
        s.pitch = params.camera.max_pitch_degrees.value.to_radians();
        let ev = step(&mut s, &hold(), &params, &world, DT);
        assert!(matches!(ev.grapple, Some(GrappleEvent::Attached { .. })));
        s.velocity = Vec3::new(speed, speed * 0.3, -speed * 0.2);
        for tick in 0..120 {
            let ev = step(&mut s, &hold(), &params, &world, DT);
            assert!(!ev.non_finite_rejected);
            let GrappleState::Attached {
                anchor,
                rope_length,
            } = s.grapple
            else {
                panic!("detached");
            };
            assert!(
                s.position.distance(anchor) <= rope_length + 1e-2,
                "speed {speed} tick {tick}: {s:?}"
            );
            assert!(s.speed() <= params.grapple.attached_max_speed.value * (1.0 + 1e-6));
        }
    }
}

// ------------------------------------------- grapple press/release edges

#[test]
fn alternating_press_release_every_tick() {
    let params = PlayerParams::default();
    let target = Vec3::new(1000.0, 0.0, 1500.0);
    let world = BoxWorld::new().with_ground(-5000.0, false).with_box(
        target - Vec3::splat(100.0),
        target + Vec3::splat(100.0),
        true,
    );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    let (yaw, pitch) = look_at(eye_position(&s, &params), target);
    s.yaw = yaw;
    s.pitch = pitch;
    for tick in 0..40 {
        let held = tick % 2 == 0;
        let before = s;
        let input = InputFrame {
            grapple_held: held,
            ..InputFrame::default()
        };
        let ev = step(&mut s, &input, &params, &world, DT);
        if held {
            // A fresh press every other tick: attaches (still aimed at it).
            assert!(
                matches!(ev.grapple, Some(GrappleEvent::Attached { .. })),
                "tick {tick}: {ev:?}"
            );
            assert!(s.grapple.is_attached());
        } else {
            let Some(GrappleEvent::Released { velocity }) = ev.grapple else {
                panic!("tick {tick}: {ev:?}");
            };
            assert_eq!(
                velocity.to_array().map(f32::to_bits),
                before.velocity.to_array().map(f32::to_bits)
            );
            assert_eq!(s.grapple, GrappleState::Idle);
        }
        // Keep aiming at the target.
        let (yaw, pitch) = look_at(eye_position(&s, &params), target);
        s.yaw = yaw;
        s.pitch = pitch;
    }
}

#[test]
fn a_ray_starting_inside_non_grapple_geometry_is_blocked() {
    let params = PlayerParams::default();
    let eye_h = params.camera.eye_height.value;
    // A thin non-grapple slab around the eye point (the aim query only looks
    // at the ray, so the collision shape overlapping it does not matter), and
    // a grapple-able block straight ahead that the ray must not reach.
    let world = BoxWorld::new()
        .with_box(
            Vec3::new(-10.0, -10.0, eye_h - 1.0),
            Vec3::new(10.0, 10.0, eye_h + 1.0),
            false,
        )
        .with_box(
            Vec3::new(500.0, -50.0, -50.0),
            Vec3::new(600.0, 50.0, 50.0),
            true,
        );
    let s = PlayerState::new(Vec3::ZERO, 0.0);
    match asamu_player::grapple::aim(&s, &params, &world) {
        Aim::Blocked { distance, .. } => assert_eq!(distance, 0.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn attach_tick_while_fast_respects_the_attached_cap() {
    let params = PlayerParams::default();
    let target = Vec3::new(0.0, 0.0, 2000.0);
    let world = BoxWorld::new().with_box(
        target - Vec3::splat(100.0),
        target + Vec3::splat(100.0),
        true,
    );
    let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
    s.pitch = params.camera.max_pitch_degrees.value.to_radians();
    s.velocity = Vec3::new(9000.0, 0.0, 0.0);
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(matches!(ev.grapple, Some(GrappleEvent::Attached { .. })));
    assert!(s.speed() <= params.grapple.attached_max_speed.value * (1.0 + 1e-6));
    let shape = collision_shape(&params.movement);
    assert!(!world.overlaps(s.position, shape));
}
