//! Property-style tests: many deterministic pseudo-random input sequences in a
//! cluttered world. Invariants: no NaN/inf (and the non-finite guard never
//! fires), never ending inside geometry, rope constraint holds (unless
//! geometry blocked the correction), speed caps hold, landing/leaving events
//! match the grounded flag, and runs are bit-for-bit reproducible.

mod common;

use asamu_player::grapple::eye_position;
use asamu_player::movement::collision_shape;
use asamu_player::trace::{TraceMeta, record_run};
use asamu_player::{
    BoxWorld, GrappleEvent, GrappleState, InputFrame, PlayerParams, PlayerState, RopeMode, step,
};
use common::{DT, SplitMix64, look_at, standing};
use glam::Vec3;

fn cluttered_world() -> BoxWorld {
    let mut w = BoxWorld::new().with_ground(0.0, false);
    // Low steps, walls, a pillar, ceilings and grapple-able blocks overhead.
    w = w
        .with_box(
            Vec3::new(200.0, -200.0, 0.0),
            Vec3::new(400.0, 200.0, 10.0),
            false,
        )
        .with_box(
            Vec3::new(-600.0, -800.0, 0.0),
            Vec3::new(-500.0, 800.0, 400.0),
            false,
        )
        .with_box(
            Vec3::new(600.0, 300.0, 0.0),
            Vec3::new(700.0, 400.0, 900.0),
            false,
        )
        .with_box(
            Vec3::new(-300.0, 400.0, 130.0),
            Vec3::new(300.0, 700.0, 160.0),
            false,
        )
        .with_box(
            Vec3::new(800.0, -900.0, 0.0),
            Vec3::new(1600.0, -800.0, 300.0),
            true,
        );
    for i in 0..6 {
        let x = -400.0 + i as f32 * 350.0;
        let y = if i % 2 == 0 { -300.0 } else { 350.0 };
        let z = 600.0 + (i % 3) as f32 * 150.0;
        w = w.with_box(
            Vec3::new(x, y, z),
            Vec3::new(x + 80.0, y + 80.0, z + 60.0),
            true,
        );
    }
    w
}

fn random_inputs(rng: &mut SplitMix64, n: usize) -> Vec<InputFrame> {
    let mut out = Vec::with_capacity(n);
    let mut grapple = false;
    let mut hold_left = 0_u32;
    for _ in 0..n {
        if hold_left == 0 {
            grapple = rng.chance(0.5);
            hold_left = 1 + (rng.next_u64() % 90) as u32;
        }
        hold_left -= 1;
        let mut f = InputFrame {
            move_forward: rng.range(-1.3, 1.3),
            move_right: rng.range(-1.3, 1.3),
            look_yaw_delta: rng.range(-0.15, 0.15),
            look_pitch_delta: rng.range(-0.12, 0.12),
            jump_pressed: rng.chance(0.05),
            jump_held: rng.chance(0.3),
            grapple_held: grapple,
            ..InputFrame::default()
        };
        // Occasionally hostile values; the simulation must sanitize them.
        if rng.chance(0.01) {
            f.move_forward = f32::NAN;
        }
        if rng.chance(0.01) {
            f.look_yaw_delta = f32::INFINITY;
        }
        out.push(f);
    }
    out
}

/// Coverage counters, so the test proves it exercised the interesting paths.
#[derive(Default, Debug)]
struct Coverage {
    attaches: usize,
    attached_ticks: usize,
    releases: usize,
    blocked_corrections: usize,
    jumps: usize,
    landings: usize,
}

