//! Grapple tests for the PLACEHOLDER grapple model.

mod common;

use asamu_player::grapple::{aim, eye_position};
use asamu_player::{
    Aim, BoxWorld, GrappleEvent, GrappleState, InputFrame, PlayerParams, PlayerState, RopeMode,
    step,
};
use common::{DT, flat_world, look_at, standing};
use glam::Vec3;

fn hold() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

/// A standing player looking straight at the point `target` (pitch/yaw set
/// directly; the look itself is not under test here).
fn looking_at(params: &PlayerParams, world: &BoxWorld, target: Vec3) -> PlayerState {
    let mut s = standing(params, world, 0.0, 0.0, 0.0);
    let (yaw, pitch) = look_at(eye_position(&s, params), target);
    s.yaw = yaw;
    s.pitch = pitch;
    s
}

fn target_box(world: BoxWorld, center: Vec3, grapple_able: bool) -> BoxWorld {
    world.with_box(
        center - Vec3::splat(50.0),
        center + Vec3::splat(50.0),
        grapple_able,
    )
}

#[test]
fn attaches_to_grapple_able_surface_in_range() {
    let params = PlayerParams::default();
    let center = Vec3::new(1000.0, 0.0, 300.0);
    let world = target_box(flat_world(), center, true);
    let mut s = looking_at(&params, &world, center);
    assert!(matches!(aim(&s, &params, &world), Aim::Grappleable { .. }));
    let ev = step(&mut s, &hold(), &params, &world, DT);
    let Some(GrappleEvent::Attached {
        anchor,
        rope_length,
    }) = ev.grapple
    else {
        panic!("expected attach, got {:?}", ev.grapple);
    };
    // The anchor lies on the box surface facing the player.
    assert!((anchor.x - 950.0).abs() < 1e-2, "{anchor}");
    // Inelastic (default) rope: length fixed at attach.
    assert_eq!(
        s.grapple,
        GrappleState::Attached {
            anchor,
            rope_length
        }
    );
    assert!(rope_length > 900.0 && rope_length < 1100.0);
}

#[test]
fn does_not_attach_to_non_grapple_surfaces_or_out_of_range() {
    let params = PlayerParams::default();
    let range = params.grapple.max_range.value;

    // Non-grapple-able target.
    let center = Vec3::new(1000.0, 0.0, 300.0);
    let world = target_box(flat_world(), center, false);
    let mut s = looking_at(&params, &world, center);
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(
        matches!(
            ev.grapple,
            Some(GrappleEvent::Missed {
                aim: Aim::Blocked { .. }
            })
        ),
        "{ev:?}"
    );
    assert_eq!(s.grapple, GrappleState::Idle);

    // Grapple-able but beyond range (the near face is `range + 50` from the eye).
    let far = Vec3::new(range + 200.0, 0.0, 300.0);
    let world = target_box(BoxWorld::new().with_ground(0.0, false), far, true);
    let mut s = looking_at(&params, &world, far);
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(
        matches!(
            ev.grapple,
            Some(GrappleEvent::Missed {
                aim: Aim::OutOfRange
            })
        ),
        "{ev:?}"
    );
    assert_eq!(s.grapple, GrappleState::Idle);

    // Grapple-able target occluded by a nearer non-grapple-able box.
    let world = target_box(target_box(flat_world(), center, true), center * 0.5, false);
    let mut s = looking_at(&params, &world, center);
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(
        matches!(
            ev.grapple,
            Some(GrappleEvent::Missed {
                aim: Aim::Blocked { .. }
            })
        ),
        "{ev:?}"
    );

    // Looking at the (non-grapple-able) floor.
    let world = flat_world();
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    s.pitch = -0.5;
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(
        matches!(
            ev.grapple,
            Some(GrappleEvent::Missed {
                aim: Aim::Blocked { .. }
            })
        ),
        "{ev:?}"
    );
}

#[test]
fn only_the_press_edge_fires() {
    let params = PlayerParams::default();
    let center = Vec3::new(1000.0, 0.0, 300.0);
    let world = target_box(flat_world(), center, true);
    let mut s = standing(&params, &world, 0.0, 0.0, 0.0);
    // Aim away (up) and press: miss.
    s.pitch = 1.4;
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(matches!(ev.grapple, Some(GrappleEvent::Missed { .. })));
    // Keep holding while turning onto the target: no retry while held.
    let (_, pitch) = look_at(eye_position(&s, &params), center);
    let turn = InputFrame {
        look_pitch_delta: pitch - s.pitch,
        grapple_held: true,
        ..InputFrame::default()
    };
    let ev = step(&mut s, &turn, &params, &world, DT);
    assert_eq!(ev.grapple, None);
    for _ in 0..10 {
        assert_eq!(step(&mut s, &hold(), &params, &world, DT).grapple, None);
    }
    assert_eq!(s.grapple, GrappleState::Idle);
    // Release and press again: attaches.
    assert_eq!(
        step(&mut s, &InputFrame::default(), &params, &world, DT).grapple,
        None
    );
    let ev = step(&mut s, &hold(), &params, &world, DT);
    assert!(
        matches!(ev.grapple, Some(GrappleEvent::Attached { .. })),
        "{ev:?}"
    );
}

/// A player hanging in open air below and to the side of a grapple-able
/// ceiling block, attached with a taut-ish rope.
fn swing_setup(params: &PlayerParams) -> (BoxWorld, PlayerState) {
    let world = BoxWorld::new().with_ground(-5000.0, false).with_box(
        Vec3::new(-100.0, -100.0, 2000.0),
        Vec3::new(100.0, 100.0, 2100.0),
        true,
    );
    let mut s = PlayerState::new(Vec3::new(-900.0, 0.0, 1200.0), 0.0);
    let (yaw, pitch) = look_at(eye_position(&s, params), Vec3::new(0.0, 0.0, 2000.0));
    s.yaw = yaw;
    s.pitch = pitch;
    s.velocity = Vec3::new(0.0, 300.0, 0.0);
    (world, s)
}

