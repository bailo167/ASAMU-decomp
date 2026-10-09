//! Rule-by-rule tests of the original grapple gun (`asamu_player::grapple_gun`)
//! against `docs/reverse-engineering/GRAPPLE.md`. Each test names the rule
//! ids it pins; expected values come from the spec's text and formulas.
//! The worlds are synthetic (hand-made boxes), never original data.

mod common;

use asamu_player::events::SimEvent;
use asamu_player::grapple_gun::{
    self, FireOutcome, ReleaseReason, UNLIMITED_GRAPPLES, WeaponState,
};
use asamu_player::pawn::{self, PawnStateName};
use asamu_player::rocket_boots::{self, BootsStateName};
use asamu_player::world::{ActorClass, ActorTag, Surface};
use asamu_player::{
    BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, Ue3PawnMovement, step_with,
};
use common::DT;
use glam::Vec3;

fn original() -> PlayerParams {
    PlayerParams::asamu_original()
}

fn tick_dt(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
    dt: f32,
) -> StepEvents {
    let e = step_with(&Ue3PawnMovement, s, input, params, world, dt);
    assert!(!e.non_finite_rejected && s.is_finite(), "{s:?}");
    e
}

fn tick(
    s: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &BoxWorld,
) -> StepEvents {
    tick_dt(s, input, params, world, DT)
}

fn fire() -> InputFrame {
    InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    }
}

fn idle() -> InputFrame {
    InputFrame::default()
}

/// A started pawn at rest in the air at `position`, looking along
/// (`yaw`, `pitch`), capacity `max_grapples`.
fn floating(
    params: &PlayerParams,
    position: Vec3,
    yaw: f32,
    pitch: f32,
    max_grapples: i32,
) -> PlayerState {
    let mut s = PlayerState::new(position, yaw);
    s.pitch = pitch;
    pawn::start(&mut s, params);
    grapple_gun::set_max_grapples(&mut s, max_grapples);
    s
}

/// Pitch that makes the eye (38 above the centre at rest) look at a point
/// at the centre's height `distance` ahead: a level 1-D pull.
fn level_pitch(distance: f32) -> f32 {
    (-38.0_f32).atan2(distance)
}

/// A grapple-able wall whose face is at `x` (facing −X), large enough.
fn wall_at(x: f32) -> BoxWorld {
    BoxWorld::new().with_box(
        Vec3::new(x, -5000.0, -5000.0),
        Vec3::new(x + 100.0, 5000.0, 8000.0),
        true,
    )
}

fn wall_with_surface(x: f32, grapple_able: bool, surface: Surface) -> BoxWorld {
    BoxWorld::new().with_surface_box(
        Vec3::new(x, -5000.0, -5000.0),
        Vec3::new(x + 100.0, 5000.0, 8000.0),
        grapple_able,
        surface,
    )
}

/// A cube target of half size `h` at `c`.
fn cube(world: BoxWorld, c: Vec3, h: f32, surface: Surface) -> BoxWorld {
    world.with_surface_box(c - Vec3::splat(h), c + Vec3::splat(h), true, surface)
}

fn attach_events(e: &StepEvents) -> bool {
    e.gun.attached.is_some()
}

// ---------------------------------------------------------------------------
// Pull, flying physics, proximity release (G-PH-2…7, G-RL-2, finding 1–4).
// ---------------------------------------------------------------------------

/// A 1-D pull from rest: returns (ticks until the proximity release,
/// speed handed over).
fn straight_pull(distance: f32, dt: f32) -> (u32, f32) {
    let params = original();
    let world = wall_at(distance);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(distance),
        3,
    );
    let e = tick_dt(&mut s, &fire(), &params, &world, dt);
    assert!(attach_events(&e), "{e:?}");
    let anchor = s.grapple_anchor().expect("attached");
    assert!(
        (anchor - Vec3::new(distance, 0.0, 1000.0)).length() < 0.05,
        "{anchor}"
    );
    if let Some(r) = e.gun.released {
        return (1, r.velocity.length());
    }
    for n in 2..2000 {
        let e = tick_dt(&mut s, &fire(), &params, &world, dt);
        if let Some(r) = e.gun.released {
            assert_eq!(r.reason, ReleaseReason::Proximity);
            return (n, r.velocity.length());
        }
    }
    panic!("never released");
}

#[test]
fn g_ph_5_straight_pull_times_and_release_speeds_match_the_spec_model() {
    // GRAPPLE.md G-PH-5 (re-simulated model): release radius reached after
    // ≈ 0.52 / 1.42 / 2.95 s from 1000 / 2500 / 4999 uu at 60 fps; speed
    // after the halving ≈ 1435–1460 uu/s at 60 fps, 1210–1220 at 120 fps,
    // 1945–2005 at 30 fps. The spec rounds; we allow 1.5 uu/s around its
    // ranges (our values: 1220.4 / 1209.3 / 1218.6 at 120 fps).
    let within = |v: f32, lo: f32, hi: f32| v >= lo - 1.5 && v <= hi + 1.5;
    for (distance, seconds) in [(1000.0_f32, 0.52_f32), (2500.0, 1.42), (4999.0, 2.95)] {
        let (ticks, speed) = straight_pull(distance, DT);
        let t = ticks as f32 * DT;
        assert!(
            (t - seconds).abs() <= 0.01,
            "{distance}: {t} s vs {seconds}"
        );
        assert!(within(speed, 1435.0, 1460.0), "{distance}: {speed}");
        let (_, speed_120) = straight_pull(distance, 1.0 / 120.0);
        assert!(
            within(speed_120, 1210.0, 1220.0),
            "{distance} @120: {speed_120}"
        );
        let (_, speed_30) = straight_pull(distance, 1.0 / 30.0);
        assert!(
            within(speed_30, 1945.0, 2005.0),
            "{distance} @30: {speed_30}"
        );
    }
}