fn check_run(
    params: &PlayerParams,
    world: &BoxWorld,
    start: PlayerState,
    rng: &mut SplitMix64,
    ticks: usize,
    seed: u64,
    cov: &mut Coverage,
) {
    let shape = collision_shape(&params.movement);
    let mut s = start;
    let speed_bound = params.grapple.attached_max_speed.value
        + params.movement.max_fall_speed.value
        + params.movement.max_ground_speed.value
        + params.movement.jump_velocity.value;
    let targets: Vec<Vec3> = world
        .boxes
        .iter()
        .filter(|b| b.grapple_able)
        .map(|b| b.bounds.center())
        .collect();
    let mut hold_left = 0_u32;
    for tick in 0..ticks {
        // Mostly random input; sometimes aim at a grapple block and hold, so
        // attach/swing/release paths are exercised. Deterministic per seed.
        let mut input = random_inputs(rng, 1).pop().unwrap_or_default();
        if hold_left > 0 {
            hold_left -= 1;
            input.grapple_held = true;
            input.look_yaw_delta *= 0.1;
            input.look_pitch_delta *= 0.1;
        } else if !s.grapple.is_attached()
            && !s.grapple_was_held
            && rng.chance(0.05)
            && !targets.is_empty()
        {
            let target = targets[(rng.next_u64() % targets.len() as u64) as usize];
            let (yaw, pitch) = look_at(eye_position(&s, params), target);
            input.look_yaw_delta = yaw - s.yaw;
            input.look_pitch_delta = pitch - s.pitch;
            input.grapple_held = true;
            hold_left = 20 + (rng.next_u64() % 220) as u32;
        } else {
            input.grapple_held = false;
        }
        let ev = step(&mut s, &input, params, world, DT);
        match ev.grapple {
            Some(GrappleEvent::Attached { .. }) => cov.attaches += 1,
            Some(GrappleEvent::Released { .. }) => cov.releases += 1,
            _ => {}
        }
        cov.jumps += usize::from(ev.jumped);
        cov.landings += usize::from(ev.landed.is_some());
        cov.blocked_corrections += usize::from(ev.rope_correction_blocked);
        cov.attached_ticks += usize::from(s.grapple.is_attached());
        let ctx = || format!("seed {seed} tick {tick}: {s:?} {ev:?}");
        assert!(s.is_finite(), "non-finite: {}", ctx());
        assert!(!ev.non_finite_rejected && !ev.dt_clamped, "{}", ctx());
        // Events describe the net grounded transition of the tick.
        assert!(ev.landed.is_none() || s.grounded, "{}", ctx());
        assert!(!(ev.left_ground || ev.jumped) || !s.grounded, "{}", ctx());
        assert!(!(ev.landed.is_some() && ev.left_ground), "{}", ctx());
        assert!(s.speed() <= speed_bound, "speed: {}", ctx());
        assert!(
            !world.overlaps(s.position, shape),
            "inside geometry: {}",
            ctx()
        );
        assert!(s.pitch.abs() <= params.camera.max_pitch_degrees.value.to_radians());
        assert!((-core::f32::consts::PI..core::f32::consts::PI).contains(&s.yaw));
        if let GrappleState::Attached {
            anchor,
            rope_length,
        } = s.grapple
        {
            assert!(
                rope_length >= params.grapple.min_rope_length.value - 1e-3,
                "{}",
                ctx()
            );
            if !ev.rope_correction_blocked {
                let dist = s.position.distance(anchor);
                assert!(
                    dist <= rope_length + 1e-2,
                    "rope stretched {dist} > {rope_length}: {}",
                    ctx()
                );
            }
            assert!(
                s.speed() <= params.grapple.attached_max_speed.value * (1.0 + 1e-5),
                "attached speed cap: {}",
                ctx()
            );
        }
    }
}

#[test]
fn random_input_sequences_keep_invariants() {
    let world = cluttered_world();
    let mut cov = Coverage::default();
    for mode in [RopeMode::Inelastic, RopeMode::ShortenToDistance] {
        let mut params = PlayerParams::default();
        params.grapple.rope_mode.value = mode;
        for seed in 0..48_u64 {
            let mut rng = SplitMix64(seed.wrapping_mul(0x9E37_79B9) ^ 0xA5A5);
            let x = rng.range(-300.0, 500.0);
            let y = rng.range(-250.0, 250.0);
            let start = standing(&params, &world, x, y, rng.range(-3.0, 3.0));
            check_run(&params, &world, start, &mut rng, 900, seed, &mut cov);
        }
    }
    eprintln!("coverage: {cov:?}");
    assert!(cov.attaches > 50 && cov.releases > 50, "{cov:?}");
    assert!(cov.attached_ticks > 2000, "{cov:?}");
    assert!(
        cov.blocked_corrections > 0,
        "rope-blocked path exercised: {cov:?}"
    );
    assert!(cov.jumps > 100 && cov.landings > 100, "{cov:?}");
}