#[test]
fn rope_constraint_holds_over_a_long_swing() {
    for mode in [RopeMode::Inelastic, RopeMode::ShortenToDistance] {
        let mut params = PlayerParams::default();
        params.grapple.rope_mode.value = mode;
        let (world, mut s) = swing_setup(&params);
        let ev = step(&mut s, &hold(), &params, &world, DT);
        assert!(
            matches!(ev.grapple, Some(GrappleEvent::Attached { .. })),
            "{ev:?}"
        );
        let initial_len = s.grapple.rope_length().unwrap_or(0.0);
        let mut previous_len = initial_len;
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        let mut taut_ticks = 0;
        for tick in 0..3000 {
            let ev = step(&mut s, &hold(), &params, &world, DT);
            assert!(!ev.rope_correction_blocked, "open air: never blocked");
            let GrappleState::Attached {
                anchor,
                rope_length,
            } = s.grapple
            else {
                panic!("detached at tick {tick}");
            };
            let dist = s.position.distance(anchor);
            assert!(
                dist <= rope_length + 1e-2,
                "tick {tick}: dist {dist} > rope {rope_length}"
            );
            if dist >= rope_length - 1.0 {
                taut_ticks += 1;
            }
            assert!(s.is_finite());
            assert!(s.speed() <= params.grapple.attached_max_speed.value * (1.0 + 1e-6));
            match mode {
                RopeMode::Inelastic => assert_eq!(rope_length, initial_len),
                RopeMode::ShortenToDistance => {
                    assert!(rope_length <= previous_len);
                    assert!(rope_length >= params.grapple.min_rope_length.value);
                }
            }
            previous_len = rope_length;
            min_x = min_x.min(s.position.x);
            max_x = max_x.max(s.position.x);
        }
        assert!(
            taut_ticks > 100,
            "{mode:?}: rope was taut for {taut_ticks} ticks"
        );
        if mode == RopeMode::Inelastic {
            assert!(
                min_x < -300.0 && max_x > 300.0,
                "it actually swung: x in [{min_x}, {max_x}]"
            );
        }
    }
}

#[test]
fn release_preserves_velocity_exactly() {
    let params = PlayerParams::default();
    let (world, mut s) = swing_setup(&params);
    step(&mut s, &hold(), &params, &world, DT);
    for _ in 0..47 {
        step(&mut s, &hold(), &params, &world, DT);
    }
    assert!(s.grapple.is_attached());
    let before = s;
    assert!(before.speed() > 100.0, "moving: {}", before.speed());

    let ev = step(&mut s, &InputFrame::default(), &params, &world, DT);
    let Some(GrappleEvent::Released { velocity }) = ev.grapple else {
        panic!("expected release, got {:?}", ev.grapple);
    };
    assert_eq!(s.grapple, GrappleState::Idle);
    // The released velocity is the pre-release velocity, bit for bit.
    assert_eq!(
        velocity.to_array().map(f32::to_bits),
        before.velocity.to_array().map(f32::to_bits)
    );
    // On the release tick only gravity acts (no input, no pull, open air):
    // horizontal velocity is untouched, vertical gains exactly g·dt.
    assert_eq!(s.velocity.x.to_bits(), before.velocity.x.to_bits());
    assert_eq!(s.velocity.y.to_bits(), before.velocity.y.to_bits());
    assert_eq!(
        s.velocity.z,
        before.velocity.z + params.movement.gravity_z.value * DT
    );
    // And momentum keeps carrying the player afterwards.
    for _ in 0..30 {
        step(&mut s, &InputFrame::default(), &params, &world, DT);
        assert_eq!(s.velocity.x.to_bits(), before.velocity.x.to_bits());
        assert_eq!(s.velocity.y.to_bits(), before.velocity.y.to_bits());
    }
}

#[test]
fn pull_accelerates_towards_the_anchor() {
    let params = PlayerParams::default();
    let world = BoxWorld::new().with_box(
        Vec3::new(1000.0, -50.0, -50.0),
        Vec3::new(1100.0, 50.0, 50.0),
        true,
    );
    // Zero-gravity variant isolates the pull.
    let mut params = params;
    params.movement.gravity_z.value = 0.0;
    let start = Vec3::new(0.0, 0.0, -params.camera.eye_height.value);
    let mut s = PlayerState::new(start, 0.0);
    let ev = step(&mut s, &hold(), &params, &world, DT);
    let Some(GrappleEvent::Attached { anchor, .. }) = ev.grapple else {
        panic!("expected attach, got {ev:?}");
    };
    let pull = params.grapple.pull_acceleration.value;
    let expected = (anchor - start).normalize() * pull * DT;
    assert!(
        (s.velocity - expected).length() < 1e-4,
        "{} vs {expected}",
        s.velocity
    );
    for _ in 0..600 {
        step(&mut s, &hold(), &params, &world, DT);
        let len = s.grapple.rope_length().unwrap_or(0.0);
        let anchor = s.grapple.anchor().unwrap_or(Vec3::ZERO);
        assert!(s.position.distance(anchor) <= len + 1e-2);
    }
    // Reaches the block (or the min rope length) without passing through it.
    assert!(
        s.position.x <= 1000.0 - params.movement.capsule_radius.value + 1e-3,
        "{}",
        s.position
    );
}