#[test]
fn g_ph_2_pull_is_ten_million_over_d_after_physics() {
    // Attach from rest 3000 uu away: the attach tick's physics moves nothing
    // (V = 0, no gravity in flying), then the gun adds dt·10⁷/d along the
    // anchor direction.
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 500.0),
        0.0,
        level_pitch(3000.0),
        3,
    );
    let p0 = s.position;
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(attach_events(&e));
    assert_eq!(s.position, p0, "no gravity, no motion in the attach tick");
    let d = 3000.0_f32;
    let expected = DT * 10_000.0 / (d / 1000.0);
    assert!(
        (s.velocity.x - expected).abs() < 1e-3,
        "{} vs {expected}",
        s.velocity.x
    );
    assert!(s.velocity.y.abs() < 1e-4 && s.velocity.z.abs() < 1e-3);
    assert_eq!(
        s.script.gun.distance, d,
        "vDistance measured before the pull"
    );
}

#[test]
fn g_ph_3_flying_has_drag_and_the_air_speed_cap_and_no_gravity() {
    // An attached pawn moving at 1000 uu/s across while the pull is
    // suspended (instant-release target): only drag acts, (1 − 0.15·dt)²
    // per tick, no gravity (G-PH-3, G-PH-7, G-RL-3).
    let params = original();
    let crystal = Surface {
        actor: Some(7),
        class: ActorClass::GlowFlower,
        tag: ActorTag::None,
    };
    let world = cube(
        BoxWorld::new(),
        Vec3::new(1500.0, 0.0, 1038.0),
        50.0,
        crystal,
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    s.velocity = Vec3::new(0.0, 1000.0, 0.0);
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(e.gun.attached.unwrap().instant_release);
    let f = 0.5 * params.movement.fluid_friction.value;
    let expected = 1000.0 * (1.0 - f * DT) * (1.0 - f * DT);
    assert!(
        (s.velocity.y - expected).abs() < 1e-3,
        "{} vs {expected}",
        s.velocity.y
    );
    assert_eq!(s.velocity.z, 0.0, "no gravity while flying");
    assert_eq!(s.velocity.x, 0.0, "no pull while the instant timer runs");
    // The 2000 cap (AirSpeed = fGrappleAccel) cuts a fast pawn on the attach
    // frame (G-AT-10, T4).
    let mut fast = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    fast.velocity = Vec3::new(0.0, 0.0, -3000.0);
    tick(&mut fast, &fire(), &params, &world);
    assert!((fast.speed() - 2000.0).abs() < 0.05, "{}", fast.speed());
}

#[test]
fn g_ph_5_attached_speed_is_a_sawtooth_above_2000_and_g_mo_1_hands_it_over() {
    // After the cap in physics (2000) the gun adds dt·10⁷/d, so the stored
    // velocity exceeds 2000; a button release hands that over unchanged.
    let params = original();
    let world = wall_at(4000.0);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 800.0),
        0.0,
        level_pitch(4000.0),
        3,
    );
    tick(&mut s, &fire(), &params, &world);
    let mut peak = 0.0_f32;
    for _ in 0..80 {
        tick(&mut s, &fire(), &params, &world);
        peak = peak.max(s.speed());
    }
    let d = s.script.gun.distance;
    let expected = 2000.0 + DT * 1.0e7 / d;
    assert!(
        (s.speed() - expected).abs() < 1.0,
        "{} vs {expected}",
        s.speed()
    );
    assert!(peak > 2000.0);
    let v = s.velocity;
    let e = tick(&mut s, &idle(), &params, &world);
    let r = e.gun.released.unwrap();
    assert_eq!(r.reason, ReleaseReason::Button);
    assert_eq!(r.velocity, v, "no impulse, no cap at the release (G-MO-1)");
}

#[test]
fn g_rl_2_proximity_release_halves_the_velocity() {
    let params = original();
    let world = wall_at(1000.0);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(1000.0),
        3,
    );
    tick(&mut s, &fire(), &params, &world);
    loop {
        let before = s;
        let e = tick(&mut s, &fire(), &params, &world);
        if let Some(r) = e.gun.released {
            assert_eq!(r.reason, ReleaseReason::Proximity);
            // Reconstruct: physics moved `before` (flying), then the pull was
            // added and the result halved.
            let d = s.script.gun.distance;
            assert!(d < 200.0, "{d}");
            assert!(before.script.gun.distance >= 200.0);
            let pulled = s.velocity * 2.0;
            assert!(pulled.x > 2000.0, "the halved velocity contains the pull");
            assert!(!s.pawn.flying && !s.grounded, "Falling after the release");
            assert_eq!(s.script.code.state, PawnStateName::Release);
            break;
        }
    }
}

#[test]
fn g_ph_6_blocked_pull_stays_attached_pressed_against_the_obstacle() {
    // T7: geometry keeps the pawn from getting within 200 uu: the trace
    // passes through a 10 uu slit in a wall that the pawn (radius 21)
    // cannot pass, so it stays attached and pressed against the wall.
    let params = original();
    let world = BoxWorld::new()
        .with_box(
            Vec3::new(500.0, -2000.0, -2000.0),
            Vec3::new(520.0, -5.0, 3000.0),
            false,
        )
        .with_box(
            Vec3::new(500.0, 5.0, -2000.0),
            Vec3::new(520.0, 2000.0, 3000.0),
            false,
        )
        .with_box(
            Vec3::new(1000.0, -2000.0, -2000.0),
            Vec3::new(1100.0, 2000.0, 3000.0),
            true,
        );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(&mut s, &fire(), &params, &world);
    let anchor = e.gun.attached.unwrap().anchor;
    assert!((anchor.x - 1000.0).abs() < 0.01, "{anchor}");
    let mut pressed = None;
    for _ in 0..240 {
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(
            e.gun.released.is_none(),
            "no automatic release while blocked"
        );
        if s.position.x > 470.0 {
            pressed.get_or_insert(s.position);
        }
    }
    assert!(s.is_grapple_attached() && s.pawn.flying);
    assert!(s.script.gun.distance >= 200.0);
    let pressed = pressed.expect("reached the wall");
    assert!(
        (s.position - pressed).length() < 1.0,
        "held against the wall: {} vs {pressed}",
        s.position
    );
    // The stored velocity is only the last pull increment: what physics
    // moved into the wall was lost (G-PH-3 step 6).
    let increment = DT * 1.0e7 / s.script.gun.distance;
    assert!(
        (s.velocity.length() - increment).abs() < 1.0,
        "{} vs {increment}",
        s.velocity.length()
    );
}