#[test]
fn random_runs_are_bit_reproducible() {
    let params = PlayerParams::default();
    let world = cluttered_world();
    for seed in 100..116_u64 {
        let mut rng = SplitMix64(seed);
        let start = standing(&params, &world, 0.0, 0.0, 0.0);
        let inputs = random_inputs(&mut rng, 600);
        let meta = TraceMeta::runtime(Some("property".into()), Some(60.0));
        let a = record_run(meta.clone(), &start, &inputs, &params, &world, DT);
        let b = record_run(meta, &start, &inputs, &params, &world, DT);
        let (ja, jb) = (a.to_jsonl_string().unwrap(), b.to_jsonl_string().unwrap());
        assert_eq!(ja.as_bytes(), jb.as_bytes(), "seed {seed}");
        let last_a = a.samples.last().unwrap();
        let last_b = b.samples.last().unwrap();
        assert_eq!(
            last_a.position.to_array().map(f32::to_bits),
            last_b.position.to_array().map(f32::to_bits)
        );
    }
}

/// `step` has no hidden state: interleaving two independent simulations tick
/// by tick (sharing the same params and world values) produces exactly the
/// same serialized traces as running each one alone.
#[test]
fn interleaved_runs_match_isolated_runs_byte_for_byte() {
    let params = PlayerParams::default();
    let world = cluttered_world();
    let mut rng_a = SplitMix64(900);
    let mut rng_b = SplitMix64(901);
    let inputs_a = random_inputs(&mut rng_a, 500);
    let inputs_b = random_inputs(&mut rng_b, 500);
    let start_a = standing(&params, &world, 0.0, 0.0, 0.0);
    let start_b = standing(&params, &world, 100.0, -100.0, 1.0);
    let meta = TraceMeta::runtime(Some("purity".into()), Some(60.0));
    let alone_a = record_run(meta.clone(), &start_a, &inputs_a, &params, &world, DT);
    let alone_b = record_run(meta.clone(), &start_b, &inputs_b, &params, &world, DT);

    let fov = params.camera.fov_degrees.value;
    let mut ta = asamu_player::Trace::new(meta.clone());
    let mut tb = asamu_player::Trace::new(meta);
    let (mut sa, mut sb) = (start_a, start_b);
    let capture = asamu_player::trace::TraceSample::capture;
    ta.samples
        .push(capture(0, 0.0, &InputFrame::default(), &sa, fov));
    tb.samples
        .push(capture(0, 0.0, &InputFrame::default(), &sb, fov));
    for (i, (ia, ib)) in inputs_a.iter().zip(&inputs_b).enumerate() {
        step(&mut sb, ib, &params, &world, DT);
        step(&mut sa, ia, &params, &world, DT);
        let tick = i as u64 + 1;
        let time = tick as f64 * f64::from(DT);
        ta.samples.push(capture(tick, time, ia, &sa, fov));
        tb.samples.push(capture(tick, time, ib, &sb, fov));
    }
    assert_eq!(
        ta.to_jsonl_string().unwrap().as_bytes(),
        alone_a.to_jsonl_string().unwrap().as_bytes()
    );
    assert_eq!(
        tb.to_jsonl_string().unwrap().as_bytes(),
        alone_b.to_jsonl_string().unwrap().as_bytes()
    );
}

#[test]
fn high_speed_impacts_do_not_tunnel() {
    let params = PlayerParams::default();
    let shape = collision_shape(&params.movement);
    let world = BoxWorld::new()
        .with_box(
            Vec3::new(500.0, -1000.0, -1000.0),
            Vec3::new(510.0, 1000.0, 1000.0),
            false,
        )
        .with_box(
            Vec3::new(-1000.0, -1000.0, -510.0),
            Vec3::new(1000.0, 1000.0, -500.0),
            false,
        );
    let mut rng = SplitMix64(7);
    for _ in 0..200 {
        let mut s = PlayerState::new(
            Vec3::new(0.0, rng.range(-200.0, 200.0), rng.range(-300.0, 300.0)),
            0.0,
        );
        // Far beyond any cap, as if teleported in at absurd speed.
        s.velocity = Vec3::new(
            rng.range(20_000.0, 200_000.0),
            rng.range(-500.0, 500.0),
            rng.range(-90_000.0, 1000.0),
        );
        for _ in 0..5 {
            step(&mut s, &InputFrame::default(), &params, &world, DT);
            assert!(s.position.x <= 500.0 - shape.radius + 1e-2, "{s:?}");
            assert!(s.position.z >= -500.0 + shape.half_height - 1e-2, "{s:?}");
            assert!(!world.overlaps(s.position, shape));
        }
    }
}