#[test]
fn g_ph_3_flying_steps_up_low_walls_and_slides_along_tall_ones() {
    let params = original();
    // A low wall (20 uu) on the way to an anchor at the pawn's height: the
    // flying step-up (near-vertical wall, moving horizontally) gets over it.
    let floor_z = 0.0;
    let world = BoxWorld::new()
        .with_ground(floor_z, false)
        .with_box(
            Vec3::new(300.0, -500.0, 0.0),
            Vec3::new(320.0, 500.0, 20.0),
            false,
        )
        .with_box(
            Vec3::new(2000.0, -500.0, 0.0),
            Vec3::new(2100.0, 500.0, 2000.0),
            true,
        );
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 50.0),
        0.0,
        level_pitch(2000.0),
        3,
    );
    tick(&mut s, &fire(), &params, &world);
    let mut max_x = 0.0_f32;
    for _ in 0..90 {
        tick(&mut s, &fire(), &params, &world);
        max_x = max_x.max(s.position.x);
        if !s.is_grapple_attached() {
            break;
        }
    }
    assert!(max_x > 400.0, "stepped over the low wall: {max_x}");
}

#[test]
fn g_ph_4_air_speed_is_set_by_the_gun_and_restored_after_a_release() {
    let params = original();
    let mut s = PlayerState::new(Vec3::ZERO, 0.0);
    pawn::start(&mut s, &params);
    assert_eq!(s.script.air_speed, 2000.0, "gun spawn writes fGrappleAccel");
    s.script.air_speed = 440.0;
    s.script.gun.released = true;
    tick(&mut s, &idle(), &params, &BoxWorld::new());
    assert!(
        !s.script.gun.released,
        "cleared at the next gun tick (G-RL-6)"
    );
    assert_eq!(s.script.air_speed, 2000.0);
}

// ---------------------------------------------------------------------------
// Targeting and acceptance (G-TG-1/2, G-AC-*).
// ---------------------------------------------------------------------------

#[test]
fn g_ac_5_range_is_5000_from_the_eye_and_g_tg_1_the_trace_is_16384_long() {
    let params = original();
    // T5: a target 4999 uu from the eye attaches, 5001 uu fails (fail
    // sound), 16000 fails, nothing hit fails.
    for (eye_distance, outcome) in [
        (4999.0_f32, FireOutcome::Attached),
        (5001.0, FireOutcome::Failed),
        (16_000.0, FireOutcome::Failed),
    ] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let eye = s.view_location(&params);
        let world = wall_at(eye.x + eye_distance);
        let e = tick(&mut s, &fire(), &params, &world);
        assert_eq!(e.gun.fire, Some(outcome), "{eye_distance}");
    }
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(&mut s, &fire(), &params, &BoxWorld::new());
    assert_eq!(e.gun.fire, Some(FireOutcome::Failed), "no hit actor");
    // The trace starts at the view location (eye height + walk bob).
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    s.script.walk_bob = Vec3::new(0.0, 0.0, 3.0);
    let world = wall_at(1000.0);
    let e = tick(&mut s, &fire(), &params, &world);
    let anchor = e.gun.attached.unwrap().anchor;
    assert!((anchor.z - (1000.0 + 38.0 + 3.0)).abs() < 1e-3, "{anchor}");
}

#[test]
fn g_ac_1_capacity_zero_is_silent_and_out_of_grapples_fails() {
    let params = original();
    let world = wall_at(1500.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 0);
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.fire, Some(FireOutcome::Silent));
    assert!(
        !s.script.gun.has_grappled,
        "silent: the press is not marked"
    );
    // used ≥ capacity → fail sound.
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 2);
    s.script.gun.times_grappled = 2;
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.fire, Some(FireOutcome::Failed));
    // NotGrappleAble (grapple_able == false) → fail sound.
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(
        &mut s,
        &fire(),
        &params,
        &wall_with_surface(1500.0, false, Surface::default()),
    );
    assert_eq!(e.gun.fire, Some(FireOutcome::Failed));
}

#[test]
fn g_ac_2_attached_released_or_hidden_hand_consumes_the_press_silently() {
    let params = original();
    let world = wall_at(1500.0);
    for setup in [
        |s: &mut PlayerState| s.script.gun.released = true,
        |s: &mut PlayerState| s.script.gun.hand_hidden = true,
    ] {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        setup(&mut s);
        let e = tick(&mut s, &fire(), &params, &world);
        assert_eq!(e.gun.fire, Some(FireOutcome::Consumed));
        assert!(s.script.gun.has_grappled, "the press is used");
        assert!(!s.is_grapple_attached());
    }
}

#[test]
fn g_ac_4_top_and_bottom_only_tags_check_the_face_normal() {
    let params = original();
    let top_only = Surface {
        tag: ActorTag::TopOnlyGrappleAble,
        ..Surface::default()
    };
    let bottom_only = Surface {
        tag: ActorTag::BottomOnlyGrappleAble,
        ..Surface::default()
    };
    let slab = |surface| {
        BoxWorld::new().with_surface_box(
            Vec3::new(-2000.0, -2000.0, 900.0),
            Vec3::new(2000.0, 2000.0, 1000.0),
            true,
            surface,
        )
    };
    // T13: TopOnly from below (normal −Z) → silent rejection before the
    // press is marked; from above (normal +Z) → attach.
    let mut below = floating(&params, Vec3::new(0.0, 0.0, 500.0), 0.0, 1.2, 3);
    let e = tick(&mut below, &fire(), &params, &slab(top_only));
    assert_eq!(e.gun.fire, Some(FireOutcome::WrongFace));
    assert!(!below.script.gun.has_grappled);
    let mut above = floating(&params, Vec3::new(0.0, 0.0, 1500.0), 0.0, -1.2, 3);
    let e = tick(&mut above, &fire(), &params, &slab(top_only));
    assert_eq!(e.gun.fire, Some(FireOutcome::Attached));
    // BottomOnly: the reverse.
    let mut below = floating(&params, Vec3::new(0.0, 0.0, 500.0), 0.0, 1.2, 3);
    let e = tick(&mut below, &fire(), &params, &slab(bottom_only));
    assert_eq!(e.gun.fire, Some(FireOutcome::Attached));
    let mut above = floating(&params, Vec3::new(0.0, 0.0, 1500.0), 0.0, -1.2, 3);
    let e = tick(&mut above, &fire(), &params, &slab(bottom_only));
    assert_eq!(e.gun.fire, Some(FireOutcome::WrongFace));
    // A side face (normal Z = 0) of a TopOnly actor is rejected too.
    let mut side = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(
        &mut side,
        &fire(),
        &params,
        &wall_with_surface(1500.0, true, top_only),
    );
    assert_eq!(e.gun.fire, Some(FireOutcome::WrongFace));
}

#[test]
fn g_ac_6_world_geometry_without_tags_is_grapple_able() {
    let params = original();
    let world = BoxWorld::new().with_ground(0.0, true);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 600.0), 0.0, -1.0, 3);
    let e = tick(&mut s, &fire(), &params, &world);
    let a = e.gun.attached.unwrap();
    assert_eq!(a.surface.class, ActorClass::WorldGeometry);
    assert!(
        e.kismet
            .contains(&SimEvent::PlayerGrappled { originator: None })
    );
}

#[test]
fn g_ac_0_story_mode_interacts_within_200_of_the_pawn_centre() {
    let params = original();
    let interactable = Surface {
        actor: Some(42),
        class: ActorClass::Interactable,
        tag: ActorTag::None,
    };
    for (face_x, interacted) in [(150.0_f32, true), (250.0, false)] {
        // The eye is 38 above the centre; the hit point is level with the
        // eye, so the centre distance is sqrt(x² + 38²).
        let world = wall_with_surface(face_x, true, interactable);
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let _ = pawn::enter_story_mode(&mut s, &params);
        let e = tick(&mut s, &fire(), &params, &world);
        assert_eq!(
            e.gun.fire,
            Some(FireOutcome::StoryMode { interacted }),
            "{face_x}"
        );
        assert_eq!(
            e.kismet.contains(&SimEvent::InteractWith { actor: 42 }),
            interacted
        );
        assert!(!s.is_grapple_attached(), "story mode never grapples");
        assert!(
            !s.script.gun.has_grappled,
            "the story branch does not mark the press"
        );
    }
}

#[test]
fn g_tg_2_crosshair_rules() {
    let params = original();
    let static_mesh = Surface {
        class: ActorClass::StaticMesh,
        ..Surface::default()
    };
    let bsp = Surface::default();
    let at = |world: &BoxWorld, max: i32, previous: bool| {
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, max);
        s.script.gun.crosshair = previous;
        tick(&mut s, &idle(), &params, world);
        s.script.gun.crosshair
    };
    // In range, grapples left, static mesh, no tag → positive.
    assert!(at(&wall_with_surface(3000.0, true, static_mesh), 3, false));
    // Out of grapples / NotGrappleAble / out of range → negative.
    assert!(!at(&wall_with_surface(3000.0, true, static_mesh), 0, true));
    assert!(!at(&wall_with_surface(3000.0, false, static_mesh), 3, true));
    assert!(!at(&wall_with_surface(5500.0, true, static_mesh), 3, true));
    // A hit without a static-mesh component in range leaves it unchanged
    // (the real grapple still attaches to it).
    assert!(at(&wall_with_surface(3000.0, true, bsp), 3, true));
    assert!(!at(&wall_with_surface(3000.0, true, bsp), 3, false));
}

#[test]
fn g_in_5_the_attach_trace_uses_the_view_of_the_previous_tick() {
    // The press is processed before the controller's look update: a look
    // delta in the press tick does not change what is grappled.
    let params = original();
    let world = cube(
        BoxWorld::new(),
        Vec3::new(2000.0, 0.0, 1038.0),
        60.0,
        Surface::default(),
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let press = InputFrame {
        look_yaw_delta: 0.5,
        ..fire()
    };
    let e = tick(&mut s, &press, &params, &world);
    assert!(attach_events(&e), "{e:?}");
    assert!(
        (s.yaw - 0.5).abs() < 1e-6,
        "the look still applies afterwards"
    );
}

// ---------------------------------------------------------------------------
// Input path (G-IN-2/3/4).
// ---------------------------------------------------------------------------

#[test]
fn g_in_3_one_attempt_per_press_holding_never_refires() {
    // T5 tail: after a failed attempt (out of range), holding the button does
    // nothing even when a target comes into range.
    let params = original();
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let far = wall_at(6000.0);
    let e = tick(&mut s, &fire(), &params, &far);
    assert_eq!(e.gun.fire, Some(FireOutcome::Failed));
    let near = wall_at(1000.0);
    for _ in 0..30 {
        let e = tick(&mut s, &fire(), &params, &near);
        assert_eq!(e.gun.fire, None);
        assert!(!s.is_grapple_attached());
    }
    // Release (latch set again), wait for `Active`, press: attaches.
    for _ in 0..8 {
        tick(&mut s, &idle(), &params, &near);
    }
    assert_eq!(s.script.gun.weapon, WeaponState::Active);
    let e = tick(&mut s, &fire(), &params, &near);
    assert_eq!(e.gun.fire, Some(FireOutcome::Attached));
}

#[test]
fn g_in_2_a_quick_repress_waits_for_the_next_refire_check() {
    // T14: release and press again 2 ticks after the first press: the
    // weapon is still `WeaponFiring`, so the second fire happens at the
    // refire check, 0.1 s after the first press (frame-quantised).
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(attach_events(&e));
    let e = tick(&mut s, &idle(), &params, &world);
    assert_eq!(e.gun.released.unwrap().reason, ReleaseReason::Button);
    let mut attached_at = None;
    for t in 2..20 {
        let e = tick(&mut s, &fire(), &params, &world);
        if let Some(a) = e.gun.attached {
            assert!(a.from_refire, "served by the refire check");
            attached_at = Some(t);
            break;
        }
    }
    // The refire timer counts from the first press (tick 0); it fires when
    // the accumulated time strictly exceeds 0.1 s, in the gun tick.
    let mut count = 0.0_f32;
    let mut expected = 0;
    while count <= 0.1 {
        count += DT;
        expected += 1;
    }
    assert_eq!(
        attached_at,
        Some(expected - 1),
        "tick index of the refire check"
    );
}

#[test]
fn g_in_4_disabling_the_grapple_only_swallows_the_next_press() {
    // T18: SeqAct_ToggleGrapple(false), two presses: the first does nothing,
    // the second grapples.
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    grapple_gun::enable_grapple(&mut s, false);
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.fire, None);
    for _ in 0..8 {
        tick(&mut s, &idle(), &params, &world);
    }
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.fire, Some(FireOutcome::Attached));
}

// ---------------------------------------------------------------------------
// Attach (G-AT-*).
// ---------------------------------------------------------------------------

#[test]
fn g_at_attach_effects() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    s.script.move_input_lock = 2;
    s.script.code.state = PawnStateName::Jumped;
    let v0 = Vec3::new(10.0, 20.0, -30.0);
    s.velocity = v0;
    let e = tick(&mut s, &fire(), &params, &world);
    let a = e.gun.attached.unwrap();
    // G-AT-1 budget, G-AT-4 anchor at the hit, G-AT-5 one lock removed,
    // G-AT-6 flying + Grappling + Shooting, events.
    assert_eq!(s.script.gun.times_grappled, 1);
    assert_eq!(s.grapple_anchor(), Some(a.anchor));
    assert_eq!(s.script.move_input_lock, 1);
    assert!(s.pawn.flying && !s.grounded);
    assert_eq!(s.script.code.state, PawnStateName::Shooting);
    assert_eq!(
        e.kismet.iter().collect::<Vec<_>>(),
        vec![SimEvent::PlayerGrappled { originator: None }]
    );
    // G-AT-10: the physics switch leaves the velocity alone; the attach
    // tick's flying update only drags it (and the gun adds the pull).
    let f = 0.15 * DT;
    let dragged = v0 * (1.0 - f) * (1.0 - f);
    let pull = s.velocity - dragged;
    let expected = DT * 1.0e7 / s.script.gun.distance;
    assert!(
        (pull.length() - expected).abs() < 0.01,
        "{pull} vs {expected}"
    );
    assert!(pull.x > 0.99 * expected, "toward the wall: {pull}");
}

#[test]
fn g_at_3_and_g_at_7_interactable_targets_and_anchor_following() {
    let params = original();
    // A mover (InterpActor): originator WorldInfo (no interface, no tag),
    // the anchor follows the actor's translation.
    let mover = Surface {
        actor: Some(9),
        class: ActorClass::InterpActor,
        tag: ActorTag::None,
    };
    let mut world = cube(
        BoxWorld::new(),
        Vec3::new(3000.0, 0.0, 1038.0),
        100.0,
        mover,
    );
    world.set_actor_location(9, Vec3::new(3000.0, 0.0, 1038.0));
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(&mut s, &fire(), &params, &world);
    let anchor0 = e.gun.attached.unwrap().anchor;
    assert!(
        e.kismet
            .contains(&SimEvent::PlayerGrappled { originator: None })
    );
    // Move the actor by (0, 50, 10): the anchor follows in the next gun tick.
    world.boxes[0].bounds.min += Vec3::new(0.0, 50.0, 10.0);
    world.boxes[0].bounds.max += Vec3::new(0.0, 50.0, 10.0);
    world.set_actor_location(9, Vec3::new(3000.0, 50.0, 1048.0));
    tick(&mut s, &fire(), &params, &world);
    let anchor1 = s.grapple_anchor().unwrap();
    assert!((anchor1 - (anchor0 + Vec3::new(0.0, 50.0, 10.0))).length() < 1e-3);
    // G-AT-8: the helper keeps following after a release (cosmetic).
    tick(&mut s, &idle(), &params, &world);
    world.set_actor_location(9, Vec3::new(3000.0, 80.0, 1048.0));
    tick(&mut s, &idle(), &params, &world);
    assert!((s.script.gun.grapple_location.y - (anchor0.y + 80.0)).abs() < 1e-3);

    // A floating rock (DynamicSMActor) does not carry the anchor (T12), and
    // a `grappleInteractable`-tagged actor is the originator.
    for (class, tag, follows, originator) in [
        (ActorClass::FloatingRock, ActorTag::None, false, None),
        (
            ActorClass::StaticMesh,
            ActorTag::GrappleInteractable,
            false,
            Some(5),
        ),
        (ActorClass::FallingRock, ActorTag::None, true, Some(5)),
    ] {
        let surface = Surface {
            actor: Some(5),
            class,
            tag,
        };
        let mut world = cube(
            BoxWorld::new(),
            Vec3::new(3000.0, 0.0, 1038.0),
            100.0,
            surface,
        );
        world.set_actor_location(5, Vec3::new(3000.0, 0.0, 1038.0));
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(
            e.kismet.contains(&SimEvent::PlayerGrappled { originator }),
            "{class:?}"
        );
        assert_eq!(s.script.gun.follow.is_some(), follows, "{class:?}");
        let anchor0 = s.grapple_anchor().unwrap();
        world.set_actor_location(5, Vec3::new(3000.0, 100.0, 1038.0));
        tick(&mut s, &fire(), &params, &world);
        let moved = (s.grapple_anchor().unwrap() - anchor0).length() > 50.0;
        assert_eq!(moved, follows, "{class:?}");
    }
}

// ---------------------------------------------------------------------------
// Release paths (G-RL-*).
// ---------------------------------------------------------------------------

#[test]
fn g_rl_1_common_release_effects() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    tick(&mut s, &fire(), &params, &world);
    for _ in 0..10 {
        tick(&mut s, &fire(), &params, &world);
    }
    let v = s.velocity;
    let used = s.script.gun.times_grappled;
    let e = tick(&mut s, &idle(), &params, &world);
    let r = e.gun.released.unwrap();
    assert_eq!(r.reason, ReleaseReason::Button);
    assert_eq!(r.velocity, v, "velocity unchanged by the release");
    assert_eq!(s.script.gun.times_grappled, used, "budget unchanged");
    assert!(!s.pawn.flying);
    assert_eq!(s.script.code.state, PawnStateName::Release);
    assert_eq!(
        e.kismet.iter().collect::<Vec<_>>(),
        vec![SimEvent::PlayerReleasedGrapple]
    );
    // A release when not attached does nothing.
    let mut again = s;
    let out = pawn::release_grapple(&mut again, ReleaseReason::External);
    assert!(out.gun.released.is_none() && out.kismet.is_empty());
}

#[test]
fn g_rl_3_instant_release_targets_hang_without_pull_for_0_05_s() {
    // T9/T10: a glow flower: attach, no pull and no gravity, the 0.05 s
    // timer releases on the first tick where the accumulated time strictly
    // exceeds 0.05 (the 3rd gun tick at 60 Hz, counted from the attach tick).
    let params = original();
    let flower = Surface {
        actor: Some(3),
        class: ActorClass::GlowFlower,
        tag: ActorTag::None,
    };
    let world = cube(
        BoxWorld::new(),
        Vec3::new(1500.0, 0.0, 1038.0),
        50.0,
        flower,
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(e.gun.attached.unwrap().instant_release);
    assert!(e.kismet.contains(&SimEvent::PlayerGrappled {
        originator: Some(3)
    }));
    assert!(e.kismet.contains(&SimEvent::ActorGrappled { actor: 3 }));
    assert_eq!(s.script.gun.times_grappled, 1, "a flower costs a grapple");
    let mut released_at = None;
    for t in 1..10 {
        let e = tick(&mut s, &fire(), &params, &world);
        assert_eq!(s.velocity, Vec3::ZERO, "no pull, no gravity while waiting");
        if let Some(r) = e.gun.released {
            assert_eq!(r.reason, ReleaseReason::InstantTimer);
            assert!(e.kismet.contains(&SimEvent::ActorUngrappled { actor: 3 }));
            released_at = Some(t);
            break;
        }
    }
    assert_eq!(released_at, Some(2));
}

#[test]
fn g_rl_6_no_cooldown_and_g_ct_4_landing_refills() {
    // T8: capacity 2, two grapples in the air, the third press fails; a
    // landing refills.
    let params = original();
    let world = wall_at(3000.0).with_ground(-200.0, false);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 2);
    let press_release = |s: &mut PlayerState| -> StepEvents {
        let e = tick(s, &fire(), &params, &world);
        tick(s, &idle(), &params, &world);
        for _ in 0..8 {
            tick(s, &idle(), &params, &world);
        }
        e
    };
    assert!(attach_events(&press_release(&mut s)));
    assert!(attach_events(&press_release(&mut s)));
    assert_eq!(s.script.gun.times_grappled, 2);
    assert_eq!(press_release(&mut s).gun.fire, Some(FireOutcome::Failed));
    // Land on the ground: the normal landing handler refills.
    let mut refilled = false;
    for _ in 0..600 {
        let e = tick(&mut s, &idle(), &params, &world);
        if let Some(l) = e.landing {
            assert!(l.grapples_refilled);
            refilled = true;
            break;
        }
    }
    assert!(refilled);
    assert_eq!(s.script.gun.times_grappled, 0);
}

#[test]
fn g_ct_4_not_landable_floors_do_not_refill() {
    // A8 (grapple part): landing on a `NotLandable` actor skips the handler.
    let params = original();
    let pad = Surface {
        tag: ActorTag::NotLandable,
        ..Surface::default()
    };
    let world = BoxWorld::new().with_surface_box(
        Vec3::new(-1000.0, -1000.0, -100.0),
        Vec3::new(1000.0, 1000.0, 0.0),
        false,
        pad,
    );
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 300.0), 0.0, 0.0, 3);
    s.script.gun.times_grappled = 2;
    s.script.move_input_lock = 1;
    let mut landed = None;
    for _ in 0..300 {
        let e = tick(&mut s, &idle(), &params, &world);
        if e.landing.is_some() {
            landed = e.landing;
            break;
        }
    }
    let l = landed.expect("lands");
    assert_eq!(l.handler, asamu_player::LandingHandler::NotLandable);
    assert!(!l.grapples_refilled);
    assert_eq!(s.script.gun.times_grappled, 2);
    assert_eq!(s.script.move_input_lock, 1, "lock not released either");
    assert!(s.grounded, "the native physics still walks");
}

#[test]
fn g_ct_4_charged_crystal_refills_in_the_attach_tick_uncharged_costs_one() {
    let params = original();
    for (charged, used_after) in [(true, 0), (false, 2)] {
        let crystal = Surface {
            actor: Some(11),
            class: ActorClass::RechargeCrystal { charged },
            tag: ActorTag::None,
        };
        let world = cube(
            BoxWorld::new(),
            Vec3::new(1500.0, 0.0, 1038.0),
            50.0,
            crystal,
        );
        let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
        s.script.gun.times_grappled = 1;
        let e = tick(&mut s, &fire(), &params, &world);
        let a = e.gun.attached.unwrap();
        assert!(a.instant_release);
        assert_eq!(s.script.gun.times_grappled, used_after, "charged {charged}");
        assert!(e.kismet.contains(&SimEvent::ActorGrappled { actor: 11 }));
    }
}

#[test]
fn g_ct_2_capacity_mapping_unlimited_and_g_ct_6_counter_clamp() {
    let mut s = PlayerState::new(Vec3::ZERO, 0.0);
    pawn::start(&mut s, &original());
    assert_eq!(s.script.gun.max_grapples, 0, "iMaxGrapples starts at 0");
    assert!(s.script.gun.can_grapple, "bCanGrapple starts true");
    grapple_gun::set_max_grapples(&mut s, -1);
    assert_eq!(s.script.gun.max_grapples, UNLIMITED_GRAPPLES);
    grapple_gun::set_max_grapples(&mut s, -7);
    assert_eq!(s.script.gun.max_grapples, UNLIMITED_GRAPPLES);
    grapple_gun::set_max_grapples(&mut s, 2);
    s.script.gun.times_grappled = 1;
    grapple_gun::unlimited_grapples(&mut s, true);
    assert_eq!(
        (s.script.gun.max_grapples, s.script.gun.times_grappled),
        (UNLIMITED_GRAPPLES, 0)
    );
    s.script.gun.times_grappled = 1;
    grapple_gun::unlimited_grapples(&mut s, false);
    assert_eq!(
        (s.script.gun.max_grapples, s.script.gun.times_grappled),
        (2, 0)
    );

    // Capacity 4: the light manager clamps the used count to 3 every gun
    // tick, so it never reaches 4: effectively unlimited.
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 4);
    for _ in 0..8 {
        let e = tick(&mut s, &fire(), &params, &world);
        assert!(attach_events(&e));
        assert!(s.script.gun.times_grappled <= 3);
        for _ in 0..9 {
            tick(&mut s, &idle(), &params, &world);
        }
    }
}

// ---------------------------------------------------------------------------
// Interactions (G-IX-*) and the hand (G-AC-3).
// ---------------------------------------------------------------------------

#[test]
fn g_ix_3_boost_then_grapple_suspends_the_pull_and_caps_the_boost() {
    // T15: a rocket boost running, then a grapple: the boost keeps writing
    // velocity every 0.05 s, the gun adds no pull, and between boost writes
    // the flying physics caps the speed at 2000 (so nothing is added after
    // physics on those ticks).
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    tick(&mut s, &space, &params, &world);
    assert_eq!(s.script.boots.state, BootsStateName::Boosting);
    // Wait into the boost phase (1.0 s charge at 60 Hz = 60 ticks).
    for _ in 0..70 {
        tick(&mut s, &idle(), &params, &world);
    }
    assert!(s.speed() > 2000.0, "boost speed above the flying cap");
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(attach_events(&e));
    let mut write_ticks = 0;
    for _ in 0..30 {
        tick(&mut s, &fire(), &params, &world);
        assert!(s.is_grapple_attached());
        if s.script.boots.sleep == Some(rocket_boots::BOOST_STEP) {
            // The boots wrote this tick (absolute velocity, uncapped).
            write_ticks += 1;
        } else {
            assert!(s.speed() <= 2000.0 + 1e-2, "no pull added: {}", s.speed());
        }
    }
    assert_eq!(write_ticks, 10, "one boost write every 3 ticks at 60 Hz");
}

#[test]
fn g_ix_3_boost_ends_when_the_anchor_is_beyond_5000_and_the_pull_resumes() {
    // The boost aims backwards (sampled at its first write); then the pawn
    // turns and grapples a wall just inside 5000 uu ahead. The boost pushes
    // it away; at the first boost write after the gun measured more than
    // fMaxDistance the boost ends (aim term only), and the pull resumes.
    let params = original();
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        std::f32::consts::PI,
        0.0,
        3,
    );
    let eye_x = s.view_location(&params).x;
    let world = wall_at(eye_x + 4990.0);
    rocket_boots::enable_rocket_boots(&mut s, true);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    tick(&mut s, &space, &params, &world);
    let mut aim = Vec3::ZERO;
    for _ in 0..80 {
        tick(&mut s, &idle(), &params, &world);
        if s.script.boots.aim != Vec3::ZERO {
            aim = s.script.boots.aim;
            break;
        }
    }
    assert!(aim.x < -0.99, "boost aims backwards: {aim}");
    // Turn around (look update in this tick), fire in the next one.
    let turn = InputFrame {
        look_yaw_delta: -std::f32::consts::PI,
        ..InputFrame::default()
    };
    tick(&mut s, &turn, &params, &world);
    s.position.x = 0.0;
    s.position.z = 1000.0;
    let e = tick(&mut s, &fire(), &params, &world);
    assert!(attach_events(&e), "{e:?}");
    let mut ended = false;
    for _ in 0..12 {
        tick(&mut s, &fire(), &params, &world);
        if s.script.boots.state == BootsStateName::Unavailable {
            ended = true;
            break;
        }
    }
    assert!(ended, "boost broken off");
    assert!(s.script.gun.distance > 5000.0);
    // The break kept only the aim term (no corkscrew) and the pull resumes:
    // the next gun ticks accelerate toward the anchor.
    let vx = s.velocity.x;
    for _ in 0..5 {
        tick(&mut s, &fire(), &params, &world);
    }
    assert!(s.velocity.x > vx, "pulled toward the anchor again");
}

#[test]
fn g_ix_4_boost_key_while_attached_does_nothing() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    tick(&mut s, &fire(), &params, &world);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..fire()
    };
    let e = tick(&mut s, &space, &params, &world);
    assert_eq!(e.boots, None);
    assert_eq!(s.script.boots.state, BootsStateName::Ready);
    assert!(!e.jumped);
}

#[test]
fn g_ix_2_space_after_a_release_fails_the_jump_starts_a_boost_and_both_run() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    rocket_boots::enable_rocket_boots(&mut s, true);
    s.velocity = Vec3::new(0.0, 0.0, 600.0);
    tick(&mut s, &idle(), &params, &world);
    let space = InputFrame {
        jump_pressed: true,
        jump_held: true,
        ..InputFrame::default()
    };
    let e = tick(&mut s, &space, &params, &world);
    assert!(!e.jumped);
    assert_eq!(e.boots, Some(rocket_boots::BootsEvent::Started));
    assert_eq!(s.script.code.state, PawnStateName::Jumped);
    // Release Space during the charge: the damping loop starts and is not
    // cancelled by the boost.
    tick(&mut s, &idle(), &params, &world);
    assert_eq!(s.script.code.state, PawnStateName::ReleasedJump);
    assert_eq!(s.script.boots.state, BootsStateName::Boosting);
}

#[test]
fn g_ac_3_hidden_hand_blocks_and_the_animated_hide_waits_0_3_s() {
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(&params, Vec3::new(0.0, 0.0, 1000.0), 0.0, 0.0, 3);
    // The story-mode style hide (visibility kept) does not block.
    grapple_gun::hide_grapple_gun(&mut s, true, false, true);
    assert!(!s.script.gun.hand_hidden);
    // Animated hide with visibility off: hidden after the 0.3 s timer
    // (strictly exceeded), even if shown again meanwhile (the timer is not
    // cancelled by a show).
    grapple_gun::hide_grapple_gun(&mut s, true, true, false);
    grapple_gun::hide_grapple_gun(&mut s, false, true, true);
    let mut hidden_at = None;
    for t in 0..30 {
        tick(&mut s, &idle(), &params, &world);
        if s.script.gun.hand_hidden {
            hidden_at = Some(t + 1);
            break;
        }
    }
    let mut count = 0.0_f32;
    let mut expected = 0;
    while count <= 0.3 {
        count += DT;
        expected += 1;
    }
    assert_eq!(hidden_at, Some(expected));
    let e = tick(&mut s, &fire(), &params, &world);
    assert_eq!(e.gun.fire, Some(FireOutcome::Consumed));
    // Leaving story mode shows the hand again.
    let _ = pawn::enter_story_mode(&mut s, &params);
    pawn::exit_story_mode(&mut s, &params);
    assert!(!s.script.gun.hand_hidden);
}

#[test]
fn gun_runs_are_bit_reproducible_at_several_tick_rates() {
    let params = original();
    let crystal = Surface {
        actor: Some(1),
        class: ActorClass::RechargeCrystal { charged: true },
        tag: ActorTag::None,
    };
    let world = cube(
        wall_at(3500.0).with_ground(0.0, false),
        Vec3::new(800.0, 300.0, 600.0),
        40.0,
        crystal,
    );
    for dt in [1.0_f32 / 30.0, 1.0 / 60.0, 1.0 / 144.0] {
        let run = || {
            let mut s = floating(&params, Vec3::new(0.0, 0.0, 400.0), 0.0, 0.1, 3);
            rocket_boots::enable_rocket_boots(&mut s, true);
            let mut rng = common::SplitMix64(0xBEEF);
            let mut out = Vec::new();
            for _ in 0..600 {
                let i = InputFrame {
                    move_forward: rng.range(-1.0, 1.0),
                    look_yaw_delta: rng.range(-0.04, 0.04),
                    look_pitch_delta: rng.range(-0.03, 0.03),
                    jump_pressed: rng.chance(0.03),
                    jump_held: rng.chance(0.5),
                    grapple_held: rng.chance(0.6),
                    power_jump_held: rng.chance(0.1),
                    ..InputFrame::default()
                };
                let e = tick_dt(&mut s, &i, &params, &world, dt);
                out.push((s, e));
            }
            serde_json::to_string(&out).unwrap()
        };
        assert_eq!(run(), run(), "dt {dt}");
    }
}

#[test]
fn g_ph_5_sideways_speed_is_replaced_by_the_pull_no_pendulum() {
    // T6: grapple ahead while moving fast sideways: each tick adds radial
    // velocity and the 2000 cap rescales the sum, so the sideways part
    // decays monotonically (no swing back) while the path curves to the
    // anchor.
    let params = original();
    let world = wall_at(3000.0);
    let mut s = floating(
        &params,
        Vec3::new(0.0, 0.0, 1000.0),
        0.0,
        level_pitch(3000.0),
        3,
    );
    s.velocity = Vec3::new(0.0, 1900.0, 0.0);
    tick(&mut s, &fire(), &params, &world);
    let mut last_vy = s.velocity.y;
    let mut last_vx = s.velocity.x;
    for _ in 0..40 {
        tick(&mut s, &fire(), &params, &world);
        assert!(
            s.velocity.y <= last_vy + 1e-3,
            "{} > {last_vy}",
            s.velocity.y
        );
        assert!(s.velocity.y > -1.0, "no swing back: {}", s.velocity);
        assert!(s.velocity.x >= last_vx - 1e-3);
        last_vy = s.velocity.y;
        last_vx = s.velocity.x;
    }
    assert!(last_vy < 1000.0 && last_vx > 1500.0, "{}", s.velocity);
}
